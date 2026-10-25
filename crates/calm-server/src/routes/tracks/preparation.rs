//! Shared mint preparation. Replay must be selected before entering this mutable service.
use super::*;

/// The **one** starting point a create request names, before admission.
pub(super) enum NamedSource {
    /// No starting point named; the track keeps the default skeleton.
    Blank,
    /// A built-in roster template, still the caller's spelling — not yet admitted.
    Template(String),
    /// A user-defined recipe row.
    Recipe(String),
    /// An existing track whose report is copied.
    Fork(String),
}

impl NamedSource {
    /// Collapse the three request fields, or refuse and say which two collided.
    pub(super) fn from_request(
        template_id: Option<String>,
        recipe_id: Option<String>,
        fork_report_from: Option<String>,
    ) -> Result<Self> {
        let mut named: Vec<(&'static str, Self)> = [
            template_id.map(|id| ("template_id", Self::Template(id))),
            recipe_id.map(|id| ("recipe_id", Self::Recipe(id))),
            fork_report_from.map(|id| ("fork_report_from", Self::Fork(id))),
        ]
        .into_iter()
        .flatten()
        .collect();
        match named.len() {
            0 => Ok(Self::Blank),
            1 => Ok(named.pop().expect("one named source").1),
            _ => {
                let fields: Vec<&str> = named.iter().map(|(field, _)| *field).collect();
                let (last, rest) = fields.split_last().expect("at least two named sources");
                Err(CalmError::BadRequest(format!(
                    "track create: `{}` and `{last}` each name a starting point for the \
                     new track's report; give at most one",
                    rest.join("`, `"),
                )))
            }
        }
    }

    /// Admit the named source, producing the value that decides both which initialization
    /// runs and what the row records about it.
    async fn resolve(self, s: &RouteState) -> Result<CreationSource> {
        Ok(CreationSource {
            init: match self {
                Self::Blank => TrackInit::Blank,
                Self::Recipe(recipe_id) => TrackInit::Recipe { recipe_id },
                Self::Fork(source_track_id) => TrackInit::Fork { source_track_id },
                Self::Template(template_id) => {
                    // Roster membership is the whole admission test; a plugin binding is an
                    // attribute of the template, not a second way in.
                    let admission = admit_template(s, &template_id).await.ok_or_else(|| {
                        CalmError::BadRequest(format!(
                            "track create: `template_id` must reference a known track template; got `{template_id}`"
                        ))
                    })?;
                    if let Some(required) =
                        missing_required_template_owner(admission.key(), admission.binding.as_ref())
                    {
                        return Err(CalmError::BadRequest(format!(
                            "track create: template `{template_id}` requires `{}`; enable its built-in capability in Settings",
                            required.manifest().id
                        )));
                    }
                    TrackInit::Template {
                        key: admission.key(),
                        capabilities: admission.template.capabilities(),
                        binding: admission.binding.map(Box::new),
                    }
                }
            },
        })
    }
}

/// An admitted creation source: the initialization that will run, and — derived from that
/// same value — the provenance the `tracks` row records.
struct CreationSource {
    init: TrackInit,
}

impl CreationSource {
    /// `None` binding has two causes the 400 must distinguish: no `template_id` at all, or an
    /// admitted one whose owning plugin is not running and trusted.
    fn template_input_owner(&self) -> crate::plugin_host::template_input::TemplateInputOwner<'_> {
        use crate::plugin_host::template_input::TemplateInputOwner;
        match &self.init {
            TrackInit::Template {
                binding: Some(manifest),
                ..
            } => TemplateInputOwner::Plugin(manifest),
            TrackInit::Template { binding: None, .. } => TemplateInputOwner::NoBoundPlugin,
            TrackInit::Blank | TrackInit::Recipe { .. } | TrackInit::Fork { .. } => {
                TemplateInputOwner::NoTemplateId
            }
        }
    }

    /// Write this source's provenance onto the row; both columns come out of one `match` arm
    /// so they cannot name different sources.
    fn stamp(&self, p: &mut NewTrack) {
        let (template_id, plugin_scope) = match &self.init {
            TrackInit::Template { key, binding, .. } => (
                Some((*key).to_string()),
                binding.as_ref().map(|manifest| manifest.id.clone()),
            ),
            TrackInit::Blank | TrackInit::Recipe { .. } | TrackInit::Fork { .. } => (None, None),
        };
        p.template_id = template_id;
        p.plugin_scope = plugin_scope;
    }
}

pub(super) struct PreparedCreate {
    pub(super) p: NewTrack,
    pub(super) folder_claim: FolderClaim,
    pub(super) body_area_id: String,
    pub(super) normalized_cwd: String,
    pub(super) init: TrackInit,
    pub(super) workspace_plan: TrackWorkspacePlan,
    pub(super) conflict: FolderConflictSlot,
}

pub(super) async fn prepare(
    s: &RouteState,
    p: NewTrack,
    named_source: NamedSource,
    isolation: CreateIsolation,
    allow_cross_area_cwd: Option<CrossAreaCwdAuthorization>,
) -> Result<PreparedCreate> {
    prepare_with_area(
        s,
        p,
        named_source,
        isolation,
        allow_cross_area_cwd,
        PreparationArea::Lookup,
    )
    .await
}

/// Canonical allocation already holds the authoritative Area admission. Reuse
/// that fact without leasing reader capacity while holding the writer.
pub(super) async fn prepare_in_area(
    s: &RouteState,
    p: NewTrack,
    named_source: NamedSource,
    area_kind: AreaKind,
    isolation: CreateIsolation,
    allow_cross_area_cwd: Option<CrossAreaCwdAuthorization>,
) -> Result<PreparedCreate> {
    prepare_with_area(
        s,
        p,
        named_source,
        isolation,
        allow_cross_area_cwd,
        PreparationArea::Admitted(area_kind),
    )
    .await
}

enum PreparationArea {
    Lookup,
    Admitted(AreaKind),
}

async fn prepare_with_area(
    s: &RouteState,
    mut p: NewTrack,
    named_source: NamedSource,
    isolation: CreateIsolation,
    allow_cross_area_cwd: Option<CrossAreaCwdAuthorization>,
    area: PreparationArea,
) -> Result<PreparedCreate> {
    let cwd_omitted = isolation == CreateIsolation::Managed;
    // Validate cwd before opening the tx; the route owns every cross-area check so
    // `track_create_tx` stays a mechanical row insert. `materialize_workspace` runs after the
    // commit, so "non-201 ⇒ no side effect" is not a property of this handler.

    // The request's named source becomes the admitted source here, and only here.
    let source = named_source.resolve(s).await?;
    // `template_input` is only accepted against a bound template whose owning plugin
    // Manifest declares an `input_schema`.
    validate_template_input_binding(source.template_input_owner(), p.template_input.as_ref())?;
    // Both provenance columns, from the one value that also decides the init.
    source.stamp(&mut p);

    // Omitted cwd stores `default_cwd()` and skips the claim scan. Never claim `$HOME` —
    // longest-prefix would poison every other area.
    if !cwd_omitted && !p.cwd.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "track create: `cwd` must be absolute (start with `/`); got `{}`",
            p.cwd
        )));
    }
    let normalized_cwd = normalize_path(&p.cwd);
    // Validated before the transaction, deliberately: `materialize_workspace` runs after
    // the commit, and a failure there leaves an orphan track row.
    if isolation == CreateIsolation::Attached(AttachedIsolation::Worktree) {
        workspace_runtime::selection::validate_attached_workspace(std::path::Path::new(
            &normalized_cwd,
        ))?;
    }
    // Stamp the normalized cwd back before the row is minted; `area_folder.path` is the
    // normalized form too.
    p.cwd = normalized_cwd.clone();

    // The system area is exempt from the `area_folders` claim namespace: claiming a path
    // under it would poison every real area's descendant check.
    let area_kind = match area {
        PreparationArea::Admitted(kind) => Some(kind),
        PreparationArea::Lookup => match s.repo.area_get(p.area_id.as_str()).await? {
            Some(area) => Some(area.kind),
            // Defer the missing-Area refusal until the authoritative ownership check
            // in the creation transaction. This branch can never mint a track.
            None if isolation == CreateIsolation::Attached(AttachedIsolation::None) => None,
            None => return Err(CalmError::NotFound(format!("area `{}`", p.area_id))),
        },
    };
    let is_system_area = area_kind == Some(AreaKind::System);
    if allow_cross_area_cwd.is_some() && (is_system_area || cwd_omitted) {
        return Err(CalmError::BadRequest(
            "cross-area cwd authorization requires an explicit cwd in a user area".into(),
        ));
    }
    if is_system_area {
        p.attach_folder = false;
    }

    let attach_folder = p.attach_folder;
    let body_area_id = p.area_id.as_str().to_string();

    // The cwd-vs-claim decision runs inside the track-create transaction: a pre-tx scan let
    // two concurrent creates for `/a` and `/a/b` commit overlapping claims.
    let conflict = FolderConflictSlot::default();
    let folder_claim = if area_kind.is_none() {
        FolderClaim::MissingArea
    } else if is_system_area || cwd_omitted {
        FolderClaim::Skip
    } else {
        FolderClaim::Enforce {
            attach: attach_folder,
            allow_cross_area_cwd,
            conflict: conflict.clone(),
        }
    };

    let init = source.init;

    // Only worktree isolation resolves a repository root; none borrows the cwd.
    // Both attached ownership proofs run in the mint's write transaction.
    let workspace_plan = match isolation {
        CreateIsolation::Managed => TrackWorkspacePlan::ManagedUnder(s.workspace_root.clone()),
        CreateIsolation::Attached(AttachedIsolation::None) => {
            workspace_runtime::selection::validate_borrowed_workspace(std::path::Path::new(
                &normalized_cwd,
            ))?;
            TrackWorkspacePlan::AttachedFromCwd
        }
        CreateIsolation::Attached(AttachedIsolation::Worktree) => {
            let repo_root = workspace_runtime::selection::git_repo_root_for_track_cwd(
                "(new)",
                &normalized_cwd,
            )?;
            // Git can print a noncanonical core.worktree. Persist the physical
            // repository root so the mechanism can enforce path equality.
            let repo_root = std::fs::canonicalize(&repo_root).map_err(|error| {
                CalmError::BadRequest(format!(
                    "cannot resolve repository root {}: {error}",
                    repo_root.display()
                ))
            })?;
            TrackWorkspacePlan::AttachedWithTrackWorktree(repo_root)
        }
    };
    Ok(PreparedCreate {
        p,
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        workspace_plan,
        conflict,
    })
}

#[cfg(test)]
mod capability_tests;
