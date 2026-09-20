//! The per-track owner binding, resolved in exactly one place: `tracks.plugin_scope` is the
//! owner; the template contract is a separate judgement under that owner, so a broken contract
//! cannot silently un-own a track.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::forge_trust::trusted_forge_plugin;
use crate::model::Track;
use crate::plugin_host::manifest::TemplateDescriptor;
use crate::plugin_host::template_input::{TemplateInputOwner, validate_template_input_binding};
use crate::plugin_host::{Manifest, PluginHost};

#[cfg(test)]
mod tests;

/// The resolved owner of one track. Answers owner identity only; the template contract is a
/// separate field on [`Self::Owned`] so a broken contract cannot silently un-own a track.
#[derive(Clone, Debug)]
pub(crate) enum TrackOwnerBinding {
    /// `plugin_scope IS NULL` — this track has no owner now. A terminal answer: a running plugin
    /// that happens to declare the row's `template_id` does NOT adopt the track (no runtime path
    /// writes a non-NULL `plugin_scope` onto an existing row).
    Unbound,
    /// `plugin_scope` names a plugin that is running ∧ trusted and in the registry. The tool scope
    /// is `Only(plugin.id)` here regardless of `contract`.
    Owned {
        /// Boxed: a `Manifest` is ~900 bytes (`clippy::large_enum_variant`).
        plugin: Box<Manifest>,
        /// Whether the template contract is still usable under the owner's current manifest. Planner only.
        contract: TemplateContract,
    },
    /// `plugin_scope` is set but names no usable owner: the planner falls back to the vanilla
    /// prompt, the MCP scope exposes zero plugin tools. The fail-closed row.
    OwnerUnavailable { plugin_id: String },
}

/// The state of a track's *template* contract under a **known, live** owner.
#[derive(Clone, Debug)]
pub(crate) enum TemplateContract {
    /// No `template_id` at all: the planner runs vanilla and the tool scope stays `Only(owner)`.
    NotTemplated,
    /// The owner still declares the `template_id` and its CURRENT `input_schema` still accepts the
    /// persisted `template_input` under the create-time matrix.
    Honored {
        template: TemplateDescriptor,
        /// Re-checked against the owner's current schema — never a blob only an older schema accepted.
        input: Option<Value>,
    },
    /// The owner is live but the contract cannot be honored: nothing template-shaped may reach
    /// the prompt. Tool visibility is unaffected. Both consumers project this and `NotTemplated`
    /// onto the same return value; only the planner's `error!` log tells them apart.
    Broken(ContractFailure),
}

/// Carried (not just logged) so the reader can emit the diagnostic from its own target.
#[derive(Clone, Debug)]
pub(crate) enum ContractFailure {
    /// An upgrade dropped the template out from under a track created against it.
    TemplateNotDeclared {
        plugin_id: String,
        template_id: String,
    },
    /// The persisted `template_input` (including its absence) no longer satisfies the create-time
    /// binding matrix against the owner's current `input_schema`.
    InputRejected {
        plugin_id: String,
        template_id: String,
        reason: String,
    },
}

impl std::fmt::Display for ContractFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TemplateNotDeclared {
                plugin_id,
                template_id,
            } => write!(
                f,
                "owner plugin `{plugin_id}` no longer declares template `{template_id}`"
            ),
            Self::InputRejected {
                plugin_id,
                template_id,
                reason,
            } => write!(
                f,
                "persisted template_input for `{template_id}` is no longer accepted under \
                 owner `{plugin_id}`'s current input_schema: {reason}"
            ),
        }
    }
}

/// The one predicate of owner eligibility (running ∧ trusted), shared with the create route.
/// `plugin_host::find_template_conflict` deliberately answers the wider question over admission reservations too.
pub(crate) fn plugin_is_eligible_owner(running: &BTreeSet<String>, plugin_id: &str) -> bool {
    running.contains(plugin_id) && trusted_forge_plugin(plugin_id)
}

/// `plugin` is `None` when the plugin host is not wired yet (MCP boot ordering); a scoped track
/// then fails closed, an unbound one does not.
pub(crate) async fn resolve_track_owner_binding(
    track: &Track,
    plugin: Option<&PluginHost>,
) -> TrackOwnerBinding {
    let Some(plugin_id) = track.plugin_scope.as_deref() else {
        return TrackOwnerBinding::Unbound;
    };
    let unavailable = || TrackOwnerBinding::OwnerUnavailable {
        plugin_id: plugin_id.to_string(),
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

    let contract = resolve_template_contract(track, &manifest, plugin_id);
    TrackOwnerBinding::Owned {
        plugin: Box::new(manifest),
        contract,
    }
}

/// The contract half, over a live owner's current manifest.
fn resolve_template_contract(
    track: &Track,
    manifest: &Manifest,
    plugin_id: &str,
) -> TemplateContract {
    let Some(template_id) = track.template_id.as_deref() else {
        return TemplateContract::NotTemplated;
    };

    let Some(descriptor) = manifest
        .templates
        .iter()
        .find(|descriptor| descriptor.id == template_id)
        .cloned()
    else {
        return TemplateContract::Broken(ContractFailure::TemplateNotDeclared {
            plugin_id: plugin_id.to_string(),
            template_id: template_id.to_string(),
        });
    };

    // A plugin upgrade can change the schema without touching the row, so the check is redone
    // against the owner's current manifest, absence of input included (an upgrade that added
    // `required` must not leave the track resolving as honored).
    if let Err(reason) = validate_template_input_binding(
        TemplateInputOwner::Plugin(manifest),
        track.template_input.as_ref(),
    ) {
        return TemplateContract::Broken(ContractFailure::InputRejected {
            plugin_id: plugin_id.to_string(),
            template_id: template_id.to_string(),
            reason,
        });
    }

    TemplateContract::Honored {
        template: descriptor,
        input: track.template_input.clone(),
    }
}
