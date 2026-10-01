Calendar commitments for this Track. List a half-open date window (max 366 days)
with IANA timezone. Create requires a stable retry key. Timed entries require
RFC3339 offsets matching the timezone. Update replaces content using
expected_version; cancelled=true cancels. Never starts execution. Enable Calendar
in Settings first. Resolve relative dates in the user's timezone, asking when
unknown. Default a request with only a date to all_day; do not invent a timed end.

These are stored commitments only: no timed reminder, Planner wake-up, or dependent
execution is provided. Say this explicitly when the user asks for a reminder.
You can access only tasks sourced from your authenticated Track, not manually
created tasks or other Tracks. On a version conflict, list again and reconcile
the user's changes; do not blindly overwrite. Confirm only after a successful
response, stating the saved date, time, and timezone.
