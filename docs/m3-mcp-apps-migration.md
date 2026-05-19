# M3 → MCP Apps Migration — Design

**Status:** design ready for parallel implementation.
**Scope:** migrate Neige's M3 plugin dialect (`neige.*` JSON-RPC + `plugin:<id>:<view>` card kinds + hand-rolled iframe sandbox + `/api/plugins/.../views/...` HTML asset) to the **MCP Apps** specification (`2026-01-26`, https://github.com/modelcontextprotocol/ext-apps) wherever the standard matches, while keeping Neige-unique extensions (overlay annotations, KV store) clearly bracketed as a custom namespace.

Read alongside `/mnt/data2/kenji/neige/docs/m3-design.md`. Section numbers below match the structure of that doc where helpful (so §1 of the original maps onto §1 of the migration delta), but this is a standalone document — it does not require reading the old doc first to be useful.

---

## 0. Why migrate

Neige's M3 architecture independently arrived at ~80% of what the MCP Apps spec codifies:

| Neige (M3, shipped) | MCP Apps (spec 2026-01-26) | Verdict |
|---|---|---|
| Sandboxed iframes for plugin UI | Sandboxed iframes via AppBridge | Same pattern |
| Line-delimited JSON-RPC 2.0 over stdio | JSON-RPC 2.0 over stdio (core MCP) + JSON-RPC over postMessage (apps) | Same wire |
| Plugin can call into kernel (`neige.*`) | Iframe can call into host (`tools/call`, `ui/*`) and host into server (core MCP) | Same direction-pair |
| Permissions declared in `manifest.json` | Permissions declared in `_meta.ui.permissions` per resource + capability negotiation in `ui/initialize` | Same intent, different shape |
| Custom card-kind format `plugin:<id>:<view>` | `ui://<scheme-and-path>` resource URI | Migrate verbatim |
| Cookie-bound iframe HTML at `GET /api/plugins/:id/views/:view_id` | HTML served via `resources/read` returning `mimeType: "text/html;profile=mcp-app"` | Migrate |
| Hand-rolled `postMessage` schema (Slice F design) | `@modelcontextprotocol/ext-apps` AppBridge (host side) + `App` class (iframe side) | Adopt SDK |

The Neige-specific pieces that have **no MCP Apps analog** are the **overlay annotation system** and the **KV store**. Those stay in a `neige.*` JSON-RPC namespace alongside MCP standard methods on the same stdio connection.

Migration is **net-cheaper** than staying on the Neige dialect: the AppBridge module replaces ~500 lines of hand-rolled UI plumbing that we hadn't written yet (Slice F), and the standard `tools/call` declarative model collapses ~900 lines of `callbacks.rs` `neige.card.*` routing into one prefix-based handler. The pieces we own (overlays, KV) shrink to a focused custom namespace; everything else benefits from interop (any MCP-Apps host can host a Neige plugin, and any Neige-targeted plugin written against the standard SDK runs in Claude / VS Code / Goose unchanged).

---

## 1. The protocol gap — direct comparison table

Every current Neige method/concept and its MCP Apps equivalent. **migrate** = adopt the standard verbatim; **keep** = stays as a Neige extension (with rationale); **adapt** = same intent, different shape, mechanical translation.

### 1.1 Stdio JSON-RPC (kernel ↔ plugin process)

| Neige today | MCP Apps standard | Status | Notes |
|---|---|---|---|
| `initialize` with `params.clientInfo.expected_echo` + `result.serverInfo.echoed_token` | Standard MCP `initialize` with `capabilities.experimental` slot | **adapt** | Keep the auth-echo round-trip but move it from the `clientInfo` body into `_meta["dev.neige/auth"]`. The reserved-prefix rule (basic.md) allows `dev.neige/` since the second label is not `mcp` / `modelcontextprotocol`. |
| `notifications/initialized` (already correct) | Standard | **keep** | No change. |
| `notifications/shutdown` (kernel→plugin) | Standard | **keep** | No change. |
| `tools/list` discovery (already correct) | Standard | **keep** | Plugins register `exposes_tools` declaratively + the kernel rediscovers via `tools/list`. |
| Plugin-initiated `neige.card.create` | Plugin registers a `tools/call`-callable tool; kernel invokes via `tools/call`; tool's `_meta.ui.resourceUri` points at the iframe | **migrate** | Inverts ownership: cards now created **by the kernel calling the plugin** instead of the plugin pushing into the kernel. See §1.4. |
| Plugin-initiated `neige.card.update` / `delete` | Standard MCP **does not** have a "tell the host to mutate one of its own objects" verb. Use **resource subscriptions** + `notifications/resources/updated` for read, but writes back to the kernel database remain a Neige extension. | **keep as `neige.card.*`** | The kernel is the source of truth for cards; the plugin needs an authenticated channel to write. MCP doesn't have this verb. |
| Plugin-initiated `neige.overlay.set` / `delete` | **No equivalent** — Neige innovation (annotation, not "render UI") | **keep** as `neige.overlay.*` | See §2.1 for the design value. |
| Plugin-initiated `neige.event.subscribe` | MCP `notifications/resources/updated` after `resources/subscribe` (server-side, but inversion applies) | **adapt** | Use the resource-subscribe primitive for entity events: kernel exposes one resource per cove/wave/card; plugin subscribes; kernel notifies. The current bespoke topic-glob filter (`card:*`, `wave:*`) maps onto URI patterns (`neige://wave/*`). |
| Plugin-initiated `neige.kv.*` | **No equivalent** | **keep** as `neige.kv.*` | See §2.2. Open question whether to propose upstream. |
| Plugin-initiated `neige.tree.read` | `resources/read` with `uri: "neige://tree"` or `neige://cove/<id>`, optionally listed via `resources/list` | **migrate** | Read access fits cleanly into MCP resources. |
| Error codes `-32001..-32005` (`PluginPermissionDenied`, `PluginOwnershipMismatch`, `PluginQuotaExceeded`, `EntityNotFound`, `KernelOverloaded`) | MCP allows implementation-defined codes outside `-32768..-32000`. Standard suggestion: include `_meta` with structured detail. | **keep** | Codes are kernel-specific; MCP doesn't standardize beyond the reserved JSON-RPC range. |

### 1.2 Plugin discovery and UI surface

| Neige today | MCP Apps standard | Status | Notes |
|---|---|---|---|
| `manifest.json` with `views[]` array | Tools declared via `tools/list`, each with `_meta.ui.resourceUri` pointing at a `ui://` resource | **migrate** | The manifest stays as the install-time descriptor (permissions + entrypoint), but `views[]` becomes redundant with `tools/list` once the plugin registers tools with `_meta.ui`. |
| Manifest `views[].entry_html` (file path) | Resource served via `resources/read` returning `mimeType: "text/html;profile=mcp-app"` | **migrate** | Spec calls this the "MCP App mime profile." Kernel still reads the file from disk; just changes how it's exposed. |
| `GET /api/plugins/:id/views/:view_id` (REST) | `resources/read { uri: "ui://<plugin>/<view>" }` over the MCP stdio connection | **migrate** | The HTTP route stays as an internal implementation detail of how the kernel materializes the iframe `srcdoc`, but the wire fetch becomes resources/read. |
| `default_size: { w, h, min_w, min_h }` in manifest | **No equivalent** in `_meta.ui`. Spec has `containerDimensions` (read-only host→view) and `ui/notifications/size-changed` (view→host) but no preferred-default. | **keep** as Neige extension on the manifest. | See §7 risk. Manifest's `default_size` stays the source of truth for AddPanel-driven card creation. |
| Card kind `plugin:<id>:<view>` (the discriminator on `Card.kind`) | `ui://` resource URI. Spec suggests `ui://<server-name>/<path>` shape, no hard rule. | **migrate** | We pick `ui://<plugin-id>/<view-id>` so the URI is reversible to the existing `(plugin_id, view_id)` pair. |
| `Card.payload` (opaque JSON the plugin/iframe reads) | Tool result delivered to the iframe via `ui/notifications/tool-result` after the host calls `tools/call` | **migrate** | The plugin tool returns `structuredContent: {...}`, which the host pushes into the iframe — exactly the existing payload semantics. |

### 1.3 Iframe ↔ host postMessage

| Neige Slice F design (not yet built) | MCP Apps `2026-01-26` | Status |
|---|---|---|
| Custom envelope `CalmMsg<TKind extends string, TData> { kind, request_id?, data }` | Standard JSON-RPC 2.0 frames | **migrate** |
| Host→iframe `card-data` | Host→iframe `ui/notifications/tool-result` (`structuredContent` carries `Card.payload`) | **migrate** |
| Host→iframe `card-data-updated` | Host→iframe `ui/notifications/tool-result` (subsequent) | **migrate** |
| Host→iframe `theme-changed` | Host→iframe `ui/notifications/host-context-changed` with `theme: "light"\|"dark"` | **migrate** |
| Host→iframe `resize-hint { px_w, px_h, cell_w, cell_h }` | Host→iframe `ui/notifications/host-context-changed` with `containerDimensions: { width, height, maxWidth, maxHeight }` | **migrate** (drop `cell_w`/`cell_h`; the iframe doesn't need RGL grid units, it just needs CSS pixels) |
| Iframe→host `view-ready` | Iframe→host `ui/notifications/initialized` | **migrate** |
| Iframe→host `request-overlay-write` | `tools/call { name: "neige.overlay.set", arguments: {...} }` — overlays exposed as a kernel-provided tool the iframe can invoke | **migrate** |
| Iframe→host `request-card-update` | `tools/call { name: "neige.card.update", arguments: {...} }` | **migrate** |
| Iframe→host `set-content-height { px }` | Iframe→host `ui/notifications/size-changed { width, height }` | **migrate** |
| Iframe→host `request-modal` | **No equivalent.** Closest is `ui/request-display-mode { mode: "fullscreen" }` — different semantics (it morphs the host's chrome, doesn't pop a modal). | **drop in M3-apps, defer** — modals weren't shipped anyway; revisit when a plugin needs one. |
| Iframe→host `log` | Iframe→host `notifications/message { level, logger, message }` | **migrate** |
| `iframe-token` cookie (per-card, 15-min sliding) | AppBridge handles iframe ↔ host channel auth implicitly via the `MessageChannel` it owns; the host enforces tool gating via `appBridge.capabilities.serverTools` | **adapt** — see §3.4. |

### 1.4 Inversion of control: tools/call as the card-creation primitive

The single biggest conceptual shift. Today:

```
plugin process --neige.card.create--> kernel
                                       writes Card row
                                       emits CardAdded event
                                       UI re-renders, mounts iframe
```

With MCP Apps, the natural shape is:

```
user clicks AddPanel item (a tool the plugin registered with _meta.ui.resourceUri)
  → kernel calls plugin's tool via tools/call
  → plugin returns CallToolResult { structuredContent, _meta.ui.resourceUri }
  → kernel writes Card row (kind = ui://..., payload = structuredContent)
  → emits CardAdded
  → UI mounts iframe, fetches resource via resources/read, hands AppBridge the payload
```

The plugin still controls **when** a card is created (it returns a CallToolResult), but the kernel **drives** the call. For autonomous plugins that want to spontaneously create cards (today: hello-world via `neige.card.create`), keep a Neige-side write path under the name `neige.card.create` — the kernel-callable handler in `callbacks.rs` stays for this case. **Most** plugins will move to the declarative `tools/call`-driven model.

---

## 2. What stays Neige-specific

### 2.1 The overlay system — the crown jewel

Overlays are **annotations on existing entities**, not "render this UI." A `wave.status = running` overlay decorates the kernel-owned wave row; it tells the renderer "show a green dot," not "mount an iframe." MCP Apps explicitly models the second case (tool returns a UI to render) and has no first-class verb for the first.

**Why MCP Apps doesn't have this:** the spec is built around a chat host where each "rendering" is a discrete tool result. Persistent decoration of host-native objects ("this calendar event has urgency=high attached by the urgency plugin") isn't in scope for the apps extension; the closest analog would be resource subscriptions where the resource's `_meta` gets updated — but that's read-side, not write-side, and conflates two plugins' annotations on the same resource.

**Recommendation:** keep `neige.overlay.set` / `neige.overlay.delete` as Neige extensions in the `neige.*` JSON-RPC namespace, alongside MCP standard methods on the same stdio pair. The plugin SDK (when we publish one) wraps them as `client.overlay.set({ entity, kind, payload })`.

**Optional upstream proposal:** the MCP Apps working group might be interested in this as a "host-state annotation" primitive — but propose only after we have two real plugins using it (urgency overlay + status overlay, today only hello-world demonstrates the wire). Don't pre-propose vaporware.

**Subscriber side:** overlay changes do map onto `notifications/resources/updated` for any plugin (or the UI) subscribed to the entity. We migrate the **read** side (other plugins consuming overlay changes) to `resources/subscribe + notifications/resources/updated`; we keep the **write** side as `neige.overlay.set` because writes have a different security model (which plugin owns this annotation?) that resources/subscribe doesn't address.

### 2.2 KV store

`neige.kv.*` is a per-plugin key-value scratchpad. MCP Apps has nothing equivalent; the closest workaround would be for each plugin to expose its own `kv://...` resource namespace, but that puts persistence semantics into the resource layer where they don't belong.

**Recommendation:** keep `neige.kv.*` as a Neige extension. Quota and per-plugin isolation are already enforced in `callbacks.rs`; nothing changes. **Do not propose upstream** — it's a kernel implementation detail, not a protocol concern. Plugins that prefer their own SQLite file are free to ignore it.

### 2.3 Auth-token echo

The Slice H token echo (`expected_echo` / `echoed_token` in `initialize`) is a Neige-specific anti-FD-hijack measure. MCP's standard `initialize` has no auth field — it leaves auth to the transport. Since our transport is stdio with a process-private fd pair, the spec is satisfied; the echo is belt-and-suspenders.

**Recommendation:** move the echo from top-level `clientInfo.expected_echo` into `clientInfo._meta["dev.neige/auth"] = { expected_echo }` and `serverInfo._meta["dev.neige/auth"] = { echoed_token }`. The `_meta` slot is exactly designed for this kind of namespaced extension and won't collide with future MCP standard auth.

---

## 3. Iframe runtime — adopt AppBridge

### 3.1 What AppBridge gives us

From the basic-host example (`examples/basic-host/src/implementation.ts` in the ext-apps repo), AppBridge is instantiated as:

```ts
import {
  AppBridge,
  PostMessageTransport,
  type McpUiResourceCsp,
  type McpUiResourcePermissions
} from "@modelcontextprotocol/ext-apps/app-bridge";

const appBridge = new AppBridge(serverInfo.client, IMPLEMENTATION, {
  openLinks: {},
  serverTools: serverCapabilities?.tools,
  serverResources: serverCapabilities?.resources,
  updateModelContext: { text: {} },
}, {
  hostContext: {
    theme: getTheme(),
    platform: "web",
    styles: { variables: HOST_STYLE_VARIABLES },
    containerDimensions: options?.containerDimensions ?? { maxHeight: 6000 },
    displayMode: options?.displayMode ?? "inline",
    availableDisplayModes: ["inline", "fullscreen"],
  },
});
await appBridge.connect(
  new PostMessageTransport(iframe.contentWindow!, iframe.contentWindow!)
);
```

What we get for free:

- **iframe lifecycle:** create, attach to a sandboxed `<iframe srcdoc=...>` (the double-iframe pattern: outer "sandbox proxy" iframe enforces CSP + permissions, inner iframe is the actual plugin UI). We do not need to write the sandbox proxy ourselves.
- **postMessage framing:** JSON-RPC 2.0 frames with id correlation, all the `ui/*` and `notifications/*` methods listed in §1.3 are already implemented.
- **`tools/call` proxying:** iframe calls `app.callServerTool({ name, arguments })`; AppBridge forwards the JSON-RPC frame; we route the result.
- **Capability negotiation:** `ui/initialize` handshake exchanges `appCapabilities` and `hostCapabilities` automatically.
- **CSP + permissions enforcement:** `_meta.ui.csp` (list of allowed connect/resource/frame/baseUri domains) and `_meta.ui.permissions` (camera, microphone, geolocation, clipboardWrite — closed set per spec) are honored by the sandbox proxy.

### 3.2 What we layer on top

AppBridge's sandbox model assumes the iframe is **untrusted** (it explicitly cannot reach `window.parent`). The Neige extras we need:

- **Per-plugin auth:** AppBridge doesn't know about our `iframe_token` cookie. We replace the cookie scheme entirely — under MCP Apps, the iframe's only outbound channel is postMessage→AppBridge→host, and the host is the same process that owns the MCP connection. **The iframe-token cookie ceases to exist.** Slice H's process-side token (still issued, still echoed in `initialize`) remains; only the iframe-token half goes away.
- **Card identity:** `tools/call` from the iframe needs to know "which card am I." AppBridge passes the tool's `_meta.toolInfo.id` in `hostContext.toolInfo`; we put `card_id` in there at iframe mount time. The plugin tool then receives the card_id as part of its arguments via host-side rewriting (the host appends `_meta.neige.card_id` before forwarding to the plugin's `tools/call`).
- **Theme:** AppBridge has `hostContext.theme: "light"|"dark"` natively. Drop the custom `theme-changed` envelope from Slice F's spec.
- **Overlay/card writes from iframe:** the iframe calls `app.callServerTool({ name: "neige.overlay.set", arguments: {...}})`. The host treats this as a kernel-namespace tool (not forwarded to the plugin server); the host's tools/call handler routes `neige.*` names into the existing `callbacks.rs::dispatch`. The plugin process never sees this call — the same security model as today, just with a different wire format.

### 3.3 What gets deleted vs kept from Slice H

Slice H shipped per-plugin process tokens **and** per-iframe cookie tokens. After migration:

- **kept:** `auth.rs` process-token mint/verify (echo on `initialize`). The token rotation + hashing logic in `plugin_tokens` table stays.
- **deleted:** iframe-token cookie minting in `routes/plugins.rs::view_html`. AppBridge owns the iframe ↔ host channel; the host's authority over postMessage replies (it has the user session cookie + the in-memory `AppBridge` instance bound to a specific card) is sufficient. No second token to validate.
- **deleted:** `POST /api/plugins/:id/iframe-write` route. Iframe writes are now `tools/call` over postMessage, intercepted by the host, dispatched to the kernel via the MCP connection.

Rough estimate: ~250 lines of cookie-extractor + iframe-cookie-name plumbing in `routes/plugins.rs` go away; ~80 lines of token-mint in `auth.rs` stay; net ~170 LoC deleted in the kernel.

### 3.4 Sandboxing — is AppBridge strict enough?

Slice F's original CSP was:

```
default-src 'self'; script-src 'self' 'unsafe-inline';
connect-src 'none'; frame-ancestors 'self';
```

AppBridge's double-iframe sandbox uses `srcdoc` (the inner iframe never touches the network) and the CSP comes from `_meta.ui.csp.connectDomains` etc. The defaults if no CSP is declared on the resource: no network access at all (matches our `connect-src 'none'`). **Verdict:** strictly stronger than what we'd have hand-rolled — the sandbox proxy is a separate origin between the host and the untrusted code.

The one regression to call out: AppBridge's sandbox uses `allow-same-origin` on the **outer** sandbox proxy (it has to, to host the inner srcdoc), which means a defect in the sandbox-proxy code could escape. The mitigation is that the sandbox-proxy is shipped by `@modelcontextprotocol/ext-apps` and audited as part of the upstream package — much better than us auditing our own.

---

## 4. Plugin authoring — what changes

### 4.1 Hello-world today (215 LoC, `plugins/hello-world/src/main.rs`)

Hand-rolls every byte of the wire: env intake, stdin line-reader, the JSON shape of `initialize` reply, the `serverInfo.echoed_token` field, one outbound `neige.overlay.set` request, SIGTERM handler, stdin EOF detection.

### 4.2 Hello-world after migration (pseudocode, ~40 LoC)

Plugin authors pull in `mcp` (the official Rust SDK once it lands) or the TypeScript/Python SDK. The plugin's `main` becomes:

```rust
// pseudocode — exact crate name TBD, currently mcp-spec/rust-sdk
use mcp::{Server, ToolHandler, tool, init_capability};
use mcp::ext::{NeigeExt};  // our thin shim — see §4.3

#[tokio::main]
async fn main() {
    let mut server = Server::new("neige-hello-world", "0.1.0");

    // Standard MCP: register a tool that creates a card.
    server.tool("hello.create_status_card", |args, ctx| async move {
        let wave_id = args["wave_id"].as_str().ok_or("wave_id")?;
        Ok(CallToolResult {
            structured: json!({ "state": "running", "source": "hello-world" }),
            meta: Some(json!({
                "ui": { "resourceUri": "ui://dev.neige.hello-world/status" }
            })),
        })
    });

    // Standard MCP: register the HTML resource.
    server.resource("ui://dev.neige.hello-world/status", |_| async {
        Ok(ResourceContents {
            mime_type: "text/html;profile=mcp-app".into(),
            text: include_str!("../views/status.html").into(),
            meta: Some(json!({ "ui": { "permissions": {}, "csp": {} }})),
        })
    });

    // Neige extension (auth echo + overlay write).
    let neige = NeigeExt::from_env().expect("Neige kernel env vars");
    server.on_init(move |req| async move {
        neige.echo_auth(req)?;
        // Optionally also push an autonomous overlay on startup, matching
        // hello-world's current "I'm up" behavior:
        neige.overlay_set("wave", &neige.demo_wave(), "status",
                           json!({ "state": "running" })).await?;
        Ok(())
    });

    server.run_stdio().await.unwrap();
}
```

What this collapses:

- All JSON-RPC framing (~80 LoC currently) → SDK handles it.
- The `initialize` echo round-trip → `NeigeExt::echo_auth` wraps it (~5 LoC of plugin).
- The `neige.overlay.set` outbound call → `NeigeExt::overlay_set` (1 LoC of plugin).
- The stdin-drain main loop → `server.run_stdio()` is the entire loop.

The view HTML (`views/status.html`) stays unchanged but its iframe code switches from a Neige-bespoke postMessage envelope to the standard `@modelcontextprotocol/ext-apps/react` `useApp` hook (or the vanilla `App` class).

### 4.3 The `NeigeExt` shim

A small crate (`neige-plugin-ext` in Rust, `@neige/plugin-ext` in TypeScript) that wraps the `neige.*` JSON-RPC namespace as ergonomic methods. Holds the kernel-callback connection (which is just the same `mcp::Server` instance — the kernel and the plugin both speak MCP on the same stdio pair, and `neige.*` is a custom method prefix recognized on the client side via the `experimental.dev.neige/kernel-callbacks` capability). API sketch:

```rust
impl NeigeExt {
    pub async fn overlay_set(&self, kind: &str, id: &str, key: &str, payload: Value) -> Result<()>;
    pub async fn overlay_delete(&self, kind: &str, id: &str, key: &str) -> Result<()>;
    pub async fn kv_get(&self, key: &str) -> Result<Option<Value>>;
    pub async fn kv_set(&self, key: &str, value: Value) -> Result<()>;
    pub async fn subscribe_events<F>(&self, glob: &str, handler: F) -> Result<SubscriptionId>;
}
```

Only the `neige.*` methods live here; standard MCP `tools/*`, `resources/*`, etc. come from the upstream SDK.

---

## 5. Backwards compatibility

The migration breaks wire compatibility for plugins that hand-rolled the dialect. Hello-world is the only one shipped. The kernel can support both wires for one minor version.

### 5.1 What breaks

1. **`neige.card.create` is still callable** (we keep the handler for autonomous-card plugins) — no break.
2. **`neige.overlay.set` is still callable** — no break.
3. **Iframe HTML served via REST (`GET /api/plugins/:id/views/:view_id`)** stops setting an iframe cookie; existing iframes that posted to `/api/plugins/:id/iframe-write` break. Migration: the route is replaced by the `resources/read` MCP method routed through the host's postMessage handler. The HTTP route can be kept as a Slice M3-deprecated alias that returns the HTML body but no cookie; the iframe code (in `views/status.html`) must be rewritten to use `useApp` instead of hand-rolled postMessage. **Hello-world's `views/status.html` is the one file that has to be touched.**
4. **Card-kind `plugin:<id>:<view>`** stays valid in the database; adapt.ts learns both forms (`plugin:` and `ui://`). New cards created via the AddPanel use `ui://`. Old rows continue to render. After two minor versions, deprecate the `plugin:` form via a migration that rewrites kinds in place.
5. **`initialize.params.clientInfo.expected_echo`** moves to `initialize.params.clientInfo._meta["dev.neige/auth"].expected_echo`. The kernel accepts both during a deprecation window (one minor version); the hello-world plugin updates to read from the new location.

### 5.2 Kernel tests

We have ~110 tests touching the plugin host (`grep -c #\[test\]` across `plugin_host/` + `tests/plugin*`). Predicted impact:

- **`callbacks.rs` tests** (~30 tests): unchanged for `neige.overlay.*`, `neige.kv.*`, `neige.event.subscribe`. The `neige.card.*` tests (~6) stay (we keep the handler) but get a parallel suite for the `tools/call`-routed path.
- **`mcp.rs` tests** (5 tests, framing-only): unchanged.
- **`auth.rs` tests** (~12 tests): one test asserting `clientInfo.expected_echo` shape needs to move to `clientInfo._meta["dev.neige/auth"].expected_echo`.
- **`routes/plugins.rs` tests** (~30 tests): the `view_html` cookie test (~4 tests) gets deleted; new test asserts `resources/read` over the MCP wire returns the HTML.
- **Hello-world e2e** (`tests/plugin_e2e.rs`): rewritten end-to-end to drive the new wire.

Net: roughly 50 tests touched, 6 deleted, 12 added. No DB migrations.

---

## 6. Implementation slices

Same shape as the original M3 design's §8. Six slices, sized for one worktree each. Slice ordering below is **integration order** (M1 must merge before M3, etc.); M5 + M6 can start in parallel with M3 once M2 is in.

### Slice M1 — adopt MCP standard `initialize` capability slot

**Goal:** auth-echo handshake moves from top-level `clientInfo.expected_echo` to spec-blessed `_meta["dev.neige/auth"]`; `experimental.dev.neige/kernel-callbacks` capability is the formal opt-in for plugins that want to call `neige.*` back into the kernel.

**Touch:**
- `/mnt/data2/kenji/neige/crates/calm-server/src/plugin_host/mcp.rs` — move echo field to `_meta["dev.neige/auth"]` only. **Hard cut** per resolved §7.6 row 2 — no dual-accept fallback.
- `/mnt/data2/kenji/neige/plugins/hello-world/src/main.rs` — read echo from the new path. (Only legacy reader; rewriting in M6 anyway.)

**Public interface produced:** the `_meta["dev.neige/auth"]` wire location.

**Depends on:** nothing.

**Test surface:** `mcp.rs::tests` echo path updated to assert the new location.

### Slice M2 — replace `neige.card.create` with `tools/call` routing

**Goal:** card creation through AddPanel calls the plugin's tool via standard `tools/call`; the kernel's `tools/call` result handler writes the Card row. Autonomous card creation (`neige.card.create`) stays as a Neige extension for plugins that need it.

**Touch:**
- `/mnt/data2/kenji/neige/crates/calm-server/src/plugin_host/mcp.rs` — `McpClient::tools_call(name, arguments) → CallToolResult`.
- `/mnt/data2/kenji/neige/crates/calm-server/src/plugin_host/callbacks.rs` — add `handle_tool_call_result` that extracts `_meta.ui.resourceUri` and rewrites it into the Card.kind on insert.
- `/mnt/data2/kenji/neige/crates/calm-server/src/routes/cards.rs` — the existing card-create REST route gains a `via_tool_call: Option<{ plugin_id, tool_name, arguments }>` payload variant.

**Public interface produced:** the `tools/call` routing pathway; `Card.kind` can now carry `ui://...`.

**Depends on:** M1.

**Test surface:** integration test — a stub plugin registers a `make_status_card` tool returning `_meta.ui.resourceUri = "ui://stub/status"`; the kernel calls it; assert a Card row exists with that kind.

### Slice M3 — replace iframe HTML asset path with `resources/read`

**Goal:** the iframe HTML is served via MCP `resources/read` instead of the `GET /api/plugins/:id/views/:view_id` REST route. **Hard cut** per resolved §7.6 row 2 — the legacy REST route is deleted, not kept as an alias.

**Touch:**
- `/mnt/data2/kenji/neige/crates/calm-server/src/plugin_host/mcp.rs` — `McpClient::resources_read(uri) → ResourceContents`.
- `/mnt/data2/kenji/neige/crates/calm-server/src/plugin_host/resources.rs` — new. Map `ui://<plugin>/<view>` → file path under `<install_path>/views/<view>.html`; emit `_meta.ui.csp` + `_meta.ui.permissions` from the manifest's view-level CSP block (which we add as an optional field on `View`).
- `/mnt/data2/kenji/neige/crates/calm-server/src/plugin_host/manifest.rs` — optional `View.csp: Option<CspBlock>` and `View.permissions: Option<UiPermissions>` mirroring the `_meta.ui` shape.
- `/mnt/data2/kenji/neige/crates/calm-server/src/routes/plugins.rs` — **delete** the `view_html` route + the iframe-cookie mint side-effect; AppBridge owns the iframe transport from M5 onward.

**Public interface produced:** `ui://` resource scheme served via `resources/read`; manifest knows about CSP + permissions.

**Depends on:** M1.

**Test surface:** new `resources.rs::tests` — round-trip a manifest declaring CSP, fetch the resource, assert `_meta.ui` carries it.

### Slice M4 — `ui://` resource URI in `adapt.ts` and the card registry

**Goal:** the UI accepts **only** `ui://<plugin>/<view>` card kinds. AddPanel's data source becomes MCP `tools/list` filtered by `_meta.ui.resourceUri` present, not the kernel's manifest-derived `/api/plugins/views` catalog. **Hard cut** per resolved §7.6 row 2 — the legacy `plugin:<id>:<view>` parser is deleted; hello-world rewrites in M6.

**Touch:**
- `/mnt/data2/kenji/neige/web-calm/src/api/adapt.ts` — `adaptCard` parses `ui://` only; emits `PluginCardData { plugin_id, view_id, resource_uri }` derived from the URI.
- `/mnt/data2/kenji/neige/web-calm/src/cards/registry.ts` — the registry indexes by `ui://` URI (not the `plugin:` slug).
- `/mnt/data2/kenji/neige/web-calm/src/cards/plugin-iframe.tsx` — accepts `resource_uri` instead of `(plugin_id, view_id)` as the iframe identifier.
- `/mnt/data2/kenji/neige/crates/calm-server/src/routes/plugins.rs` — `/api/plugins/views` returns each entry with `resource_uri: "ui://<plugin>/<view>"`; legacy `plugin_id`/`view_id` fields removed from the response shape.

**Public interface produced:** `Card.kind` is `ui://...`; UI dispatches on URI.

**Depends on:** M2 (for new cards) and M3 (for the resources/read path).

**Test surface:** adapt.ts unit tests asserting `ui://` parse + reject of malformed; route-level test for `/api/plugins/views` emitting only `resource_uri`.

### Slice M5 — drop in AppBridge on the UI side (replaces Slice F)

**Goal:** the plugin-iframe component uses `@modelcontextprotocol/ext-apps` AppBridge on the host (web-calm) side. The bridge owns the iframe element, the postMessage transport, the CSP, and the tool-call proxying.

**Touch:**
- `/mnt/data2/kenji/neige/web-calm/package.json` — add `@modelcontextprotocol/ext-apps`, `@modelcontextprotocol/sdk`.
- `/mnt/data2/kenji/neige/web-calm/src/cards/plugin-iframe.tsx` — rewrite. Instantiate `new AppBridge(neigeClient, hostInfo, capabilities, { hostContext })` per mounted iframe; `await bridge.connect(new PostMessageTransport(...))`. Pass card `payload` via the `hostContext.toolInfo` slot. Wire `bridge.onToolCall` → kernel REST POST `/api/plugins/:id/tool-call` (the host-side fan-out endpoint; see below).
- `/mnt/data2/kenji/neige/web-calm/src/api/calm.ts` — replace `iframeWrite()` with `toolCallFromIframe(pluginId, name, arguments)` that POSTs to a new kernel route.
- `/mnt/data2/kenji/neige/crates/calm-server/src/routes/plugins.rs` — new `POST /api/plugins/:id/tool-call` that the web-calm host (logged-in user session cookie) hits with the iframe's outbound tool-call. Kernel checks: is the user's session valid, does `name` start with `neige.` (kernel-namespace) or is it a tool the plugin exposes? Dispatches accordingly. Replaces the deleted `iframe-write` route.

**Public interface produced:** working iframes that speak the MCP Apps wire.

**Depends on:** M2, M3, M4.

**Test surface:** Playwright/Vitest test that mounts the AppBridge against a stub kernel (the existing tools/call-stub from M2) and asserts the iframe's `ui/initialize` round-trips.

### Slice M6 — migrate hello-world onto the new wire

**Goal:** prove the wire end-to-end with a plugin written against the minimal inline MCP client + the `neige-plugin-ext` shim.

**Touch:**
- `/mnt/data2/kenji/neige/plugins/hello-world/Cargo.toml` — depend on `neige-plugin-ext` workspace crate. **No external MCP SDK dependency** per resolved §7.6 row 4 — Anthropic's official Rust SDK isn't on crates.io yet and we don't pin to community packages.
- `/mnt/data2/kenji/neige/plugins/hello-world/src/main.rs` — rewrite to ~40 LoC using `neige-plugin-ext`.
- `/mnt/data2/kenji/neige/plugins/hello-world/views/status.html` — rewrite the iframe code to use `@modelcontextprotocol/ext-apps` via `<script type="module">` instead of the bespoke postMessage envelope.
- `/mnt/data2/kenji/neige/crates/neige-plugin-ext/` — new workspace crate (~150 LoC) exposing `NeigeExt`, wrapping the `neige.*` JSON-RPC namespace and a minimal MCP `initialize`/`tools/list`/`tools/call`/`resources/read` client. When Anthropic ships an official Rust SDK, swap the inner client for it; the `NeigeExt` API stays.

**Public interface produced:** a reference plugin demonstrating the entire stack.

**Depends on:** M1, M2, M3, M4, M5.

**Test surface:** the existing `plugins/hello-world/demo.sh` e2e script should pass unchanged after migration (kernel boots, plugin installs, plugin enables, overlay event arrives over WS) — but now the plugin code is the SDK-based version.

---

## 7. Risks and open questions

### 7.1 AppBridge might not let us inject overlay-write capability

**Risk:** AppBridge's capability negotiation enumerates `serverTools`, `serverResources`, `openLinks`, `updateModelContext` — there's no slot for "kernel-namespace tools the iframe can call." The iframe might call `tools/call { name: "neige.overlay.set" }` expecting it to be a server-side tool, and AppBridge could refuse to forward because the plugin server didn't register it via `tools/list`.

**Mitigation:** the kernel-side host-bridge intercepts `tools/call` before forwarding. If `name` starts with `neige.`, the host fan-out route (`POST /api/plugins/:id/tool-call` from M5) routes to the in-kernel handler in `callbacks.rs`; the plugin process never sees the call. AppBridge's API allows custom `onToolCall` handlers per the basic-host example. Verified-pattern.

**Fallback:** if AppBridge proves rigid, register `neige.overlay.set` as a synthetic entry in the kernel's `tools/list` response (we own the kernel-side tool catalog, so we can add fake-but-real tools). This adds noise but works around any rigidity.

### 7.2 `tools/call` scoping for plugin-owned cards

**Risk:** today, `callbacks.rs::card_update` checks `card.kind.starts_with(format!("plugin:{}:", self.plugin_id))`. Once we move to `ui://`, the check becomes `card.kind.starts_with(format!("ui://{}/", self.plugin_id))`. Straightforward, but the `tools/call`-driven creation path (M2) needs the kernel to attribute the new Card to the right plugin — `_meta.ui.resourceUri` doesn't carry the plugin id explicitly; we have to parse it from the URI.

**Mitigation:** `ui://` URI authority component **is** the plugin id by convention (we pick the URI shape). Parsing is regex-trivial. Add a validator at insert time that rejects card creation if `tool_call.plugin_id != parse_authority(resource_uri)`.

**Fallback:** thread the plugin id explicitly through the host route (M5) so we never depend on URI parsing.

### 7.3 `ui://` resource scheme has no slot for `default_size` or `scope: "card"`

**Risk:** `_meta.ui` in the spec is a closed object: `csp`, `permissions`, `domain`, `prefersBorder`. Our `default_size: { w, h, min_w, min_h }` and `scope: "card"` need a home.

**Mitigation:** the spec permits arbitrary additional keys under `_meta` (the `_meta` slot is explicitly extensible). Use `_meta.ui._neige = { default_size, scope }`. AppBridge will pass it through transparently because it doesn't validate `_meta` strictly.

**Fallback:** if AppBridge does drop unknown `_meta` keys, keep `default_size` in the Neige manifest and look it up server-side at AddPanel render time (this is what we do today; nothing breaks).

### 7.4 AppBridge assumes an LLM in the loop

**Risk:** the MCP Apps spec is written assuming a chat host with an LLM driving `tools/call`. Neige has no LLM; the user clicks AddPanel and the host directly invokes `tools/call`. The `hostInfo` slot might want chat-specific fields (`tools/list_changed`, agent state, etc.).

**Mitigation:** Goose, VS Code Copilot, and Postman are listed as supported MCP Apps hosts, and Postman emphatically has no LLM in the active loop — its host is "user clicks button → call tool." The spec explicitly says the protocol is LLM-agnostic. `ui/notifications/tool-input-partial` (streaming arguments from an LLM-typing-into-the-tool) is optional; we never emit it. `ui/update-model-context` is also iframe-initiated, we can stub it. **No real blocker.**

**Fallback:** none required. The spec is general-purpose; we're a non-chat host, which is a known shape.

### 7.5 Plugins that already use `neige.card.create` autonomously

**Risk:** hello-world calls `neige.overlay.set` autonomously on startup (not in response to a tools/call). The post-migration plugin model is "respond to tools/call." Autonomous overlay/card writes don't fit the declarative `_meta.ui.resourceUri`-on-a-tool pattern.

**Mitigation:** keep `neige.card.create` and `neige.overlay.set` as Neige extensions on the same MCP connection — explicitly outside the tools/call model. This is exactly the "experimental.dev.neige/kernel-callbacks" capability we already declare on `initialize`. The new SDK shim (§4.3) wraps them so plugin authors don't see the difference.

**Fallback:** none — keeping the autonomous-write path is the design decision.

### 7.6 Open questions — resolved 2026-05-19

| # | Question | Decision |
|---|---|---|
| 1 | `ui://` URI authority shape | **`ui://<plugin_id>/<view_id>`** with `/<asset>` extension for multi-resource views. |
| 2 | Deprecation timeline for legacy `plugin:<id>:<view>` | **Hard cut, no dual-accept.** Only hello-world uses it today; rewrite that one plugin in M6, drop the adapter. |
| 3 | Propose `neige.overlay.*` upstream now? | **Wait** until two real plugins exercise the shape before proposing — avoids bikeshedding on a one-example basis. |
| 4 | Rust MCP SDK choice | **Hand-roll a minimal inline client** (today's `mcp.rs` is the starting point). When Anthropic ships an official `rust-mcp-sdk` on crates.io, swap to it. Don't take a community-pkg dependency. |
| 5 | iframe `tools/call` authorization | **Deny by default for the plugin's own server tools** — iframes can only call `neige.*` kernel-namespace tools. Matches today's "iframe-write is overlay-only" gate. |

---

## 8. Recommendation

**Ship the migration.** Three reasons, in priority order:

1. **The bulk of the work is replacing code we hadn't written yet with code that's already audited.** Slice F (~500 LoC of iframe runtime + postMessage envelope + theme propagation + sandbox + iframe-cookie auth) is replaced by ~50 LoC of AppBridge wiring + ~150 LoC of host-route dispatch. The CSP enforcement, capability negotiation, message-id correlation are upstream concerns. We do not have to maintain that surface.

2. **Interop is real, not theoretical.** A plugin author can write one `tools/call`-shaped server and run it in Claude, VS Code Copilot, Goose, Postman, MCPJam, **and** Neige. Today we'd be asking authors to learn a bespoke wire that only Neige speaks. The cost to plugin authors of "learn the Neige dialect" is the cost we're not paying — that's where the real leverage is.

3. **The pieces we keep are sharper after migration, not blurred.** Overlays + KV become an explicit `experimental.dev.neige/*` capability bracketed by spec-blessed `_meta` extensions; today they're commingled with `neige.card.*` (which migrates to standard `tools/call`) and the difference between "kernel-callback" and "tool-result-driven write" is implicit. After migration the line is clean: standard MCP wire for everything that fits the standard, `neige.*` namespace for the genuinely Neige-unique semantics. That's a better story to defend, both to plugin authors and (eventually) to the MCP working group if we ever propose upstream.

**The pushback we should hear from the user but do not.** The cleanest counterargument would be "we already have working code; the MCP Apps spec is six months old, why bet on an evolving standard?" The answer is that the spec is already adopted by five production hosts, the wire is JSON-RPC over postMessage (the simplest possible thing — there is nothing to bet on going wrong), and the AppBridge SDK is small enough that if it ever rots we can fork and maintain ~500 LoC ourselves. The cost ceiling of migration risk is "we ship a custom hostbridge fork in 18 months." The cost ceiling of not migrating is "every plugin author has to learn neige.* before they can ship." That asymmetry is the decision.

Proceed with M1 → M6 in the order listed. Start M1 + M5's frontend prep in parallel (no dependency); cut over hello-world in M6 only after all five precursor slices have landed and `cargo test -p calm-server -- plugin_` is green.
