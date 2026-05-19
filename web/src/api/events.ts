// WebSocket subscription manager for /api/events.
//
// Auto-reconnects with exponential backoff. The subscription set is sticky —
// changing it (via `subscribe(topics)`) re-publishes to the server and is
// remembered across reconnects.

import type { WireEvent } from './wire';

type Listener = (ev: WireEvent) => void;

export class EventStream {
  private url: string;
  private ws: WebSocket | null = null;
  private listeners = new Set<Listener>();
  private topics = new Set<string>();
  private closed = false;
  private backoffMs = 500;
  private readonly maxBackoff = 8000;

  constructor(url = wsUrl('/api/events')) {
    this.url = url;
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
      let parsed: WireEvent;
      try {
        parsed = JSON.parse(typeof m.data === 'string' ? m.data : '');
      } catch {
        return;
      }
      for (const fn of this.listeners) fn(parsed);
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

  private publishSub(): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ sub: [...this.topics] }));
    }
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
