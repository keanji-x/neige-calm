# Terminology Glossary

Canonical names for cross-cutting concepts in neige-calm. The codebase is
mostly internally consistent already; this glossary exists to (a) pin the
canonical name for each cluster, (b) call out the handful of dual-use and
weakly-named terms so new PRs don't reinvent vocabulary, and (c) document
what is deliberately *not* being renamed and why.

Closes #201. Companion to `docs/sync-engine-design.md` (write path /
authorization design) and `docs/upgrade-stability.md` (Tier A/B persistence
contracts).

> **Scope.** Definitions only — no code rename mandate. The issue explicitly
> defers mechanical renames unless they obviously reduce ambiguity. Entries
> marked **"weak name (kept)"** are acknowledged technical debt.

---

## Actor identity

| Symbol | Where | Meaning |
|---|---|---|
| `Actor(pub String)` | `crates/calm-server/src/actor.rs` | Declared identity attached to a request. Populated by the axum middleware that reads `X-Calm-Actor`. **Not authenticated** — it's a declared field, see `actor.rs` module-level doc and `docs/sync-engine-design.md` §1.1. |
| `X-Calm-Actor` header | `crates/calm-server/src/actor.rs` (`Actor::HEADER`) | Wire surface for the actor. Accepted forms: `user` (the default when the header is absent) and `ai:<id>` where `<id>` matches `[a-z0-9-]{1,64}`. `kernel` and `plugin:*` are rejected from the header — those are reserved for server-internal writes. |
| `ActorId` | `crates/calm-server/src/ids.rs` | The typed, semantically-tagged identity. Variants: `User`, `Kernel`, `KernelDispatcher`, `Plugin(String)`, `AiSpec(CardId)`, `AiCodex(CardId)`. Serialized as `{"kind": "...", "id": ...}` (JSON-tagged) and persisted into the `events.actor` TEXT column. |
| persisted actor | `events.actor` (TEXT column) | The JSON-encoded `ActorId` for every event row. Audit-log truth. |
| `Actor::to_actor_id()` | `crates/calm-server/src/actor.rs` | Maps a header-derived string to an `ActorId`. Defensive default: anything the middleware didn't already reject collapses to `ActorId::User`. |

The string `Actor` and the typed `ActorId` coexist by design: `Actor` rides
through the HTTP stack as plumbing; `ActorId` is what the role gate and the
event log reason about.

---

## CardRole vs "Wave-as-Actor"

| Symbol | Where | Meaning |
|---|---|---|
| `CardRole` | `crates/calm-server/src/model.rs` | Canonical type. Three variants: `Plain` (default — no extra restrictions), `Spec` (the wave's spec card — only spec cards may emit `WaveUpdated`), `Worker` (dispatcher-spawned; events scoped to the card itself). Persisted lowercase in `cards.role` (migration 0008). |
| `cards.role` | DB column | Lowercase serialization of `CardRole`. |
| role gate | `crates/calm-server/src/role_gate.rs` | The single authorization choke point, invoked inside the write transaction by `Repo::write_with_event`. Denies events that violate per-role scope rules. |
| `SeededCardRole` | `crates/calm-server/src/spec_card.rs` (`pub(crate)`) | Narrower internal variant carved out of `CardRole` — only `Spec` and `Worker`, the two roles whose `$CODEX_HOME` seeding helper needs a system-prompt template. `Plain` cards never reach the seeding helper, so the absence of a `Plain` arm is structural. |
| "Wave-as-Actor" | design-initiative name (#136) | The **PR series** that introduced typed `ActorId` + role gate + dispatcher + spec card. **Not a type.** Comments like `// Wave-as-Actor PR3 (#136): ...` mean "this code landed as PR3 of the Wave-as-Actor series." If you see prose that treats "Wave-as-Actor" as a thing rather than a PR-series label, that's a doc bug. |

**Spec card vs AI agent (informal).** "Spec card" is shorthand for a `Card`
row with `role = CardRole::Spec`. There is intentionally **no** `SpecCard` or
`SpecAgent` type — the spec card is just a card. "AI agent" / "spec agent" is
informal shorthand for the transient codex / terminal session the dispatcher
spawns in response to that card's `TaskRequested` event; it's a process, not
a persisted entity.

---

## Worker role vs worker job kind

| Symbol | Where | Meaning |
|---|---|---|
| `CardRole::Worker` | `crates/calm-server/src/model.rs` | The persistent card role for dispatcher-spawned worker cards. |
| `payload.role_request` | dispatcher-stamped bookkeeping field, e.g. `crates/calm-server/src/dispatcher.rs` ~line 568, 868 | Internal selector telling the dispatcher *which daemon* to spawn for this worker card. Values today: `"codex"` (a codex session), `"terminal"` (a plain PTY). Lives on the worker card's `payload`. |

**Weak name (kept).** `role_request` is acknowledged as weakly named — it
collides conceptually with `CardRole` even though it's a different axis
(*job kind* / *daemon-to-spawn*, not authorization). Renaming would require
touching every dispatcher write site and the worker-card payload migrators;
defer to a future payload refactor. Treat new code as if the canonical name
were `worker_kind` / `job_kind` and the current field is a legacy spelling.

---

## Dispatcher (two distinct kinds, neither split into a typed pair)

| Symbol | Where | Meaning |
|---|---|---|
| `Dispatcher` | `crates/calm-server/src/dispatcher.rs` | The **job dispatcher**. A struct + spawned task that subscribes to task/report/hook/plan/wave events, pushes observations to the spec harness, and pokes the plan scheduler. Worker spawns now flow through scheduler-owned `task.dispatched` operations. Acts as `ActorId::KernelDispatcher`. |
| plugin callback dispatcher | `crates/calm-server/src/plugin_host/callbacks.rs` (`dispatch()`) | Module-level function, **not** a struct. Resolves each plugin-originated `neige.*` request against the kernel (permission check → repo write → event emit → respond). Identity is injected from `CallbackCtx`, never trusted from the plugin params. |

**Not split into a typed pair.** The issue floated `JobDispatcher` /
`PluginCallbackDispatcher` as a possible rename. Skipped: the two live in
different modules with non-overlapping APIs (one is a long-lived task struct,
the other is a stateless dispatch function), the existing names disambiguate
by module path, and the rename would churn every call site without
materially clarifying anything. If you reach for "the dispatcher" in a new
comment, qualify with "job dispatcher" or "plugin callback dispatcher."

---

## EventScope (dual-purpose by design)

| Symbol | Where | Meaning |
|---|---|---|
| `EventScope` | `crates/calm-server/src/event.rs` | Enum with `System`, `Cove`, `Wave`, `Card` variants (the cove/wave/card variants carry the full ancestor chain for filter ergonomics). |

`EventScope` intentionally serves **two roles**:

1. **Persistence "home scope"** stamped on every event row at write time
   (`events.scope_kind` / `scope_cove` / `scope_wave` / `scope_card`).
   `EventScope::from_row(...)` rehydrates it on the replay path.
2. **Subscription / filter scope** — `SubscribeFilter` + `SubscribeScope`
   reuse the same enum to express "this subscriber wants events whose home
   scope falls under cove X / wave Y / card Z."

The dual use is deliberate: stamping at write time means filtering at
subscribe time is a cheap column comparison instead of a payload re-parse.
Don't introduce a parallel "filter scope" enum.

`EventScope::System` is the catch-all for cross-entity events (e.g.
`PluginState`, the `CoveCreated` case where the cove doesn't yet exist, and
NULL-on-replay fallbacks). Pick `System` only when you've ruled out the
narrower scopes — a `System`-tagged event opts out of every per-scope filter.

---

## Version constants — three distinct boundaries

These three live in two crates and answer three different questions; do not
conflate them.

| Constant | Where | Boundary |
|---|---|---|
| `FRAME_VERSION` | `crates/calm-session/src/lib.rs` (currently `2`) | Terminal wire-frame envelope format. Bumped when the on-wire payload format changes incompatibly. A reader seeing an unexpected version closes via `FrameError::UnsupportedFrameVersion`. |
| `PROTOCOL_VERSION` | `crates/calm-session/src/lib.rs` (currently `2`) | Application-layer terminal protocol carried in `ClientHello` / `ServerHello`. Distinct from `FRAME_VERSION` because the wire envelope and the payload schema can move independently; today they happen to be in lockstep at 2/2. |
| `SYNC_EVENT_VERSION` | `crates/calm-server/src/event.rs` (currently `1`) | Sync-engine event envelope schema. Stamped on every `BroadcastEnvelope` (fresh and replay), persisted in the `events.event_version` column, surfaced on the wire as `eventVersion` and via `GET /api/version` as `syncEventVersion`. |

**UI text.** Per the issue: UI should display the expected/supported version
dynamically rather than hardcode `"v2"`. Today `web/src/XtermView.tsx`
(~line 552) renders the literal `"(refresh required for protocol v2)"`
because the local `ProtocolError` interface doesn't yet capture the
`expected_version: number | null` field the wire type carries. Threading it
through is more than a one-line change (interface + capture site + display +
test fixture in `XtermView.test.tsx`); left for a future tidy-up.

---

## Version fields on payloads / wire — three distinct boundaries

| Field | Where | Boundary |
|---|---|---|
| `schemaVersion` | per-card-kind / per-overlay-kind payload constants in `crates/calm-server/src/validation.rs` (e.g. `TERMINAL_PAYLOAD_SCHEMA_VERSION`, `OVERLAY_STATUS_SCHEMA_VERSION`) | Per-kind payload schema. Tier A per `docs/upgrade-stability.md`. Validator rejects future versions on write; the read-side guard filters future versions out of `/api/overlays` and `/api/events`. Plugin-defined kinds are opaque (no version policy). |
| `eventVersion` | wire field on `BroadcastEnvelope`; mirrors `SYNC_EVENT_VERSION` | Per-frame event envelope schema. See the row above. |
| `WEB_COMPAT_VERSION` / `minWebCompatVersion` | `crates/calm-server/src/routes/version.rs` and `web/src/api/version.ts` | Frontend bundle ↔ backend REST/WS contract. Monotonically increasing. The two constants must stay in lockstep — `ServerCompatGate` hard-refreshes a frontend whose `WEB_COMPAT_VERSION` is below the server's `minWebCompatVersion`. |

When you add a new compatibility boundary, pick a fourth name — do not
overload one of these.

---

## Plugin card kind

| Form | Where | Status |
|---|---|---|
| `ui://<plugin>/<view>` | parsed at `crates/calm-server/src/plugin_host/resources.rs`; consumed by `web/src/api/adapt.ts` and the registry in `web/src/cards/registry.ts` | **Canonical.** New code emits and accepts only this form. |
| `plugin:<id>:<view>` | server-side perms / manifest still gate on this prefix (`crates/calm-server/src/plugin_host/perms.rs`, `manifest.rs`); legacy fixtures / persisted rows still carry it | **Legacy.** The web side hard-cut accept of this form in M4 (`web/src/api/adapt.ts:174-176`); see `docs/m3-mcp-apps-migration.md` for the migration plan. Treat occurrences in `[legacy]`-marked comments as historical context. |

When you add a comment that mentions the legacy form, mark it `[legacy]`
(e.g. `// [legacy] plugin:<id>:<view> kind`) so a future grep stays
self-explanatory.

---

## Terminal owner / observer (no rename needed)

The owner / observer vocabulary in `calm-session` is already consistent: the
first successful handshake on a freshly-spawned daemon becomes
`Role::Owner`; subsequent clients default to `Role::Observer` and can
self-promote via `ClientMsg::OwnerClaim` (hostile takeover — the daemon
never negotiates). The single subtlety worth remembering: **kernel-originated
input is allowed from an observer connection** (the kernel acts on behalf of
the user even when a browser tab holds owner). That exception is the only
reason "observer cannot write input" is not literally true.

---

## Quick lookup — "I want to refer to..."

| Concept | Use this name |
|---|---|
| who emitted an event (typed) | `ActorId` |
| who emitted an event (request plumbing) | `Actor` |
| HTTP header carrying the declared actor | `X-Calm-Actor` |
| card's persisted authorization label | `CardRole` |
| the design-initiative that introduced the above | "Wave-as-Actor" series (#136) |
| `Card` row with `role = Spec` | "spec card" (informal — no dedicated type) |
| transient codex/terminal session spawned for a spec card | "AI agent" / "spec agent" (informal — a process) |
| job-spawning subscriber | "job dispatcher" → `Dispatcher` struct |
| plugin `neige.*` request handler | "plugin callback dispatcher" → `plugin_host::callbacks::dispatch` |
| event home scope / subscribe scope | `EventScope` (dual-use is intentional) |
| terminal wire envelope version | `FRAME_VERSION` |
| terminal application protocol version | `PROTOCOL_VERSION` |
| sync-event envelope schema version | `SYNC_EVENT_VERSION` / wire `eventVersion` |
| per-payload schema version | `schemaVersion` |
| frontend ↔ backend bundle compatibility | `WEB_COMPAT_VERSION` / wire `minWebCompatVersion` |
| canonical plugin card kind URI | `ui://<plugin>/<view>` |
| legacy plugin card kind | `plugin:<id>:<view>` (mark `[legacy]` in new comments) |
