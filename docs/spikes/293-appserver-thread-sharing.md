# Spike #293 — app-server thread sharing (push migration linchpin)

**Issue:** [#293 — Switch spec agents from pull to push via codex app-server](https://github.com/keanji-x/neige-calm/issues/293)
**Date:** 2026-05-24
**Binary:** `codex-cli 0.133.0` (native musl static-pie at
`~/.nvm/.../@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex`)
**Status:** Linchpin question answered against the real binary with a live two-client run.

---

## TL;DR — VERDICT

**Can a real `codex --remote unix://PATH` TUI client and a separate programmatic
app-server client drive/observe the SAME codex thread? → YES (verified live).**

A programmatic WebSocket client started a thread + ran a turn, then the **real TUI**
(`codex resume <threadId> --remote unix://PATH`) attached to that same thread. A
prompt typed into the TUI ("Reply with the single word FOUR…") was observed in
full by the programmatic client: it received the `userMessage` item carrying the
typed text, the agent's `"FOUR"` reply, and the complete
`turn/started → item/* → turn/completed` stream. Bidirectional fan-out (either
client drives, both observe) was also verified with two programmatic clients.

**One hard caveat** (mechanics, not a blocker): `thread/resume {threadId}` resolves
the thread by its **persisted rollout file on disk**. A brand-new thread that has
not yet run a turn has no rollout, so resume fails with
`-32600 "no rollout found for thread id …"`. Once any turn has run (or items have
been persisted), resume succeeds and the second client joins the live thread and
receives real-time events. See [Caveats](#caveats--gotchas).

---

## Environment probe

| Check | Result |
| --- | --- |
| `codex --version` | `codex-cli 0.133.0` |
| Auth | `codex login status` → **Logged in using ChatGPT** (`auth_mode: chatgpt`, `~/.codex/auth.json`) |
| `$CODEX_HOME` | unset → defaults to `~/.codex` (confirmed in `initialize` result: `"codexHome":"/home/kenji/.codex"`) |
| Model turns runnable? | **YES.** Full turn loop completed via `127.0.0.1:2080` proxy. Default model `gpt-5.5`. |
| Proxy | `HTTP_PROXY/HTTPS_PROXY=http://127.0.0.1:2080` (the `codex` shell alias injects these). Required for model calls. |
| app-server boots? | YES on `--listen stdio://` and `--listen unix://PATH` (with the directory-ownership fix below). |

Model turns are NOT blocked here, so the full loop (`turn/start` → `turn/completed`,
`turn/steer`, `thread/inject_items`) was exercised — nothing is "untested-due-to-env".

---

## Wire framing (the valuable bit for the future Rust client)

The transport **differs per `--listen` scheme**:

- **`stdio://` (default):** newline-delimited JSON-RPC — one JSON object per `\n`-terminated
  line. (Confirmed: feeding `Content-Length:` headers produced
  `Failed to deserialize JSONRPCMessage: expected value at line 1 column 1`, i.e. it
  tried to parse the header line as JSON. **No LSP-style Content-Length framing.**)

- **`unix://PATH`:** **WebSocket** over the Unix domain socket — NOT raw JSON lines.
  The server performs an HTTP→WebSocket upgrade, then carries JSON-RPC objects as
  **WebSocket text frames**. Two things bite here:
  1. Sending raw bytes/JSON without a WS handshake → server logs
     `failed to upgrade control socket websocket connection: ... invalid token`
     and closes the connection with **zero bytes returned** (silent from the client's
     view — looks like an instant disconnect).
  2. The client **must disable the `permessage-deflate` extension**. Offering
     compression yields server-side
     `WebSocket protocol error: Missing, duplicated or incorrect header sec-websocket-extensions`
     and the handshake is rejected. In Python `websockets`, pass `compression=None`.
     URI path used: `ws://localhost/`.

- **`codex app-server proxy --sock PATH`:** intended to bridge stdio JSON-RPC ↔ the
  control socket, but in this spike it **also failed the WS upgrade** against a
  manually-launched `--listen unix://` server (same `invalid token` warning). The
  proxy appears to target the managed-daemon socket convention, not an arbitrary
  `--listen` endpoint. Direct WebSocket (option above) was the working programmatic path.

- **JSON-RPC envelope:** `{"id":<int|string>,"method":"…","params":{…}}`. The
  `"jsonrpc":"2.0"` field is accepted but **not required** (schema marks it optional;
  server ignores its absence). `id` may be int or string.

- **`api_version`:** server tags connections `app_server.api_version="v2"` and
  `rpc.transport="unix_socket"`. The default model returned was `gpt-5.5`.

---

## Exact commands / scripts used

Helper scripts committed under `scripts/spike/`:

- `appserver_thread_sharing.py` — two WS clients: A does `initialize`+`thread/start`,
  B does `initialize`+`thread/resume`; logs every frame per client. Documents framing
  in its header.
- `two_client_sharing.py` — full sequence: A starts+turns (flush rollout) → B resumes
  → A turn / B turn, asserting each observes the other.
- `tui_interop.py` — programmatic A starts+turns a thread, then launches the **real
  TUI** `codex resume <tid> --remote unix://PATH` in a PTY, types a prompt, and checks
  A observes the TUI-driven turn.

### Booting the server (note the directory-ownership requirement)

```sh
# A bare /tmp socket FAILS: the server chmod()s the socket's PARENT dir to 0700,
# which EPERMs on the world-writable sticky /tmp (owned by root).
#   strace evidence:  chmod("/tmp", 0700) = -1 EPERM
#   user-visible:     Error: Operation not permitted (os error 1)
# Fix: put the socket in a directory the launching user OWNS.
SOCKDIR=$(mktemp -d /tmp/neige_spike_dir.XXXXXX)   # drwx------ owned by us
SOCK="$SOCKDIR/app.sock"
HTTP_PROXY=http://127.0.0.1:2080 HTTPS_PROXY=http://127.0.0.1:2080 \
  codex app-server --listen "unix://$SOCK"
# socket is created srw------- (owner only); parent dir is chmod'd to 0700 by the server.
```

### Schema regen

```sh
codex app-server generate-json-schema --out /tmp/codex_schema --experimental
```

### Programmatic client (WebSocket over UDS)

```python
from websockets.asyncio.client import unix_connect
ws = await unix_connect("/path/app.sock", "ws://localhost/", compression=None)  # compression=None REQUIRED
await ws.send(json.dumps({"id":1,"method":"initialize",
    "params":{"clientInfo":{"name":"x","version":"0"},
              "capabilities":{"experimentalApi":True}}}))
```

### Real TUI client

```sh
codex --remote unix://$SOCK                 # attaches a fresh-session TUI to the server
codex resume <threadId> --remote unix://$SOCK   # attaches TUI to a SPECIFIC existing thread
```

---

## Protocol shapes observed (live, not just schema)

**`initialize` result:**
```json
{"userAgent":"…/0.133.0 …","codexHome":"/home/kenji/.codex",
 "platformFamily":"unix","platformOs":"linux"}
```

**`thread/start` result** (params `{}` is fine; all fields optional):
```json
{"thread":{"id":"019e59e6-…","sessionId":"019e59e6-…","forkedFromId":null,
  "preview":"","ephemeral":false,"modelProvider":"openai","status":{"type":"idle"},
  "path":"/home/kenji/.codex/sessions/2026/05/24/rollout-…-019e59e6-….jsonl",
  "cwd":"…","cliVersion":"0.133.0","source":"vscode","name":null,"turns":[]},
 "model":"gpt-5.5","modelProvider":"openai","cwd":"…",
 "runtimeWorkspaceRoots":["…"],"instructionSources":[]}
```
Note `thread.id == sessionId`, and `thread.path` is the rollout file (does **not**
exist until the first turn — see caveats).

**`thread/resume`** — required param `threadId` (string). Success returns the thread
object (with `preview` reflecting prior turns). Failure (no rollout yet):
`{"error":{"code":-32600,"message":"no rollout found for thread id …"}}`.

**`turn/start`** — required `threadId` + `input[]`; input item `{"type":"text","text":"…"}`
(also `image`/file variants). Returns `{"turn":{"id":"019e…","status":"inProgress",…}}`
quickly; work streams as notifications.

**`turn/steer`** — required `threadId` + `expectedTurnId` + `input[]`. Returns
`{"turnId":"…"}`. Verified: mid-turn steer redirected an in-flight turn (agent emitted
partial output then honored the steer).

**`thread/inject_items`** — required `threadId` + `items[]`. Returns `{}`. Works on a
**fresh thread with no rollout** (does not require a prior turn); injects context without
starting a turn.

### Notification stream for one turn (every line seen by ALL attached clients)
```
thread/status/changed {status:active}
turn/started
item/started      (userMessage)
item/completed    (userMessage)
item/started      (agentMessage, empty)
item/agentMessage/delta  (× N)
item/completed    (agentMessage, text="OK")
thread/tokenUsage/updated
account/rateLimits/updated
thread/status/changed {status:idle}
turn/completed
```
Per-connection (not thread-scoped) housekeeping events also arrive:
`remoteControl/status/changed`, `mcpServer/startupStatus/updated`,
`app/list/updated`, `thread/settings/updated`, `thread/goal/cleared`.

---

## Evidence for the verdict

### Test 1 — two programmatic clients (`two_client_sharing.py`)
A starts thread + turn 1 (rollout flushed). B `thread/resume {threadId}` → **ok**.
Then:
- A runs turn 2 → **B observed** `turn/started`, all `item/*`, `turn/completed` (identical list to A).
- B runs a turn → **A observed** the full identical stream.

`>>> B observed A's turn? True` and `>>> A observed B's turn? True`.

### Test 2 — real TUI interop (`tui_interop.py`)
Programmatic A starts thread + turn 1. Launch real
`codex resume <tid> --remote unix://$SOCK` (PTY). The TUI boot screen showed the
prior turn's preview (`"Reply with OK."`), proving it **loaded the same thread**.
Typed `"Reply with the single word FOUR and nothing else."` into the TUI. The
programmatic client A then received, after the TUI prompt:

```
A events after TUI prompt: [thread/goal/cleared, app/list/updated,
  thread/settings/updated, thread/status/changed, turn/started, item/started,
  item/completed, item/started, item/agentMessage/delta, item/agentMessage/delta,
  item/completed, thread/tokenUsage/updated, account/rateLimits/updated,
  thread/status/changed, turn/completed]
A saw agentMessage/userMessage texts: ['USER_MSG:Reply with the single word FOUR
  and nothing else.', 'FOUR']
```

A observed the exact text typed into the TUI and the agent's reply. **This is the
linchpin, end-to-end, with the real binary.**

---

## Caveats / gotchas

1. **Resume is rollout-(disk-)backed, not pure in-memory rejoin.** `thread/resume`
   resolves a thread by its persisted rollout file. A never-turned thread has no
   rollout (`thread.path` does not exist yet) → `-32600 "no rollout found"`. The
   second client can only join after at least one turn (or persisted items) exists.
   *Implication for push migration:* if the spec agent must attach a control/observer
   client to a freshly started thread, either (a) drive the first turn from the client
   that created the thread, or (b) start the thread from the daemon and hand the
   threadId to observers only after the first persisted activity. `thread/inject_items`
   does NOT create a rollout, so it alone is insufficient to make a thread resumable
   (it succeeded but did not enable a subsequent resume of a turn-less thread in
   testing — treat "rollout exists" as the gating condition).

2. **Socket directory must be user-owned.** Server chmods the socket's parent dir to
   `0700`; a shared sticky `/tmp` → `chmod("/tmp",0700)=EPERM` → `Operation not
   permitted (os error 1)` at boot. Use a per-user/per-run dir (e.g. `mktemp -d`,
   or `$XDG_RUNTIME_DIR`). Socket itself is `srw-------`.

3. **WebSocket, compression off.** The `unix://` transport is WebSocket; the client
   must offer **no `permessage-deflate`** or the upgrade is rejected. Raw JSON to the
   UDS is silently dropped (connection closed, 0 bytes). Budget for a real WS client
   (tungstenite/tokio-tungstenite in Rust) with compression disabled — not a raw
   length-prefixed framer.

4. **`codex app-server proxy --sock` did not bridge to a manual `--listen unix://`
   server** in this spike (same WS upgrade failure). Don't assume the proxy is a
   drop-in stdio shim for an arbitrary listen socket; the working programmatic path
   was a direct WS connection.

5. **Per-connection vs per-thread events.** Connection-lifecycle/housekeeping events
   (`remoteControl/status/changed`, `mcpServer/startupStatus/updated`, `app/list/updated`)
   are delivered per-connection. **Thread/turn/item events fan out to every connection
   attached to that thread** — that is exactly the property we need. (Server logs even
   show `targeted_connections=N` for outgoing events, confirming a multi-connection
   fan-out model.) `initialize.capabilities.optOutNotificationMethods` lets a client
   suppress specific methods if needed.

6. **`experimentalApi` capability.** All the relevant methods are `[experimental]`.
   Set `capabilities.experimentalApi=true` in `initialize` to be safe (the spike did,
   throughout).

7. **Latency:** sub-second handshake and event delivery on a local UDS; no perceptible
   lag between the two clients. Turn latency is just model latency.

---

## Render-fork recommendation

**Keep the PTY TUI as-is, add a PARALLEL programmatic control/observe channel.**
Do **not** migrate rendering onto app-server `item/*` notifications for this work.

Rationale, grounded in what was observed:

- The whole point of the migration (push) is satisfied **without** touching rendering:
  a programmatic client attached to the same thread receives the complete, real-time
  event stream (`thread/status/changed`, `turn/*`, `item/*`, token usage) for turns
  driven by *either* client — including a turn driven by the real TUI. So spec agents
  can switch from polling to a push subscription by simply opening a second app-server
  connection and `thread/resume`-ing the agent's thread. This is a low-risk, additive
  change.
- The TUI already renders perfectly over `--remote unix://` and loads the shared
  thread's history on resume. Re-implementing that rendering against raw `item/*`
  notifications would be a large, fragile rewrite (reasoning summaries, command-exec
  output deltas, file-change patches, approval prompts, plan deltas — all of which the
  TUI handles today) for no functional gain in this issue's scope.
- The bidirectional fan-out means the control channel can also *drive* (turn/start,
  turn/steer, inject_items, interrupt) the same thread the human TUI is on, which is
  the capability the push model wants. Keep rendering in the proven PTY TUI; build the
  control/observe path as a separate app-server WS client.

**Concrete shape for PR2+:** a Rust app-server client (tokio-tungstenite, WS over UDS,
compression disabled) that `initialize`s with `experimentalApi`, attaches to the
agent's thread via `thread/resume` (after first activity exists), subscribes to the
notification stream for push updates, and issues control RPCs. Run the TUI in parallel
with `--remote` for the human view. Gate the daemon socket in a user-owned directory.

---

## What was blocked by env

Nothing material. Model auth + network were available, so the full turn loop,
`turn/steer`, `thread/inject_items`, and the real-TUI interop were all exercised
end-to-end. The only friction encountered (and resolved/documented) was the
socket-directory `chmod` EPERM and the WebSocket-compression handshake requirement.
