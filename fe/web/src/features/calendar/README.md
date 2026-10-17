# Calendar presentation

`CalendarTasks` owns the light panel, Week/Month choice, selected-day list and
captured editor draft. Queries, date selection, write admission and keyed retries
remain in the app's Calendar adapter. These controls do not wait for grid code.

Only `calendar-view.tsx` imports FullCalendar. `grid-loader.ts` requests that
module when the Calendar feature mounts, independently of plugin availability:
date/Track navigation still works with the task plugin disabled. The latest
selected date, timezone and retained view mode reach the mounted grid. Once ready,
view choices call the grid's narrow handle in the original event, using the public
FullCalendar changeView API; there is no post-commit effect replaying the command. Its real
`datesSet` callback remains the only source of the month query window.

An effect owns each module attempt and ignores completion after retry/unmount.
A failed grid has its own retry; day reads and the task editor keep their existing
error and write policies. Retry invokes the same public dynamic import again;
a persistent deployment/module failure may still fail and remains visible.
The browser owns module reuse; there is no feature cache or timer. Loading code
and reading month data share one grid status, while day reads remain separate.

Tests: `app/router/calendar-loading.browser.test.tsx` holds/rejects only the
loader and resolves it to the real grid; `calendar.browser.test.tsx` covers actual
range/navigation behavior; `calendar-writes.test.tsx` and this feature's
`public.test.tsx` retain task draft, revision and recovery coverage.
