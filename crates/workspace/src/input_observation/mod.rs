//! One FD-relative input observation at a time. These are facts, not admission.
//!
//! The caller owns authorization, namespace exclusion and related resource gates.
//! Run a step in its bounded owned-work capsule, after admitting that exact edge.
//! No step follows a symlink, walks another component, selects a program, or
//! creates SourceProof/Ready. In particular, an open ancestor does not prove an
//! absent name remains absent. Revalidation performs a new observation.
mod sys;

#[cfg(all(test, target_os = "linux"))]
mod tests;

use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io,
    os::{fd::AsFd, unix::fs::MetadataExt},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use crate::execution::objects::{
    NamespaceContext, ObjectQualificationUnsupported, fd_mount_id, local_object_platform,
};

/// Audit fields only. Deserializing them cannot reconstruct a retained object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputFileKind {
    File,
    Directory,
    SymbolicLink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputMetadata {
    pub kind: InputFileKind,
    pub length: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl From<&Metadata> for InputMetadata {
    fn from(metadata: &Metadata) -> Self {
        let kind = if metadata.is_file() {
            InputFileKind::File
        } else if metadata.is_dir() {
            InputFileKind::Directory
        } else if metadata.file_type().is_symlink() {
            InputFileKind::SymbolicLink
        } else {
            InputFileKind::Other
        };
        Self {
            kind,
            length: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InputObservationError {
    #[error("input observation is unsupported: {0:?}")]
    Unsupported(InputObservationUnsupported),
    #[error("original input name or retained object changed")]
    Changed,
    #[error("symbolic link needs a separate target/admission step")]
    UnresolvedSymbolicLink,
    #[error("invalid input observation step: {0}")]
    InvalidStep(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputObservationUnsupported {
    ObjectPlatform(ObjectQualificationUnsupported),
    EffectiveAccessUnavailable,
    SymbolicLinkTargetTooLarge,
}

type Result<T> = std::result::Result<T, InputObservationError>;

/// Descriptor audit facts. Equality alone never proves retained identity,
/// namespace stability, same-mount qualification, or backing independence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputObjectFact {
    pub device: u64,
    pub inode: u64,
    pub mount: u64,
    pub kind: InputFileKind,
}

impl InputObjectFact {
    fn of(file: &File, metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mount: fd_mount_id(file)?,
            kind: InputMetadata::from(metadata).kind,
        })
    }
}

struct HeldNode {
    file: File,
    identity: InputObjectFact,
    namespace: Arc<NamespaceContext>,
    namespace_inode: u64,
    edge: Option<NamedEdge>,
}

#[derive(Clone)]
struct NamedEdge {
    parent: Arc<HeldNode>,
    name: OsString,
}

impl HeldNode {
    fn validate(&self) -> Result<()> {
        let mut node = self;
        loop {
            if !node.namespace.matches_current()? {
                return Err(InputObservationError::Changed);
            }
            let metadata = node.file.metadata()?;
            if metadata.nlink() == 0 || InputObjectFact::of(&node.file, &metadata)? != node.identity
            {
                return Err(InputObservationError::Changed);
            }
            let Some(edge) = &node.edge else {
                return Ok(());
            };
            let current = sys::open_entry(edge.parent.file.as_fd(), &edge.name)
                .map_err(|_| InputObservationError::Changed)?;
            if InputObjectFact::of(&current, &current.metadata()?)? != node.identity {
                return Err(InputObservationError::Changed);
            }
            node = &edge.parent;
        }
    }
}

/// A continuously retained lookup anchor, not a named-root stability proof.
/// Only the caller can prove that the root's outer namespace is controlled.
#[derive(Clone)]
pub struct InputLookupFrontier {
    node: Arc<HeldNode>,
}

impl InputLookupFrontier {
    /// The caller supplies its actual already-held directory, not a pathname.
    pub fn from_directory(directory: &File) -> Result<Self> {
        local_object_platform().map_err(|reason| {
            InputObservationError::Unsupported(InputObservationUnsupported::ObjectPlatform(reason))
        })?;
        let file = directory.try_clone()?;
        let namespace = Arc::new(NamespaceContext::capture()?);
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(InputObservationError::InvalidStep(
                "anchor is not a directory",
            ));
        }
        let identity = InputObjectFact::of(&file, &metadata)?;
        let node = Arc::new(HeldNode {
            file,
            identity,
            namespace_inode: namespace.inode()?,
            namespace,
            edge: None,
        });
        node.validate()?;
        Ok(Self { node })
    }

    /// Includes `.` and `..` as real component lookups; never accepts a path.
    /// The returned work description is not authorization to perform the step.
    pub fn lookup_step(&self, name: &OsStr) -> Result<InputObservationStep> {
        sys::validate_component(name)?;
        Ok(InputObservationStep(Step::Lookup(NamedEdge {
            parent: self.node.clone(),
            name: name.into(),
        })))
    }

    pub fn file(&self) -> &File {
        &self.node.file
    }

    pub fn fact(&self) -> InputObjectFact {
        self.node.identity
    }

    pub fn mount_namespace_inode(&self) -> u64 {
        self.node.namespace_inode
    }

    /// Checks retained facts and known inner edges, never an unrecorded root path.
    pub fn revalidate(&self) -> Result<()> {
        self.node.validate()
    }
}

enum Step {
    Lookup(NamedEdge),
    Metadata(Arc<HeldNode>),
    EffectiveExecute(Arc<HeldNode>),
    SymbolicLinkTarget(Arc<HeldNode>),
}

/// Owned closed work item suitable for a bounded blocking capsule. There is no
/// callback or authority constructor. The caller must admit the edge first.
pub struct InputObservationStep(Step);

pub enum InputObservation {
    Lookup(InputLookupObservation),
    Metadata(InputMetadata),
    EffectiveExecute(std::result::Result<(), i32>),
    SymbolicLinkTarget(OsString),
}

impl InputObservationStep {
    /// Performs this fixed primitive only. Async callers must use their actual
    /// owned-work capsule; this method does not acquire gates or spawn tasks.
    pub fn observe(self) -> Result<InputObservation> {
        match self.0 {
            Step::Lookup(edge) => {
                edge.parent.validate()?;
                let observed = match sys::open_entry(edge.parent.file.as_fd(), &edge.name) {
                    Ok(file) => {
                        let identity = InputObjectFact::of(&file, &file.metadata()?)?;
                        let node = Arc::new(HeldNode {
                            file,
                            identity,
                            namespace: edge.parent.namespace.clone(),
                            namespace_inode: edge.parent.namespace_inode,
                            edge: Some(edge.clone()),
                        });
                        node.validate()?;
                        LookupResult::Present(node)
                    }
                    Err(error) => LookupResult::Error(raw_errno(error)?),
                };
                edge.parent.validate()?;
                Ok(InputObservation::Lookup(InputLookupObservation {
                    edge,
                    observed,
                }))
            }
            Step::Metadata(node) => {
                node.validate()?;
                let metadata = InputMetadata::from(&node.file.metadata()?);
                node.validate()?;
                Ok(InputObservation::Metadata(metadata))
            }
            Step::EffectiveExecute(node) => {
                node.validate()?;
                let outcome = sys::effective_execute(node.file.as_fd())?;
                node.validate()?;
                Ok(InputObservation::EffectiveExecute(outcome))
            }
            Step::SymbolicLinkTarget(node) => {
                node.validate()?;
                let target = sys::read_symbolic_link(node.file.as_fd())?;
                node.validate()?;
                Ok(InputObservation::SymbolicLinkTarget(target))
            }
        }
    }
}

enum LookupResult {
    Present(Arc<HeldNode>),
    Error(i32),
}

/// Holds the actual named edge and original object/error. A negative result is
/// an observed syscall outcome, not permission to skip later admission.
pub struct InputLookupObservation {
    edge: NamedEdge,
    observed: LookupResult,
}

impl InputLookupObservation {
    pub fn errno(&self) -> Option<i32> {
        match self.observed {
            LookupResult::Error(errno) => Some(errno),
            LookupResult::Present(_) => None,
        }
    }

    pub fn file(&self) -> Option<&File> {
        match &self.observed {
            LookupResult::Present(node) => Some(&node.file),
            LookupResult::Error(_) => None,
        }
    }

    pub fn kind(&self) -> Option<InputFileKind> {
        match &self.observed {
            LookupResult::Present(node) => Some(node.identity.kind),
            LookupResult::Error(_) => None,
        }
    }

    pub fn fact(&self) -> Option<InputObjectFact> {
        match &self.observed {
            LookupResult::Present(node) => Some(node.identity),
            LookupResult::Error(_) => None,
        }
    }

    /// The caller must admit this object/scope before issuing its next lookup.
    /// A non-directory is kept so a further lookup reports actual ENOTDIR.
    pub fn next_frontier(&self) -> Result<InputLookupFrontier> {
        let node = self.resolved_node()?;
        Ok(InputLookupFrontier { node })
    }

    /// Terminal metadata follows no additional path. A symbolic link must be
    /// read and its target admitted separately before constructing this step.
    pub fn metadata_step(&self) -> Result<InputObservationStep> {
        Ok(InputObservationStep(Step::Metadata(self.resolved_node()?)))
    }

    /// Effective credentials and ACLs are evaluated by the OS on the actual FD.
    /// The caller retains MCP's original conditional-access/errno policy.
    pub fn effective_execute_step(&self) -> Result<InputObservationStep> {
        Ok(InputObservationStep(Step::EffectiveExecute(
            self.resolved_node()?,
        )))
    }

    pub fn symbolic_link_target_step(&self) -> Result<InputObservationStep> {
        let node = self.node()?;
        if node.identity.kind != InputFileKind::SymbolicLink {
            return Err(InputObservationError::InvalidStep(
                "entry is not a symbolic link",
            ));
        }
        Ok(InputObservationStep(Step::SymbolicLinkTarget(node)))
    }

    pub fn revalidate(&self) -> Result<()> {
        self.edge.parent.validate()?;
        match &self.observed {
            LookupResult::Present(node) => node.validate(),
            LookupResult::Error(expected) => {
                match sys::open_entry(self.edge.parent.file.as_fd(), &self.edge.name) {
                    Err(error) if error.raw_os_error() == Some(*expected) => Ok(()),
                    _ => Err(InputObservationError::Changed),
                }
            }
        }
    }

    fn node(&self) -> Result<Arc<HeldNode>> {
        match &self.observed {
            LookupResult::Present(node) => Ok(node.clone()),
            LookupResult::Error(_) => {
                Err(InputObservationError::InvalidStep("entry was not opened"))
            }
        }
    }

    fn resolved_node(&self) -> Result<Arc<HeldNode>> {
        let node = self.node()?;
        if node.identity.kind == InputFileKind::SymbolicLink {
            return Err(InputObservationError::UnresolvedSymbolicLink);
        }
        Ok(node)
    }
}

fn raw_errno(error: io::Error) -> Result<i32> {
    error.raw_os_error().ok_or_else(|| error.into())
}
