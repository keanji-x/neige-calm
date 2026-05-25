# Sync Engine — Design Document

**Status:** Phases 1–3 shipped; #136 PR1–PR7b shipped (PR1 #141, PR2 #146, PR3 #162, PR4 #173, PR5 #179, PR6 #182, PR7a #202, PR7b #206). The originally-planned #136 PR8 (`wait_for_events` long-poll / pull) was superseded by the #293 push cutover: spec agents are driven by observations the kernel pushes onto their codex thread as turn inputs, and the pull machinery (`wait_for_events`, `/internal/codex/pending_events`, the Stop-hook long-poll) was deleted.
**Author:** Codex agent, on behalf of @keanji-x.
**Scope:** Backend-authoritative event-sourced sync engine layered on top of the existing axum + React stack. Decision against Electric / LiveStore / Zero is final; this doc covers only the in-house build.

This doc is the contract between the architectural decisions already locked in and the implementation that has shipped. Phases are independently shippable; each leaves `main` working.

---

## 1. Data model changes (server)

### 1.1 The `events` table

Introduced in `crates/calm-server/migrations/0004_events.sql` and extended by `0006_events_version.sql` (event-version stamp) and `0007_events_scope.sql` (per-row home scope). The current cumulative shape:

```sql
CREATE TABLE events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    kind          TEXT    NOT NULL,                       -- mirrors Event::serde tag, e.g. "wave.updated"
    payload       TEXT    NOT NULL,                       -- JSON, the `data` field of the wire envelope
    actor         TEXT    NOT NULL,                       -- JSON-encoded ActorId (see below)
    at            INTEGER NOT NULL,                       -- unix ms, matches model::now_ms()
    correlation   TEXT,                                   -- optional request id for tracing/replay grouping
    event_version INTEGER NOT NULL DEFAULT 1,             -- 0006: envelope schema version
    scope_kind    TEXT    NOT NULL DEFAULT 'system',      -- 0007: 'system' | 'cove' | 'wave' | 'card'
    scope_cove    TEXT,                                   -- 0007: populated for cove/wave/card scopes
    scope_wave    TEXT,                                   -- 0007: populated for wave/card scopes
    scope_card    TEXT                                    -- 0007: populated for card scope
);
CREATE INDEX idx_events_kind        ON events(kind);
CREATE INDEX idx_events_at          ON events(at);
CREATE INDEX idx_events_scope_wave  ON events(scope_wave) WHERE scope_wave IS NOT NULL;
CREATE INDEX idx_events_scope_cove  ON events(scope_cove) WHERE scope_cove IS NOT NULL;
```

Justifications:

- **`INTEGER PRIMARY KEY AUTOINCREMENT`** — SQLite reuses `rowid` after deletion without `AUTOINCREMENT`; the cursor protocol depends on strict monotonicity, so the small `sqlite_sequence` cost is worth it.
- **`payload TEXT`** — same convention as `cards.payload` and `overlays.payload`; avoids dependency on `jsonb` builds.
- **`actor` is JSON-encoded `ActorId`.** Not a flat string. See `crates/calm-server/src/ids.rs` — `ActorId` is `#[serde(tag = "kind", content = "id")]`, so a row's `actor` column reads `{"kind":"AiCodex","id":"card-7"}`, `{"kind":"User"}`, `{"kind":"Plugin","id":"hello-world"}`, etc. The tagged enum guarantees the wire shape and lets later PRs add variants without column churn.
- **`at`** vs `id` — wall-clock for humans (debug, audit), `id` for ordering and cursors. Never mix.
- **`correlation`** — optional; threads multi-step mutations (e.g. the 3-step terminal-card create) for replay tooling.
- **`event_version`** — sync envelope schema stamp (0006). Mirrored by the Rust constant `SYNC_EVENT_VERSION` in `event.rs`; bumped together on envelope-shape changes so replicas can refuse incompatible logs.
- **`scope_*`** — per-row "home scope" (0007). Lets PR3 filter authorization decisions by card scope and PR5's `SubscribeFilter` / `Dispatcher` route queues by wave scope (the #293 dispatcher push path reuses that same wave-scoped filter to deliver task/report events to a wave's spec card). Old rows backfill to `scope_kind = 'system'` with NULL ancestor cols; the WS-replay path treats NULL `scope_*` as `EventScope::System`.
- **No FK to entity tables.** Events outlive the rows they describe. Replay must handle "this card was deleted in event #5,300" gracefully.

**Actor is a declared field, not an authenticated identity.** The `actor` column records who the producer of an event claims to be. In the single-user local-host deployment, this is trust-based — the calling subsystem populates the typed `ActorId` correctly. If neige-calm ever opens an externally-reachable API or accepts remote AI agents, **a separate auth design must precede that exposure** — `actor` becomes a security boundary at that point, not just a debug field. Today it is the latter.

**Header plumbing (Scope G).** REST writes flow through `calm_server::actor::actor_middleware`, which reads the legacy `X-Calm-Actor` header and stamps an `Actor` extension on the request. Handlers extract `Actor` via `FromRequestParts` and the wrapper converts to the typed `ActorId` (`Actor::to_actor_id` in `actor.rs`). The string-level validation rules are unchanged:

- Header absent / empty → `ActorId::User`. Preserves today's no-header UX for the web frontend.
- `"user"` → `ActorId::User`.
- `"ai:codex"` → `ActorId::AiCodex(CardId)` (the card id is filled in by downstream code from the request context; the header alone carries the actor *kind*).
- `"kernel"` → **rejected with 400**. Reserved for kernel-internal writes (card-FSM projector, codex hook ingest, orphan terminal sweeper). Those sites bypass the middleware entirely and pass `ActorId::Kernel` directly into `write_with_event_typed`.
- `"plugin:<id>"` → **rejected with 400**. Reserved for the plugin callback dispatcher (`plugin_host::callbacks`), which builds `ActorId::Plugin("<id>")` from the connection context — plugins cannot spoof their own actor over either MCP or REST.
- Anything else → rejected with 400.

The middleware is layered on the REST router only; WebSocket endpoints (`/api/events`, `/api/terminals/:id`) are upgrade-style and do not write through the same path. Actor on WS frames is a separate (currently no-op) concern. The middleware is plumbing, not authentication — the §1.1 disclaimer above still applies: a real auth design must precede any externally-reachable surface before `actor` becomes a security boundary.

### 1.2 Existing `Event` enum: keep it, narrow its role

`crates/calm-server/src/event.rs:39` already defines a typed `Event` enum that ts-rs exports. **Keep it.** It stays the typed input to `event_append` (one place that knows the serde tag/content shape and topic mapping), the unit `EventBus` broadcasts, and the ts-rs source for `web/src/api/generated-events.ts`. The only lifecycle change: events are now **first persisted, then broadcast**, never broadcast-only (see §3). We do **not** merge `Event` into a free-form row body — that would lose ts-rs typing and re-open the schema-drift class of bugs that #5 closed.

### 1.3 No `version` column on entity tables

**Decision: global event id cursor, no per-row version.** The alternative — adding `version: INTEGER` to every entity table — costs 4+ migrations of churn for a benefit (resumable per-table snapshots) we don't need. Clients that fall off the event tail just re-`GET /api/waves/:id`; the path already exists. The optimistic-reconcile case in `web/src/api/queries.ts:181-222` is served by stamping `_id` on the WS envelope (see §2.4).

### 1.4 Where `event_append` lives in `Repo`

The **only** public path that writes events is `RepoEventWrite::write_with_event`. The raw `event_append` insert is private to the `SqlxRepo` impl (or `#[cfg(test)]`-gated for replay-loader / fixture-seeding use cases). The signature is the wrapper:

```text
async fn write_with_event<F, R>(
    &self,
    actor: &str,
    f: F,
) -> Result<(R, i64)>
where
    F: for<'tx> FnOnce(&'tx mut Transaction<'_, Sqlite>) -> BoxFuture<'tx, Result<(R, Event)>>;
```

It opens a transaction, runs the closure (entity statements), executes `INSERT INTO events ... RETURNING id` in the same txn, commits, then emits the `Event` on the bus stamped with the returned id:

```text
write_with_event_typed(repo, actor, None, &bus, |tx| async move {
    let card = card_create_tx(tx, p).await?;
    Ok((card.clone(), Event::CardAdded(card)))
})
```

Handlers stop calling `s.events.emit(...)` directly; the wrapper emits *after* commit succeeds. Partial-failure semantics: if either insert fails the txn rolls back; neither row exists.

**Rationale for not exposing the raw form.** Two parallel paths invite handlers to drift back to the raw form, bypassing the transaction guarantee. Tightening later is hard; opening up later (if a justified use case emerges) is trivial. The `card_fsm` projector at `crates/calm-server/src/card_fsm.rs` writes overlays — overlays ARE entity writes, so the FSM goes through `write_with_event` like every other writer.

#### 1.4.1 Trait capability split (Scope α)

After Scope α (PR #21), `Repo` is split into four sub-traits along the *capability* axis. The split converts the "no route handler reaches a raw sync-domain write" rule from grep-time discipline into a compile-time gate:

- **`RepoRead`** — universal read surface (`coves_list`, `wave_get`, `overlays_for`, `plugins_list_all`, `terminal_get`, `settings_get_all`, `plugin_kv_get`, …). Anyone with a `&dyn RepoRead` can read anything; no writes.
- **`RepoEventWrite: RepoRead`** — the audited write surface (`write_with_event`, `log_pure_event`, `events_since`, `events_earliest_id`).
- **`RepoSyncDomainRaw: RepoRead`** — **gated.** Raw entity writes for the in-scope sync domain: `cove_*`, `wave_*`, `card_*`, `overlay_upsert`, `overlay_delete`. These exist on the trait because `SqlxRepo` is the canonical impl and the types must be addressable somewhere — but the `RouteRepo` trait object route handlers see does **not** include this supertrait.
- **`RepoOutOfDomain: RepoRead`** — operational writes the kernel deliberately keeps off the sync engine: `terminal_*`, `plugin_*` (install/enable/config/KV/tokens), `settings_*`. These do **not** emit events; they are server-private state that no other peer needs to replicate. Routes see them — they are part of the normal REST surface for plugin install, settings PUT, etc.

`Repo` is the marker that combines all four (used internally and in tests). `RouteRepo` is `RepoEventWrite + RepoOutOfDomain` (transitively `RepoRead`); that's what `AppState::repo` exposes to handlers. Trait-object upcasting (Rust 1.86+) makes `Arc<dyn Repo>` → `Arc<dyn RouteRepo>` cheap and infallible at the trait-object boundary, which is what lets `AppState::new` hold a single concrete `SqlxRepo` and hand out the narrow view to handlers without a parallel struct.

A handler that types `s.repo.cove_create(...)` now fails to compile:

```text
error[E0599]: no method named `cove_create` found for struct
              `Arc<(dyn RouteRepo + 'static)>` in the current scope
   --> crates/calm-server/src/routes/coves.rs:NNN:NN
note: `RepoSyncDomainRaw` defines an item `cove_create`, perhaps you need
      to implement it
```

The error message is the gate: future contributors learn the rule from the type system, not from a comment.

**Sync-domain vs out-of-domain.** Sync-domain == the per-user / per-AI co-edit shared state defined by the engine: coves, waves, cards, overlays. Every write here is audit-logged and replicated. Out-of-domain == server-private operational state: terminal lifecycle (the daemon process is local), plugin install/config (per-instance), settings (per-instance). Adding these to the event log would be possible but not yet motivated — Phase 1 deliberately leaves them as plain repo writes.

**Escape hatch.** `AppState::raw_repo() -> &dyn Repo` is the deliberately-named accessor reserved for integration-test fixture seeding. **Currently used only by integration tests** (`tests/terminal_sweeper.rs`, `tests/plugin_routes.rs`, `tests/payload_validation.rs`); no production module reaches for `raw_repo()` — `terminal_sweeper.rs` and the `replay` lib funnel through `write_with_event_typed` / `log_pure_event` like everything else. To enforce this in the type system the method is **gated behind the `fixtures` cargo feature** (`crates/calm-server/Cargo.toml`): production builds (the binary, every `routes/*`, `plugin_host/*`, `terminal_sweeper`, the `replay` lib) compile without the feature and therefore physically cannot reach `raw_repo` — invoking it fails at compile time with `E0599: no method named raw_repo`. Integration tests get the feature automatically via a `[dev-dependencies]` self-loop (`calm-server = { path = ".", features = ["fixtures"] }`). If a future production module needs raw access, the access pattern is to extend the feature gate (and justify the case in code review), not to drop the gate. CI also runs a grep guard against `raw_repo` appearing in any production file as a soft backstop.

---

## 2. Wire protocol changes

### 2.1 Subscribe message

Today (`crates/calm-server/src/ws/events.rs:60`):

```json
{ "sub": ["wave:w-001", "plugin:*"] }
```

Extends to:

```json
{ "sub": ["wave:w-001", "plugin:*"], "since": 1729 }
```

`since` is optional. Server semantics:

- **Absent** — behave exactly as today (live broadcast only). Old clients keep working.
- **0** — replay from beginning. Useful for cold-start replay tests.
- **Number N** — replay all rows with `id > N` matching the topic filter, then transition to live.

Subscription is **replace-on-message**, same as today. A client can issue a fresh `{sub, since}` mid-connection to re-anchor; the server replays again.

### 2.2 Replay-then-live boundary (no drops, no dupes)

The naive approach (`SELECT * FROM events WHERE id > $since`, stream them, then start `bus.recv()`) has a race: an event may land between the SELECT and the `bus.subscribe()`. We avoid this with **subscribe-first-then-query**, the canonical lock-free pattern:

1. Client sends `{sub, since: N}`.
2. Server immediately calls `state.events.subscribe()` to grab a live receiver. Live events start *buffering* in the broadcast channel.
3. Server runs the SQL: `SELECT id, kind, payload FROM events WHERE id > N ORDER BY id ASC`.
4. Server streams each row to the client, tracking `last_replayed_id`.
5. Server then drains the live receiver. For each live event, if `event_id <= last_replayed_id`, drop it (dupe — already in the replay set); otherwise forward.
6. After drain, switch to normal forward loop.

This relies on `event_append` having committed the row **before** the broadcast emits, which is what the repo wrapper guarantees (commit-then-emit).

The broadcast channel capacity is currently 1024 (`event.rs:23`). Replay of a large historical window could exceed this — a slow client lagging in step 3–4 might see `Lagged(n)`. We treat that as "cursor too old; client must do a snapshot refetch" and close the connection. The client then re-issues `?since=<head>` plus a manual GET on the entities it cares about. This collapses to the existing recovery path.

**Edge case: client issues a mutation mid-replay.** A reconnecting client may fire a `POST /api/overlays` (or any other write) *before* its WS receives `_replay_complete`. The mutation goes through the REST handler normally → `write_with_event` → commit → broadcast. Two scenarios:

1. The new event's `id` is greater than `last_replayed_id` at the moment the broadcast arrives at the WS handler's drain step. It forwards normally. Client sees the event after some replay frames; `_id` ordering is preserved.
2. The new event lands *during* the replay SQL query window (between subscribe and SELECT completion). The subscribe-first ordering guarantees it's in the broadcast buffer; the drain step's `event_id <= last_replayed_id` dedup catches it correctly since the SELECT included it.

The client's `EventStream` does not need to gate writes on `_replay_complete`. Optimistic mutations work the same way during replay as during steady state. The cursor advances on each `_id`, so by the time `_replay_complete` arrives, `lastEventId` is past every replayed and any in-flight events.

### 2.3 Retention & snapshot-required fallback

**Retention is permanent today.** The events table grows monotonically and no pruner runs at startup. There is no `config.events_retention_days` field on the `Config` struct (`crates/calm-server/src/config.rs`) and `main.rs` spawns no nightly pruner task. The knob + pruner remain unimplemented; activation work is tracked by **issue #36**.

When a client's `since` predates the oldest surviving row, the server replies with a first-class control frame:

```json
{ "ev": "_snapshot_required", "data": { "earliest_id": 50000 } }
```

The client treats this as "throw away IndexedDB cache + refetch from REST". Today this can only fire when an operator manually deletes from the events table — there is no kernel-driven path that drops rows.

**Why forever is the right default.** Audit and replay are the architecture's headline value; defaulting to drop them inverts the value proposition. Storage cost is trivial for a single user: ~4 MB/year at 10k events/day. Premature pruning is a net negative; pruning policy can be added later when real storage pressure exists. Row-count rules of thumb: 10k events ≈ 2–4 MB; 100M events ≈ ~30 GB (replay scans get slow long before storage does). We won't approach the upper bound in single-user mode. Postgres later — see §8.

**Forward-looking: per-actor retention.** When plugins emit high-frequency status/progress events, a simple global retention knob may grow blunt. The natural future refinement is per-actor policy — e.g. `Plugin(_)` retained 30 days, `User` / `AiCodex(_)` forever. Out of scope for first version. See issue #36 for retention activation.

### 2.4 Event envelope on the wire

Today (event.rs:30):

```json
{ "ev": "wave.updated", "data": { ... } }
```

Becomes:

```json
{ "_id": 1729, "ev": "wave.updated", "data": { ... } }
```

`_id` (underscore prefix to keep it out of the `Event` payload namespace) is the value `event_append` returned. Required on all live and replayed frames. The frontend persists the highest `_id` seen as its cursor.

The `_snapshot_required` and a new `_replay_complete` synthetic envelope (`{ "_id": <last>, "ev": "_replay_complete" }`) are server-only frames that don't go through ts-rs (kept out of `Event` enum). The client treats them specially. This avoids polluting the Rust enum with control frames.

### 2.5 Backward compatibility

Clients that omit `since` get today's behavior verbatim. The added `_id` field is invisible to old builds (zod ignores unknowns by default), though we should surface it in the schema for new consumers. Reconnect logic is unchanged — the only diff is that the client now sends `since: lastSeen`, getting replay for free.

---

## 3. Write handler changes

### 3.1 The invariant

Every existing write handler in `crates/calm-server/src/routes/*.rs` currently does:

```text
let row = s.repo.x_create(p).await?;
s.events.emit(Event::XCreated(row.clone()));
```

(See `routes/cards.rs:140-145`, `routes/overlays.rs:75-77`, `routes/waves.rs:79-81` for the canonical shape.)

We replace this with a single repo-level wrapper:

```text
let (row, event_id) = s.repo.write_with_event(actor, |tx| async move {
    let row = card_create_tx(tx, p).await?;
    Ok((row.clone(), Event::CardAdded(row)))
}).await?;
// The wrapper has already emitted on the bus, stamped with event_id.
```

The handler's job collapses to: extract actor from request extensions, call `write_with_event`, return the row. After Phase 1, no `s.events.emit(...)` lines remain in `routes/*.rs` or `plugin_host/callbacks.rs` (§9) — `grep -r "events.emit" crates/calm-server/src/{routes,plugin_host}` returning zero hits is the lint that proves the conversion is complete.

### 3.2 Before/after, prose only

**Before** — `routes/overlays.rs::upsert_overlay`:

1. Validate payload (`validate_overlay_payload`).
2. Call `repo.overlay_upsert(p)` — this commits its own transaction internally.
3. On success, emit `Event::OverlaySet(overlay.clone())` to the in-memory bus.
4. Return JSON.

Failure mode: if step 3 didn't run (server crash between commit and emit), the row exists but subscribers never heard. Today this is invisible; with the event log it becomes a missed `events` row, which the cursor protocol would skip silently. Bad.

**After** — same handler, but step 2's commit and the `INSERT INTO events ...` happen in the same txn. The bus emit is the very last action *after* commit. Server crash between commit and emit: subscribers reconnect with their stale `since`, replay catches them up from the persisted events row. Idempotent.

### 3.3 Why one wrapper, not 20 edits

A middleware/extractor approach (axum middleware inspects an `Event` from a response extension) is tempting but wrong — it can't share the entity-write transaction. So we go with **mechanical per-handler conversion, all in one Phase 1 series**: the events-table migration, `write_with_event`, and every existing write handler convert together. The conversions are mechanical replacements of `repo.X(...) + emit(...)` with `write_with_event(actor, |tx| ...)`; splitting them across PRs is review convenience, not technical necessity.

There is **no transitional double-write**. The earlier idea of having `EventBus::emit` do a `tokio::spawn` persist for unconverted handlers is rejected: it violates the commit-then-emit invariant. A crash between broadcast and the spawned persist drops the event from the log while live subscribers already saw it, exactly the "row exists but the log doesn't" failure mode the design is meant to eliminate.

Suggested PR pacing within the Phase 1 series (review convenience only): `routes/overlays.rs` is the smallest (start there); `routes/cards.rs` is the biggest (3 entry points + the `via_tool_call` branch); `plugin_host/callbacks.rs` (§9) lands in the same series. All PRs in the series must merge before Phase 2 begins.

---

## 4. Client-side architecture

### 4.1 `useOverlayState` — the synced `useState`

**File:** `web/src/hooks/useOverlayState.ts`.

Signature:

```text
function useOverlayState<T extends JsonValue>(opts: {
  entity_kind: string;
  entity_id: string;
  kind: string;
  default: T;
  pluginId?: string;  // defaults to "kernel" for app-level state
}): [Persistent<T>, (next: T | ((prev: T) => T)) => void]
```

Internal flow:

1. `useQuery({ queryKey: ['overlay', plugin_id, entity_kind, entity_id, kind] })` — fetches via the existing `GET /api/overlays?entity_kind=...&entity_id=...` endpoint, filters to the requested `kind`. The `eventBridge` already invalidates this query family on `overlay.set` / `overlay.deleted` (see `web/src/app/eventBridge.tsx:179-199`); no new wiring.
2. `useMutation` wrapping `POST /api/overlays` (upsert). The setter calls this with optimistic update against the queryKey above and a rollback on error — same pattern as `useUpdateCoveMutation` (`web/src/api/queries.ts:181`).
3. Return tuple is `[brandedValue, setter]`. The branding lives only in TypeScript; runtime it's just `T`.

The setter accepts either a value or `(prev) => next` — true `useState` parity. Internally the functional form calls `qc.getQueryData` for prev, computes next, then mutates.

### 4.2 The `Persistent<T>` brand and `useState` shadowing

```text
type Persistent<T> = T & { readonly __persistent: unique symbol }
```

Two enforcement layers:

1. **Type-level.** We re-export `useState` (and `useReducer`) from `web/src/shared/state.ts` with a conditional-type guard:

   ```text
   function useState<T>(initial: T):
     T extends Persistent<unknown> ? never : [T, Dispatch<SetStateAction<T>>]
   ```

   Passing a `Persistent<T>` produces a `never` return — the call site fails to type-check ("Property '0' does not exist on type 'never'"). Not the prettiest error, but unambiguous in practice with the ESLint rule below.

2. **ESLint rule.** `no-persistent-in-usestate` — a custom rule under `web/eslint-rules/`. Inspects `CallExpression`s of `useState` and reports any whose type argument or argument expression resolves to `Persistent<_>`. Uses the TS type-checker (`@typescript-eslint/utils`). Emits a fix suggestion: "use `useOverlayState` instead". This is the human-readable layer; the type-error is the hard gate.

   Companion rule `no-direct-overlay-write-for-view-state` flags raw `POST /api/overlays` calls with `kind: "layout" | "filter" | ...` (the kernel-owned `view` kinds, registered in a small allowlist) and tells the dev to use `useOverlayState`.

3. **ESLint `no-restricted-imports` for `useState` / `useReducer`.** The type-level guard above only fires when developers actually import `useState` from `web/src/shared/state.ts`. A direct `import { useState } from 'react'` bypasses the shadow. Closing this gap is a standard `no-restricted-imports` config forbidding `useState` (and `useReducer` — same persistent-state shape, same problem) from `react` everywhere except `web/src/shared/state.ts` itself:

   ```text
   'no-restricted-imports': ['error', {
     paths: [{
       name: 'react',
       importNames: ['useState', 'useReducer'],
       message: "Import useState/useReducer from '@/shared/state' so the Persistent<T> guard applies.",
     }],
   }]
   ```

   Combined with an override that re-allows the raw import inside `web/src/shared/state.ts` only. With this rule in place, the type guard is unbypassable in normal code; without it, it's advisory at best.

### 4.3 `persistQueryClient` configuration

```text
import { persistQueryClient } from '@tanstack/react-query-persist-client';
import { createAsyncStoragePersister } from '@tanstack/query-async-storage-persister';
```

Setup in `web/src/app/providers.tsx`:

- **Persister.** IndexedDB via `idb-keyval` adapter (wrapping `createAsyncStoragePersister`). Single DB `neige-calm`, single store `query-cache`, key `tanstack-query-v1`. Namespace by user email *if* multi-account becomes a thing — for now single-user, no namespace.
- **Persisted queries.** Allowlist via `dehydrateOptions.shouldDehydrateQuery`. Persist: `['coves']`, `['waves', *]`, `['wave', *]`, `['overlays', *]`, `['overlay', *]`. Do **not** persist: ephemeral query keys, anything tagged `meta: { ephemeral: true }`.
- **Max age.** `maxAge: 7 * 24 * 60 * 60 * 1000` (7 days). After that the cache is dropped on rehydrate; offline-only users on stale caches just refetch on reconnect. Configurable per query via `staleTime`/`gcTime`, default `gcTime: Infinity` for view state.
- **Buster.** `buster: <semver of web build>` — bump on schema-breaking change.

### 4.4 `api/events.ts` cursor protocol

`web/src/api/events.ts:18` (`EventStream`) gains a `lastEventId: number | null`, persisted to `localStorage['calm:sync:cursor']` (batched via `requestIdleCallback` so busy streams don't thrash localStorage). On `connect()` open: send `{ sub, since: lastEventId }` if set, otherwise omit `since`. On every parsed frame: update `lastEventId` from `_id`. New listener `onSnapshotRequired` fires on the `_snapshot_required` control frame; default handler calls `queryClient.clear()` and triggers a reload.

### 4.5 `eventBridge.tsx` integration

`web/src/app/eventBridge.tsx:139-211` stays structurally identical — the cursor-aware reconnect is transparent inside `EventStream`. One addition: handle `_replay_complete` to drop any "reconnecting" UI banner and run a defensive batch invalidate (covers the edge where replay touched every key but optimistic mutations had left dirty entries).

---

## 5. First migration target — WaveGrid layout

Current code (`web/src/WaveGrid.tsx:34-65`) stores positions in `localStorage['calm:layout:<waveId>']`. Move this to an Overlay row.

### 5.1 Why first

- Small surface: 2 functions (`loadStored`, `saveStored`), 1 callback (`persistLayout`).
- Real value: layouts roam with the user across machines once we have sync.
- Exercises every layer: new overlay kind, validator, `useOverlayState` hook, cursor flow, optimistic update (drag-end), invalidation. Anything broken anywhere will show up.
- Acceptable failure mode: if sync is briefly down, the user just keeps editing on stale layout; on reconnect, server wins and they see the merge — usually identical.

### 5.2 Steps

1. **Validator.** Add `"layout"` to `validate_overlay_payload` in `crates/calm-server/src/validation.rs`. The `layout` overlay payload schema:

   ```
   {
     "positions": {
       "<card_id>": { "x": <u32>, "y": <u32>, "w": <u32>, "h": <u32> },
       ...
     }
   }
   ```

   Constraints, enforced in `validate_overlay_payload`:
   - `positions` is a JSON object (map), required.
   - Each key is a non-empty string (validated as card id format).
   - Each value has all four fields `x`, `y`, `w`, `h` as non-negative integers.
   - `x + w <= 12` (grid columns, see `web/src/WaveGrid.tsx::COLS`).
   - `w >= 1`, `h >= 1`.
   - Unknown keys in either the outer object or the position record reject (strict mode).

   The validator returns a typed `CalmError::BadRequest` with the offending field path, surfaced as HTTP 400 on the upsert. Frontend `useOverlayState` setter `onError` rolls back the optimistic value. Reject malformed at the write boundary, exactly like the existing `status`/`progress` cases.
2. **Migration helper.** First mount of `WaveGrid` for a given `waveId`: if no overlay row exists but `localStorage['calm:layout:<waveId>']` does, read it, POST an overlay with the parsed positions, then delete the localStorage key. Idempotent — every subsequent mount finds the overlay and skips.
3. **Component switch.** `WaveGrid` calls `useOverlayState({ entity_kind: 'view', entity_id: waveId, kind: 'layout', default: { positions: {} } })`. The `reconcile` function consumes `value.positions` instead of `loadStored(...)`. The `persistLayout` callback becomes `setLayout({ positions: ... })`.
4. **Fallback period.** Keep `loadStored`/`saveStored` as a local fallback path for one release (read-only): if the overlay query is in `pending` state (offline + empty cache), fall back to localStorage to avoid a layout flash. The setter never writes to localStorage in this state — it queues the mutation, which TanStack Query retries on reconnect. Drop the fallback in the next release.
5. **Cleanup.** Remove the localStorage code entirely. The migration helper from step 2 stays for one more release to catch upgrades-from-old, then also goes.

This is the template every subsequent migration follows: add validator → add migration helper → switch component → keep fallback for one release → drop.

---

## 6. Testing strategy

The developer cares specifically about this. We're going to use event sourcing as a first-class test affordance, not bolt it on.

### 6.1 Server-side tests

**Unit — `event_append` atomicity.** Existing `crates/calm-server/src/db/sqlite.rs` test harness already uses `sqlite::memory:` and runs migrations on open. Add a test that wraps `write_with_event` with a deliberately failing inner closure (returns `Err`) and asserts:
1. No new event row.
2. No new entity row.
3. The error bubbles unchanged.

A second test injects a faulty `INSERT INTO events` (e.g. force a SQL syntax error via a test-only override) and asserts the entity write rolls back. Coverage on the "event insert fails after entity insert" branch.

**Integration — replay correctness.** A new test harness in `crates/calm-server/tests/` that:
1. Builds a real `AppState` with `sqlite::memory:`.
2. Seeds a few entities via the REST handlers (so events land properly).
3. Opens a WS client (using `tokio-tungstenite`) with `?since=0`.
4. Asserts the received frames match the events table contents in order.

Variants: `since=<latest>` (zero replay rows, just the `_replay_complete` synthetic), `since=<middle>` (only newer events).

**Integration — replay-then-live boundary.** The crown jewel of correctness tests. Harness:

1. Connect a WS client with `since=N`.
2. Block on the first replay frame (test holds an in-process semaphore).
3. While the replay path is blocked, fire a *new* mutation via the REST handler. This event lands in the broadcast channel buffer.
4. Release the semaphore. Replay drains, then live drains.
5. Assert: the client received every event exactly once, in id order, no gaps.

This is the test that catches the lock-free ordering bug if we ever regress. Without it, the bug only appears under production load.

**Property test — `proptest` crate, already vendorable in workspace.** Generate a sequence of arbitrary writes (`enum WriteOp { CreateCove, UpdateWave(id, patch), DeleteCard(id), ... }`). For each generated sequence:
1. Run the sequence on a fresh server, then connect a client with `since=0`.
2. Run the same sequence with a continuously-connected subscriber from before any writes.
3. Assert the two clients converge to byte-identical state (canonicalized JSON).

The shrinking property guarantees minimal failing examples.

### 6.2 Client-side tests

**Unit — `useOverlayState` round-trip.** Vitest + RTL + mock fetch (`msw` already used elsewhere; check `web/src/api/queries.test.tsx` for the existing pattern):

1. Initial render returns `default`.
2. Setter invokes POST, optimistic value visible synchronously.
3. Server echoes overlay-set event via mock WS → cache reconciles, value matches server.
4. Mock POST rejects → optimistic rolls back to previous.
5. Setter as `(prev) => next` form sees correct `prev`.

**Type test — `Persistent<T>` rejection.** `expectTypeOf` from vitest. Companion file `web/src/hooks/useOverlayState.test-d.ts`:

```text
const p = {} as Persistent<{positions: Record<string, Pos>}>
expectTypeOf(() => useState(p)).toBeNever  // or .toBeCallable's return type
```

These run in CI as part of the vitest type-check pass. Drift in the brand definition fails here, not in a regression several days later.

**Component — full reconnect-replay.** Use `vitest-environment-jsdom` plus the existing fake WS pattern. Sequence:

1. Mount `<AppProviders><App /></AppProviders>` with the IndexedDB persister pointed at fake-indexeddb.
2. Drive a few mutations; assert UI state.
3. Drop the WS connection (mock fires `close`).
4. Inject synthetic events into the mock server side.
5. Re-open WS; client should send `since=<last>`; mock replies with replay frames.
6. Assert UI converges to the post-replay state.

This is the unit-scale dry run of the e2e test that lives in `web/e2e/`.

### 6.3 The crown jewel: replay-based regression tests

**Fixture format.** JSON files under `crates/calm-server/tests/fixtures/events/<name>.events.json`. `actor` is the JSON-encoded `ActorId` shape (`{"kind": ..., "id": ...}`):

```json
{
  "name": "bug-1234-card-rename-during-ai-move",
  "entities_seed": [ ... ],
  "events": [
    { "id": 1, "kind": "card.added",   "actor": {"kind": "User"},                                 "payload": { ... } },
    { "id": 2, "kind": "card.updated", "actor": {"kind": "AiCodex", "id": "card-7"},              "payload": { ... } },
    { "id": 3, "kind": "card.updated", "actor": {"kind": "User"},                                 "payload": { ... } },
    { "id": 4, "kind": "card.deleted", "actor": {"kind": "User"},                                 "payload": { ... } }
  ],
  "expected_state": { "cards": [], "overlays": [] }
}
```

**Loader.** Binary `crates/calm-server/src/bin/replay.rs` (shipped — scope γ, issue #31). `cargo run --bin replay -- --file <path>` boots an in-memory `calm-server` and raw-inserts the fixture's events via `Repo::log_pure_event` (seed is seed — no validation, no FSM). Exactly one of:

  * `--serve` — keep the full REST + WS router running on a fixed port (`--port`, default `4040`, matches the regular server) so a developer / Playwright session can poke the seeded state interactively. The REST entity-table reads will be empty (the loader bypasses write handlers by design); the WS `/api/events?since=0` replay returns the full seeded log, which is how `useOverlayState` and the rest of the frontend consume it.
  * `--assert` — verify the fixture's `expected` block (`last_event_kind`, `layout_positions`) by folding the persisted event log; exits 0 on match, non-zero on mismatch.

```text
$ cargo run --bin replay -- --file crates/calm-server/tests/fixtures/events/wave-grid-layout-trace.events.json --serve
calm-server (replay mode) listening on http://127.0.0.1:4040
  loaded 7 events from wave-grid-layout-trace.events.json
  last event: overlay.set at id=7

$ cargo run --bin replay -- --file crates/calm-server/tests/fixtures/events/wave-grid-layout-trace.events.json --assert
OK: 2/2 assertions matched (7 events seeded, last id=7, ...)
  ok: last_event_kind == overlay.set
  ok: layout_positions (3 entries) match
```

The boot + seed pipeline lives in `calm_server::replay` and is shared with the `tests/replay_fixtures.rs` integration test, so the test harness and the binary cannot drift.

**Recording.** `RECORD_SESSION=<path>` (env var honored by the regular `calm-server` `main.rs`) appends every bus-emitted event to `<path>` as line-delimited JSON in the fixture's per-event shape (`{"kind", "actor", "payload"}`). The loader (`load_fixture_from_path`) sniffs the first non-blank line to detect NDJSON (a `FixtureEvent` shape) vs. a curated fixture object, so a recorded file is replayable via `cargo run --bin replay -- --file <recorded.events.json> --assert` with no manual wrapping. Bug report = file + one `replay --assert` command. Bugs become reproducible artifacts — this is the headline capability. `BroadcastEnvelope` carries the producing `actor` from `write_with_event` / `log_pure_event` as a typed `ActorId`, so recorded traces preserve real attribution (`{"kind":"User"}` / `{"kind":"AiCodex","id":"<card>"}` / `{"kind":"Plugin","id":"<id>"}`) end-to-end (closed by issue #39).

### 6.4 Performance / load

**Microbench — `event_append` overhead.** `criterion` bench under `crates/calm-server/benches/event_append.rs`. Target: <50µs per call against `sqlite::memory:`, <500µs against on-disk. We're adding ~1 INSERT per existing write; this should be noise relative to the existing entity write. Bench compares baseline (current code) vs. `write_with_event` (post-change). Gate the PR on no regression worse than +20%.

**Replay throughput.** Synthetic harness: pre-populate 10k events, connect a client with `since=0`, measure time-to-`_replay_complete`. Target: <500ms over loopback. If we miss, the fix is server-side streaming in chunks (already the natural shape) plus a small `LIMIT 1000 OFFSET` pagination *during the replay phase* — bookkeeping the cursor between chunks.

**Storage growth.** Document the back-of-envelope numbers from §2.3 in the operator docs. Once retention activation (issue #36) lands, that becomes the safety valve.

---

## 7. Migration path / phases

Phases 1–3 shipped; the schema, the wrapper, the cursor protocol, and the first `useOverlayState` migration are all on main.

### Phase 1 — Schema + `write_with_event` + all handlers converted — shipped

Migration `0004_events.sql` plus the `Repo::write_with_event` wrapper. Every existing write handler in `routes/*.rs` and `plugin_host/callbacks.rs` (§9) converted in a single atomic series; no transitional double-write. `EventBus::emit` is an internal detail of the wrapper. `grep -r "events.emit" crates/calm-server/src/{routes,plugin_host}` returns zero hits — the lint that proved the conversion was complete.

### Phase 2 — WS `since` protocol + client cursor — shipped

`ws::events::handle` parses `since` and implements the subscribe-first-then-query pattern from §2.2. Client `EventStream` persists and sends `lastEventId`. `_replay_complete` and `_snapshot_required` synthetic frames handled. Visible benefit: a tab open across a server restart no longer misses events.

### Phase 3 — `useOverlayState` + `Persistent<T>` + WaveGrid migration — shipped

The hook, ESLint rule + brand type, `persistQueryClient` in `providers.tsx`, and WaveGrid migrated per §5. Layouts now sync across devices and survive reload with no flash.

### #136 (Wave-as-Actor) — PR1–PR7b shipped

The follow-on Wave-as-Actor series (#136) refined the typed write path on top of Phase 1's wrapper:

- **PR1** (#141) — typed ID newtypes `CoveId` / `WaveId` / `CardId` + the `ActorId` semantic enum (`crates/calm-server/src/ids.rs`).
- **PR2** (#146) — `EventScope` carried on every event row (migration `0007_events_scope.sql`); `write_with_event_typed` signature.
- **PR3** (#162) — `cards.role` (`plain` / `spec` / `worker`) + `enforce_role` gate + `CardRoleCache` (migration `0008_cards_role.sql`).
- **PR4** (#173) — four new `Event` variants for dispatcher + task lifecycle.
- **PR5** (#179) — `SubscribeFilter` + dispatcher worker + permit semaphore.
- **PR6** (#182) — atomic spec card on wave create + dispatcher daemon spawn on wave activate.
- **PR7a** (#202) — kernel-as-MCP-server infrastructure + 3 emit tools.
- **PR7b** (#206) — MCP `wave_state` tools.

The originally-planned **PR8** (`wait_for_events` long-poll / pull) was cancelled. The #293 push cutover replaced it: the kernel owns a `codex app-server` per spec card and the dispatcher pushes task/report observations onto the spec's thread as turn inputs — there is no pull tool, no `/internal/codex/pending_events`, and no Stop-hook long-poll.

### Phase 4+ — New persistent view state defaults to `useOverlayState`

The Phase 1-3 push shipped the full infrastructure: `useOverlayState`, `Persistent<T>` brand, ESLint rule, validation, replay, capability-split traits, actor-through-envelope. The first migration target (WaveGrid layout) is live (§5).

Earlier drafts of this doc enumerated a forecasted candidate list for Phase 4+ (cove ordering, sidebar collapse, card folding, per-wave filters, plugin settings). A May 2026 audit (PR-29 close-out) found that **most of those candidates don't actually exist as client-side state**:

- Cove ordering is a backend `Cove.sort` field — not view state.
- Sidebar collapse, card folding, per-wave filter — these UI features haven't been built. They're forward-looking guesses, not pending migrations.
- Plugin settings panels are already routed through `useSettingsQuery` / `useUpdateSettingsMutation` (TanStack Query), not raw `fetch`.
- Theme (light/dark) is `useState<'light' \| 'dark'>` in `CalmApp.tsx` and resets on reload; a separate design (issue #22) covers ThemeProvider + iframe sync + system-mode + Settings UI, which subsumes any naïve overlay migration.

The principle going forward, in lieu of a candidate list:

**Any new persistable client-side state defaults to `useOverlayState({ entity_kind: 'view', ... })`.** Opting back to `useState` for transient/ephemeral UI (modal-open, input drafts, hover) is fine. Opting to `localStorage` for new state requires a reason (sync-engine-internal cursors are the precedent). The ESLint + `Persistent<T>` brand make this enforceable at the type level the moment a `Persistent` value enters the picture.

Retention/pruning is **not implemented** today — see §2.3. Activation work (config knob + pruner task) is tracked by issue #36.

---

## 8. Open questions & resolved decisions

Still open — reviewer input most valuable on:

1. **Postgres now vs later.** Defer until multi-user lands or observable sqlite write contention. The repo trait is already Postgres-ready.
2. **Topic for `view` overlays.** A `view:<waveId>` topic is consistent with today's `<entity_kind>:<entity_id>` pattern. Confirm we don't also want a `view:*` firehose for debugging.

Decisions:

- **Retention default → forever, no pruner.** No config knob, no nightly pruner task. Audit/replay is the architecture's headline value; defaulting to drop it is wrong. Per-actor retention and the operator runbook for activating finite retention are tracked by issue #36.
- **Actor plumbing → axum middleware + typed `ActorId`.** Shipped. REST writes flow through `actor_middleware` which builds an `Actor` extension from the `X-Calm-Actor` header; the write wrapper converts to the typed `ActorId` enum (`crates/calm-server/src/ids.rs`). Plugins → callback dispatcher stamps `ActorId::Plugin("<id>")`. Kernel-internal writes (FSM, codex hook ingest) pass `ActorId::Kernel` directly. `kernel` and `plugin:*` are explicitly rejected from the header so REST callers cannot impersonate either. Actor remains a declared field, not authenticated identity (§1.1).
- **`event_append` visibility → wrapper only.** `Repo::write_with_event` is the sole public path; the raw insert is `SqlxRepo`-private (or `#[cfg(test)]`-gated for replay-loader / fixture use). Tightening later is hard; opening up later is trivial.
- **`view` entity_kind on Overlay → reuse Overlay table.** `entity_kind: "view"` rather than a new `view_state` table. Saves a concept; validators (§5.2) and the kernel-owned `view` kinds allowlist keep the namespace clean.

---

## 10. Terminal lifecycle cleanup (eager teardown + sweeper fallback)

Terminal rows carry three pieces of operational state: the row itself, a `calm-session-daemon` process, and a unix socket. All three must be torn down when the owning card / wave / cove is deleted. Cleanup happens in two layers.

### 10.1 Eager teardown — the happy path

The `terminals.card_id` foreign key is `ON DELETE RESTRICT` (migration 0011). The schema refuses any card delete that would orphan a terminal row, surfacing missed cleanup as a transaction-level FK error instead of a silent daemon-process leak.

Three route handlers (`routes/cards.rs::delete_card`, `routes/waves.rs::delete_wave`, `routes/coves.rs::delete_cove`) own the synchronous teardown:

1. **Enumerate** every card under the entity being deleted (`cards_by_wave`, or `waves_by_cove` + `cards_by_wave` for the cove path).
2. **Resolve the terminal row** (if any) via `terminal_get_by_card`.
3. **Reap the daemon + socket** via `terminal_sweeper::reap_terminal_artifacts` — graceful `ClientMsg::Kill` via socket (5 s timeout) → SIGTERM via persisted PID → `unlink(socket)`. Idempotent against missing artifacts.
4. **Drop the terminal row + the card/wave/cove row** in one `write_with_event_typed` transaction. The terminal-row delete is funneled through `terminal_delete_tx` and tolerates a `NotFound` race (a sweeper tick may have beaten us to it). The headline audit event is `Event::CardDeleted` / `WaveDeleted` / `CoveDeleted`; the terminal row delete rides under it without a separate event.

The plugin-host `card_delete` callback (`plugin_host/callbacks.rs`) follows the same pattern even though plugin-deletable kinds (`ui://*`) don't carry terminals in practice — keeping the FK invariant inviolable across every write site.

### 10.2 Sweeper — the crash-recovery fallback

`terminal_sweeper` is a tokio task spawned at server start (modeled after `card_fsm::spawn`). It exists for the residual shape: a crashed server, a SIGKILL'd writer, or a partial-success transaction that left a terminal row whose `card_id` no longer matches any `cards.payload.terminal_id`. It is **not** the cleanup path for the user-initiated delete happy case — that's eager teardown.

**Orphan definition**: a `terminals` row whose `id` is not referenced by any `cards.payload.terminal_id`, AND whose `created_at` is older than a grace window (default: 1 minute). The grace window absorbs the 3-step terminal-card creation race (POST card → POST terminal → PATCH card.payload — `eventBridge.tsx:60-70`).

**Tick cadence**: every 30 s. For each orphan: send `ClientMsg::Kill` via unix socket, fall back to SIGTERM after a 5 s grace, remove the socket file, then `write_with_event(actor=Kernel, ...)` to delete the terminal row and emit `Event::TerminalDeleted`. The audit event distinguishes the sweeper's crash-recovery path from the eager handler path (the latter emits `CardDeleted` / `WaveDeleted` / `CoveDeleted`, never `TerminalDeleted`).

**Schema support**: the `pid INTEGER` column added in migration 0005 gives the SIGTERM fallback a target.

### 10.3 Why two layers

Eager teardown owns the happy path because it gives the user (a) a synchronous "your terminal is gone" guarantee at the end of the HTTP request and (b) a coherent audit trail — the `CardDeleted` event is emitted only after the daemon is gone. The sweeper exists to ensure no terminal lingers indefinitely if a writer is killed mid-teardown; without it a crash between "row delete committed" and "daemon killed" would leak the process forever (the FK CASCADE used to nominally clean up the row before this design — but as issue #197 made clear, the sweeper couldn't catch what the FK had already nuked, so the daemon leaked anyway). Eager teardown + RESTRICT closes that gap; the sweeper handles the still-narrower remaining gap (writer crash *after* the row is created but *before* it's linked back via `cards.payload.terminal_id`).

**Out of scope**: a user-initiated `DELETE /api/terminals/:id` endpoint. There's no current product need — terminals are dependents of cards, not first-class user entities.

---

## 9. Plugin compatibility

Plugin write callbacks (`crates/calm-server/src/plugin_host/callbacks.rs`) follow the exact same `permission check → validate → repo write → event_bus.emit` shape as REST handlers (see `callbacks.rs:188-413` for the canonical examples on `overlay_set`, `card_create`, `card_update`, `card_delete`, `overlay_delete`). The sync engine changes apply identically:

1. **`write_with_event` covers plugin writes.** The Phase 1 atomic conversion (§3.3, §7) must include `plugin_host/callbacks.rs` alongside `routes/*.rs`. Same mechanical replacement: `repo.X(...) + emit(...)` becomes `write_with_event(actor, |tx| ...)`. Missing this means plugin writes broadcast live but never persist, breaking audit and replay for the most prolific writers in the system.

2. **Actor for plugins: `ActorId::Plugin("<plugin_id>")`.** Populated by the callback dispatcher (`plugin_host::dispatch_neige_callback`), not by the plugin process — plugins cannot spoof their own actor. The typed enum carries the plugin id; the `correlation` field handles cases that need finer-grained tracing.

3. **Tool-call writes (`routes::plugins::tool_call`) — actor is the plugin, correlation is the user trigger.** When a frontend iframe invokes `app.callServerTool({ name: "neige.overlay.set", ... })`, the resulting event's actor is still `ActorId::Plugin("<id>")` (the entity making the kernel write), but the `correlation` field records `"user_tool_call:<call_id>"` to distinguish user-triggered plugin writes from autonomous plugin writes. Audit queries can join on correlation to reconstruct the user-driven causal chain.

4. **Plugin event subscriptions (`neige.event.subscribe`) require no cursor changes.** Plugins are long-lived processes whose lifecycle is bound to the kernel: when calm-server restarts, plugin processes restart with it, getting a fresh subscription. Plugin disconnection across kernel restarts is not a use case. The `since` parameter on the WS protocol is browser-tab-replay; plugins continue to receive live broadcasts only.

5. **Plugin-defined overlay kinds remain opaque pass-through.** The `validation.rs::validate_overlay_payload` function applies kernel-owned validation only; plugin-defined kinds are accepted as-is, as today. Sync engine does not change this semantics.

6. **Per-actor retention as a future concern.** Plugins emitting frequent progress / status overlays (potentially many per second per plugin) will dominate event volume. The forever-retention default (§2.3) holds today, but once retention activation (issue #36) lands a natural follow-up is per-actor retention policy: e.g. `Plugin(_)` retained 30 days, `User`/`AiCodex(_)` forever.

7. **Permission system unchanged.** `write_with_event` is a transaction wrapper; the permission check at the callback boundary (`perms.can_overlay_write(...)` etc., `callbacks.rs:197`) happens before the wrapper is called and is unaffected. Plugins still cannot write under `plugin_id: "kernel"` or violate their declared grants.

8. **Headline net win: built-in plugin audit log.** `SELECT * FROM events WHERE json_extract(actor, '$.kind') = 'Plugin' ORDER BY at DESC` is a complete time-travel record of every plugin write. With persistence, plugin writes are queryable, replayable, and AI-readable. This alone justifies the design for plugin-heavy users.

---

*End of design.*
