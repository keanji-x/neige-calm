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

These execution fixes do not provide structured checkpoints or attempt-aware
resume/partial acceptance. Those remain follow-ups in
[Long task reliability](architecture/long-task-reliability.md#delivery-scope).

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
| **Remote MCP server** | Name, unique plugin ID, streamable-HTTP endpoint, and the exact tool names to expose. Add an API key only if the server requires one. |
| **Server directory** | A directory containing `manifest.json` on the machine running Neige Calm. This is not a directory on the browser's computer. |

For a remote server, **Tools** is a strict allowlist, separated by commas,
spaces, or newlines. The form requires at least one name; an empty list never
means “all tools.” Check names against the upstream server: an unknown name is
skipped with a server warning, and a plugin shown as `running` does not prove
that every requested tool was found.

The optional key uses **Authorization: Bearer** or **Custom header** placement;
for a custom header, enter its name and supply the raw key. Do not put credentials
in the URL. The key is stored on the server and is not shown again by the UI or
returned in the install response. There is no stored-key editor: remove and
re-add the connector to replace it.

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
