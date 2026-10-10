# #2530 — Agent desktop: a headless compositor that streams app windows into Reports

Status: design, converged after six L2 rounds (two channels; round 6 approved by both). Issue: #2530. Code references are to `d4f1ba067`.

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
   binary wires them together. The kernel gains three small generic mechanisms (§2.2–§2.4), and
   none of them names this feature.

## 1. Model

| Fact | Owner | Lifetime | Meaning |
|---|---|---|---|
| Display | `compositor` crate | the plugin process | one headless Wayland server; fixed output size; its socket lives in the plugin's private run directory |
| Window | `compositor` | a client's `xdg_toplevel` | `WindowId` (process-unique counter), title, size, client pid; frames = this window's surface tree plus its popups, nothing else |
| App | `desktop` plugin | configured; process launched on demand | a named launch recipe (version 1: one app, `chrome`) with a persistent profile directory; its windows are the ones whose client pid is the launched pid |
| Viewer | `window-stream` crate | one WebSocket | watches one app's main window: receives frames, sends input |
| Page | `chrome-control` crate | a CDP page target | the one visible tab (`document.visibilityState == "visible"`); CDP targets carry no Wayland window identity, so when several pages are visible (a second window, an OAuth popup) `page_*` refuses and names them |
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
  browser ── WS /api/plugins/desktop/ws/apps/chrome/stream ──► proxy ─UDS─► http.sock ─► window-stream ─► compositor ◄─wl─ Chrome
  agent  ── MCP plugin_desktop_* ──► plugin host ── stdio MCP ──────────────► tools ───────┬─► compositor
                                                                                           └─► chrome-control ─CDP pipe─► Chrome
```

| Crate | Kind | Depends on | Owns | Does not know |
|---|---|---|---|---|
| `compositor` | lib, Linux | smithay 0.7 (`wayland_frontend`, `desktop`, `renderer_pixman`), calloop | Wayland protocol, windows, per-window software rendering and damage, frame pacing, input injection, US keymap | browsers, encoding, networks, Neige |
| `window-stream` | lib | tokio, tokio-tungstenite, `jpeg-encoder` (pure Rust) | its own `Frame` type, `FrameEncoder`, wire protocol v1, one viewer session over any WebSocket, latest-wins pacing, browser key code → evdev | smithay, Chrome, Neige |
| `chrome-control` | lib, Linux | tokio, serde_json | Chrome command line, allowlisted child environment, process-group ownership of its own child, `--remote-debugging-pipe` CDP client, page target selection | Wayland internals, Neige |
| `desktop` (`plugins/desktop`) | bin, Linux | the three above, axum | plugin stdio MCP (handshake echo per `plugin-handshake-meta.md`), app registry and launch, HTTP on a Unix socket, the five tools, adapting `compositor` frames and input to `window-stream`'s types | kernel internals: it uses only the plugin protocol |
| kernel: plugin WebSocket proxy | calm-server | — | a session- and origin-gated route that tunnels WebSocket upgrades to a socket that a plugin declares | which plugin, what protocol |
| kernel: `window` block | calm-types, fe | — | payload validation; the fe viewer component that speaks window-stream v1 | which plugin serves it |
| kernel: plugin approval delegation | calm-server | — | Codex approval delegation for plugin tools whose manifest asks for it, granted by the owner's enable, carried in the shared kernel MCP entry | which plugin, which tool |

The three libraries carry no `calm-` prefix (AGENTS.md "Contracts and code") and no Neige
dependencies. `compositor` has a dev example that runs without Neige and dumps a window to PNG.
`window-stream` had a loopback dev page for E1; E3b removed it, and the Report `window` block's
viewer (`fe/web/src/systems/window-stream/`) is the protocol's viewer since.

### 2.1 Why a plugin, not a kernel feature

| | App plugin (chosen) | Kernel-owned |
|---|---|---|
| Process launch, supervision, respawn | exists: `PluginProcess::spawn`, backoff 1–30 s, stop after 5 crashes in 300 s (`crates/plugin/src/host/supervision.rs`) | new code |
| Agent tools | exist: `exposes_tools` minted `plugin_desktop_<tool>` (`results.rs:69-83`), served to Planner and Worker while the plugin runs (`transport.rs:57,448`) | new kernel tools plus CLI mirrors |
| Image tool results | passed through: plugin `ContentBlock` keeps unknown fields (`crates/plugin/src/mcp.rs:20-70`) | the kernel image result was removed in #1899 and would have to come back |
| Enable/disable, opt-in | exists: install and enable routes; disabled by default | new config |
| Browser → process streaming | **missing**: no proxy to a plugin process exists | a new route either way |
| Report block | **missing**: no block renders a live stream | a new kind either way |

The plugin route reuses four existing mechanisms. It adds the two that both routes need, plus
the approval delegation of §2.4, which a kernel route would need for its own tools too. All
three additions are generic, so later plugins can use them.

### 2.2 Kernel addition 1: plugin WebSocket socket

- Manifest field `http_socket` (a relative file name, for example `"http.sock"`), in manifest
  version 6 (v4 and v5 already gate fields, `manifest.rs:601-613`). Validation refuses absolute
  paths, `..` and separators. The socket lives in the plugin's working directory
  `<plugins_data_dir>/<id>/`, which the kernel already creates (`process.rs:55-66`).
- Route `GET /api/plugins/{id}/ws/{*path}`, WebSocket upgrades only, in the WS router under
  `require_session_ws` (`auth.rs:380-392`), which checks the session and the origin of every
  upgrade, like `/api/terminals/{id}`. The kernel strips the `calm-session` cookie and forwards
  the upgrade to the Unix socket with the remaining path and query. The cookie strip moves into
  the shared proxy module (today it sits in the gateway's `rewrite_request`,
  `gateway.rs:288-297`), and so does the response-side filter that drops a `Set-Cookie` for
  `calm-session` (`gateway.rs:310-318`), so neither caller can forget either. Version 1 proxies no plain
  HTTP: plugin-generated pages on calm's own origin would need the sandbox and `nosniff` rules
  that `fs.rs:765-766` applies, and nothing here needs them. The route is reachable wherever the
  WS router is mounted, including the mobile router (`routes/application.rs:30-49`), with the
  same checks.
- It serves only while the plugin is `Running` and enabled. Otherwise the plugin is unavailable
  (503, the same meaning as tool code -32503). On a plugin stop or respawn, the kernel cancels
  every tunnel opened to that plugin.
- **One proxy implementation.** The preview gateway's forwarding and upgrade tunnel
  (`crates/calm-server/src/preview/gateway.rs:108-187`) moves into one shared module that takes
  a target (`Tcp(port)` or `Unix(path)`) and a cancellation token. The gateway and this route
  both call it. The gateway keeps its preview-specific Host/Origin rewrites (`gateway.rs:230+`),
  and its behaviour does not change.

### 2.3 Kernel addition 2: `window` Report block

- Payload `{src, title?, height?}`. `src` is a same-origin path under `/api/plugins/{id}/ws/`, validated
  like the `app` block's `src` (`crates/calm-types/src/report_blocks/kinds.rs:674-684`), and
  refused if it contains a dot segment, encoded or not (`/../`, `%2e`), because the browser
  normalizes it. The kind
  means "a live window that speaks window-stream protocol v1 at `src`". It names no plugin.
- The fe viewer (`fe/web/src/systems/window-stream/`) opens `ws(s)://<location.host><src>`, so the
  session cookie and origin rules of the page apply. It draws frames on a canvas scaled to fit,
  maps pointer and key events back into window pixels, reconnects with backoff, and shows
  "unavailable" over a dimmed last frame while disconnected. It never presents an old frame as
  live.
- Agents add the block with the existing report tools. `window_ls` returns each app's `src`.

### 2.4 Kernel addition 3: plugin approval delegation

Tool annotations stay truthful (`plugins/market/README.md:33-44`). `page_open` and
`window_input` reach the open web, so they declare `openWorldHint: true`. Codex then asks for
approval, and under approval policy `never` such a call fails (same README). The kernel already
solves this for its own terminal tools: the annotations stay honest, and the kernel delegates
approval for exactly its write tools, per role, in the Codex thread config
(`mcp_server/wiring.rs:78-93`; pinned by
`terminal_policy_keeps_truthful_annotations_and_exact_write_inventory`).

Codex asks for approval for any MCP tool that is not `readOnlyHint: true`, unless it is both
`destructiveHint: false` and `openWorldHint: false` (`requires_mcp_tool_approval`,
`external/codex/codex-rs/core/src/mcp_tool_call.rs:2096`). A read-only tool is never asked unless
it also says `destructiveHint: true`. Under the Planner's `full` tier (`never` with
`dangerFullAccess`) Codex approves MCP prompts by itself (`codex-mcp/src/mcp/mod.rs:79-87`), so
the delegation matters for the `never` and `ask` tiers and for Workers.

- A manifest tool may declare `"approval": "delegated"` (manifest version 7; E3a's
  `http_socket` is version 6).
- **Carrier: the shared kernel MCP entry, read at thread start.** The refresh that bumps
  `NEIGE_MCP_TOOLSET` (`codex_mcp_toolset.rs:112-127`, `shared_codex_home.rs`) also writes, into
  the shared `CODEX_HOME` entry's `tools` map, `approval_mode: "approve"` for the delegated tools
  (minted names) of every installed and **enabled** plugin. "Enabled", not "running", avoids a
  race with boot autospawn and crash backoff; a call to a plugin that is not running is still
  refused by routing (`transport.rs:562-564`). Codex merges config layers key by key, so the
  per-thread terminal map and this map combine.
- **v1 reach: threads that start, or cold-resume, after the refresh.** A loaded thread keeps
  the user config it started with: an MCP reload rebuilds only the server connections, and loaded
  threads ignore resume config (`codex_mcp_toolset.rs:16-19`,
  `shared_codex_appserver.rs:3419-3420`; in Codex, `core/src/session/mcp.rs` and
  `core/src/session/turn_context.rs`). So a Planner thread loaded before the owner enables the
  plugin lists the write tools but has no delegated grant: under `never` its calls are refused,
  under `ask` they are asked (KNOWN GAP). Hot-reloading loaded threads
  (Codex `config/batchWrite` with `reloadUserConfig: true`) is a later, separate kernel→Codex
  call, not part of v1.
- **Grant scope.** The shared entry is read by every Codex thread, so the grant covers every role
  that the kernel serves plugin tools to: the Planner and Workers (`transport.rs:57`). This is
  stated, not hidden: Claude-backed agents already allow every `mcp__neige` tool in every mode
  (`claude_planner/spawn.rs:106-112`), so a Planner-only grant would not be a boundary anyway.
  Assistant and ReportCard threads read the map too, but it is inert there because plugin tools
  are not served to them. The kernel's own per-role terminal map is unchanged, and
  `card_mcp_thread_start_config` is not touched.
- **Authority.** The owner's install and enable is the grant. The kernel stays the live role,
  scope and running-state authority for every call.
- **Probe (E3c), against the production Codex binary** with a fake model: a thread started after
  enable calls a delegated tool without approval and still has its terminal grants; a thread
  started after disable is asked (or refused under `never`).
- Effect: the desktop's two write tools are usable by Codex agents whose thread started after the
  owner enabled the plugin. The three views are `readOnlyHint: true` and need nothing.

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

`window-stream` declares its own `Frame` with the same fields. The `desktop` binary converts
between the two by moving the `Arc`, so nothing is copied and neither library depends on the other.

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
- The child environment is built from an explicit allowlist: `LANG`/`LC_*`, `PATH`,
  `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR` and font variables, with `HOME` set to
  `<plugins_data_dir>/desktop/home` so that Chrome's crash database and NSS state stay with the
  profile instead of the owner's real home (AGENTS.md: credential-sensitive
  boundary). The plugin process itself inherits the whole service environment (`process.rs:70`),
  so this allowlist is what keeps service secrets out of the browser.
- **Teardown ownership.** The kernel's plugin stop does not send SIGTERM: it aborts the supervisor,
  which owns the child, and drops it with `kill_on_drop`, i.e. SIGKILL (`host/state.rs:81-95`,
  `host/spawn_app.rs:121-133`, `process.rs:79-80`). And a production calm-server that dies
  abruptly leaves its children running (`KillMode=process`). So teardown cannot depend on
  `desktop` being asked to stop. It rests on two mechanisms:
  1. `chrome-control` spawns Chrome in its own process group with `PR_SET_PDEATHSIG(SIGKILL)`, from a
     dedicated launcher thread that lives as long as the process (PDEATHSIG fires when the
     forking *thread* exits, so never from a tokio blocking thread; precedent
     `neige-app/src/tailnet/mod.rs:189`). Whenever `desktop` dies, SIGKILL included, the
     browser process dies.
  2. Chrome's helper processes exit when their browser process is gone: the zygote, renderers,
     GPU and utility processes (in the browser's group), and the two `chrome_crashpad_handler`
     processes, which double-fork into their own sessions under the user's systemd (observed in
     the E0 probe), so neither a group stop nor parent links reach them. This is Chrome's own
     behaviour, not ours, so E2 measures it (A4). If the crashpad handlers do not exit, E2 finds
     and records a launch switch that stops Chrome from starting them, on the binary the runbook
     installs (the candidate, `--disable-crashpad-for-testing`, is a testing switch); if none
     works, A4 fails and the design returns for revision; the kernel group-stop
     contingency below would not reach them.
  There is deliberately no start-up reaping of earlier runs: a recorded pid, group or session id
  can be reused by an unrelated process after the owner dies or the host reboots, and a
  command-line match is not ownership. Relaunching on the same profile needs no reaping either:
  Chrome's `SingletonLock` names the browser process, which mechanism 1 has killed, and Chrome
  takes over a lock whose holder is gone. A stale `http.sock` is unlinked only after a
  connect to it fails.
- **Restart overlap.** A plugin restart spawns the new `desktop` while the old one's SIGKILL is
  still in flight (`host/state.rs:83-85,118-128`). A live old `http.sock` or a live old
  `SingletonLock` holder is therefore a transient condition that the new `desktop` retries with
  a bound. A Chrome launch that hands its command line to a live holder and exits is detected by
  its exit, never awaited on the CDP pipe.
- `desktop` exits on stdin EOF (the kernel side of the MCP pipe is gone) and on SIGTERM. On
  those paths it stops the group of the Chrome child it still holds (not yet reaped, so its id
  cannot have been reused) in order: SIGTERM, then SIGKILL after one second.
- **Contingency.** If E2 shows Chrome helpers outliving their browser process, or a relaunch
  refused by the profile lock, Chrome stays in `desktop`'s process group, and the kernel spawns
  app plugins as group
  leaders and stops them by group (`child_process.rs:31-47` has the helper). That is a generic
  kernel change added to E3a at L2. A separate pre-existing defect makes every app-plugin stop a
  SIGKILL (stop finds the child already moved to the supervisor); it is filed as #2535, and
  fixing it would give `desktop` a graceful stop.
- CDP runs over pipe fds 3 and 4 with NUL-delimited JSON. No TCP port exists, so no other local
  process can reach the debugger through the network.
- Calls in version 1: list page targets, find the visible one, `Page.navigate` and wait for load
  (with a timeout), and `Runtime.evaluate` for url, title and `document.body.innerText`.

### 3.4 `desktop` plugin

- **Config** (`config_schema`, typed, delivered in the handshake): `chrome_binary` (required path),
  `size` (optional, default 1280×800), `max_fps` (optional, default 15). The binary is not
  bundled; installing Chrome is a runbook step (`dpkg -x` of the Google Chrome stable package, or
  Chrome for Testing), not code.
- **Layout under `<plugins_data_dir>/desktop/`** (0700): `run/` (the Wayland socket, named per
  `desktop` process so a restart never meets an old one), `http.sock` (0600, a fixed name because
  the kernel proxy dials it), `profiles/chrome/`, and `home/` (Chrome's crash database and NSS
  state; minidumps can hold page memory and cookies).
- **`http.sock`** serves one route: `GET /apps/{app}/stream`, a WebSocket that launches the app
  if it is not running and then serves its main window.
- **Tools** (minted `plugin_desktop_<tool>`, names per `docs/conventions/agent-commands.md` §6).
  Annotations are truthful: the three views are `readOnlyHint: true, destructiveHint: false,
  openWorldHint: false`;
  `page_open` and `window_input` are `openWorldHint: true` with `"approval": "delegated"`
  (§2.4).

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
| `desktop` crashes or is killed | PDEATHSIG kills Chrome's browser process and its helpers exit; the kernel cancels tunnels and respawns `desktop` with backoff; viewers reconnect |
| Plugin disabled or stopped | the kernel SIGKILLs `desktop` (`kill_on_drop`); the same chain as a crash, without respawn; the kernel cancels tunnels |
| calm-server graceful restart or deploy | plugin children die with it (`kill_on_drop`); Chrome restarts on next demand with logins intact and tabs lost (KNOWN GAP) |
| calm-server dies abruptly | `desktop` survives (`KillMode=process`), reads stdin EOF and stops Chrome's group, then exits; the next calm-server spawns a new `desktop` |

## 5. Accepts

| # | Invariant | Evidence (where it is proven) |
|---|---|---|
| A1 | The browser is reachable only through the session-gated route: no TCP listener from `desktop` or Chrome; CDP over a pipe; `http.sock` 0600 inside a 0700 tree | E4: `ss -lntp` before/after shows no new listener; permission check in a test |
| A2 | The proxy refuses requests without a session, refuses cross-origin upgrades, strips `calm-session`, returns 503 when the plugin is not running, and cancels tunnels on stop | E3a route tests through the real router; mutation-verify the session and origin checks |
| A3 | Chrome's environment is exactly the allowlist | E2 test reads `/proc/<pid>/environ` of the launched child |
| A4 | No orphan: after SIGKILL of `desktop`, its stdin EOF, or SIGKILL of calm-server, no process of that launch survives 5 s. The test records the set while Chrome runs, before the kill: the descendants by parent links plus the crashpad handlers named by the children's `--crashpad-handler-pid` and their `--monitor-self` peer (measurement only, never used to signal), and a relaunch on the same profile succeeds. `desktop` never signals a process it did not spawn | E2 process test (SIGKILL, EOF); E4 with the real plugin host stop and a calm-server SIGKILL |
| A5 | A frame contains only its window's surface tree and popups | E0 test with two test clients in distinct colours |
| A6 | Nothing is encoded without a viewer; unwatched windows get 1 Hz frame callbacks | E1 counter test |
| A7 | A slow viewer does not block the compositor or a second viewer; memory is bounded by one frame per session | E1 test with a stalled socket |
| A8 | Unavailable never returns stale: a closed window, a stopped app or a stopped plugin gives an error or `closed`, never a cached image | E0/E1/E4 tests |
| A9 | Generic layers name no plugin: the proxy, the `window` block and the approval delegation contain no `desktop` identity | E3a–E3c review + grep |
| A11 | Delegation is exact: the shared entry's approval map equals the delegated tools of enabled plugins after every refresh; per-thread terminal maps are unchanged; a thread started after enable has the grant and one started after disable has none | E3c unit test on the entry writer; E3c fake-model probe on the production binary, recorded in the PR |
| A10 | Logins persist: a login, followed by at least 60 s and then a plugin respawn and a server restart, still shows the logged-in X timeline | E4 manual acceptance, recorded in the PR |

## 6. Slices

| Slice | Content | Acceptance | Tier |
|---|---|---|---|
| **E0** | `compositor` crate and a probe example that launches Chrome on it and dumps PNGs | Chrome renders a real page with no GPU; a `<select>` popup appears in the frame; injected click and typing change the page; A5, A8 with a Rust test client; idle CPU/RSS recorded; `navigator.webdriver` recorded | L1: no Neige surface |
| **E1** | `window-stream` crate and a loopback dev page (dev harness, removed in E3b) | the owner clicks and types in Chrome through the dev page; A6, A7 | L1 |
| **E2** | `chrome-control` crate (can run in parallel with E1) | launch, navigate, read the visible tab's text; A3, A4; `navigator.webdriver` is false with `--remote-debugging-pipe`, or a mitigation is named | **L2**: credential-sensitive child environment and process teardown |
| **E3a** | kernel: manifest v6 with `http_socket`, the shared proxy module, the WebSocket proxy route | A2, A9; gateway tests unchanged and green | **L2**: authentication |
| **E3b** | `window` block: calm-types, report contracts, fe viewer and browser test (removes E1's dev page) | the viewer streams and sends input through E3a; A9 | L1 |
| **E3c** | `"approval": "delegated"` and the shared-entry writer; lands after E3a; manifest version 7 | A9, A11 | **L2**: approval |
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
- **Tool scope.** The plugin keeps the default scope, so its tools appear on every unbound Track
  for the Planner and Workers, and the delegation covers both (§2.4). Narrowing to bound Tracks is
  one manifest field (`agent_tools_scope: "bound-track"`, `manifest.rs:138-145`).
- **Approval grant reaches only new threads.** A Codex thread loaded before the owner enables
  the plugin lists the two write tools without the delegated grant until it starts again: refused
  under `never`, asked under `ask` (§2.4).
- **Outbound from pages.** Pages that Chrome loads run script on the host and can try loopback
  services, such as unauthenticated dev servers behind preview ports. Only Chrome's Local Network
  Access checks stand between them, and `window_input` can click through their prompts. A launch
  policy that blocks local-network requests is a later option.
- **Prompt injection.** Page text reaches models, and delegated writes are never asked, also
  when the Planner runs in the `ask` tier (the tier sets only the turn's policy,
  `provider/src/codex/approvals.rs:39-55`). Dropping `"approval": "delegated"` from the manifest
  makes Codex ask (`ask`) or refuse (`never`) in threads started afterwards; loaded threads keep
  their grant. The immediate off switch is disabling the plugin: routing then refuses every call.
- **Abrupt stop loses the last cookies.** Plugin stop is SIGKILL, and Chrome commits cookies to
  disk in batches, so a login made seconds before a stop can be lost.
- **Shared seat.** Human and agent input interleave; last input wins.
- **Model image receipt unverified.** It is unknown whether Codex/Claude pass MCP image blocks to
  the model. E4 checks this; text tools alone satisfy the MVP.
- Non-ASCII input and IME, clipboard, audio, file chooser dialogs (no portal), downloads UI,
  notifications, resize and HiDPI, and popups larger than their window (clipped).
- A server restart or deploy restarts Chrome and loses open tabs.
- Chrome install and updates are manual (runbook).
- Linux only.
