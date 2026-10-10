import {
  decodeWindowFrame, decodeWindowServerText, encodeWindowInput, windowStreamRetryDelay,
  type WindowFrame, type WindowInput,
} from '../../../../core/domain/window-stream.ts';

/**
 * `connecting`: no frame of this viewer has been drawn yet. `live`: the canvas shows a frame of the
 * open session. `unavailable`: the session ended (the window closed, the socket dropped, the server
 * speaks another version or codec) and any frame still on the canvas is old.
 */
export type WindowStreamStatus = 'connecting' | 'live' | 'unavailable';

export type WindowStreamView = Readonly<{ status: WindowStreamStatus; windowTitle: string | null }>;

/** The part of `WebSocket` the session uses. */
export interface WindowSocket {
  binaryType: BinaryType;
  readonly readyState: number;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  send(data: string): void;
  close(): void;
}

export type WindowStreamPorts = Readonly<{
  openSocket: (url: string) => WindowSocket;
  decode: (frame: WindowFrame) => Promise<ImageBitmap>;
  /** Draws a decoded frame of the open session; the session closes the bitmap afterwards. */
  draw: (bitmap: ImageBitmap, frame: WindowFrame) => void;
  view: (view: WindowStreamView) => void;
}>;

const OPEN = 1;

/** `ws(s)://<this page's host><src>`: the page's session cookie and origin rules apply to the socket. */
export function windowStreamUrl(src: string, location: Pick<Location, 'protocol' | 'host'>): string {
  return `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}${src}`;
}

/**
 * One viewer of one window-stream URL: connects, reconnects with backoff, decodes frames latest-wins
 * and sends input. Every socket is one generation; a decode that finishes after its generation ended
 * is discarded, so an ended session can never draw or report itself live.
 */
export class WindowStreamSession {
  private generation = 0;
  private stopped = true;
  private socket: WindowSocket | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private greeted = false;
  private live = false;
  private decoding = false;
  private pending: WindowFrame | null = null;
  private status: WindowStreamStatus = 'connecting';
  private windowTitle: string | null = null;

  private readonly url: string;
  private readonly ports: WindowStreamPorts;

  constructor(url: string, ports: WindowStreamPorts) {
    this.url = url;
    this.ports = ports;
  }

  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.endSocket();
  }

  /** Whether the canvas shows a frame of the open session: the only time input is taken. */
  isLive(): boolean {
    return this.live && this.socket !== null && this.socket.readyState === OPEN;
  }

  /** Sends input only while the canvas shows a frame of the open session. */
  send(input: WindowInput): boolean {
    const socket = this.socket;
    if (!this.isLive() || socket === null) return false;
    socket.send(encodeWindowInput(input));
    return true;
  }

  private connect(): void {
    if (this.stopped) return;
    const generation = ++this.generation;
    let socket: WindowSocket;
    try { socket = this.ports.openSocket(this.url); } catch { this.unavailable(); return; }
    socket.binaryType = 'arraybuffer';
    this.socket = socket;
    socket.onmessage = (event) => {
      if (generation === this.generation) this.receive(event.data, generation);
    };
    socket.onclose = () => { if (generation === this.generation) this.unavailable(); };
    socket.onerror = () => { if (generation === this.generation) this.unavailable(); };
  }

  private receive(data: unknown, generation: number): void {
    if (typeof data === 'string') {
      const message = decodeWindowServerText(data);
      if (message === null) return;
      switch (message.type) {
        case 'hello':
          this.greeted = true;
          this.publish(this.status, message.title);
          return;
        case 'title':
          this.publish(this.status, message.title);
          return;
        case 'unsupported':
        case 'closed':
          this.unavailable();
          return;
      }
    }
    if (!(data instanceof ArrayBuffer) || !this.greeted) return;
    const frame = decodeWindowFrame(data);
    if (frame === null) return;
    if (this.decoding) { this.pending = frame; return; }
    this.decodeLatest(frame, generation);
  }

  /** Latest wins: while one frame decodes, a newer one replaces any waiting frame. */
  private decodeLatest(frame: WindowFrame, generation: number): void {
    this.decoding = true;
    this.ports.decode(frame).then((bitmap) => {
      try {
        if (generation !== this.generation || this.stopped) return;
        this.ports.draw(bitmap, frame);
      } catch {
        return;
      } finally {
        bitmap.close();
      }
      this.attempt = 0;
      if (!this.live) {
        this.live = true;
        this.publish('live', this.windowTitle);
      }
    }, () => undefined).finally(() => {
      if (generation !== this.generation) return;
      this.decoding = false;
      const next = this.pending;
      this.pending = null;
      if (next !== null) this.decodeLatest(next, generation);
    });
  }

  /** Ends the session, says so, and retries with backoff. */
  private unavailable(): void {
    this.endSocket();
    this.publish('unavailable', this.windowTitle);
    if (this.stopped || this.retryTimer !== null) return;
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      this.connect();
    }, windowStreamRetryDelay(this.attempt++));
  }

  private endSocket(): void {
    this.generation += 1;
    this.greeted = false;
    this.live = false;
    this.decoding = false;
    this.pending = null;
    const socket = this.socket;
    this.socket = null;
    if (socket === null) return;
    socket.onopen = null;
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
    socket.close();
  }

  private publish(status: WindowStreamStatus, windowTitle: string | null): void {
    if (this.stopped) return;
    if (status === this.status && windowTitle === this.windowTitle) return;
    this.status = status;
    this.windowTitle = windowTitle;
    this.ports.view({ status, windowTitle });
  }
}
