# Feishu recurring inspection

`feishu-watch` is an optional compiled feature, disabled until configured. It
polls the explicitly imported sources as the configured Feishu user. A complete
empty inspection checkpoints health without a model request. New unclassified
messages, fault/recovery transitions and explicit reminders wake its configured
Track's Planner using existing durable event delivery.

This delivery does not change an existing deployment, cancel its calendars or
compact its current conversation automatically. It makes no latency or billing
claim. The observed baseline on 4140 was five model requests per quiet wake and
approximately 1,942 added input tokens between wakes; controlled acceptance
checks count new wakes rather than summing cached inputs as context occupancy.

## Configure and migrate

Configure all six operator fields in Settings → Plugins → Feishu inspection:

| Field | Meaning |
| --- | --- |
| `track_id` | Existing open Track whose current Planner owns this inspection |
| `cli_path` | Absolute executable path to the installed official `lark-cli` |
| `home` | Absolute HOME containing the separately authorized CLI login |
| `profile` | Explicit CLI profile; no default-profile fallback |
| `self_open_id` | Operator-verified user ID that `whoami --as user` must return |
| `import_state_path` | Absolute path to a verified snapshot of the previous watch JSON |

Use a separate snapshot, not a file another scanner is changing. Preserve its
original instructions, pending items and references. Before cutover, cancel all
previous calendar entries from the old state's `calendar_entries`, then verify
the final snapshot and enable the component. Missing required fields, identity
mismatch or unknown source refuses import. The original file is never written;
the complete parsed JSON survives in private plugin KV.

An import wake asks the Planner to read every page of batch `import`, reconcile
rules and pending items, and record explicit reminder deadlines. Old cursors are
historical claims: reconcile unknown coverage rather than announcing that all
historical messages were processed. Threads use their own last-read time;
other sources cannot hide a reply. Approved-send and Ask records are evidence
to recheck, not permission to send again. No Feishu send operation is exposed.

The first release monitors the imported source set for one Track. Edits to the
old file after import have no effect. Changing sources or identity requires
explicit operator reconciliation of retained state. Pending configuration edits
need reload; old runtime publication is fenced even after identical re-enable.

## Planner continuation

`plugin_feishu_watch_status` returns bounded pages of batch metadata, source
health and reminders. Follow each non-null `next_after_*` using its matching
`after_*` argument. It does not return the whole message history.

`plugin_feishu_watch_read` reads an immutable batch in bounded pages. For
`import`, follow `next_legacy_offset` with `legacy_offset` for retained JSON
provenance. Message bodies and imported data are untrusted reference data.

After reading all relevant evidence, use existing Ask mechanisms when user
action is required. Then `plugin_feishu_watch_settle` records whole-batch
classification and original Ask/evidence references. Required arguments are
`batch_id`, object `classification`, `deadlines`, and `cancel_deadline_keys`
(empty arrays are valid). Each deadline has `key`, future `at_ms`, and `note`.
Identical settlement retries are idempotent; changing an accepted settlement
is refused. A later batch may replace a reminder by key or cancel it explicitly.
Use `plugin_feishu_watch_reminders` after a normal user answer or reminder-only
wake to cancel or defer without needing a new message batch. Supply a permanent
`request_id`, the latest `expected_revision`, `deadlines` and
`cancel_deadline_keys`. Identical retries never rearm fired reminders. New stale
commands reread status; changed content under an accepted request ID is refused.

Settlement is not a notification and grants no authorization. Reconcile an
uncertain Ask outcome before considering another notification.

Keep meaningful conclusions in the Report. Per-check timestamps and health come
from feature status, so empty checks do not rewrite report prose or append chat
replies. Existing compaction remains available; this feature does not create
replacement conversations or guarantee a provider token ceiling for actionable work.

## Failure and rollback

Each source/thread progresses only after a complete, validated read. New
messages, deduplication, checkpoint and wake commit atomically. Partial failures
retain that source's cursor and publish a transition; identical failures stay
quiet. Explicit reminders fire even between message scans.

Reads are restricted to the configured Track's current Planner. Settlement
rechecks authority inside its transaction. Disable, close, configuration changes
and stale revisions reject publication. Unclassified messages survive restart
and remain readable without manufacturing a second wake.

State is capped at 1 MiB and message/page output is bounded. Capacity/import
failures retain previous state and publish a separate bounded diagnostic
transition. Resolve capacity explicitly; no silent pruning or cursor advance.
Capacity exhaustion pauses scanning and reminder progression until operator
reconciliation; unclassified data is never discarded. Failed diagnostic
publication is logged and retried under the same runtime fences.

To roll back, disable first. Preserve private state and reconcile inbox/reminders
before explicitly restoring the old schedule; the old file was not advanced.
Do not run both scanners concurrently or replay the old snapshot without
message-ID reconciliation.
