# Using Neige Calm

This guide describes the maintained desktop frontend at `/next/` on `main`.
An older installation may not have these controls. Use
`/api/version` to identify its build and the [upgrade guide](deploy-and-upgrade.md)
to update it. For a fresh installation, start with the
[Linux Alpha runbook](alpha-release.md).

## Choose a Planner

In **New Track**, use the grouped model picker to choose a Codex or Claude
model. That choice also selects the Planner provider. Claude appears when the
server has configured its backend; installing the CLI alone does not enable it.
An existing Track keeps its Planner provider; its picker changes models within
that provider.

**Settings → Planners** shows **Ready**, **Unavailable**, or **Not configured**
with the server's reason when a provider is not ready. Choose **Recheck** after
fixing installation, version, or login problems. This pane reports readiness;
it does not install a CLI or sign in for you. Claude Track creation is refused
until Claude is ready. Codex creation can still succeed while unavailable,
but its Planner cannot run until the provider is ready.
See [Claude Planner setup](neige-app-config.md#enable-a-claude-planner).

## Create a Track from a Recipe

In **New Track**, open the Template picker. You can choose a built-in template,
one of **My recipes**, or **No template**. The **Manage recipes…** entry opens
the Recipe list even when you have not saved a Recipe yet.

Choose **New recipe**, enter a title and Markdown body, then **Save**. An existing
Recipe offers **Edit** and **Delete**. Write the working method, constraints,
and expected Report format in ordinary Markdown; see
[Recipe body format](recipe-body-format.md). Working instructions can go in a
closed HTML comment so the Planner receives them without showing them in the
Report. Do not preallocate generic task steps: the Planner creates concrete
tasks when the actual request calls for execution.

After saving, the editor shows the server's normalized body. If another window
saved first, the revision conflict preserves your draft; copy anything you need,
then close and reopen the Recipe to work from the current version.

Return to New Track and select the saved Recipe. Its working instructions are
snapshotted for the Planner at creation; prose and non-task blocks seed the
Report. Old task fences remain reference material and do not create queued
tasks. Editing the source Recipe later does not change an existing Track's
snapshot. The first message in the composer goes with the create request;
send it once. Built-in templates remain read-only, and the picker does not
offer to duplicate a built-in template into a Recipe.

## Reference tags, reports, and blocks in chat

In the Planner composer, type `@` to open suggestions from the current Area.
Add a prefix to show only one kind of reference, then type a search term:

| Input | Candidates | Example |
| --- | --- | --- |
| `@` | Tags, Track reports, and report blocks | `@release` |
| `@#` | Report tags | `@#release` |
| `@/` | Track reports | `@/release` |
| `@>` | Individual report blocks | `@>Findings` |

Each prefix also works without a search term to show recommendations. Select
an entry to insert a reference chip; the message carries the corresponding
report reference for the Planner to read. These are references within the
Area, not user mentions or host filesystem paths. Chinese input punctuation
is supported too: `@＃`, `@、`, and `@》` select the same three kinds respectively.

## Close and find Tracks

Tracks are **open** or **closed**. Use **Close** on the Track page when you are
finished and **Reopen** before scheduling more work. Sending a message to a
closed Track does not reopen it; its Planner cannot schedule until you reopen.

The desktop rail shows an Area's five most recent visible Tracks, followed by
**Show N more**. Closed Tracks are hidden unless unread or currently open.
Use the Area's menu **Show closed** / **Hide closed** to change this preference.
The mobile Area page and Today do not apply this desktop hiding rule.

## Supervise tasks and respond to requests

The desktop task list shows aggregate counts and a short status on each row.
Hover a status for its full reason. Select a task row to reveal its declaration
in the Report; use its task-kind button to reveal the worker card when one exists.
The Notification Center shows requests addressed to you and Planner failures,
and opens the relevant conversation or card. Dismiss a notification after
handling it; dismissal clears its attention marker, not the underlying task
or request.

Codex and Claude tasks of a Track run one at a time in the Track's checkout,
except that tasks declared `access: "read_only"` (e.g. reviews, investigations) run beside
each other. A read-only task may also declare `head` (and `base`), full commit
ids: it starts only while the checkout is at `head`, and its worker is told the
repository, checkout, head and base. A task waiting for the checkout shows
*Waiting for the track's checkout*.
Terminal and child-track tasks are not held.

Worker cards and verification terminals display their directories separately.
A gate uses its explicit directory override when supplied, otherwise the bound
worker execution's persisted directory before task/Track defaults. A missing
bound workspace fails verification instead of silently checking another tree.
Codex and Claude workers run in the Track's own checkout; recovery of older
frozen operations keeps their recorded directory.

## Read a task's execution history

Expand the task in the Report to see its current attempt and **Attempt history**.
Each historical attempt keeps its outcome and a link to its worker conversation.
A failed task stays failed: to try again, declare a new task under a new key.

## Work in a Track checkout and publish a PR

A newly created Track attached to a Git repository gets its own worktree under
`.claude/worktrees/track-<track_id>` on branch `neige/track-<track_id>`. The
Planner and ordinary Codex/Claude workers use that checkout. Commit or undo
uncommitted changes before starting a worker; a dirty checkout is refused.
Older attached Tracks without a Track worktree need a new Track to run these
workers. Managed Tracks use their provisioned workspace.

Enable **development** in **Settings → Plugins** before creating an Issue development Track.
The Issue template automatically includes its development guide. Use `@` in the
chat input to attach a plugin's brief description; disabled installed plugins
can also be referenced. A reference supplies context and does not enable the
plugin or change its permissions.

For a development-bound Track with its own worktree and an upstream, the Planner can
publish through `neige.track.publish`: the kernel pushes the branch without
forcing and opens or reuses its PR. The branch tip must be the candidate of a
**done** task attempt. A later unverified commit cannot be published through
this tool. Publication returns the PR link; merging remains a separate action.

## Open files from a Report

Link to a file with ordinary Markdown, for example
`[Notes](notes.md)` or `[Source](src/main.rs)`, relative to the Track workspace.
Selecting a supported local file link opens the file inside the Track; recently
opened files are available from its recent-files surface. Markdown, text, and
supported image formats have viewers.

Absolute paths inside the same workspace can also resolve. Paths outside the
Track's workspace and symlinks that escape it are rejected. This is a viewer for
files in the service's workspace, not a browser upload or an unrestricted host
file browser. Missing files produce a read error. Line/column suffixes and URL
fragments are stripped when resolving a file; they do not select a line in the
viewer.

## View a live development preview

A Planner can register a development server listening on `127.0.0.1` and add a
`preview` block to the Report. Ask it to start the server, register it with
`neige.preview.register`, and embed the returned preview key. The preview offers
**Desktop** and **Mobile** viewports and fullscreen. An offline notice means
the target server is not responding; a missing-registration notice means the
Planner needs to register it again.

The operator must first configure a preview port pool; see
[Preview gateway configuration](neige-app-config.md#enable-report-previews).
The browser connects to a separate port on the same host using the owner's
session. Previews currently require direct HTTP access; HTTPS pages show
“预览仅 LAN 可用” instead of loading the frame. Registrations are held in
memory and must be recreated after a kernel restart. The development server
must also remain running; registration does not supervise its process.

## Agent CLI and Report editing

`neige` forwards commands to the running kernel from an authenticated agent
terminal. Even `neige --help` needs `NEIGE_MCP_SOCKET` and `NEIGE_MCP_TOKEN`;
only `neige --version` works without a connection. Use the `neige` binary
shipped beside the running kernel; old clients are refused with the correct
path in the error.

A Planner can read other reports through the read-only `area/reports/` view,
inspect selected blocks with `neige cat report.md --blocks <id>`, and query
report tags with `neige tag report.md`. Report writes use `neige.report.commit`
for targeted edits or `neige.report.write` for whole-document changes.
Both anchor to this session's prior `neige.report.read`; CLI reads do not establish
that write anchor.

## Add and configure plugins

Open **Settings → Plugins → Add a plugin**. Choose a source:

| Source | What to provide |
| --- | --- |
| **Remote MCP server** | Paste the server’s MCP configuration JSON. Tool access defaults to the complete catalog. |
| **Server directory** | A directory containing `manifest.json` on the machine running Neige Calm. This is not a directory on the browser's computer. |

Update the server and app together before using JSON setup; older servers
cannot interpret its new configuration fields. The web-only upgrade preflight
rejects this mismatch, and older servers refuse the new install request type.

Paste a direct `{"url":"https://example.com/mcp","headers":{}}` object,
Claude-style `mcpServers`, or VS Code-style `servers`. If there is more than one
server, choose the one to add. The name and stable plugin ID are filled in
for you. **Advanced settings** lets you change them or choose **Selected tools**
and enter exact tool names separated by commas, spaces, or newlines.
An empty selected list is refused; **All tools** is the default.

**Check connection** is optional. It contacts the server using the current
unsaved configuration, reads every page of its tool catalog, and shows the
count and names. It does not install, enable, or call a business tool. A failed
check leaves your draft intact. Editing the draft invalidates the result.
A successful check confirms discovery, not that every tool call will succeed.

Only remote HTTP / streamable-HTTP is supported. Local commands (stdio), SSE,
OAuth, helper commands and unresolved variables are refused. Use literal
`headers` for authentication, for example `"Authorization": "Bearer your-key"`;
multiple custom headers are supported. Since all header values are private,
they use the existing credential rules: at least eight printable ASCII
characters, no spaces/quotes/backslashes, and not a JSON number. For
`Authorization`, these rules apply to the token after the scheme (such as
`Bearer`). Empty values, leading/trailing whitespace, and values that overlap
the redaction marker are refused by both Check and Add. Short ordinary header
values are not supported by this connector’s redaction contract.
Transport-controlled headers such as
`Host`, `Content-Length` and `Mcp-Session-Id` cannot be supplied.
Do not put credentials in the URL. Header values stay in memory until Add and
are then stored privately on the server, never in the public manifest or a
browser cache. Remove and re-add a connector to change its stored credentials.

The kernel refreshes the complete catalog on each enable/reload; it does not
update an existing conversation live. Selected tools remain a strict allowlist.
Unknown selected names are skipped with a server warning.
Existing manifests and API callers keep their original semantics: omitted or
empty `tools_allow` exposes nothing unless `tools_all: true` is explicit.

Select **Add plugin**, return to the list, and enable its switch. New plugins
are installed disabled. Check the resulting status and any error before use.
Plugin enable/disable does not refresh an existing conversation's tool list;
start a new conversation to use the updated set.

A plugin with a configuration schema has a **Configure** action. **Save** stores
edited values; **Apply & restart** makes them live by restarting the plugin.
Review its outcome: a saved configuration is not proof that the restart worked.

**Remove** asks for confirmation in the plugin's row. Removal deletes its stored
configuration. For a remote connector created by the form, it also attempts to
delete the kernel-created directory and saved key. Filesystem cleanup failures
are logged under `plugin_host` without failing the removal response; check the
log and remove any residual connector files if cleanup failed. A local-path
plugin's operator-owned source tree remains on disk. Disabling a plugin retains
its configuration.
Read [Plugin host security](plugin-security.md) before installing local code.

## Configure provider networking

If the service needs a proxy, set **Settings → Network → HTTP proxy / HTTPS
proxy** before the first agent task, then start a new Track or conversation.
Fields save when you leave them or press Enter. A systemd installation captures
PATH at installation but does not inherit your interactive shell's proxy
variables. Existing running cards keep their launch configuration; see
[Alpha network setup](alpha-release.md#network-setup-before-the-first-agent-task)
for diagnostics when a task waits without a reply.
