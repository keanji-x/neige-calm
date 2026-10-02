# Calendar commitments

Calendar is a compiled plugin (`dev.neige.calendar`), disabled on first install
like other builtins. Its own module owns validation, namespaced persistence,
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
missing timezone or timed end instead of inventing values. A reminder request
must be declined clearly unless a real delivery path exists. Future reminder
acceptance must observe the due-time wake-up, recipient, retries/deduplication,
rescheduling and cancellation; a stored timestamp does not satisfy that check.

The current review found two unresolved product boundaries: a development-owned
Track cannot discover or call Calendar under the existing owner-only plugin-tool
policy, and Calendar has no due-time delivery/wake-up mechanism. Human-created
entries also remain outside Planner's Track scope. These are not fixed by
bypassing the generic authorization boundary. A declarative cross-plugin grant
and a separately designed durable reminder contract are needed before claiming
that normal development Planners can schedule and receive reminders end to end.
Essential limitations and recovery instructions are included in the standard
per-tool prompt files, because component instructions are only injected for an
owning plugin. The three compact tool schemas and action-specific descriptions
add about 2.3 KiB to the Planner catalog; the aggregate budget is capped at
29,100 bytes, with the existing per-description bound retained.

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
