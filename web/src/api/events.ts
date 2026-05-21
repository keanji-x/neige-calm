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

  /** Current cursor — `null` for a fresh client, or the largest `_id`
   *  observed. Exposed primarily for tests. */
  get cursor(): number | null {
    return this.lastEventId;
  }

  start(): void {
    if (this.ws || this.closed) return;
    this.connect();
  }

  close(): void {
    this.closed = true;
    this.ws?.close();
    this.ws = null;
  }

  private connect(): void {
    const ws = new WebSocket(this.url);
    this.ws = ws;
    ws.addEventListener('open', () => {
      this.backoffMs = 500;
      this.publishSub();
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
    // Update cursor BEFORE zod parse so a malformed payload still
    // advances the cursor (its `_id` is the server's idea of "you saw
    // this row"). Otherwise a single bad frame could pin the cursor and
    // force a re-replay on every reconnect.
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
    const payload: { sub: string[]; since?: number } = { sub: [...this.topics] };
    if (this.lastEventId !== null) {
      payload.since = this.lastEventId;
    }
    this.ws.send(JSON.stringify(payload));
  }
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
 * Singleton stream. Lazily started on first subscribe; callers don't manage
 * its lifecycle. Module-level so a hot-reload doesn't churn the connection.
 */
let _shared: EventStream | null = null;
export function sharedEventStream(): EventStream {
  if (!_shared) {
    _shared = new EventStream();
    _shared.start();
  }
  return _shared;
}
