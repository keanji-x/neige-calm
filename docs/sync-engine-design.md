# Sync Engine — Design Document

**Status:** Draft v2 — incorporates reviewer feedback. No code yet.
**Author:** Codex agent, on behalf of @keanji-x.
**Scope:** Extend the existing axum + React stack with a backend-authoritative event-sourced sync engine. Decision against Electric / LiveStore / Zero is final; this doc covers only the in-house build.

This doc is the contract between the architectural decisions already locked in (see prompt) and the implementation that follows. Phases are independently shippable; each leaves `main` working.

### Changelog — v2

- **Phase 1 is now atomic (no transitional double-write).** The events-table migration, `write_with_event` wrapper, and every existing write handler convert in a single PR series; the `tokio::spawn` persist path in `EventBus::emit` is removed. The "one route file per PR" idea is recast as review-process pacing within the single Phase 1 milestone. (§3.3, §7)
- **Retention default flipped to forever.** `config.events_retention_days = None`; pruner only runs when an operator configures a finite window. Per-actor retention noted as a forward-looking refinement, not first-version. (§2.3, §8)
- **`event_append` is private.** Only `write_with_event` is public on `Repo`; the raw insert is `SqlxRepo`-private (or `#[cfg(test)]`-gated for replay-loader / fixture use). `card_fsm` projector writes overlays through the wrapper like everyone else. (§1.4, §8)
- **ESLint `no-restricted-imports` for `useState`/`useReducer`** — added to §4.2 to make the shadowed `useState` shim unbypassable.
- **Actor is declarative, not authenticated.** Disclaimer added at §1.1; a separate auth design is required before any externally-reachable surface ships.
- **Concrete `layout` overlay payload schema** with grid-column bound from `WaveGrid.tsx::COLS`. (§5.2)
- **In-flight client mutation during replay window** edge case documented. (§2.2)
- **New §9 — Plugin compatibility.** `write_with_event` applies to `plugin_host/callbacks.rs` identically; tool-call writes carry `actor = "plugin:<id>"` with `correlation = "user_tool_call:<call_id>"`; plugin-defined overlay kinds remain opaque pass-through.
- **Open questions trimmed.** Q2/Q3/Q4/Q5 resolved (now "Decisions (was open in v1)"); Q1 (Postgres) and Q6 (firehose topic) remain.

---

## 1. Data model changes (server)

### 1.1 The `events` table

A single new table, added in a new migration file `crates/calm-server/migrations/0004_events.sql`.

```sql
CREATE TABLE events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT    NOT NULL,            -- mirrors Event::serde tag, e.g. "wave.updated"
    payload     TEXT    NOT NULL,            -- JSON, the `data` field of the wire envelope
    actor       TEXT    NOT NULL,            -- "user", "ai:<agent_id>", "kernel", "plugin:<id>"
    at          INTEGER NOT NULL,            -- unix ms, matches model::now_ms()
    correlation TEXT                          -- optional request id for tracing/replay grouping
);
CREATE INDEX idx_events_kind ON events(kind);
CREATE INDEX idx_events_at   ON events(at);
```

Justifications:

- **`INTEGER PRIMARY KEY AUTOINCREMENT`** — SQLite reuses `rowid` after deletion without `AUTOINCREMENT`; the cursor protocol depends on strict monotonicity, so the small `sqlite_sequence` cost is worth it.
- **`payload TEXT`** — same convention as `cards.payload` and `overlays.payload`; avoids dependency on `jsonb` builds.
- **`actor`** — string, not enum: plugin and AI actors carry sub-ids. Validated at the handler boundary.
- **`at`** vs `id` — wall-clock for humans (debug, audit), `id` for ordering and cursors. Never mix.
- **`correlation`** — optional; threads multi-step mutations (e.g. the 3-step terminal-card create called out in `web/src/app/eventBridge.tsx:61-70`) for replay tooling.
- **No FK to entity tables.** Events outlive the rows they describe. Replay must handle "this card was deleted in event #5,300" gracefully.

**Actor is a declared field, not an authenticated identity.** The `actor` column records who the producer of an event claims to be (`"user"`, `"ai:codex"`, `"plugin:<id>"`, `"kernel"`). In the single-user local-host deployment, this is trust-based — the calling subsystem populates it correctly. If neige-calm ever opens an externally-reachable API or accepts remote AI agents, **a separate auth design must precede that exposure** — `actor` becomes a security boundary at that point, not just a debug field. Today it is the latter.

**Header plumbing (Scope G).** REST writes flow through an axum middleware (`calm_server::actor::actor_middleware`) that reads `X-Calm-Actor` from the incoming request and stamps an `Actor` extension on the request before the handler runs. Handlers extract `Actor` via a `FromRequestParts` impl and pass it straight to `write_with_event_typed`. Validation rules:

- Header absent / empty → defaults to `"user"`. Preserves today's no-header UX for the web frontend.
- `"user"` → accepted verbatim.
- `"ai:<id>"` where `<id>` matches `[a-z0-9-]{1,64}` → accepted verbatim.
- `"kernel"` → **rejected with 400**. Reserved for kernel-internal writes (card-FSM projector, codex hook ingest, orphan terminal sweeper). Those sites bypass the middleware entirely and call `write_with_event_typed` with `"kernel"` directly.
- `"plugin:<id>"` → **rejected with 400**. Reserved for the plugin callback dispatcher (`plugin_host::callbacks`), which stamps the kernel-known plugin id from the connection context (`format!("plugin:{}", ctx.plugin_id)`) — plugins cannot spoof their own actor over either MCP or REST.
- Anything else → rejected with 400.

The middleware is layered on the REST router only; WebSocket endpoints (`/api/events`, `/api/terminals/:id`) are upgrade-style and do not write through the same path. Actor on WS frames is a separate (currently no-op) concern. The middleware is plumbing, not authentication — the §1.1 disclaimer above still applies: a real auth design must precede any externally-reachable surface before `actor` becomes a security boundary.

### 1.2 Existing `Event` enum: keep it, narrow its role

`crates/calm-server/src/event.rs:39` already defines a typed `Event` enum that ts-rs exports. **Keep it.** It stays the typed input to `event_append` (one place that knows the serde tag/content shape and topic mapping), the unit `EventBus` broadcasts, and the ts-rs source for `web/src/api/generated-events.ts`. The only lifecycle change: events are now **first persisted, then broadcast**, never broadcast-only (see §3). We do **not** merge `Event` into a free-form row body — that would lose ts-rs typing and re-open the schema-drift class of bugs that #5 closed.

### 1.3 No `version` column on entity tables

**Decision: global event id cursor, no per-row version.** The alternative — adding `version: INTEGER` to every entity table — costs 4+ migrations of churn for a benefit (resumable per-table snapshots) we don't need. Clients that fall off the event tail just re-`GET /api/waves/:id`; the path already exists. The optimistic-reconcile case in `web/src/api/queries.ts:181-222` is served by stamping `_id` on the WS envelope (see §2.4).

### 1.4 Where `event_append` lives in `Repo`

The **only** public path that writes events is `Repo::write_with_event`. The raw `event_append` insert is private to the `SqlxRepo` impl (or `#[cfg(test)]`-gated for replay-loader / fixture-seeding use cases). The signature on `Repo` is the wrapper:

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
repo.write_with_event(actor, |tx| async move {
    let card = card_create_tx(tx, p).await?;
    Ok((card.clone(), Event::CardAdded(card)))
})
```

Handlers stop calling `s.events.emit(...)` directly; the wrapper emits *after* commit succeeds. Partial-failure semantics: if either insert fails the txn rolls back; neither row exists.

**Rationale for not exposing the raw form.** Two parallel paths invite handlers to drift back to the raw form, bypassing the transaction guarantee. Tightening later is hard; opening up later (if a justified use case emerges) is trivial. The `card_fsm` projector at `crates/calm-server/src/card_fsm.rs` writes overlays — overlays ARE entity writes, so the FSM goes through `write_with_event` like every other writer.

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

The events table grows monotonically, but the **default is to keep events forever**. `config.events_retention_days = None`; the nightly pruner is wired in phase 1 but only runs when an operator sets a finite value. When pruning runs, it does `DELETE FROM events WHERE at < ?`. When a client's `since` predates the oldest surviving row, the server replies with a first-class control frame:

```json
{ "ev": "_snapshot_required", "data": { "earliest_id": 50000 } }
```

The client treats this as "throw away IndexedDB cache + refetch from REST".

**Why forever is the right default.** Audit and replay are the architecture's headline value; defaulting to drop them inverts the value proposition. Storage cost is trivial for a single user: ~4 MB/year at 10k events/day. Premature pruning is a net negative; pruning policy can be added later when real storage pressure exists. Row-count rules of thumb: 10k events ≈ 2–4 MB; 100M events ≈ ~30 GB (replay scans get slow long before storage does). We won't approach the upper bound in single-user mode. Postgres later — see §8.

**Forward-looking: per-actor retention.** When plugins emit high-frequency status/progress events, the simple global retention knob may grow blunt. A future refinement is per-actor policy — e.g. `plugin:*` retained 30 days, `user` / `ai:*` forever. Out of scope for first version, but it is the natural escape valve before reaching for global pruning.

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

**Fixture format.** JSON files under `crates/calm-server/tests/fixtures/events/<name>.events.json`:

```json
{
  "name": "bug-1234-card-rename-during-ai-move",
  "entities_seed": [ ... ],
  "events": [
    { "id": 1, "kind": "card.added",   "actor": "user",     "payload": { ... } },
    { "id": 2, "kind": "card.updated", "actor": "ai:codex", "payload": { ... } },
    { "id": 3, "kind": "card.updated", "actor": "user",     "payload": { ... } },
    { "id": 4, "kind": "card.deleted", "actor": "user",     "payload": { ... } }
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

**Recording.** `RECORD_SESSION=<path>` (env var honored by the regular `calm-server` `main.rs`) appends every bus-emitted event to `<path>` as line-delimited JSON in the fixture's per-event shape (`{"kind", "actor", "payload"}`). Bug report = file + one `replay --assert` command. Bugs become reproducible artifacts — this is the headline capability. Caveat: the broadcast envelope does not yet carry actor (the wrapper that knows actor — `write_with_event` / `log_pure_event` — does, but `BroadcastEnvelope` doesn't surface it), so recorded events land with `actor: "unknown"`; replaying the trace still produces the same event-log shape. Threading actor through the envelope is a follow-up issue.

### 6.4 Performance / load

**Microbench — `event_append` overhead.** `criterion` bench under `crates/calm-server/benches/event_append.rs`. Target: <50µs per call against `sqlite::memory:`, <500µs against on-disk. We're adding ~1 INSERT per existing write; this should be noise relative to the existing entity write. Bench compares baseline (current code) vs. `write_with_event` (post-change). Gate the PR on no regression worse than +20%.

**Replay throughput.** Synthetic harness: pre-populate 10k events, connect a client with `since=0`, measure time-to-`_replay_complete`. Target: <500ms over loopback. If we miss, the fix is server-side streaming in chunks (already the natural shape) plus a small `LIMIT 1000 OFFSET` pagination *during the replay phase* — bookkeeping the cursor between chunks.

**Storage growth.** Document the back-of-envelope numbers from §2.3 in the operator docs. The nightly compaction task is the safety valve.

---

## 7. Migration path / phases

Each phase ships independently, each merges cleanly, each leaves the app working.

### Phase 1 — Schema + `write_with_event` + all handlers converted (atomic, no frontend changes)

Migration `0004_events.sql`. New `Repo::write_with_event` (raw `event_append` stays private to `SqlxRepo`, see §1.4). **Every existing write handler converts in the same PR series** — `routes/*.rs` and `plugin_host/callbacks.rs` (§9). No transitional double-write; no `tokio::spawn` persist in `EventBus::emit`. `EventBus::emit` is now an internal detail of `write_with_event` rather than a public mutation path. After Phase 1, `grep -r "events.emit" crates/calm-server/src/{routes,plugin_host}` returns zero hits — that's the lint that proves the conversion is complete.

Existing tests pass; add §6.1 atomicity tests and the §6.4 microbench gate. Frontend untouched. Worst-case overhead: ~20µs per write. PR pacing inside the series is review convenience (§3.3), but all PRs in the series must merge before Phase 2 begins — partial conversion plus the absence of a transitional path would mean unconverted handlers crash the persistence invariant.

### Phase 2 — WS `since` protocol + client cursor

`ws::events::handle` parses `since` and implements the subscribe-first-then-query pattern from §2.2. Client `EventStream` persists and sends `lastEventId`. `_replay_complete` and `_snapshot_required` synthetic frames handled. Visible benefit: a tab open across a server restart no longer misses events. New tests: §6.1's replay integration, §6.2's reconnect-replay component test.

### Phase 3 — `useOverlayState` + `Persistent<T>` + WaveGrid migration

New hook file, ESLint rule + brand type, `persistQueryClient` in `providers.tsx`, WaveGrid migrated per §5. New tests: §6.2's round-trip, type-test, fixture-based replay tests for the layout overlay. First visible feature: layouts sync across devices and survive reload with no flash.

### Phase 4+ — Incremental migrations

Order by friction-to-value: (1) Cove ordering prefs (sidebar `useState`), (2) per-Cove filter/search state, (3) card-local UI state (collapse, scroll), (4) plugin settings panels (today raw fetch — convert to `useOverlayState`). The retention pruner cron is wired in Phase 1 but stays inert under the forever default (§2.3); no phase turns it on — an operator turns it on by setting `config.events_retention_days` to a finite value.

---

## 8. Open questions & resolved decisions

Still open — reviewer input most valuable on:

1. **Postgres now vs later.** Defer until multi-user lands or observable sqlite write contention. The repo trait is already Postgres-ready.
2. **Topic for `view` overlays.** A `view:<waveId>` topic is consistent with today's `<entity_kind>:<entity_id>` pattern. Confirm we don't also want a `view:*` firehose for debugging.

Decisions (was open in v1):

- **Retention default → forever.** `config.events_retention_days = None`; pruner only runs when an operator configures a finite window. Audit/replay is the architecture's headline value; defaulting to drop it is wrong. Per-actor retention is a forward-looking refinement (§2.3, §9.6). *Was v1 Q2.*
- **Actor plumbing → axum middleware + typed request extension.** Shipped in Scope G. User → `"user"`. AI agents → `X-Calm-Actor: ai:<id>` header, validated against the `[a-z0-9-]{1,64}` id format. Plugins → existing hashed-token path; the callback dispatcher (`plugin_host::callbacks::dispatch`) stamps `"plugin:<id>"` — the plugin process cannot spoof its own actor. Kernel-internal writes (FSM, codex hook ingest) pass `"kernel"` directly without going through the middleware. `kernel` and `plugin:*` are explicitly rejected from the header so REST callers cannot impersonate either. Actor remains a declared field, not authenticated identity (§1.1); a separate auth design precedes any externally-reachable surface. *Was v1 Q3.*
- **`event_append` visibility → wrapper only.** `Repo::write_with_event` is the sole public path; the raw insert is `SqlxRepo`-private (or `#[cfg(test)]`-gated for replay-loader / fixture use). Tightening later is hard; opening up later is trivial. *Was v1 Q4.*
- **`view` entity_kind on Overlay → reuse Overlay table.** `entity_kind: "view"` rather than a new `view_state` table. Saves a concept; validators (§5.2) and the kernel-owned `view` kinds allowlist keep the namespace clean. *Was v1 Q5.*

---

## 10. Orphan terminal cleanup

Terminal rows (and their associated `calm-session-daemon` processes + unix sockets) currently leak when a terminal card is deleted: `routes/cards.rs::card_delete` removes only the card row, leaving the terminal entity and its daemon process alive forever. Scope C closes this with a sweeper that walks for orphans and reaps them via the same `write_with_event` pipeline so the cleanup is audited.

**Orphan definition**: a `terminals` row whose `id` is not referenced by any `cards.payload.terminal_id`, AND whose `created_at` is older than a grace window (default: 1 minute). The grace window absorbs the 3-step terminal-card creation race (POST card → POST terminal → PATCH card.payload — `eventBridge.tsx:60-70`).

**Sweeper**: a tokio task spawned at server start (`main.rs`, modeled after `card_fsm::spawn`). Ticks every 30s. For each orphan: send `ClientMsg::Kill` via unix socket, fall back to SIGTERM after a 5s grace, remove socket file, then `write_with_event(actor="kernel", ...)` to delete the terminal row and emit `Event::TerminalDeleted`.

**Schema change**: add `pid INTEGER` column to `terminals` table so the SIGTERM fallback has a target.

**Why through `write_with_event`**: cleanup events show up in the audit log (`SELECT * FROM events WHERE kind='terminal.deleted' AND actor='kernel'`), and any UI subscribed to terminal events sees them disappear cleanly.

**Out-of-scope**: not handling user-initiated terminal deletion (today that endpoint doesn't exist; sweeper only catches orphans from deleted cards). If a future explicit "delete terminal" endpoint lands, it goes through `write_with_event` like any other write.

---

## 9. Plugin compatibility

Plugin write callbacks (`crates/calm-server/src/plugin_host/callbacks.rs`) follow the exact same `permission check → validate → repo write → event_bus.emit` shape as REST handlers (see `callbacks.rs:188-413` for the canonical examples on `overlay_set`, `card_create`, `card_update`, `card_delete`, `overlay_delete`). The sync engine changes apply identically:

1. **`write_with_event` covers plugin writes.** The Phase 1 atomic conversion (§3.3, §7) must include `plugin_host/callbacks.rs` alongside `routes/*.rs`. Same mechanical replacement: `repo.X(...) + emit(...)` becomes `write_with_event(actor, |tx| ...)`. Missing this means plugin writes broadcast live but never persist, breaking audit and replay for the most prolific writers in the system.

2. **Actor format for plugins: `"plugin:<plugin_id>"`.** Populated by the callback dispatcher (`plugin_host::dispatch_neige_callback`), not by the plugin process — plugins cannot spoof their own actor. First version uses the plugin id only; the `correlation` field handles cases that need finer-grained tracing.

3. **Tool-call writes (`routes::plugins::tool_call`) — actor is the plugin, correlation is the user trigger.** When a frontend iframe invokes `app.callServerTool({ name: "neige.overlay.set", ... })`, the resulting event's actor is still `"plugin:<id>"` (the entity making the kernel write), but the `correlation` field records `"user_tool_call:<call_id>"` to distinguish user-triggered plugin writes from autonomous plugin writes. Audit queries can join on correlation to reconstruct the user-driven causal chain.

4. **Plugin event subscriptions (`neige.event.subscribe`) require no cursor changes.** Plugins are long-lived processes whose lifecycle is bound to the kernel: when calm-server restarts, plugin processes restart with it, getting a fresh subscription. Plugin disconnection across kernel restarts is not a use case. The `since` parameter on the WS protocol is browser-tab-replay; plugins continue to receive live broadcasts only.

5. **Plugin-defined overlay kinds remain opaque pass-through.** The `validation.rs::validate_overlay_payload` function applies kernel-owned validation only; plugin-defined kinds are accepted as-is, as today. Sync engine does not change this semantics.

6. **Per-actor retention as a future concern.** Plugins emitting frequent progress / status overlays (potentially many per second per plugin) will dominate event volume. The forever-retention default (§2.3) holds for first version, but a future enhancement may be per-actor retention policy: e.g. `plugin:*` retained 30 days, `user`/`ai:*` forever. Out of scope for first ship.

7. **Permission system unchanged.** `write_with_event` is a transaction wrapper; the permission check at the callback boundary (`perms.can_overlay_write(...)` etc., `callbacks.rs:197`) happens before the wrapper is called and is unaffected. Plugins still cannot write under `plugin_id: "kernel"` or violate their declared grants.

8. **Headline net win: built-in plugin audit log.** `SELECT * FROM events WHERE actor LIKE 'plugin:%' ORDER BY at DESC` becomes a complete time-travel record of every plugin write. Today plugin writes broadcast and vanish; with persistence, they're queryable, replayable, and AI-readable. This alone justifies the design for plugin-heavy users.

---

*End of design.*
