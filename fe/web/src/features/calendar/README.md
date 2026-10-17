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

One mounted effect requests the grid module and ignores completion after unmount.
A failed native import can stay rejected in the current document even after the
network recovers. The error therefore offers **Reload page**, through the app's
required document-reload capability, rather than repeating the import. It explains
that unsaved changes are cleared. Reload is disabled while a Calendar editor is
open or a task write is pending; day tasks, New task, and their existing error/write
policies remain usable. Reload is explicit and does not promise that a persistent
server/deployment failure will recover. There is no cache busting, automatic
refresh, feature cache or timer. Loading code and reading month data share one
grid status, while day reads remain separate.

Tests: `app/router/calendar-loading.browser.test.tsx` holds/rejects only the
loader and resolves it to the real grid; `calendar.browser.test.tsx` covers actual
range/navigation behavior; `calendar-writes.test.tsx` and this feature's
`public.test.tsx` retain task draft, revision and recovery coverage.

`grid-reload.test.tsx` keeps module rejection persistent and verifies explicit
reload dispatch and editor/write protection. The native loading test no longer
models recovery by resolving a second mocked import. Real production verification
must fail the actual chunk, restore its serving path, and use Reload page to prove
a new document requests the same module URL.
