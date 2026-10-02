Calendar records user-level work commitments, not execution tasks. Use
`calm.calendar.list`, `calm.calendar.create`, and `calm.calendar.update` only when
the user intends to arrange work. Calendar is always enabled by the kernel.
They can access only entries sourced from your authenticated Track.

Resolve relative dates using the user's explicit IANA timezone; ask if the intended
timezone or time is unknown. All-day schedules use YYYY-MM-DD. Timed schedules
prefer local start/end in YYYY-MM-DDTHH:mm plus the IANA zone; Calendar computes
the offset. RFC3339 with a correct explicit offset is also accepted. Resolve
ambiguous DST wall times with an explicit offset. Do not invent an end time.

Create uses a stable idempotency_key: keep it and the request unchanged on a retry.
List before updating; send expected_version and the full replacement task. Set
cancelled=true to cancel. On version conflict, reread and reconcile the user's edit.
Source identity is server supplied. Confirm creation only after successful storage,
including the actual date/time and timezone in the reply. A timed entry you create
wakes this Track's Planner once when it starts; a wake missed while the server was
down fires only before the entry ends. All-day entries never wake. A later start
wakes again; cancelling stops the wake. Scheduling never starts a Track or
dependent execution.
