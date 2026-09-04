# `features/area`

Area is a workspace grouping, not a page or a conversation scope.

| Module | What it is |
| --- | --- |
| `palette.ts` | INV-DUP-006 — the canonical Area colour table, consumed by the sidebar's new-Area flow. |
| `new-track/public.tsx` | `<NewTrackForm>` — the route-owned create composer for a Track in one Area. |

The desktop rail owns Area disclosure and management. Selecting an Area expands
or collapses its Tracks; it never navigates. Double-click (or F2) renames, hover
reveals delete, and the permanent `+` navigates to `/area/{id}/new`.

On mobile, the Areas sheet keeps the same hierarchy as a list drill-in:
`Areas → Tracks → Track page`.

## Create: a sentence, optionally a template and folder

The surface is `/area/{id}/new`; `area_id` comes from the URL, not from a form
field. Submit is enabled iff the composer is non-empty. Template and folder are
optional and default to nothing.

`NewTrackDraft.message` is the Track's intent, not its title. The route puts it
on the create as `first_message` (#1299), which delivers it to the planner agent
inside the harness-start transaction; a blank draft omits the key rather than
sending an empty string the kernel would reject. The route still opens that
conversation on arrival — now for the reply.

With no folder, the request omits both `cwd` and `attach_folder` and the kernel
creates a managed workspace. Choosing a folder sends `{ cwd, attach_folder:
true }` and attaches the Track to a directory the kernel does not own.

The folder picker remains a route-local `Dialog` around `DirectoryBrowser` so
it does not expand inline beneath the composer.
