//! Startup and takeover isolation checks. No daemon or credential seeding here.
use super::{
    BackendId,
    config::AgentBackendConfig,
    paths::{BackendPaths, resolved_path},
};
use anyhow::{Context, ensure};
use calm_truth::db::Repo;
use calm_truth::db::sqlite::backend_registry::{self, BackendIdentity};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn check_environment(
    multiple: bool,
    mut read: impl FnMut(&str) -> Option<OsString>,
) -> anyhow::Result<()> {
    if multiple {
        for key in provider::codex::shared::CREDENTIAL_ENV_KEYS {
            ensure!(
                read(key).is_none(),
                "process credential key {key} is forbidden with multiple backends"
            );
        }
    }
    Ok(())
}

// StrictModes-style protection of every directory entry, including lexical
// alias parents. Sticky writable parents protect entries owned by us or root.
fn secure_ancestors(path: &Path) -> anyhow::Result<()> {
    let mut pending = vec![std::path::absolute(path)?];
    let mut checked = std::collections::HashSet::new();
    while let Some(path) = pending.pop() {
        for component in path.ancestors() {
            if !checked.insert(component.to_owned()) {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(component) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            ensure!(
                metadata.uid() == unsafe { nix::libc::geteuid() } || metadata.uid() == 0,
                "backend home ancestor has an untrusted owner; repair ownership and run chmod 700 {}",
                component.display()
            );
            if metadata.is_dir() {
                ensure!(
                    metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0,
                    "backend home ancestor is replaceable by another user; run chmod 700 {}",
                    component.display()
                );
            } else if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(component)?;
                let target = if target.is_absolute() {
                    target
                } else {
                    component
                        .parent()
                        .context("home alias has no parent")?
                        .join(target)
                };
                // canonicalize rejects dangling/looping aliases, but its result
                // alone omits intermediate target parents. Check both spellings.
                pending.push(target.canonicalize()?);
                pending.push(std::path::absolute(target)?);
            }
        }
    }
    Ok(())
}

// Sticky ancestors protect existing entries, but cannot reserve an absent socket
// or checkpoint name. Its immediate container must exclude other-user writers.
fn secure_runtime_directory(path: &Path) -> anyhow::Result<()> {
    secure_ancestors(path)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    // Follow legacy aliases for inspection, but leave existing permissions intact.
    // Permission and ownership repairs belong to the operator.
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_dir(),
        "backend runtime parent must be a directory"
    );
    ensure!(
        metadata.uid() == unsafe { nix::libc::geteuid() },
        "backend runtime parent has another owner; repair ownership and run chmod 700 {}",
        path.display()
    );
    ensure!(
        metadata.mode() & 0o022 == 0,
        "backend runtime parent is writable by another user; run chmod 700 {}",
        path.display()
    );
    secure_ancestors(path)?;
    Ok(())
}

fn secure_home(path: &Path, canonical: &Path) -> anyhow::Result<()> {
    secure_ancestors(path)?;
    // Create missing components with private permissions; never repair an existing
    // home silently. The owner must decide how to repair permissions/ownership.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "backend home must be a directory, not a symlink"
    );
    ensure!(
        metadata.uid() == unsafe { nix::libc::geteuid() },
        "backend home has another owner; repair ownership"
    );
    ensure!(
        metadata.mode() & 0o7777 == 0o700,
        "backend home requires mode 0700; repair permissions"
    );
    secure_ancestors(canonical)?;
    // Reject aliases in any component, including symlinked parents and `..`.
    ensure!(
        canonical.as_os_str() == path.as_os_str(),
        "backend home must use its canonical path without aliases"
    );
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimePathError {
    #[error("persisted daemon path differs from verified runtime path")]
    Mismatch,
    #[error(
        "legacy daemon path has U+FFFD; stop daemon and clear its runtime record before enabling multiple backends"
    )]
    LossyLegacy,
}

/// Authority for daemon process effects. Only a successful isolation check may
/// construct this in production. Keep the verified runtime spellings: canonical
/// identities are comparison evidence. Single-backend adoption may
/// authorize the recorded socket for its takeover probe.
pub struct VerifiedRuntimePaths {
    paths: Arc<BackendPaths>,
    identity: BackendPaths,
    multi_backend: bool,
    adoption_socket: Option<PathBuf>,
}
impl VerifiedRuntimePaths {
    pub(crate) fn paths(&self) -> &BackendPaths {
        &self.paths
    }

    pub(crate) fn connection_socket(&self) -> &Path {
        self.adoption_socket
            .as_deref()
            .unwrap_or(&self.paths.socket)
    }

    /// Scope the historical socket authority to one verified live takeover.
    pub(crate) fn for_takeover(
        &self,
        record: &calm_truth::db::SharedCodexDaemonRecord,
    ) -> anyhow::Result<Self> {
        self.check_record(record)?;
        let adoption_socket = if !self.multi_backend {
            // Preserve recorded bytes, including legacy display text, as main did.
            record
                .sock_path
                .as_ref()
                .map(|bytes| PathBuf::from(OsString::from_vec(bytes.clone())))
        } else {
            None
        };
        Ok(Self {
            paths: Arc::clone(&self.paths),
            identity: self.identity.clone(),
            multi_backend: self.multi_backend,
            adoption_socket,
        })
    }

    pub(crate) fn check_record(
        &self,
        record: &calm_truth::db::SharedCodexDaemonRecord,
    ) -> anyhow::Result<()> {
        // Runtime rows never constrain single-backend paths. Process ownership
        // is verified by the supervisor, just as on main.
        if !self.multi_backend {
            return Ok(());
        }
        for (persisted, verified) in [
            (record.sock_path.as_deref(), &self.identity.socket),
            (record.codex_home_path.as_deref(), &self.identity.home),
        ] {
            if let Some(persisted) = persisted {
                let persisted = runtime_record_path(persisted, record.paths_are_legacy_display)?;
                if self.multi_backend {
                    secure_ancestors(&persisted)?;
                }
                ensure!(
                    resolved_path(&persisted)? == *verified,
                    RuntimePathError::Mismatch
                );
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn for_fixture(paths: Arc<BackendPaths>) -> Self {
        Self {
            identity: paths.resolved().expect("fixture paths must resolve"),
            multi_backend: false,
            adoption_socket: None,
            paths,
        }
    }
}

// Legacy TEXT without replacement characters retains its original path bytes.
// Lossy display text cannot establish multi-backend isolation; never guess the
// old writer's spelling or attempt a second encoding.
fn runtime_record_path(bytes: &[u8], legacy_display: bool) -> anyhow::Result<PathBuf> {
    if legacy_display {
        ensure!(
            !std::str::from_utf8(bytes)?.contains('\u{fffd}'),
            RuntimePathError::LossyLegacy
        );
    }
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

struct RegistrationEvidence {
    identities: Vec<BackendIdentity>,
    legacy: PathBuf,
}

pub struct BackendIsolation {
    config: AgentBackendConfig,
    // These exact owners are also consumed by the runtime. Never canonicalize
    // their stored spellings: old daemons and authentication checkpoints use them.
    paths: Vec<Arc<BackendPaths>>,
    frozen: Vec<BackendPaths>,
}
impl BackendIsolation {
    pub async fn prepare(
        cfg: &crate::config::Config,
        repo: &Arc<dyn Repo>,
    ) -> anyhow::Result<Arc<Self>> {
        let config = AgentBackendConfig::for_startup(
            cfg.agent_backend_config.as_deref(),
            &cfg.data_dir_resolved(),
        )?;
        Self::prepare_config(cfg, repo, config).await
    }

    pub(crate) async fn prepare_config(
        cfg: &crate::config::Config,
        repo: &Arc<dyn Repo>,
        config: AgentBackendConfig,
    ) -> anyhow::Result<Arc<Self>> {
        let data = cfg.data_dir_resolved();
        if config.is_multi_backend() {
            secure_ancestors(&data)?;
        }
        std::fs::create_dir_all(&data)?;
        ensure!(
            config.backends.iter().any(|p| p.id.is_default()),
            "P2a requires codex/default"
        );
        let paths: Vec<_> = config
            .backends
            .iter()
            .map(|profile| {
                Arc::new(BackendPaths::new(
                    &data,
                    &profile.id,
                    profile.credentials.path(),
                    &cfg.shared_codex_appserver_log_dir_resolved(),
                ))
            })
            .collect();
        let frozen = paths
            .iter()
            .map(|p| {
                if config.is_multi_backend() {
                    p.resolved()
                } else {
                    Ok(p.as_ref().clone())
                }
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let isolation = Arc::new(Self {
            config,
            paths,
            frozen,
        });
        let (_, registration) = isolation.validate(repo.as_ref()).await?;
        if let Some(RegistrationEvidence { identities, legacy }) = registration {
            let db = repo
                .sqlite_db()
                .context("backend registry requires SQLite")?;
            backend_registry::reconcile(&db, &identities, legacy.as_os_str().as_bytes()).await?;
        }
        Ok(isolation)
    }

    pub fn profiles(&self) -> &[super::config::BackendProfile] {
        &self.config.backends
    }

    fn index(&self, id: &BackendId) -> usize {
        self.config
            .backends
            .iter()
            .position(|p| &p.id == id)
            .expect("configured backend")
    }

    pub fn paths(&self, id: &BackendId) -> &Arc<BackendPaths> {
        &self.paths[self.index(id)]
    }

    pub async fn verify_backend(
        &self,
        repo: &dyn Repo,
        id: &BackendId,
    ) -> anyhow::Result<VerifiedRuntimePaths> {
        let default = self.verify(repo).await?;
        if id.is_default() {
            return Ok(default);
        }
        let paths = self.paths(id);
        let verified = VerifiedRuntimePaths {
            paths: Arc::clone(paths),
            identity: self.frozen[self.index(id)].clone(),
            multi_backend: self.config.is_multi_backend(),
            adoption_socket: None,
        };
        let record = repo.shared_daemon_runtime_get(id.as_str()).await?;
        if !crate::shared_codex_appserver::persisted_process_proven_absent(&record) {
            verified.check_record(&record)?;
        }
        Ok(verified)
    }

    pub fn default_paths(&self) -> &Arc<BackendPaths> {
        let index = self
            .config
            .backends
            .iter()
            .position(|p| p.id.is_default())
            .expect("verified default");
        &self.paths[index]
    }

    pub fn default_home(&self) -> &Path {
        &self.default_paths().home
    }

    pub async fn verify(&self, repo: &dyn Repo) -> anyhow::Result<VerifiedRuntimePaths> {
        Ok(self.validate(repo).await?.0)
    }

    // Runtime verification is read-only: it never enters writer admission.
    async fn validate(
        &self,
        repo: &dyn Repo,
    ) -> anyhow::Result<(VerifiedRuntimePaths, Option<RegistrationEvidence>)> {
        let multi_backend = self.config.is_multi_backend();
        check_environment(multi_backend, |key| std::env::var_os(key))?;
        // Single-backend startup/verify has no isolation policy for runtime rows
        // or filesystem identities. Historical scope belongs only to registration.
        if !multi_backend {
            return Ok((
                VerifiedRuntimePaths {
                    paths: Arc::clone(self.default_paths()),
                    identity: self.default_paths().as_ref().clone(),
                    multi_backend,
                    adoption_socket: None,
                },
                None,
            ));
        }

        // Multi-backend isolation checks every namespace before home creation or
        // registry writes.
        // Frozen values are comparison evidence, never process-effect inputs.
        let mut paths = Vec::new();
        for (runtime, frozen) in self.paths.iter().zip(&self.frozen) {
            for path in runtime.namespaces() {
                secure_ancestors(path)?;
            }
            let current = runtime.resolved()?;
            ensure!(&current == frozen, "backend runtime path identity changed");
            paths.push(current);
        }

        for (i, left) in paths.iter().enumerate() {
            for (j, right) in paths.iter().enumerate() {
                if i == j {
                    continue;
                }
                ensure!(!left.home.starts_with(&right.home), "backend homes overlap");
                for derived in right.derived_namespaces() {
                    ensure!(
                        !left.home.starts_with(derived) && !derived.starts_with(&left.home),
                        "backend home overlaps another backend's derived path"
                    );
                }
            }
            for right in paths.iter().skip(i + 1) {
                for a in left.private_namespaces() {
                    for b in right.private_namespaces() {
                        ensure!(
                            !a.starts_with(b) && !b.starts_with(a),
                            "backend runtime paths overlap"
                        );
                    }
                }
            }
        }
        for runtime in &self.paths {
            for file in [
                &runtime.socket,
                &runtime.authentication,
                &runtime.kernel_mcp_socket,
                &runtime.logs,
            ] {
                secure_runtime_directory(file.parent().context("runtime path has no parent")?)?;
            }
            // stdout.log and stderr.log are created inside this directory.
            secure_runtime_directory(&runtime.logs)?;
            for name in ["stdout.log", "stderr.log"] {
                secure_ancestors(&runtime.logs.join(name))?;
            }
        }

        for (runtime, current) in self.paths.iter().zip(&paths) {
            secure_home(&runtime.home, &current.home)?;
        }
        let default_index = self
            .config
            .backends
            .iter()
            .position(|profile| profile.id.is_default())
            .expect("verified default");
        let record = repo.shared_daemon_runtime_get("default").await?;
        let verified = VerifiedRuntimePaths {
            paths: Arc::clone(self.default_paths()),
            identity: paths[default_index].clone(),
            multi_backend,
            adoption_socket: None,
        };
        // Stale process paths cannot authorize effects and must not block recovery.
        // Credential scope below remains permanent even for a dead process.
        if !crate::shared_codex_appserver::persisted_process_proven_absent(&record) {
            verified.check_record(&record)?;
        }
        // Historical scope evidence belongs only to the multi-backend
        // registration checks. Preserve their existing runtime revalidation.
        let identities: Vec<_> = self
            .config
            .backends
            .iter()
            .zip(&self.paths)
            .zip(&self.frozen)
            .map(|((profile, runtime), frozen)| BackendIdentity {
                provider: profile.provider.wire_name().to_owned(),
                backend_id: profile.id.as_str().to_owned(),
                protocol: "codex".into(),
                configured_home: runtime.home.as_os_str().as_bytes().to_vec(),
                canonical_home: frozen.home.as_os_str().as_bytes().to_vec(),
            })
            .collect();
        let legacy = record
            .codex_home_path
            .as_deref()
            .map(|bytes| runtime_record_path(bytes, record.paths_are_legacy_display))
            .transpose()?
            .unwrap_or_else(|| self.default_paths().legacy_home());
        secure_ancestors(&legacy)?;
        // A missing historical path cannot be replaced by a newly configured home.
        let legacy = resolved_path(&legacy)?;
        Ok((verified, Some(RegistrationEvidence { identities, legacy })))
    }
}

pub fn default_id() -> BackendId {
    BackendId::try_from("default".to_owned()).expect("valid default id")
}
