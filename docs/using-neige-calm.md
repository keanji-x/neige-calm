# Using Neige Calm

This guide describes the new desktop frontend at `/next/` on `main`, including
the Settings plugin installation and worker verification fixes merged on
2026-09-05. An older installation may not have these controls. Use
`/api/version` to identify its build and the [upgrade guide](deploy-and-upgrade.md)
to update it. For a fresh installation, start with the
[Linux Alpha runbook](alpha-release.md).

## Create a Track from a Recipe

In **New Track**, open the Template picker. You can choose a built-in template,
one of **My recipes**, or **No template**. The **Manage recipes…** entry opens
the Recipe list even when you have not saved a Recipe yet.

Choose **New recipe**, enter a title and Markdown body, then **Save**. An existing
Recipe offers **Edit** and **Delete**. The body uses ordinary Markdown plus
`neige-block task` fences; use the examples in the
[Recipe body format](recipe-body-format.md). Agent tasks use `goal`; terminal
tasks use `command`. A malformed fence can be saved as ordinary prose, so a
successful save alone does not prove that the Recipe contains runnable tasks.

After saving, the editor shows the server's normalized body. If another window
saved first, the revision conflict preserves your draft; copy anything you need,
then close and reopen the Recipe to work from the current version.

Return to New Track and select the saved Recipe. Its title and body seed the new
Track's Report and tasks. The first message in the composer goes with the create
request; send it once. Built-in templates remain read-only, and the picker does
not offer to duplicate a built-in template into a Recipe.

## Supervise tasks and respond to requests

The desktop task list shows aggregate counts and a short status on each row.
Hover a status for its full reason. Select a task row to reveal its declaration
in the Report; use its task-kind button to reveal the worker card when one exists.
The Notification Center identifies which Planner,
Assistant, or Worker needs attention and opens its conversation or card.

**Settings → General → Task concurrency** sets the default number of concurrently
admitted tasks per Track. Commit the positive integer by pressing Enter or
leaving the field. The deployment fallback is one; a Track-specific budget and
server capacity limits still apply. Raising the default can release pending
work; lowering it does not interrupt already running tasks.

Worker cards and verification terminals display their directories separately.
A gate uses its explicit directory override when supplied, otherwise the bound
worker execution's persisted directory before task/Track defaults. A missing
bound workspace fails verification instead of silently checking another tree.
New Claude worker operations provision Git worktrees; recovery of older frozen
operations keeps their recorded directory.

## Recover a failed task

Expand the task in the Report to see its current attempt and **Attempt history**.
Each historical attempt keeps its outcome and a link to its worker conversation.
Select **Recover task** when the current failed attempt is eligible. Recovery
starts a new execution under the same task key and unchanged requirements;
completed sibling tasks and downstream dependency declarations stay in place.

The initial recovery entry supports preparation failures where no Worker or
verifier was prepared or started. An exited terminal or completed provider session
does not prove its background processes stopped. Attempts that reached execution
remain unavailable through this entry until a supported stop boundary is available;
the task explains this prerequisite.

The accepted request first waits for preparation and scheduling. A prepared or
queued attempt has not necessarily begun business execution. If the request's
response is lost, retry the request through the displayed recovery control; it
keeps the original request identity so that one action cannot create two attempts.

When recovery is unavailable, the task explains the prerequisite: for example,
its requirements changed, execution permission was withdrawn, the previous execution
has no supported write-stop proof, or its historical contract is missing. A terminal Track
must first use the existing **Resume work** action. An ordinary Blocked Track can
return to Working as part of an admitted recovery.

Planner can recover its own automatically admitted task once. User-owned tasks,
tasks awaiting user release, and further failed attempts need an explicit user
recovery action. Successful and canceled attempts are not eligible for this action.

This recovery starts a fresh execution. Restoring a failed workspace, exact
artifact handoff and partial-result acceptance have separate delivery requirements
in [Task continuity](architecture/1501-task-continuity.md#reliable-delivery).

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

## Add and configure plugins

Open **Settings → Plugins → Add a plugin**. Choose a source:

| Source | What to provide |
| --- | --- |
| **Remote MCP server** | Paste the server’s MCP configuration JSON. Tool access defaults to the complete catalog. |
| **Server directory** | A directory containing `manifest.json` on the machine running Neige Calm. This is not a directory on the browser's computer. |

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
multiple custom headers are supported. Transport-controlled headers such as
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
