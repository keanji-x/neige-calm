Calendar records user-level work commitments, not execution tasks. Use
`neige_calendar_ls`, `neige_calendar_add`, `neige_calendar_set` and `neige_calendar_rm` only when
the user intends to arrange work. Calendar is always enabled by the kernel.
They can access only entries sourced from your authenticated Track.

Resolve relative dates using the user's explicit IANA timezone; ask if the intended
timezone or time is unknown. All-day schedules use YYYY-MM-DD. Timed schedules
prefer local start/end in YYYY-MM-DDTHH:mm plus the IANA zone; Calendar computes
the offset. RFC3339 with a correct explicit offset is also accepted. Resolve
ambiguous DST wall times with an explicit offset. Do not invent an end time.
Weekly schedules repeat local HH:MM start..end (same day) on the listed weekdays
from one date through an optional last date; a wall time skipped by DST skips that
day and a repeated one takes the earlier instant. List returns each entry's
occurrences in the window [from, to).

Add uses a stable idempotency_key: keep it and the request unchanged on a retry.
List before a set or rm; send the entry_id and expected_version, and for set the full
replacement task. Rm has no undo. On version conflict, reread and reconcile the user's edit.
Source identity is server supplied. Confirm creation only after successful storage,
including the actual date/time and timezone in the reply. Each occurrence of a
timed or weekly entry you create wakes this Track's Planner once when it starts; a
wake missed while the server was down fires only before that occurrence ends.
All-day entries never wake. A later start wakes again; rm stops the wakes.
Scheduling never starts a Track or dependent execution.
