# Desktop plugin

A real Google Chrome on this host, inside a headless Wayland compositor. The
owner watches and uses its window live in a Report `window` block; agents read
and drive the same window with five tools. Design: issue #2530,
`docs/architecture/2530-agent-desktop.md`.

There is no GPU, no FFmpeg/PipeWire/GStreamer and no new apt package. Chrome is
not bundled: installing it is a step of this runbook.

## How it works

`desktop` is one process, spawned by the kernel's plugin host. It wires three
libraries together and speaks only the plugin protocol:

- `compositor`: a headless Wayland server; its socket lives in the plugin's
  private `run/` directory.
- `chrome-control`: launches Chrome on that display with a persistent profile
  and an allowlisted environment, and drives the visible tab over CDP on a
  pipe (no debugging port).
- `window-stream`: streams one window as JPEG frames over a WebSocket and
  carries the viewer's pointer and keys back.

The kernel reaches the stream through the manifest's `http_socket`
(`http.sock`, a Unix socket) behind its session- and origin-gated route
`/api/plugins/desktop/ws/…`. Neither `desktop` nor Chrome listens on TCP.

Chrome starts on the first demand: a viewer of the stream, or `page_open`. It
stops when the plugin stops:

- stdin EOF (the kernel went away) or SIGTERM: `desktop` asks Chrome over CDP
  to close (`Browser.close`), as closing its last window does, so Chrome
  writes its cookies; if it has not exited after five seconds it gets SIGTERM,
  and two seconds later its process group gets SIGKILL;
- a SIGKILL of `desktop`, which is what the kernel's plugin stop, disable,
  reload and server restart send: Chrome dies through `PR_SET_PDEATHSIG` and
  its helpers exit with it. Nothing is written in order then, so cookies set
  in the last seconds before it can be lost (see the known gaps).

If the owner closes Chrome's last window, the app is not running until the
next demand. A viewer always shows the app's newest open window: when Chrome
opens another window (a login popup), the stream switches to it, and back
when it closes.

Files, all under `<plugins_data_dir>/desktop/` (directories 0700):

| Path | What |
|---|---|
| `run/<pid>-<tag>/wayland-0` | this process's Wayland socket (0600) |
| `http.sock` | the stream socket (0600) |
| `profiles/chrome/` | Chrome's profile: cookies and logins |
| `home/` | Chrome's `HOME`: crash database, NSS state |

The profile survives plugin, server and host restarts.

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

`window_input` and `page_open` reach the open web, so their annotations say
`openWorldHint: true`, and Codex asks for approval before each call (under the
`never` approval policy it refuses them). The three views are read-only and
never asked.

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

Chrome's sandbox needs unprivileged user namespaces
(`sysctl kernel.unprivileged_userns_clone` is 1 where that knob exists).

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
   B=$(pgrep -o -f -- "--user-data-dir=$PLUGINS_DATA/desktop/profiles/chrome")
   ps -o pid=,args= -g "$B" | cut -c1-100 > /tmp/a4-group.txt          # browser, zygotes, renderers…
   pgrep -af "chrome_crashpad_handler.*$PLUGINS_DATA/desktop/home" > /tmp/a4-crashpad.txt
   ```

   Disable the plugin (`POST /api/plugins/desktop/disable`), wait 5 seconds,
   and run `ps -g "$B"` and the `pgrep` again: neither may print a process.

Record the result in the PR.

## Known gaps

- **Same user, no isolation.** Agents and Chrome run as the same OS user. Any
  local process of that user can read the profile, and a Worker's shell can
  reach `http.sock`. The plugin keeps the browser off the network and out of
  service secrets; it is no boundary against local code of the same user.
- **Tool scope.** The tools appear on every unbound Track for the Planner and
  Workers. Narrowing to bound Tracks is one manifest field
  (`agent_tools_scope: "bound-track"`).
- **Approval.** `page_open` and `window_input` are asked (or refused under
  `never`) until the kernel's delegated approval (#2530 E3c) is declared for
  them; once it is, a thread loaded before the plugin was enabled still lacks
  the grant until it starts again.
- **`page_open` takes web URLs only, but that is no boundary.** It refuses
  `file:`, `chrome:` and other schemes, yet `window_input` can type any
  address into Chrome's address bar, `file:///` included, and `page_cat` then
  reads it. This is the same-user gap above: the Planner can read what the
  OS user can read.
- **Outbound from pages.** Pages run script on the host and can try loopback
  services. Only Chrome's Local Network Access checks stand between them, and
  `window_input` can click through their prompts.
- **Prompt injection.** Page text reaches models. Disabling the plugin is the
  immediate off switch.
- **Abrupt stop loses the last cookies.** The kernel stops a plugin with
  SIGKILL, so a login made seconds before a plugin stop, reload or server
  restart can be lost; only stdin EOF and SIGTERM close Chrome in order.
- **Abrupt stop loses the tabs.** After a SIGKILL Chrome would ask "Restore
  pages?" in a bubble over the page; `--hide-crash-restore-bubble` hides it
  (checked on Chrome for Testing 151, no extra flag bar), and the tabs are not
  restored.
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
- A server restart or deploy restarts Chrome and loses open tabs (logins stay).
- Chrome install and updates are manual. Linux only.
