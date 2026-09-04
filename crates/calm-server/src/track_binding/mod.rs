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
//! Not one writer. The earlier claim here ("written once, by
//! `routes::tracks::create_track` … and `PATCH` cannot change it") was false
//! in its first half. Enumerated by sweeping every mention of the column
//! across `crates/calm-server/src`, `crates/calm-truth/src` and
//! `crates/calm-truth/migrations` — three runtime writers plus one migration:
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
//! 4. Migration 0076 (`crates/calm-truth/migrations`, the one that adds the
//!    column) — a one-time backfill that derived the column for pre-existing
//!    rows. Not a runtime writer, but it is where the oldest values in the
//!    column came from, so "create wrote every value in this column" is false
//!    of them too.
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
//! every `tools/list` / `tools/call`. That asymmetry has **two** instances,
//! and this PR's relationship to them is not the same:
//!
//! 1. **The owner stops while the thread lives.** A's descriptor is still in
//!    the prompt while the tool scope has already gone to `None`. Same *shape*
//!    as the bug this slice fixes, different cause: it comes from **when** each
//!    reader asks, not from which column it reads. It predates this PR and this
//!    PR does not change it — the old `tool_visibility` failed closed on a
//!    stopped owner too, and the old `bound_template` was likewise called once.
//! 2. **The contract breaks while the thread lives** (owner still running ∧
//!    trusted; only its `input_schema` or its template list moved). This one is
//!    **new with this slice**, and nothing observes it. `bound_template` runs
//!    exactly once per *newly minted* thread — it sits in the `else` branch of
//!    the reusable-thread check in
//!    `operation::planner_harness_start_adapter`'s `app_server_interact`, so a
//!    reused thread never re-resolves — hence the `error!` on the `Broken` row fires at
//!    start and never again. The tool scope *does* re-resolve on every
//!    `tools/list`, but it projects `Owned { .. } => Only(plugin.id)` **without
//!    reading `contract`**. So for the rest of that thread's life the prompt
//!    keeps a frozen descriptor plus a `template_input` that no current schema
//!    accepts, the scope keeps saying `Only(owner)`, and **no reader can
//!    discover that the contract broke**. Before the split, the run-time check
//!    was a single verdict, so the next `tools/list` drove the scope to `None`
//!    — crude, but the one signal a live system could observe.
//!
//! The narrower claim, then: "this PR does not change it" is true of (1) and
//! false of (2). (2) is a deliberate price of the split, not an oversight — the
//! alternative is exactly the unrepairable degradation the section above
//! rejects, since `TrackPatch` cannot rewrite the three columns a withdrawn
//! scope would strand. Closing either instance means re-rendering developer
//! instructions on owner/contract transitions, or pinning a resolution
//! snapshot for the life of a thread — both are prompt-lifecycle work well
//! outside a binding-resolution slice. Registered here, not fixed.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::forge_trust::trusted_forge_plugin;
use crate::model::Track;
use crate::plugin_host::manifest::TemplateDescriptor;
use crate::plugin_host::template_input::{TemplateInputOwner, validate_template_input_binding};
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
    /// `plugin_scope IS NULL` — this track has no owner *now*.
    ///
    /// Deliberately not "no owner was ever recorded": `routes::today`'s
    /// launchpad adoption clears the column (together with `template_id` and
    /// `template_input`) on a track that may well have had one, and migration
    /// 0076 backfilled the column for rows that predate it. Neither changes
    /// what this variant means to a reader.
    ///
    /// It is a *terminal* answer, not a "look harder" one. A running plugin
    /// that happens to declare the row's `template_id` does **not** adopt the
    /// track: no **runtime** code path in `crates/calm-server` writes a
    /// non-NULL `plugin_scope` onto an existing row (the sweep behind the
    /// module doc's writer list — the three runtime writers there either
    /// `INSERT` a fresh row or clear the column).
    ///
    /// Scope of that negative, exactly: it covers the running server, not the
    /// column's whole history. Migration 0076 — entry 4 on the same list — does
    /// promote existing rows into a plugin's scope; it just does so once, at
    /// migration time, before any reader in this process runs. So an `Unbound`
    /// answer stays `Unbound` for as long as this server is the only writer,
    /// which is the property the two readers actually need.
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

// 第二轮评审 NIT-4 — there used to be a `scoped_plugin_id(&self) -> Option<&str>`
// accessor here, documented as "for log fields on both readers". Its only
// caller allocated it unconditionally (`.unwrap_or("<none>").to_string()`) and
// then used it in a single `error!` arm, where the arm's own `plugin` binding
// already carries the id; `tool_visibility` never called it at all. Both the
// allocation and the "both readers" claim are gone rather than narrowed —
// every arm that logs an id has one in scope.

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
    /// same matrix the create route enforces — specifically the
    /// `TemplateInputOwner::Plugin(_)` rows of it, which are the only rows a
    /// live owner can be in.
    Honored {
        template: TemplateDescriptor,
        /// The row's `template_input`, re-checked against `plugin`'s current
        /// schema — never a blob that only an older schema accepted.
        input: Option<Value>,
    },
    /// The owner is live but the contract cannot be honored: nothing template
    /// -shaped may reach the prompt. Tool visibility is unaffected.
    ///
    /// # Same return values as [`Self::NotTemplated`], and no current test
    /// tells them apart
    ///
    /// This enum has exactly two consumers —
    /// `operation::planner_harness_start_adapter::bound_template` and
    /// `mcp_server::tool_visibility` — and **both project this variant and
    /// `NotTemplated` onto the same return value** (`Ok(None)` / `Only(owner)`).
    /// They are *not* indistinguishable in general: `bound_template`'s `Broken`
    /// arm additionally emits a `tracing::error!` on the
    /// `planner_harness::template_binding` target, so an operator subscribed to
    /// that target can tell the two apart without any change to return values.
    /// What is true is narrower: nothing in this repository asserts on that log
    /// line, so **no test here tells the two variants apart** — swapping every
    /// `Broken(..)` construction for `NotTemplated` leaves the suite green.
    ///
    /// Keep the distinction anyway — the log *is* the carrier (it is the only
    /// operator-visible signal that a contract broke, and see the module doc's
    /// KNOWN GAP (2) for how thin that signal is), and `NotTemplated` cannot
    /// carry a [`ContractFailure`] to render. But do not reason from "the type
    /// distinguishes them" to "a *test* distinguishes them": if you add a
    /// consumer that must treat them differently, it needs its own test,
    /// because no existing one will go red for you.
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
    // reject (400) resolving as fully honored. The matrix is one function and
    // this calls it, absence included.
    //
    // 第二轮评审 NIT-1 — "both callers enter this function over the whole
    // matrix" is true only of the rows a live owner can occupy. This branch is
    // already past the `template_id IS NULL` early return above, so it only
    // ever reaches the `TemplateInputOwner::Plugin(_)` half; the create route
    // additionally reaches `NoTemplateId` / `NoBoundPlugin`, and the first of
    // those disagrees with this resolver by construction — create answers 400
    // for `(no template_id, input present)` while a row in that shape resolves
    // `NotTemplated` here without consulting the matrix at all. No production
    // writer produces such a row (a NULL `template_id` is only ever written
    // together with a NULL `template_input`), so the disagreement is
    // unreachable rather than reconciled.
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
