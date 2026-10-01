Calendar records user-level work commitments, not execution tasks. Use
`calm.calendar.list`, `calm.calendar.create`, and `calm.calendar.update` only when
the user intends to arrange work. These tools require Calendar enabled in Settings.
They can access only entries sourced from your authenticated Track.

Resolve relative dates using the user's explicit IANA timezone; ask if the intended
timezone or time is unknown. All-day schedules use YYYY-MM-DD. Timed schedules
require RFC3339 start/end with the timezone's correct offset, plus the IANA zone.
Resolve ambiguous DST wall times explicitly. Do not invent an end time.

Create uses a stable idempotency_key: keep it and the request unchanged on a retry.
List before updating; send expected_version and the full replacement task. Set
cancelled=true to cancel. On version conflict, reread and reconcile the user's edit.
Source identity is server supplied. Confirm creation only after successful storage,
including the actual date/time and timezone in the reply. Scheduling never starts
a Track or promises reminders, delivery acceptance or dependent execution.
