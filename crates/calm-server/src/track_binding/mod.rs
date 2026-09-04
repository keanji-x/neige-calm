//! #1321 S1 — the per-track owner binding, resolved in exactly one place.
//!
//! ## What was wrong
//!
//! Two readers answered "which plugin owns this track" from two different
//! columns:
//!
//! * the planner harness's `bound_template` read `tracks.template_id` and
//!   scanned **every** running ∧ trusted plugin for a matching descriptor;
//! * the MCP per-track tool scope (`mcp_server::tool_visibility`) read
//!   `tracks.plugin_scope` only.
//!
//! Those two agree only as long as the plugin that declared a template id at
//! create time is still the one declaring it. It need not be:
//! `plugin_template_uniqueness` deliberately frees a template id when its
//! trusted holder stops, so plugin **B** may take over the id that plugin
//! **A** held when the track was created. In that state the planner adopted
//! B's descriptor and injected the `template_input` that only **A**'s
//! `input_schema` ever validated, while the tool scope stayed locked on the
//! stopped A and withdrew every plugin tool. With A's and B's `input_schema`
//! differing, the agent got a prompt whose contract nobody had checked.
//!
//! ## The rule now
//!
//! `tracks.plugin_scope` is **the** owner column. It is written once, by
//! `routes::tracks::create_track`, from the admitted template's binding, and
//! `PATCH` cannot change it (INV-1110-004). Everything downstream —
//! descriptor, `template_input`, tool visibility — is derived from that one
//! column by [`resolve_track_owner_binding`], and both readers call it.
//!
//! `tracks.template_id` did not stop mattering; it stopped being a *search
//! key*. It is now only checked **against the recorded owner**: the owner
//! must still declare it, and the persisted input must still satisfy the
//! owner's *current* `input_schema`. Both checks fail closed, on both
//! readers, because "who owns this track" must have exactly one answer.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::forge_trust::trusted_forge_plugin;
use crate::model::Track;
use crate::plugin_host::manifest::TemplateDescriptor;
use crate::plugin_host::template_input::validate_template_input;
use crate::plugin_host::{Manifest, PluginHost};

#[cfg(test)]
mod tests;

/// The resolved owner of one track. Produced only by
/// [`resolve_track_owner_binding`].
#[derive(Clone, Debug)]
pub(crate) enum TrackOwnerBinding {
    /// `plugin_scope IS NULL` — no owner was ever recorded for this track.
    ///
    /// This is a *terminal* answer, not a "look harder" one. A running plugin
    /// that happens to declare the row's `template_id` does **not** adopt the
    /// track: the create that would have bound it ran while no owner was
    /// available and deliberately stored NULL, and nothing after create may
    /// promote a track into a plugin's scope.
    Unbound,
    /// `plugin_scope` names a plugin that is running ∧ trusted, still
    /// declares the track's `template_id` (when the track has one), and whose
    /// current `input_schema` still accepts the persisted `template_input`.
    Owned {
        /// Boxed: a `Manifest` is ~900 bytes and would otherwise make every
        /// `TrackOwnerBinding` that large (`clippy::large_enum_variant`).
        plugin: Box<Manifest>,
        /// `None` when the track carries no `template_id` at all. (The create
        /// route cannot produce `plugin_scope` without `template_id`; the
        /// repo-level writer used by fixtures can.)
        template: Option<TemplateDescriptor>,
        /// The row's `template_input`, re-validated against `plugin`'s
        /// current schema — never a blob that only an older schema accepted.
        input: Option<Value>,
    },
    /// `plugin_scope` is set but the binding cannot be honored. Both readers
    /// degrade: the planner falls back to the vanilla prompt, the MCP scope
    /// exposes zero plugin tools.
    FailedClosed(BindingFailure),
}

/// Why a scoped track has no usable owner. Carried (not just logged) so both
/// readers can emit the same diagnostic from their own target.
#[derive(Clone, Debug)]
pub(crate) enum BindingFailure {
    /// The owner is stopped, no longer trusted, not in the registry, or the
    /// plugin host is not up yet.
    OwnerUnavailable { plugin_id: String },
    /// The owner is live, but its Manifest no longer declares the track's
    /// `template_id` — an upgrade dropped the template out from under a track
    /// created against it.
    TemplateNotDeclared {
        plugin_id: String,
        template_id: String,
    },
    /// The owner is live and still declares the template, but the persisted
    /// `template_input` does not satisfy its **current** `input_schema`.
    StaleTemplateInput {
        plugin_id: String,
        template_id: String,
        reason: String,
    },
}

impl BindingFailure {
    /// The plugin the track is scoped to, for log fields.
    pub(crate) fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::OwnerUnavailable { plugin_id }
            | Self::TemplateNotDeclared { plugin_id, .. }
            | Self::StaleTemplateInput { plugin_id, .. } => Some(plugin_id),
        }
    }
}

impl std::fmt::Display for BindingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnerUnavailable { plugin_id } => write!(
                f,
                "owner plugin `{plugin_id}` is not currently running and trusted"
            ),
            Self::TemplateNotDeclared {
                plugin_id,
                template_id,
            } => write!(
                f,
                "owner plugin `{plugin_id}` no longer declares template `{template_id}`"
            ),
            Self::StaleTemplateInput {
                plugin_id,
                template_id,
                reason,
            } => write!(
                f,
                "persisted template_input for `{template_id}` no longer satisfies \
                 owner `{plugin_id}`'s current input_schema: {reason}"
            ),
        }
    }
}

/// The one predicate that decides whether a plugin may act as a template
/// owner at all: it is **running** and it is a **trusted** forge plugin.
///
/// Shared with `routes::tracks::resolve_template_binding`, which asks the same
/// question at create time about a candidate rather than about a recorded
/// owner — same filter, one definition.
pub(crate) fn plugin_is_eligible_owner(running: &BTreeSet<String>, plugin_id: &str) -> bool {
    running.contains(plugin_id) && trusted_forge_plugin(plugin_id)
}

/// Resolve the owner binding for one already-read track row.
///
/// `plugin` is `None` when the plugin host is not wired yet (MCP boot
/// ordering); a scoped track then fails closed, an unbound one does not.
pub(crate) async fn resolve_track_owner_binding(
    track: &Track,
    plugin: Option<&PluginHost>,
) -> TrackOwnerBinding {
    let Some(plugin_id) = track.plugin_scope.as_deref() else {
        return TrackOwnerBinding::Unbound;
    };
    let unavailable = || {
        TrackOwnerBinding::FailedClosed(BindingFailure::OwnerUnavailable {
            plugin_id: plugin_id.to_string(),
        })
    };
    let Some(host) = plugin else {
        return unavailable();
    };
    if !plugin_is_eligible_owner(&host.running_plugin_ids().await, plugin_id) {
        return unavailable();
    }
    // A running plugin whose registry entry vanished cannot be asked what it
    // declares, so it cannot serve as an owner either.
    let Some(manifest) = host.registry().get(plugin_id) else {
        return unavailable();
    };

    let Some(template_id) = track.template_id.as_deref() else {
        // Scoped without a template id. Not reachable through the create
        // route (`plugin_scope` is copied off an admitted template's
        // binding), but the repo-level writer permits it and #1110 S4 pins
        // the tool scope for it: the owner is still the owner.
        return TrackOwnerBinding::Owned {
            plugin: Box::new(manifest),
            template: None,
            input: None,
        };
    };

    let Some(descriptor) = manifest
        .templates
        .iter()
        .find(|descriptor| descriptor.id == template_id)
        .cloned()
    else {
        return TrackOwnerBinding::FailedClosed(BindingFailure::TemplateNotDeclared {
            plugin_id: plugin_id.to_string(),
            template_id: template_id.to_string(),
        });
    };

    // #1321 S1 — the input was validated at create time against the schema the
    // owner declared *then*. A plugin upgrade can change it without touching
    // the row, so the check is redone against the owner's current schema
    // before the blob is handed to anyone. Same function the create route
    // validates with (`validate_template_input`), not a restatement of it.
    if let Some(input) = track.template_input.as_ref() {
        let stale = |reason: String| {
            TrackOwnerBinding::FailedClosed(BindingFailure::StaleTemplateInput {
                plugin_id: plugin_id.to_string(),
                template_id: template_id.to_string(),
                reason,
            })
        };
        let Some(schema) = manifest.input_schema.as_ref() else {
            return stale("owner no longer declares an input_schema".to_string());
        };
        if let Err(reason) = validate_template_input(schema, input) {
            return stale(reason);
        }
    }

    TrackOwnerBinding::Owned {
        plugin: Box::new(manifest),
        template: Some(descriptor),
        input: track.template_input.clone(),
    }
}
