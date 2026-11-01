# Recurring inspection continuation

Issue: #3425. Review tier: L2, because observation checkpoints, durable inboxes,
background authority and restart recovery change.

## Outcome and evidence

The existing Feishu inspection on deployment 4140 repeatedly enters one Planner
conversation even when no input changed. Ten observed quiet wakes each made five
model requests and added approximately 1,942 input tokens between inspections.
This establishes redundant continuation growth, not a context-induced latency
cause. Existing provider compaction remains the only context maintenance owner.

The existing task-generated scanner is outside this repository. Controlled
executions of that actual script reproduce two defects: missing pagination
metadata is accepted as complete, and a reply newer than its thread cursor is
discarded when another source has a newer cursor. The replacement must own the
whole scan/checkpoint path rather than copying these policies into the kernel.

## Ownership

Add an optional compiled `feishuwatch` feature component using the existing
builtin registry and background lifecycle. V1 intentionally binds one
operator-configured Track. Its operator configuration pins the Track, CLI
executable, authentication HOME, profile and expected user identity. A separately
configured legacy state path is imported once; the original file is never
modified. Only the known legacy schema is accepted. Configuration and imported
identity must agree; each scan verifies CLI whoami against the configured user
before reading messages. The original source set is fixed for this scoped v1;
later source changes require operator reconciliation. Existing user rules,
pending records and notification provenance survive import; none grants new
authorization.

The `plugin` crate owns Feishu CLI parsing, independent source/thread progress,
deduplication, health transitions, inbox and reminder decisions. The server
adapter owns current plugin/configuration and open-Track checks, current Planner
authorization for settlement, and one atomic state-plus-Kernel-wake transaction.
Calendar, Dispatcher and generic conversation/provider code remain unchanged.
Use the existing persistent `TrackWakeRequested` delivery/catch-up path.

## Durable continuation and delivery

Each complete source contributes messages to a durable inbox before its cursor
advances. Incomplete or failed sources keep their previous cursor; successful
siblings may progress. Each thread has its own cursor. Overlap is filtered by
message identity, not by discarding every timestamp below the previous scan.
Missing required fields and unknown pagination completeness fail visibly.

Persist one revisioned feature state in plugin KV, with a 1 MiB cap. Quiet
checkpoints use the existing silent transaction; state plus a nonempty wake
uses the existing event transaction. Both share one authority/CAS/write helper. A CAS failure,
configuration change, disable or Track closure rejects stale publication. Capture
the active runtime Arc under the plugin lifecycle lock, release during external
reads, and re-acquire before publication with Arc identity equality. This also
fences disable/re-enable with identical configuration. Never
drop unclassified messages to fit the cap. A separate bounded diagnostic record
and transition wake remain writable when the main inbox reaches capacity. Polling is serialized by its component;
the persistence check remains authoritative across racing tools and restarts.

An unchanged successful scan updates health/checkpoint state with no model wake.
New unclassified messages, a due explicit reminder or a health transition produce
a bounded wake naming durable evidence; message bodies are retrieved in bounded
pages by authenticated native tools. Unchanged failure produces no repeated wake.
User answers and existing task events retain their ordinary delivery path.

The Planner classifies inbox data and uses existing Ask/approval mechanisms when
needed. Settlement records classification and optional explicit reminders. A
separate revisioned reminder command records permanent request receipts, allowing
ordinary user answers to cancel and due-only wakes to defer without a new message
batch; identical retries cannot rearm fired reminders.
uncertain notification outcomes require reconciliation through original Ask
bindings, never blind retry. Source messages and imported summaries are untrusted
reference data. No Feishu sending API is exposed.

## Rollout and acceptance

Ship code and operator instructions without changing the live DB or schedules.
Before enabling on an existing Track, preserve a verified import snapshot and
disable its old recurring calendar entries to avoid parallel scanners. Verify
retained pending actions and rules, then enable the new component. Keep ordinary
user-driven Planner conversation history and existing compaction; this feature
does not create blank replacement conversations or promise a provider token cap.

Required checks exercise production entry points with controlled CLI responses:
quiet repeated scans (zero wakes), unseen/delayed replies, missing metadata,
partial source failure, duplicate overlap, restart with unclassified messages,
fault/recovery transitions, due reminders, size bounds, stale CAS/configuration,
disabled/closed Track and stale Planner settlement. Mutation-verify the key
no-silent-miss and transactional authority assertions. Two independent L2 reviews
must converge. Run focused plugin/server suites, required text, quick Rust and
full contract gates; CI owns broad suites. No real Codex E2E or live message send.
