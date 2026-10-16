# Start with one repository

[简体中文](getting-started.zh-CN.md)

This guide is for a developer who already uses Codex or Claude. Start with a
repository investigation, then try a small change or a pull request.

## Install the Linux package

Use a reviewed Linux Alpha archive for your CPU architecture and its checksum.
The package needs no Rust, Node.js, Go, Docker, or systemd at runtime. You need
Git, ripgrep (`rg`), `zstd`, a browser, and an authenticated coding CLI available
to the same OS user who starts Neige. Linux CLI sandboxing also needs bubblewrap
(`bwrap`). On Debian/Ubuntu these runtime tools are packaged as `git ripgrep zstd
bubblewrap`. Codex also needs the companion executables from its installed
distribution. Linux is the packaged target; the limited macOS source build is
documented in the [README](../README.md#macos-local-builds-limited).

Check the archive with its supplied `.sha256` file, extract it, and enter the
extracted directory. Run:

```bash
./release/bin/neige-app system setup --package ./release
```

Choose your local Neige password when prompted. This is separate from your
coding CLI login. Setup verifies the package, creates a private installation,
and prints the exact foreground start command and browser URL. Run that command
in the same terminal and keep it running. Open the printed URL and sign in as
`owner` with your chosen password.

By default the installation lives under `~/.local/share/neige-app`, including
its configuration, releases, data, plugin storage and helper binaries. To use a
different **new, absolute** directory, add `--root /absolute/path/neige` to setup.
Setup refuses existing installations; use the [upgrade guide](deploy-and-upgrade.md)
for those. It does not overwrite your previous work or register a systemd unit.
For automation, `--password-file /absolute/private/file` reads the password
without putting it on the command line; keep that file readable only by you.

If you do not have an archive, the maintainer can build one with the
[Alpha packaging runbook](alpha-release.md). The [source preview](../README.md#source-preview)
is available for contributors.

## Prepare the connection and check the agent

If this machine needs a proxy, first set **Settings → Network → HTTP proxy /
HTTPS proxy**. Leave each field or press Enter to save. Use an address reachable
from the machine running Neige. For this first setup, stop the foreground
service with Ctrl-C, wait for it to exit, then run the saved start command
again after saving the network settings. This gives the shared agent process the saved configuration;
it does not repeat setup or remove your data.

Configure network settings before starting work. For later changes, let active
work finish first, save the settings and restart with the same command. Live
proxy changes can leave an old Planner turn waiting; keep its Track and saved
work, restart the service and continue there. This recovery limitation is tracked
in [#2693](https://github.com/keanji-x/neige-calm/issues/2693).

Then open **Settings → Planners**. At least one provider must show **Ready**
before you send your first request. **Recheck** performs a fresh check after you
fix a login, executable or configuration problem. Opening the website confirms
the local service is running; it does not confirm model connectivity.

Codex uses the service user's installed CLI and login. Claude needs an explicit
[Planner configuration and dedicated login](neige-app-config.md#enable-a-claude-planner);
having `claude` on PATH alone does not enable its Planner. Choose a ready model
from the corresponding provider in New Track; the initial default is Codex.

A timed-out sign-in check does not prove you are signed out. Check that the same
OS user can run the installed CLI and is logged in. If you just changed proxy
settings and Recheck still times out, restart with the saved foreground command
and Recheck once more. Keep the error visible if it persists; creating duplicate
Tracks will not repair the connection. A supervised service may not inherit
proxy variables from your interactive shell.

## Get your first report

1. Click **Create your first area**, name it after your project, and save.
   An **Area** groups related work; one project is a useful starting point.
2. Click **＋** beside the Area to create a **Track**: one investigation, change,
   or continuing line of work. Choose **Folder** and select a local Git checkout.
   This is a directory on the machine running Neige, not a browser upload.
3. Choose **Investigation** from **Template**, then a ready Planner model.
   A template supplies a working method and the report's starting structure.
   The text you send supplies the actual job.
4. Enter a bounded request and click the create arrow once:

   > Read this repository. Explain its purpose, main directories and one key
   > execution flow. Link the relevant files in the report and suggest three
   > questions worth investigating. Do not change project code or open a PR.

5. Read the **Report** for the current findings and linked evidence. Use the
   Track's conversation to ask for clarification or continue the investigation.
   The first request is already sent when the Track is created.

Success means you have a useful report with file evidence, can refresh the
browser and find it again, and can continue in the same Track. Neige prepares
the Track's checkout; you do not need to create its worktree yourself.

## Choose your next job

| Your goal | Starting point | Expected result |
| --- | --- | --- |
| Understand code or investigate a question | Investigation | Findings and evidence |
| Make one focused repository change | Small change | A change and the checks that actually ran |
| Deliver a request or Issue through a PR | Development | Implementation, verification, review and authorized delivery |
| Explore freely without a prescribed method | No template | Work guided by your request |

For a **Small change**, select the repository and say what should change and how
you will recognize success. For example: “Correct the usage example in README,
verify the example against the implementation, and report the change. Keep the
result local; do not publish a PR.” Repository instructions still apply.

For **Development**, first enable **development** in **Settings → Plugins**.
The picker reports when that capability is required; return to your draft after
enabling it. Publishing PRs also needs an authenticated `gh` and `jq` on the
service's PATH, an appropriate remote and permission to publish there.

Development saves worker results as Git commits. Its Planner checks the Git
author and committer before implementation. If either identity is missing,
configure your usual Git name/email as the service user, or explicitly authorize
the Planner to configure an identity for this repository. From that repository,
`git var GIT_AUTHOR_IDENT` and `git var GIT_COMMITTER_IDENT` check the current
identity. Investigation does not require a commit identity.

Choose the repository, describe the outcome and acceptance, and optionally paste
a GitHub **Issue URL**. An Issue is not required. When supplied, its repository
must match the checkout. Leave **Merge automatically** off to review the result
before approving a merge. The template follows the target repository's
`AGENTS.md` and `CONTRIBUTING.md` for development and verification.

### If you may want a PR later

Choose **Development** from the start when PR delivery is likely, even without
an Issue. You can ask for local implementation and verification first, then
request publication when ready. **Small change** is a useful local-edit path;
its result may remain uncommitted in that Track's checkout.

For an existing local result, keep the original Track and inspect its checkout
and Git diff. Save reviewed work with your repository's normal Git workflow
before handing it to another Track. A new Track uses committed repository
content; it does not carry uncommitted changes automatically. Working
instructions and plugin binding are fixed when a Track is created. A guided
handoff into managed delivery is tracked in [#2680](https://github.com/keanji-x/neige-calm/issues/2680).

## Know when to act and how to return

The Report is the current outcome; the conversation carries requests, feedback
and decisions. Tasks and worker cards show execution and verification details.
Use **Waiting on you** or the Notification Center to find decisions addressed to
you. Read the requested decision and its evidence before responding. A worker
finishing, verification passing, a PR being published and a PR being merged are
different milestones; inspect the report's actual result.

Close a Track when finished; the Planner may also close completed work. Reopen
it before scheduling more work. If a closed
Track disappears from the desktop list, open its Area menu and choose **Show
closed**. To resume after restarting the service, run the same printed start
command and open the existing Track; you do not need to run setup again.

When you repeat the same kind of work, open **Template → Manage recipes… → New
recipe**. Save the working instructions and desired report headings as ordinary
Markdown. A Recipe does not enqueue a fixed list of tasks or enable plugins;
the Planner creates tasks for the actual request. Changes to a Recipe affect
new Tracks, not existing ones. See the [Recipe guide](recipe-body-format.md).

## If you get stuck

| What you see | What to do |
| --- | --- |
| Setup says the destination exists | Keep it; use its saved start command or the upgrade guide. Choose a fresh `--root` only for a separate installation. |
| Missing loader or shared library | Check the package's CPU architecture and Linux/libc baseline in `BUILD.json`. |
| Setup refuses a path or says a socket path is too long | Choose a shorter absolute installation root without `%`, `?` or `:`; setup has not installed anything. |
| A port is already in use | Before fresh setup, choose distinct free `--listen` and `--admin-listen` loopback addresses. Do not stop another instance to make room. |
| Planner is Unavailable or Not configured | Read its reason in Settings → Planners, fix it as the service user, and Recheck. |
| A sign-in check times out, or a request waits without a reply | Check CLI login and Network settings. If proxy settings just changed, restart the saved foreground command and Recheck; keep a persistent error for diagnosis. Avoid duplicate jobs. |
| Development requires a plugin | Enable development in Settings → Plugins and return to the saved draft. |
| A worker refuses a dirty checkout | Review and commit or deliberately undo that checkout's changes before trying again. |

For report references (`@`), live previews, plugins and detailed supervision,
continue with [Using Neige Calm](using-neige-calm.md). Preview support must be
configured before use and currently requires HTTP access.
