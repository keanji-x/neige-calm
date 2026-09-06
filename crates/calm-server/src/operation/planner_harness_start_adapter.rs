use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    HarnessTranscriptMeasure, HarvestOutcome, HarvestedFrom, HarvestedMessage,
    append_decision_event_in_tx, card_create_with_id_tx, card_delete_tx, card_update_tx,
    harness_items_delete_by_card_tx, harness_items_measure_by_card_tx,
    harvest_pending_user_messages_tx, session_bind_attribution_tx,
    session_clear_queue_harvested_tx, session_delete_tx, session_fail_if_active_runtime_tx,
    session_handle_state_by_id_tx, session_prepare_deferred_planner_tx,
    session_projection_active_for_card_tx, session_restore_from_superseded_runtime_tx,
    session_set_handle_state_of_any_runtime_tx, session_set_handle_state_tx,
    session_start_runtime_tx, session_supersede_active_tx, session_supersede_and_start_tx,
};
use crate::db::{Repo, write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::harness::{
    HARNESS_MODE, HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation,
    PlannerHarness, PlannerHarnessParams, QueueEntry, initial_snapshot_with_goal,
    is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, TrackId};
use crate::mcp_server::wiring::{
    mint_card_mcp_token_pair, mirror_session_mcp_token, persist_card_mcp_token_hash,
};
use crate::model::{Card, CardPatch, CardRole, NewCard, new_id, now_ms};
// Issue #649 i2 lifted the per-card lock-map machinery that used to live in
// this module into `crate::per_card_lock` so the `/planner/input` lazy-recovery
// path can share it. Same semantics: guards self-clean their entry on drop.
use crate::activity_window::launchpad_opening_briefing;
use crate::operation::codex_adapter::card_payload_get_tx;
use crate::per_card_lock::{PerCardLockGuard, PerCardLocks, lock_card, new_per_card_locks};
use crate::plugin_host::{PluginHost, manifest::TemplateDescriptor};
use crate::routes::cards::{MAX_PLANNER_INPUT_CHARS, card_scope, card_scope_tx};
use crate::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams, ThreadConfig};
use crate::state::WriteContext;
use crate::track_area_cache::TrackAreaCache;
use crate::track_binding::{TemplateContract, TrackOwnerBinding, resolve_track_owner_binding};

use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
    checkpoint_app_server_interact_tx,
};

const START_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

const REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR: &str =
    "no per-card MCP token row; refusing to start an unauthenticated shell";

#[cfg(feature = "fixtures")]
pub const FIXTURE_SOCKET_PREFIX: &str = "neige-mcp-fixture-";

#[cfg(feature = "fixtures")]
pub fn fixture_socket_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{FIXTURE_SOCKET_PREFIX}{}.sock",
        std::process::id()
    ))
}

#[derive(Clone)]
pub struct PlannerHarnessStartAdapter {
    repo: Arc<dyn Repo>,
    daemon: Arc<SharedCodexAppServer>,
    harness_registry: HarnessRegistry,
    plugin: Arc<PluginHost>,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    mcp_socket_path: Option<PathBuf>,
    per_card_mint_locks: PerCardLocks,
}

impl PlannerHarnessStartAdapter {
    pub fn new(
        repo: Arc<dyn Repo>,
        daemon: Arc<SharedCodexAppServer>,
        harness_registry: HarnessRegistry,
        plugin: Arc<PluginHost>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
        mcp_socket_path: Option<PathBuf>,
    ) -> Self {
        Self {
            repo,
            daemon,
            harness_registry,
            plugin,
            card_role_cache,
            track_area_cache,
            mcp_socket_path,
            per_card_mint_locks: new_per_card_locks(),
        }
    }

    /// Defense-in-depth: today OperationRuntime drives serially under
    /// drive_mutex, but if drive ever shifts to per-card-lease parallelism,
    /// this lock keeps card_mcp_token rotation atomic with the thread/start
    /// RPC that ships the matching raw token.
    async fn lock_card_mint(&self, card_id: &str) -> PerCardLockGuard {
        lock_card(&self.per_card_mint_locks, card_id).await
    }

    fn mcp_socket_path_for_thread(&self) -> Result<String> {
        if let Some(path) = self.mcp_socket_path.as_ref() {
            return Ok(path.to_string_lossy().to_string());
        }

        #[cfg(feature = "fixtures")]
        {
            let path = fixture_socket_path();
            Ok(path.to_string_lossy().to_string())
        }
        #[cfg(not(feature = "fixtures"))]
        {
            Err(CalmError::Internal(
                "planner harness MCP socket path missing".into(),
            ))
        }
    }

    /// #1321 S1 — resolve the track's bound template **through the track's
    /// recorded owner** (`tracks.plugin_scope`), never by scanning the running
    /// roster for `tracks.template_id`.
    ///
    /// That scan was the bug: it made this reader and the MCP per-track tool
    /// scope answer "who owns this track" from different columns, so a plugin
    /// that took over a stopped owner's template id got its descriptor
    /// injected here — together with a `template_input` only the *previous*
    /// owner's schema ever validated — while the tool scope stayed on the
    /// stopped owner. Both readers now share
    /// [`crate::track_binding::resolve_track_owner_binding`], so there is one
    /// owner answer per track.
    pub(crate) async fn bound_template(&self, track_id: &str) -> Result<Option<BoundTemplate>> {
        let track = match self.repo.track_get(track_id).await {
            Ok(Some(track)) => track,
            Ok(None) => {
                tracing::error!(
                    target: "planner_harness::template_binding",
                    track_id,
                    "bound template track was not found while resolving descriptor; using vanilla planner prompt"
                );
                return Ok(None);
            }
            Err(error) => {
                tracing::error!(
                    target: "planner_harness::template_binding",
                    track_id,
                    error = %error,
                    "template binding lookup failed; using vanilla planner prompt"
                );
                return Ok(None);
            }
        };
        let binding = resolve_track_owner_binding(&track, Some(self.plugin.as_ref())).await;
        match binding {
            TrackOwnerBinding::Owned {
                contract: TemplateContract::Honored { template, input },
                ..
            } => Ok(Some(BoundTemplate {
                descriptor: template,
                input,
            })),
            // Owner resolved but the track names no template, or the track is
            // unbound: an ordinary vanilla prompt, not a degradation.
            TrackOwnerBinding::Owned {
                contract: TemplateContract::NotTemplated,
                ..
            }
            | TrackOwnerBinding::Unbound => Ok(None),
            // #1321 S1 第一轮评审 MAJOR-2 — the owner is alive and keeps its
            // tool scope (`Only(owner)`); it is only the *template contract*
            // that is unusable, so the descriptor and the persisted
            // `template_input` are dropped rather than injected without a
            // checked contract behind them. The prompt degrades, the track
            // does not lose its plugin.
            TrackOwnerBinding::Owned {
                plugin,
                contract: TemplateContract::Broken(failure),
            } => {
                tracing::error!(
                    target: "planner_harness::template_binding",
                    track_id,
                    template_id = track.template_id.as_deref().unwrap_or("<none>"),
                    plugin_id = %plugin.id,
                    failure = %failure,
                    "the track's owner is live but its template contract no longer holds; using vanilla planner prompt (tool scope is unaffected)"
                );
                Ok(None)
            }
            TrackOwnerBinding::OwnerUnavailable { plugin_id } => {
                tracing::error!(
                    target: "planner_harness::template_binding",
                    track_id,
                    template_id = track.template_id.as_deref().unwrap_or("<none>"),
                    plugin_id = %plugin_id,
                    "the track's recorded owner is not running ∧ trusted; using vanilla planner prompt"
                );
                Ok(None)
            }
        }
    }
}

/// #891 — a resolved template binding: the descriptor from the running
/// trusted plugin plus the track row's persisted `template_input` (already
/// schema-validated at create time).
pub(crate) struct BoundTemplate {
    pub(crate) descriptor: TemplateDescriptor,
    pub(crate) input: Option<serde_json::Value>,
}

/// The serialized names below are FROZEN, and deliberately out of step with the
/// Rust variant names — see [`PlannerHarnessStartOperationPayload`] for why.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessProfile {
    #[default]
    #[serde(rename = "spec")]
    Planner,
    PlainChat,
    /// #1189 — a track-scoped assistant conversation. Minted lazily like a
    /// plain chat, but it is an MCP-authenticated `CardRole::Assistant` card on
    /// an ordinary track: it reads and writes that track's report through the
    /// block channel and has no lifecycle / plan / review / admin authority.
    Assistant,
}

/// Whether a conversation create prepends #1343's activity briefing.
///
/// **An explicit ruling, because the answer is not a property of the track.**
/// `POST /api/today/summary` also creates its conversation on the launchpad, by
/// calling `create_track_conversation_inner`, and it carries the day's counts
/// itself in `summary_prompt` — briefing it as well would put the same five
/// numbers in front of the agent twice and leave three
/// `harness.user_message.enqueued` rows where INV-TODAYDOC-010 requires two.
/// Deriving the answer from the track would make that outcome unavoidable; a
/// ruling makes each caller say what it means.
///
/// It lives beside [`PlannerHarnessStartOperationPayload`] because that is what
/// carries it: since #1314 the create has no post-operation step left to act on
/// it, so the ruling has to reach `prepare_tx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpeningBriefing {
    /// The user is starting this conversation. On the launchpad track it opens
    /// with today's activity window; on any other track it opens with nothing.
    TodaysActivityOnTheLaunchpad,
    /// The caller supplies its own material and must not be given a second
    /// copy of it. `POST /api/today/summary` is the only such caller.
    CallerSuppliesItsOwn,
}

/// **The serialized field names in this struct are FROZEN and do not follow the
/// #1316 renames.** They are `wave_id` and `spec_card_id`, and the Rust fields
/// deliberately disagree with them.
///
/// This struct is not a DTO — it is persisted verbatim in
/// `operations.payload_json`, and a SHA-256 over its serialization is persisted
/// alongside it as `operations.payload_hash`
/// (`routes/today.rs::stable_payload_hash(json!({"actor":…,"request":&req}))`).
/// The operation runtime treats "same idempotency key, different payload hash"
/// as a permanent 409 (`operation/repo_sqlite.rs:88`), and **nothing ever
/// deletes rows from `operations`**.
///
/// So renaming a serialized field here is not a rename, it is a silent,
/// permanent outage for every install that already has rows: the Today panel's
/// `today-launchpad:<card>:<mode>:<digest>` key and the scheduler's child
/// bootstrap key are both stable across deploys, so the first `ensure` after
/// the upgrade re-submits an unchanged key with a changed hash and 409s —
/// forever, with no self-healing path. `routes/today.rs:766-781` records
/// #1147 hitting exactly this and solving it by putting the varying part in
/// the KEY; here the varying part would be the whole payload shape, so the
/// answer is to not vary it.
///
/// Migration 0082 therefore rewrites `operations.kind` (a lookup key, not
/// hashed) but deliberately leaves `operations.payload_json` alone.
///
/// `track_id` also carries a `serde(alias)` for the S2-era spelling: #1316 S2
/// renamed this field to `track_id` without freezing it, so an install that
/// ran that build has rows spelled the other way. The alias lets those rows
/// deserialize; their hash still cannot match, which is the pre-existing S2
/// defect this freeze stops from spreading.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannerHarnessStartOperationPayload {
    pub actor: ActorId,
    #[serde(rename = "wave_id", alias = "track_id")]
    pub track_id: String,
    #[serde(rename = "spec_card_id")]
    pub planner_card_id: CardId,
    #[serde(default)]
    pub report_card_id: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    pub cwd: String,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub reset_harness_items: bool,
    #[serde(default)]
    pub force_new_thread: bool,
    #[serde(default)]
    pub profile: HarnessProfile,
    /// #1098 §5.6 — "mint the card only on the first message".
    ///
    /// `Some` asks this operation to create `planner_card_id` itself, inside the
    /// same transaction that writes the session row, so a failed `thread/start`
    /// can take the card back out again (the compensation chain unconditionally
    /// prepends a `delete_card` step). `None` keeps the historical contract
    /// verbatim: the card must already exist.
    ///
    /// Like every other field here it is `#[serde(default)]` because
    /// `abandoned_running_operations_on_boot` re-drives payloads written by
    /// older binaries.
    #[serde(default)]
    pub create_card: Option<LazyMintCardSeed>,
    /// #1343 — whether this create opens with the launchpad activity briefing.
    ///
    /// **The RULING travels here, never the briefing TEXT.** `prepare_tx`
    /// renders the text itself, from inside the mint transaction. That split is
    /// forced by `stable_payload_hash`, which covers the whole payload: the
    /// day's counts change through the day, so a briefing folded in here would
    /// give one `Idempotency-Key` a different `payload_hash` on every retry and
    /// `submit` would answer a permanent 409 — permanent because `operations`
    /// has no pruner. That is #1377's own argument, made where it used to live
    /// (`routes/track_conversations.rs`, before #1314 deleted the
    /// out-of-transaction send it was written against); folding the ruling in
    /// is safe precisely because it is a fixed two-valued enum chosen per
    /// caller, so it hashes stably for as long as the caller is the same.
    ///
    /// `skip_serializing_if` keeps the key out of the payload JSON of every
    /// producer that does not set one, so their payload bytes — and therefore
    /// their `payload_hash` — are byte-identical to what older binaries wrote.
    /// `#[serde(default)]` because `abandoned_running_operations_on_boot`
    /// deserializes and re-drives operation rows written before a restart;
    /// `None` there means "no briefing", which is what every pre-#1343 payload
    /// meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_briefing: Option<OpeningBriefing>,
    /// #1299 S1 — the track's first user message, delivered **inside this
    /// operation's transaction**.
    ///
    /// Read by `prepare_tx`, which pushes an `Observation::UserMessage { text }`
    /// onto the harness snapshot's `pending_queue` and writes
    /// `harness.user_message.enqueued` in the same transaction, so the mint and
    /// the delivery commit together. That is the structural fix
    /// `routes/track_conversations.rs` documented as the way out of its two
    /// known gaps; #1299 S1 put `POST /api/tracks` on it and #1314 moved
    /// `POST /api/tracks/{id}/conversations` onto it too, retiring the gaps
    /// with the code that had them.
    ///
    /// It is `Observation::UserMessage`, never `TrackGoal`: the two are
    /// different semantic slots (`TrackGoal` renders bare and does not
    /// hard-fire; `UserMessage` renders `"User says:\n…"`, hard-fires, and is
    /// attributed to the human who typed it). `goal` stays reserved for the
    /// machine-written child-track bootstrap.
    ///
    /// The text — not a hash — has to travel here because the adapter has to
    /// enqueue the actual bytes. That does copy one user message into
    /// `operations.payload_json`; it is the price of an atomic delivery, and
    /// it is bounded by `validate_first_message`'s 32768-character ceiling.
    ///
    /// `skip_serializing_if` keeps the key out of the payload JSON of every
    /// caller that does not set one, so a message-less start hashes to the
    /// same `payload_hash` whether or not this field exists.
    /// `#[serde(default)]` because `abandoned_running_operations_on_boot`
    /// deserializes and re-drives operation rows written before a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message: Option<String>,
    /// #1384 / #1434 — SHA-256 (lower-case hex) of the track create request's
    /// mint inputs.
    ///
    /// Never read by this adapter — unlike [`Self::first_message`], which
    /// `prepare_tx` does read. It keeps the operation's local collision
    /// check aligned with the durable binding-level fingerprint. The binding
    /// is authoritative because it survives the pre-operation failure window;
    /// this copy preserves operation replay compatibility.
    ///
    /// The digest covers title, sort, the request's original cwd, template and
    /// recipe ids, template input, attach-folder intent, theme, and fork source.
    /// It hashes the request, not mutable track state, so a later workspace
    /// repoint does not change replay identity. `area_id` is scoped by the
    /// binding primary key instead.
    ///
    /// `skip_serializing_if` keeps every caller that does not set it — every
    /// message-less create, and the four non-create producers — writing
    /// byte-identical payload JSON, so adding this field cannot move an
    /// existing `payload_hash` and turn an in-flight retry across a deploy into
    /// a spurious 409. `#[serde(default)]` because
    /// `abandoned_running_operations_on_boot` re-drives payloads written by
    /// older binaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_request_sha256: Option<String>,
}

/// The user-supplied part of a lazily minted conversation card — an area chat
/// (`HarnessProfile::PlainChat`) or a track assistant (`HarnessProfile::Assistant`).
/// Everything the kernel owns (kind, role, `deletable`, the persisted
/// `harness_profile` marker) is pinned by the adapter and deliberately absent
/// here.
///
/// Named for the mint, not for one profile: #1189 gave the branch a second
/// caller, and a `PlainChat`-shaped name on the assistant path would have been
/// the kind of stale label that makes the next reader assume the branch is
/// single-purpose.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LazyMintCardSeed {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    /// The caller's `Idempotency-Key`, so [`PlannerHarnessStartAdapter::validate`]
    /// can **recompute** the deterministic card id and refuse anything it did
    /// not derive itself (§4.3). Required on the assistant profile; see the
    /// guard for why the area profile does not read it.
    ///
    /// It lives on the seed rather than on the payload root so the area route,
    /// which does not set it, keeps writing byte-identical
    /// `operations.payload_json` — a changed payload hash would turn an
    /// in-flight retry across a deploy into a spurious 409.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Which session row a profile writes.
///
/// `Assistant` is `CodexCard`, NOT `SharedPlanner`, and the difference is a
/// safety property rather than bookkeeping: `derive_session_identity`
/// (`calm-truth/src/db/sqlite/session_row.rs`) maps `SharedPlanner` to
/// `WorkerContract::Planner`, and `session_repoint_current_links_tx`
/// (`session_mirror.rs`) makes every live Planner session the track's
/// `root_session_id`. An assistant session under `SharedPlanner` would therefore
/// displace the planner card's session as the track's planning authority the
/// moment it started.
fn session_kind_for(profile: HarnessProfile) -> WorkerSessionKind {
    match profile {
        HarnessProfile::Planner => WorkerSessionKind::SharedPlanner,
        HarnessProfile::PlainChat | HarnessProfile::Assistant => WorkerSessionKind::CodexCard,
    }
}

/// The `(role, harness_profile marker)` pair the lazy-mint branch pins onto a
/// freshly created conversation card.
///
/// Kept as one function returning both halves because they must never disagree:
/// the role decides what the card's MCP token may call, the marker decides
/// which list the card appears in, and a card with one of each flavour is
/// invisible in the UI while holding the other flavour's authority.
///
/// `Planner` is rejected here rather than silently defaulted — `validate` already
/// refuses that profile on the mint branch, and this is the fail-closed twin so
/// a future caller that skips `validate` cannot mint a planner card either.
fn minted_card_shape(profile: HarnessProfile) -> Result<(CardRole, &'static str)> {
    match profile {
        HarnessProfile::PlainChat => Ok((CardRole::Worker, "plain_chat")),
        HarnessProfile::Assistant => Ok((CardRole::Assistant, ASSISTANT_HARNESS_PROFILE_MARKER)),
        HarnessProfile::Planner => Err(CalmError::BadRequest(
            "the planner profile does not mint its own card".into(),
        )),
    }
}

/// The persisted `payload.harness_profile` value of a track assistant card.
/// Read by `plain_chat::card_is_track_assistant` and by the track conversation
/// list predicate; those three places are the whole contract.
pub(crate) const ASSISTANT_HARNESS_PROFILE_MARKER: &str = "assistant";

pub(crate) fn render_planner_developer_instructions(
    track_id: &str,
    template_descriptor: Option<&TemplateDescriptor>,
    template_input: Option<&serde_json::Value>,
) -> String {
    let mut instructions = crate::planner_card::render_system_prompt(
        crate::planner_card::SeededCardRole::Planner.prompt_template(),
        track_id,
    );
    // #1110 S5 — the descriptor is an id handle only. Plan prose lives in
    // the forked report; the remaining injected contract is the track's
    // validated `template_input`, gated on a resolved binding.
    if template_descriptor.is_none() {
        return instructions;
    }
    // #891 — the track's validated template_input, verbatim. Deliberately
    // NOT passed through `render_system_prompt`: user-controlled JSON must
    // not have literal `{track_id}` substituted.
    if let Some(input) = template_input {
        instructions.push_str("\n\n## Bound Template Input\n");
        instructions.push_str("```json\n");
        instructions
            .push_str(&serde_json::to_string_pretty(input).expect("template_input serializes"));
        instructions.push_str("\n```");
    }
    instructions
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_planner_developer_instructions_for_test(
    track_id: &str,
    template_descriptor: Option<&TemplateDescriptor>,
    template_input: Option<&serde_json::Value>,
) -> String {
    render_planner_developer_instructions(track_id, template_descriptor, template_input)
}

#[async_trait]
impl ProviderAdapter for PlannerHarnessStartAdapter {
    fn kind(&self) -> &'static str {
        "planner-harness-start"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        START_PHASES
    }

    fn app_server_interact_kind(
        &self,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: PlannerHarnessStartOperationPayload = serde_json::from_value(input.clone())?;
        let track = self
            .repo
            .track_get(&payload.track_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("track {}", payload.track_id)))?;
        // #1098 §5.6 — the lazy-mint branch. `validate` runs BEFORE the
        // operation row is inserted, so the usual "card must exist + its role
        // cache entry must match" assertions are not merely wrong here, they
        // are unsatisfiable: the card is this operation's own output. The
        // checks below are the fail-closed replacement, and they must stay
        // strictly narrower than the ordinary branch — a caller must not be
        // able to conjure a chat card onto an arbitrary track.
        if let Some(seed) = payload.create_card.as_ref() {
            // Guard ① — which profiles may mint at all. `Planner` never can: a
            // planner card is minted with its track, and letting this branch write
            // one would hand a caller the track's lifecycle authority.
            //
            // Guard ② — where. The two profiles answer it differently, and the
            // assistant answer is the stronger one (#1189 §4.3): instead of
            // asking what KIND of track this is, the adapter recomputes the
            // deterministic card id from `(track_id, idempotency_key)` and
            // refuses any id it did not derive itself. A forged id, an id
            // derived for somebody else's track, and an id conjured out of
            // nothing all fail the same comparison, so the assistant branch
            // needs no track-shape allowlist to stay closed.
            match payload.profile {
                HarnessProfile::Planner => {
                    return Err(CalmError::BadRequest(format!(
                        "card {} can only be minted by this operation under the plain-chat or assistant profile",
                        payload.planner_card_id
                    )));
                }
                HarnessProfile::PlainChat => {
                    // Deliberately NOT migrated to the derived-id guard. The
                    // area flavour's id is derived from `(area_id, key)`, and
                    // recomputing it here would mean binding the key into the
                    // area route's operation payload — changing a live payload
                    // hash for no gain, since this check is already exact for
                    // that flavour: an area chat card belongs on an area chat
                    // track and nowhere else.
                    if track.purpose.as_deref() != Some(crate::AREA_CHAT_PURPOSE) {
                        return Err(CalmError::Forbidden(format!(
                            "track {} is not an area chat track; chat cards are only minted there",
                            track.id
                        )));
                    }
                }
                HarnessProfile::Assistant => {
                    // Fail closed on a missing key: without it there is nothing
                    // to recompute from, and "no key" must not read as "no
                    // check". Only `POST /api/tracks/{id}/conversations` sets
                    // it, and that route requires the header.
                    let Some(idempotency_key) = seed.idempotency_key.as_deref() else {
                        return Err(CalmError::BadRequest(format!(
                            "card {} cannot be minted without the idempotency key its id is derived from",
                            payload.planner_card_id
                        )));
                    };
                    let expected = crate::conversation_keys::derive_track_conversation_keys(
                        track.id.as_str(),
                        idempotency_key,
                    )
                    .card_id;
                    if payload.planner_card_id.as_str() != expected {
                        return Err(CalmError::Forbidden(format!(
                            "card {} is not the conversation id derived for track {} under this idempotency key",
                            payload.planner_card_id, track.id
                        )));
                    }
                }
            }
            // An existing row means this is not a first mint. A genuine retry
            // is deduplicated far earlier, by the operation idempotency key in
            // `submit`; reaching here with the card already present would mean
            // adopting somebody else's card, so it is a conflict.
            if self
                .repo
                .card_get(payload.planner_card_id.as_str())
                .await?
                .is_some()
            {
                return Err(CalmError::Conflict(format!(
                    "card {} already exists; refusing to re-mint a chat conversation card",
                    payload.planner_card_id
                )));
            }
            if !self.daemon.is_running() {
                return Err(self.daemon.not_running_error());
            }
            return Ok(());
        }
        let Some(card) = self.repo.card_get(payload.planner_card_id.as_str()).await? else {
            return Err(CalmError::NotFound(format!(
                "card {}",
                payload.planner_card_id
            )));
        };
        if card.track_id.as_str() != payload.track_id {
            return Err(CalmError::BadRequest(format!(
                "planner card {} belongs to track {}, not {}",
                card.id, card.track_id, payload.track_id
            )));
        }
        let expected_role = match payload.profile {
            HarnessProfile::Planner => CardRole::Planner,
            HarnessProfile::PlainChat
                if crate::plain_chat::card_is_plain_chat(
                    &card,
                    self.card_role_cache.get(&card.id),
                    true,
                ) =>
            {
                CardRole::Worker
            }
            HarnessProfile::PlainChat => {
                return Err(CalmError::BadRequest(format!(
                    "card {} is not marked for plain chat",
                    card.id
                )));
            }
            // Re-start paths (`/planner/reset`, `/planner/input` lazy recovery) reach
            // here with an existing assistant card; the marker + role pair is
            // what makes the profile legitimate, exactly as for plain chat.
            HarnessProfile::Assistant
                if crate::plain_chat::card_is_track_assistant(
                    &card,
                    self.card_role_cache.get(&card.id),
                    true,
                ) =>
            {
                CardRole::Assistant
            }
            HarnessProfile::Assistant => {
                return Err(CalmError::BadRequest(format!(
                    "card {} is not marked as a track assistant",
                    card.id
                )));
            }
        };
        if self.card_role_cache.get(&card.id) != Some(expected_role) {
            let message = match payload.profile {
                HarnessProfile::Planner => format!("card {} is not a planner card", card.id),
                HarnessProfile::PlainChat => {
                    format!("card {} is not marked for plain chat", card.id)
                }
                HarnessProfile::Assistant => {
                    format!("card {} is not marked as a track assistant", card.id)
                }
            };
            return Err(CalmError::BadRequest(message));
        }
        if expected_role == CardRole::Planner
            && track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE)
        {
            return Err(CalmError::Forbidden(format!(
                "planner harness is disabled for area chat track {}",
                track.id
            )));
        }
        if !self.daemon.is_running() {
            // #953 — same variant/status; message carries the live failure
            // and the background-retry fact. Preflights stay non-blocking.
            return Err(self.daemon.not_running_error());
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: PlannerHarnessStartOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = payload.planner_card_id;
        let track_id = payload.track_id;
        let report_card_id = payload.report_card_id;
        let defer_runtime_start = payload.force_new_thread;
        let session_kind = session_kind_for(payload.profile);
        // #1343 — the launchpad opening briefing, rendered HERE, inside the
        // mint transaction, because since #1314 there is no post-operation step
        // left to render it in.
        //
        // **The placement is load-bearing: this must stay ABOVE the first write
        // of this transaction, next to the pool-reading `card_scope` below.**
        // Both statements it issues (`track_get_launchpad`, and the activity
        // window's `events ⋈ tracks ⋈ areas`) are single autocommit reads off
        // the pool — a different connection from `tx`. Run before this
        // transaction has written anything, that connection can always be
        // granted its shared lock and can never be the waiter in a cycle.
        //
        // The hazardous table is `events`, not `tracks`: this transaction's
        // write set is `cards` + `events`, and `tracks` is not in it. Reasoning
        // from a `tracks`-centric story looks at the wrong table and concludes
        // the read is harmless wherever it sits. It is not. Moved to AFTER the
        // first write, the same read goes red on the first contended round —
        // `tx` holds RESERVED, the pool read wants a shared lock on a page the
        // writer has dirtied, and neither side yields.
        //
        // `briefing_ordering_survives_contention_in_the_mint_transaction` is
        // the wall-clocked contention test that pins this; it is bounded so a
        // deadlock fails the test rather than hanging the harness.
        let opening_briefing = match payload.opening_briefing {
            Some(OpeningBriefing::TodaysActivityOnTheLaunchpad) => {
                launchpad_opening_briefing(self.repo.as_ref(), track_id.as_str()).await?
            }
            Some(OpeningBriefing::CallerSuppliesItsOwn) | None => None,
        };
        if let Some(briefing) = opening_briefing.as_deref() {
            // Server-owned context is bounded independently from the user's
            // input, and neither consumes the other's allowance — the same
            // budget, and the same reason for it, that `/planner/input` gave
            // this material while it still travelled the observation channel.
            //
            // `Internal`, not `BadRequest`: nothing the caller sent can make
            // this fire. The text is the server's own projection of the day,
            // so an over-long one is the server's bug, not a bad request.
            if briefing.chars().count() > MAX_PLANNER_INPUT_CHARS {
                return Err(CalmError::Internal(format!(
                    "opening briefing must be at most {MAX_PLANNER_INPUT_CHARS} characters",
                )));
            }
        }
        // #1098 §5.6 — mint the chat card in this very transaction, so the
        // card and its session row commit together and compensation can undo
        // both. The SELECT below then reads the row we just wrote and every
        // downstream step is unchanged.
        let mut post_commit_events = Vec::new();
        if let Some(seed) = payload.create_card.as_ref() {
            let (minted_role, minted_marker) = minted_card_shape(payload.profile)?;
            let scope = card_scope(
                self.repo.as_ref(),
                card_id.clone(),
                TrackId::from(track_id.clone()),
            )
            .await?;
            let created = card_create_with_id_tx(
                tx,
                card_id.to_string(),
                NewCard {
                    track_id: TrackId::from(track_id.clone()),
                    kind: "codex".into(),
                    sort: seed.sort,
                    // Pinned by the kernel, not by the caller. Note the absent
                    // `planner_harness` key (INV-CHAT-016) and the unchanged
                    // `schemaVersion`: a conversation card is a v1 codex card
                    // that carries one extra marker, not a new payload dialect.
                    //
                    // The marker and the role both track the profile (#1189
                    // §4.2). Hard-coding either one would mint an assistant
                    // conversation that is a plain-chat worker card in the
                    // database, and the three readers that decide what an
                    // assistant may do — the authorization gate, this
                    // endpoint's list predicate, and the CARDS panel filter —
                    // all read exactly these two columns.
                    payload: json!({"schemaVersion": 1, "harness_profile": minted_marker}),
                    title: seed.title.clone(),
                },
                minted_role,
                // Deleting a single conversation is its own issue; until it
                // exists the card is kernel-owned so `DELETE /api/cards/:id`
                // cannot orphan a live harness.
                false,
                &self.card_role_cache,
            )
            .await?;
            let event = Event::CardAdded(created);
            let event_id =
                append_decision_event_in_tx(tx, &payload.actor, &scope, None, &event).await?;
            post_commit_events.push(BroadcastEnvelope {
                id: event_id,
                event_version: SYNC_EVENT_VERSION,
                actor: payload.actor.clone(),
                scope,
                event,
            });
        }
        let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at
                 FROM cards
                WHERE id = ?1
                  AND track_id = ?2"#,
        )
        .bind(card_id.as_str())
        .bind(track_id.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .map(Card::from)
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

        let existing_active_runtime = if defer_runtime_start {
            session_projection_active_for_card_tx(tx, card.id.as_str()).await?
        } else {
            None
        };
        let inherited_snapshot = existing_active_runtime.as_ref().and_then(|runtime| {
            let state = runtime.handle_state_json.as_ref()?;
            if state.get("mode").and_then(Value::as_str) != Some(HARNESS_MODE) {
                return None;
            }
            if !is_harness_snapshot_value(state) {
                tracing::warn!(
                    card_id = %card_id,
                    "reset: dormant runtime snapshot has corrupt/unknown shape; \
                     discarding inherited queue and starting a fresh session"
                );
                return None;
            }
            Some(HarnessSnapshot::from_value_strict(state.clone()))
        });
        let mut snapshot = initial_snapshot_with_goal(payload.goal.clone());
        let mut inherited_queue_moved = false;
        // #1449 — what the INHERIT took, for the same undo journal the harvest
        // fills. The harvest's journal cannot cover it: the predecessor is
        // still `active` when the harvest runs (it is retired later, inside
        // `session_prepare_deferred_planner_tx`), so it is invisible to a
        // predicate keyed on `superseded`. Without this entry a deferred mint
        // that fails after committing empties the predecessor's queue, marks
        // the successor `failed` — a state the harvest never reads — and
        // `restore_old_runtime` brings the predecessor back with nothing on it.
        let mut inherited_from: Vec<HarvestedFrom> = Vec::new();
        if let Some(inherited) = inherited_snapshot {
            snapshot.push_watermark = inherited.push_watermark;
            // #1505 PR1 — inheriting the fused entries (rather than the raw
            // arrays) is what makes a reset KEEP the queue ids the client has
            // already been shown. Before this the ids did not exist and the
            // reset silently re-created every entry as a fresh anonymous one.
            //
            // Pinned end to end by
            // `planner_card_reset::reset_planner_card_preserves_runtime_pending_queue_and_push_watermark`,
            // which reddens if this line goes back to copying observations
            // alone.
            //
            // An earlier note here claimed no such test could exist, on the
            // ground that "a user message hard-fires, so the harness this reset
            // starts drains it inside the same request". That reason was false
            // about THIS arm, and the correction is the reason the test works:
            // this branch runs only under `defer_runtime_start`
            // (`payload.force_new_thread`), which takes
            // `session_prepare_deferred_planner_tx` and starts NO harness in
            // this request — it writes a placeholder row. The inherited entries
            // sit on the successor's persisted snapshot until something later
            // spawns the harness, and that window is a state, not a race.
            //
            // Also pinned, one layer down: that `pending_entries` /
            // `set_pending_entries` preserve ids across this round trip
            // (`snapshot::tests::set_pending_entries_writes_every_parallel_array_in_step`,
            // `planner_pending_queue::a_queue_entry_id_survives_a_snapshot_round_trip`).
            //
            // #1449 — the inherit CARRIES the predecessor's message ids too; it
            // does not mint over them. These are the same instances, moved.
            let mut inherited_entries = inherited.pending_entries();
            inherited_queue_moved = true;
            if let Some(existing) = existing_active_runtime.as_ref() {
                // Only the human sentences are journalled, because only they
                // are returned: the rest of an inherited queue is machine
                // context the successor re-derives.
                // #1449 — the same mint-at-the-boundary rule as the harvest:
                // an inherited entry with no ids gets one, written into the
                // successor's snapshot AND into the journal, so a failed mint
                // can give it back. `is_user_authored` is the filter rather
                // than the observation shape, so it is the SAME predicate
                // `ensure_message_id` mints under — a filter that admitted an
                // entry the mint declines would journal an empty id set.
                let mut messages: Vec<HarvestedMessage> = Vec::new();
                for entry in inherited_entries.iter_mut() {
                    if !entry.is_user_authored() {
                        continue;
                    }
                    let ids = entry.ensure_message_id().to_vec();
                    let Observation::UserMessage { text } = entry.observation() else {
                        continue;
                    };
                    messages.push(HarvestedMessage { text, ids });
                }
                if !messages.is_empty() {
                    tracing::info!(
                        card_id = %card_id,
                        from = %existing.id,
                        moved = messages.len(),
                        "planner harness: inherited undelivered user messages from the \
                         predecessor"
                    );
                    inherited_from.push(HarvestedFrom {
                        worker_session_id: existing.id.clone(),
                        messages,
                    });
                }
            }
            snapshot.set_pending_entries(inherited_entries);
        }
        // One clock for the whole mint: the supersede below, the harvest stamp
        // (passed IN, so the helper does not read a second clock of its own)
        // and the new row's `now_ms` are all this instant, so the predecessor's
        // `queue_harvested_at_ms` can never be newer than the successor that
        // took its queue.
        let now = now_ms();
        // #1449 — the NON-deferred arm supersedes its predecessor without
        // inheriting anything from it, so the supersede is hoisted here, ahead
        // of the harvest, and the row it retires becomes harvestable inside
        // this same transaction. Left where it was — at the bottom, after the
        // snapshot has already been serialized — the predecessor would still be
        // `active` when the harvest runs and its queue would be dropped exactly
        // as it is today. `session_supersede_active_tx` here plus
        // `session_start_runtime_tx` below are the same two statements, in the
        // same order, that `session_supersede_and_start_tx` used to issue as
        // one call.
        //
        // The deferred arm is deliberately NOT hoisted: it *inherits* the
        // predecessor's whole queue a few lines above, and its supersede lives
        // inside `session_prepare_deferred_planner_tx`.
        let superseded_predecessor = if defer_runtime_start {
            None
        } else {
            match session_projection_active_for_card_tx(tx, card.id.as_str()).await? {
                Some(existing) => {
                    session_supersede_active_tx(tx, &existing.id, now).await?;
                    Some(existing)
                }
                None => None,
            }
        };
        // #1449 — sentences a human typed and this system accepted, stranded on
        // a runtime that left the active set before its queue drained. The
        // re-point fence (`PATCH /api/tracks/{id}`) is the reachable case: it
        // supersedes every live runtime of the track and then restarts through
        // this very adapter, and until now the successor started with an empty
        // queue and the sentence was unreachable forever.
        //
        // Read, carried and stamped in THIS transaction, together with the
        // successor's own insert, so the mint and the transfer commit or roll
        // back together and a second restart reads the stamp and takes nothing.
        let worker_session_id = new_id();
        let harvested = harvest_pending_user_messages_tx(
            tx,
            card.id.as_str(),
            worker_session_id.as_str(),
            now,
            stranded_user_messages,
        )
        .await?;
        // #1299 S1 — the track's first user message ships INSIDE this
        // transaction, after the inherited queue (if any) is folded in, so it
        // is the newest entry rather than being overwritten by the inherit
        // above.
        //
        // `UserMessage`, not `TrackGoal`, and the difference is not cosmetic:
        // `TrackGoal` renders as bare text and does not hard-fire, while
        // `UserMessage` renders `"User says:\n{text}"`, hard-fires (so the turn
        // issues without waiting out the debounce) and cannot be evicted under
        // backpressure. `queue::tests::a_user_entry_never_folds_into_a_system_tail`
        // is the other end of the same rule.
        //
        // Empty / blank text is refused rather than silently dropped: the only
        // producer validates with `validate_first_message` before anything is
        // minted, so a blank string reaching here means the payload was written
        // by something that skipped that gate.
        //
        // #1343's briefing goes in FIRST, and the order is the whole point: an
        // agent whose first turn holds the user's question and no context
        // answers it from the workspace, which is the state the injection
        // exists to end. The ordering is only as strong as the enqueue order —
        // the harness may fold both into one turn — but within that turn the
        // briefing precedes the question.
        //
        // `SystemContext`, not a second `UserMessage`: it renders bare,
        // presents as System rather than as something the user said, hard-fires
        // like `UserMessage`, and — the property INV-TODAYDOC-010 rests on —
        // writes NO audit row. So `harness.user_message.enqueued` counts are
        // exactly what they were before the briefing existed.
        //
        // **KNOWN, and not fixed here: both observations inherit #1449.** They
        // sit on the `pending_queue` of a runtime that has not started yet, so
        // a mint superseded before its queue drains strands both. It is worse
        // for the briefing than for the user's sentence: #1314's healing
        // predicate keys on `harness.user_message.enqueued` rows, and
        // `SystemContext` writes none, so a stranded mint loses the briefing
        // with no re-send even in the cases where the user's sentence is
        // correctly re-delivered.
        let mut seeded = false;
        let mut entries = snapshot.pending_entries();
        if let Some(briefing) = opening_briefing {
            entries.push(QueueEntry::system(
                Observation::SystemContext { text: briefing },
                None,
            )?);
            seeded = true;
        }
        // #1449 — harvested sentences go after the successor's own briefing and
        // goal (which are what the #1343 ordering rule above is about) and
        // before this mint's own `first_message`, oldest first: they were said
        // before the one that is arriving now.
        if !harvested.messages.is_empty() {
            // #1449 — a sentence moving between runtimes leaves a trace. The
            // whole feature is "a human's sentence does not vanish"; a transfer
            // that happens silently cannot be told from a loss afterwards.
            tracing::info!(
                card_id = %card_id,
                worker_session_id = %worker_session_id,
                moved = harvested.messages.len(),
                from_rows = harvested.stamped_worker_session_ids.len(),
                "planner harness: harvested undelivered user messages into a new runtime"
            );
        }
        for message in harvested.messages {
            entries.push(QueueEntry::user_message_moved(message.text, message.ids));
            seeded = true;
        }
        if let Some(text) = payload.first_message.as_deref() {
            if text.trim().is_empty() {
                return Err(CalmError::BadRequest(
                    "first_message must not be empty".into(),
                ));
            }
            // #1505 PR1 — the track's first message is minted through the same
            // constructor as every other user message, so it gets a stable
            // queue id like the rest. Before this it was the one user entry
            // that could never be addressed (design D10).
            //
            // #1449 — that constructor is also where its message id is minted.
            //
            // Pinned by
            // `track_create_first_message::the_tracks_first_message_is_addressable_in_the_pending_page`,
            // which reads `GET /planner/run` with the drain held and fails if
            // this entry is not on the addressable page.
            //
            // That test exists because an earlier note here stood IN PLACE of
            // one, arguing that the type made it unnecessary: "the only
            // `QueueEntry` this module can build that renders as a
            // `UserMessage` is `User` … the id-less variant has no constructor
            // reachable from here". Both halves were wrong, and the second is
            // the general lesson: `QueueEntry` is a `pub` enum this module
            // already imports, and a variant is exactly as visible as its enum,
            // so `QueueEntry::LegacyUser { .. }` can be written on this line
            // today. `QueueEntry::legacy_user` being `pub(in crate::harness)`
            // constrains the named constructor and nothing else — **"no
            // constructor reachable from here" is not a property a `pub`
            // variant has.** The privacy of `harness::snapshot`'s arrays is
            // real and still worth having; it does not decide which VARIANT
            // this line pushes, which is the only thing addressability turns
            // on.
            //
            // What made it look untestable was the drain, and that is a race
            // rather than an impossibility: a user message hard-fires, so the
            // harness this request starts drains it almost at once. #1449's
            // drain hook parks the runtime immediately before the drain, which
            // turns the window into a held state the endpoint can be read in.
            entries.push(QueueEntry::user_message(text.to_string(), None));
            seeded = true;
        }
        // One write for whatever the pushes above added — briefing, harvested
        // sentences, this mint's own `first_message` — and none at all when
        // they added nothing: `set_pending_entries` rebuilds all four stored
        // arrays together, so writing after zero pushes would only rewrite a
        // snapshot this branch never changed.
        if seeded {
            snapshot.set_pending_entries(entries);
        }

        let mut old_worker_session_id = None;
        let mut old_runtime_status = None;
        let runtime_init = WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: card.id.to_string(),
            kind: session_kind,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot)?),
            spawn_op_id: None,
            now_ms: now,
        };
        if defer_runtime_start {
            if let Some(existing) = existing_active_runtime.as_ref() {
                old_worker_session_id = Some(existing.id.clone());
                old_runtime_status = Some(existing.status);
            }
            // #1449 — the inherit above took the predecessor's WHOLE queue, so
            // that row is stamped for the same reason a harvested one is:
            // without it the NEXT restart would find an unstamped superseded
            // row still holding the sentences its successor already carries and
            // would deliver them a second time. The stamp lives INSIDE
            // `session_prepare_deferred_planner_tx`, next to the supersede it
            // pairs with, so the row that is retired and the row that is
            // stamped are the same row by construction — stamping here would
            // stamp whatever a second, differently-shaped active-runtime query
            // answered (`ws.card_id` here against `cards.session_id` there).
            // #1449 S2 — the inherit is a MOVE too. It takes the predecessor's
            // WHOLE queue, so the predecessor stops holding it, in this
            // transaction. A copy left behind is what a later harvest, or an
            // operation re-driven with an older snapshot, delivers a second
            // time. Written before `session_prepare_deferred_planner_tx`
            // retires the row, while the ordinary writer still accepts it.
            if inherited_queue_moved
                && let Some(existing) = existing_active_runtime.as_ref()
                && let Some(state) = existing.handle_state_json.as_ref()
                && is_harness_snapshot_value(state)
            {
                let mut emptied = HarnessSnapshot::from_value_strict(state.clone());
                emptied.set_pending_entries(Vec::new());
                session_set_handle_state_tx(
                    tx,
                    &existing.id,
                    Some(serde_json::to_value(&emptied)?),
                )
                .await?;
            }
            session_prepare_deferred_planner_tx(tx, &runtime_init).await?;
        } else {
            if let Some(existing) = superseded_predecessor.as_ref() {
                old_worker_session_id = Some(existing.id.clone());
                old_runtime_status = Some(existing.status);
            }
            session_start_runtime_tx(tx, runtime_init).await?;
        }

        // #1299 S1 — the audit row for the seeded first message, written in the
        // SAME transaction as the queue that carries it. `send_planner_input`
        // writes this row *after* a non-transactional `harness.observe`, which
        // was the "non-transactional evidence" half of the two gaps
        // `track_conversations.rs` documented until #1314 moved that route onto
        // this seam; here the observation and its evidence commit or roll back
        // together, so neither a re-send nor a silently unrecorded send is
        // reachable on this path.
        //
        // The actor is `payload.actor` verbatim — the human who submitted the
        // create. `send_planner_input`'s `planner_input_audit_actor` rebind
        // exists only to give an *AI* actor with an empty placeholder card id a
        // card to point at; a `User` actor falls through it unchanged, and this
        // path's only producer is a human REST create. Rebinding here would
        // therefore change nothing for the reachable actor and would silently
        // launder a non-human one, so the actor is passed through and the role
        // gate below is what refuses anything it should not accept.
        if let Some(text) = payload.first_message.as_deref() {
            let char_count = text.chars().count() as u32;
            // `card_scope_tx`, NOT the pool-reading `card_scope` the mint
            // branch above uses. The rule is stated at `card_scope_tx`'s own
            // definition: a transaction must not read off the pool any table it
            // has itself written. By this point the session writes above have
            // touched `tracks` (a live `SharedPlanner` session is repointed into
            // the track's `root_session_id`), so the pool read blocks on a lock
            // only this transaction can release — the task waits on itself
            // forever. The mint branch is safe only because it runs before any
            // write.
            let scope = card_scope_tx(tx, card.id.clone(), card.track_id.clone()).await?;
            let event = Event::HarnessUserMessageEnqueued {
                worker_session_id: worker_session_id.clone(),
                card_id: card.id.clone(),
                track_id: card.track_id.clone(),
                char_count,
            };
            if let Err(violation) = crate::role_gate::enforce_role(
                &payload.actor,
                &event,
                &scope,
                &self.card_role_cache,
                &self.track_area_cache,
            ) {
                return Err(CalmError::Forbidden(violation.to_string()));
            }
            let event_id =
                append_decision_event_in_tx(tx, &payload.actor, &scope, None, &event).await?;
            post_commit_events.push(BroadcastEnvelope {
                id: event_id,
                event_version: SYNC_EVENT_VERSION,
                actor: payload.actor.clone(),
                scope,
                event,
            });
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.post_commit_events = post_commit_events;
        output.data = json!({
            "card_id": card.id,
            "track_id": track_id,
            "runtime_id": worker_session_id,
            "runtime_deferred": defer_runtime_start,
            "cwd": payload.cwd,
            "goal": payload.goal,
            "report_card_id": report_card_id,
            "snapshot": snapshot,
            // #1449 — every row this mint stamped, so compensation can give the
            // queues back. Without it a `thread/start` that fails AFTER this
            // transaction commits leaves the sentences on a runtime the
            // compensation marks `failed` — a state the harvest predicate
            // deliberately never reads — taken from rows that are stamped. That
            // is silent, permanent loss, and the mint transaction's own
            // rollback does not cover it: compensation is a different
            // transaction.
            // #1449 S3 — the undo journal: which sentences came off which row,
            // from BOTH transfers this transaction can make. Durable in
            // `operations.tx_output_json`, read only when this operation
            // compensates. Not a second live home for the message — the live
            // home is the successor's row — but the record a failed mint needs
            // to put each sentence back where it came from.
            "harvested_from": harvested_from_journal(
                &harvested
                    .taken_from
                    .iter()
                    .chain(inherited_from.iter())
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
        });
        if let Some(old_worker_session_id) = old_worker_session_id {
            output.set_output_data(
                "old_runtime_id",
                json!(old_worker_session_id),
                "planner harness",
            )?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            output.set_output_data(
                "old_runtime_status",
                serde_json::to_value(old_runtime_status)?,
                "planner harness",
            )?;
        }
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let payload: PlannerHarnessStartOperationPayload =
            serde_json::from_value(op.payload.clone())?;
        let reset_harness_items = payload.reset_harness_items;
        let force_new_thread = payload.force_new_thread;
        let profile = payload.profile;
        let session_kind = session_kind_for(profile);
        // The role argument of `thread_start_for_card`. Stated plainly because
        // it is easy to mistake for a security boundary and it is not one:
        // `SharedCodexAppServer::thread_start_for_card` takes it as `_role`
        // (`shared_codex_appserver.rs`) and production reads it nowhere — the
        // only consumer is the `fixtures`-gated `started_thread_params_for_test`
        // capture. The thread's actual tool surface is resolved per MCP request
        // by `resolve_thread_identity` (`mcp_server/transport.rs`), which walks
        // thread → session → `card_identity_get_by_session` and takes the role
        // from the card's PERSISTED `role` column — the one `minted_card_shape`
        // pins.
        //
        // It is also unreachable on the conversation profiles as they are
        // driven today: both mint with `force_new_thread: true`, which takes the
        // `thread_start_mint_for_card` path below — that call has no role
        // parameter at all. Kept correct anyway so the fixtures capture cannot
        // be read as evidence of a role the card does not carry.
        let card_role = match profile {
            HarnessProfile::Planner => CardRole::Planner,
            HarnessProfile::PlainChat => CardRole::Worker,
            HarnessProfile::Assistant => CardRole::Assistant,
        };
        let card_id = output.output_string("card_id", "planner harness")?;
        let track_id = output.output_string("track_id", "planner harness")?;
        let worker_session_id = output.output_string("runtime_id", "planner harness")?;
        let runtime_deferred = output_bool(output, "runtime_deferred")?;
        let cwd = output.output_string("cwd", "planner harness")?;
        // OLD PTY shutdown at Phase-2 entry, immediately after the Phase-1
        // tx commit. Per RATIFY-8 section 5 / 1.4, force_new_thread is a
        // hard reset: the DB-side supersede lives in calm-truth, and the
        // handle kill stays here as the first app-server-side action after
        // commit.
        if let Some(old_worker_session_id) =
            output.output_optional_string("old_runtime_id", "planner harness")?
            && old_worker_session_id != worker_session_id
            && let Some(old_handle) = self.harness_registry.remove(&old_worker_session_id)
        {
            old_handle.shutdown().await?;
        }
        if let Some(existing) = output_existing_thread_id(output)? {
            return Ok(AppServerInteractOutcome::MintedAndAwaited {
                thread_id: existing,
            });
        }
        let mint_lock_guard = self.lock_card_mint(&card_id).await;
        // Reuse requires the existing thread to have been minted under
        // PR #567's per-card token contract: the card owns a
        // `card_mcp_tokens` row. Migration 0035 forces a fresh mint for
        // any earlier thread.
        let reusable_thread_id = if force_new_thread {
            None
        } else if let Some(runtime) = self
            .repo
            .session_projection_active_for_card(&card_id)
            .await?
            && let Some(thread_id) = TxOutput::non_empty_string(runtime.thread_id.as_deref())
        {
            Some(thread_id)
        } else {
            None
        };
        let mut new_mcp_token_hash = None;
        let thread_id = if let Some(thread_id) = reusable_thread_id {
            if !self.repo.card_mcp_token_exists_for_card(&card_id).await? {
                let message = format!(
                    "planner card {card_id} reuses thread {thread_id} with \
                     {REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR} \
                     (re-run to mint a fresh thread)"
                );
                tracing::warn!(
                    target: "planner_harness::reusable_thread_invariant",
                    %card_id,
                    thread_id = %thread_id,
                    error = %message,
                    "refusing to reuse planner thread without per-card MCP token row; migration 0035 should have nulled this thread_id"
                );
                return Err(CalmError::Conflict(message));
            }
            thread_id
        } else {
            let developer_instructions = match profile {
                // A plain chat is a bare codex thread: no kernel prompt, no
                // MCP tools to describe.
                HarnessProfile::PlainChat => None,
                // The assistant gets its own prompt, not the planner one: the planner
                // prompt is mostly lifecycle/plan/verdict instructions for
                // tools the assistant is forbidden to call, and a template
                // binding drives the track's plan, which is likewise not the
                // assistant's business.
                //
                // #1343 — on Today's launchpad it gets a different *identity*
                // under the same tools: there, keeping the day's report current
                // is the job rather than a capability, and the ordinary
                // prompt's closing "you are a guest in a document the planner
                // agent maintains" is simply false, because no planner agent
                // writes that report. See
                // `LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE`.
                //
                // The criterion is `routes::today::is_launchpad_track`, the
                // same call the activity briefing makes. One criterion on
                // purpose: two spellings could send a conversation its material
                // and then start it under an identity that says the document is
                // not its to write.
                HarnessProfile::Assistant => {
                    let template =
                        if crate::routes::today::is_launchpad_track(self.repo.as_ref(), &track_id)
                            .await?
                        {
                            crate::planner_card::LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE
                        } else {
                            crate::planner_card::ASSISTANT_SYSTEM_PROMPT_TEMPLATE
                        };
                    Some(crate::planner_card::render_system_prompt(
                        template, &track_id,
                    ))
                }
                HarnessProfile::Planner => {
                    let bound_template = self.bound_template(&track_id).await?;
                    Some(render_planner_developer_instructions(
                        &track_id,
                        bound_template.as_ref().map(|bound| &bound.descriptor),
                        bound_template
                            .as_ref()
                            .and_then(|bound| bound.input.as_ref()),
                    ))
                }
            };
            let (raw, hashed) = mint_card_mcp_token_pair();
            new_mcp_token_hash = Some(hashed);
            let socket_path = self.mcp_socket_path_for_thread()?;
            // #838 (lean Move 1): build the channel-3 `thread/start` config
            // (`shell_environment_policy.set.{NEIGE_MCP_SOCKET,NEIGE_MCP_TOKEN}`)
            // through the single shared producer so the worker, cold-respawn,
            // and planner spawn paths all emit the byte-identical shape from one
            // place. Previously this path wrapped `card_mcp_env` in its own
            // parallel `PlannerThread*` structs.
            let params = SharedThreadStartParams {
                cwd,
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions,
                // The assistant needs the same channel-3 MCP credentials as the
                // planner harness — the block channel is its only write surface,
                // and `calm.report.read` is its report read surface. WHICH tools that token
                // can reach is not decided here: `tools/list` filters on
                // `ToolDescriptor::visible_to_roles` and every write handler
                // re-checks `require_role*`, both resolved from the card's
                // persisted role. So the §3.1 whitelist rides on
                // `minted_card_shape`'s role, and a third `ThreadConfig`
                // variant would carry nothing the kernel actually reads.
                config: match profile {
                    HarnessProfile::Planner | HarnessProfile::Assistant => ThreadConfig::McpShell {
                        socket_path: PathBuf::from(&socket_path),
                        raw_token: raw,
                    },
                    HarnessProfile::PlainChat => ThreadConfig::NoMcp,
                },
            };
            if runtime_deferred {
                self.daemon
                    .thread_start_mint_for_card(&card_id, params)
                    .await?
            } else {
                self.daemon
                    .thread_start_for_card(&card_id, card_role, Some(&track_id), params)
                    .await?
            }
        };
        output.set_output_data(
            "codex_thread_id",
            json!(thread_id.clone()),
            "planner harness",
        )?;
        let mut snapshot = output_snapshot(output)?;
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(thread_id.clone());
        output.set_output_data(
            "snapshot",
            serde_json::to_value(&snapshot)?,
            "planner harness",
        )?;
        let appserver_sock = self.daemon.remote_uri();

        // Nothing in this phase reads the `output.result` card snapshot the
        // previous transactional phase left behind — not its payload (see
        // `card_apply_harness_start_payload_tx`) and not its identity, which
        // the `card_id` / `track_id` strings above already carry out of
        // `output.data`. Deserializing it anyway would be a durability hazard
        // of exactly the kind this fix is about: `operations.tx_output_json` is
        // persisted replay input, so a change to `Card`'s serde shape would
        // make every in-flight operation fail hard on replay at a line that
        // needs nothing from the snapshot.
        let scope = card_scope(
            ctx.repo.as_ref(),
            CardId::from(card_id.clone()),
            TrackId::from(track_id.clone()),
        )
        .await?;
        let transcript_scope = scope.clone();
        let transcript_worker_session_id = worker_session_id.clone();
        let transcript_card_id = CardId::from(card_id.clone());
        let transcript_track_id = TrackId::from(track_id.clone());
        let write = WriteContext::new(self.card_role_cache.clone(), self.track_area_cache.clone());
        let op_clone = op.clone();
        let output_clone = output.clone();
        let thread_for_tx = thread_id.clone();
        // Destructured on the next statement rather than inline: the inline
        // pattern no longer fits one line, and wrapping it reindents the whole
        // transaction closure for no semantic reason.
        let (tx_out, _id) = write_with_event_typed(
            ctx.repo.as_ref(),
            payload.actor,
            scope,
            None,
            &ctx.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    let mut checkpoint_output = output_clone;
                    let mut old_worker_session_id = None;
                    let mut old_runtime_status = None;
                    // #1449 — what this transaction took off which row, for
                    // the undo journal. It has to leave the closure: the
                    // journal lives in `output`, and `read_harvested_from_journal`
                    // is the only thing compensation reads.
                    let mut taken_from: Vec<HarvestedFrom> = Vec::new();
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        persist_card_mcp_token_hash(tx, &card_id, hashed).await?;
                    }
                    if runtime_deferred {
                        let now = now_ms();
                        let occupant = session_projection_active_for_card_tx(tx, &card_id).await?;
                        // A runtime that is NOT this operation's own deferred
                        // placeholder raced into the card's active slot while
                        // the thread was being minted.
                        let raced_in = match occupant.as_ref() {
                            Some(existing) if existing.id != worker_session_id => {
                                Some(existing.clone())
                            }
                            _ => None,
                        };
                        // The successor's queue, plus — when a racer is being
                        // retired here — whatever the racer never delivered.
                        // #1449 S1 — the queue comes from THIS RUNTIME'S ROW,
                        // not from the snapshot `output` has been carrying
                        // since `prepare_tx`.
                        //
                        // `output` is durable and frozen at mint time. An
                        // operation re-driven from `tx_committed` or
                        // `app_server_interact` — any crash after the mint
                        // transaction — would otherwise write that frozen queue
                        // back onto the row, undoing a transfer another mint
                        // made in between and putting the sentence in two
                        // places. Re-reading at `spawn_side_effect` does not
                        // save it: this write happens first, so the re-read
                        // reads back the resurrected queue.
                        //
                        // What `output` still carries is this operation's own
                        // decisions — phase, thread id, watermarks — which no
                        // other operation touches.
                        let mut runtime_snapshot = snapshot.clone();
                        overwrite_queue_from_the_runtimes_own_row_tx(
                            tx,
                            &worker_session_id,
                            &mut runtime_snapshot,
                        )
                        .await?;
                        if let Some(existing) = raced_in.as_ref() {
                            old_worker_session_id = Some(existing.id.clone());
                            old_runtime_status = Some(existing.status);
                            checkpoint_output.set_output_data(
                                "old_runtime_id",
                                json!(existing.id.clone()),
                                "planner harness",
                            )?;
                            checkpoint_output.set_output_data(
                                "old_runtime_status",
                                serde_json::to_value(existing.status)?,
                                "planner harness",
                            )?;
                            // #1449 — supersede FIRST, then harvest, because
                            // the harvest predicate is `state = 'superseded'`
                            // and this row is the one being retired. Until now
                            // this branch dropped the racer's queue outright:
                            // a sentence a human typed into it in the window
                            // between the deferred mint and this write was lost
                            // with no error and no trace.
                            session_supersede_active_tx(tx, &existing.id, now).await?;
                            let harvested = harvest_pending_user_messages_tx(
                                tx,
                                &card_id,
                                &worker_session_id,
                                now,
                                stranded_user_messages,
                            )
                            .await?;
                            taken_from = harvested.taken_from;
                            // #1449 — the journal is written INSIDE this
                            // transaction, onto the checkpoint. Merging it into
                            // `output` only after the commit leaves a window in
                            // which the sentences are taken, the source rows
                            // are stamped, and a crash loses the record of
                            // both. The post-commit merge keeps `output` in
                            // step for the in-process path.
                            let mut journal = read_harvested_from_journal(&checkpoint_output)
                                .into_iter()
                                .map(|entry| HarvestedFrom {
                                    worker_session_id: entry.worker_session_id,
                                    messages: entry.messages,
                                })
                                .collect::<Vec<_>>();
                            journal.extend(taken_from.iter().cloned());
                            checkpoint_output.set_output_data(
                                "harvested_from",
                                harvested_from_journal(&journal),
                                "planner harness",
                            )?;
                            if !harvested.messages.is_empty() {
                                let mut entries = runtime_snapshot.pending_entries();
                                for message in harvested.messages {
                                    entries.push(QueueEntry::user_message_moved(
                                        message.text,
                                        message.ids,
                                    ));
                                }
                                runtime_snapshot.set_pending_entries(entries);
                            }
                        }
                        let runtime_init = WorkerSessionInit {
                            id: worker_session_id.clone(),
                            card_id: card_id.clone(),
                            kind: session_kind,
                            agent_provider: Some(AgentProvider::Codex),
                            status: WorkerSessionState::Starting,
                            terminal_run_id: None,
                            thread_id: Some(thread_for_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: Some(serde_json::to_value(&runtime_snapshot)?),
                            spawn_op_id: None,
                            now_ms: now,
                        };
                        match (occupant, raced_in) {
                            // This operation's own placeholder: superseded and
                            // re-inserted under the same id, exactly as before.
                            (Some(existing), None) => {
                                session_supersede_and_start_tx(tx, &existing.id, runtime_init)
                                    .await?;
                            }
                            // The racer is already superseded above, so the
                            // pair `supersede + start` is complete here.
                            (Some(_), Some(_)) => {
                                session_start_runtime_tx(tx, runtime_init).await?;
                            }
                            (None, _) => {
                                session_start_runtime_tx(tx, runtime_init).await?;
                            }
                        }
                    } else {
                        session_bind_attribution_tx(
                            tx,
                            &worker_session_id,
                            ThreadAttribution {
                                worker_session_id: worker_session_id.clone(),
                                provider: AgentProvider::Codex,
                                thread_id: Some(thread_for_tx.clone()),
                                session_id: None,
                                active_turn_id: None,
                            },
                        )
                        .await?;
                        // #1449 S1 — same rule on this arm.
                        let mut bound_snapshot = snapshot.clone();
                        overwrite_queue_from_the_runtimes_own_row_tx(
                            tx,
                            &worker_session_id,
                            &mut bound_snapshot,
                        )
                        .await?;
                        session_set_handle_state_tx(
                            tx,
                            &worker_session_id,
                            Some(serde_json::to_value(&bound_snapshot)?),
                        )
                        .await?;
                    }
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        mirror_session_mcp_token(tx, &worker_session_id, hashed).await?;
                    }
                    // #1252 S0-2 — the delete is a hard delete across the
                    // card's whole history, so measure the transcript here,
                    // inside the tx and strictly before the delete. After
                    // this line the evidence is gone for good; the numbers
                    // ride out on `Event::HarnessTranscriptCleared` below.
                    let mut cleared_measure = HarnessTranscriptMeasure::default();
                    if reset_harness_items {
                        cleared_measure = harness_items_measure_by_card_tx(tx, &card_id).await?;
                        harness_items_delete_by_card_tx(tx, &card_id).await?;
                    }
                    let card = card_apply_harness_start_payload_tx(
                        tx,
                        &card_id,
                        &thread_for_tx,
                        &appserver_sock,
                    )
                    .await?;
                    checkpoint_output.result = serde_json::to_value(&card)?;
                    checkpoint_output.target_id = Some(card.id.to_string());
                    checkpoint_app_server_interact_tx(
                        tx,
                        &op_clone,
                        AppServerInteractKind::MintAndAwait {
                            thread_id: Some(thread_for_tx),
                        },
                        &checkpoint_output,
                    )
                    .await?;
                    Ok((
                        (
                            card.clone(),
                            old_worker_session_id,
                            old_runtime_status,
                            cleared_measure,
                            taken_from,
                        ),
                        Event::CardUpdated(card),
                    ))
                })
            },
        )
        .await?;
        let (card, old_worker_session_id, old_runtime_status, cleared_measure, taken_from) = tx_out;
        drop(mint_lock_guard);
        // #1449 — merge this transaction's undo journal into the one
        // `prepare_tx` wrote. Both transfers this operation can make have to be
        // in it, or a compensation puts back only half of what it took.
        if !taken_from.is_empty() {
            let mut merged = read_harvested_from_journal(output)
                .into_iter()
                .map(|entry| HarvestedFrom {
                    worker_session_id: entry.worker_session_id,
                    messages: entry.messages,
                })
                .collect::<Vec<_>>();
            merged.extend(taken_from);
            output.set_output_data(
                "harvested_from",
                harvested_from_journal(&merged),
                "planner harness",
            )?;
        }
        if let Some(old_worker_session_id) = old_worker_session_id {
            output.set_output_data(
                "old_runtime_id",
                json!(old_worker_session_id),
                "planner harness",
            )?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            output.set_output_data(
                "old_runtime_status",
                serde_json::to_value(old_runtime_status)?,
                "planner harness",
            )?;
        }
        if reset_harness_items {
            ctx.repo
                .log_pure_event(
                    ActorId::Kernel,
                    transcript_scope,
                    None,
                    &ctx.events,
                    &self.card_role_cache,
                    &self.track_area_cache,
                    Event::HarnessTranscriptCleared {
                        worker_session_id: transcript_worker_session_id,
                        card_id: transcript_card_id,
                        track_id: transcript_track_id,
                        // Always `Some(..)` — the `Option` on the event exists
                        // only so pre-#1252 rows still deserialize on replay.
                        cleared_item_count: Some(cleared_measure.item_count),
                        cleared_params_bytes: Some(cleared_measure.params_bytes),
                        // Card age at reset, from the card row this tx just
                        // wrote. Clamped at 0: a clock step backwards must
                        // not report a negative age.
                        card_age_ms_at_clear: Some((now_ms() - card.created_at).max(0)),
                    },
                )
                .await?;
        }
        output.result = serde_json::to_value(&card)?;
        output.target_id = Some(card.id.to_string());

        Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id })
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let worker_session_id = output.output_string("runtime_id", "planner harness")?;
        let card_id = output.output_string("card_id", "planner harness")?;
        let track_id = output.output_string("track_id", "planner harness")?;
        let thread_id = output.output_optional_string("codex_thread_id", "planner harness")?;
        let mut snapshot = output_snapshot(output)?;
        // #1449 S1 — THE ROW IS THE SINGLE HOME FOR THE QUEUE.
        //
        // Everything else in this snapshot is the operation's own decision
        // (phase, thread id, watermarks) and rightly travels in `output`. The
        // pending queue is not: it is shared state that other operations move
        // between rows, and `output` is a durable copy written at
        // `prepare_tx` time that no later transfer can reach.
        //
        // Reading it from `output` is how a stranded sentence comes back from
        // the dead. An operation whose `prepare_tx` committed, and which is
        // re-driven later — after a crash, or on a second `AppState` over the
        // same file — carries a queue that a mint in between may already have
        // handed to somebody else. Starting the harness from that copy
        // delivers the same sentence twice, and `handle.persist_snapshot()`
        // then writes the resurrected queue back over the row.
        //
        // So the queue is re-read here, from the runtime's own row, at the
        // last moment before the harness is built.
        //
        // Unreadable or absent row: keep what `output` carries. That is not a
        // safety argument, it is the pre-#1449 behaviour preserved — a missing
        // row at spawn means the mint transaction's own insert is gone, and
        // this function is not where that gets adjudicated.
        overwrite_queue_from_the_runtimes_own_row(
            self.repo.as_ref(),
            &worker_session_id,
            &mut snapshot,
        )
        .await?;
        // #953 §5 — atomic replace claim: the old remove-then-insert pair
        // left a window where a concurrent registration could land between
        // the two ops. `reserve_replacing` swaps the slot to Reserved in one
        // entry op and hands back the previous Live handle for shutdown
        // outside the map lock.
        let (reservation, previous_live) = self
            .harness_registry
            .reserve_replacing(worker_session_id.clone());
        if let Some(existing) = previous_live {
            existing.shutdown().await?;
        }
        let handle = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: worker_session_id.clone(),
            track_id: TrackId::from(track_id),
            card_id: CardId::from(card_id),
            thread_id,
            repo: self.repo.clone(),
            events: ctx.events.clone(),
            card_role_cache: self.card_role_cache.clone(),
            track_area_cache: self.track_area_cache.clone(),
            daemon: self.daemon.clone(),
            config: HarnessConfig::default(),
            snapshot,
        });
        if !reservation.install(handle.clone()) {
            // Superseded by a concurrent `reserve_replacing` between our
            // reserve and install: shut down the handle we just built (never
            // leak its run loop) and fail the op — compensation handles the
            // row.
            handle.shutdown().await?;
            return Err(crate::error::CalmError::Internal(format!(
                "planner harness registration for runtime {worker_session_id} superseded during start"
            )));
        }
        handle.persist_snapshot().await?;
        Ok(SpawnOutcome::Ready(SpawnHandle::Harness {
            worker_session_id,
        }))
    }

    /// Boundary this compensation inherits (pre-existing driver semantics, NOT
    /// introduced with lazy card minting): `Driver::apply_compensation` marks
    /// the operation `Stuck` on the FIRST error from any step and boot recovery
    /// skips `Stuck` rows, so a compensation is never re-driven. If
    /// `delete_card` fails before its delete commits, the lazily minted card
    /// stays — and it carries `deletable: false`, so the user cannot remove it
    /// either. Every adapter shares this; making compensation retryable is a
    /// driver-level change, tracked separately.
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let payload: PlannerHarnessStartOperationPayload =
            serde_json::from_value(op.payload.clone())?;
        let card_id = output.output_string("card_id", "planner harness")?;
        let worker_session_id = output.output_string("runtime_id", "planner harness")?;
        let thread_id = output.output_optional_string("codex_thread_id", "planner harness")?;
        let mut steps = Vec::new();
        // #1098 §5.6 — a card this operation minted must come back out on
        // EVERY failure path. The step is built here, before any early return,
        // and appended by `finish` at the single point where each return path
        // materializes its state — including the zero-step
        // `is_reusable_thread_missing_card_mcp_token_failure` exit below, which
        // would otherwise leave an orphan card with no session behind it. It is
        // deliberately unconditional rather than a patch on that one arm: a
        // future early return that goes through `finish` cannot silently
        // reintroduce the orphan.
        //
        // It runs LAST rather than first: on the `SpawnStarted`/`SpawnSucceeded`
        // arms the harness task is still alive, and deleting the card (plus its
        // session and cascaded `harness_items`) out from under a running run
        // loop is the wrong order. Stop the task, then remove the row.
        let delete_card_step = match payload.create_card.is_some() {
            true => {
                let track_id = output.output_string("track_id", "planner harness")?;
                Some(CompensationStep::new(
                    "delete_card",
                    json!({ "card_id": card_id, "track_id": track_id }),
                ))
            }
            false => None,
        };
        let finish = |mut steps: Vec<CompensationStep>| {
            steps.extend(delete_card_step.clone());
            CompensationStateVersioned {
                version: 1,
                from_phase,
                reason: reason.to_string(),
                steps,
            }
        };
        if from_phase == PhaseTag::AppServerInteract
            && is_reusable_thread_missing_card_mcp_token_failure(reason)
        {
            // #1449 — this arm returns before `fail_runtime`, so it never runs
            // the give-back either. That matters only if this operation's
            // transaction had already taken sentences off other rows, and it
            // had not: this refusal is raised while assembling the thread-start
            // request, before the transaction that harvests. The journal it
            // would have replayed is empty.
            //
            // Checked, and NOT with `debug_assert`: that compiles out of a
            // release build, so the thing it guards would be dropped exactly
            // where nobody is watching — the failure mode the paragraph above
            // it argues against. If the journal is not empty the assumption is
            // wrong and the give-back has to run rather than be skipped.
            // `finish` appends `delete_card` after whatever is pushed here, and
            // `delete_card` cascades `DELETE FROM worker_sessions WHERE card_id`
            // — which would take the source rows with it before a later replay
            // could read them. It only appends that step when this operation
            // minted the card, and an operation that minted its own card has no
            // predecessor on it to harvest from, so the two cannot co-occur.
            // Untested; written down because it is the ordering that would
            // matter if either half changed.
            if !read_harvested_from_journal(output).is_empty() {
                tracing::error!(
                    card_id = %card_id,
                    "planner harness: a compensation arm that assumed an empty harvest journal \
                     found one; adding `fail_runtime` so the give-back runs"
                );
                steps.push(CompensationStep::new(
                    "fail_runtime",
                    json!({ "runtime_id": worker_session_id }),
                ));
            }
            return Ok(finish(steps));
        }
        if matches!(
            from_phase,
            PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) {
            steps.push(CompensationStep::new(
                "abort_harness_task",
                json!({ "runtime_id": worker_session_id }),
            ));
        }
        if matches!(
            from_phase,
            PhaseTag::AppServerInteract | PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) && (!payload.force_new_thread || thread_id.is_some())
        {
            steps.push(CompensationStep::new(
                "interrupt_thread",
                json!({
                    "card_id": card_id,
                    "thread_id": thread_id,
                }),
            ));
        }
        steps.push(CompensationStep::new(
            "fail_runtime",
            json!({ "runtime_id": worker_session_id }),
        ));
        if let Some(old_worker_session_id) =
            output.output_optional_string("old_runtime_id", "planner harness")?
        {
            let old_runtime_status =
                output
                    .data
                    .get("old_runtime_status")
                    .cloned()
                    .ok_or_else(|| {
                        CalmError::Internal(
                            "planner harness tx_output missing old_runtime_status".into(),
                        )
                    })?;
            steps.push(CompensationStep::new(
                "restore_old_runtime",
                json!({
                    "runtime_id": old_worker_session_id,
                    "status": old_runtime_status,
                }),
            ));
        }
        Ok(finish(steps))
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        match step.op.as_str() {
            "abort_harness_task" => {
                let worker_session_id = step.arg_string("runtime_id", "planner harness")?;
                if let Some(handle) = self.harness_registry.remove(&worker_session_id) {
                    handle.shutdown().await?;
                }
                Ok(())
            }
            "interrupt_thread" => {
                if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str)
                    && let Err(e) = self.daemon.interrupt_active_turn(thread_id).await
                {
                    tracing::warn!(thread_id, error = %e, "planner harness compensation interrupt failed");
                }
                let card_id = step.arg_string("card_id", "planner harness")?;
                clear_card_runtime_fields(ctx, &card_id).await?;
                Ok(())
            }
            // #1449 S3 — failing the runtime and giving its harvested
            // sentences back are ONE transaction, not two steps.
            //
            // They were two, and that did not hold: `resume_compensation` marks
            // the operation `Stuck` on the first step error, and boot recovery
            // skips `Stuck` rows, so a later step is not re-driven. A give-back
            // that never ran would leave the harvested sentences on a `failed`
            // row — a state the harvest predicate does not read — with the rows
            // they came from already stamped. Atomicity replaces that ordering
            // argument: either the runtime is failed and the sentences are
            // back, or neither happened and the operation is Stuck with the
            // sentences still on one row.
            //
            // What comes back is conditioned on identity, not on the journal
            // alone: only messages whose ids are still on this runtime's queue.
            // If another mint has since taken them onward, they are not here to
            // return, so they do not end up in two places. An entry with no ids
            // (enqueued before #1449) is skipped rather than matched on text.
            "fail_runtime" => {
                let worker_session_id = step.arg_string("runtime_id", "planner harness")?;
                let journal = read_harvested_from_journal(_output);
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        return_harvested_queues_and_fail_tx(tx, &worker_session_id, &journal).await
                    })
                })
                .await
            }
            // #1098 §5.6 — undo the lazily minted chat card. Tolerant of an
            // already-absent row so a re-driven compensation is idempotent;
            // emits `card.deleted` because `card.added` was already broadcast
            // at commit, and a client that saw the add must see the removal.
            "delete_card" => {
                let card_id = step.arg_string("card_id", "planner harness")?;
                let track_id = step.arg_string("track_id", "planner harness")?;
                if ctx.repo.card_get(&card_id).await?.is_none() {
                    return Ok(());
                }
                let card_id = CardId::from(card_id);
                let track_id = TrackId::from(track_id);
                let scope =
                    card_scope(ctx.repo.as_ref(), card_id.clone(), track_id.clone()).await?;
                let write =
                    WriteContext::new(self.card_role_cache.clone(), self.track_area_cache.clone());
                let write_for_tx = write.clone();
                // Narrow to the single deprecated call. A function-level allow
                // here would silently cover every other arm of this match.
                #[allow(deprecated)]
                let (_unit, _id) = write_with_event_typed(
                    ctx.repo.as_ref(),
                    ActorId::Kernel,
                    scope,
                    None,
                    &ctx.events,
                    &write,
                    move |tx| {
                        Box::pin(async move {
                            card_delete_tx(tx, card_id.as_str(), write_for_tx.role_cache()).await?;
                            Ok((
                                (),
                                Event::CardDeleted {
                                    id: card_id,
                                    track_id,
                                },
                            ))
                        })
                    },
                )
                .await?;
                Ok(())
            }
            "restore_old_runtime" => {
                let worker_session_id = step.arg_string("runtime_id", "planner harness")?;
                let status = step_arg_run_status(step, "status")?;
                restore_old_runtime_after_spawn_failure(
                    ctx.repo.as_ref(),
                    worker_session_id,
                    status,
                )
                .await
            }
            "delete_runtime" => {
                let worker_session_id = step.arg_string("runtime_id", "planner harness")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        session_delete_tx(tx, &worker_session_id)
                            .await
                            .map_err(CalmError::from)?;
                        Ok(())
                    })
                })
                .await
            }
            other => Err(CalmError::Internal(format!(
                "unknown planner harness start compensation op {other}"
            ))),
        }
    }
}

fn is_reusable_thread_missing_card_mcp_token_failure(reason: &str) -> bool {
    reason.contains(REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR)
}

/// #1449 S1 — replace a snapshot's pending queue with the one persisted on the
/// runtime's own row, leaving every other field alone.
fn adopt_queue_from_row(snapshot: &mut HarnessSnapshot, row_state: Option<Value>) {
    let Some(state) = row_state else {
        return;
    };
    if state.get("mode").and_then(Value::as_str) != Some(HARNESS_MODE)
        || !is_harness_snapshot_value(&state)
    {
        return;
    }
    let row = HarnessSnapshot::from_value_strict(state);
    snapshot.set_pending_entries(row.pending_entries());
}

async fn overwrite_queue_from_the_runtimes_own_row(
    repo: &dyn Repo,
    worker_session_id: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let state = repo
        .session_projection_handle_state_by_id(worker_session_id)
        .await?;
    adopt_queue_from_row(snapshot, state);
    Ok(())
}

/// The same rule inside a transaction, for the writers that run there.
async fn overwrite_queue_from_the_runtimes_own_row_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_session_id: &str,
    snapshot: &mut HarnessSnapshot,
) -> Result<()> {
    let state = session_handle_state_by_id_tx(tx, worker_session_id)
        .await
        .map_err(CalmError::from)?;
    adopt_queue_from_row(snapshot, state);
    Ok(())
}

/// #1449 S3 — fail the runtime and return what it harvested, in one
/// transaction.
///
/// Order inside the transaction is deliberate: read the successor's queue,
/// decide what is still there, write the source rows, write the successor, then
/// fail it. A partial application is not reachable from here — either the
/// transaction commits or none of it happened.
async fn return_harvested_queues_and_fail_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_session_id: &str,
    journal: &[HarvestedFromJournalEntry],
) -> Result<()> {
    let now = crate::model::now_ms();
    let successor_state = session_handle_state_by_id_tx(tx, worker_session_id)
        .await
        .map_err(CalmError::from)?;
    if let Some(state) = successor_state
        && is_harness_snapshot_value(&state)
    {
        let mut successor = HarnessSnapshot::from_value_strict(state);
        let successor_entries = successor.pending_entries();
        let held: std::collections::HashSet<&str> = successor_entries
            .iter()
            .flat_map(|entry| entry.message_ids().iter().map(String::as_str))
            .collect();
        let mut returned_any_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for entry in journal {
            let returning: Vec<&HarvestedMessage> = entry
                .messages
                .iter()
                .filter(|m| m.ids.iter().any(|id| held.contains(id.as_str())))
                .collect();
            if returning.is_empty() {
                continue;
            }
            let Some(source_state) = session_handle_state_by_id_tx(tx, &entry.worker_session_id)
                .await
                .map_err(CalmError::from)?
            else {
                continue;
            };
            if !is_harness_snapshot_value(&source_state) {
                continue;
            }
            let mut source = HarnessSnapshot::from_value_strict(source_state);
            // #1449 — idempotent against the SOURCE row, not only against a
            // re-driven compensation.
            //
            // A retired runtime can put a batch back on its own row after the
            // harvest took it: `maybe_issue_turn`'s `turn/start` error arm
            // re-buffers the batch, and `persist_issuance_outcome` writes that
            // in-process queue to the retired row. If this mint then fails, the
            // failing runtime still holds those ids so they qualify for return,
            // and pushing them would leave the row carrying the same instance
            // twice — which `restore_old_runtime` revives and delivers twice.
            let mut source_entries = source.pending_entries();
            let already_on_source: std::collections::HashSet<String> = source_entries
                .iter()
                .flat_map(|entry| entry.message_ids().iter().cloned())
                .collect();
            for message in &returning {
                if message
                    .ids
                    .iter()
                    .any(|id| already_on_source.contains(id.as_str()))
                {
                    // Already back where it belongs. Still counts as returned,
                    // so it is pruned from the failing runtime below.
                    returned_any_ids.extend(message.ids.iter().cloned());
                    continue;
                }
                // The returned entry takes a FRESH `QueueEntryId` — the one it
                // had on this row before the harvest did not survive the
                // journal, which carries text and message ids only. Its
                // transfer identity is what has to be the same instance, and
                // that is preserved verbatim.
                source_entries.push(QueueEntry::user_message_moved(
                    message.text.clone(),
                    message.ids.clone(),
                ));
                returned_any_ids.extend(message.ids.iter().cloned());
            }
            source.set_pending_entries(source_entries);
            session_set_handle_state_of_any_runtime_tx(
                tx,
                &entry.worker_session_id,
                Some(serde_json::to_value(&source)?),
                now,
            )
            .await
            .map_err(CalmError::from)?;
            // The row is harvestable again only because something was put back
            // on it. A row this operation stamped and returned nothing to keeps
            // its stamp: its queue is somewhere else, legitimately.
            tracing::info!(
                worker_session_id = %worker_session_id,
                returned_to = %entry.worker_session_id,
                returned = returning.len(),
                "planner harness: a failed mint returned undelivered user messages"
            );
            session_clear_queue_harvested_tx(tx, &entry.worker_session_id)
                .await
                .map_err(CalmError::from)?;
        }
        if !returned_any_ids.is_empty() {
            // #1449 — SUBTRACT the returned ids; do not delete the entry.
            //
            // `queue::try_fold_tail` unions two `UserMessage`s into one entry
            // under backpressure, so an entry can carry a returned id next to a
            // newly enqueued one that was never harvested and sits on no source
            // row. Dropping the entry on an intersection would delete that
            // sentence outright.
            //
            // The folded TEXT cannot be split — the fold concatenated it and
            // nothing records where the seam was — so an entry that keeps at
            // least one id keeps all of its text. The returned sentence's words
            // then sit on the failed runtime as well as on the source row; the
            // harvest never reads a `failed` row, so that is dead text rather
            // than a second delivery.
            let mut kept = Vec::new();
            for mut entry in successor_entries {
                // Two different things make an entry hold no ids, and only one
                // of them means "returned"; `remove_message_ids` answers the
                // narrower question and says why.
                let emptied_by_the_return = entry.remove_message_ids(&returned_any_ids);
                if emptied_by_the_return && entry.is_user_authored() {
                    continue;
                }
                kept.push(entry);
            }
            successor.set_pending_entries(kept);
            session_set_handle_state_of_any_runtime_tx(
                tx,
                worker_session_id,
                Some(serde_json::to_value(&successor)?),
                now,
            )
            .await
            .map_err(CalmError::from)?;
        }
    }
    session_fail_if_active_runtime_tx(tx, &worker_session_id.to_string())
        .await
        .map_err(CalmError::from)
}

/// #1449 S3 — the undo journal, as JSON for `operations.tx_output_json`.
fn harvested_from_journal(taken_from: &[HarvestedFrom]) -> Value {
    Value::Array(
        taken_from
            .iter()
            .map(|from| {
                json!({
                    "worker_session_id": from.worker_session_id,
                    "messages": from
                        .messages
                        .iter()
                        .map(|m| json!({"text": m.text, "ids": m.ids}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// One journal entry read back at compensation time.
struct HarvestedFromJournalEntry {
    worker_session_id: String,
    messages: Vec<HarvestedMessage>,
}

fn read_harvested_from_journal(output: &TxOutput) -> Vec<HarvestedFromJournalEntry> {
    let Some(Value::Array(entries)) = output.data.get("harvested_from") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let worker_session_id = entry.get("worker_session_id")?.as_str()?.to_string();
            let messages = entry
                .get("messages")?
                .as_array()?
                .iter()
                .filter_map(|m| {
                    Some(HarvestedMessage {
                        text: m.get("text")?.as_str()?.to_string(),
                        ids: m
                            .get("ids")?
                            .as_array()?
                            .iter()
                            .filter_map(|id| id.as_str().map(str::to_owned))
                            .collect(),
                    })
                })
                .collect();
            Some(HarvestedFromJournalEntry {
                worker_session_id,
                messages,
            })
        })
        .collect()
}

/// #1449 — the human sentences a superseded runtime never delivered.
///
/// The filter is the product ruling, and it is narrow on purpose: **only**
/// `Observation::UserMessage` travels. `SystemContext` (the #1343 opening
/// briefing) and `TrackGoal` are functions of the successor's OWN workspace and
/// payload — after a re-point that is a different directory — so the successor
/// re-derives them and carrying the predecessor's copies forward would inject a
/// briefing describing the workspace the track just left. Every other
/// observation (report bodies, hook observations, diff blocks) describes the old
/// workspace at an old head; carrying it is worse than dropping it.
///
/// Bad shapes warn and yield nothing rather than failing the mint — the same
/// posture the dormant-restart inherit above takes — and the caller stamps the
/// row either way, so a snapshot nothing can read is not re-examined forever.
///
/// # KNOWN GAP — a batch the daemon has and the row does not know about
///
/// `maybe_issue_turn` persists "the batch is still queued", drains in memory,
/// calls `turn/start`, and writes the emptied queue afterwards. That last write
/// is `persist_issuance_outcome`, a SEPARATE `write_in_tx_typed` that has to
/// take SQLite's single writer lock — contended by the very mint transactions
/// that cause this hazard. The window is therefore
/// `[the second carrier check, persist_issuance_outcome commits]`: one RPC plus
/// one writer-lock acquisition, not one RPC.
///
/// The reachable path is a MINT — reset, `POST /conversations`, Today ensure —
/// where the predecessor's handle is not torn down until `app_server_interact`,
/// long after `prepare_tx` harvested. On the re-point fence it is narrower:
/// `shutdown_fenced_harness` is awaited before the restart, and
/// `shutdown_inner` queues behind `inner.issuance`, which the run loop holds
/// across `persist_issuance_outcome`.
///
/// Nothing here closes it: the persisted queue and what the daemon accepted
/// have no shared truth to compare, and giving them one needs an idempotency
/// key on the daemon side. The direction matches #1314's ruling — re-deliver
/// rather than record a delivery that may not have happened.
fn stranded_user_messages(worker_session_id: &str, handle_state_json: &str) -> HarvestOutcome {
    let Ok(state) = serde_json::from_str::<Value>(handle_state_json) else {
        tracing::warn!(
            worker_session_id,
            "harvest: superseded runtime snapshot is not JSON; leaving its queue behind"
        );
        return HarvestOutcome::default();
    };
    if state.get("mode").and_then(Value::as_str) != Some(HARNESS_MODE) {
        return HarvestOutcome::default();
    }
    if !is_harness_snapshot_value(&state) {
        tracing::warn!(
            worker_session_id,
            "harvest: superseded runtime snapshot has corrupt/unknown shape; \
             leaving its queue behind"
        );
        return HarvestOutcome::default();
    }
    let snapshot = HarnessSnapshot::from_value_strict(state);
    // `pending_entries` pads every side array to the queue's length, so this
    // walk is total: a pre-#1449 snapshot yields an empty id set per entry
    // rather than a short array that would pair ids with the wrong sentences.
    // #1449 S2 — a MOVE: the row keeps what was not taken and loses what was.
    let mut remaining = snapshot.clone();
    let mut kept = Vec::new();
    let mut taken = Vec::new();
    for mut entry in snapshot.pending_entries() {
        // #1505 PR1 — the filter is `is_user_authored`, so a `LegacyUser`
        // (a user sentence from a row written before PR1) is harvested exactly
        // like a `User`. Filtering on the addressable variant alone would leave
        // every pre-PR1 sentence stranded, which is the loss #1449 exists to
        // stop.
        if entry.is_user_authored() {
            // #1449 — MINT AT THE TRANSFER BOUNDARY.
            //
            // An entry enqueued before this field existed has no ids, and
            // the give-back returns only ids the failing runtime still
            // holds — so an id-less entry could be moved off its row and
            // never returned, ending on a `failed` successor that the
            // harvest does not read and `restore_old_runtime` does not
            // revive. Every pre-upgrade entry is in that class, and
            // migration 0095 leaves live rows unstamped precisely so their
            // queues stay harvestable, which is what puts them there.
            //
            // Minting here rather than at load: the same id goes into the
            // successor's snapshot and into the journal entry, in one
            // transaction, so the instance is identifiable from the moment
            // it moves. Minting on load would give the same entry a
            // different id on every read.
            let ids = entry.ensure_message_id().to_vec();
            let Observation::UserMessage { text } = entry.observation() else {
                continue;
            };
            taken.push(HarvestedMessage { text, ids });
        } else {
            kept.push(entry);
        }
    }
    remaining.set_pending_entries(kept);
    let Ok(remaining_snapshot) = serde_json::to_value(&remaining) else {
        // `None` has one meaning to the caller — "nothing was taken" — so a
        // remainder that will not serialize takes nothing, rather than turning
        // the move into a copy.
        tracing::warn!(
            worker_session_id,
            "harvest: could not re-serialize the remainder of this snapshot; leaving its queue \
             behind"
        );
        return HarvestOutcome::default();
    };
    HarvestOutcome {
        taken,
        remaining_snapshot: Some(remaining_snapshot),
    }
}

fn output_snapshot(output: &TxOutput) -> Result<HarnessSnapshot> {
    let value = output
        .data
        .get("snapshot")
        .cloned()
        .ok_or_else(|| CalmError::Internal("planner harness output missing snapshot".into()))?;
    // #1449 — through the single aligner, not a bare `from_value`.
    //
    // A pre-#1449 `tx_output_json` has an N-entry queue and no
    // `pending_message_ids` at all, which decodes to an EMPTY outer vec. Left
    // unaligned, the raced-in harvest appends to the queue and only then
    // aligns, so the harvested ids land on the FIRST entries of the queue and
    // the harvested sentences end up with none.
    //
    // Guarded, because `from_value_strict` panics on an unknown
    // `schema_version` and this value comes off disk: a pending operation
    // minted by an older binary and re-driven by a newer one would take the
    // whole operation runner down. Unreachable while the version is 1, which is
    // exactly how long such a guard looks unnecessary.
    if !is_harness_snapshot_value(&value) {
        return Err(CalmError::Internal(
            "planner harness output snapshot has an unreadable shape".into(),
        ));
    }
    Ok(HarnessSnapshot::from_value_strict(value))
}

fn output_bool(output: &TxOutput, key: &str) -> Result<bool> {
    match output.data.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        None => Ok(false),
        Some(_) => Err(CalmError::Internal(format!(
            "planner harness tx_output {key} must be bool"
        ))),
    }
}

fn output_existing_thread_id(output: &TxOutput) -> Result<Option<String>> {
    Ok(output
        .output_optional_string("codex_thread_id", "planner harness")?
        .filter(|id| !id.trim().is_empty()))
}

/// Write this adapter's payload keys onto the card, merging them into the
/// payload **as it stands inside `tx`** rather than onto a snapshot taken
/// earlier.
///
/// #1505 S4-1. The previous shape cloned the payload out of
/// `TxOutput::result` — a card snapshot produced by the previous
/// transactional phase — mutated the keys below on that clone, and handed the
/// whole thing to `card_update_tx`, which replaces the card's `payload`
/// column wholesale. Between the snapshot and this write sits a cross-process
/// `thread/start` JSON-RPC call, and, because an operation is a durable
/// resumable entity, a process restart: on a recovery replay the snapshot can
/// be minutes or hours old. Every key another writer had put into the payload
/// in that window was silently dropped.
///
/// The six keys touched below are the ones this adapter owns. Everything else
/// is read fresh from the row and written back untouched. Note the key set is
/// deliberately NOT the same as `clear_card_runtime_fields`': that one clears
/// five keys and does not know about `appserver_needs_initial_prompt`.
async fn card_apply_harness_start_payload_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    thread_id: &str,
    appserver_sock: &str,
) -> Result<Card> {
    let mut payload = card_payload_get_tx(tx, card_id).await?;
    let Some(map) = payload.as_object_mut() else {
        return Err(CalmError::Internal(format!(
            "planner harness card {card_id} payload is not a JSON object"
        )));
    };
    map.insert(
        "codex_thread_id".into(),
        Value::String(thread_id.to_string()),
    );
    map.insert(
        "appserver_sock".into(),
        Value::String(appserver_sock.to_string()),
    );
    map.remove("appserver_pgid");
    map.remove("appserver_start_time");
    map.remove("appserver_boot_id");
    map.remove("appserver_needs_initial_prompt");
    card_update_tx(
        tx,
        card_id,
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await
    .map_err(CalmError::from)
}

async fn clear_card_runtime_fields(ctx: &SpawnCtx, card_id: &str) -> Result<()> {
    let card_id = card_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
                .bind(&card_id)
                .fetch_optional(&mut **tx)
                .await?;
            let Some((payload_text,)) = row else {
                return Ok(());
            };
            let mut payload: Value = serde_json::from_str(&payload_text).map_err(|e| {
                CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}"))
            })?;
            let Some(map) = payload.as_object_mut() else {
                return Ok(());
            };
            map.remove("codex_thread_id");
            map.remove("appserver_sock");
            map.remove("appserver_pgid");
            map.remove("appserver_start_time");
            map.remove("appserver_boot_id");
            let _card = card_update_tx(
                tx,
                &card_id,
                CardPatch {
                    title: None,
                    kind: None,
                    sort: None,
                    payload: Some(payload),
                    deletable: None,
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
}

async fn restore_old_runtime_after_spawn_failure(
    repo: &dyn crate::db::RouteRepo,
    old_worker_session_id: String,
    status: WorkerSessionState,
) -> Result<()> {
    active_run_status_to_db(&status)?;
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_restore_from_superseded_runtime_tx(tx, &old_worker_session_id, status)
                .await
                .map_err(CalmError::from)
        })
    })
    .await
}

fn active_run_status_to_db(status: &WorkerSessionState) -> Result<&'static str> {
    if status.is_terminal() {
        Err(CalmError::Internal(format!(
            "cannot restore old planner harness runtime to terminal status {status:?}"
        )))
    } else {
        Ok(status.as_db_str())
    }
}

fn step_arg_run_status(step: &CompensationStep, key: &str) -> Result<WorkerSessionState> {
    let value = step.args.get(key).cloned().ok_or_else(|| {
        CalmError::Internal(format!(
            "planner harness compensation step {} missing {key}",
            step.op
        ))
    })?;
    Ok(serde_json::from_value(value)?)
}

// The per-card lock behavior test moved to `crate::per_card_lock::tests`
// alongside the lifted implementation (issue #649 i2).

#[cfg(test)]
mod tests {

    /// #1449 — the decoder the harvest runs over every retired row it reads.
    ///
    /// Four inputs reach it in production and only one of them is a snapshot:
    /// `handle_state_json` is free-form TEXT, a terminal or Claude runtime
    /// writes a different dialect into it (or nothing at all), and a snapshot
    /// written by an older binary can fail the strict shape check. Every one of
    /// those must yield nothing rather than fail the mint — the caller stamps
    /// the row either way, so a snapshot nothing can read is not re-examined
    /// forever.
    #[test]
    fn the_harvest_decoder_yields_only_human_sentences_and_never_fails() {
        // Not JSON at all.
        assert!(
            super::stranded_user_messages("r1", "not json")
                .taken
                .is_empty()
        );
        // JSON, but not this mode: a terminal/Claude runtime's own dialect.
        assert!(
            super::stranded_user_messages("r1", r#"{"mode":"terminal","pending_queue":[]}"#)
                .taken
                .is_empty()
        );
        // Right mode, shape the strict reader refuses.
        assert!(
            super::stranded_user_messages(
                "r1",
                r#"{"mode":"harness","schema_version":9999,"pending_queue":[]}"#
            )
            .taken
            .is_empty()
        );
        // Right mode, valid shape, empty queue.
        let empty = serde_json::to_string(&crate::harness::initial_snapshot_with_goal(None))
            .expect("serialize snapshot");
        assert!(super::stranded_user_messages("r1", &empty).taken.is_empty());

        // The one that carries something — and the filter that is the product
        // ruling: the human's sentence travels, the machine's context does not.
        let mut snapshot = crate::harness::initial_snapshot_with_goal(Some("the goal".into()));
        let mut entries = snapshot.pending_entries();
        entries.push(
            QueueEntry::system(
                Observation::SystemContext {
                    text: "briefing for the old workspace".into(),
                },
                None,
            )
            .expect("a system context wraps as a system entry"),
        );
        entries.push(QueueEntry::user_message("first thing said".into(), None));
        entries.push(QueueEntry::user_message("second thing said".into(), None));
        snapshot.set_pending_entries(entries);
        // #1449 — the ids must travel with the text: the decoder is transport,
        // not a producer.
        let carried = super::stranded_user_messages(
            "r1",
            &serde_json::to_string(&snapshot).expect("serialize snapshot"),
        );
        assert_eq!(
            carried
                .taken
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first thing said", "second thing said"],
            "only `UserMessage`, in queue order: `TrackGoal` and `SystemContext` are functions \
             of the SUCCESSOR's payload and cwd, which after a re-point is a different directory"
        );
    }

    /// #1514 review — the two transfer boundaries do DIFFERENT things to a
    /// `LegacyUser`, which is why the doc on that variant names them separately
    /// instead of quantifying over "a transfer boundary".
    ///
    /// The sentence it replaced said a legacy entry gaining a message id "does
    /// not make it addressable — `user_view` still denies it". True of
    /// `ensure_message_id` and of the reset inherit, and false of the harvest:
    /// the journal carries text and message ids only, so the successor has to
    /// rebuild the entry, and the only user-authored thing it can rebuild it as
    /// is a `User`. Addressability is therefore path-dependent, and a
    /// universally quantified sentence over "a transfer boundary" is wrong
    /// whichever way it is pointed.
    ///
    /// This test is the carrier for both halves at once, so neither can drift
    /// back into a single claim.
    #[test]
    fn the_harvest_makes_a_legacy_sentence_addressable_and_the_inherit_does_not() {
        // A row written before #1505 PR1: user text in `pending_queue`, no meta
        // slot beside it. Reached through JSON because no constructor produces
        // it, deliberately.
        let seeded = crate::harness::HarnessSnapshot::initial(
            0,
            vec![QueueEntry::user_message(
                "said before the upgrade".into(),
                None,
            )],
        );
        let mut row = serde_json::to_value(&seeded).expect("serialize snapshot");
        row["pending_entry_meta"][0] = Value::Null;
        row["pending_message_ids"][0] = json!([]);
        let snapshot = crate::harness::HarnessSnapshot::from_value_strict(row);
        let legacy = snapshot.pending_entries();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].id(), None, "premise: it is a LegacyUser");
        assert!(
            legacy[0].message_ids().is_empty(),
            "premise: and has no identity of either kind"
        );

        // THE INHERIT carries the entry whole, so it stays legacy. This is what
        // `prepare_tx` does with `inherited.pending_entries()`.
        let mut inherited = legacy.clone();
        let minted_in_place = inherited[0].ensure_message_id().to_vec();
        assert_eq!(minted_in_place.len(), 1, "it does gain a transfer identity");
        assert_eq!(inherited[0].id(), None, "…and still no queue id");
        assert!(
            inherited[0].user_view().is_none(),
            "the inherit boundary leaves it OFF the addressable page — GAP-B verbatim"
        );

        // THE HARVEST cannot: it round-trips through a journal that holds text
        // and message ids only.
        let taken = super::stranded_user_messages(
            "r1",
            &serde_json::to_string(&snapshot).expect("serialize snapshot"),
        )
        .taken;
        assert_eq!(
            taken.len(),
            1,
            "a legacy sentence is harvested like any other"
        );
        assert_eq!(taken[0].ids.len(), 1, "with an id minted at the boundary");
        let rebuilt = QueueEntry::user_message_moved(taken[0].text.clone(), taken[0].ids.clone());
        assert!(
            rebuilt.user_view().is_some(),
            "the harvest boundary DOES make it addressable, and the doc must say so"
        );
        assert_eq!(
            rebuilt.message_ids(),
            taken[0].ids.as_slice(),
            "…while carrying the SAME transfer identity, so the give-back still recognises it"
        );
    }

    /// The persisted wire shape of [`PlannerHarnessStartOperationPayload`],
    /// pinned as a golden.
    ///
    /// Not a style check. This payload is stored in `operations.payload_json`
    /// with a SHA-256 of its serialization in `operations.payload_hash`, and
    /// the runtime turns "same idempotency key, different payload hash" into a
    /// permanent 409 that nothing cleans up. #1316 S2 renamed one of these
    /// fields without noticing, which is how this test came to exist; S3 froze
    /// them and added this so the next rename fails here instead of in
    /// production on the first `ensure` after a deploy.
    ///
    /// If this test fails, the fix is NOT to update the expectation.
    #[test]
    fn the_persisted_payload_field_names_are_frozen() {
        let payload = PlannerHarnessStartOperationPayload {
            actor: ActorId::Kernel,
            track_id: "t1".into(),
            planner_card_id: CardId::from("c1"),
            report_card_id: None,
            sort: None,
            cwd: "/tmp".into(),
            goal: None,
            reset_harness_items: false,
            force_new_thread: false,
            profile: HarnessProfile::Planner,
            create_card: None,
            opening_briefing: None,
            first_message: None,
            create_request_sha256: None,
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        let object = json.as_object().expect("object");
        assert!(
            object.contains_key("wave_id") && !object.contains_key("track_id"),
            "the track field must still serialize as `wave_id`: {json}"
        );
        assert!(
            object.contains_key("spec_card_id") && !object.contains_key("planner_card_id"),
            "the planner-card field must still serialize as `spec_card_id`: {json}"
        );
        assert_eq!(
            object.get("profile").and_then(serde_json::Value::as_str),
            Some("spec"),
            "the default profile must still serialize as `spec`: {json}"
        );

        // And the S2-era spelling still reads back, for installs that ran it.
        // Derived from the real serialization rather than hand-written, so the
        // case cannot drift out of shape as unrelated fields change.
        let mut s2_era = object.clone();
        let track = s2_era.remove("wave_id").expect("wave_id present");
        s2_era.insert("track_id".into(), track);
        let s2_era = serde_json::Value::Object(s2_era);
        assert!(
            serde_json::from_value::<PlannerHarnessStartOperationPayload>(s2_era.clone()).is_ok(),
            "the `track_id` alias must keep S2-era rows readable: {s2_era}"
        );
    }
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::db::prelude::{ServerRepoOutOfDomainExt, ServerRepoSyncDomainRawExt};
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::forge_trust::trusted_forge_plugin;
    use crate::model::{NewArea, NewPlugin, NewTrack};
    use crate::operation::Phase;
    use crate::plugin_host::{Manifest, PluginRegistry, PluginRuntimeStatus};
    use crate::routes::theme::RequestTheme;
    use tokio::time::{Instant, sleep};

    const TEMPLATE_ID: &str = "issue-development";

    fn populated_template_descriptor() -> TemplateDescriptor {
        TemplateDescriptor {
            id: "issue-development".into(),
        }
    }

    #[test]
    fn planner_developer_instructions_do_not_inject_template_plan_prose() {
        let template = populated_template_descriptor();
        let out = render_planner_developer_instructions("track-abc", Some(&template), None);

        assert!(
            !out.contains("## Bound Template Instructions"),
            "S3 must not inject planner_instructions"
        );
        assert!(
            !out.contains("## Bound Template Plan Template"),
            "S3 must not inject plan_template"
        );
        assert!(
            !out.contains("## Bound Template Gates"),
            "S3 must not inject gates"
        );
        assert!(
            !out.contains("Follow template instructions for track"),
            "descriptor prose must not leak into the planner prompt"
        );
        assert!(!out.contains(r#""key": "review-a""#));
        assert!(!out.contains(r#""cmd": "cargo fmt --all --check""#));
        assert!(!out.contains("## Bound Template Input"));
        assert_eq!(
            out,
            crate::planner_card::render_system_prompt(
                crate::planner_card::SeededCardRole::Planner.prompt_template(),
                "track-abc",
            )
        );
    }

    #[test]
    fn planner_developer_instructions_without_template_match_static_template() {
        let expected = crate::planner_card::render_system_prompt(
            crate::planner_card::SeededCardRole::Planner.prompt_template(),
            "track-abc",
        );

        let out = render_planner_developer_instructions("track-abc", None, None);

        assert_eq!(out, expected);
        assert!(
            expected.contains(
                "Before you write anything to the report in a session, call `calm.report.read` once"
            ),
            "base prompt must mandate an unconditional first read (#1185 §1.5 A) — the \
             document's own maintenance contract is only reachable by reading it"
        );
        assert!(expected.contains("authoritative pre-set plan"));
        assert!(expected.contains("Do not mint duplicate tasks"));
    }

    #[test]
    fn planner_developer_instructions_append_template_input_when_present() {
        let template = populated_template_descriptor();
        let input = json!({
            "issue_url": "https://github.com/o/r/issues/1",
            "repo": "o/r",
            "issue_number": 1,
            "notes": "literal {track_id} must survive"
        });

        let out = render_planner_developer_instructions("track-abc", Some(&template), Some(&input));

        assert!(
            !out.contains("## Bound Template Instructions")
                && !out.contains("## Bound Template Plan Template")
                && !out.contains("## Bound Template Gates"),
            "input injection must not revive the retired plan-prose sections"
        );
        let input_at = out
            .find("## Bound Template Input")
            .expect("template input section");
        let rendered_input = out[input_at..]
            .strip_prefix("## Bound Template Input\n```json\n")
            .and_then(|section| section.strip_suffix("\n```"))
            .expect("template input section must be a trailing JSON fence");
        let rendered_input: Value =
            serde_json::from_str(rendered_input).expect("template input must be valid JSON");
        assert_eq!(
            rendered_input, input,
            "template input must round-trip in full"
        );
        // User JSON is injected verbatim — no `{track_id}` template substitution.
        assert!(out.contains("literal {track_id} must survive"));

        // input = None renders no section at all.
        let without = render_planner_developer_instructions("track-abc", Some(&template), None);
        assert!(!without.contains("## Bound Template Input"));
    }

    /// #1189 review F3 — the boot-recovery selector's SQL literals have no
    /// compile-time link to the Rust values they mirror.
    ///
    /// `session_projection_recover_harnesses_on_boot` (calm-truth) hardcodes
    /// two `(role, harness_profile)` pairs: `('assistant', 'assistant')` and
    /// `('worker', 'plain_chat')`. The truth for both lives in
    /// `minted_card_shape` here, in calm-server, which calm-truth cannot depend
    /// on. Renaming `ASSISTANT_HARNESS_PROFILE_MARKER`, the `"plain_chat"`
    /// literal, or a `CardRole` db string therefore compiles cleanly and fails
    /// by making the selector silently skip that conversation class on boot —
    /// which is exactly the bug #1189 A1 just fixed.
    ///
    /// So this seeds one recoverable runtime per profile using the shape
    /// `minted_card_shape` returns and requires the selector to return both. A
    /// rename on either side is red here.
    #[tokio::test]
    async fn boot_recovery_sql_literals_track_the_minted_card_shape() {
        use crate::session_projection_repo::WorkerSessionProjectionRepo;

        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite repo"),
        );
        let track = make_track(repo.as_ref(), None, None, None).await;

        let mut base = HarnessSnapshot::initial(0, vec![]);
        base.phase = HarnessPhaseTag::Idle;

        let mut expected = Vec::new();
        let mut tx = repo.pool().begin().await.expect("begin tx");
        for profile in [HarnessProfile::Assistant, HarnessProfile::PlainChat] {
            let (minted_role, minted_marker) =
                minted_card_shape(profile).expect("conversation profile mints a card");
            let card = card_create_with_id_tx(
                &mut tx,
                new_id(),
                NewCard {
                    track_id: track.id.clone(),
                    title: None,
                    kind: "codex".into(),
                    sort: None,
                    payload: json!({"schemaVersion": 1, "harness_profile": minted_marker}),
                },
                minted_role,
                false,
                repo.card_role_cache(),
            )
            .await
            .expect("mint conversation card");

            let worker_session_id = new_id();
            let thread_id = format!("thread-{}", card.id.as_str());
            let mut snapshot = base.clone();
            snapshot.last_thread_id = Some(thread_id.clone());
            session_start_runtime_tx(
                &mut tx,
                WorkerSessionInit {
                    id: worker_session_id.clone(),
                    card_id: card.id.to_string(),
                    kind: session_kind_for(profile),
                    agent_provider: Some(AgentProvider::Codex),
                    // The state the restart bug bites in: a turn was in flight.
                    status: WorkerSessionState::TurnPending,
                    terminal_run_id: None,
                    thread_id: Some(thread_id),
                    session_id: None,
                    active_turn_id: None,
                    handle_state_json: Some(
                        serde_json::to_value(&snapshot).expect("serialize snapshot"),
                    ),
                    spawn_op_id: None,
                    now_ms: now_ms(),
                },
            )
            .await
            .expect("start runtime");
            expected.push(worker_session_id);
        }
        tx.commit().await.expect("commit seed tx");

        let mut selected = repo
            .session_projection_recover_harnesses_on_boot()
            .await
            .expect("boot selector")
            .into_iter()
            .map(|runtime| runtime.id)
            .collect::<Vec<_>>();
        selected.sort();
        expected.sort();
        assert_eq!(
            selected, expected,
            "the boot selector no longer recognises a card minted by \
             `minted_card_shape`: its SQL literals ('assistant'/'assistant' and \
             'worker'/'plain_chat') have drifted from the Rust values, so a \
             kernel restart would drop that conversation class"
        );
    }

    #[tokio::test]
    async fn bound_template_descriptor_filters_running_trusted_template_binding() {
        let trusted_plugin_id = configured_trusted_plugin_id();
        let untrusted_plugin_id = untrusted_plugin_id(&trusted_plugin_id);
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite repo"),
        );
        let bound_input = json!({ "issue_url": "https://github.com/o/r/issues/1" });
        let bound_track = make_track(
            repo.as_ref(),
            Some(TEMPLATE_ID),
            Some(bound_input.clone()),
            Some(trusted_plugin_id.as_str()),
        )
        .await;
        let unbound_track = make_track(repo.as_ref(), None, None, None).await;
        // #1321 S1 — a track owned by the *untrusted* plugin. Pins the
        // trust half of the filter on the owner column itself, which the
        // old "untrusted plugin declares the id" arm can no longer reach
        // now that declaration is not how an owner is found.
        let untrusted_owned_track = make_track(
            repo.as_ref(),
            Some(TEMPLATE_ID),
            None,
            Some(untrusted_plugin_id.as_str()),
        )
        .await;

        let (trusted_running_host, trusted_running_tmp) =
            plugin_host_with_template(repo.clone(), &trusted_plugin_id, true).await;
        trusted_running_host
            .spawn(&trusted_plugin_id)
            .await
            .expect("spawn trusted plugin");
        wait_for_running(&trusted_running_host, &trusted_plugin_id).await;
        let trusted_running_adapter = adapter_for(repo.clone(), trusted_running_host.clone());
        let bound = trusted_running_adapter
            .bound_template(bound_track.id.as_str())
            .await
            .expect("resolve trusted running descriptor")
            .expect("bound template");
        assert_eq!(bound.descriptor.id, TEMPLATE_ID);
        // The track row's persisted template_input rides along with the descriptor.
        assert_eq!(bound.input.as_ref(), Some(&bound_input));

        let (trusted_stopped_host, _trusted_stopped_tmp) =
            plugin_host_with_template(repo.clone(), &trusted_plugin_id, false).await;
        let trusted_stopped_adapter = adapter_for(repo.clone(), trusted_stopped_host);
        assert!(
            trusted_stopped_adapter
                .bound_template(bound_track.id.as_str())
                .await
                .expect("trusted stopped lookup")
                .is_none()
        );

        let (untrusted_running_host, untrusted_running_tmp) =
            plugin_host_with_template(repo.clone(), &untrusted_plugin_id, true).await;
        untrusted_running_host
            .spawn(&untrusted_plugin_id)
            .await
            .expect("spawn untrusted plugin");
        wait_for_running(&untrusted_running_host, &untrusted_plugin_id).await;
        let untrusted_running_adapter = adapter_for(repo.clone(), untrusted_running_host.clone());
        assert!(
            untrusted_running_adapter
                .bound_template(bound_track.id.as_str())
                .await
                .expect("untrusted running lookup")
                .is_none(),
            "the track's owner is the trusted plugin, which is not running here"
        );
        assert!(
            untrusted_running_adapter
                .bound_template(untrusted_owned_track.id.as_str())
                .await
                .expect("untrusted owner lookup")
                .is_none(),
            "an untrusted owner must not bind even while it is running and \
             declaring the template id"
        );

        assert!(
            trusted_running_adapter
                .bound_template(unbound_track.id.as_str())
                .await
                .expect("unbound lookup")
                .is_none()
        );

        untrusted_running_host
            .stop(&untrusted_plugin_id)
            .await
            .expect("stop untrusted plugin");
        trusted_running_host
            .stop(&trusted_plugin_id)
            .await
            .expect("stop trusted plugin");
        drop(untrusted_running_tmp);
        drop(trusted_running_tmp);
    }

    fn configured_trusted_plugin_id() -> String {
        std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
            .ok()
            .and_then(|configured| {
                configured
                    .split(',')
                    .map(str::trim)
                    .find(|id| !id.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "dev.neige.git-forge".to_string())
    }

    fn untrusted_plugin_id(trusted_plugin_id: &str) -> String {
        let mut candidate = "dev.neige.untrusted-template-test".to_string();
        let mut suffix = 0;
        while candidate == trusted_plugin_id || trusted_forge_plugin(&candidate) {
            suffix += 1;
            candidate = format!("dev.neige.untrusted-template-test-{suffix}");
        }
        candidate
    }

    /// #1321 S1 — `plugin_scope` is now a parameter, and every caller has to
    /// state it. It used to be hard-coded `None`, which combined with a
    /// `Some` `template_id` + `template_input` into a row `POST /api/tracks`
    /// cannot produce: create copies the bound plugin into `plugin_scope`,
    /// and refuses `template_input` without a bound plugin. That fixture is
    /// what let the two owner readers drift apart unnoticed —
    /// `crate::track_binding::tests` now drives the same states through the
    /// real create route.
    async fn make_track(
        repo: &SqlxRepo,
        template_id: Option<&str>,
        template_input: Option<serde_json::Value>,
        plugin_scope: Option<&str>,
    ) -> crate::model::Track {
        let area = repo
            .area_create(NewArea {
                name: format!("area-{template_id:?}"),
                color: "#101010".into(),
                sort: None,
            })
            .await
            .expect("create area");
        repo.track_create(NewTrack {
            template_input,
            area_id: area.id,
            title: "template resolver".into(),
            sort: None,
            cwd: String::new(),
            template_id: template_id.map(str::to_string),
            plugin_scope: plugin_scope.map(str::to_string),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track")
    }

    async fn plugin_host_with_template(
        repo: Arc<SqlxRepo>,
        plugin_id: &str,
        seed_plugin_row: bool,
    ) -> (Arc<PluginHost>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugins_dir = tmp.path().join("plugins");
        let plugins_data_dir = tmp.path().join("plugins-data");
        let install_dir = plugins_dir.join(plugin_id);
        let bin_dir = install_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::fs::create_dir_all(&plugins_data_dir).expect("create plugins data dir");
        std::os::unix::fs::symlink(stub_echo_bin(), bin_dir.join("stub"))
            .expect("symlink echo stub");

        let manifest_json = json!({
            "manifest_version": 2,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Template Resolver Stub",
            "entrypoint": { "command": "bin/stub" },
            // #1321 S1 — a plugin that accepts no `template_input` cannot be
            // the owner of a track that carries one: `POST /api/tracks`
            // refuses that create (`validate_template_input_binding`), and
            // the per-track resolver now fails closed on the same
            // combination. Declaring the schema keeps this fixture inside
            // the set of states production can actually reach.
            "input_schema": {
                "type": "object",
                "properties": { "issue_url": { "type": "string" } },
                "required": ["issue_url"],
                "additionalProperties": false
            },
            "templates": [
                { "id": TEMPLATE_ID }
            ],
            "permissions": {}
        });
        let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
        let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
        if seed_plugin_row {
            repo.plugin_install(NewPlugin {
                id: plugin_id.to_string(),
                version: "0.1.0".into(),
                install_path: install_dir.display().to_string(),
                manifest: manifest_json,
                enabled: true,
                user_config: json!({}),
            })
            .await
            .expect("seed plugin row");
        }
        let repo_dyn: Arc<dyn Repo> = repo;
        let host = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo_dyn,
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        ));
        (host, tmp)
    }

    fn adapter_for(repo: Arc<SqlxRepo>, plugin: Arc<PluginHost>) -> PlannerHarnessStartAdapter {
        let repo_dyn: Arc<dyn Repo> = repo;
        PlannerHarnessStartAdapter::new(
            repo_dyn.clone(),
            SharedCodexAppServer::new_stub(repo_dyn),
            HarnessRegistry::new(),
            plugin,
            CardRoleCache::new(),
            TrackAreaCache::new(),
            None,
        )
    }

    async fn wait_for_running(host: &Arc<PluginHost>, plugin_id: &str) {
        let start = Instant::now();
        loop {
            if let Some(status) = host.status(plugin_id).await
                && matches!(status.status, PluginRuntimeStatus::Running)
            {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timed out waiting for plugin {plugin_id} to run"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn stub_echo_bin() -> PathBuf {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        if let Some(path) = option_env!("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        let current = std::env::current_exe().expect("current test executable");
        let deps_dir = current.parent().expect("test executable parent");
        let debug_dir = deps_dir.parent().expect("target debug dir");
        let candidate = debug_dir.join("plugin-host-stub-echo");
        assert!(
            candidate.exists(),
            "missing plugin-host-stub-echo at {}",
            candidate.display()
        );
        candidate
    }

    /// #1098 INV-CHAT-013(a), zero-step arm.
    ///
    /// `plan_compensation` returns EARLY with no steps when a thread reuse hit
    /// the missing-per-card-MCP-token fence. That early return predates lazy
    /// card minting; without an unconditional `delete_card` prepended before
    /// it, this exact failure would leave a card with no session behind.
    ///
    /// The arm is unreachable from the conversations route (it mints with
    /// `force_new_thread`, so nothing is ever reused), which is precisely why
    /// it is pinned here rather than only through an end-to-end failure.
    #[tokio::test]
    async fn missing_mcp_token_early_return_still_deletes_a_lazily_minted_card() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite repo"),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            PathBuf::new(),
            std::env::temp_dir().join(format!("calm-plan-compensation-{}", new_id())),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        ));
        let adapter = adapter_for(repo.clone(), host);
        let reason = format!(
            "planner card conv-1 reuses thread t-1 with {REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR} (re-run to mint a fresh thread)"
        );
        assert!(
            is_reusable_thread_missing_card_mcp_token_failure(&reason),
            "test reason must hit the zero-step arm"
        );

        let mut output = TxOutput::new("card", Some("conv-1".into()), json!({}));
        output.data = json!({
            "card_id": "conv-1",
            "track_id": "track-1",
            "runtime_id": "runtime-1",
        });

        let with_seed = plan_compensation_for(&adapter, &output, &reason, true).await;
        assert_eq!(
            with_seed
                .steps
                .iter()
                .map(|step| step.op.as_str())
                .collect::<Vec<_>>(),
            vec!["delete_card"],
            "a lazily minted card must be deleted even on the zero-step early return"
        );
        assert_eq!(with_seed.steps[0].args["card_id"], json!("conv-1"));
        assert_eq!(with_seed.steps[0].args["track_id"], json!("track-1"));

        // Counterexample: without `create_card` the historical zero-step
        // contract is preserved verbatim.
        let without_seed = plan_compensation_for(&adapter, &output, &reason, false).await;
        assert!(
            without_seed.steps.is_empty(),
            "operations that did not mint a card must keep the zero-step early return: {:?}",
            without_seed.steps
        );
    }

    /// #1098 — `delete_card` is unconditional AND ordered after
    /// `abort_harness_task`.
    ///
    /// On the spawn arms the harness run loop is still alive; removing the
    /// card (and, by cascade, its session and `harness_items`) out from under
    /// it is the wrong order. The exact sequence is pinned, not merely
    /// "contains delete_card": ordering is the whole point of this test.
    #[tokio::test]
    async fn lazily_minted_card_is_deleted_after_the_harness_task_is_aborted() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite repo"),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo_dyn,
            PathBuf::new(),
            std::env::temp_dir().join(format!("calm-compensation-order-{}", new_id())),
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        ));
        let adapter = adapter_for(repo.clone(), host);

        let mut output = TxOutput::new("card", Some("conv-1".into()), json!({}));
        output.data = json!({
            "card_id": "conv-1",
            "track_id": "track-1",
            "runtime_id": "runtime-1",
        });

        let planned = plan_compensation_from(
            &adapter,
            PhaseTag::SpawnStarted,
            &output,
            "spawn blew up",
            true,
        )
        .await;
        assert_eq!(
            planned
                .steps
                .iter()
                .map(|step| step.op.as_str())
                .collect::<Vec<_>>(),
            vec![
                "abort_harness_task",
                "interrupt_thread",
                "fail_runtime",
                "delete_card"
            ],
            "the minted card must be removed only after the run loop is stopped"
        );

        // Counterexample: the same phase without a minted card keeps the
        // historical step list untouched.
        let without_seed = plan_compensation_from(
            &adapter,
            PhaseTag::SpawnStarted,
            &output,
            "spawn blew up",
            false,
        )
        .await;
        assert_eq!(
            without_seed
                .steps
                .iter()
                .map(|step| step.op.as_str())
                .collect::<Vec<_>>(),
            vec!["abort_harness_task", "interrupt_thread", "fail_runtime"],
        );
    }

    async fn plan_compensation_for(
        adapter: &PlannerHarnessStartAdapter,
        output: &TxOutput,
        reason: &str,
        create_card: bool,
    ) -> CompensationStateVersioned {
        plan_compensation_from(
            adapter,
            PhaseTag::AppServerInteract,
            output,
            reason,
            create_card,
        )
        .await
    }

    async fn plan_compensation_from(
        adapter: &PlannerHarnessStartAdapter,
        from_phase: PhaseTag,
        output: &TxOutput,
        reason: &str,
        create_card: bool,
    ) -> CompensationStateVersioned {
        let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
            actor: ActorId::User,
            track_id: "track-1".into(),
            planner_card_id: CardId::from("conv-1"),
            report_card_id: None,
            sort: None,
            cwd: "/tmp".into(),
            goal: None,
            reset_harness_items: false,
            force_new_thread: false,
            profile: HarnessProfile::PlainChat,
            create_card: create_card.then(LazyMintCardSeed::default),
            opening_briefing: None,
            first_message: None,
            create_request_sha256: None,
        })
        .expect("payload serializes");
        adapter
            .plan_compensation(from_phase, reason, output, &operation_with_payload(payload))
            .await
            .expect("plan compensation")
    }

    fn operation_with_payload(payload: Value) -> Operation {
        Operation {
            id: "op-1".into(),
            operation_key: "op-key".into(),
            kind: "planner-harness-start".into(),
            idempotency_key: None,
            payload_hash: String::new(),
            target_type: "card".into(),
            target_id: Some("conv-1".into()),
            target: json!({}),
            payload,
            tx_output: None,
            phase: Phase::AppServerInteract {
                kind: AppServerInteractKind::MintAndAwait { thread_id: None },
            },
            phase_detail: None,
            attempt: 1,
            last_error: None,
            compensation_state: None,
            lease_owner: None,
            lease_until_ms: None,
            spawn_artifacts: None,
            parked_at_ms: None,
            parked_deadline_ms: None,
        }
    }
}
