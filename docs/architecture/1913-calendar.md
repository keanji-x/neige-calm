# Calendar commitments

Calendar is an always-enabled compiled plugin (`dev.neige.calendar`); other
builtins keep their declared optional lifecycle. Its own module owns validation, namespaced persistence,
HTTP handlers, and native AI tools. The catalog registers HTTP routers without
application-ID dispatch in the kernel. Page loads only read.

## Desktop presentation

The sidebar uses FullCalendar for Week and Month date navigation. Both views
show dates with Track counts above and task counts below; full task content
appears only in the selected-day list. Track counts reuse Today's activity
selection and closed-track visibility. Task counts project loaded entries onto
local calendar dates, respecting exclusive timed ends. No execution is implied.

The Calendar heading contains an Astryx segmented control. Task rows put title
and time on one line, truncating long titles; notes remain in editing. Date and
timezone share the heading with an icon-only plus button. Calendar, Activity and
Conversations use sibling PanelModules and full-width group dividers. The task
and activity lists have preferred 12rem scrolling viewports. The card is bounded
by the available desktop height; Month consumes more date-grid space, so both
lists shrink and keep independent scrolling. Shared PanelCard/PanelModule props
express generic fill/shrink behavior without Calendar-specific UI rules.

Activity replaces the separate Open group and shows update times using existing
Track rows. Closed tracks are hidden initially; the Activity menu toggles them in
both Activity and date counts. Show read defaults on and uses the same receipt predicate as Track unread status,
injected by the app. Both filters apply to date counts and Activity. The panel
has top padding around its controls. Task text reuses ListText primary typography.

Today owns the focused date. The app queries the visible window and selected day
separately. Month/week navigation and default creation date stay connected. An
open edit dialog retains its captured date when background selection changes.
New task starts with a title and all-day date; time and notes expand on demand.
Nondefault zones and overnight ends stay explicit. Cancellation uses stored
content. The compact viewport excludes scheduling; mobile is deferred.

## Domain and authority

Calendar entries are work commitments, separate from execution Tasks and Track
lifecycle. This slice stores dates and time ranges only: no execution, dependency,
reminder or delivery workflow is triggered. Source Track identity is attribution.

Use existing plugin KV storage: one versioned record per entry and immutable
creation receipts, within the normal transaction boundary. The generic
`plugin.data.changed` event contains only plugin identity and invalidates that
plugin's query prefix. No calendar policy enters the generic event dispatcher.

Humans can access calendar entries through session-protected REST. AI tools are
exposed to Planner and Assistant roles, subject to existing plugin admission and
Track scope. AI reads/writes only entries belonging to its authenticated Track;
a launchpad assistant has the same restriction, not implicit global power.
Human-created entries have no source Track. Request arguments cannot forge source
or creator. Worker roles and forge lowering cannot call the Calendar capability.

Time contracts distinguish an all-day civil date from an RFC3339 instant range
with an explicit IANA timezone. Create/update also accept local YYYY-MM-DDTHH:mm
start/end; Calendar resolves unambiguous wall times before storage and returns
RFC3339 times. DST gaps and overlaps fail clearly unless a valid explicit offset
resolves the overlap. Offset/timezone mismatches are rejected. Queries
use a half-open date window and explicit display timezone. Updates compare
revisions; cancelled entries remain durable. Lost creation responses can be
retried using the same key and body.

## Contracts and checks

REST revision 17, sync event revision 22 and web revision 36 gate the new surface.
No released migration changes; the new migration stamps the invalidation kind.
Generated outputs come from the real generators. Check both creation paths,
durability, concurrent writes, retry/revision conflicts, scope rejection, timezone
boundaries, month/day queries and real desktop interaction. Critical scope tests
are mutation-verified. Two independent full-diff reviews cover ownership,
duplication and application-specific assumptions.

The standard React calendar and its Temporal peer are pinned. Official CSS enters
through the existing vendor stylesheet; local theme adjustments remain scoped.
The dependency introduces four lockfile matches of a retired vocabulary token in
the Temporal runtime package name. The bounded baseline allowance is documented
in the terminology gate header; matching rules and scopes are unchanged.

## Planner experience review gate

Run this gate separately from the two code-review channels. Record the user
request, discovered tools, actual arguments and responses, persisted result, and
Planner confirmation. Separate scripted native-tool checks from live-model turns.
Use an isolated permitted host for live Codex; never enable real Codex E2E on the
shared production host.

Cover date-only creation, explicit timed creation, retry after response loss,
owner list/reschedule/cancel, a human edit causing a revision conflict, disabled
Calendar, a development-bound Planner, and a manually created task. Ask for a
missing timezone or timed end instead of inventing values. A Track-created
timed entry wakes that Track's Planner at its start (see Due-time wake); a
reminder for a human or for an all-day entry must still be declined clearly.

The current review found two unresolved product boundaries: a development-owned
Track cannot discover or call Calendar under the existing owner-only plugin-tool
policy, and human-created entries remain outside Planner's Track scope. These
are not fixed by bypassing the generic authorization boundary; a declarative
cross-plugin grant is needed before development-owned Planners can schedule.
Essential limitations and recovery instructions are included in the standard
per-tool prompt files, because component instructions are only injected for an
owning plugin. The three compact tool schemas and action-specific descriptions
add about 2.3 KiB to the Planner catalog; the aggregate budget is capped at
29,100 bytes, with the existing per-description bound retained.

## Due-time wake (#1967)

A timed entry created from a Track (`source_track_id` set) wakes that Track's
Planner once when it starts. Human entries, all-day entries and cancelled
entries never wake. Calendar declares a generic optional background hook on
its compiled component; boot starts every declared hook from the catalog, so
the kernel names no application. The Calendar scanner ticks every 30 seconds,
skips missed ticks, and acts only while Calendar is running.

For each due occurrence the scanner compares the start instant with the
entry's `fired:{entry_id}` cursor, a separate plugin record so a wake never
changes the version a user edit checks. In one immediate transaction it
advances the cursor and writes the generic Track-scoped kernel event
`track.wake_requested {track_id, source, key, text}`. The event is kernel-only
at the role gate, read back by Planner catch-up, and maps to a hard-fire wake
naming the entry, its local start and zone, and how late the wake is. Delivery
dedupe stays with the dispatcher watermark.

A wake missed while the server was down fires once on the next scan only while
the entry has not ended; an entry shorter than two scan ticks keeps that grace
after its start. After that deadline, or when the Track is closed or missing,
the cursor advances without an event. Moving an entry to a later start makes it
fire again at the new start. A wake reaches a running Planner: it is delivered on
its next turn and replayed after a restart. The kernel does not start Planners; a
wake that comes due before the Track's Planner has ever started is not delivered
when that Planner starts, though a later server restart may replay it through boot
catch-up with its original text. There is
no per-entry opt-out or holiday exception; the woken Planner decides to skip.
Weekly recurrence is a separate slice.

## Ownership change request and decision

The orchestrator approves the narrow Calendar feature registration, REST/event
contracts, invalidation policies, dependencies and official vendor styles under
#1913. Existing ownership enforcement and dependency rules remain unchanged.

```
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — expose builtin calendar scheduling contracts (#1913)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — export plugin data invalidation event (#1913)
OWNERSHIP-CHANGE: fe/core/api/schemas.ts — decode plugin data invalidation event (#1913)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.ts — refresh plugin data and lifecycle queries (#1913)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.test.ts — verify plugin query invalidation (#1913)
OWNERSHIP-CHANGE: fe/module-file-inventory.yaml — register the calendar feature owner (#1913)
OWNERSHIP-CHANGE: fe/package.json — add the standard event calendar and Temporal peer (#1913)
OWNERSHIP-CHANGE: fe/package-lock.json — pin the event calendar dependency graph (#1913)
OWNERSHIP-CHANGE: fe/web/src/styles/vendor.css — load official calendar CSS through the vendor layer (#1913)
```

## Local-time Planner acceptance

Issue #1942 removes offset arithmetic from Planner input without changing stored
schedule shapes. The Calendar native-tool regression covers local create/list,
retry, local update, and DST refusal without partial writes. Seven Calendar tests
and four MCP registry/prompt/budget checks passed; the existing catalog budget
remains unchanged.

A manual real-model probe reused the reviewed #1940 Docker acceptance harness
against this implementation. The Planner sent local 2026-10-02T09:00 and 10:00
with Asia/Shanghai; create and subsequent list returned matching persisted
RFC3339 values and the same ID. The probe completed in 36.19 seconds. The Planner
reported that it did not need to calculate UTC offsets. It still reported broad
tool-discovery results; this change does not claim to fix that discovery behavior.
The probe used an unbound Track and does not establish development-owner access
or reminders. Temporary acceptance instrumentation was restored after execution.

## Separate follow-up: always-enabled lifecycle

After #1936, Calendar becomes an always-enabled compiled component. A typed
compiled lifecycle policy supplies enablement and disable permission; external
manifest data cannot opt into it. Reconciliation enables new and legacy-disabled
Calendar rows without changing stored entries. User disable attempts are rejected,
and conflicting operator `plugins_disabled` configuration is refused explicitly.
Other builtins retain their optional lifecycle.

The plugin list declares required `can_disable` metadata. Settings uses that
contract to show Always on instead of a switch. REST revision18 and web revision37
gate matched clients. Non-running always-on rows offer Retry through the existing
enable endpoint without a disable action. This follow-up does not change Track/role tool admission,
associate manual tasks, or add reminder delivery. Those remain separate work.

Acceptance covers first boot, legacy disabled reconciliation, rejected disable
with retained data, unchanged optional builtin behavior, required metadata
decoding, and Settings controls. The orchestrator approves the narrow generated
OpenAPI and frontend compatibility contract updates under #1913.

Follow-up ownership records:

```
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — expose required builtin disable permission (#1913)
```

## Real Planner creation evidence

The Docker-isolated Codex runner executed a real unbound Planner with a Chinese
request to create 调研日历交互 for 2026-10-02 09:00–10:00 Asia/Shanghai and then
query it. The model invoked calm.calendar.create and calm.calendar.list itself;
its persisted creator/Track matched the live session, and it confirmed the saved
time. No injected execution plan was used. The successful run completed in
37.55 seconds after fixing the standalone Codex code-mode companion mount.
The network fence rejected production and private-network targets. This proves
creation/query for the unbound case, not development-owner access or reminders.
