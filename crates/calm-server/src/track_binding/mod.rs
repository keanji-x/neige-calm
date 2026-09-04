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
//! ## The rule now: two questions, one resolver
//!
//! `tracks.plugin_scope` is **the** owner column, and it answers the *only*
//! question both readers must agree on: **who owns this track**. Everything
//! about the *template contract* — does that owner still declare the track's
//! `template_id`, does its current `input_schema` still accept the persisted
//! `template_input` — is a second, subordinate question, answered on top of a
//! known owner and consumed by the planner alone.
//!
//! [`resolve_track_owner_binding`] returns both, and the two readers project
//! different parts of it:
//!
//! | state | tool scope | planner |
//! |---|---|---|
//! | `plugin_scope IS NULL` | `All` (historical union) | vanilla |
//! | owner set, not running ∨ not trusted ∨ not in registry ∨ no host | `None` (fail closed) | vanilla |
//! | owner live, contract honored | `Only(owner)` | descriptor + input |
//! | owner live, contract broken | `Only(owner)` | vanilla + `error!` |
//!
//! ### Why the last row does not fail closed on the tool side
//!
//! The first cut collapsed both questions into one verdict, so a broken
//! contract withdrew the tool scope too. That is not conservative, it is
//! destructive: `TrackPatch` (`calm-truth/src/model.rs`) carries no
//! `template_id` / `plugin_scope` / `template_input`, so **no API call can
//! repair those three columns**. One backwards-incompatible `input_schema`
//! bump by the owner would therefore drop a working track to "vanilla prompt
//! **and** zero plugin tools" permanently — recoverable only by downgrading
//! the plugin or recreating the track.
//!
//! Nothing about tool authorization depends on the template contract: the
//! owner's identity is proven by `plugin_scope` alone, and the original
//! escalation (successor **B** adopting A's track) is already closed by
//! looking the manifest up *by A* instead of searching for the template id.
//! What a stale contract must prevent is injecting an unchecked descriptor or
//! input into the prompt — which is exactly, and only, the planner side.
//!
//! `tracks.template_id` did not stop mattering; it stopped being a *search
//! key*. It is now only checked **against the recorded owner**.
//!
//! ## Who writes `plugin_scope`
//!
//! Not one writer — three, and the earlier claim here ("written once, by
//! `routes::tracks::create_track` … and `PATCH` cannot change it") was false
//! in its first half:
//!
//! 1. `routes::tracks::create_track` — copies the admitted template's binding
//!    into the column. The only writer that pairs it with a `template_id`.
//! 2. `operation::child_track_adapter` — a child track `SELECT`s its parent's
//!    `plugin_scope` and passes the non-NULL value to `track_create_tx` with
//!    `template_id: None`. It is a **production** path (one of the four
//!    track-create entry points), and it is what actually produces the
//!    `plugin_scope = Some(_) ∧ template_id = None` shape below — not just
//!    test fixtures.
//! 3. `routes::today.rs`'s launchpad adoption — `UPDATE tracks SET
//!    purpose='launchpad', template_id=NULL, plugin_scope=NULL,
//!    template_input=NULL`, i.e. `Only(X) → All`, a **widening**. All three
//!    columns are cleared in one statement, so the row stays self-consistent
//!    (an owner is never dropped while a template id survives), but "the
//!    column is only ever added, never changed" is not true of it.
//!
//! `PATCH /api/tracks` genuinely cannot touch it (INV-1110-004,
//! `calm-truth/src/model.rs`) — that half of the old sentence stands, and it
//! is also why the last table row above cannot fail closed.
//!
//! ## KNOWN GAP — resolution time, not resolution column
//!
//! "Who owns this track" has exactly one answer **per resolution**; it is not
//! pinned across time. `operation::planner_harness_start_adapter` resolves
//! once, at planner thread start, and freezes the descriptor into the
//! developer instructions, while `mcp_server::tool_visibility` re-resolves on
//! every `tools/list` / `tools/call`. An owner that stops *after* the planner
//! started therefore leaves a durable mismatch: A's descriptor is still in the
//! prompt while the tool scope has already gone to `None`.
//!
//! That is the same *shape* as the bug this slice fixes, but not the same
//! cause: it comes from **when** each reader asks, not from which column it
//! reads, it predates this PR and this PR does not change it. Closing it means
//! either re-rendering developer instructions on owner transitions or pinning
//! a resolution snapshot for the life of a thread — both are prompt-lifecycle
//! work well outside a binding-resolution slice. Registered here, not fixed.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::forge_trust::trusted_forge_plugin;
use crate::model::Track;
use crate::plugin_host::manifest::TemplateDescriptor;
use crate::plugin_host::template_input::validate_template_input_binding;
use crate::plugin_host::{Manifest, PluginHost};

#[cfg(test)]
mod tests;

/// The resolved owner of one track. Produced only by
/// [`resolve_track_owner_binding`].
///
/// The enum answers **owner identity** only. Whether the track's *template
/// contract* still holds is a separate field on [`Self::Owned`]
/// ([`TemplateContract`]) precisely so a broken contract cannot silently
/// un-own a track — see the module doc.
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
    /// `plugin_scope` names a plugin that is running ∧ trusted and present in
    /// the registry. This is the *whole* owner judgement: the tool scope is
    /// `Only(plugin.id)` here regardless of `contract`.
    Owned {
        /// Boxed: a `Manifest` is ~900 bytes and would otherwise make every
        /// `TrackOwnerBinding` that large (`clippy::large_enum_variant`).
        plugin: Box<Manifest>,
        /// Whether the track's template contract is still usable under that
        /// owner's *current* manifest. Consumed by the planner only.
        contract: TemplateContract,
    },
    /// `plugin_scope` is set but names no usable owner. Both readers degrade:
    /// the planner falls back to the vanilla prompt, the MCP scope exposes
    /// zero plugin tools. This is the fail-closed row, and the only one.
    OwnerUnavailable { plugin_id: String },
}

impl TrackOwnerBinding {
    /// The plugin id the track is scoped to, whatever became of it — for log
    /// fields on both readers.
    pub(crate) fn scoped_plugin_id(&self) -> Option<&str> {
        match self {
            Self::Unbound => None,
            Self::Owned { plugin, .. } => Some(plugin.id.as_str()),
            Self::OwnerUnavailable { plugin_id } => Some(plugin_id.as_str()),
        }
    }
}

/// The state of a track's *template* contract under a **known, live** owner.
#[derive(Clone, Debug)]
pub(crate) enum TemplateContract {
    /// The track carries no `template_id` at all, so there is no contract to
    /// honor or break. Produced in production by
    /// `operation::child_track_adapter`, which inherits the parent's
    /// `plugin_scope` with `template_id: None` (see the module doc); the
    /// planner runs vanilla and the tool scope stays `Only(owner)` — #1110 S4
    /// pins exactly that.
    NotTemplated,
    /// The owner still declares the track's `template_id`, and its **current**
    /// `input_schema` still accepts the persisted `template_input` under the
    /// same matrix the create route enforces.
    Honored {
        template: TemplateDescriptor,
        /// The row's `template_input`, re-checked against `plugin`'s current
        /// schema — never a blob that only an older schema accepted.
        input: Option<Value>,
    },
    /// The owner is live but the contract cannot be honored: nothing template
    /// -shaped may reach the prompt. Tool visibility is unaffected.
    Broken(ContractFailure),
}

/// Why a live owner's template contract is unusable. Carried (not just
/// logged) so the reader can emit the diagnostic from its own target.
#[derive(Clone, Debug)]
pub(crate) enum ContractFailure {
    /// The owner's Manifest no longer declares the track's `template_id` — an
    /// upgrade dropped the template out from under a track created against it.
    TemplateNotDeclared {
        plugin_id: String,
        template_id: String,
    },
    /// The owner still declares the template, but the persisted
    /// `template_input` (including its **absence**) no longer satisfies the
    /// create-time binding matrix against its current `input_schema`.
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

/// The one predicate that decides whether a plugin may act as a template
/// owner at all: it is **running** and it is a **trusted** forge plugin.
///
/// Shared with `routes::tracks::resolve_template_binding`, which asks the same
/// question at create time about a candidate rather than about a recorded
/// owner — same filter, one definition **of owner eligibility**. It is
/// deliberately not the only predicate over plugins and templates in the
/// crate: `plugin_host::find_template_conflict` asks the wider "who is
/// holding this template id", which includes admission reservations and is
/// correct to answer over a larger set.
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

    // #1321 S1 — the input was checked at create time against the schema the
    // owner declared *then*. A plugin upgrade can change it without touching
    // the row, so the check is redone against the owner's current manifest
    // before the blob is handed to anyone.
    //
    // 第一轮评审 MAJOR-1: this used to be `if let Some(input) = …` around a
    // bare `validate_template_input`, which is only the `(Some(schema),
    // Some(input))` corner of the create-time matrix — a NULL
    // `template_input` skipped the check entirely, so an owner upgrade that
    // *added* `required: [...]` left a track the create route would now
    // reject (400) resolving as fully honored. The whole matrix is one
    // function and this calls it, absence included.
    if let Err(reason) =
        validate_template_input_binding(Some(manifest), track.template_input.as_ref())
    {
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
