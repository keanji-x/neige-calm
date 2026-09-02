use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    HarnessTranscriptMeasure, append_decision_event_in_tx, card_create_with_id_tx, card_delete_tx,
    card_update_tx, harness_items_delete_by_card_tx, harness_items_measure_by_card_tx,
    session_bind_attribution_tx, session_delete_tx, session_fail_if_active_runtime_tx,
    session_prepare_deferred_spec_tx, session_projection_active_for_card_tx,
    session_restore_from_superseded_runtime_tx, session_set_handle_state_tx,
    session_start_runtime_tx, session_supersede_and_start_tx,
};
use crate::db::{Repo, write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::forge_trust::trusted_forge_plugin;
use crate::harness::{
    HARNESS_MODE, HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, SpecHarness,
    SpecHarnessParams, initial_snapshot_with_goal, is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, WaveId};
use crate::mcp_server::wiring::{
    mint_card_mcp_token_pair, mirror_session_mcp_token, persist_card_mcp_token_hash,
};
use crate::model::{Card, CardPatch, CardRole, NewCard, new_id, now_ms};
// Issue #649 i2 lifted the per-card lock-map machinery that used to live in
// this module into `crate::per_card_lock` so the `/spec/input` lazy-recovery
// path can share it. Same semantics: guards self-clean their entry on drop.
use crate::per_card_lock::{PerCardLockGuard, PerCardLocks, lock_card, new_per_card_locks};
use crate::plugin_host::{PluginHost, manifest::WorkflowDescriptor};
use crate::routes::cards::card_scope;
use crate::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams, ThreadConfig};
use crate::state::WriteContext;
use crate::validation::{OVERLAY_TEMPLATE_ENTITY_KIND, is_template_overlay};
use crate::wave_cove_cache::WaveCoveCache;
use calm_truth::decision_gate::PermissiveGate;

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

/// 4xx used by `validate` and `/spec/reset` when the wave carries the kernel
/// view/template overlay (#1110 S1).
pub fn template_wave_spec_harness_error(wave_id: &str) -> CalmError {
    CalmError::BadRequest(format!(
        "wave {wave_id} is a template (kernel view/template overlay); spec harness is not started on template waves"
    ))
}

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
pub struct SpecHarnessStartAdapter {
    repo: Arc<dyn Repo>,
    daemon: Arc<SharedCodexAppServer>,
    harness_registry: HarnessRegistry,
    plugin: Arc<PluginHost>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    mcp_socket_path: Option<PathBuf>,
    per_card_mint_locks: PerCardLocks,
}

impl SpecHarnessStartAdapter {
    pub fn new(
        repo: Arc<dyn Repo>,
        daemon: Arc<SharedCodexAppServer>,
        harness_registry: HarnessRegistry,
        plugin: Arc<PluginHost>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        mcp_socket_path: Option<PathBuf>,
    ) -> Self {
        Self {
            repo,
            daemon,
            harness_registry,
            plugin,
            card_role_cache,
            wave_cove_cache,
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
                "spec harness MCP socket path missing".into(),
            ))
        }
    }

    async fn bound_workflow(&self, wave_id: &str) -> Result<Option<BoundWorkflow>> {
        let wave = match self.repo.wave_get(wave_id).await {
            Ok(wave) => wave,
            Err(error) => {
                tracing::error!(
                    target: "spec_harness::workflow_binding",
                    wave_id,
                    error = %error,
                    "workflow binding lookup failed; using vanilla spec prompt"
                );
                return Ok(None);
            }
        };
        let Some(wave) = wave else {
            tracing::error!(
                target: "spec_harness::workflow_binding",
                wave_id,
                "bound workflow wave was not found while resolving descriptor; using vanilla spec prompt"
            );
            return Ok(None);
        };
        let Some(template_id) = wave.template_id.as_deref() else {
            return Ok(None);
        };
        let running_plugin_ids = self.plugin.running_plugin_ids().await;
        for manifest in self.plugin.registry().list() {
            if !running_plugin_ids.contains(&manifest.id) || !trusted_forge_plugin(&manifest.id) {
                continue;
            }
            if let Some(workflow) = manifest
                .workflows
                .into_iter()
                .find(|workflow| workflow.id == template_id)
            {
                return Ok(Some(BoundWorkflow {
                    descriptor: workflow,
                    input: wave.template_input.clone(),
                }));
            }
        }
        // Descriptor unresolved (plugin stopped / trust revoked): fail-safe
        // to the vanilla prompt — the persisted template_input is dropped
        // along with the descriptor rather than injected without context.
        tracing::error!(
            target: "spec_harness::workflow_binding",
            wave_id,
            template_id,
            "bound workflow descriptor was not resolved from a running trusted forge plugin; using vanilla spec prompt"
        );
        Ok(None)
    }
}

/// #891 — a resolved workflow binding: the descriptor from the running
/// trusted plugin plus the wave row's persisted `template_input` (already
/// schema-validated at create time).
struct BoundWorkflow {
    descriptor: WorkflowDescriptor,
    input: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessProfile {
    #[default]
    Spec,
    PlainChat,
    /// #1189 — a wave-scoped assistant conversation. Minted lazily like a
    /// plain chat, but it is an MCP-authenticated `CardRole::Assistant` card on
    /// an ordinary wave: it reads and writes that wave's report through the
    /// block channel and has no lifecycle / plan / review / admin authority.
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecHarnessStartOperationPayload {
    pub actor: ActorId,
    pub wave_id: String,
    pub spec_card_id: CardId,
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
    /// `Some` asks this operation to create `spec_card_id` itself, inside the
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
    /// #1098 slice 3, round-3 Y1 — SHA-256 (lower-case hex) of the cove
    /// conversation's first message, set only by
    /// `POST /api/coves/{cove_id}/conversations`.
    ///
    /// This field is **never read** by this adapter. It exists so the first
    /// message body reaches `stable_payload_hash`, which is what makes
    /// `OperationRuntime::submit` answer 409 when one `Idempotency-Key` is
    /// replayed with a different body instead of silently replaying the
    /// earlier conversation (see `driver.rs::submit`, which compares
    /// `payload_hash` before anything else runs).
    ///
    /// A hash, not the text: this payload is persisted verbatim in
    /// `operations.payload_json`, and storing the body would copy every user
    /// message into a second table with a different retention story.
    ///
    /// `skip_serializing_if` keeps every non-cove caller's payload bytes
    /// byte-identical to what older binaries wrote, so adding this field
    /// cannot move their `payload_hash` and turn an in-flight retry into a
    /// spurious 409. `#[serde(default)]` because
    /// `abandoned_running_operations_on_boot` re-drives old payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message_sha256: Option<String>,
}

/// The user-supplied part of a lazily minted conversation card — a cove chat
/// (`HarnessProfile::PlainChat`) or a wave assistant (`HarnessProfile::Assistant`).
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
    /// The caller's `Idempotency-Key`, so [`SpecHarnessStartAdapter::validate`]
    /// can **recompute** the deterministic card id and refuse anything it did
    /// not derive itself (§4.3). Required on the assistant profile; see the
    /// guard for why the cove profile does not read it.
    ///
    /// It lives on the seed rather than on the payload root so the cove route,
    /// which does not set it, keeps writing byte-identical
    /// `operations.payload_json` — a changed payload hash would turn an
    /// in-flight retry across a deploy into a spurious 409.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Which session row a profile writes.
///
/// `Assistant` is `CodexCard`, NOT `SharedSpec`, and the difference is a
/// safety property rather than bookkeeping: `derive_session_identity`
/// (`calm-truth/src/db/sqlite/session_row.rs`) maps `SharedSpec` to
/// `WorkerContract::Planner`, and `session_repoint_current_links_tx`
/// (`session_mirror.rs`) makes every live Planner session the wave's
/// `root_session_id`. An assistant session under `SharedSpec` would therefore
/// displace the spec card's session as the wave's planning authority the
/// moment it started.
fn session_kind_for(profile: HarnessProfile) -> WorkerSessionKind {
    match profile {
        HarnessProfile::Spec => WorkerSessionKind::SharedSpec,
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
/// `Spec` is rejected here rather than silently defaulted — `validate` already
/// refuses that profile on the mint branch, and this is the fail-closed twin so
/// a future caller that skips `validate` cannot mint a spec card either.
fn minted_card_shape(profile: HarnessProfile) -> Result<(CardRole, &'static str)> {
    match profile {
        HarnessProfile::PlainChat => Ok((CardRole::Worker, "plain_chat")),
        HarnessProfile::Assistant => Ok((CardRole::Assistant, ASSISTANT_HARNESS_PROFILE_MARKER)),
        HarnessProfile::Spec => Err(CalmError::BadRequest(
            "the spec profile does not mint its own card".into(),
        )),
    }
}

/// The persisted `payload.harness_profile` value of a wave assistant card.
/// Read by `plain_chat::card_is_wave_assistant` and by the wave conversation
/// list predicate; those three places are the whole contract.
pub(crate) const ASSISTANT_HARNESS_PROFILE_MARKER: &str = "assistant";

pub(crate) fn render_spec_developer_instructions(
    wave_id: &str,
    workflow_descriptor: Option<&WorkflowDescriptor>,
    template_input: Option<&serde_json::Value>,
) -> String {
    let mut instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Spec.prompt_template(),
        wave_id,
    );
    // #1110 S5 — the descriptor is an id handle only. Plan prose lives in
    // the forked report; the remaining injected contract is the wave's
    // validated `template_input`, gated on a resolved binding.
    if workflow_descriptor.is_none() {
        return instructions;
    }
    // #891 — the wave's validated template_input, verbatim. Deliberately
    // NOT passed through `render_system_prompt`: user-controlled JSON must
    // not have literal `{wave_id}` substituted.
    if let Some(input) = template_input {
        instructions.push_str("\n\n## Bound Workflow Input\n");
        instructions.push_str("```json\n");
        instructions
            .push_str(&serde_json::to_string_pretty(input).expect("template_input serializes"));
        instructions.push_str("\n```");
    }
    instructions
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn render_spec_developer_instructions_for_test(
    wave_id: &str,
    workflow_descriptor: Option<&WorkflowDescriptor>,
    template_input: Option<&serde_json::Value>,
) -> String {
    render_spec_developer_instructions(wave_id, workflow_descriptor, template_input)
}

#[async_trait]
impl ProviderAdapter for SpecHarnessStartAdapter {
    fn kind(&self) -> &'static str {
        "spec-harness-start"
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
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(input.clone())?;
        let wave = self
            .repo
            .wave_get(&payload.wave_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("wave {}", payload.wave_id)))?;
        if self
            .repo
            .overlays_for(OVERLAY_TEMPLATE_ENTITY_KIND, payload.wave_id.as_str())
            .await?
            .iter()
            .any(is_template_overlay)
        {
            return Err(template_wave_spec_harness_error(payload.wave_id.as_str()));
        }
        // #1098 §5.6 — the lazy-mint branch. `validate` runs BEFORE the
        // operation row is inserted, so the usual "card must exist + its role
        // cache entry must match" assertions are not merely wrong here, they
        // are unsatisfiable: the card is this operation's own output. The
        // checks below are the fail-closed replacement, and they must stay
        // strictly narrower than the ordinary branch — a caller must not be
        // able to conjure a chat card onto an arbitrary wave.
        if let Some(seed) = payload.create_card.as_ref() {
            // Guard ① — which profiles may mint at all. `Spec` never can: a
            // spec card is minted with its wave, and letting this branch write
            // one would hand a caller the wave's lifecycle authority.
            //
            // Guard ② — where. The two profiles answer it differently, and the
            // assistant answer is the stronger one (#1189 §4.3): instead of
            // asking what KIND of wave this is, the adapter recomputes the
            // deterministic card id from `(wave_id, idempotency_key)` and
            // refuses any id it did not derive itself. A forged id, an id
            // derived for somebody else's wave, and an id conjured out of
            // nothing all fail the same comparison, so the assistant branch
            // needs no wave-shape allowlist to stay closed.
            match payload.profile {
                HarnessProfile::Spec => {
                    return Err(CalmError::BadRequest(format!(
                        "card {} can only be minted by this operation under the plain-chat or assistant profile",
                        payload.spec_card_id
                    )));
                }
                HarnessProfile::PlainChat => {
                    // Deliberately NOT migrated to the derived-id guard. The
                    // cove flavour's id is derived from `(cove_id, key)`, and
                    // recomputing it here would mean binding the key into the
                    // cove route's operation payload — changing a live payload
                    // hash for no gain, since this check is already exact for
                    // that flavour: a cove chat card belongs on a cove chat
                    // wave and nowhere else.
                    if wave.purpose.as_deref() != Some(crate::COVE_CHAT_PURPOSE) {
                        return Err(CalmError::Forbidden(format!(
                            "wave {} is not a cove chat wave; chat cards are only minted there",
                            wave.id
                        )));
                    }
                }
                HarnessProfile::Assistant => {
                    // Fail closed on a missing key: without it there is nothing
                    // to recompute from, and "no key" must not read as "no
                    // check". Only `POST /api/waves/{id}/conversations` sets
                    // it, and that route requires the header.
                    let Some(idempotency_key) = seed.idempotency_key.as_deref() else {
                        return Err(CalmError::BadRequest(format!(
                            "card {} cannot be minted without the idempotency key its id is derived from",
                            payload.spec_card_id
                        )));
                    };
                    let expected = crate::conversation_keys::derive_wave_conversation_keys(
                        wave.id.as_str(),
                        idempotency_key,
                    )
                    .card_id;
                    if payload.spec_card_id.as_str() != expected {
                        return Err(CalmError::Forbidden(format!(
                            "card {} is not the conversation id derived for wave {} under this idempotency key",
                            payload.spec_card_id, wave.id
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
                .card_get(payload.spec_card_id.as_str())
                .await?
                .is_some()
            {
                return Err(CalmError::Conflict(format!(
                    "card {} already exists; refusing to re-mint a chat conversation card",
                    payload.spec_card_id
                )));
            }
            if !self.daemon.is_running() {
                return Err(self.daemon.not_running_error());
            }
            return Ok(());
        }
        let Some(card) = self.repo.card_get(payload.spec_card_id.as_str()).await? else {
            return Err(CalmError::NotFound(format!(
                "card {}",
                payload.spec_card_id
            )));
        };
        if card.wave_id.as_str() != payload.wave_id {
            return Err(CalmError::BadRequest(format!(
                "spec card {} belongs to wave {}, not {}",
                card.id, card.wave_id, payload.wave_id
            )));
        }
        let expected_role = match payload.profile {
            HarnessProfile::Spec => CardRole::Spec,
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
            // Re-start paths (`/spec/reset`, `/spec/input` lazy recovery) reach
            // here with an existing assistant card; the marker + role pair is
            // what makes the profile legitimate, exactly as for plain chat.
            HarnessProfile::Assistant
                if crate::plain_chat::card_is_wave_assistant(
                    &card,
                    self.card_role_cache.get(&card.id),
                    true,
                ) =>
            {
                CardRole::Assistant
            }
            HarnessProfile::Assistant => {
                return Err(CalmError::BadRequest(format!(
                    "card {} is not marked as a wave assistant",
                    card.id
                )));
            }
        };
        if self.card_role_cache.get(&card.id) != Some(expected_role) {
            let message = match payload.profile {
                HarnessProfile::Spec => format!("card {} is not a spec card", card.id),
                HarnessProfile::PlainChat => {
                    format!("card {} is not marked for plain chat", card.id)
                }
                HarnessProfile::Assistant => {
                    format!("card {} is not marked as a wave assistant", card.id)
                }
            };
            return Err(CalmError::BadRequest(message));
        }
        if expected_role == CardRole::Spec
            && wave.purpose.as_deref() == Some(crate::COVE_CHAT_PURPOSE)
        {
            return Err(CalmError::Forbidden(format!(
                "spec harness is disabled for cove chat wave {}",
                wave.id
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
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = payload.spec_card_id;
        let wave_id = payload.wave_id;
        let report_card_id = payload.report_card_id;
        let defer_runtime_start = payload.force_new_thread;
        let session_kind = session_kind_for(payload.profile);
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
                WaveId::from(wave_id.clone()),
            )
            .await?;
            let created = card_create_with_id_tx(
                tx,
                card_id.to_string(),
                NewCard {
                    wave_id: WaveId::from(wave_id.clone()),
                    kind: "codex".into(),
                    sort: seed.sort,
                    // Pinned by the kernel, not by the caller. Note the absent
                    // `spec_harness` key (INV-CHAT-016) and the unchanged
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
            if let Err(violation) = crate::role_gate::enforce_role(
                &payload.actor,
                &event,
                &scope,
                &self.card_role_cache,
                &self.wave_cove_cache,
            ) {
                return Err(CalmError::Forbidden(violation.to_string()));
            }
            let event_id = append_decision_event_in_tx(
                tx,
                &PermissiveGate,
                &payload.actor,
                &scope,
                None,
                &event,
            )
            .await?;
            post_commit_events.push(BroadcastEnvelope {
                id: event_id,
                event_version: SYNC_EVENT_VERSION,
                actor: payload.actor.clone(),
                scope,
                event,
            });
        }
        let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, title, deletable, created_at, updated_at
                 FROM cards
                WHERE id = ?1
                  AND wave_id = ?2"#,
        )
        .bind(card_id.as_str())
        .bind(wave_id.as_str())
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
        if let Some(inherited) = inherited_snapshot {
            snapshot.push_watermark = inherited.push_watermark;
            snapshot.pending_queue = inherited.pending_queue;
            snapshot.pending_envelope_ids = inherited.pending_envelope_ids;
            snapshot.align_pending_envelope_ids();
        }

        let runtime_id = new_id();
        let mut old_runtime_id = None;
        let mut old_runtime_status = None;
        let runtime_init = WorkerSessionInit {
            id: runtime_id.clone(),
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
            now_ms: now_ms(),
        };
        if defer_runtime_start {
            if let Some(existing) = existing_active_runtime.as_ref() {
                old_runtime_id = Some(existing.id.clone());
                old_runtime_status = Some(existing.status);
            }
            session_prepare_deferred_spec_tx(tx, &runtime_init).await?;
        } else {
            if let Some(existing) =
                session_projection_active_for_card_tx(tx, card.id.as_str()).await?
            {
                old_runtime_id = Some(existing.id.clone());
                old_runtime_status = Some(existing.status);
                session_supersede_and_start_tx(tx, &existing.id, runtime_init).await?;
            } else {
                session_start_runtime_tx(tx, runtime_init).await?;
            }
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.post_commit_events = post_commit_events;
        output.data = json!({
            "card_id": card.id,
            "wave_id": wave_id,
            "runtime_id": runtime_id,
            "runtime_deferred": defer_runtime_start,
            "cwd": payload.cwd,
            "goal": payload.goal,
            "report_card_id": report_card_id,
            "snapshot": snapshot,
        });
        if let Some(old_runtime_id) = old_runtime_id {
            output.set_output_data("old_runtime_id", json!(old_runtime_id), "spec harness")?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            output.set_output_data(
                "old_runtime_status",
                serde_json::to_value(old_runtime_status)?,
                "spec harness",
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
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(op.payload.clone())?;
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
            HarnessProfile::Spec => CardRole::Spec,
            HarnessProfile::PlainChat => CardRole::Worker,
            HarnessProfile::Assistant => CardRole::Assistant,
        };
        let card_id = output.output_string("card_id", "spec harness")?;
        let wave_id = output.output_string("wave_id", "spec harness")?;
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let runtime_deferred = output_bool(output, "runtime_deferred")?;
        let cwd = output.output_string("cwd", "spec harness")?;
        // OLD PTY shutdown at Phase-2 entry, immediately after the Phase-1
        // tx commit. Per RATIFY-8 section 5 / 1.4, force_new_thread is a
        // hard reset: the DB-side supersede lives in calm-truth, and the
        // handle kill stays here as the first app-server-side action after
        // commit.
        if let Some(old_runtime_id) =
            output.output_optional_string("old_runtime_id", "spec harness")?
            && old_runtime_id != runtime_id
            && let Some(old_handle) = self.harness_registry.remove(&old_runtime_id)
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
                    "spec card {card_id} reuses thread {thread_id} with \
                     {REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR} \
                     (re-run to mint a fresh thread)"
                );
                tracing::warn!(
                    target: "spec_harness::reusable_thread_invariant",
                    %card_id,
                    thread_id = %thread_id,
                    error = %message,
                    "refusing to reuse spec thread without per-card MCP token row; migration 0035 should have nulled this thread_id"
                );
                return Err(CalmError::Conflict(message));
            }
            thread_id
        } else {
            let developer_instructions = match profile {
                // A plain chat is a bare codex thread: no kernel prompt, no
                // MCP tools to describe.
                HarnessProfile::PlainChat => None,
                // The assistant gets its own prompt, not the spec one: the spec
                // prompt is mostly lifecycle/plan/verdict instructions for
                // tools the assistant is forbidden to call, and a workflow
                // binding drives the wave's plan, which is likewise not the
                // assistant's business.
                HarnessProfile::Assistant => Some(crate::spec_card::render_system_prompt(
                    crate::spec_card::ASSISTANT_SYSTEM_PROMPT_TEMPLATE,
                    &wave_id,
                )),
                HarnessProfile::Spec => {
                    let bound_workflow = self.bound_workflow(&wave_id).await?;
                    Some(render_spec_developer_instructions(
                        &wave_id,
                        bound_workflow.as_ref().map(|bound| &bound.descriptor),
                        bound_workflow
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
            // and spec spawn paths all emit the byte-identical shape from one
            // place. Previously this path wrapped `card_mcp_env` in its own
            // parallel `SpecThread*` structs.
            let params = SharedThreadStartParams {
                cwd,
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions,
                // The assistant needs the same channel-3 MCP credentials as the
                // spec harness — the block channel is its only write surface,
                // and `neige` is how it reads the wave. WHICH tools that token
                // can reach is not decided here: `tools/list` filters on
                // `ToolDescriptor::visible_to_roles` and every write handler
                // re-checks `require_role*`, both resolved from the card's
                // persisted role. So the §3.1 whitelist rides on
                // `minted_card_shape`'s role, and a third `ThreadConfig`
                // variant would carry nothing the kernel actually reads.
                config: match profile {
                    HarnessProfile::Spec | HarnessProfile::Assistant => ThreadConfig::McpShell {
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
                    .thread_start_for_card(&card_id, card_role, Some(&wave_id), params)
                    .await?
            }
        };
        output.set_output_data("codex_thread_id", json!(thread_id.clone()), "spec harness")?;
        let mut snapshot = output_snapshot(output)?;
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(thread_id.clone());
        output.set_output_data("snapshot", serde_json::to_value(&snapshot)?, "spec harness")?;
        let mut card: crate::model::Card = serde_json::from_value(output.result.clone())?;
        let mut card_payload = card.payload.clone();
        let Some(map) = card_payload.as_object_mut() else {
            return Err(CalmError::Internal(format!(
                "spec harness card {card_id} payload is not a JSON object"
            )));
        };
        map.insert("codex_thread_id".into(), Value::String(thread_id.clone()));
        map.insert(
            "appserver_sock".into(),
            Value::String(self.daemon.remote_uri()),
        );
        map.remove("appserver_pgid");
        map.remove("appserver_start_time");
        map.remove("appserver_boot_id");
        map.remove("appserver_needs_initial_prompt");

        let scope = card_scope(ctx.repo.as_ref(), card.id.clone(), card.wave_id.clone()).await?;
        let transcript_scope = scope.clone();
        let transcript_runtime_id = runtime_id.clone();
        let transcript_card_id = CardId::from(card_id.clone());
        let transcript_wave_id = WaveId::from(wave_id.clone());
        let write = WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
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
                    let mut old_runtime_id = None;
                    let mut old_runtime_status = None;
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        persist_card_mcp_token_hash(tx, &card_id, hashed).await?;
                    }
                    if runtime_deferred {
                        let runtime_init = WorkerSessionInit {
                            id: runtime_id.clone(),
                            card_id: card_id.clone(),
                            kind: session_kind,
                            agent_provider: Some(AgentProvider::Codex),
                            status: WorkerSessionState::Starting,
                            terminal_run_id: None,
                            thread_id: Some(thread_for_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: Some(serde_json::to_value(&snapshot)?),
                            spawn_op_id: None,
                            now_ms: now_ms(),
                        };
                        if let Some(existing) =
                            session_projection_active_for_card_tx(tx, &card_id).await?
                        {
                            let existing_id = existing.id.clone();
                            let existing_status = existing.status;
                            if existing_id != runtime_id {
                                old_runtime_id = Some(existing_id.clone());
                                old_runtime_status = Some(existing_status);
                                checkpoint_output.set_output_data(
                                    "old_runtime_id",
                                    json!(existing_id),
                                    "spec harness",
                                )?;
                                checkpoint_output.set_output_data(
                                    "old_runtime_status",
                                    serde_json::to_value(existing_status)?,
                                    "spec harness",
                                )?;
                            }
                            session_supersede_and_start_tx(tx, &existing.id, runtime_init).await?;
                        } else {
                            session_start_runtime_tx(tx, runtime_init).await?;
                        }
                    } else {
                        session_bind_attribution_tx(
                            tx,
                            &runtime_id,
                            ThreadAttribution {
                                runtime_id: runtime_id.clone(),
                                provider: AgentProvider::Codex,
                                thread_id: Some(thread_for_tx.clone()),
                                session_id: None,
                                active_turn_id: None,
                            },
                        )
                        .await?;
                        session_set_handle_state_tx(
                            tx,
                            &runtime_id,
                            Some(serde_json::to_value(&snapshot)?),
                        )
                        .await?;
                    }
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        mirror_session_mcp_token(tx, &runtime_id, hashed).await?;
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
                    let card = card_update_tx(
                        tx,
                        &card_id,
                        CardPatch {
                            title: None,
                            kind: None,
                            sort: None,
                            payload: Some(card_payload),
                            deletable: None,
                        },
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
                            old_runtime_id,
                            old_runtime_status,
                            cleared_measure,
                        ),
                        Event::CardUpdated(card),
                    ))
                })
            },
        )
        .await?;
        let (updated_card, old_runtime_id, old_runtime_status, cleared_measure) = tx_out;
        drop(mint_lock_guard);
        card = updated_card;
        if let Some(old_runtime_id) = old_runtime_id {
            output.set_output_data("old_runtime_id", json!(old_runtime_id), "spec harness")?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            output.set_output_data(
                "old_runtime_status",
                serde_json::to_value(old_runtime_status)?,
                "spec harness",
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
                    &self.wave_cove_cache,
                    Event::HarnessTranscriptCleared {
                        runtime_id: transcript_runtime_id,
                        card_id: transcript_card_id,
                        wave_id: transcript_wave_id,
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
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let card_id = output.output_string("card_id", "spec harness")?;
        let wave_id = output.output_string("wave_id", "spec harness")?;
        let thread_id = output.output_optional_string("codex_thread_id", "spec harness")?;
        let snapshot = output_snapshot(output)?;
        // #953 §5 — atomic replace claim: the old remove-then-insert pair
        // left a window where a concurrent registration could land between
        // the two ops. `reserve_replacing` swaps the slot to Reserved in one
        // entry op and hands back the previous Live handle for shutdown
        // outside the map lock.
        let (reservation, previous_live) =
            self.harness_registry.reserve_replacing(runtime_id.clone());
        if let Some(existing) = previous_live {
            existing.shutdown().await?;
        }
        let handle = SpecHarness::run(SpecHarnessParams {
            runtime_id: runtime_id.clone(),
            wave_id: WaveId::from(wave_id),
            card_id: CardId::from(card_id),
            thread_id,
            repo: self.repo.clone(),
            events: ctx.events.clone(),
            card_role_cache: self.card_role_cache.clone(),
            wave_cove_cache: self.wave_cove_cache.clone(),
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
                "spec harness registration for runtime {runtime_id} superseded during start"
            )));
        }
        handle.persist_snapshot().await?;
        Ok(SpawnOutcome::Ready(SpawnHandle::Harness { runtime_id }))
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
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(op.payload.clone())?;
        let card_id = output.output_string("card_id", "spec harness")?;
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let thread_id = output.output_optional_string("codex_thread_id", "spec harness")?;
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
                let wave_id = output.output_string("wave_id", "spec harness")?;
                Some(CompensationStep::new(
                    "delete_card",
                    json!({ "card_id": card_id, "wave_id": wave_id }),
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
            return Ok(finish(steps));
        }
        if matches!(
            from_phase,
            PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) {
            steps.push(CompensationStep::new(
                "abort_harness_task",
                json!({ "runtime_id": runtime_id }),
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
            json!({ "runtime_id": runtime_id }),
        ));
        if let Some(old_runtime_id) =
            output.output_optional_string("old_runtime_id", "spec harness")?
        {
            let old_runtime_status =
                output
                    .data
                    .get("old_runtime_status")
                    .cloned()
                    .ok_or_else(|| {
                        CalmError::Internal(
                            "spec harness tx_output missing old_runtime_status".into(),
                        )
                    })?;
            steps.push(CompensationStep::new(
                "restore_old_runtime",
                json!({
                    "runtime_id": old_runtime_id,
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
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                if let Some(handle) = self.harness_registry.remove(&runtime_id) {
                    handle.shutdown().await?;
                }
                Ok(())
            }
            "interrupt_thread" => {
                if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str)
                    && let Err(e) = self.daemon.interrupt_active_turn(thread_id).await
                {
                    tracing::warn!(thread_id, error = %e, "spec harness compensation interrupt failed");
                }
                let card_id = step.arg_string("card_id", "spec harness")?;
                clear_card_runtime_fields(ctx, &card_id).await?;
                Ok(())
            }
            "fail_runtime" => {
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        session_fail_if_active_runtime_tx(tx, &runtime_id)
                            .await
                            .map_err(CalmError::from)
                    })
                })
                .await
            }
            // #1098 §5.6 — undo the lazily minted chat card. Tolerant of an
            // already-absent row so a re-driven compensation is idempotent;
            // emits `card.deleted` because `card.added` was already broadcast
            // at commit, and a client that saw the add must see the removal.
            "delete_card" => {
                let card_id = step.arg_string("card_id", "spec harness")?;
                let wave_id = step.arg_string("wave_id", "spec harness")?;
                if ctx.repo.card_get(&card_id).await?.is_none() {
                    return Ok(());
                }
                let card_id = CardId::from(card_id);
                let wave_id = WaveId::from(wave_id);
                let scope = card_scope(ctx.repo.as_ref(), card_id.clone(), wave_id.clone()).await?;
                let write =
                    WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
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
                                    wave_id,
                                },
                            ))
                        })
                    },
                )
                .await?;
                Ok(())
            }
            "restore_old_runtime" => {
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                let status = step_arg_run_status(step, "status")?;
                restore_old_runtime_after_spawn_failure(ctx.repo.as_ref(), runtime_id, status).await
            }
            "delete_runtime" => {
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        session_delete_tx(tx, &runtime_id)
                            .await
                            .map_err(CalmError::from)?;
                        Ok(())
                    })
                })
                .await
            }
            other => Err(CalmError::Internal(format!(
                "unknown spec harness start compensation op {other}"
            ))),
        }
    }
}

fn is_reusable_thread_missing_card_mcp_token_failure(reason: &str) -> bool {
    reason.contains(REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR)
}

fn output_snapshot(output: &TxOutput) -> Result<HarnessSnapshot> {
    let value = output
        .data
        .get("snapshot")
        .cloned()
        .ok_or_else(|| CalmError::Internal("spec harness output missing snapshot".into()))?;
    Ok(serde_json::from_value(value)?)
}

fn output_bool(output: &TxOutput, key: &str) -> Result<bool> {
    match output.data.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        None => Ok(false),
        Some(_) => Err(CalmError::Internal(format!(
            "spec harness tx_output {key} must be bool"
        ))),
    }
}

fn output_existing_thread_id(output: &TxOutput) -> Result<Option<String>> {
    Ok(output
        .output_optional_string("codex_thread_id", "spec harness")?
        .filter(|id| !id.trim().is_empty()))
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
    old_runtime_id: String,
    status: WorkerSessionState,
) -> Result<()> {
    active_run_status_to_db(&status)?;
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_restore_from_superseded_runtime_tx(tx, &old_runtime_id, status)
                .await
                .map_err(CalmError::from)
        })
    })
    .await
}

fn active_run_status_to_db(status: &WorkerSessionState) -> Result<&'static str> {
    if status.is_terminal() {
        Err(CalmError::Internal(format!(
            "cannot restore old spec harness runtime to terminal status {status:?}"
        )))
    } else {
        Ok(status.as_db_str())
    }
}

fn step_arg_run_status(step: &CompensationStep, key: &str) -> Result<WorkerSessionState> {
    let value = step.args.get(key).cloned().ok_or_else(|| {
        CalmError::Internal(format!(
            "spec harness compensation step {} missing {key}",
            step.op
        ))
    })?;
    Ok(serde_json::from_value(value)?)
}

// The per-card lock behavior test moved to `crate::per_card_lock::tests`
// alongside the lifted implementation (issue #649 i2).

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::db::prelude::{ServerRepoOutOfDomainExt, ServerRepoSyncDomainRawExt};
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::model::{NewCove, NewPlugin, NewWave};
    use crate::operation::Phase;
    use crate::plugin_host::{Manifest, PluginRegistry, PluginRuntimeStatus};
    use crate::routes::theme::RequestTheme;
    use tokio::time::{Instant, sleep};

    const WORKFLOW_ID: &str = "issue-development";

    fn populated_workflow_descriptor() -> WorkflowDescriptor {
        WorkflowDescriptor {
            id: "issue-development".into(),
        }
    }

    #[test]
    fn spec_developer_instructions_do_not_inject_workflow_plan_prose() {
        let workflow = populated_workflow_descriptor();
        let out = render_spec_developer_instructions("wave-abc", Some(&workflow), None);

        assert!(
            !out.contains("## Bound Workflow Instructions"),
            "S3 must not inject spec_instructions"
        );
        assert!(
            !out.contains("## Bound Workflow Plan Template"),
            "S3 must not inject plan_template"
        );
        assert!(
            !out.contains("## Bound Workflow Gates"),
            "S3 must not inject gates"
        );
        assert!(
            !out.contains("Follow workflow instructions for wave"),
            "descriptor prose must not leak into the spec prompt"
        );
        assert!(!out.contains(r#""key": "review-a""#));
        assert!(!out.contains(r#""cmd": "cargo fmt --all --check""#));
        assert!(!out.contains("## Bound Workflow Input"));
        assert_eq!(
            out,
            crate::spec_card::render_system_prompt(
                crate::spec_card::SeededCardRole::Spec.prompt_template(),
                "wave-abc",
            )
        );
    }

    #[test]
    fn spec_developer_instructions_without_workflow_match_static_template() {
        let expected = crate::spec_card::render_system_prompt(
            crate::spec_card::SeededCardRole::Spec.prompt_template(),
            "wave-abc",
        );

        let out = render_spec_developer_instructions("wave-abc", None, None);

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
    fn spec_developer_instructions_append_template_input_when_present() {
        let workflow = populated_workflow_descriptor();
        let input = json!({
            "issue_url": "https://github.com/o/r/issues/1",
            "repo": "o/r",
            "issue_number": 1,
            "notes": "literal {wave_id} must survive"
        });

        let out = render_spec_developer_instructions("wave-abc", Some(&workflow), Some(&input));

        assert!(
            !out.contains("## Bound Workflow Instructions")
                && !out.contains("## Bound Workflow Plan Template")
                && !out.contains("## Bound Workflow Gates"),
            "input injection must not revive the retired plan-prose sections"
        );
        let input_at = out
            .find("## Bound Workflow Input")
            .expect("workflow input section");
        let rendered_input = out[input_at..]
            .strip_prefix("## Bound Workflow Input\n```json\n")
            .and_then(|section| section.strip_suffix("\n```"))
            .expect("workflow input section must be a trailing JSON fence");
        let rendered_input: Value =
            serde_json::from_str(rendered_input).expect("workflow input must be valid JSON");
        assert_eq!(
            rendered_input, input,
            "workflow input must round-trip in full"
        );
        // User JSON is injected verbatim — no `{wave_id}` template substitution.
        assert!(out.contains("literal {wave_id} must survive"));

        // input = None renders no section at all.
        let without = render_spec_developer_instructions("wave-abc", Some(&workflow), None);
        assert!(!without.contains("## Bound Workflow Input"));
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
        let wave = make_wave(repo.as_ref(), None, None).await;

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
                    wave_id: wave.id.clone(),
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

            let runtime_id = new_id();
            let thread_id = format!("thread-{}", card.id.as_str());
            let mut snapshot = base.clone();
            snapshot.last_thread_id = Some(thread_id.clone());
            session_start_runtime_tx(
                &mut tx,
                WorkerSessionInit {
                    id: runtime_id.clone(),
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
            expected.push(runtime_id);
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
    async fn bound_workflow_descriptor_filters_running_trusted_workflow_binding() {
        let trusted_plugin_id = configured_trusted_plugin_id();
        let untrusted_plugin_id = untrusted_plugin_id(&trusted_plugin_id);
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite repo"),
        );
        let bound_input = json!({ "issue_url": "https://github.com/o/r/issues/1" });
        let bound_wave =
            make_wave(repo.as_ref(), Some(WORKFLOW_ID), Some(bound_input.clone())).await;
        let unbound_wave = make_wave(repo.as_ref(), None, None).await;

        let (trusted_running_host, trusted_running_tmp) =
            plugin_host_with_workflow(repo.clone(), &trusted_plugin_id, true).await;
        trusted_running_host
            .spawn(&trusted_plugin_id)
            .await
            .expect("spawn trusted plugin");
        wait_for_running(&trusted_running_host, &trusted_plugin_id).await;
        let trusted_running_adapter = adapter_for(repo.clone(), trusted_running_host.clone());
        let bound = trusted_running_adapter
            .bound_workflow(bound_wave.id.as_str())
            .await
            .expect("resolve trusted running descriptor")
            .expect("bound workflow");
        assert_eq!(bound.descriptor.id, WORKFLOW_ID);
        // The wave row's persisted template_input rides along with the descriptor.
        assert_eq!(bound.input.as_ref(), Some(&bound_input));

        let (trusted_stopped_host, _trusted_stopped_tmp) =
            plugin_host_with_workflow(repo.clone(), &trusted_plugin_id, false).await;
        let trusted_stopped_adapter = adapter_for(repo.clone(), trusted_stopped_host);
        assert!(
            trusted_stopped_adapter
                .bound_workflow(bound_wave.id.as_str())
                .await
                .expect("trusted stopped lookup")
                .is_none()
        );

        let (untrusted_running_host, untrusted_running_tmp) =
            plugin_host_with_workflow(repo.clone(), &untrusted_plugin_id, true).await;
        untrusted_running_host
            .spawn(&untrusted_plugin_id)
            .await
            .expect("spawn untrusted plugin");
        wait_for_running(&untrusted_running_host, &untrusted_plugin_id).await;
        let untrusted_running_adapter = adapter_for(repo.clone(), untrusted_running_host.clone());
        assert!(
            untrusted_running_adapter
                .bound_workflow(bound_wave.id.as_str())
                .await
                .expect("untrusted running lookup")
                .is_none()
        );

        assert!(
            trusted_running_adapter
                .bound_workflow(unbound_wave.id.as_str())
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
        let mut candidate = "dev.neige.untrusted-workflow-test".to_string();
        let mut suffix = 0;
        while candidate == trusted_plugin_id || trusted_forge_plugin(&candidate) {
            suffix += 1;
            candidate = format!("dev.neige.untrusted-workflow-test-{suffix}");
        }
        candidate
    }

    async fn make_wave(
        repo: &SqlxRepo,
        template_id: Option<&str>,
        template_input: Option<serde_json::Value>,
    ) -> crate::model::Wave {
        let cove = repo
            .cove_create(NewCove {
                name: format!("cove-{template_id:?}"),
                color: "#101010".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        repo.wave_create(NewWave {
            template_input,
            cove_id: cove.id,
            title: "workflow resolver".into(),
            sort: None,
            cwd: String::new(),
            template_id: template_id.map(str::to_string),
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave")
    }

    async fn plugin_host_with_workflow(
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
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Workflow Resolver Stub",
            "entrypoint": { "command": "bin/stub" },
            "workflows": [
                { "id": WORKFLOW_ID }
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
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        ));
        (host, tmp)
    }

    fn adapter_for(repo: Arc<SqlxRepo>, plugin: Arc<PluginHost>) -> SpecHarnessStartAdapter {
        let repo_dyn: Arc<dyn Repo> = repo;
        SpecHarnessStartAdapter::new(
            repo_dyn.clone(),
            SharedCodexAppServer::new_stub(repo_dyn),
            HarnessRegistry::new(),
            plugin,
            CardRoleCache::new(),
            WaveCoveCache::new(),
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
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        ));
        let adapter = adapter_for(repo.clone(), host);
        let reason = format!(
            "spec card conv-1 reuses thread t-1 with {REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR} (re-run to mint a fresh thread)"
        );
        assert!(
            is_reusable_thread_missing_card_mcp_token_failure(&reason),
            "test reason must hit the zero-step arm"
        );

        let mut output = TxOutput::new("card", Some("conv-1".into()), json!({}));
        output.data = json!({
            "card_id": "conv-1",
            "wave_id": "wave-1",
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
        assert_eq!(with_seed.steps[0].args["wave_id"], json!("wave-1"));

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
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        ));
        let adapter = adapter_for(repo.clone(), host);

        let mut output = TxOutput::new("card", Some("conv-1".into()), json!({}));
        output.data = json!({
            "card_id": "conv-1",
            "wave_id": "wave-1",
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
        adapter: &SpecHarnessStartAdapter,
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
        adapter: &SpecHarnessStartAdapter,
        from_phase: PhaseTag,
        output: &TxOutput,
        reason: &str,
        create_card: bool,
    ) -> CompensationStateVersioned {
        let payload = serde_json::to_value(SpecHarnessStartOperationPayload {
            actor: ActorId::User,
            wave_id: "wave-1".into(),
            spec_card_id: CardId::from("conv-1"),
            report_card_id: None,
            sort: None,
            cwd: "/tmp".into(),
            goal: None,
            reset_harness_items: false,
            force_new_thread: false,
            profile: HarnessProfile::PlainChat,
            create_card: create_card.then(LazyMintCardSeed::default),
            first_message_sha256: None,
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
            kind: "spec-harness-start".into(),
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
