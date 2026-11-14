# `features/area`

Area is a workspace grouping, not a page or a conversation scope.

| Module | What it is |
| --- | --- |
| `palette.ts` | INV-DUP-006 — the canonical Area colour table, consumed by AppShell's shared new-Area flow. |
| `editor/public.tsx` | `<AreaEditorForm>` — the shared New/Edit form for name and optional directory; hosted in the shell's Dialog. |
| `default-pills/public.tsx` | The shared borderless Folder control used by both Area editor and New Track composer. |
| `new-track/public.tsx` | `<NewTrackForm>` — the route-owned create composer for a Track in one Area. |

The desktop rail owns Area disclosure and management. The muted Area row
expands or collapses its Tracks immediately on click and never navigates. Edit
in the permanently visible actions menu opens the Area editor Dialog. The
permanent `+` navigates to `/area/{id}/new`; deletion remains behind the typed
confirmation.

The same Dialog creates and edits an Area with a name and optional folder.
Neither operation reads templates. Editing changes only name and directory;
an existing `default_template_id` is preserved by omitting it from the patch.
An empty folder gives each new Track its own managed Neige workspace. A saved
folder is the exact directory preselected for an attached Track. The opening
name and directory snapshot stays fixed while the draft is open.

On mobile, the Areas sheet keeps the same hierarchy as a list drill-in:
`Areas → Tracks → Track page`. Its root header opens the same New Area Dialog,
and a selected Area's header opens the same editor; Area settings are not a
desktop-only capability.

## Create: a sentence and optional project, model and role

The surface is `/area/{id}/new`; `area_id` comes from the URL. Submit requires
a nonblank message. New drafts never select templates or recipes, read their
rosters, or inherit an Area template default. Only the directory starts from
the Area snapshot. The ordinary create omits `template_id`, `template_input`
and `recipe_id`. An already submitted request retains its original body and
idempotency key on retry, including any legacy template fields.

`NewTrackDraft.message` is the Track's intent, not its title. The route puts it
on the create as `first_message` (#1299), which delivers it to the planner agent
inside the harness-start transaction; a blank draft omits the key rather than
sending an empty string the kernel would reject. The route still opens that
conversation on arrival — now for the reply.

Blank is `isBlankForKernel` (`core/domain/track.ts`) — the kernel's Unicode
`White_Space` criterion, not JS `trim()`, which disagrees about `U+0085` — and
both the composer's submit gate and the route's spread ask that one function.
What is *sent* is never trimmed: the kernel forwards the text to the agent
verbatim, so the whitespace around the sentence is the reader's.

With no folder, the request omits both `cwd` and `attach_folder` and the kernel
creates a managed workspace. Accepting an inherited Area folder or choosing a
different one sends `{ cwd, attach_folder: true, isolation: 'none' }` and attaches the Track to a
directory the kernel does not own.

The folder picker remains a route-local `Dialog` around `DirectoryBrowser` so
it does not expand inline beneath the composer.
