//! Persisted event shapes, scopes, metadata, and subscription topics.

use crate::harness::HarnessPhaseTag;
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::{Area, Card, Overlay, Track, TrackLifecycle};
use crate::proposal::{ProposalDecision, ProposalOp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Deref;
use ts_rs::TS;

/// One report-block identity captured in a task context freeze.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TaskContextRef {
    pub track_id: TrackId,
    pub block_id: String,
    pub rev: i64,
    pub hash: String,
    #[serde(default)]
    pub is_root: bool,
}

/// One changed frozen reference carried by a context-advance verdict.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TaskContextChangedRef {
    #[serde(default)]
    pub track_id: TrackId,
    #[serde(default)]
    pub block_id: String,
    #[serde(default)]
    pub from_rev: i64,
    #[serde(default)]
    pub to_rev: i64,
    #[serde(default)]
    pub from_hash: String,
    #[serde(default)]
    pub to_hash: String,
}

/// Opaque identifier for a worker-produced artifact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ArtifactRef(pub String);

impl ArtifactRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ArtifactRef {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ArtifactRef {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for ArtifactRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// Payload for `Event::TrackUpdated`; `track` is flattened to preserve the historical wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TrackUpdatedPayload {
    #[serde(flatten)]
    pub track: Track,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_message: Option<String>,
}

impl TrackUpdatedPayload {
    pub fn new(track: Track, agent_message: Option<String>) -> Self {
        Self {
            track,
            agent_message,
        }
    }
}

impl Deref for TrackUpdatedPayload {
    type Target = Track;

    fn deref(&self) -> &Self::Target {
        &self.track
    }
}

impl AsRef<str> for ArtifactRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Producer of a track-report edit. Existing variants are persisted wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum EditAuthor {
    Planner,
    User,
    /// A track-scoped assistant conversation (`CardRole::Assistant`).
    Assistant,
    /// Server-internal rewrite — FSM scaffolding, migrations, etc.
    Kernel,
    /// Historical proposal-channel apply author.
    Plugin,
}

impl EditAuthor {
    /// The bare-lowercase spelling this variant takes on the wire, taken from the derived `Serialize` impl.
    pub fn wire_str(self) -> String {
        serde_json::to_value(self)
            .expect("EditAuthor serializes")
            .as_str()
            .expect("EditAuthor is a unit variant, i.e. a JSON string")
            .to_string()
    }
}

/// Where an event lives in the area → track → card hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "id")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum EventScope {
    /// No entity scope — server-internal or cross-entity event.
    System,
    /// Scoped to one area. No track or card context.
    Area { area: AreaId },
    /// Scoped to one track. Carries the owning area for filter ergonomics.
    Track { track: TrackId, area: AreaId },
    /// Scoped to one card. Carries track + area for the same reason.
    Card {
        card: CardId,
        track: TrackId,
        area: AreaId,
    },
}

impl EventScope {
    /// String discriminator stored in `events.scope_kind`; stable, changing it breaks replay.
    pub fn kind(&self) -> &'static str {
        match self {
            EventScope::System => "system",
            EventScope::Area { .. } => "area",
            EventScope::Track { .. } => "track",
            EventScope::Card { .. } => "card",
        }
    }

    pub fn area_id(&self) -> Option<&AreaId> {
        match self {
            EventScope::System => None,
            EventScope::Area { area } => Some(area),
            EventScope::Track { area, .. } => Some(area),
            EventScope::Card { area, .. } => Some(area),
        }
    }

    /// Owning track id, if the scope is track-or-narrower.
    pub fn track_id(&self) -> Option<&TrackId> {
        match self {
            EventScope::System | EventScope::Area { .. } => None,
            EventScope::Track { track, .. } => Some(track),
            EventScope::Card { track, .. } => Some(track),
        }
    }

    /// Card id, only for the card scope.
    pub fn card_id(&self) -> Option<&CardId> {
        match self {
            EventScope::Card { card, .. } => Some(card),
            _ => None,
        }
    }

    /// Malformed or incomplete rows fall back to `System` so replay continues.
    pub fn from_row(
        kind: Option<&str>,
        area: Option<&str>,
        track: Option<&str>,
        card: Option<&str>,
    ) -> EventScope {
        match kind.unwrap_or("system") {
            "area" => match area {
                Some(c) => EventScope::Area {
                    area: AreaId::from(c),
                },
                None => EventScope::System,
            },
            "track" => match (track, area) {
                (Some(w), Some(c)) => EventScope::Track {
                    track: TrackId::from(w),
                    area: AreaId::from(c),
                },
                _ => EventScope::System,
            },
            "card" => match (card, track, area) {
                (Some(card), Some(w), Some(c)) => EventScope::Card {
                    card: CardId::from(card),
                    track: TrackId::from(w),
                    area: AreaId::from(c),
                },
                _ => EventScope::System,
            },
            _ => EventScope::System,
        }
    }
}

/// Sync-engine event envelope version. Bump together with a migration default whenever clients
/// must gate on a new persisted wire shape.
pub const SYNC_EVENT_VERSION: u32 = 20;

/// What happened to one entry in the harness pending queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum HarnessQueueChange {
    /// A human rewrote the entry's text through `PATCH /api/cards/{id}/planner/input/{entry_id}`.
    Edited,
    /// A human removed the entry through `DELETE /api/cards/{id}/planner/input/{entry_id}`.
    Deleted,
    /// The entry left the queue because codex took it into the running turn (`turn/steer`); a steer
    /// codex refused is announced as `Restored`, not as this.
    Steered,
    /// The entry is back in the queue, at the head, with the id it left with.
    Restored,
    /// The kernel discarded the entry without delivering it: a snapshot loaded with more than
    /// `MAX_PENDING_QUEUE_LEN` entries drops from the head.
    Dropped,
}

/// Phase/slice PR identity carried by `forge.pr.merged`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ForgeMergeSubject {
    pub phase: String,
    pub slice_id: String,
    pub pr_number: u64,
}

/// Logical review subject key for `review.round`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ReviewSubject {
    pub phase: String,
    pub slice_id: String,
    pub pr_number: Option<u64>,
}

/// Per-channel verdict recorded on a `review.round`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ChannelVerdict {
    pub role: String,
    pub verdict: ChannelVerdictKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ChannelVerdictKind {
    Approved,
    ChangesRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum RatifyDecision {
    Grant,
    Deny,
}

/// The full set of WS event envelopes the kernel emits on `/api/events`. ts-rs requires every
/// payload type referenced here to also derive `TS`.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(tag = "ev", content = "data")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum Event {
    #[serde(rename = "area.updated")]
    AreaUpdated(Area),
    #[serde(rename = "area.deleted")]
    AreaDeleted { id: AreaId },

    #[serde(rename = "track.updated")]
    TrackUpdated(TrackUpdatedPayload),
    #[serde(rename = "track.deleted")]
    TrackDeleted { id: TrackId, area_id: AreaId },

    /// Explicit Track lifecycle transition, emitted exactly once per validated `from → to` change.
    #[serde(rename = "track.lifecycle_changed")]
    TrackLifecycleChanged {
        id: TrackId,
        area_id: AreaId,
        from: TrackLifecycle,
        to: TrackLifecycle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    #[serde(rename = "card.added")]
    CardAdded(Card),
    #[serde(rename = "card.updated")]
    CardUpdated(Card),
    #[serde(rename = "card.deleted")]
    CardDeleted { id: CardId, track_id: TrackId },

    #[serde(rename = "worker_session.started")]
    WorkerSessionStarted {
        worker_session_id: String,
        card_id: String,
        kind: crate::runtime::WorkerSessionKind,
        agent_provider: Option<crate::runtime::AgentProvider>,
        status: crate::worker::WorkerSessionState,
    },
    #[serde(rename = "worker_session.status_changed")]
    WorkerSessionStatusChanged {
        worker_session_id: String,
        card_id: String,
        old_status: crate::worker::WorkerSessionState,
        new_status: crate::worker::WorkerSessionState,
    },
    #[serde(rename = "worker_session.superseded")]
    WorkerSessionSuperseded {
        old_worker_session_id: String,
        new_worker_session_id: String,
        card_id: String,
    },
    #[serde(rename = "harness.item.added")]
    HarnessItemAdded {
        worker_session_id: String,
        card_id: CardId,
        track_id: TrackId,
        item_db_id: i64,
        item_uuid: Option<String>,
        item_type: Option<String>,
        turn_id: Option<String>,
        method: String,
    },
    #[serde(rename = "harness.phase.changed")]
    HarnessPhaseChanged {
        worker_session_id: String,
        card_id: CardId,
        track_id: TrackId,
        old_phase: HarnessPhaseTag,
        new_phase: HarnessPhaseTag,
    },
    /// The harness transcript reset hard-deletes every `harness_items` row for the card, so this event
    /// is the only surviving evidence of what was there. The measurements are `Option` so rows written
    /// before they existed still replay: `None` means unmeasured, `Some(0)` means measured and empty.
    #[serde(rename = "harness.transcript.cleared")]
    HarnessTranscriptCleared {
        worker_session_id: String,
        card_id: CardId,
        track_id: TrackId,
        /// Number of `harness_items` rows deleted by this reset; `None` on unmeasured historical rows.
        #[serde(default)]
        cleared_item_count: Option<i64>,
        /// Summed byte length of those rows' `params` payloads; `None` on unmeasured historical rows.
        #[serde(default)]
        cleared_params_bytes: Option<i64>,
        /// Card age at reset in milliseconds; `None` on unmeasured historical rows.
        #[serde(default)]
        card_age_ms_at_clear: Option<i64>,
    },
    /// Emitted when `POST /api/cards/{id}/planner/input` queues a user-authored text observation onto
    /// the planner harness. Body text is intentionally not on the payload; only `char_count` is.
    #[serde(rename = "harness.user_message.enqueued")]
    HarnessUserMessageEnqueued {
        worker_session_id: String,
        card_id: CardId,
        track_id: TrackId,
        char_count: u32,
    },

    /// One addressable entry in the planner harness pending queue stopped being what it was: rewritten,
    /// removed by its author, delivered by a steer, or discarded by the kernel. `actor` duplicates the
    /// envelope's actor column on purpose: the `{ev, data}` WS frame does not carry it.
    #[serde(rename = "harness.queue.changed")]
    HarnessQueueChanged {
        worker_session_id: String,
        card_id: CardId,
        track_id: TrackId,
        entry_id: String,
        change: HarnessQueueChange,
        actor: ActorId,
    },

    /// Structured track-report edit-log entry, emitted alongside `Event::CardUpdated` from every
    /// successful report persist. `*_before` / `*_after` are the projected text values around the update.
    #[serde(rename = "track.report_edited")]
    TrackReportEdited {
        track_id: TrackId,
        card_id: CardId,
        author: EditAuthor,
        /// Submitting plugin id when `author == EditAuthor::Plugin`; `None` for every other author.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        author_plugin_id: Option<String>,
        edit_id: String,
        summary_before: String,
        summary_after: String,
        body_before: String,
        body_after: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    #[serde(rename = "overlay.set")]
    OverlaySet(Overlay),
    #[serde(rename = "overlay.deleted")]
    OverlayDeleted {
        plugin_id: String,
        entity_kind: String,
        entity_id: String,
        kind: String,
    },

    /// Terminal row removed. Carries the terminal id plus the card_id the row pointed at.
    #[serde(rename = "terminal.deleted")]
    TerminalDeleted { id: String, card_id: CardId },

    #[serde(rename = "plugin.state")]
    PluginState {
        id: String,
        state: String,
        /// Crash reason / initialize-rejected message, surfaced to the WS so the UI can show it without a
        /// separate `/log` fetch; `None` for healthy transitions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_error: Option<String>,
    },
    #[serde(rename = "plugin.tool.registered")]
    PluginToolRegistered {
        plugin_id: String,
        tool_name: String,
    },

    /// Codex CLI hook passthrough; the payload is intentionally opaque (`Value`).
    #[serde(rename = "codex.hook")]
    CodexHook {
        /// Owning card id — topic key `card:<card_id>`.
        card_id: CardId,
        /// Snake_case discriminator: `hook.codex.<event_name>`; defaults to `hook.codex.unknown` if missing.
        kind: String,
        /// Stable hook ingest key used by the server and planner harness to
        /// suppress duplicate lifecycle posts.
        #[serde(default)]
        hook_idempotency_key: String,
        /// Original codex hook JSON, verbatim.
        #[ts(type = "unknown")]
        payload: Value,
    },

    /// Claude CLI hook passthrough. Same shape as [`Event::CodexHook`].
    #[serde(rename = "claude.hook")]
    ClaudeHook {
        /// Owning card id — topic key `card:<card_id>`.
        card_id: CardId,
        /// Hook discriminator supplied by the future Claude hook route.
        kind: String,
        /// Stable hook ingest key used by the server and planner harness to
        /// suppress duplicate lifecycle posts.
        #[serde(default)]
        hook_idempotency_key: String,
        /// Original Claude hook JSON, verbatim.
        #[ts(type = "unknown")]
        payload: Value,
    },

    /// Deprecated: retained for old-log deserialization only. Planner/worker card asked the kernel
    /// dispatcher to spawn a codex worker card.
    #[serde(rename = "codex.worker_requested", alias = "codex.job_requested")]
    CodexWorkerRequested {
        idempotency_key: String,
        goal: String,
        #[ts(type = "unknown")]
        context: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        acceptance_criteria: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Deprecated: retained for old-log deserialization only. Planner card asked the kernel dispatcher
    /// to spawn a terminal worker card.
    #[serde(rename = "terminal.worker_requested", alias = "terminal.job_requested")]
    TerminalWorkerRequested {
        idempotency_key: String,
        cmd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Worker card reports task completion; `idempotency_key` echoes the matching `*.worker_requested` event.
    #[serde(rename = "task.completed")]
    TaskCompleted {
        idempotency_key: String,
        #[ts(type = "unknown")]
        result: Value,
        artifacts: Vec<ArtifactRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Worker card reports task failure; `reason` is free-form and never parsed by the kernel.
    #[serde(rename = "task.failed")]
    TaskFailed {
        idempotency_key: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        #[ts(type = "unknown")]
        details: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Exact immutable-file publication settled; does not change task business status.
    #[serde(rename = "task.file_publication_settled")]
    TaskFilePublicationSettled {
        task_id: String,
        operation_id: String,
    },
    /// Exact candidate verification settled; result must be read before qualification.
    #[serde(rename = "task.candidate_verification_settled")]
    TaskCandidateVerificationSettled {
        task_id: String,
        operation_id: String,
    },
    /// Persisted after confirmed stop with a failed execution or a terminal Done Reviewer Operation.
    #[serde(rename = "task.execution_settled")]
    TaskExecutionSettled {
        task_id: String,
        operation_id: String,
    },

    /// The task plan changed via an explicit plan tool or report-block projection; `changed_keys` is
    /// the sorted, deduplicated union of inserted, updated, and deleted rows.
    #[serde(rename = "plan.updated")]
    PlanUpdated {
        track_id: TrackId,
        changed_keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// The kernel scheduler claimed a plan task (`pending → dispatched`); appended inside the claim tx
    /// so the runs projection stays purely event-sourced.
    #[serde(rename = "task.dispatched")]
    TaskDispatched {
        idempotency_key: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// The kernel froze the task's resolved report-block context in the same transaction as
    /// `task.dispatched`; an empty `refs` array is an explicit freeze, not missing context.
    #[serde(rename = "task.context_frozen")]
    TaskContextFrozen {
        #[serde(default)]
        track_id: TrackId,
        #[serde(default)]
        task_key: String,
        #[serde(default)]
        idempotency_key: String,
        task_id: String,
        refs: Vec<TaskContextRef>,
        #[serde(default)]
        doc_revs: BTreeMap<String, u64>,
        #[serde(default)]
        truncated: bool,
    },

    /// The kernel recorded that a frozen task context advanced.
    #[serde(rename = "task.context_advanced")]
    TaskContextAdvanced {
        #[serde(default)]
        track_id: TrackId,
        #[serde(default)]
        task_key: String,
        task_id: String,
        #[serde(default)]
        changed_refs: Vec<TaskContextChangedRef>,
        verdict: String,
        #[serde(default)]
        rationale: String,
    },

    /// The kernel acquired a workflow-agnostic isolated workspace lease for a Codex task.
    #[serde(rename = "workspace.leased")]
    WorkspaceLeased {
        track_id: TrackId,
        card_id: CardId,
        lease_id: String,
        path: String,
    },

    /// The kernel released a workspace lease after worker completion, compensation, or boot reclaim.
    #[serde(rename = "workspace.released")]
    WorkspaceReleased {
        track_id: TrackId,
        card_id: CardId,
        lease_id: String,
    },

    /// A forge adapter merged the authoritative phase/slice PR for a track.
    #[serde(rename = "forge.pr.merged")]
    ForgePrMerged {
        track_id: TrackId,
        subject: ForgeMergeSubject,
        head_sha: String,
        merge_sha: String,
    },
    #[serde(rename = "review.round")]
    ReviewRound {
        track_id: TrackId,
        subject: ReviewSubject,
        head_sha: Option<String>,
        n: u32,
        cap: u32,
        converged: bool,
        channels: Vec<ChannelVerdict>,
        root_cause: Option<String>,
        idempotency_key: String,
    },
    #[serde(rename = "ratify.requested")]
    RatifyRequested { track_id: TrackId, reason: String },
    #[serde(rename = "ratify.resolved")]
    RatifyResolved {
        track_id: TrackId,
        decision: RatifyDecision,
    },

    /// A plugin submitted a report-edit proposal. Append-only record: the full op list + anchors ride
    /// on the payload so the pending set can be rebuilt from the event log alone.
    #[serde(rename = "proposal.submitted")]
    ProposalSubmitted {
        track_id: TrackId,
        proposal_id: String,
        /// Submitting plugin, injected kernel-side from the callback connection (never trusted from plugin input).
        plugin_id: String,
        /// Proposal subject kind — `"report"` is the only accepted value today.
        subject_kind: String,
        /// Opaque Automerge canonical-heads token of the snapshot the plugin proposed against.
        base_doc_heads: String,
        ops: Vec<ProposalOp>,
        /// Human-facing rationale rendered in the adjudication UI.
        note: String,
        /// Pending-scoped idempotency key: re-submits while pending return the original proposal id;
        /// resolution releases the key.
        idem_key: String,
    },
    /// A pending proposal reached one of its four terminal decisions; `plugin_id` is the SUBMITTER, not the resolver.
    #[serde(rename = "proposal.resolved")]
    ProposalResolved {
        track_id: TrackId,
        proposal_id: String,
        plugin_id: String,
        decision: ProposalDecision,
    },
    #[serde(rename = "forge.scan.completed")]
    ForgeScanCompleted {
        track_id: TrackId,
        overlapping_prs: Vec<u64>,
    },
    #[serde(rename = "forge.pr.opened")]
    ForgePrOpened {
        track_id: TrackId,
        pr_number: u64,
        head_sha: String,
    },
    #[serde(rename = "forge.pr.diff.read")]
    ForgePrDiffRead {
        track_id: TrackId,
        pr_number: u64,
        base_sha: String,
        head_sha: String,
        artifact_path: String,
    },
    #[serde(rename = "forge.pr.checks")]
    ForgePrChecks {
        track_id: TrackId,
        pr_number: u64,
        conclusion: String,
    },
    #[serde(rename = "forge.issue.read")]
    ForgeIssueRead {
        track_id: TrackId,
        issue_number: u64,
        artifact_path: String,
    },
    #[serde(rename = "forge.issue.closed")]
    ForgeIssueClosed {
        track_id: TrackId,
        issue_number: u64,
    },
    #[serde(rename = "worktree.provisioned")]
    WorktreeProvisioned {
        track_id: TrackId,
        card_id: CardId,
        path: String,
    },
    #[serde(rename = "worktree.committed")]
    WorktreeCommitted {
        track_id: TrackId,
        card_id: CardId,
        commit_sha: String,
        branch: String,
    },
    #[serde(rename = "worktree.removed")]
    WorktreeRemoved {
        track_id: TrackId,
        card_id: CardId,
        path: String,
    },

    /// The kernel `task-verify` runner completed an attempt and recorded its verdict; actor is always
    /// `ActorId::KernelDispatcher` so it is never classified as a planner verdict.
    #[serde(rename = "task.gate_result")]
    TaskGateResult {
        task_id: String,
        idempotency_key: String,
        passed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        failing_step: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        exit_code: Option<i32>,
        log_tail: String,
        log_path: String,
        attempt: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },
}

/// Bounded typed result-extraction contract: only a target event kind + named field reads, not a predicate DSL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeEventSpec {
    pub event_kind: String,
    pub fields: std::collections::BTreeMap<String, FieldSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    ExitCode,
    JsonField { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForgeExtractError {
    #[error("forge event spec requires JSON stdout but none was provided")]
    MissingJsonStdout,
    #[error("forge event spec field `{field}` pointer `{path}` did not resolve")]
    PointerUnresolved { field: String, path: String },
}

impl ForgeEventSpec {
    /// Build the event `data` payload map from the action's exit code and optional --json stdout;
    /// strict-fail on an unresolved pointer.
    pub fn extract_payload(
        &self,
        exit_code: i32,
        json_stdout: Option<&serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ForgeExtractError> {
        let needs_json = self
            .fields
            .values()
            .any(|source| matches!(source, FieldSource::JsonField { .. }));
        if needs_json && json_stdout.is_none() {
            return Err(ForgeExtractError::MissingJsonStdout);
        }

        let mut payload = serde_json::Map::new();
        for (field, source) in &self.fields {
            match source {
                FieldSource::ExitCode => {
                    payload.insert(field.clone(), serde_json::json!(exit_code));
                }
                FieldSource::JsonField { path } => {
                    let value =
                        json_stdout
                            .and_then(|json| json.pointer(path))
                            .ok_or_else(|| ForgeExtractError::PointerUnresolved {
                                field: field.clone(),
                                path: path.clone(),
                            })?;
                    payload.insert(field.clone(), value.clone());
                }
            }
        }
        Ok(payload)
    }
}

/// Central event-classifier result for the kernel's event surfaces. Keep the producing match in
/// [`Event::metadata`] exhaustive so adding a variant forces an explicit classifier decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMetadata {
    pub kind_tag: &'static str,
    pub plugin_id: Option<String>,
    pub entity_kind: Option<String>,
    pub entity_id: Option<String>,
}

impl Event {
    /// Centralized event classifier surface used by persistence and plugin subscription filters; keep exhaustive.
    pub fn metadata(&self) -> EventMetadata {
        let kind_tag = self.kind_tag();
        match self {
            Event::AreaUpdated(c) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: Some(c.id.to_string()),
            },
            Event::AreaDeleted { id } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: Some(id.to_string()),
            },
            Event::TrackUpdated(w) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("track".into()),
                entity_id: Some(w.id.to_string()),
            },
            Event::TrackDeleted { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("track".into()),
                entity_id: Some(id.to_string()),
            },
            Event::TrackLifecycleChanged { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("track".into()),
                entity_id: Some(id.to_string()),
            },
            Event::CardAdded(c) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(c.id.to_string()),
            },
            Event::CardUpdated(c) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(c.id.to_string()),
            },
            Event::CardDeleted { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(id.to_string()),
            },
            Event::WorkerSessionStarted { card_id, .. }
            | Event::WorkerSessionStatusChanged { card_id, .. }
            | Event::WorkerSessionSuperseded { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::HarnessItemAdded { card_id, .. }
            | Event::HarnessPhaseChanged { card_id, .. }
            | Event::HarnessTranscriptCleared { card_id, .. }
            | Event::HarnessUserMessageEnqueued { card_id, .. }
            | Event::HarnessQueueChanged { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::TrackReportEdited { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::OverlaySet(o) => EventMetadata {
                kind_tag,
                plugin_id: Some(o.plugin_id.clone()),
                entity_kind: Some(o.entity_kind.clone()),
                entity_id: Some(o.entity_id.clone()),
            },
            Event::OverlayDeleted {
                plugin_id,
                entity_kind,
                entity_id,
                ..
            } => EventMetadata {
                kind_tag,
                plugin_id: Some(plugin_id.clone()),
                entity_kind: Some(entity_kind.clone()),
                entity_id: Some(entity_id.clone()),
            },
            Event::TerminalDeleted { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: Some(id.clone()),
            },
            Event::PluginState { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: Some(id.clone()),
                entity_kind: None,
                entity_id: Some(id.clone()),
            },
            Event::PluginToolRegistered { plugin_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: Some(plugin_id.clone()),
                entity_kind: None,
                entity_id: Some(plugin_id.clone()),
            },
            Event::CodexHook { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::ClaudeHook { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::CodexWorkerRequested { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TerminalWorkerRequested { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TaskCompleted { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TaskFailed { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::PlanUpdated { track_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("track".into()),
                entity_id: Some(track_id.to_string()),
            },
            Event::TaskDispatched { .. }
            | Event::TaskExecutionSettled { .. }
            | Event::TaskCandidateVerificationSettled { .. }
            | Event::TaskFilePublicationSettled { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TaskContextFrozen { .. } | Event::TaskContextAdvanced { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::WorkspaceLeased { card_id, .. } | Event::WorkspaceReleased { card_id, .. } => {
                EventMetadata {
                    kind_tag,
                    plugin_id: None,
                    entity_kind: Some("card".into()),
                    entity_id: Some(card_id.to_string()),
                }
            }
            Event::ForgePrMerged { track_id, .. }
            | Event::ReviewRound { track_id, .. }
            | Event::RatifyRequested { track_id, .. }
            | Event::RatifyResolved { track_id, .. }
            | Event::ForgeScanCompleted { track_id, .. }
            | Event::ForgePrOpened { track_id, .. }
            | Event::ForgePrDiffRead { track_id, .. }
            | Event::ForgePrChecks { track_id, .. }
            | Event::ForgeIssueRead { track_id, .. }
            | Event::ForgeIssueClosed { track_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("track".into()),
                entity_id: Some(track_id.to_string()),
            },
            Event::ProposalSubmitted {
                track_id,
                plugin_id,
                ..
            }
            | Event::ProposalResolved {
                track_id,
                plugin_id,
                ..
            } => EventMetadata {
                kind_tag,
                plugin_id: Some(plugin_id.clone()),
                entity_kind: Some("track".into()),
                entity_id: Some(track_id.to_string()),
            },
            Event::WorktreeProvisioned { card_id, .. }
            | Event::WorktreeCommitted { card_id, .. }
            | Event::WorktreeRemoved { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::TaskGateResult { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
        }
    }

    /// String tag for the events-table `kind` column; matches the `#[serde(rename = "...")]` on each variant.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Event::AreaUpdated(_) => "area.updated",
            Event::AreaDeleted { .. } => "area.deleted",
            Event::TrackUpdated(_) => "track.updated",
            Event::TrackDeleted { .. } => "track.deleted",
            Event::TrackLifecycleChanged { .. } => "track.lifecycle_changed",
            Event::CardAdded(_) => "card.added",
            Event::CardUpdated(_) => "card.updated",
            Event::CardDeleted { .. } => "card.deleted",
            Event::WorkerSessionStarted { .. } => "worker_session.started",
            Event::WorkerSessionStatusChanged { .. } => "worker_session.status_changed",
            Event::WorkerSessionSuperseded { .. } => "worker_session.superseded",
            Event::HarnessItemAdded { .. } => "harness.item.added",
            Event::HarnessPhaseChanged { .. } => "harness.phase.changed",
            Event::HarnessTranscriptCleared { .. } => "harness.transcript.cleared",
            Event::HarnessUserMessageEnqueued { .. } => "harness.user_message.enqueued",
            Event::HarnessQueueChanged { .. } => "harness.queue.changed",
            Event::TrackReportEdited { .. } => "track.report_edited",
            Event::OverlaySet(_) => "overlay.set",
            Event::OverlayDeleted { .. } => "overlay.deleted",
            Event::TerminalDeleted { .. } => "terminal.deleted",
            Event::PluginState { .. } => "plugin.state",
            Event::PluginToolRegistered { .. } => "plugin.tool.registered",
            Event::CodexHook { .. } => "codex.hook",
            Event::ClaudeHook { .. } => "claude.hook",
            Event::CodexWorkerRequested { .. } => "codex.worker_requested",
            Event::TerminalWorkerRequested { .. } => "terminal.worker_requested",
            Event::TaskCompleted { .. } => "task.completed",
            Event::TaskFailed { .. } => "task.failed",
            Event::PlanUpdated { .. } => "plan.updated",
            Event::TaskDispatched { .. } => "task.dispatched",
            Event::TaskExecutionSettled { .. } => "task.execution_settled",
            Event::TaskFilePublicationSettled { .. } => "task.file_publication_settled",
            Event::TaskCandidateVerificationSettled { .. } => "task.candidate_verification_settled",
            Event::TaskContextFrozen { .. } => "task.context_frozen",
            Event::TaskContextAdvanced { .. } => "task.context_advanced",
            Event::WorkspaceLeased { .. } => "workspace.leased",
            Event::WorkspaceReleased { .. } => "workspace.released",
            Event::ForgePrMerged { .. } => "forge.pr.merged",
            Event::ReviewRound { .. } => "review.round",
            Event::RatifyRequested { .. } => "ratify.requested",
            Event::RatifyResolved { .. } => "ratify.resolved",
            Event::ProposalSubmitted { .. } => "proposal.submitted",
            Event::ProposalResolved { .. } => "proposal.resolved",
            Event::ForgeScanCompleted { .. } => "forge.scan.completed",
            Event::ForgePrOpened { .. } => "forge.pr.opened",
            Event::ForgePrDiffRead { .. } => "forge.pr.diff.read",
            Event::ForgePrChecks { .. } => "forge.pr.checks",
            Event::ForgeIssueRead { .. } => "forge.issue.read",
            Event::ForgeIssueClosed { .. } => "forge.issue.closed",
            Event::WorktreeProvisioned { .. } => "worktree.provisioned",
            Event::WorktreeCommitted { .. } => "worktree.committed",
            Event::WorktreeRemoved { .. } => "worktree.removed",
            Event::TaskGateResult { .. } => "task.gate_result",
        }
    }

    /// Extract just the `data` payload, so the events table persists the bare payload, not the `{ev, data}` envelope.
    pub fn payload_value(&self) -> serde_json::Value {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(mut map)) => {
                map.remove("data").unwrap_or(serde_json::Value::Null)
            }
            _ => serde_json::Value::Null,
        }
    }

    /// Rebuild a typed `Event` from the `(kind, payload)` pair stored in the `events` table.
    pub fn from_kind_and_payload(
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        let envelope = serde_json::json!({ "ev": kind, "data": payload });
        serde_json::from_value(envelope)
    }
}

/// Subscription topics an `Event` matches; the WS handler intersects this with each client's `sub` filter.
/// Grammar (mirrored in the frontend): `area:<id>`, `track:<id>`, `card:<id>`, `plugin:<id>`, `plugin:*`, `*`.
pub fn topics(ev: &Event) -> Vec<String> {
    match ev {
        Event::AreaUpdated(c) => vec![format!("area:{}", c.id), "*".into()],
        Event::AreaDeleted { id } => vec![format!("area:{}", id), "*".into()],

        Event::TrackUpdated(w) => vec![
            format!("track:{}", w.id),
            format!("area:{}", w.area_id),
            "*".into(),
        ],
        Event::TrackDeleted { id, area_id } => vec![
            format!("track:{}", id),
            format!("area:{}", area_id),
            "*".into(),
        ],

        Event::TrackLifecycleChanged { id, area_id, .. } => vec![
            format!("track:{}", id),
            format!("area:{}", area_id),
            "*".into(),
        ],

        Event::CardAdded(c) | Event::CardUpdated(c) => vec![
            format!("card:{}", c.id),
            format!("track:{}", c.track_id),
            "*".into(),
        ],
        Event::CardDeleted { id, track_id } => vec![
            format!("card:{}", id),
            format!("track:{}", track_id),
            "*".into(),
        ],
        // Runtime payloads route by card only: they carry no track_id.
        Event::WorkerSessionStarted { card_id, .. }
        | Event::WorkerSessionStatusChanged { card_id, .. }
        | Event::WorkerSessionSuperseded { card_id, .. } => {
            vec![format!("card:{}", card_id), "*".into()]
        }
        Event::HarnessItemAdded {
            track_id, card_id, ..
        }
        | Event::HarnessPhaseChanged {
            track_id, card_id, ..
        }
        | Event::HarnessTranscriptCleared {
            track_id, card_id, ..
        }
        | Event::HarnessUserMessageEnqueued {
            track_id, card_id, ..
        }
        | Event::HarnessQueueChanged {
            track_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
            format!("track:{}", track_id),
            "*".into(),
        ],

        Event::TrackReportEdited {
            track_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
            format!("track:{}", track_id),
            "*".into(),
        ],

        Event::OverlaySet(o) => vec![
            format!("{}:{}", o.entity_kind, o.entity_id),
            format!("plugin:{}", o.plugin_id),
            "plugin:*".into(),
            "*".into(),
        ],
        Event::OverlayDeleted {
            plugin_id,
            entity_kind,
            entity_id,
            ..
        } => vec![
            format!("{}:{}", entity_kind, entity_id),
            format!("plugin:{}", plugin_id),
            "plugin:*".into(),
            "*".into(),
        ],

        Event::TerminalDeleted { id, .. } => vec![format!("terminal:{}", id), "*".into()],

        Event::PluginState { id, .. } => {
            vec![format!("plugin:{}", id), "plugin:*".into(), "*".into()]
        }
        Event::PluginToolRegistered { plugin_id, .. } => {
            vec![
                format!("plugin:{}", plugin_id),
                "plugin:*".into(),
                "*".into(),
            ]
        }

        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => {
            vec![format!("card:{}", card_id), "*".into()]
        }

        Event::CodexWorkerRequested { .. }
        | Event::TerminalWorkerRequested { .. }
        | Event::TaskCompleted { .. }
        | Event::TaskFailed { .. }
        | Event::TaskDispatched { .. }
        | Event::TaskExecutionSettled { .. }
        | Event::TaskCandidateVerificationSettled { .. }
        | Event::TaskFilePublicationSettled { .. }
        | Event::TaskContextFrozen { .. }
        | Event::TaskContextAdvanced { .. }
        | Event::TaskGateResult { .. } => vec!["*".into()],

        Event::WorkspaceLeased {
            track_id, card_id, ..
        }
        | Event::WorkspaceReleased {
            track_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
            format!("track:{}", track_id),
            "*".into(),
        ],

        Event::ForgePrMerged { track_id, .. }
        | Event::ReviewRound { track_id, .. }
        | Event::RatifyRequested { track_id, .. }
        | Event::RatifyResolved { track_id, .. }
        | Event::ForgeScanCompleted { track_id, .. }
        | Event::ForgePrOpened { track_id, .. }
        | Event::ForgePrDiffRead { track_id, .. }
        | Event::ForgePrChecks { track_id, .. }
        | Event::ForgeIssueRead { track_id, .. }
        | Event::ForgeIssueClosed { track_id, .. } => {
            vec![format!("track:{}", track_id), "*".into()]
        }

        Event::ProposalSubmitted {
            track_id,
            plugin_id,
            ..
        }
        | Event::ProposalResolved {
            track_id,
            plugin_id,
            ..
        } => vec![
            format!("track:{}", track_id),
            format!("plugin:{}", plugin_id),
            "plugin:*".into(),
            "*".into(),
        ],

        Event::WorktreeProvisioned {
            track_id, card_id, ..
        }
        | Event::WorktreeCommitted {
            track_id, card_id, ..
        }
        | Event::WorktreeRemoved {
            track_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
            format!("track:{}", track_id),
            "*".into(),
        ],

        Event::PlanUpdated { track_id, .. } => vec![format!("track:{}", track_id), "*".into()],
    }
}
#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn task_context_ref_from_3a_defaults_missing_root_marker_with_nonempty_refs() {
        let refs: Vec<TaskContextRef> = serde_json::from_value(serde_json::json!([{
            "track_id": "w-old",
            "block_id": "b_old",
            "rev": 3,
            "hash": "abc123"
        }]))
        .expect("3a context refs remain readable");
        assert_eq!(refs.len(), 1);
        assert!(!refs[0].is_root);
    }

    #[test]
    fn scope_kind_strings_pinned() {
        // Persisted to `events.scope_kind`; changing them is a wire break.
        assert_eq!(EventScope::System.kind(), "system");
        assert_eq!(
            EventScope::Area {
                area: AreaId::from("c")
            }
            .kind(),
            "area"
        );
        assert_eq!(
            EventScope::Track {
                track: TrackId::from("w"),
                area: AreaId::from("c"),
            }
            .kind(),
            "track"
        );
        assert_eq!(
            EventScope::Card {
                card: CardId::from("k"),
                track: TrackId::from("w"),
                area: AreaId::from("c"),
            }
            .kind(),
            "card"
        );
    }

    #[test]
    fn ancestor_accessors_return_chain() {
        let s = EventScope::Card {
            card: CardId::from("k"),
            track: TrackId::from("w"),
            area: AreaId::from("c"),
        };
        assert_eq!(s.card_id().map(|x| x.as_str()), Some("k"));
        assert_eq!(s.track_id().map(|x| x.as_str()), Some("w"));
        assert_eq!(s.area_id().map(|x| x.as_str()), Some("c"));

        let s = EventScope::Track {
            track: TrackId::from("w"),
            area: AreaId::from("c"),
        };
        assert_eq!(s.card_id(), None);
        assert_eq!(s.track_id().map(|x| x.as_str()), Some("w"));
        assert_eq!(s.area_id().map(|x| x.as_str()), Some("c"));

        let s = EventScope::Area {
            area: AreaId::from("c"),
        };
        assert!(s.card_id().is_none() && s.track_id().is_none());
        assert_eq!(s.area_id().map(|x| x.as_str()), Some("c"));

        let s = EventScope::System;
        assert!(s.card_id().is_none() && s.track_id().is_none() && s.area_id().is_none());
    }

    #[test]
    fn serde_round_trip_all_variants() {
        for s in [
            EventScope::System,
            EventScope::Area {
                area: AreaId::from("c"),
            },
            EventScope::Track {
                track: TrackId::from("w"),
                area: AreaId::from("c"),
            },
            EventScope::Card {
                card: CardId::from("k"),
                track: TrackId::from("w"),
                area: AreaId::from("c"),
            },
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            let back: EventScope = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, s, "round-trip mismatch for {s:?} via {json}");
        }
    }

    #[test]
    fn serde_card_shape_pinned() {
        let s = EventScope::Card {
            card: CardId::from("k"),
            track: TrackId::from("w"),
            area: AreaId::from("c"),
        };
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert_eq!(v["kind"], "Card");
        assert_eq!(v["id"]["card"], "k");
        assert_eq!(v["id"]["track"], "w");
        assert_eq!(v["id"]["area"], "c");

        let v: serde_json::Value = serde_json::to_value(EventScope::System).unwrap();
        assert_eq!(v["kind"], "System");
    }

    #[test]
    fn from_row_recovers_typed_scope() {
        assert_eq!(
            EventScope::from_row(Some("system"), None, None, None),
            EventScope::System,
        );
        assert_eq!(
            EventScope::from_row(Some("area"), Some("c"), None, None),
            EventScope::Area {
                area: AreaId::from("c"),
            },
        );
        assert_eq!(
            EventScope::from_row(Some("track"), Some("c"), Some("w"), None),
            EventScope::Track {
                track: TrackId::from("w"),
                area: AreaId::from("c"),
            },
        );
        assert_eq!(
            EventScope::from_row(Some("card"), Some("c"), Some("w"), Some("k")),
            EventScope::Card {
                card: CardId::from("k"),
                track: TrackId::from("w"),
                area: AreaId::from("c"),
            },
        );
    }

    #[test]
    fn from_row_null_fallback_to_system() {
        assert_eq!(
            EventScope::from_row(None, None, None, None),
            EventScope::System,
        );
        assert_eq!(
            EventScope::from_row(Some("plugin"), None, None, None),
            EventScope::System,
        );
        assert_eq!(
            EventScope::from_row(Some("card"), Some("c"), Some("w"), None),
            EventScope::System,
        );
        assert_eq!(
            EventScope::from_row(Some("track"), Some("c"), None, None),
            EventScope::System,
        );
    }

    #[test]
    fn artifact_ref_transparent_serde() {
        let r = ArtifactRef::from("artifact-1");
        assert_eq!(serde_json::to_string(&r).unwrap(), r#""artifact-1""#);
        let back: ArtifactRef = serde_json::from_str(r#""artifact-1""#).unwrap();
        assert_eq!(back, r);
        assert_eq!(r.as_str(), "artifact-1");
        assert_eq!(format!("{r}"), "artifact-1");
    }

    #[test]
    fn kind_tag_new_variants_pinned() {
        let codex_req = Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        };
        assert_eq!(codex_req.kind_tag(), "codex.worker_requested");

        let term_req = Event::TerminalWorkerRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        };
        assert_eq!(term_req.kind_tag(), "terminal.worker_requested");

        let done = Event::TaskCompleted {
            idempotency_key: "k".into(),
            result: serde_json::Value::Null,
            artifacts: vec![],
            agent_message: None,
        };
        assert_eq!(done.kind_tag(), "task.completed");

        let failed = Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
            details: None,
            agent_message: None,
        };
        assert_eq!(failed.kind_tag(), "task.failed");

        let plan_updated = Event::PlanUpdated {
            track_id: TrackId::from("track-1"),
            changed_keys: vec!["impl-parser".into()],
            agent_message: None,
        };
        assert_eq!(plan_updated.kind_tag(), "plan.updated");

        let task_dispatched = Event::TaskDispatched {
            idempotency_key: "track-1:impl-parser".into(),
            kind: "codex".into(),
            agent_message: None,
        };
        assert_eq!(task_dispatched.kind_tag(), "task.dispatched");

        let workspace_leased = Event::WorkspaceLeased {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
            path: ".claude/worktrees/track-1/card-1".into(),
        };
        assert_eq!(workspace_leased.kind_tag(), "workspace.leased");

        let workspace_released = Event::WorkspaceReleased {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
        };
        assert_eq!(workspace_released.kind_tag(), "workspace.released");

        let forge_pr_merged = Event::ForgePrMerged {
            track_id: TrackId::from("track-1"),
            subject: ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 760,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        };
        assert_eq!(forge_pr_merged.kind_tag(), "forge.pr.merged");

        let review_round = Event::ReviewRound {
            track_id: TrackId::from("track-1"),
            subject: ReviewSubject {
                phase: "impl".into(),
                slice_id: "5b".into(),
                pr_number: Some(760),
            },
            head_sha: Some("head-sha".into()),
            n: 1,
            cap: 8,
            converged: false,
            channels: vec![ChannelVerdict {
                role: "design-correctness".into(),
                verdict: ChannelVerdictKind::ChangesRequested,
            }],
            root_cause: Some("tests failing".into()),
            idempotency_key: "review.round:track-1:impl:5b:760:1".into(),
        };
        assert_eq!(review_round.kind_tag(), "review.round");

        let ratify_requested = Event::RatifyRequested {
            track_id: TrackId::from("track-1"),
            reason: "cap_exhausted".into(),
        };
        assert_eq!(ratify_requested.kind_tag(), "ratify.requested");

        let ratify_resolved = Event::RatifyResolved {
            track_id: TrackId::from("track-1"),
            decision: RatifyDecision::Grant,
        };
        assert_eq!(ratify_resolved.kind_tag(), "ratify.resolved");

        let forge_scan_completed = Event::ForgeScanCompleted {
            track_id: TrackId::from("track-1"),
            overlapping_prs: vec![1, 2],
        };
        assert_eq!(forge_scan_completed.kind_tag(), "forge.scan.completed");

        let forge_pr_opened = Event::ForgePrOpened {
            track_id: TrackId::from("track-1"),
            pr_number: 1,
            head_sha: "head-sha".into(),
        };
        assert_eq!(forge_pr_opened.kind_tag(), "forge.pr.opened");

        let forge_pr_diff_read = Event::ForgePrDiffRead {
            track_id: TrackId::from("track-1"),
            pr_number: 1,
            base_sha: "base-sha".into(),
            head_sha: "head-sha".into(),
            artifact_path: "/tmp/neige/forge-diff.patch".into(),
        };
        assert_eq!(forge_pr_diff_read.kind_tag(), "forge.pr.diff.read");

        let forge_pr_checks = Event::ForgePrChecks {
            track_id: TrackId::from("track-1"),
            pr_number: 1,
            conclusion: "success".into(),
        };
        assert_eq!(forge_pr_checks.kind_tag(), "forge.pr.checks");

        let forge_issue_read = Event::ForgeIssueRead {
            track_id: TrackId::from("track-1"),
            issue_number: 1,
            artifact_path: "/tmp/neige/issue-body.md".into(),
        };
        assert_eq!(forge_issue_read.kind_tag(), "forge.issue.read");

        let forge_issue_closed = Event::ForgeIssueClosed {
            track_id: TrackId::from("track-1"),
            issue_number: 1,
        };
        assert_eq!(forge_issue_closed.kind_tag(), "forge.issue.closed");

        let worktree_provisioned = Event::WorktreeProvisioned {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            path: "/tmp/worktree".into(),
        };
        assert_eq!(worktree_provisioned.kind_tag(), "worktree.provisioned");

        let worktree_committed = Event::WorktreeCommitted {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            branch: "neige/track-1/card-1".into(),
        };
        assert_eq!(worktree_committed.kind_tag(), "worktree.committed");

        let worktree_removed = Event::WorktreeRemoved {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            path: "/tmp/worktree".into(),
        };
        assert_eq!(worktree_removed.kind_tag(), "worktree.removed");

        let claude_hook = Event::ClaudeHook {
            card_id: CardId::from("card-1"),
            kind: "hook.claude.stop".into(),
            hook_idempotency_key: "hook-claude".into(),
            payload: serde_json::Value::Null,
        };
        assert_eq!(claude_hook.kind_tag(), "claude.hook");

        let runtime_started = Event::WorkerSessionStarted {
            worker_session_id: "runtime-1".into(),
            card_id: "card-1".into(),
            kind: crate::runtime::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::runtime::AgentProvider::Codex),
            status: crate::worker::WorkerSessionState::Starting,
        };
        assert_eq!(runtime_started.kind_tag(), "worker_session.started");

        let runtime_status_changed = Event::WorkerSessionStatusChanged {
            worker_session_id: "runtime-1".into(),
            card_id: "card-1".into(),
            old_status: crate::worker::WorkerSessionState::Starting,
            new_status: crate::worker::WorkerSessionState::Running,
        };
        assert_eq!(
            runtime_status_changed.kind_tag(),
            "worker_session.status_changed"
        );

        let runtime_superseded = Event::WorkerSessionSuperseded {
            old_worker_session_id: "runtime-1".into(),
            new_worker_session_id: "runtime-2".into(),
            card_id: "card-1".into(),
        };
        assert_eq!(runtime_superseded.kind_tag(), "worker_session.superseded");

        let transcript_cleared = Event::HarnessTranscriptCleared {
            worker_session_id: "runtime-1".into(),
            card_id: CardId::from("card-1"),
            track_id: TrackId::from("track-1"),
            cleared_item_count: Some(12),
            cleared_params_bytes: Some(3_400),
            card_age_ms_at_clear: Some(86_400_000),
        };
        assert_eq!(transcript_cleared.kind_tag(), "harness.transcript.cleared");

        let user_message_enqueued = Event::HarnessUserMessageEnqueued {
            worker_session_id: "runtime-1".into(),
            card_id: CardId::from("card-1"),
            track_id: TrackId::from("track-1"),
            char_count: 5,
        };
        assert_eq!(
            user_message_enqueued.kind_tag(),
            "harness.user_message.enqueued"
        );

        let queue_changed = Event::HarnessQueueChanged {
            worker_session_id: "runtime-1".into(),
            card_id: CardId::from("card-1"),
            track_id: TrackId::from("track-1"),
            entry_id: "entry-1".into(),
            change: HarnessQueueChange::Deleted,
            actor: ActorId::User,
        };
        assert_eq!(queue_changed.kind_tag(), "harness.queue.changed");
    }

    #[test]
    fn harness_queue_change_wire_spellings() {
        for (change, wire) in [
            (HarnessQueueChange::Edited, "\"edited\""),
            (HarnessQueueChange::Deleted, "\"deleted\""),
            (HarnessQueueChange::Steered, "\"steered\""),
            (HarnessQueueChange::Restored, "\"restored\""),
            (HarnessQueueChange::Dropped, "\"dropped\""),
        ] {
            let encoded = serde_json::to_string(&change).expect("change serializes");
            assert_eq!(encoded, wire);
            let decoded: HarnessQueueChange =
                serde_json::from_str(wire).expect("change round-trips");
            assert_eq!(decoded, change);
        }
    }

    #[test]
    fn event_metadata_covers_all_variants_via_kind_tag() {
        for ev in metadata_coverage_events() {
            let metadata = ev.metadata();
            assert_eq!(
                metadata.kind_tag,
                ev.kind_tag(),
                "metadata kind_tag mismatch for {ev:?}",
            );
        }
    }

    #[test]
    fn kind_tag_does_not_allocate_for_string_payload_variants() {
        let ev = Event::OverlaySet(overlay_sample("p1", "card", "c1", "status"));
        let s: &'static str = ev.kind_tag();
        assert_eq!(s, "overlay.set");
    }

    #[test]
    fn event_metadata_overlay_carries_plugin_id_and_entity() {
        let ev = Event::OverlaySet(overlay_sample("p1", "card", "c1", "status"));
        let metadata = ev.metadata();

        assert_eq!(metadata.plugin_id.as_deref(), Some("p1"));
        assert_eq!(metadata.entity_kind.as_deref(), Some("card"));
        assert_eq!(metadata.entity_id.as_deref(), Some("c1"));
    }

    #[test]
    fn runtime_started_serde_round_trip() {
        let ev = Event::WorkerSessionStarted {
            worker_session_id: "runtime-1".into(),
            card_id: "card-1".into(),
            kind: crate::runtime::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::runtime::AgentProvider::Codex),
            status: crate::worker::WorkerSessionState::Starting,
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "worker_session.started");
        assert_eq!(json["data"]["worker_session_id"], "runtime-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["kind"], "codex");
        assert_eq!(json["data"]["agent_provider"], "codex");
        assert_eq!(json["data"]["status"], "starting");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::WorkerSessionStarted {
                worker_session_id,
                card_id,
                kind,
                agent_provider,
                status,
            } => {
                assert_eq!(worker_session_id, "runtime-1");
                assert_eq!(card_id, "card-1");
                assert_eq!(kind, crate::runtime::WorkerSessionKind::CodexCard);
                assert_eq!(agent_provider, Some(crate::runtime::AgentProvider::Codex));
                assert_eq!(status, crate::worker::WorkerSessionState::Starting);
            }
            other => panic!("expected WorkerSessionStarted after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn runtime_status_changed_serde_round_trip() {
        let ev = Event::WorkerSessionStatusChanged {
            worker_session_id: "runtime-1".into(),
            card_id: "card-1".into(),
            old_status: crate::worker::WorkerSessionState::Starting,
            new_status: crate::worker::WorkerSessionState::Running,
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "worker_session.status_changed");
        assert_eq!(json["data"]["worker_session_id"], "runtime-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["old_status"], "starting");
        assert_eq!(json["data"]["new_status"], "running");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::WorkerSessionStatusChanged {
                worker_session_id,
                card_id,
                old_status,
                new_status,
            } => {
                assert_eq!(worker_session_id, "runtime-1");
                assert_eq!(card_id, "card-1");
                assert_eq!(old_status, crate::worker::WorkerSessionState::Starting);
                assert_eq!(new_status, crate::worker::WorkerSessionState::Running);
            }
            other => panic!("expected WorkerSessionStatusChanged after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn runtime_superseded_serde_round_trip() {
        let ev = Event::WorkerSessionSuperseded {
            old_worker_session_id: "runtime-1".into(),
            new_worker_session_id: "runtime-2".into(),
            card_id: "card-1".into(),
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "worker_session.superseded");
        assert_eq!(json["data"]["old_worker_session_id"], "runtime-1");
        assert_eq!(json["data"]["new_worker_session_id"], "runtime-2");
        assert_eq!(json["data"]["card_id"], "card-1");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::WorkerSessionSuperseded {
                old_worker_session_id,
                new_worker_session_id,
                card_id,
            } => {
                assert_eq!(old_worker_session_id, "runtime-1");
                assert_eq!(new_worker_session_id, "runtime-2");
                assert_eq!(card_id, "card-1");
            }
            other => panic!("expected WorkerSessionSuperseded after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn claude_hook_serde_round_trip_kind_and_topics() {
        let ev = Event::ClaudeHook {
            card_id: CardId::from("card-claude"),
            kind: "hook.claude.pre_tool_use".into(),
            hook_idempotency_key: "hook-claude".into(),
            payload: serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
            }),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "claude.hook");
        assert_eq!(json["data"]["card_id"], "card-claude");
        assert_eq!(json["data"]["kind"], "hook.claude.pre_tool_use");
        assert_eq!(json["data"]["hook_idempotency_key"], "hook-claude");
        assert_eq!(json["data"]["payload"]["tool_name"], "Bash");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::ClaudeHook {
                card_id,
                kind,
                hook_idempotency_key,
                payload,
            } => {
                assert_eq!(card_id.as_str(), "card-claude");
                assert_eq!(kind, "hook.claude.pre_tool_use");
                assert_eq!(hook_idempotency_key, "hook-claude");
                assert_eq!(payload["hook_event_name"], "PreToolUse");
            }
            other => panic!("expected ClaudeHook after round-trip, got {other:?}"),
        }

        let replay = Event::from_kind_and_payload(
            "claude.hook",
            serde_json::json!({
                "card_id": "card-claude",
                "kind": "hook.claude.stop",
                "payload": { "hook_event_name": "Stop" },
            }),
        )
        .expect("replay decode ClaudeHook");
        assert_eq!(replay.kind_tag(), "claude.hook");
        match &replay {
            Event::ClaudeHook {
                hook_idempotency_key,
                ..
            } => assert!(hook_idempotency_key.is_empty()),
            other => panic!("expected ClaudeHook replay, got {other:?}"),
        }

        let t = topics(&replay);
        assert!(t.iter().any(|s| s == "card:card-claude"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
    }

    #[test]
    fn codex_worker_requested_serde_round_trip() {
        let ev = Event::CodexWorkerRequested {
            idempotency_key: "idem-1".into(),
            goal: "refactor X".into(),
            context: serde_json::json!({ "cwd": "/tmp", "hints": [1, 2] }),
            acceptance_criteria: Some("tests pass".into()),
            agent_message: Some("dispatch rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "codex.worker_requested");
        assert_eq!(json["data"]["idempotency_key"], "idem-1");
        assert_eq!(json["data"]["goal"], "refactor X");
        assert_eq!(json["data"]["context"]["cwd"], "/tmp");
        assert_eq!(json["data"]["acceptance_criteria"], "tests pass");
        assert_eq!(json["data"]["agent_message"], "dispatch rationale");

        let back: Event = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.kind_tag(), "codex.worker_requested");

        let mut old_json = json;
        old_json["ev"] = serde_json::json!("codex.job_requested");
        let back: Event = serde_json::from_value(old_json).unwrap();
        assert_eq!(back.kind_tag(), "codex.worker_requested");

        let no_ac = Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        };
        let v = serde_json::to_value(&no_ac).unwrap();
        assert!(
            v["data"].get("acceptance_criteria").is_none(),
            "acceptance_criteria should be omitted when None, got {v}",
        );
    }

    #[test]
    fn terminal_worker_requested_serde_round_trip() {
        let ev = Event::TerminalWorkerRequested {
            idempotency_key: "idem-2".into(),
            cmd: "cargo test".into(),
            cwd: Some("/repo".into()),
            agent_message: Some("terminal rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "terminal.worker_requested");
        assert_eq!(json["data"]["idempotency_key"], "idem-2");
        assert_eq!(json["data"]["cmd"], "cargo test");
        assert_eq!(json["data"]["cwd"], "/repo");
        assert_eq!(json["data"]["agent_message"], "terminal rationale");

        let no_cwd = Event::TerminalWorkerRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        };
        let v = serde_json::to_value(&no_cwd).unwrap();
        assert!(
            v["data"].get("cwd").is_none(),
            "cwd should be omitted when None, got {v}",
        );

        let back: Event = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.kind_tag(), "terminal.worker_requested");

        let mut old_json = json;
        old_json["ev"] = serde_json::json!("terminal.job_requested");
        let back: Event = serde_json::from_value(old_json).unwrap();
        assert_eq!(back.kind_tag(), "terminal.worker_requested");
    }

    #[test]
    fn task_completed_serde_round_trip() {
        let ev = Event::TaskCompleted {
            idempotency_key: "idem-3".into(),
            result: serde_json::json!({ "summary": "ok", "lines": 42 }),
            artifacts: vec![ArtifactRef::from("a-1"), ArtifactRef::from("a-2")],
            agent_message: Some("accepted rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.completed");
        assert_eq!(json["data"]["idempotency_key"], "idem-3");
        assert_eq!(json["data"]["result"]["summary"], "ok");
        assert_eq!(json["data"]["agent_message"], "accepted rationale");
        assert_eq!(json["data"]["artifacts"][0], "a-1");
        assert_eq!(json["data"]["artifacts"][1], "a-2");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.completed");
    }

    #[test]
    fn task_failed_serde_round_trip() {
        let ev = Event::TaskFailed {
            idempotency_key: "idem-4".into(),
            reason: "process exited with code 137".into(),
            details: Some(
                serde_json::json!({"pty_output": "boom\n", "pty_output_truncated": false}),
            ),
            agent_message: Some("rejected rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.failed");
        assert_eq!(json["data"]["idempotency_key"], "idem-4");
        assert_eq!(json["data"]["reason"], "process exited with code 137");
        assert_eq!(json["data"]["details"]["pty_output"], "boom\n");
        assert_eq!(json["data"]["agent_message"], "rejected rationale");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.failed");
    }

    #[test]
    fn task_gate_result_serde_round_trip() {
        let ev = Event::TaskGateResult {
            task_id: "w-1:impl".into(),
            idempotency_key: "w-1:impl".into(),
            passed: false,
            failing_step: Some("clippy".into()),
            exit_code: Some(101),
            log_tail: "error: ...".into(),
            log_path: "/data/gate-logs/w-1:impl-g2.log".into(),
            attempt: 2,
            agent_message: None,
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.gate_result");
        assert_eq!(json["data"]["task_id"], "w-1:impl");
        assert_eq!(json["data"]["idempotency_key"], "w-1:impl");
        assert_eq!(json["data"]["passed"], false);
        assert_eq!(json["data"]["failing_step"], "clippy");
        assert_eq!(json["data"]["exit_code"], 101);
        assert_eq!(json["data"]["log_tail"], "error: ...");
        assert_eq!(json["data"]["attempt"], 2);

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.gate_result");

        let green = Event::TaskGateResult {
            task_id: "w-1:impl".into(),
            idempotency_key: "w-1:impl".into(),
            passed: true,
            failing_step: None,
            exit_code: Some(0),
            log_tail: String::new(),
            log_path: "/data/gate-logs/w-1:impl-g1.log".into(),
            attempt: 1,
            agent_message: None,
        };
        let json = serde_json::to_value(&green).unwrap();
        assert!(json["data"].get("failing_step").is_none());
        assert!(json["data"].get("agent_message").is_none());
    }

    #[test]
    fn workspace_lease_events_serde_round_trip_and_topics() {
        let leased = Event::WorkspaceLeased {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
            path: ".claude/worktrees/track-1/card-1".into(),
        };
        let json = serde_json::to_value(&leased).unwrap();
        assert_eq!(json["ev"], "workspace.leased");
        assert_eq!(json["data"]["track_id"], "track-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["lease_id"], "lease-1");
        assert_eq!(json["data"]["path"], ".claude/worktrees/track-1/card-1");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "workspace.leased");
        assert_eq!(
            topics(&back),
            vec!["card:card-1", "track:track-1", "*"],
            "workspace lease events route by card and track"
        );
        let meta = back.metadata();
        assert_eq!(meta.entity_kind.as_deref(), Some("card"));
        assert_eq!(meta.entity_id.as_deref(), Some("card-1"));

        let released = Event::WorkspaceReleased {
            track_id: TrackId::from("track-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
        };
        let json = serde_json::to_value(&released).unwrap();
        assert_eq!(json["ev"], "workspace.released");
        assert_eq!(json["data"]["lease_id"], "lease-1");
        assert!(json["data"].get("path").is_none());

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "workspace.released");
        assert_eq!(
            topics(&back),
            vec!["card:card-1", "track:track-1", "*"],
            "workspace release events route by card and track"
        );
    }

    #[test]
    fn forge_pr_merged_serde_round_trip_metadata_and_topics() {
        let merged = Event::ForgePrMerged {
            track_id: TrackId::from("track-1"),
            subject: ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 760,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        };
        let json = serde_json::to_value(&merged).unwrap();
        assert_eq!(json["ev"], "forge.pr.merged");
        assert_eq!(json["data"]["track_id"], "track-1");
        assert_eq!(json["data"]["subject"]["phase"], "impl");
        assert_eq!(json["data"]["subject"]["slice_id"], "6");
        assert_eq!(json["data"]["subject"]["pr_number"], 760);
        assert_eq!(json["data"]["head_sha"], "head-sha");
        assert_eq!(json["data"]["merge_sha"], "merge-sha");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "forge.pr.merged");
        assert_eq!(
            topics(&back),
            vec!["track:track-1", "*"],
            "forge PR merge events route by track"
        );
        let meta = back.metadata();
        assert_eq!(meta.plugin_id, None);
        assert_eq!(meta.entity_kind.as_deref(), Some("track"));
        assert_eq!(meta.entity_id.as_deref(), Some("track-1"));
    }

    #[test]
    fn forge_event_spec_extracts_json_fields_with_nested_array_pointer() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "head_sha".into(),
            FieldSource::JsonField {
                path: "/oid".into(),
            },
        );
        fields.insert(
            "merge_sha".into(),
            FieldSource::JsonField {
                path: "/commits/0/oid".into(),
            },
        );
        let event_spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };
        let stdout = serde_json::json!({
            "oid": "head-sha",
            "commits": [{ "oid": "merge-sha" }],
        });

        let payload = event_spec.extract_payload(0, Some(&stdout)).unwrap();
        assert_eq!(
            payload.get("head_sha"),
            Some(&serde_json::json!("head-sha"))
        );
        assert_eq!(
            payload.get("merge_sha"),
            Some(&serde_json::json!("merge-sha"))
        );
    }

    #[test]
    fn forge_event_spec_missing_pointer_is_strict_error() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "merge_sha".into(),
            FieldSource::JsonField {
                path: "/missing".into(),
            },
        );
        let event_spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };

        let err = event_spec
            .extract_payload(0, Some(&serde_json::json!({})))
            .unwrap_err();
        assert_eq!(
            err,
            ForgeExtractError::PointerUnresolved {
                field: "merge_sha".into(),
                path: "/missing".into(),
            }
        );
    }

    #[test]
    fn forge_event_spec_missing_json_stdout_is_strict_error() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "head_sha".into(),
            FieldSource::JsonField {
                path: "/oid".into(),
            },
        );
        let event_spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };

        let err = event_spec.extract_payload(0, None).unwrap_err();
        assert_eq!(err, ForgeExtractError::MissingJsonStdout);
    }

    #[test]
    fn forge_event_spec_exit_code_yields_json_number() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("exit_code".into(), FieldSource::ExitCode);
        let event_spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };

        let payload = event_spec.extract_payload(37, None).unwrap();
        assert_eq!(payload.get("exit_code"), Some(&serde_json::json!(37)));
    }

    #[test]
    fn forge_event_spec_empty_fields_yields_empty_object() {
        let event_spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields: std::collections::BTreeMap::new(),
        };

        let payload = event_spec.extract_payload(0, None).unwrap();
        assert!(payload.is_empty());
    }

    #[test]
    fn edit_author_lowercase_wire_shape() {
        assert_eq!(
            serde_json::to_string(&EditAuthor::Planner).unwrap(),
            r#""planner""#
        );
        assert_eq!(
            serde_json::to_string(&EditAuthor::User).unwrap(),
            r#""user""#
        );
        assert_eq!(
            serde_json::to_string(&EditAuthor::Kernel).unwrap(),
            r#""kernel""#
        );
        assert_eq!(
            serde_json::to_string(&EditAuthor::Plugin).unwrap(),
            r#""plugin""#
        );

        for variant in [
            EditAuthor::Planner,
            EditAuthor::User,
            EditAuthor::Kernel,
            EditAuthor::Plugin,
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            let back: EditAuthor = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant, "round-trip mismatch for {variant:?}");
        }
    }

    #[test]
    fn track_report_edited_kind_tag_pinned() {
        let ev = track_report_edited_sample();
        assert_eq!(ev.kind_tag(), "track.report_edited");
    }

    #[test]
    fn track_report_edited_serde_round_trip() {
        let ev = track_report_edited_sample();
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "track.report_edited");
        assert_eq!(json["data"]["track_id"], "w-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["author"], "planner");
        assert_eq!(json["data"]["edit_id"], "edit-uuid-1");
        assert_eq!(json["data"]["summary_before"], "old summary");
        assert_eq!(json["data"]["summary_after"], "new summary");
        assert_eq!(json["data"]["body_before"], "old body");
        assert_eq!(json["data"]["body_after"], "new body");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "track.report_edited");
        match back {
            Event::TrackReportEdited {
                track_id,
                card_id,
                author,
                edit_id,
                summary_before,
                summary_after,
                body_before,
                body_after,
                ..
            } => {
                assert_eq!(track_id.as_str(), "w-1");
                assert_eq!(card_id.as_str(), "card-1");
                assert_eq!(author, EditAuthor::Planner);
                assert_eq!(edit_id, "edit-uuid-1");
                assert_eq!(summary_before, "old summary");
                assert_eq!(summary_after, "new summary");
                assert_eq!(body_before, "old body");
                assert_eq!(body_after, "new body");
            }
            other => panic!("expected TrackReportEdited after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn track_report_edited_replay_via_from_kind_and_payload() {
        for author_str in ["planner", "user", "kernel", "plugin"] {
            let payload = serde_json::json!({
                "track_id": "w-1",
                "card_id": "card-1",
                "author": author_str,
                "edit_id": "edit-uuid-1",
                "summary_before": "s0",
                "summary_after": "s1",
                "body_before": "b0",
                "body_after": "b1",
            });
            let ev = Event::from_kind_and_payload("track.report_edited", payload)
                .unwrap_or_else(|e| panic!("replay decode failed for author={author_str}: {e}"));
            assert_eq!(ev.kind_tag(), "track.report_edited");
            match ev {
                Event::TrackReportEdited { author, .. } => match (author_str, author) {
                    ("planner", EditAuthor::Planner)
                    | ("user", EditAuthor::User)
                    | ("kernel", EditAuthor::Kernel)
                    | ("plugin", EditAuthor::Plugin) => {}
                    (expected, actual) => {
                        panic!("author mismatch: expected {expected}, deserialized into {actual:?}")
                    }
                },
                other => panic!("expected TrackReportEdited, got {other:?}"),
            }
        }
    }

    #[test]
    fn track_report_edited_topics_card_and_track() {
        let ev = track_report_edited_sample();
        let t = topics(&ev);
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "track:w-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
    }

    #[test]
    fn runtime_started_topics_card_only() {
        let ev = Event::WorkerSessionStarted {
            worker_session_id: "rt-1".into(),
            card_id: "card-1".into(),
            kind: crate::runtime::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::runtime::AgentProvider::Codex),
            status: crate::worker::WorkerSessionState::Starting,
        };
        let t = topics(&ev);
        assert_eq!(t.len(), 2, "topics={t:?}");
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
        assert!(
            !t.iter().any(|s| s.starts_with("track:")),
            "topics must not include track: scope; topics={t:?}"
        );
    }

    #[test]
    fn runtime_status_changed_topics_card_only() {
        let ev = Event::WorkerSessionStatusChanged {
            worker_session_id: "rt-1".into(),
            card_id: "card-1".into(),
            old_status: crate::worker::WorkerSessionState::Starting,
            new_status: crate::worker::WorkerSessionState::Running,
        };
        let t = topics(&ev);
        assert_eq!(t.len(), 2, "topics={t:?}");
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
        assert!(
            !t.iter().any(|s| s.starts_with("track:")),
            "topics must not include track: scope; topics={t:?}"
        );
    }

    #[test]
    fn runtime_superseded_topics_card_only() {
        let ev = Event::WorkerSessionSuperseded {
            old_worker_session_id: "rt-old".into(),
            new_worker_session_id: "rt-new".into(),
            card_id: "card-1".into(),
        };
        let t = topics(&ev);
        assert_eq!(t.len(), 2, "topics={t:?}");
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
        assert!(
            !t.iter().any(|s| s.starts_with("track:")),
            "topics must not include track: scope; topics={t:?}"
        );
    }

    fn track_report_edited_sample() -> Event {
        Event::TrackReportEdited {
            track_id: TrackId::from("w-1"),
            card_id: CardId::from("card-1"),
            author: EditAuthor::Planner,
            author_plugin_id: None,
            edit_id: "edit-uuid-1".into(),
            summary_before: "old summary".into(),
            summary_after: "new summary".into(),
            body_before: "old body".into(),
            body_after: "new body".into(),
            agent_message: None,
        }
    }

    fn metadata_coverage_events() -> Vec<Event> {
        vec![
            Event::AreaUpdated(area_sample("area-updated")),
            Event::AreaDeleted {
                id: AreaId::from("area-deleted"),
            },
            Event::TrackUpdated(TrackUpdatedPayload::new(
                track_sample("track-updated", "area-1"),
                None,
            )),
            Event::TrackDeleted {
                id: TrackId::from("track-deleted"),
                area_id: AreaId::from("area-1"),
            },
            Event::TrackLifecycleChanged {
                id: TrackId::from("track-lifecycle"),
                area_id: AreaId::from("area-1"),
                from: TrackLifecycle::Draft,
                to: TrackLifecycle::Planning,
                agent_message: None,
            },
            Event::CardAdded(card_sample("card-added", "track-1")),
            Event::CardUpdated(card_sample("card-updated", "track-1")),
            Event::CardDeleted {
                id: CardId::from("card-deleted"),
                track_id: TrackId::from("track-1"),
            },
            Event::WorkerSessionStarted {
                worker_session_id: "runtime-started".into(),
                card_id: "card-runtime".into(),
                kind: crate::runtime::WorkerSessionKind::CodexCard,
                agent_provider: Some(crate::runtime::AgentProvider::Codex),
                status: crate::worker::WorkerSessionState::Starting,
            },
            Event::WorkerSessionStatusChanged {
                worker_session_id: "runtime-status".into(),
                card_id: "card-runtime".into(),
                old_status: crate::worker::WorkerSessionState::Starting,
                new_status: crate::worker::WorkerSessionState::Running,
            },
            Event::WorkerSessionSuperseded {
                old_worker_session_id: "runtime-old".into(),
                new_worker_session_id: "runtime-new".into(),
                card_id: "card-runtime".into(),
            },
            Event::HarnessTranscriptCleared {
                worker_session_id: "runtime-transcript".into(),
                card_id: CardId::from("card-runtime"),
                track_id: TrackId::from("track-1"),
                cleared_item_count: Some(12),
                cleared_params_bytes: Some(3_400),
                card_age_ms_at_clear: Some(86_400_000),
            },
            Event::HarnessUserMessageEnqueued {
                worker_session_id: "runtime-user-message".into(),
                card_id: CardId::from("card-runtime"),
                track_id: TrackId::from("track-1"),
                char_count: 5,
            },
            Event::HarnessQueueChanged {
                worker_session_id: "runtime-queue-changed".into(),
                card_id: CardId::from("card-runtime"),
                track_id: TrackId::from("track-1"),
                entry_id: "entry-1".into(),
                change: HarnessQueueChange::Edited,
                actor: ActorId::User,
            },
            track_report_edited_sample(),
            Event::OverlaySet(overlay_sample("plugin-1", "card", "card-1", "status")),
            Event::OverlayDeleted {
                plugin_id: "plugin-1".into(),
                entity_kind: "card".into(),
                entity_id: "card-1".into(),
                kind: "status".into(),
            },
            Event::TerminalDeleted {
                id: "terminal-1".into(),
                card_id: CardId::from("card-1"),
            },
            Event::PluginState {
                id: "plugin-1".into(),
                state: "running".into(),
                last_error: None,
            },
            Event::PluginToolRegistered {
                plugin_id: "plugin-1".into(),
                tool_name: "calm.plugin.echo".into(),
            },
            Event::CodexHook {
                card_id: CardId::from("card-codex"),
                kind: "hook.codex.stop".into(),
                hook_idempotency_key: "hook-codex".into(),
                payload: serde_json::Value::Null,
            },
            Event::ClaudeHook {
                card_id: CardId::from("card-claude"),
                kind: "hook.claude.stop".into(),
                hook_idempotency_key: "hook-claude".into(),
                payload: serde_json::Value::Null,
            },
            Event::CodexWorkerRequested {
                idempotency_key: "k".into(),
                goal: "g".into(),
                context: serde_json::Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            },
            Event::TerminalWorkerRequested {
                idempotency_key: "k".into(),
                cmd: "ls".into(),
                cwd: None,
                agent_message: None,
            },
            Event::TaskCompleted {
                idempotency_key: "k".into(),
                result: serde_json::Value::Null,
                artifacts: vec![],
                agent_message: None,
            },
            Event::TaskFailed {
                idempotency_key: "k".into(),
                reason: "boom".into(),
                details: None,
                agent_message: None,
            },
            Event::PlanUpdated {
                track_id: TrackId::from("track-1"),
                changed_keys: vec!["impl-parser".into()],
                agent_message: None,
            },
            Event::TaskDispatched {
                idempotency_key: "track-1:impl-parser".into(),
                kind: "codex".into(),
                agent_message: None,
            },
            Event::WorkspaceLeased {
                track_id: TrackId::from("track-1"),
                card_id: CardId::from("card-workspace"),
                lease_id: "lease-1".into(),
                path: ".claude/worktrees/track-1/card-workspace".into(),
            },
            Event::WorkspaceReleased {
                track_id: TrackId::from("track-1"),
                card_id: CardId::from("card-workspace"),
                lease_id: "lease-1".into(),
            },
            Event::ForgePrMerged {
                track_id: TrackId::from("track-1"),
                subject: ForgeMergeSubject {
                    phase: "impl".into(),
                    slice_id: "6".into(),
                    pr_number: 760,
                },
                head_sha: "head-sha".into(),
                merge_sha: "merge-sha".into(),
            },
            Event::ReviewRound {
                track_id: TrackId::from("track-1"),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: "5b".into(),
                    pr_number: Some(760),
                },
                head_sha: Some("head-sha".into()),
                n: 1,
                cap: 8,
                converged: false,
                channels: vec![ChannelVerdict {
                    role: "design-correctness".into(),
                    verdict: ChannelVerdictKind::ChangesRequested,
                }],
                root_cause: Some("tests failing".into()),
                idempotency_key: "review.round:track-1:impl:5b:760:1".into(),
            },
            Event::RatifyRequested {
                track_id: TrackId::from("track-1"),
                reason: "cap_exhausted".into(),
            },
            Event::RatifyResolved {
                track_id: TrackId::from("track-1"),
                decision: RatifyDecision::Grant,
            },
            Event::ProposalSubmitted {
                track_id: TrackId::from("track-1"),
                proposal_id: "pp-1".into(),
                plugin_id: "dev.neige.invest".into(),
                subject_kind: "report".into(),
                base_doc_heads: "ah1:deadbeef".into(),
                ops: vec![crate::proposal::ProposalOp::DeleteBlock {
                    block_id: "b_0001".into(),
                    if_rev: 1,
                }],
                note: "why".into(),
                idem_key: "idem-1".into(),
            },
            Event::ProposalResolved {
                track_id: TrackId::from("track-1"),
                proposal_id: "pp-1".into(),
                plugin_id: "dev.neige.invest".into(),
                decision: ProposalDecision::Accepted,
            },
            Event::ForgeScanCompleted {
                track_id: TrackId::from("track-1"),
                overlapping_prs: vec![1, 2],
            },
            Event::ForgePrOpened {
                track_id: TrackId::from("track-1"),
                pr_number: 1,
                head_sha: "head-sha".into(),
            },
            Event::ForgePrDiffRead {
                track_id: TrackId::from("track-1"),
                pr_number: 1,
                base_sha: "base-sha".into(),
                head_sha: "head-sha".into(),
                artifact_path: "/tmp/neige/forge-diff.patch".into(),
            },
            Event::ForgePrChecks {
                track_id: TrackId::from("track-1"),
                pr_number: 1,
                conclusion: "success".into(),
            },
            Event::ForgeIssueRead {
                track_id: TrackId::from("track-1"),
                issue_number: 1,
                artifact_path: "/tmp/neige/issue-body.md".into(),
            },
            Event::ForgeIssueClosed {
                track_id: TrackId::from("track-1"),
                issue_number: 1,
            },
            Event::WorktreeProvisioned {
                track_id: TrackId::from("track-1"),
                card_id: CardId::from("card-worktree"),
                path: "/tmp/worktree".into(),
            },
            Event::WorktreeCommitted {
                track_id: TrackId::from("track-1"),
                card_id: CardId::from("card-worktree"),
                commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
                branch: "neige/track-1/card-worktree".into(),
            },
            Event::WorktreeRemoved {
                track_id: TrackId::from("track-1"),
                card_id: CardId::from("card-worktree"),
                path: "/tmp/worktree".into(),
            },
        ]
    }

    fn area_sample(id: &str) -> Area {
        Area {
            id: AreaId::from(id),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: crate::model::AreaKind::User,
            default_template_id: None,
            default_cwd: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn track_sample(id: &str, area_id: &str) -> Track {
        Track {
            id: TrackId::from(id),
            area_id: AreaId::from(area_id),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: TrackLifecycle::Draft,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: None,
            template_input: None,
            terminal_at: None,
            recipe_id: None,
            recipe_revision: None,
            claude_permissions_policy: None,
            workspace: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_sample(id: &str, track_id: &str) -> Card {
        Card {
            id: CardId::from(id),
            track_id: TrackId::from(track_id),
            kind: "terminal".into(),
            sort: 1.0,
            payload: serde_json::json!({}),
            title: None,
            runtime: None,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn overlay_sample(plugin_id: &str, entity_kind: &str, entity_id: &str, kind: &str) -> Overlay {
        Overlay {
            id: "overlay-1".into(),
            plugin_id: plugin_id.into(),
            entity_kind: entity_kind.into(),
            entity_id: entity_id.into(),
            kind: kind.into(),
            payload: serde_json::json!({}),
            updated_at: 0,
        }
    }

    #[test]
    fn new_variants_round_trip_via_from_kind_and_payload() {
        for (kind, expected_kind, payload) in [
            (
                "claude.hook",
                "claude.hook",
                serde_json::json!({
                    "card_id": "card-1",
                    "kind": "hook.claude.stop",
                    "payload": {},
                }),
            ),
            (
                "codex.worker_requested",
                "codex.worker_requested",
                serde_json::json!({
                    "idempotency_key": "k",
                    "goal": "g",
                    "context": {},
                }),
            ),
            (
                "codex.job_requested",
                "codex.worker_requested",
                serde_json::json!({
                    "idempotency_key": "k",
                    "goal": "g",
                    "context": {},
                }),
            ),
            (
                "terminal.worker_requested",
                "terminal.worker_requested",
                serde_json::json!({ "idempotency_key": "k", "cmd": "ls" }),
            ),
            (
                "terminal.job_requested",
                "terminal.worker_requested",
                serde_json::json!({ "idempotency_key": "k", "cmd": "ls" }),
            ),
            (
                "task.completed",
                "task.completed",
                serde_json::json!({
                    "idempotency_key": "k",
                    "result": {},
                    "artifacts": [],
                }),
            ),
            (
                "task.failed",
                "task.failed",
                serde_json::json!({ "idempotency_key": "k", "reason": "r" }),
            ),
            (
                "worker_session.started",
                "worker_session.started",
                serde_json::json!({
                    "worker_session_id": "runtime-1",
                    "card_id": "card-1",
                    "kind": "codex",
                    "agent_provider": "codex",
                    "status": "starting",
                }),
            ),
            (
                "worker_session.status_changed",
                "worker_session.status_changed",
                serde_json::json!({
                    "worker_session_id": "runtime-1",
                    "card_id": "card-1",
                    "old_status": "starting",
                    "new_status": "running",
                }),
            ),
            (
                "worker_session.superseded",
                "worker_session.superseded",
                serde_json::json!({
                    "old_worker_session_id": "runtime-1",
                    "new_worker_session_id": "runtime-2",
                    "card_id": "card-1",
                }),
            ),
            (
                "workspace.leased",
                "workspace.leased",
                serde_json::json!({
                    "track_id": "track-1",
                    "card_id": "card-1",
                    "lease_id": "lease-1",
                    "path": ".claude/worktrees/track-1/card-1",
                }),
            ),
            (
                "workspace.released",
                "workspace.released",
                serde_json::json!({
                    "track_id": "track-1",
                    "card_id": "card-1",
                    "lease_id": "lease-1",
                }),
            ),
            (
                "forge.pr.merged",
                "forge.pr.merged",
                serde_json::json!({
                    "track_id": "track-1",
                    "subject": {
                        "phase": "impl",
                        "slice_id": "6",
                        "pr_number": 760,
                    },
                    "head_sha": "head-sha",
                    "merge_sha": "merge-sha",
                }),
            ),
            (
                "review.round",
                "review.round",
                serde_json::json!({
                    "track_id": "track-1",
                    "subject": {
                        "phase": "impl",
                        "slice_id": "5b",
                        "pr_number": 760,
                    },
                    "head_sha": "head-sha",
                    "n": 1,
                    "cap": 8,
                    "converged": false,
                    "channels": [
                        { "role": "design-correctness", "verdict": "changes_requested" },
                        { "role": "failure-path", "verdict": "approved" },
                    ],
                    "root_cause": "tests failing",
                    "idempotency_key": "review.round:track-1:impl:5b:760:1",
                }),
            ),
            (
                "ratify.requested",
                "ratify.requested",
                serde_json::json!({
                    "track_id": "track-1",
                    "reason": "cap_exhausted",
                }),
            ),
            (
                "ratify.resolved",
                "ratify.resolved",
                serde_json::json!({
                    "track_id": "track-1",
                    "decision": "grant",
                }),
            ),
            (
                "forge.scan.completed",
                "forge.scan.completed",
                serde_json::json!({
                    "track_id": "track-1",
                    "overlapping_prs": [1, 2],
                }),
            ),
            (
                "forge.pr.opened",
                "forge.pr.opened",
                serde_json::json!({
                    "track_id": "track-1",
                    "pr_number": 1,
                    "head_sha": "head-sha",
                }),
            ),
            (
                "forge.pr.diff.read",
                "forge.pr.diff.read",
                serde_json::json!({
                    "track_id": "track-1",
                    "pr_number": 1,
                    "base_sha": "base-sha",
                    "head_sha": "head-sha",
                    "artifact_path": "/tmp/neige/forge-diff.patch",
                }),
            ),
            (
                "forge.pr.checks",
                "forge.pr.checks",
                serde_json::json!({
                    "track_id": "track-1",
                    "pr_number": 1,
                    "conclusion": "success",
                }),
            ),
            (
                "forge.issue.read",
                "forge.issue.read",
                serde_json::json!({
                    "track_id": "track-1",
                    "issue_number": 1,
                    "artifact_path": "/tmp/neige/issue-body.md",
                }),
            ),
            (
                "forge.issue.closed",
                "forge.issue.closed",
                serde_json::json!({
                    "track_id": "track-1",
                    "issue_number": 1,
                }),
            ),
            (
                "worktree.provisioned",
                "worktree.provisioned",
                serde_json::json!({
                    "track_id": "track-1",
                    "card_id": "card-1",
                    "path": "/tmp/worktree",
                }),
            ),
            (
                "worktree.committed",
                "worktree.committed",
                serde_json::json!({
                    "track_id": "track-1",
                    "card_id": "card-1",
                    "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                    "branch": "neige/track-1/card-1",
                }),
            ),
            (
                "worktree.removed",
                "worktree.removed",
                serde_json::json!({
                    "track_id": "track-1",
                    "card_id": "card-1",
                    "path": "/tmp/worktree",
                }),
            ),
        ] {
            let ev = Event::from_kind_and_payload(kind, payload)
                .unwrap_or_else(|e| panic!("replay decode failed for {kind}: {e}"));
            assert_eq!(ev.kind_tag(), expected_kind, "round-trip kind mismatch");
            match kind {
                "codex.job_requested" => {
                    assert!(matches!(ev, Event::CodexWorkerRequested { .. }))
                }
                "terminal.job_requested" => {
                    assert!(matches!(ev, Event::TerminalWorkerRequested { .. }))
                }
                _ => {}
            }
        }
    }
}
