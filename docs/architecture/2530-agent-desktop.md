# #2530 — Agent desktop: a headless compositor that streams app windows into Reports

Status: design, draft for L2 review. Issue: #2530. Code references are to `d4f1ba067`.

## 0. Outcome

A real Google Chrome runs on the Neige host inside a headless Wayland compositor that we build
with smithay. The owner sees its window live in a Report block, clicks and types in it, and logs
into X once. An agent then drives the same window: it opens a page, reads its text, takes a
screenshot and sends input. There is no GPU, no FFmpeg/PipeWire/GStreamer and no new apt package
on the host.

The three owner requirements set the shape:

1. **End to end first, crude allowed.** Version 1 sends full-frame JPEGs over a WebSocket, uses one
   fixed window size and a US keyboard, and has five tools.
2. **Good abstractions.** Each later upgrade replaces one part behind a seam: H.264 replaces the
   encoder, WebRTC replaces the transport, another app needs another adapter.
3. **Standalone crates, low coupling.** Three library crates know nothing about Neige. One plugin
   binary wires them together. The kernel gains only two generic mechanisms, and neither names
   this feature.

## 1. Model

| Fact | Owner | Lifetime | Meaning |
|---|---|---|---|
| Display | `compositor` crate | the plugin process | one headless Wayland server; fixed output size; its socket lives in the plugin's private run directory |
| Window | `compositor` | a client's `xdg_toplevel` | `WindowId` (process-unique counter), title, size, client pid; frames = this window's surface tree plus its popups, nothing else |
| App | `desktop` plugin | configured; process launched on demand | a named launch recipe (version 1: one app, `chrome`) with a persistent profile directory; its windows are the ones whose client pid is the launched pid |
| Viewer | `window-stream` crate | one WebSocket | watches one app's main window: receives frames, sends input |
| Page | `chrome-control` crate | a CDP page target | the tab that is visible (`document.visibilityState == "visible"`) in the app's main window |
| Report block `window` | calm-types + fe | persisted in the Report | `{src, title?, height?}`: a same-origin path that speaks the window-stream protocol |

Nothing new is persisted in the Neige database. Windows, viewers and pages are runtime state. The
Chrome profile (cookies, logins) is files under the plugin's data directory and survives plugin,
server and host restarts.

**Main window.** A Report block names an app, not a window, because window ids do not survive a
restart. The app's main window is its most recently created toplevel that is still open. Version 1
streams only that window; popups and menus are drawn into it.

**One seat.** The human and the agent share one input seat and one keyboard focus. Whoever sends
input last wins. Focus moves to a window when a pointer button is pressed in it or a tool sends it
input.

## 2. Components and boundaries

```
                         Neige (kernel, generic)                    desktop plugin process
  browser ── WS /api/plugins/desktop/http/apps/chrome/stream ──► proxy ─UDS─► http.sock ─► window-stream ─► compositor ◄─wl─ Chrome
  agent  ── MCP plugin_desktop_* ──► plugin host ── stdio MCP ──────────────► tools ───────┬─► compositor
                                                                                           └─► chrome-control ─CDP pipe─► Chrome
```

| Crate | Kind | Depends on | Owns | Does not know |
|---|---|---|---|---|
| `compositor` | lib, Linux | smithay 0.7 (`wayland_frontend`, `desktop`, `renderer_pixman`), calloop | Wayland protocol, windows, per-window software rendering and damage, frame pacing, input injection, US keymap | browsers, encoding, networks, Neige |
| `window-stream` | lib | tokio, tokio-tungstenite, `jpeg-encoder` (pure Rust) | `FrameEncoder`, wire protocol v1, one viewer session over any WebSocket, latest-wins pacing, browser key code → evdev | smithay, Chrome, Neige |
| `chrome-control` | lib, Linux | tokio, serde_json | Chrome command line, allowlisted child environment, `--remote-debugging-pipe` CDP client, page target selection | Wayland internals, Neige |
| `desktop` (`plugins/desktop`) | bin, Linux | the three above, axum | plugin stdio MCP (handshake echo per `plugin-handshake-meta.md`), app registry and launch, HTTP on a Unix socket, the five tools | kernel internals: it uses only the plugin protocol |
| kernel: plugin HTTP proxy | calm-server | — | a session-gated route that tunnels HTTP and WebSocket to a socket that a plugin declares | which plugin, what protocol |
| kernel: `window` block | calm-types, fe | — | payload validation; the fe viewer component that speaks window-stream v1 | which plugin serves it |

The three libraries carry no `calm-` prefix (AGENTS.md "Contracts and code") and no Neige
dependencies. Each has a dev example that runs without Neige: `compositor` dumps a window to PNG,
and `window-stream` serves a loopback test page.

### 2.1 Why a plugin, not a kernel feature

| | App plugin (chosen) | Kernel-owned |
|---|---|---|
| Process launch, supervision, respawn | exists: `PluginProcess::spawn`, backoff 1–30 s, stop after 5 crashes in 300 s (`crates/plugin/src/host/supervision.rs`) | new code |
| Agent tools | exist: `exposes_tools` minted `plugin_desktop_<tool>` (`results.rs:69-83`), served to Planner and Worker while the plugin runs (`transport.rs:57,448`) | new kernel tools plus CLI mirrors |
| Image tool results | passed through: plugin `ContentBlock` keeps unknown fields (`crates/plugin/src/mcp.rs:20-70`) | the kernel image result was removed in #1899 and would have to come back |
| Enable/disable, opt-in | exists: install and enable routes; disabled by default | new config |
| Browser → process streaming | **missing**: no proxy to a plugin process exists | a new route either way |
| Report block | **missing**: no block renders a live stream | a new kind either way |

The plugin route reuses four existing mechanisms and adds the two that both routes need. The two
additions are generic, so later plugins can serve rich views through them.

### 2.2 Kernel addition 1: plugin HTTP socket

- Manifest field `http_socket` (a relative file name, for example `"http.sock"`). It needs a new
  manifest version. Validation refuses absolute paths, `..` and separators. The socket lives in
  the plugin's working directory `<plugins_data_dir>/<id>/`, which the kernel already creates
  (`process.rs:41-119`).
- Route `ANY /api/plugins/{id}/http/{*path}`, inside the session-gated router. HTTP requests
  require a session (`require_session`); WebSocket upgrades also pass the origin check
  (`require_session_ws`, `auth.rs:303-392`). The kernel strips the `calm-session` cookie and
  forwards the request to the Unix socket with the remaining path and query.
- It serves only while the plugin is `Running` and enabled. Otherwise the plugin is unavailable
  (503, the same meaning as tool code -32503). On a plugin stop or respawn, the kernel cancels
  every tunnel opened to that plugin.
- **One proxy implementation.** The preview gateway's forwarding and upgrade tunnel
  (`crates/calm-server/src/preview/gateway.rs:108-160`) moves into one shared module that takes
  a target (`Tcp(port)` or `Unix(path)`) and a cancellation token. The gateway and this route
  both call it. The gateway's behaviour does not change.

### 2.3 Kernel addition 2: `window` Report block

- Payload `{src, title?, height?}`. `src` is a same-origin path under `/api/plugins/`, validated
  like the `app` block's `src` (`crates/calm-types/src/report_blocks/kinds.rs:674-684`). The kind
  means "a live window that speaks window-stream protocol v1 at `src`". It names no plugin.
- The fe viewer (`fe/web/src/systems/window-stream/`) opens `ws(s)://<location.host><src>`, so the
  session cookie and origin rules of the page apply. It draws frames on a canvas scaled to fit,
  maps pointer and key events back into window pixels, reconnects with backoff, and shows
  "unavailable" over a dimmed last frame while disconnected. It never presents an old frame as
  live.
- Agents add the block with the existing report tools. `window_ls` returns each app's `src`.

## 3. Interfaces (proposals, settled in each slice's PR)

### 3.1 `compositor`

```rust
pub struct Config { pub run_dir: PathBuf, pub size: (u32, u32), pub max_fps: u32 }
pub fn start(config: Config) -> Result<(Compositor, mpsc::Receiver<WindowEvent>)>; // own thread, calloop

impl Compositor {                       // Clone + Send; every call is a message to the compositor thread
    pub fn wayland_socket(&self) -> &Path;          // set WAYLAND_DISPLAY / XDG_RUNTIME_DIR from this
    pub fn windows(&self) -> Vec<WindowInfo>;       // id, title, size, pid
    pub fn watch(&self, id: WindowId) -> Result<FrameWatch>;  // latest frame + damage accumulated since last take
    pub fn capture(&self, id: WindowId) -> Result<Frame>;     // one full frame now
    pub fn input(&self, id: WindowId, events: Vec<InputEvent>) -> Result<()>;
}
pub enum WindowEvent { Opened(WindowInfo), Changed(WindowInfo), Closed(WindowId) }
pub enum InputEvent { Motion { x: f64, y: f64 }, Button { code: u32, pressed: bool },
                      Axis { dx: f64, dy: f64 }, Key { evdev: u32, pressed: bool } }
pub struct Frame { pub size: (u32, u32), pub stride: u32, pub xrgb8888: Arc<[u8]>, pub damage: Vec<Rect> }
```

- One virtual output at a fixed size (default 1280×800, scale 1). Every toplevel is configured
  maximized to that size. Windows are rendered separately, never composed together.
- Software rendering with smithay's `PixmanRenderer` into one offscreen buffer per window. Damage
  comes from a per-window damage tracker. No `linux-dmabuf` global is advertised, so clients use
  `wl_shm`.
- `xdg_popup` is drawn into its parent's frame. The positioner constrains it to the window.
- Pacing: a window with at least one `FrameWatch` gets frame callbacks at up to `max_fps`. A
  window nobody watches gets them at 1 Hz, which keeps the client alive at low cost.
- `capture` on a window with no committed buffer, or on a closed window, returns an error, never
  an older image.
- smithay comes from crates.io (0.7.0). A git pin, like cosmic-comp's, is a fallback only if a
  slice hits a fix that is missing from 0.7.

### 3.2 `window-stream`

```rust
pub trait FrameEncoder: Send {            // JpegEncoder now; an H.264 encoder later
    fn codec(&self) -> Codec;
    fn encode(&mut self, frame: &Frame) -> Result<EncodedFrame>;
}
pub trait WindowSource: Send + Sync {     // implemented by the desktop binary over `compositor`
    fn watch(&self) -> Result<Box<dyn FrameFeed>>;   // async next(): Option<Frame>, None = window gone
    fn input(&self, events: Vec<StreamInput>) -> Result<()>;
}
pub async fn serve_viewer<S: WebSocketStream>(ws: S, source: Arc<dyn WindowSource>, encoder: Box<dyn FrameEncoder>);
```

- **Wire protocol v1.** Server → client: a text `hello {version, codec, width, height, title}`,
  binary frame messages (a fixed header with codec, keyframe flag and size, then the encoded
  bytes), text `title` and `closed`. Client → server: text JSON `pointer {x, y}`,
  `button {button, pressed}`, `wheel {dx, dy}` and `key {code, pressed}`, where `code` is the
  browser's `KeyboardEvent.code`. Coordinates are window pixels. `window-stream` maps codes to
  evdev; unknown codes are ignored and counted.
- **Latest wins.** A session encodes only when the socket can take the next message. Frames that
  arrive in between are merged by taking the newest one. A slow viewer never blocks the
  compositor or another viewer, and memory per session is bounded by one frame.
- Version 1 sends the whole frame when damage is non-empty. Damage rectangles are kept in `Frame`
  so a later version can send tiles.
- **Upgrade seams.** H.264 replaces `FrameEncoder`; the browser decodes it with WebCodecs over the
  same socket. WebRTC (str0m) replaces the transport behind `serve_viewer`. Neither change touches
  `compositor` or the kernel.

### 3.3 `chrome-control`

- `Chrome::launch(LaunchSpec { binary, profile_dir, wayland: WaylandEnv, size })` builds the
  command line: `--ozone-platform=wayland --user-data-dir=<profile> --remote-debugging-pipe
  --no-first-run --no-default-browser-check --window-size=…`. It adds no `--enable-automation` and
  no headless flag.
- The child environment is built from an explicit allowlist: `HOME`, `LANG`/`LC_*`, `PATH`,
  `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` and font variables (AGENTS.md: credential-sensitive
  boundary). The plugin process itself inherits the whole service environment (`process.rs:70`),
  so this allowlist is what keeps service secrets out of the browser.
- The child gets `PR_SET_PDEATHSIG(SIGKILL)` and its own process group. Plugin stop (SIGTERM)
  sends SIGTERM to the group, then SIGKILL after a grace period.
- CDP runs over pipe fds 3 and 4 with NUL-delimited JSON. No TCP port exists, so no other local
  process can reach the debugger through the network.
- Calls in version 1: list page targets, find the visible one, `Page.navigate` and wait for load
  (with a timeout), and `Runtime.evaluate` for url, title and `document.body.innerText`.

### 3.4 `desktop` plugin

- **Config** (`config_schema`, typed, delivered in the handshake): `chrome_binary` (required path),
  `size` (optional, default 1280×800), `max_fps` (optional, default 15). The binary is not
  bundled; installing Chrome is a runbook step (`dpkg -x` of the Google Chrome stable package, or
  Chrome for Testing), not code.
- **Layout under `<plugins_data_dir>/desktop/`:** `run/` (0700: Wayland socket),
  `http.sock` (0600), `profiles/chrome/`.
- **HTTP on `http.sock`:** `GET /apps/{app}/stream` (WebSocket; launches the app if it is not
  running, then serves its main window) and `GET /health`.
- **Tools** (minted `plugin_desktop_<tool>`, names per `docs/conventions/agent-commands.md` §6;
  every tool declares `openWorldHint: false`, because the kernel runs agents with approval policy
  `never` and an open-world tool is then unusable, `plugins/market/README.md`):

| Tool | Class | Input | Result |
|---|---|---|---|
| `window_ls` | V | — | `windows[{window_id, app, title, width, height}]`, `apps[{app, running, src}]` |
| `window_cat` | V | `window_id` | a PNG image block plus `{window_id, title, width, height}` |
| `window_input` | W | `window_id`, `events[]`: `click {x, y, button?}`, `move {x, y}`, `scroll {x, y, dx, dy}`, `key {keys}` (e.g. `"ctrl+l"`), `text {text}` (US-ASCII) | `{ok: true}`; read back with `window_cat` |
| `page_open` | LC | `url` | `{url, title}` after load or timeout; launches Chrome when needed |
| `page_cat` | V | — | `{url, title, text, text_truncated}` of the visible tab |

Non-ASCII `text` is refused with a message that names `page_open`/IME as known gaps. An
unavailable app or window is an error (-32503 / -32404), never a stale result.

## 4. Lifecycle

| Event | Effect |
|---|---|
| Owner enables the plugin | the kernel spawns `desktop`; it starts the compositor, binds `http.sock` and answers the handshake. Chrome is not launched yet |
| First stream connect or `page_open` | Chrome is launched with the profile; a viewer waits for the main window (with a timeout, then `closed`) |
| Chrome exits (owner closed the last window, or a crash) | the app becomes not running; viewers get `closed`; the next demand relaunches it |
| `desktop` crashes or is killed | the Wayland socket closes and PDEATHSIG kills Chrome; the kernel cancels tunnels and respawns `desktop` with backoff; viewers reconnect |
| Plugin disabled or stopped | SIGTERM → `desktop` terminates Chrome's group and exits; the kernel cancels tunnels |
| calm-server restart or deploy | plugin children die with it (`kill_on_drop`); Chrome restarts on next demand with logins intact and tabs lost (KNOWN GAP) |

## 5. Accepts

| # | Invariant | Evidence (where it is proven) |
|---|---|---|
| A1 | The browser is reachable only through the session-gated route: no TCP listener from `desktop` or Chrome; CDP over a pipe; `http.sock` 0600 inside a 0700 tree | E4: `ss -lntp` before/after shows no new listener; permission check in a test |
| A2 | The proxy refuses requests without a session, refuses cross-origin upgrades, strips `calm-session`, returns 503 when the plugin is not running, and cancels tunnels on stop | E3 route tests through the real router; mutation-verify the session and origin checks |
| A3 | Chrome's environment is exactly the allowlist | E2 test reads `/proc/<pid>/environ` of the launched child |
| A4 | No orphan: after SIGTERM or SIGKILL of `desktop`, no process from that profile survives 5 s, and a relaunch on the same profile succeeds | E2/E4 process test |
| A5 | A frame contains only its window's surface tree and popups | E0 test with two test clients in distinct colours |
| A6 | Nothing is encoded without a viewer; unwatched windows get 1 Hz frame callbacks | E1 counter test |
| A7 | A slow viewer does not block the compositor or a second viewer; memory is bounded by one frame per session | E1 test with a stalled socket |
| A8 | Unavailable never returns stale: a closed window, a stopped app or a stopped plugin gives an error or `closed`, never a cached image | E0/E1/E4 tests |
| A9 | Generic layers name no plugin: the proxy and the `window` block contain no `desktop` identity | E3 review + grep |
| A10 | Logins persist: after a plugin respawn and a server restart, X still shows the logged-in timeline | E4 manual acceptance, recorded in the PR |

## 6. Slices

| Slice | Content | Acceptance | Tier |
|---|---|---|---|
| **E0** | `compositor` crate and a probe example that launches Chrome on it and dumps PNGs | Chrome renders a real page with no GPU; a `<select>` popup appears in the frame; injected click and typing change the page; A5, A8 with a Rust test client; idle CPU/RSS recorded | L1: no Neige surface |
| **E1** | `window-stream` crate and a loopback dev page (dev harness, removed in E3) | the owner clicks and types in Chrome through the dev page; A6, A7 | L1 |
| **E2** | `chrome-control` crate (can run in parallel with E1) | launch, navigate, read the visible tab's text; A3, A4 | L1 |
| **E3** | kernel: manifest `http_socket` and proxy route (shared with the preview gateway), `window` block (calm-types, report contracts, fe viewer and browser test) | A2, A9; gateway tests unchanged and green | **L2**: authentication boundary |
| **E4** | `desktop` plugin: wiring, config, tools, manifest, runbook (Chrome install, enable) | **MVP:** in a Report the owner sees Chrome live, logs into X, and an agent uses `page_open` + `page_cat` to summarize the timeline and `window_cat` to show it; A1, A4, A8, A10 | **L2**: credentials and a new endpoint |
| E5+ | later, one at a time by need: H.264 + WebCodecs, then WebRTC; IME and non-ASCII; clipboard; resize/HiDPI; multiple profiles; separate agent seat; write approvals; surviving server restarts | — | — |

E0 is the spike that can kill the approach. If Chrome cannot render through `wl_shm` without a
GPU, stop and report before E1. The fallback options in #2530 then apply.

CI: `compositor` links libxkbcommon and pixman (already on this host, no new package here). CI's
Ubuntu jobs add `libxkbcommon-dev libpixman-1-dev` to their apt lines. The Linux-only crates
compile to nothing on other targets (`#![cfg(target_os = "linux")]`), so the macOS cross check
is unaffected. Tests use a small Rust Wayland test client, not Chrome. Chrome checks are
probes, recorded in each PR.

## 7. KNOWN GAPS

- **Same user, no isolation.** Workers and the Planner run as the same OS user as Chrome. Any
  local process of that user can read the profile directory, and a Worker's shell can reach
  `http.sock`. The design keeps the browser off the network and out of service secrets. It
  creates no boundary against local code of the same user; the owner accepts this for personal use.
- **Tool scope.** Plugin tools reach the Planner and Workers on every unbound Track while the
  plugin runs (default scope). Narrowing this to the Planner, or to bound Tracks, is a later
  choice.
- **Prompt injection.** Page text reaches models. Write actions have no approval step in version 1.
- **Shared seat.** Human and agent input interleave; last input wins.
- **Model image receipt unverified.** It is unknown whether Codex/Claude pass MCP image blocks to
  the model. E4 checks this; text tools alone satisfy the MVP.
- Non-ASCII input and IME, clipboard, audio, file chooser dialogs (no portal), downloads UI,
  notifications, resize and HiDPI, and popups larger than their window (clipped).
- A server restart or deploy restarts Chrome and loses open tabs.
- Chrome install and updates are manual (runbook).
- Linux only.
