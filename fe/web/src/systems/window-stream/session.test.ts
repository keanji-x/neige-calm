import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { WindowFrame } from '../../../../core/domain/window-stream.ts';
import { WindowStreamSession, windowStreamUrl, type WindowSocket, type WindowStreamView } from './session.ts';

class FakeSocket implements WindowSocket {
  binaryType: BinaryType = 'blob';
  readyState = 1;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readonly sent: string[] = [];
  closed = false;
  send(data: string) { this.sent.push(data); }
  close() { this.closed = true; this.readyState = 3; }
  text(value: unknown) { this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(value) })); }
  frame(width: number, height: number, marker: number) {
    const bytes = new Uint8Array(13);
    bytes.set([1, 1, 1, 0]);
    new DataView(bytes.buffer).setUint32(4, width, true);
    new DataView(bytes.buffer).setUint32(8, height, true);
    bytes[12] = marker;
    this.onmessage?.(new MessageEvent('message', { data: bytes.buffer }));
  }
  drop() { this.onclose?.(new CloseEvent('close')); }
}

const HELLO = { type: 'hello', version: 1, codec: 'jpeg', width: 4, height: 2, title: 'Example' };

type Bitmap = ImageBitmap & { marker: number; closed: boolean };

function harness() {
  const sockets: FakeSocket[] = [];
  const decodes: Array<{ frame: WindowFrame; resolve: () => void }> = [];
  const drawn: number[] = [];
  const closedBitmaps: number[] = [];
  const views: WindowStreamView[] = [];
  const session = new WindowStreamSession('ws://host.test/api/plugins/p/ws/s', {
    openSocket: () => { const socket = new FakeSocket(); sockets.push(socket); return socket; },
    decode: (frame) => new Promise<ImageBitmap>((resolve) => {
      const marker = frame.payload[0] ?? -1;
      const bitmap = { width: frame.width, height: frame.height, marker, closed: false,
        close() { closedBitmaps.push(marker); } } as unknown as Bitmap;
      decodes.push({ frame, resolve: () => resolve(bitmap) });
    }),
    draw: (bitmap) => { drawn.push((bitmap as Bitmap).marker); },
    view: (view) => { views.push(view); },
  });
  const settle = async () => { for (let i = 0; i < 5; i += 1) await Promise.resolve(); };
  return { session, sockets, decodes, drawn, closedBitmaps, views, settle };
}

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); });

describe('windowStreamUrl', () => {
  it('opens the src on this page host with the matching WebSocket scheme', () => {
    expect(windowStreamUrl('/api/plugins/p/ws/s', { protocol: 'https:', host: 'host.example:8443' })).toBe('wss://host.example:8443/api/plugins/p/ws/s');
    expect(windowStreamUrl('/api/plugins/p/ws/s', { protocol: 'http:', host: '127.0.0.1:5173' })).toBe('ws://127.0.0.1:5173/api/plugins/p/ws/s');
  });
});

describe('WindowStreamSession', () => {
  it('is live only once a frame of the session is drawn, and sends input only then', async () => {
    const h = harness();
    h.session.start();
    const socket = h.sockets[0];
    expect(socket.binaryType).toBe('arraybuffer');
    socket.text(HELLO);
    expect(h.views.at(-1)).toEqual({ status: 'connecting', windowTitle: 'Example' });
    expect(h.session.send({ type: 'key', code: 'KeyA', pressed: true })).toBe(false);
    socket.frame(4, 2, 7);
    h.decodes[0].resolve();
    await h.settle();
    expect(h.drawn).toEqual([7]);
    expect(h.closedBitmaps).toEqual([7]);
    expect(h.views.at(-1)).toEqual({ status: 'live', windowTitle: 'Example' });
    expect(h.session.send({ type: 'key', code: 'KeyA', pressed: true })).toBe(true);
    expect(socket.sent).toEqual(['{"type":"key","code":"KeyA","pressed":true}']);
    socket.text({ type: 'title', title: 'Next' });
    expect(h.views.at(-1)).toEqual({ status: 'live', windowTitle: 'Next' });
  });

  it('ignores a frame before hello and messages it does not know', () => {
    const h = harness();
    h.session.start();
    h.sockets[0].frame(4, 2, 1);
    h.sockets[0].text({ type: 'cursor' });
    expect(h.decodes).toHaveLength(0);
  });

  it('decodes latest-wins: frames arriving during a decode collapse to the newest', async () => {
    const h = harness();
    h.session.start();
    const socket = h.sockets[0];
    socket.text(HELLO);
    socket.frame(4, 2, 1);
    socket.frame(4, 2, 2);
    socket.frame(4, 2, 3);
    h.decodes[0].resolve();
    await h.settle();
    expect(h.decodes.map((d) => d.frame.payload[0])).toEqual([1, 3]);
    h.decodes[1].resolve();
    await h.settle();
    expect(h.drawn).toEqual([1, 3]);
  });

  it('after closed it is unavailable, draws no late frame and refuses input', async () => {
    const h = harness();
    h.session.start();
    const socket = h.sockets[0];
    socket.text(HELLO);
    socket.frame(4, 2, 1);
    h.decodes[0].resolve();
    await h.settle();
    socket.frame(4, 2, 2);
    socket.text({ type: 'closed' });
    expect(h.views.at(-1)?.status).toBe('unavailable');
    expect(socket.closed).toBe(true);
    h.decodes[1].resolve();
    await h.settle();
    expect(h.drawn).toEqual([1]);
    expect(h.closedBitmaps).toEqual([1, 2]);
    expect(h.session.send({ type: 'pointer', x: 1, y: 1 })).toBe(false);
    expect(h.views.at(-1)?.status).toBe('unavailable');
  });

  it('treats a hello it cannot decode as unavailable', () => {
    const h = harness();
    h.session.start();
    h.sockets[0].text({ ...HELLO, codec: 'h264' });
    expect(h.views.at(-1)?.status).toBe('unavailable');
  });

  it('reconnects with backoff and is live again only after a new frame', async () => {
    const h = harness();
    h.session.start();
    h.sockets[0].drop();
    expect(h.views.at(-1)?.status).toBe('unavailable');
    vi.advanceTimersByTime(499);
    expect(h.sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(h.sockets).toHaveLength(2);
    h.sockets[1].drop();
    vi.advanceTimersByTime(999);
    expect(h.sockets).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(h.sockets).toHaveLength(3);
    const socket = h.sockets[2];
    socket.text(HELLO);
    expect(h.views.at(-1)?.status).toBe('unavailable');
    socket.frame(4, 2, 5);
    h.decodes[0].resolve();
    await h.settle();
    expect(h.views.at(-1)?.status).toBe('live');
    socket.drop();
    vi.advanceTimersByTime(500);
    expect(h.sockets).toHaveLength(4);
  });

  it('stop closes the socket and cancels a pending reconnect', () => {
    const h = harness();
    h.session.start();
    h.sockets[0].drop();
    h.session.stop();
    vi.advanceTimersByTime(60_000);
    expect(h.sockets).toHaveLength(1);
    h.session.start();
    h.session.stop();
    expect(h.sockets[1].closed).toBe(true);
  });
});
