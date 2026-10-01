# Calendar time scheduling

Calendar is a compiled plugin (`dev.neige.calendar`), disabled on first install
like other builtins. Enabling it is an explicit user action. Its own module
owns validation, namespaced persistence, HTTP handlers, and native AI tools.
The catalog registers HTTP routers without application-ID dispatch in the kernel.
Today composes a calendar feature on desktop only; page loads only read.

Calendar entries are work commitments, separate from execution Tasks and Track
lifecycle. This slice stores dates and time ranges only: no execution, dependency,
reminder or delivery workflow is triggered. Source Track identity is attribution.

Use existing plugin KV storage, one versioned record per entry and immutable
creation receipts for idempotency, inside the normal transaction write boundary.
No released migration changes; a new migration records sync event revision 22. A generic `plugin.data.changed` event contains
only plugin identity; the owning component publishes it with the transaction.
Clients invalidate that plugin's data queries, never infer calendar policy.

Humans can access all calendar entries through session-protected REST. AI tools
are exposed to Planner and Assistant roles, subject to existing plugin admission
and track scope. AI reads/writes only entries belonging to its authenticated
Track; a launchpad assistant has the same restriction, not implicit global power.
Human-created entries have no source Track. Neither REST nor tool arguments may
forge the source or creator. Worker roles cannot call these tools.

Time contracts distinguish a civil all-day date from an RFC3339 instant range
with an explicit IANA timezone. Offset/timezone mismatches are rejected; ambiguous
wall times must carry their chosen offset. Queries use a half-open date window
and an explicit display timezone. Updates compare revisions; cancelled entries
remain durable. Lost create responses can be retried using the same key and body.

Acceptance follows #1913: both creation paths, durability, idempotency conflicts,
revision conflicts, time boundaries, scope rejection, lifecycle gating, desktop
UI and real browser interaction. Existing report/conversation UI remains composed
alongside the calendar. Task dependencies and deliverable acceptance remain future work.

The UI reuses Today's existing sidebar calendar as its only date selector.
Today owns the selected date; the injected task agenda uses it for queries and
creation. Entry/edit controls use Astryx TextInput, DateInput, TimeInput, TextArea
and Button. There is no separate month calendar in the main column.
Creating an all-day entry needs only its title on the selected date. Time and
notes expand on request; timezone and cross-day end date are secondary controls.
The display timezone comes from the device and is visible next to the selected day;
a timed edit retains its stored timezone and explicit offset.

Owner scope correction: mobile scheduling is deferred. The compact Today viewport
excludes the Calendar task slot and retains its prior date-only presentation.
A browser check pins the absence of the task surface on compact viewports.

## Ownership change request and decision

The orchestrator approves these narrow contract changes under #1913: register the
Calendar feature owner, expose the plugin-owned REST schemas, add the opaque
plugin data event to the generated/client event union, and invalidate the owning
plugin's query prefix. Existing ownership enforcement and dependency rules remain
unchanged. All generated outputs come from the real generators.

The schema/event/ownership commit carries these exact trailers:

```
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — expose builtin calendar scheduling contracts (#1913)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — export plugin data invalidation event (#1913)
OWNERSHIP-CHANGE: fe/core/api/schemas.ts — decode plugin data invalidation event (#1913)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.ts — refresh plugin data and lifecycle queries (#1913)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.test.ts — verify plugin query invalidation (#1913)
OWNERSHIP-CHANGE: fe/module-file-inventory.yaml — register the calendar feature owner (#1913)
```
