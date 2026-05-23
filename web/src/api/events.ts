// WebSocket subscription manager for /api/events.
//
// Auto-reconnects with exponential backoff. The subscription set is sticky —
// changing it (via `subscribe(topics)`) re-publishes to the server and is
// remembered across reconnects.
//
// Each incoming frame is runtime-validated through `wireEventSchema` (zod)
// from `./schemas`. A malformed frame logs a warning and is dropped — the
// stream itself keeps going. This guards against schema drift from new
// kernel versions while keeping the JSON.parse / dispatch path the only
// existing failure mode for listeners.
//
// ## Sync engine phase 2 (Scope D) — cursor protocol
//
// The server stamps every wire envelope with `_id: <events.id>`. We track
// the largest id we've ever seen in `lastEventId` and persist it to
// `localStorage['calm:sync:cursor']` so a tab open across reconnects /
// restarts can ask the server to **replay** any events missed during the
// downtime.
//
// ## Per-frame eventVersion gate (issue #198, concern 2)
//
// Every wire envelope also carries `eventVersion: <n>` (set by the server
// to the `events.event_version` column for replayed rows, or to
// `SYNC_EVENT_VERSION` for fresh writes / control frames). The client
// compares each frame's `eventVersion` against the server's declared
// `syncEventVersion` from `/api/version`, which is set on the stream via
// `setSyncEventVersion()` before WS subscribe.
//
// A frame whose `eventVersion` exceeds the client-known `syncEventVersion`
// is from a future protocol the running frontend wasn't compiled to
// understand. Such frames are LOGGED, DROPPED, AND THE REPLAY CURSOR IS
// NOT ADVANCED — so a later, compatible frontend reconnecting under the
// same cursor will receive the frame again. (Contrast with the malformed-
// payload path below, where we DO advance the cursor: there the frame is
// shaped wrong for *us*, not from-the-future, and re-replaying it on the
// next reconnect would just pin the cursor forever.)
//
// On every open, we send `{ sub: [...], since: lastEventId }` when a
// cursor is set; otherwise `{ sub: [...] }` (pre-Scope-D behavior, kept
// for cold-start clients). The server streams missed events, then sends
// a `_replay_complete` control frame to mark the boundary.
//
// Two server-only control frames are handled here, before the regular
// zod parse runs — they're not part of the `Event` union and would fail
// `wireEventSchema`:
//
//   * `_replay_complete` — fire `replayCompleteListeners` so the bridge
//     can run a defensive batch `qc.invalidateQueries()`. We DO advance
//     `lastEventId` from this frame's `_id` (it's stamped with the
//     server's tip cursor), so a subsequent reconnect resumes from the
//     right place even if zero replay rows matched.
//   * `_snapshot_required` — the cursor is older than what the server
//     can still serve (retention pruner kicked in). Clear `lastEventId`,
//     fire `snapshotRequiredListeners` so the bridge can `qc.clear()`,
//     and bounce the socket — the next reconnect comes up cold.

import type { WireEvent } from './wire';
import { fireUnauthorized } from './onUnauthorized';
import { wireEventSchema } from './schemas';

/**
 * Per-frame envelope metadata the server stamps onto every broadcast.
 *
 *   * `id` — `events.id` of the persisted row this broadcast came from
 *     (Scope D's cursor protocol). `0` is reserved for synthetic emits
 *     and never produced by the auto-increment, so it's safe to use as a
 *     "no persisted row" sentinel if a listener needs to discriminate
 *     (today, only the cursor advancer does — see `advanceCursor`).
 *   * `eventVersion` — value of the `event_version` column on the
 *     persisted row (migration 0006). Pre-sync rows backfill to `1`;
 *     fresh writes carry `SYNC_EVENT_VERSION`. Exposed here so test
 *     traces can pin the wire-protocol contract they captured against.
 *
 * Option α (two-arg callback) was picked over Option β (widen `WireEvent`)
 * because the validated `WireEvent` type is generated from the Rust
 * `Event` enum via ts-rs — that's a closed payload schema that doesn't
 * include envelope-level routing fields. Stuffing `_id` / `eventVersion`
 * into it would either fork the generated type or require a wrapper
 * shape; keeping the meta as a separate argument lets callers ignore it
 * when they don't care (most do today) without touching the event
 * shape's semantics.
 */
export interface EventMeta {
  id: number;
  eventVersion: number;
}

type Listener = (ev: WireEvent, meta: EventMeta) => void;
type ReplayCompleteListener = () => void;
type SnapshotRequiredListener = () => void;

/**
 * Observable connection state for the `/api/events` WebSocket.
 *
 *   * `disconnected` — `close()` was called explicitly (e.g. component
 *     unmount, or before the very first `start()`). Subscribers can use
 *     this to hide the UI indicator entirely; nothing is being attempted.
 *   * `connecting` — initial connection in progress, OR the socket dropped
 *     and we're in the exponential-backoff retry loop. The user-visible
 *     UX is the same either way ("not live; working on it") so they're
 *     collapsed into one state.
 *   * `connected` — the `_replay_complete` control frame has arrived,
 *     meaning we've finished consuming any historical replay from `since`
 *     and are now streaming live. NOTE: WS `'open'` alone is NOT enough —
 *     between open and `_replay_complete`, the server may still be
 *     catching us up on missed events, and emitting `connected` then
 *     would tell the user "you're live" while they actually aren't.
 */
export type ConnectionState = 'connecting' | 'connected' | 'disconnected';
type ConnectionStateListener = (state: ConnectionState) => void;

/** Storage key for the persisted cursor. Single namespace because we're
 *  single-user; if multi-account ever lands, namespace by user email. */
const CURSOR_STORAGE_KEY = 'calm:sync:cursor';

/** Server-only control frames — design doc §2.4 and ws::events::handle.
 *  These don't go through `wireEventSchema` (they're not in the `Event`
 *  union); the EventStream extracts them by name before the regular parse. */
const REPLAY_COMPLETE_EV = '_replay_complete';
const SNAPSHOT_REQUIRED_EV = '_snapshot_required';

/** `requestIdleCallback` is widely supported but missing in jsdom (test
 *  environment) and Safari versions still in the field. `setTimeout(0)`
 *  is the standard fallback and good enough for "batch a localStorage
 *  write so it doesn't happen on every WS frame." */
type IdleScheduler = (cb: () => void) => void;
const scheduleIdle: IdleScheduler =
  typeof globalThis !== 'undefined' &&
  typeof (globalThis as { requestIdleCallback?: unknown }).requestIdleCallback === 'function'
    ? (cb) =>
        (globalThis as { requestIdleCallback: (cb: () => void) => void }).requestIdleCallback(cb)
    : (cb) => setTimeout(cb, 0);

export class EventStream {
  private url: string;
  private ws: WebSocket | null = null;
  private listeners = new Set<Listener>();
  private replayCompleteListeners = new Set<ReplayCompleteListener>();
  private snapshotRequiredListeners = new Set<SnapshotRequiredListener>();
  private connectionStateListeners = new Set<ConnectionStateListener>();
  private connectionState: ConnectionState = 'disconnected';
  private topics = new Set<string>();
  private closed = false;
  private backoffMs = 500;
  private readonly maxBackoff = 8000;
  /** Highest `_id` observed across every frame on every connection. Sent
   *  back to the server on the next `{sub, since}` as the resume cursor.
   *  `null` means "fresh client; no replay" — equivalent to the pre-Scope-D
   *  message shape. */
  private lastEventId: number | null;
  /** True while a localStorage flush is queued via `scheduleIdle`. Avoids
   *  stacking one flush per frame on busy streams. */
  private cursorFlushQueued = false;
  /** Maximum `eventVersion` the server has declared via `/api/version`
   *  (its `syncEventVersion` field). Set by the caller once the version
   *  query resolves, BEFORE any WS subscribe runs — see `EventBridge`.
   *  `null` means "version not yet known" → no per-frame gating (the
   *  stream tolerates this for the bootstrap window, but in practice the
   *  bridge sets this before invoking `subscribe`). A frame with
   *  `eventVersion > syncEventVersion` is dropped without advancing the
   *  cursor. See module docstring §"Per-frame eventVersion gate". */
  private syncEventVersion: number | null = null;

  constructor(url = wsUrl('/api/events')) {
    this.url = url;
    this.lastEventId = loadCursor();
  }

  /** Replace the topic set. Sends `{sub:[...]}` immediately if connected. */
  subscribe(topics: Iterable<string>): void {
    this.topics = new Set(topics);
    this.publishSub();
  }

  addTopic(t: string): void {
    if (!this.topics.has(t)) {
      this.topics.add(t);
      this.publishSub();
    }
  }

  removeTopic(t: string): void {
    if (this.topics.delete(t)) this.publishSub();
  }

  on(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Register a callback for `_replay_complete` control frames. Returns
   *  an unsubscribe function. Used by the eventBridge to run a defensive
   *  batch invalidate at the replay→live boundary. */
  onReplayComplete(fn: ReplayCompleteListener): () => void {
    this.replayCompleteListeners.add(fn);
    return () => this.replayCompleteListeners.delete(fn);
  }

  /** Register a callback for `_snapshot_required` control frames. Used by
   *  the eventBridge to `qc.clear()` the persisted cache. */
  onSnapshotRequired(fn: SnapshotRequiredListener): () => void {
    this.snapshotRequiredListeners.add(fn);
    return () => this.snapshotRequiredListeners.delete(fn);
  }

  /** Subscribe to connection-state transitions. Fires the current state
   *  synchronously on subscribe so React `useSyncExternalStore` consumers
   *  get a non-`undefined` snapshot on first render. Returns an
   *  unsubscribe function. */
  onConnectionState(fn: ConnectionStateListener): () => void {
    this.connectionStateListeners.add(fn);
    fn(this.connectionState);
    return () => this.connectionStateListeners.delete(fn);
  }

  /** Current connection state. Exposed for `useSyncExternalStore`'s
   *  getSnapshot and for tests. */
  get state(): ConnectionState {
    return this.connectionState;
  }

  private setConnectionState(next: ConnectionState): void {
    if (next === this.connectionState) return;
    this.connectionState = next;
    for (const fn of this.connectionStateListeners) fn(next);
  }

  /** Current cursor — `null` for a fresh client, or the largest `_id`
   *  observed. Exposed primarily for tests. */
  get cursor(): number | null {
    return this.lastEventId;
  }

  /** Server-declared maximum `eventVersion` (from `/api/version`). Once
   *  set, the stream drops any frame whose envelope `eventVersion`
   *  exceeds this value, WITHOUT advancing the cursor. Idempotent — the
   *  caller (the EventBridge effect) sets it on every mount; a no-op
   *  call with the same value is fine. Pass `null` to clear (currently
   *  unused; reserved for tests). */
  setSyncEventVersion(version: number | null): void {
    if (version === null) {
      this.syncEventVersion = null;
      return;
    }
    if (!Number.isFinite(version) || version < 0) return;
    this.syncEventVersion = version;
  }

  /** Current server-declared max `eventVersion`, or `null` if not yet set.
   *  Exposed for tests. */
  get serverSyncEventVersion(): number | null {
    return this.syncEventVersion;
  }

  start(): void {
    if (this.ws || this.closed) return;
    this.setConnectionState('connecting');
    this.connect();
  }

  close(): void {
    this.closed = true;
    this.ws?.close();
    this.ws = null;
    this.setConnectionState('disconnected');
  }

  private connect(): void {
    // Cover the reconnect-after-backoff path: `connect()` is also called
    // from the close handler's `setTimeout`, where the state already
    // moved to `connecting` — but make it idempotent so the initial
    // `start()` path and any future entry point stays consistent.
    this.setConnectionState('connecting');
    const ws = new WebSocket(this.url);
    this.ws = ws;
    // Issue #189 — track whether the upgrade succeeded so the close
    // handler can discriminate "auth/handshake rejected" (close fires
    // without `open` ever firing) from "server dropped a live socket".
    // The former probes whoami so a 401-cookie can surface the
    // unauthorized flow; the latter just continues the backoff loop.
    let opened = false;
    ws.addEventListener('open', () => {
      opened = true;
      this.backoffMs = 500;
      this.publishSub();
      // NOTE: don't transition to `connected` here. WS-open just means
      // the socket handshake succeeded; the server may still stream
      // replay frames before we're "live". `_replay_complete` is the
      // authoritative signal — handled in `handleFrame`.
    });
    ws.addEventListener('message', (m) => {
      const raw = typeof m.data === 'string' ? m.data : '';
      let json: unknown;
      try {
        json = JSON.parse(raw);
      } catch {
        return;
      }
      this.handleFrame(json, raw);
    });
    ws.addEventListener('close', () => {
      this.ws = null;
      if (!this.closed) {
        // We're going back into the backoff loop. Surface `connecting`
        // immediately so any UI indicator can show "reconnecting" while
        // we wait, rather than appearing stale-but-connected.
        this.setConnectionState('connecting');
        if (!opened) {
          // Upgrade never completed — could be transient network or a
          // 401-rejected handshake (axum's middleware turns an
          // unauthenticated upgrade into a plain HTTP 401 rather than a
          // ws frame, which the browser surfaces here as `close` without
          // a prior `open`). Probe whoami: if it returns 401 the
          // unauthorized listener (SessionProvider) takes over and
          // closes the stream; if it returns 200 we just keep retrying.
          probeUnauthorized();
        }
        setTimeout(() => this.connect(), this.backoffMs);
        this.backoffMs = Math.min(this.backoffMs * 2, this.maxBackoff);
      }
    });
    ws.addEventListener('error', () => {
      // close handler does the work
    });
  }

  /** Dispatch one parsed envelope. Pulls control frames off first
   *  (they're not in the typed `Event` union); everything else goes
   *  through `wireEventSchema` validation and then fan-out to listeners. */
  private handleFrame(json: unknown, raw: string): void {
    if (typeof json !== 'object' || json === null) {
      // eslint-disable-next-line no-console
      console.warn('event bus: non-object payload', raw);
      return;
    }
    const envelope = json as { _id?: unknown; eventVersion?: unknown; ev?: unknown };

    // ---- control frames first -----------------------------------------
    if (envelope.ev === REPLAY_COMPLETE_EV) {
      this.advanceCursor(envelope._id);
      // Authoritative "we're live" signal — replay window is done and
      // we're now streaming current events. See the ConnectionState
      // doc comment for why this is preferred over `'open'`.
      this.setConnectionState('connected');
      for (const fn of this.replayCompleteListeners) fn();
      return;
    }
    if (envelope.ev === SNAPSHOT_REQUIRED_EV) {
      // Cursor is past the retention horizon. Drop everything and let
      // the bridge clear the React Query cache. The server will close
      // the socket; the auto-reconnect path picks it up with a `null`
      // cursor next time around.
      this.clearCursor();
      for (const fn of this.snapshotRequiredListeners) fn();
      return;
    }

    // ---- normal wire event -------------------------------------------
    // Issue #198, concern 2: gate cursor advance on `eventVersion`.
    //
    // A frame from a future protocol (eventVersion above what /api/version
    // declared via `syncEventVersion`) must NOT advance the cursor — a
    // future, compatible frontend reconnecting under the same cursor needs
    // to receive that frame again. We log, drop, and bail BEFORE
    // `advanceCursor`. Tolerant of a missing/non-numeric `eventVersion`
    // (treat as "version unknown, don't gate") so legacy / synthetic frames
    // continue to work; and tolerant of `syncEventVersion === null` (the
    // version query hasn't resolved yet) — the EventBridge sets the value
    // before subscribe, but this is the defensive path.
    const envEventVersion = envelope.eventVersion;
    if (
      this.syncEventVersion !== null &&
      typeof envEventVersion === 'number' &&
      Number.isFinite(envEventVersion) &&
      envEventVersion > this.syncEventVersion
    ) {
      // eslint-disable-next-line no-console
      console.warn(
        `event bus: dropping future-protocol frame (eventVersion=${envEventVersion} > syncEventVersion=${this.syncEventVersion}); cursor not advanced`,
        raw,
      );
      return;
    }

    // Advance cursor BEFORE zod parse for any in-range frame, so a
    // payload that's malformed for *us* (but not from-the-future) still
    // moves the cursor forward — otherwise a single bad frame could pin
    // the cursor and force a re-replay on every reconnect.
    this.advanceCursor(envelope._id);

    const result = wireEventSchema.safeParse(json);
    if (!result.success) {
      // eslint-disable-next-line no-console
      console.warn('event bus: unknown payload', raw, result.error);
      return;
    }
    const parsed: WireEvent = result.data as WireEvent;
    // Envelope meta: `_id` and `eventVersion` aren't in the discriminated-
    // union schema (they live on the routing envelope, not the kernel
    // event payload). Tolerate missing / wrong-typed values gracefully —
    // synthetic emissions can omit them, and a malformed envelope still
    // gets a `0` so the trace shape stays uniform for test helpers.
    const meta: EventMeta = {
      id: typeof envelope._id === 'number' && Number.isFinite(envelope._id) ? envelope._id : 0,
      eventVersion:
        typeof envelope.eventVersion === 'number' && Number.isFinite(envelope.eventVersion)
          ? envelope.eventVersion
          : 0,
    };
    for (const fn of this.listeners) fn(parsed, meta);
  }

  /** Maybe-advance `lastEventId` from a wire `_id`. Tolerates missing,
   *  wrong-typed, or zero `_id` values — those come from synthetic /
   *  test bus emissions and aren't real cursor positions.
   *
   *  Batches the localStorage write through `scheduleIdle` so a busy
   *  stream doesn't pay the localStorage cost on every frame. */
  private advanceCursor(rawId: unknown): void {
    if (typeof rawId !== 'number' || !Number.isFinite(rawId) || rawId <= 0) return;
    if (this.lastEventId !== null && rawId <= this.lastEventId) return;
    this.lastEventId = rawId;
    this.queueCursorFlush();
  }

  private clearCursor(): void {
    this.lastEventId = null;
    try {
      localStorage.removeItem(CURSOR_STORAGE_KEY);
    } catch {
      // localStorage can throw in private mode / SSR; silent fallback.
    }
  }

  private queueCursorFlush(): void {
    if (this.cursorFlushQueued) return;
    this.cursorFlushQueued = true;
    scheduleIdle(() => {
      this.cursorFlushQueued = false;
      const value = this.lastEventId;
      if (value === null) return;
      try {
        localStorage.setItem(CURSOR_STORAGE_KEY, String(value));
      } catch {
        // localStorage can throw on quota / private mode; cursor stays
        // in-memory only for this session.
      }
    });
  }

  private publishSub(): void {
    if (this.ws?.readyState !== WebSocket.OPEN) return;
    // Always send `since`: fresh clients (no cursor) request a full replay
    // with `since: 0` so fixture-seeded events emitted before WS connect
    // still reach the trace ring buffer. Server treats `since: 0` as
    // "from the beginning of the event log" (see
    // crates/calm-server/src/ws/events.rs).
    const payload: { sub: string[]; since: number } = {
      sub: [...this.topics],
      since: this.lastEventId ?? 0,
    };
    this.ws.send(JSON.stringify(payload));
  }
}

/** Issue #189 — module-level guard against probe-storming. When the WS
 *  upgrade fails (close without prior open), we hit `/api/auth/whoami`
 *  once to discriminate auth-rejection from generic network failure.
 *  Multiple EventStream instances all reconnecting at once would
 *  otherwise pile up redundant probes.
 *
 *  Exported `_resetProbeForTest` lets unit tests reset the latch between
 *  test cases that otherwise share module state. */
let probeInFlight = false;
function probeUnauthorized(): void {
  if (probeInFlight) return;
  probeInFlight = true;
  // `credentials: 'include'` matches `auth.ts`'s whoami — the cookie
  // ride-along is what the probe is actually testing.
  fetch('/api/auth/whoami', { credentials: 'include' })
    .then((res) => {
      if (res.status === 401) {
        fireUnauthorized();
      }
    })
    .catch(() => {
      // Network down entirely — nothing to do. The next reconnect tick
      // will retry; if the network comes back and we're still
      // unauthenticated, the probe runs again.
    })
    .finally(() => {
      probeInFlight = false;
    });
}

export function _resetProbeForTest(): void {
  probeInFlight = false;
}

/** Read the persisted cursor from localStorage. Returns `null` on missing
 *  / malformed / non-numeric values (cold-start case). */
function loadCursor(): number | null {
  try {
    const raw = localStorage.getItem(CURSOR_STORAGE_KEY);
    if (raw === null) return null;
    const n = Number(raw);
    if (!Number.isFinite(n) || n <= 0) return null;
    return n;
  } catch {
    return null;
  }
}

function wsUrl(path: string): string {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${location.host}${path}`;
}

// ---------------- React-flavored helper (kept here so callers can pick) ----

/**
 * Singleton stream. Module-level so a hot-reload doesn't churn the connection.
 *
 * Issue #198 followup (PR #215): we DO NOT auto-call `start()` here. The
 * `/api/events` WebSocket may only be opened once `ServerCompatGate` has
 * confirmed the server is compatible AND `EventBridge` has stamped the
 * `syncEventVersion` onto the stream — otherwise an incompatible frontend
 * could briefly hold a live socket before the compat verdict lands, and a
 * subscriber could see frames the per-frame eventVersion gate is not yet
 * configured to drop.
 *
 * The bridge is the singleton's sole `start()` caller: it sets the version,
 * subscribes, THEN starts the socket (see `app/eventBridge.tsx`). Other
 * consumers (`useConnectionState`, codex's hook listener) just register
 * handlers — registration is connection-agnostic, so calling
 * `sharedEventStream()` from a component that mounts before the bridge is
 * now safe by construction: no socket is attempted until the bridge runs.
 *
 * For tests that need a connected stream without the bridge in scope,
 * construct `new EventStream(url)` directly and call `start()` — the class
 * surface is unchanged; only the singleton's lifecycle moved.
 */
let _shared: EventStream | null = null;
export function sharedEventStream(): EventStream {
  if (!_shared) {
    _shared = new EventStream();
  }
  return _shared;
}

/** Test-only reset for the singleton. Mirrors `_resetProbeForTest` — used
 *  by test suites that import this module across `vi.resetModules()` calls
 *  to make sure each scenario starts from a clean singleton. Not part of
 *  the public surface; the underscore prefix marks the unsupported escape
 *  hatch. */
export function _resetSharedStreamForTest(): void {
  if (_shared) _shared.close();
  _shared = null;
}
