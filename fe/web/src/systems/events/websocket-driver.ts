import { decodeEventFrame, eventSubscriptionFrame } from '../../../../core/events/protocol.ts';
import type { SyncCursorPort } from './cursor-port.ts';
import type { EventStreamConfiguration, EventStreamDriver, EventStreamSink } from './event-stream.ts';

export interface SocketPort {
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  send(data: string): void;
  close(): void;
}

export type UnauthorizedProbe = () => Promise<unknown>;
export type SocketFactory = (url: string) => SocketPort;

function isUnauthorized(error: unknown): boolean {
  if (typeof error !== 'object' || error === null || !('failure' in error)) return false;
  const failure = error.failure;
  return typeof failure === 'object' && failure !== null && 'kind' in failure && failure.kind === 'unauthorized';
}

export function wsUrl(location: Pick<Location, 'protocol' | 'host'> = window.location): string {
  return `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/api/events`;
}

export class WebSocketDriver implements EventStreamDriver {
  private readonly cursor: Pick<SyncCursorPort, 'read'>;
  private readonly probeUnauthorized: UnauthorizedProbe;
  private readonly onUnauthorized: () => void;
  private readonly socketFactory: SocketFactory;
  private epoch = 0;
  private closed = true;
  private socket: SocketPort | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private retryDelay = 500;
  private probeInFlight = false;
  private unauthorized = false;
  private configuration: EventStreamConfiguration | null = null;
  private url = '';
  private sink: EventStreamSink | null = null;

  constructor(options: Readonly<{
    cursor: Pick<SyncCursorPort, 'read'>;
    probeUnauthorized: UnauthorizedProbe;
    onUnauthorized: () => void;
    socketFactory?: SocketFactory;
  }>) {
    this.cursor = options.cursor;
    this.probeUnauthorized = options.probeUnauthorized;
    this.onUnauthorized = options.onUnauthorized;
    this.socketFactory = options.socketFactory ?? ((url) => new WebSocket(url));
  }

  start(configuration: EventStreamConfiguration, url: string, sink: EventStreamSink): void {
    const epoch = ++this.epoch;
    this.closed = false;
    this.probeInFlight = false;
    this.configuration = configuration;
    this.url = url;
    this.sink = sink;
    sink.connectionState('connecting');
    if (epoch !== this.epoch || this.closed) return;
    this.connect(epoch);
  }

  stop(): void {
    ++this.epoch;
    this.closed = true;
    this.probeInFlight = false;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    const socket = this.socket;
    this.socket = null;
    if (socket !== null) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      socket.close();
    }
  }

  private connect(epoch: number): void {
    if (this.closed || epoch !== this.epoch) return;
    const configuration = this.configuration;
    const sink = this.sink;
    if (configuration === null || sink === null) return;
    let socket: SocketPort;
    try { socket = this.socketFactory(this.url); }
    catch { this.scheduleRetry(epoch); return; }
    if (this.closed || epoch !== this.epoch) { socket.close(); return; }
    this.socket = socket;
    let opened = false;
    socket.onopen = () => {
      if (this.closed || epoch !== this.epoch) { socket.close(); return; }
      opened = true;
      this.retryDelay = 500;
      socket.send(JSON.stringify(eventSubscriptionFrame(configuration.topics, this.cursor.read())));
    };
    socket.onmessage = (event) => {
      if (this.closed || epoch !== this.epoch) return;
      let input: unknown;
      try { input = typeof event.data === 'string' ? JSON.parse(event.data) : event.data; }
      catch { input = event.data; }
      const decoded = decodeEventFrame(input);
      if (decoded.status !== 'ready') return;
      if (decoded.frame.type === 'replay-complete') {
        sink.connectionState('connected');
        if (this.closed || epoch !== this.epoch) return;
      }
      sink.frame(decoded.frame);
      if (this.closed || epoch !== this.epoch) return;
    };
    socket.onerror = () => { if (this.closed || epoch !== this.epoch) return; };
    socket.onclose = () => {
      if (this.closed || epoch !== this.epoch) return;
      if (this.socket === socket) this.socket = null;
      sink.connectionState('connecting');
      if (this.closed || epoch !== this.epoch) return;
      if (!opened) this.probe(epoch);
      if (this.closed || epoch !== this.epoch) return;
      this.scheduleRetry(epoch);
    };
  }

  private scheduleRetry(epoch: number): void {
    if (this.closed || epoch !== this.epoch || this.retryTimer !== null) return;
    const delay = this.retryDelay;
    this.retryDelay = Math.min(this.retryDelay * 2, 8000);
    this.retryTimer = setTimeout(() => {
      if (this.closed || epoch !== this.epoch) return;
      this.retryTimer = null;
      this.connect(epoch);
    }, delay);
  }

  private probe(epoch: number): void {
    if (this.probeInFlight) return;
    this.probeInFlight = true;
    void this.probeUnauthorized().then(() => {
      if (this.closed || epoch !== this.epoch) return;
      this.unauthorized = false;
    }).catch((error: unknown) => {
      if (this.closed || epoch !== this.epoch) return;
      if (isUnauthorized(error) && !this.unauthorized) {
        this.unauthorized = true;
        this.onUnauthorized();
      }
    }).finally(() => {
      if (this.closed || epoch !== this.epoch) return;
      this.probeInFlight = false;
    });
  }
}
