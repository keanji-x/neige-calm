use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::activity_window::launchpad_opening_briefing;
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
    PlannerHarness, PlannerHarnessParams, QueueEntry, QueueEntryId, initial_snapshot_with_goal,
    is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, TrackId};
use crate::mcp_server::wiring::{
    mint_card_mcp_token_pair, mirror_session_mcp_token, persist_card_mcp_token_hash,
};
use crate::model::{Card, CardPatch, CardRole, NewCard, new_id, now_ms};
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

    /// Keeps card_mcp_token rotation atomic with the thread/start RPC that ships the matching raw token, should drive ever become parallel.
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

    /// Resolve through the track's recorded owner (`tracks.plugin_scope`), never by scanning the running roster for `template_id`, so this reader and the MCP tool scope share one owner answer.
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
            // The track names no template, or is unbound: an ordinary vanilla prompt, not a degradation.
            TrackOwnerBinding::Owned {
                contract: TemplateContract::NotTemplated,
                ..
            }
            | TrackOwnerBinding::Unbound => Ok(None),
            // The owner is alive and keeps its tool scope; only the template contract is unusable, so the descriptor and `template_input` are dropped.
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

/// The descriptor from the running trusted plugin plus the track row's persisted `template_input` (schema-validated at create time).
pub(crate) struct BoundTemplate {
    pub(crate) descriptor: TemplateDescriptor,
    pub(crate) input: Option<serde_json::Value>,
}

pub use crate::harness::profile::HarnessProfile;

/// Whether a conversation create prepends the activity briefing. A ruling per caller, not a property of the track: `POST /api/today/summary`
/// carries the day's counts itself and must not be briefed twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpeningBriefing {
    /// On the launchpad track the conversation opens with today's activity window; elsewhere with nothing.
    TodaysActivityOnTheLaunchpad,
    /// The caller supplies its own material and must not be given a second copy.
    CallerSuppliesItsOwn,
}

/// **The serialized field names here are FROZEN** (`wave_id`, `spec_card_id`): this struct is persisted verbatim in `operations.payload_json` and hashed into
/// `operations.payload_hash`; renaming a field 409s every stable idempotency key forever. `track_id` keeps a `serde(alias)` for rows written under the other spelling.
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
    /// `Some` asks this operation to mint `planner_card_id` itself, in the same tx as the session row, so a failed `thread/start` can take the card back out; `None` means the card must already exist.
    #[serde(default)]
    pub create_card: Option<LazyMintCardSeed>,
    /// The RULING travels here, never the briefing TEXT: the day's counts change, and `stable_payload_hash` covers the whole payload.
    /// `skip_serializing_if` keeps producers that do not set it byte-identical to older payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_briefing: Option<OpeningBriefing>,
    /// The track's first user message, enqueued by `prepare_tx` as `Observation::UserMessage` (never `TrackGoal`) inside this operation's transaction.
    /// The text travels (bounded by `validate_first_message`); `skip_serializing_if` keeps message-less payload hashes unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message: Option<String>,
    /// SHA-256 of the track create request's mint inputs; never read by this adapter, kept aligned with the durable binding-level fingerprint.
    /// `skip_serializing_if` so callers that do not set it keep byte-identical payload hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_request_sha256: Option<String>,
}

/// The user-supplied part of a lazily minted conversation card; everything the kernel owns (kind, role, `deletable`, the profile marker) is pinned by the adapter.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LazyMintCardSeed {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    /// The caller's `Idempotency-Key`, so `validate` can recompute the deterministic card id and refuse anything it did not derive. Lives on the seed so the area route keeps byte-identical payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Omitted fields keep old operation payload hashes byte-for-byte stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// `Assistant` is `CodexCard`, NOT `SharedPlanner`: a `SharedPlanner` session becomes the track's `root_session_id` and would displace the planner card as planning authority.
fn session_kind_for(profile: HarnessProfile) -> WorkerSessionKind {
    match profile {
        HarnessProfile::Planner => WorkerSessionKind::SharedPlanner,
        HarnessProfile::PlainChat | HarnessProfile::Assistant => WorkerSessionKind::CodexCard,
    }
}

/// One function returning both halves because they must never disagree: the role decides what the token may call, the marker which list the card appears in.
/// `Planner` is rejected here as the fail-closed twin of `validate`.
fn minted_card_shape(profile: HarnessProfile) -> Result<(CardRole, &'static str)> {
    match profile {
        HarnessProfile::PlainChat => {
            Ok((CardRole::Worker, crate::harness::profile::PLAIN_CHAT_MARKER))
        }
        HarnessProfile::Assistant => Ok((CardRole::Assistant, ASSISTANT_HARNESS_PROFILE_MARKER)),
        HarnessProfile::Planner => Err(CalmError::BadRequest(
            "the planner profile does not mint its own card".into(),
        )),
    }
}

/// Read by `plain_chat::card_is_track_assistant` and the track conversation list predicate; those three places are the whole contract.
pub(crate) const ASSISTANT_HARNESS_PROFILE_MARKER: &str = crate::harness::profile::ASSISTANT_MARKER;

pub(crate) fn render_planner_developer_instructions(
    track_id: &str,
    template_descriptor: Option<&TemplateDescriptor>,
    template_input: Option<&serde_json::Value>,
) -> String {
    let mut instructions = crate::planner_card::render_system_prompt(
        crate::planner_card::SeededCardRole::Planner.prompt_template(),
        track_id,
    );
    // The plugin descriptor is an id handle, not the working method. The
    // creation-time method is appended from the Planner card's snapshot at
    // thread/start. Plugin input still requires a currently resolved binding.
    if template_descriptor.is_none() {
        return instructions;
    }
    // Deliberately NOT passed through `render_system_prompt`: user-controlled JSON must not have literal `{track_id}` substituted.
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
        // The lazy-mint branch: `validate` runs BEFORE the operation row exists, so the card is this operation's own output. These checks must stay strictly narrower than the ordinary branch.
        if let Some(seed) = payload.create_card.as_ref() {
            if (seed.model.is_some() || seed.reasoning_effort.is_some())
                && payload.actor != ActorId::User
            {
                return Err(CalmError::Forbidden(
                    "Only the user may choose a conversation model".into(),
                ));
            }

            // `Planner` never mints here (it would hand a caller the track's lifecycle authority). The assistant branch recomputes the deterministic card id
            // from `(track_id, idempotency_key)` and refuses any id it did not derive, so it needs no track-shape allowlist.
            match payload.profile {
                HarnessProfile::Planner => {
                    return Err(CalmError::BadRequest(format!(
                        "card {} can only be minted by this operation under the plain-chat or assistant profile",
                        payload.planner_card_id
                    )));
                }
                HarnessProfile::PlainChat => {
                    // Not migrated to the derived-id guard: recomputing would bind the key into the area route's payload and change a live payload hash for no gain.
                    if track.purpose.as_deref() != Some(crate::AREA_CHAT_PURPOSE) {
                        return Err(CalmError::Forbidden(format!(
                            "track {} is not an area chat track; chat cards are only minted there",
                            track.id
                        )));
                    }
                }
                HarnessProfile::Assistant => {
                    // Fail closed on a missing key: "no key" must not read as "no check".
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
            // A genuine retry is deduplicated earlier by the operation idempotency key; an existing row here would mean adopting somebody else's card.
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
            // Re-start paths reach here with an existing assistant card; the marker + role pair is what makes the profile legitimate.
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
            // Message carries the live failure and the background-retry fact; preflights stay non-blocking.
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
        // The briefing is rendered HERE, inside the mint transaction, and MUST stay ABOVE this transaction's first write: its reads run on a pool
        // connection, and once `tx` holds RESERVED on `events` the shared-lock read deadlocks against it (pinned by `briefing_ordering_survives_contention_in_the_mint_transaction`).
        let opening_briefing = match payload.opening_briefing {
            Some(OpeningBriefing::TodaysActivityOnTheLaunchpad) => {
                launchpad_opening_briefing(self.repo.as_ref(), track_id.as_str()).await?
            }
            Some(OpeningBriefing::CallerSuppliesItsOwn) | None => None,
        };
        if let Some(briefing) = opening_briefing.as_deref() {
            // `Internal`, not `BadRequest`: nothing the caller sent can make this fire.
            if briefing.chars().count() > MAX_PLANNER_INPUT_CHARS {
                return Err(CalmError::Internal(format!(
                    "opening briefing must be at most {MAX_PLANNER_INPUT_CHARS} characters",
                )));
            }
        }
        // Mint the chat card in this very transaction, so the card and its session row commit together and compensation can undo both.
        let mut post_commit_events = Vec::new();
        if let Some(seed) = payload.create_card.as_ref() {
            let (minted_role, minted_marker) = minted_card_shape(payload.profile)?;
            let scope = card_scope(
                self.repo.as_ref(),
                card_id.clone(),
                TrackId::from(track_id.clone()),
            )
            .await?;
            let mut card_payload = serde_json::Map::from_iter([
                ("schemaVersion".to_string(), json!(1)),
                ("harness_profile".to_string(), json!(minted_marker)),
            ]);
            if seed.model.is_some() || seed.reasoning_effort.is_some() {
                crate::planner_model::CardModelSelection::apply_to_payload(
                    &mut card_payload,
                    seed.model.as_deref(),
                    seed.reasoning_effort.as_deref(),
                );
            }
            let created = card_create_with_id_tx(
                tx,
                card_id.to_string(),
                NewCard {
                    track_id: TrackId::from(track_id.clone()),
                    kind: "codex".into(),
                    sort: seed.sort,
                    // Pinned by the kernel: no `planner_harness` key, unchanged `schemaVersion`; the marker and the role both track the profile and are the two columns every assistant reader consults.
                    payload: Value::Object(card_payload),
                    title: seed.title.clone(),
                },
                minted_role,
                // Kernel-owned until single-conversation delete exists, so `DELETE /api/cards/:id` cannot orphan a live harness.
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
        // What the INHERIT took, for the undo journal: the predecessor is still `active` when the harvest runs, so the harvest's journal cannot cover it.
        let mut inherited_from: Vec<HarvestedFrom> = Vec::new();
        if let Some(inherited) = inherited_snapshot {
            snapshot.push_watermark = inherited.push_watermark;
            // Inheriting the fused entries (not the raw arrays) is what keeps the queue ids the client has already been shown; the inherit CARRIES the predecessor's message ids, it does not mint over them.
            let mut inherited_entries = inherited.pending_entries();
            inherited_queue_moved = true;
            if let Some(existing) = existing_active_runtime.as_ref() {
                // Only the human sentences are journalled, because only they are returned. `is_user_authored` is the SAME predicate `ensure_message_id` mints under.
                let mut messages: Vec<HarvestedMessage> = Vec::new();
                for entry in inherited_entries.iter_mut() {
                    if !entry.is_user_authored() {
                        continue;
                    }
                    let ids = entry.ensure_message_id().to_vec();
                    let entry_id = entry.id().map(|id| id.as_str().to_string());
                    let Observation::UserMessage { text } = entry.observation() else {
                        continue;
                    };
                    messages.push(HarvestedMessage {
                        text,
                        ids,
                        entry_id,
                    });
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
        // One clock for the whole mint, so the predecessor's `queue_harvested_at_ms` can never be newer than the successor that took its queue.
        let now = now_ms();
        // The NON-deferred arm supersedes its predecessor ahead of the harvest so the retired row is harvestable in this same transaction;
        // the deferred arm inherits the whole queue above and its supersede lives inside `session_prepare_deferred_planner_tx`.
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
        // Sentences a human typed, stranded on a runtime that left the active set before its queue drained. Read, carried and stamped in THIS transaction so a second restart takes nothing.
        let worker_session_id = new_id();
        let harvested = harvest_pending_user_messages_tx(
            tx,
            card.id.as_str(),
            worker_session_id.as_str(),
            now,
            stranded_user_messages,
        )
        .await?;
        // The first user message ships INSIDE this transaction, after the inherited queue, as `UserMessage` (hard-fires, cannot be evicted), never `TrackGoal`.
        // The briefing goes FIRST as `SystemContext` (no audit row). KNOWN: both sit on an unstarted runtime's queue, so a mint superseded before draining strands them.
        let mut seeded = false;
        let mut entries = snapshot.pending_entries();
        if let Some(briefing) = opening_briefing {
            entries.push(QueueEntry::system(
                Observation::SystemContext { text: briefing },
                None,
            )?);
            seeded = true;
        }
        // Harvested sentences go after the successor's own briefing and goal and before this mint's `first_message`, oldest first.
        if !harvested.messages.is_empty() {
            // A sentence moving between runtimes leaves a trace; a silent transfer cannot be told from a loss afterwards.
            tracing::info!(
                card_id = %card_id,
                worker_session_id = %worker_session_id,
                moved = harvested.messages.len(),
                from_rows = harvested.stamped_worker_session_ids.len(),
                "planner harness: harvested undelivered user messages into a new runtime"
            );
        }
        for message in harvested.messages {
            entries.push(QueueEntry::user_message_moved(
                message.text,
                message.ids,
                message.entry_id.map(QueueEntryId::from_wire),
            ));
            seeded = true;
        }
        if let Some(text) = payload.first_message.as_deref() {
            if text.trim().is_empty() {
                return Err(CalmError::BadRequest(
                    "first_message must not be empty".into(),
                ));
            }
            // Minted through the same constructor as every other user message, so it gets a stable queue id. No attachments: nothing on this path has ever seen an upload.
            entries.push(QueueEntry::user_message(text.to_string(), None, Vec::new()));
            seeded = true;
        }
        // `set_pending_entries` rebuilds all four stored arrays together, so write only when something was pushed.
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
            // The inherit is a MOVE: the predecessor stops holding its queue in this transaction, or a later harvest or re-driven operation delivers it twice.
            // Written before `session_prepare_deferred_planner_tx` retires the row, while the ordinary writer still accepts it.
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

        // The audit row for the seeded first message, in the SAME transaction as the queue that carries it. The actor is `payload.actor` verbatim; the role gate below refuses anything it should not.
        if let Some(text) = payload.first_message.as_deref() {
            let char_count = text.chars().count() as u32;
            // `card_scope_tx`, NOT the pool-reading `card_scope`: the session writes above touched `tracks`, so a pool read would wait on a lock only this transaction can release.
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
            // The undo journal: which sentences came off which row, from BOTH transfers this transaction can make, so a compensation (a different transaction) can give them back.
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
        // Not a security boundary: production reads this role nowhere (the tool surface is resolved per MCP request from the card's persisted `role` column).
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
        // OLD PTY shutdown at Phase-2 entry: force_new_thread is a hard reset; the handle kill is the first app-server-side action after commit.
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
        // Reuse requires the existing thread to have been minted under the per-card token contract (the card owns a `card_mcp_tokens` row).
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
                // A plain chat is a bare codex thread: no kernel prompt, no MCP tools to describe.
                HarnessProfile::PlainChat => None,
                // The assistant gets its own prompt, not the planner one. On Today's launchpad it gets a different identity (keeping the day's report current is the job);
                // the criterion is `routes::today::is_launchpad_track`, the same call the activity briefing makes.
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
                    let mut instructions = render_planner_developer_instructions(
                        &track_id,
                        bound_template.as_ref().map(|bound| &bound.descriptor),
                        bound_template
                            .as_ref()
                            .and_then(|bound| bound.input.as_ref()),
                    );
                    let card = self
                        .repo
                        .card_get(&card_id)
                        .await?
                        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
                    if let Some(context) =
                        crate::template_context::TemplateContext::from_card_payload(&card.payload)?
                    {
                        context.append_to(&mut instructions)?;
                    }
                    Some(instructions)
                }
            };
            let (raw, hashed) = mint_card_mcp_token_pair();
            new_mcp_token_hash = Some(hashed);
            let socket_path = self.mcp_socket_path_for_thread()?;
            // The channel-3 `thread/start` config comes from the single shared producer so every spawn path emits the byte-identical shape.
            let params = SharedThreadStartParams {
                cwd,
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions,
                // The assistant needs the same channel-3 MCP credentials as the planner harness; WHICH tools the token can reach is resolved from the card's persisted role, not here.
                config: match profile.mcp_role() {
                    Some(role) => ThreadConfig::McpShell {
                        role,
                        socket_path: PathBuf::from(&socket_path),
                        raw_token: raw,
                    },
                    None => ThreadConfig::NoMcp,
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

        // Nothing in this phase reads the `output.result` card snapshot: `operations.tx_output_json` is persisted replay input, so a `Card` serde change would fail every in-flight replay here.
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
                    // What this transaction took off which row; it has to leave the closure because the journal lives in `output`.
                    let mut taken_from: Vec<HarvestedFrom> = Vec::new();
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        persist_card_mcp_token_hash(tx, &card_id, hashed).await?;
                    }
                    if runtime_deferred {
                        let now = now_ms();
                        let occupant = session_projection_active_for_card_tx(tx, &card_id).await?;
                        // A runtime that is NOT this operation's own deferred placeholder raced into the card's active slot while the thread was being minted.
                        let raced_in = match occupant.as_ref() {
                            Some(existing) if existing.id != worker_session_id => {
                                Some(existing.clone())
                            }
                            _ => None,
                        };
                        // The queue comes from THIS RUNTIME'S ROW, not from the snapshot `output` froze at mint time: a re-driven operation would otherwise write
                        // that frozen queue back over a transfer another mint made in between. `output` still carries only this operation's own decisions.
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
                            // Supersede FIRST, then harvest: the harvest predicate is `state = 'superseded'` and this row is the one being retired.
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
                            // The journal is written INSIDE this transaction, onto the checkpoint; a post-commit merge would leave a crash window with stamped rows and no record.
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
                                        message.entry_id.map(QueueEntryId::from_wire),
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
                            // This operation's own placeholder: superseded and re-inserted under the same id.
                            (Some(existing), None) => {
                                session_supersede_and_start_tx(tx, &existing.id, runtime_init)
                                    .await?;
                            }
                            // The racer is already superseded above, so the pair `supersede + start` is complete here.
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
                        // Same rule on this arm: the queue comes from the row.
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
                    // The delete is a hard delete across the card's whole history, so measure the transcript inside the tx and strictly before it.
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
        // Merge this transaction's undo journal into the one `prepare_tx` wrote, or a compensation puts back only half of what it took.
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
                        // Always `Some(..)`; the `Option` exists only so older rows still deserialize on replay.
                        cleared_item_count: Some(cleared_measure.item_count),
                        cleared_params_bytes: Some(cleared_measure.params_bytes),
                        // Clamped at 0: a clock step backwards must not report a negative age.
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
        // THE ROW IS THE SINGLE HOME FOR THE QUEUE: `output` is a durable copy frozen at `prepare_tx`, and a re-driven operation started from it would
        // deliver a sentence another mint has since moved. Re-read from the runtime's own row at the last moment; an unreadable row keeps what `output` carries.
        overwrite_queue_from_the_runtimes_own_row(
            self.repo.as_ref(),
            &worker_session_id,
            &mut snapshot,
        )
        .await?;
        // Atomic replace claim: `reserve_replacing` swaps the slot to Reserved in one entry op and hands back the previous Live handle for shutdown outside the map lock.
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
            backend: self.daemon.clone().into(),
            config: HarnessConfig::default(),
            snapshot,
        });
        if !reservation.install(handle.clone()) {
            // Superseded by a concurrent `reserve_replacing`: shut down the handle we just built (never leak its run loop) and fail the op.
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

    /// `Driver::apply_compensation` marks the operation `Stuck` on the FIRST step error and it is never re-driven; if `delete_card` fails, the lazily minted card stays with `deletable: false`.
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
        // A card this operation minted must come back out on EVERY failure path, so the step is built before any early return and appended by `finish`.
        // It runs LAST: on the spawn arms the harness task is still alive, and deleting the card out from under its run loop is the wrong order.
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
            // This arm returns before `fail_runtime`, so it never runs the give-back; that is only safe because this refusal is raised before the harvesting transaction.
            // Checked with a real assertion, not `debug_assert`: if the journal is not empty the give-back has to run.
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
            // Failing the runtime and giving its harvested sentences back are ONE transaction: a `Stuck` compensation is never re-driven, so a later step would not run.
            // Only messages whose ids are still on this runtime's queue come back, so nothing ends up in two places.
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
            // Tolerant of an already-absent row so a re-driven compensation is idempotent; emits `card.deleted` because `card.added` was already broadcast at commit.
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
                // Narrow to the single deprecated call; a function-level allow would silently cover every other arm.
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

/// Replace a snapshot's pending queue with the one persisted on the runtime's own row, leaving every other field alone.
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

/// Fail the runtime and return what it harvested, in one transaction: either it commits or none of it happened.
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
            // Idempotent against the SOURCE row too: a retired runtime can re-buffer a batch onto its own row after the harvest took it, and pushing it again would deliver the instance twice.
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
                    // Already back where it belongs; still counts as returned, so it is pruned from the failing runtime below.
                    returned_any_ids.extend(message.ids.iter().cloned());
                    continue;
                }
                // The entry goes back under the id it left with, so a give-back returns the same addressable instance rather than a renamed copy.
                source_entries.push(QueueEntry::user_message_moved(
                    message.text.clone(),
                    message.ids.clone(),
                    message.entry_id.clone().map(QueueEntryId::from_wire),
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
            // A row this operation stamped and returned nothing to keeps its stamp: its queue is somewhere else, legitimately.
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
            // SUBTRACT the returned ids; do not delete the entry: `try_fold_tail` can fold a returned id next to a never-harvested one, and the folded text cannot be split.
            let mut kept = Vec::new();
            for mut entry in successor_entries {
                // Two different things make an entry hold no ids, and only one of them means "returned".
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

/// The undo journal, as JSON for `operations.tx_output_json`.
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
                        .map(|m| json!({
                            "text": m.text, "ids": m.ids, "entry_id": m.entry_id,
                        }))
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
                        // Absent in older journals; `None` is the right reading, nothing holds an id.
                        entry_id: m.get("entry_id").and_then(Value::as_str).map(str::to_owned),
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

/// The human sentences a superseded runtime never delivered. ONLY `Observation::UserMessage` travels: `SystemContext` and `TrackGoal` describe the
/// predecessor's workspace and are re-derived. Bad shapes warn and yield nothing; the caller stamps the row either way. KNOWN GAP: a batch the daemon accepted but `persist_issuance_outcome` has not yet written is harvested and re-delivered.
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
    // `pending_entries` pads every side array to the queue's length, so this walk is total. A MOVE: the row keeps what was not taken.
    let mut remaining = snapshot.clone();
    let mut kept = Vec::new();
    let mut taken = Vec::new();
    for mut entry in snapshot.pending_entries() {
        // The filter is `is_user_authored`, so a `LegacyUser` is harvested exactly like a `User`.
        if entry.is_user_authored() {
            // MINT AT THE TRANSFER BOUNDARY: an id-less entry could be moved off its row and never returned. The same id goes into the successor's snapshot and the journal in one transaction.
            let ids = entry.ensure_message_id().to_vec();
            let entry_id = entry.id().map(|id| id.as_str().to_string());
            let Observation::UserMessage { text } = entry.observation() else {
                continue;
            };
            taken.push(HarvestedMessage {
                text,
                ids,
                entry_id,
            });
        } else {
            kept.push(entry);
        }
    }
    remaining.set_pending_entries(kept);
    let Ok(remaining_snapshot) = serde_json::to_value(&remaining) else {
        // `None` means "nothing was taken", so a remainder that will not serialize takes nothing rather than turning the move into a copy.
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
    // Through the single aligner: an older `tx_output_json` has no `pending_message_ids`, and appending before aligning would pair harvested ids with the wrong entries.
    // Guarded because `from_value_strict` panics on an unknown `schema_version` and this value comes off disk.
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

/// Merge this adapter's payload keys into the payload **as it stands inside `tx`**: a snapshot from the previous phase can be hours old on a recovery replay,
/// and writing it back wholesale drops every key another writer put in since. The key set is deliberately NOT `clear_card_runtime_fields`'.
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

#[cfg(test)]
mod tests {

    /// `handle_state_json` is free-form TEXT (terminal/Claude runtimes write other dialects, older binaries other shapes); every bad input must yield nothing rather than fail the mint.
    #[test]
    fn the_harvest_decoder_yields_only_human_sentences_and_never_fails() {
        assert!(
            super::stranded_user_messages("r1", "not json")
                .taken
                .is_empty()
        );
        assert!(
            super::stranded_user_messages("r1", r#"{"mode":"terminal","pending_queue":[]}"#)
                .taken
                .is_empty()
        );
        assert!(
            super::stranded_user_messages(
                "r1",
                r#"{"mode":"harness","schema_version":9999,"pending_queue":[]}"#
            )
            .taken
            .is_empty()
        );
        let empty = serde_json::to_string(&crate::harness::initial_snapshot_with_goal(None))
            .expect("serialize snapshot");
        assert!(super::stranded_user_messages("r1", &empty).taken.is_empty());

        // The human's sentence travels, the machine's context does not.
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
        entries.push(QueueEntry::user_message(
            "first thing said".into(),
            None,
            Vec::new(),
        ));
        entries.push(QueueEntry::user_message(
            "second thing said".into(),
            None,
            Vec::new(),
        ));
        snapshot.set_pending_entries(entries);
        // The ids must travel with the text: the decoder is transport, not a producer.
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

    /// The two transfer boundaries do DIFFERENT things to a `LegacyUser`: the inherit carries it whole, the harvest rebuilds it as a `User`.
    #[test]
    fn the_harvest_makes_a_legacy_sentence_addressable_and_the_inherit_does_not() {
        // A legacy row: user text in `pending_queue`, no meta slot. Reached through JSON because no constructor produces it.
        let seeded = crate::harness::HarnessSnapshot::initial(
            0,
            vec![QueueEntry::user_message(
                "said before the upgrade".into(),
                None,
                Vec::new(),
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

        // THE INHERIT carries the entry whole, so it stays legacy.
        let mut inherited = legacy.clone();
        let minted_in_place = inherited[0].ensure_message_id().to_vec();
        assert_eq!(minted_in_place.len(), 1, "it does gain a transfer identity");
        assert_eq!(inherited[0].id(), None, "…and still no queue id");
        assert!(
            inherited[0].user_view().is_none(),
            "the inherit boundary leaves it OFF the addressable page — GAP-B verbatim"
        );

        // THE HARVEST cannot: it rebuilds the entry on the successor.
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
        assert_eq!(
            taken[0].entry_id, None,
            "and no queue id to carry, because it never had one"
        );
        let rebuilt = QueueEntry::user_message_moved(
            taken[0].text.clone(),
            taken[0].ids.clone(),
            taken[0].entry_id.clone().map(QueueEntryId::from_wire),
        );
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

    /// A harvest does not rename an addressable entry (the browser claims a queued message by `entry_id`); an entry that never had an id must still GAIN one.
    #[test]
    fn a_harvest_carries_an_addressable_id_and_mints_only_for_one_without() {
        let addressable =
            QueueEntry::user_message("please look at the report".into(), None, Vec::new());
        let claimed = addressable
            .id()
            .expect("a minted user entry is addressable")
            .as_str()
            .to_string();

        let mut snapshot = HarnessSnapshot::initial(0, vec![addressable]);
        // An older sentence beside it: `pending_queue` holds the text and the meta slot is absent.
        let mut value = serde_json::to_value(&snapshot).expect("serialize");
        value["pending_queue"]
            .as_array_mut()
            .expect("queue")
            .push(json!({"type": "user_message", "text": "queued before PR1"}));
        snapshot = HarnessSnapshot::from_value_strict(value);
        assert_eq!(
            snapshot.pending_entries()[1].id(),
            None,
            "premise: the second entry has no queue id"
        );

        let taken = super::stranded_user_messages(
            "r1",
            &serde_json::to_string(&snapshot).expect("serialize snapshot"),
        )
        .taken;
        assert_eq!(taken.len(), 2, "both sentences are harvested");
        assert_eq!(
            taken[0].entry_id.as_deref(),
            Some(claimed.as_str()),
            "the journal carries the id the client is holding"
        );
        assert_eq!(
            taken[1].entry_id, None,
            "and carries none for the entry that never had one"
        );

        let rebuilt: Vec<QueueEntry> = taken
            .iter()
            .map(|message| {
                QueueEntry::user_message_moved(
                    message.text.clone(),
                    message.ids.clone(),
                    message.entry_id.clone().map(QueueEntryId::from_wire),
                )
            })
            .collect();
        assert_eq!(
            rebuilt[0].id().map(|id| id.as_str().to_string()),
            Some(claimed),
            "the successor lists the SAME id, so the claim still names this sentence"
        );
        let minted = rebuilt[1]
            .id()
            .expect("the legacy sentence becomes addressable on arrival");
        assert!(
            !minted.as_str().is_empty(),
            "and it is a real id, not an empty one"
        );
    }

    /// The persisted wire shape, pinned as a golden: this payload is hashed into `operations.payload_hash` and a renamed field is a permanent 409.
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

        // The alias spelling still reads back; derived from the real serialization so the case cannot drift.
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
            expected.contains(concat!(
                "Before you directly edit the report in a session, call `calm.report.read` once: ",
                "the report carries its own structure and its own maintenance contract, and ",
                "you may not directly edit a document you have not read. ",
                "The bounded `calm.task.dispatch` creation below does not require this report read."
            )),
            "base prompt must require the report read before direct edits and explicitly exempt bounded Dispatch"
        );
        assert!(expected.contains("Selected Template snapshot"));
        assert!(expected.contains("not to reproduce a template checklist"));
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

    /// The boot-recovery selector's SQL literals (`('assistant','assistant')`, `('worker','plain_chat')` in calm-truth) have no compile-time link to
    /// `minted_card_shape`; a rename on either side must be red here.
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
        // A track owned by the *untrusted* plugin pins the trust half of the filter on the owner column itself.
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

    /// `plugin_scope` is a parameter every caller has to state: create copies the bound plugin into `plugin_scope` and refuses `template_input` without one.
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
            // A plugin that accepts no `template_input` cannot own a track that carries one; declaring the schema keeps this fixture inside reachable states.
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

    /// `plan_compensation` returns EARLY with no steps on the missing-per-card-MCP-token fence; without an unconditional `delete_card` this would leave a card with no session.
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

        // Counterexample: without `create_card` the zero-step contract is preserved verbatim.
        let without_seed = plan_compensation_for(&adapter, &output, &reason, false).await;
        assert!(
            without_seed.steps.is_empty(),
            "operations that did not mint a card must keep the zero-step early return: {:?}",
            without_seed.steps
        );
    }

    /// `delete_card` is unconditional AND ordered after `abort_harness_task`: on the spawn arms the run loop is still alive.
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

        // Counterexample: the same phase without a minted card keeps the step list untouched.
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
