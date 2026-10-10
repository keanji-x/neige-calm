import { cleanup, render, screen } from '@testing-library/react';
import { userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import type { WindowSocket } from '../../../systems/window-stream/public.tsx';
import { ReportWindowBlock } from './public.tsx';
import '../../../styles/entry.css';

afterEach(cleanup);

const WIDTH = 80;
const HEIGHT = 50;

/** One real JPEG of the whole window in one colour, encoded by this browser. */
async function jpeg(color: string): Promise<Uint8Array> {
  const canvas = new OffscreenCanvas(WIDTH, HEIGHT);
  const context = canvas.getContext('2d')!;
  context.fillStyle = color;
  context.fillRect(0, 0, WIDTH, HEIGHT);
  return new Uint8Array(await (await canvas.convertToBlob({ type: 'image/jpeg', quality: 0.95 })).arrayBuffer());
}

/** The server end of one v1 session: what `crates/window-stream` sends, byte for byte. */
class ServerSocket implements WindowSocket {
  binaryType: BinaryType = 'blob';
  readyState = 1;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readonly received: unknown[] = [];
  readonly url: string;
  constructor(url: string) { this.url = url; }
  send(data: string) { this.received.push(JSON.parse(data)); }
  close() { this.readyState = 3; }
  text(value: unknown) { this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(value) })); }
  hello() { this.text({ type: 'hello', version: 1, codec: 'jpeg', width: WIDTH, height: HEIGHT, title: 'Example - Chrome' }); }
  frame(payload: Uint8Array) {
    const message = new Uint8Array(12 + payload.length);
    message.set([1, 1, 1, 0]);
    new DataView(message.buffer).setUint32(4, WIDTH, true);
    new DataView(message.buffer).setUint32(8, HEIGHT, true);
    message.set(payload, 12);
    this.onmessage?.(new MessageEvent('message', { data: message.buffer }));
  }
  drop() { this.readyState = 3; this.onclose?.(new CloseEvent('close')); }
}

function centrePixel(canvas: HTMLCanvasElement): readonly number[] {
  return [...canvas.getContext('2d')!.getImageData(WIDTH / 2, HEIGHT / 2, 1, 1).data.slice(0, 3)];
}

const isRed = ([r = 0, g = 0, b = 0]: readonly number[]) => r > 200 && g < 60 && b < 60;
const isGreen = ([r = 0, g = 0, b = 0]: readonly number[]) => g > 100 && r < 60 && b < 60;

it('draws v1 frames live, sends input as v1 JSON, and dims the last frame under "unavailable" once the session ends', async () => {
  const sockets: ServerSocket[] = [];
  const openSocket = (url: string) => { const socket = new ServerSocket(url); sockets.push(socket); return socket; };
  const red = await jpeg('#ff0000');
  const blue = await jpeg('#0000ff');
  const green = await jpeg('#00a000');
  render(<ReportWindowBlock payload={{ src: '/api/plugins/test/ws/apps/one/stream', title: 'Browser', height: 300 }} stream={{ openSocket }} />);

  await expect.poll(() => sockets.length).toBe(1);
  expect(sockets[0].url).toBe(`ws://${window.location.host}/api/plugins/test/ws/apps/one/stream`);
  const canvas = screen.getByLabelText(/^Browser: live window/);
  if (!(canvas instanceof HTMLCanvasElement)) throw new Error('the viewer renders a canvas');
  expect(screen.getByRole('status').textContent).toBe('connecting…');

  sockets[0].hello();
  sockets[0].frame(red);
  await expect.poll(() => screen.queryByRole('status')).toBeNull();
  expect([canvas.width, canvas.height]).toEqual([WIDTH, HEIGHT]);
  expect(isRed(centrePixel(canvas))).toBe(true);
  expect(getComputedStyle(canvas).opacity).toBe('1');
  expect(screen.getByText('Browser · Example - Chrome')).toBeTruthy();

  // A click in the middle of the canvas is the middle of the window, whatever the canvas's CSS size.
  await userEvent.click(canvas);
  await expect.poll(() => sockets[0].received.filter((m) => (m as { type: string }).type === 'button')).toEqual([
    { type: 'button', button: 0, pressed: true },
    { type: 'button', button: 0, pressed: false },
  ]);
  const pointer = sockets[0].received.find((m) => (m as { type: string }).type === 'pointer') as { x: number; y: number };
  expect(Math.abs(pointer.x - WIDTH / 2)).toBeLessThan(1);
  expect(Math.abs(pointer.y - HEIGHT / 2)).toBeLessThan(1);
  expect(document.activeElement).toBe(canvas);

  let keyDefaultPrevented: boolean | null = null;
  const observe = (event: KeyboardEvent) => { keyDefaultPrevented = event.defaultPrevented; };
  window.addEventListener('keydown', observe);
  await userEvent.keyboard('a');
  window.removeEventListener('keydown', observe);
  expect(keyDefaultPrevented).toBe(true);
  expect(sockets[0].received.filter((m) => (m as { type: string }).type === 'key')).toEqual([
    { type: 'key', code: 'KeyA', pressed: true },
    { type: 'key', code: 'KeyA', pressed: false },
  ]);

  // A frame still decoding when `closed` arrives is never drawn: the canvas keeps the old frame, dimmed.
  sockets[0].frame(blue);
  sockets[0].text({ type: 'closed' });
  await expect.poll(() => screen.queryByRole('status')?.textContent).toBe('unavailable');
  await new Promise((resolve) => setTimeout(resolve, 200));
  expect(isRed(centrePixel(canvas))).toBe(true);
  expect(getComputedStyle(canvas).opacity).toBe('0.3');
  expect(canvas.getAttribute('aria-label')).toBe('Browser: live window, unavailable');
  const sentBefore = sockets[0].received.length;
  await userEvent.click(canvas);
  expect(sockets[0].received).toHaveLength(sentBefore);

  // The viewer reconnects; only a frame of the new session makes it live again.
  await expect.poll(() => sockets.length, { timeout: 3000 }).toBe(2);
  sockets[1].hello();
  expect(screen.getByRole('status').textContent).toBe('unavailable');
  sockets[1].frame(green);
  await expect.poll(() => screen.queryByRole('status')).toBeNull();
  expect(isGreen(centrePixel(canvas))).toBe(true);

  // A dropped socket is the same: dimmed, "unavailable", and no new frame drawn.
  sockets[1].frame(red);
  sockets[1].drop();
  await expect.poll(() => screen.queryByRole('status')?.textContent).toBe('unavailable');
  await new Promise((resolve) => setTimeout(resolve, 200));
  expect(isGreen(centrePixel(canvas))).toBe(true);
  expect(getComputedStyle(canvas).opacity).toBe('0.3');
});

function buttonsSent(socket: ServerSocket): unknown[] {
  return socket.received.filter((m) => (m as { type: string }).type === 'button');
}

function pointer(canvas: HTMLCanvasElement, type: string, button: number, buttons: number): void {
  const box = canvas.getBoundingClientRect();
  canvas.dispatchEvent(new PointerEvent(type, {
    bubbles: true, cancelable: true, pointerId: 1, pointerType: 'mouse', button, buttons,
    clientX: box.left + box.width / 2, clientY: box.top + box.height / 2,
  }));
}

function prevented(canvas: HTMLCanvasElement, event: Event): boolean {
  canvas.dispatchEvent(event);
  return event.defaultPrevented;
}

const keyDown = () => new KeyboardEvent('keydown', { bubbles: true, cancelable: true, code: 'Tab', key: 'Tab' });
const wheel = () => new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: 120 });

async function mountLive(): Promise<{ sockets: ServerSocket[]; canvas: HTMLCanvasElement; red: Uint8Array }> {
  const sockets: ServerSocket[] = [];
  const openSocket = (url: string) => { const socket = new ServerSocket(url); sockets.push(socket); return socket; };
  const red = await jpeg('#ff0000');
  render(<ReportWindowBlock payload={{ src: '/api/plugins/test/ws/s', title: 'Browser' }} stream={{ openSocket }} />);
  await expect.poll(() => sockets.length).toBe(1);
  const canvas = screen.getByLabelText(/^Browser: live window/);
  if (!(canvas instanceof HTMLCanvasElement)) throw new Error('the viewer renders a canvas');
  canvas.focus();
  return { sockets, canvas, red };
}

async function goLive(sockets: ServerSocket[], index: number, frame: Uint8Array): Promise<void> {
  sockets[index].hello();
  sockets[index].frame(frame);
  await expect.poll(() => screen.queryByRole('status')).toBeNull();
}

it('swallows keys and wheel only while live', async () => {
  const { sockets, canvas, red } = await mountLive();
  // Not live yet: Tab and the page's scroll keep working.
  expect(prevented(canvas, keyDown())).toBe(false);
  expect(prevented(canvas, wheel())).toBe(false);
  await goLive(sockets, 0, red);
  expect(prevented(canvas, keyDown())).toBe(true);
  expect(prevented(canvas, wheel())).toBe(true);
  sockets[0].text({ type: 'closed' });
  await expect.poll(() => screen.queryByRole('status')?.textContent).toBe('unavailable');
  expect(prevented(canvas, keyDown())).toBe(false);
  expect(prevented(canvas, wheel())).toBe(false);
});

it('sends chorded buttons one change at a time and releases held buttons', async () => {
  const { sockets, canvas, red } = await mountLive();
  await goLive(sockets, 0, red);

  // Press left, press right, release left, release right: browsers report the chord on pointermove.
  pointer(canvas, 'pointerdown', 0, 1);
  pointer(canvas, 'pointermove', 2, 3);
  pointer(canvas, 'pointermove', 0, 2);
  pointer(canvas, 'pointerup', 2, 0);
  expect(buttonsSent(sockets[0])).toEqual([
    { type: 'button', button: 0, pressed: true },
    { type: 'button', button: 2, pressed: true },
    { type: 'button', button: 0, pressed: false },
    { type: 'button', button: 2, pressed: false },
  ]);

  // A cancelled pointer, a lost capture and a blur each release whatever is held.
  for (const end of [
    () => pointer(canvas, 'pointercancel', 0, 0),
    () => canvas.dispatchEvent(new PointerEvent('lostpointercapture', { pointerId: 1 })),
    () => canvas.blur(),
  ]) {
    sockets[0].received.length = 0;
    canvas.focus();
    pointer(canvas, 'pointerdown', 1, 4);
    end();
    expect(buttonsSent(sockets[0])).toEqual([
      { type: 'button', button: 1, pressed: true },
      { type: 'button', button: 1, pressed: false },
    ]);
  }

  // A button held when the session ends is forgotten: the next session never hears a stale release.
  canvas.focus();
  pointer(canvas, 'pointerdown', 0, 1);
  sockets[0].text({ type: 'closed' });
  await expect.poll(() => screen.queryByRole('status')?.textContent).toBe('unavailable');
  await expect.poll(() => sockets.length, { timeout: 3000 }).toBe(2);
  await goLive(sockets, 1, red);
  pointer(canvas, 'pointerup', 0, 0);
  expect(buttonsSent(sockets[1])).toEqual([]);
});
