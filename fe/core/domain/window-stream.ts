// window-stream protocol v1 (`crates/window-stream/PROTOCOL.md`), the pure half: decoding what the
// server sends, encoding what the viewer sends, and mapping canvas CSS pixels to window pixels.
// No socket, timer or decoder lives here; `web/src/systems/window-stream` owns those.

import { z } from 'zod';

export const WINDOW_STREAM_VERSION = 1;
/** Bytes before a frame's payload: version, codec, flags, reserved, then u32 LE width and height. */
export const FRAME_HEADER_BYTES = 12;
const CODEC_JPEG = 1;
const FLAG_KEYFRAME = 0b1;

/** One binary frame message. Version 1 knows only JPEG, where every frame is a keyframe. */
export type WindowFrame = Readonly<{
  codec: 'jpeg';
  keyframe: boolean;
  width: number;
  height: number;
  payload: Uint8Array<ArrayBuffer>;
}>;

/**
 * A binary message as a frame, or `null` for one a v1 viewer ignores: too short, another protocol
 * version, an unknown codec, or an empty window size. The header's size is authoritative.
 */
export function decodeWindowFrame(data: ArrayBuffer): WindowFrame | null {
  if (data.byteLength <= FRAME_HEADER_BYTES) return null;
  const view = new DataView(data);
  if (view.getUint8(0) !== WINDOW_STREAM_VERSION || view.getUint8(1) !== CODEC_JPEG) return null;
  const width = view.getUint32(4, true);
  const height = view.getUint32(8, true);
  if (width === 0 || height === 0) return null;
  return {
    codec: 'jpeg',
    keyframe: (view.getUint8(2) & FLAG_KEYFRAME) !== 0,
    width,
    height,
    payload: new Uint8Array(data, FRAME_HEADER_BYTES),
  };
}

const helloSchema = z.object({
  type: z.literal('hello'),
  version: z.number(),
  codec: z.string(),
  width: z.number().int().positive(),
  height: z.number().int().positive(),
  title: z.string(),
});
const titleSchema = z.object({ type: z.literal('title'), title: z.string() });
const closedSchema = z.object({ type: z.literal('closed') });

/**
 * A text message the viewer acts on. `unsupported` is a `hello` naming another protocol version or
 * a codec this viewer cannot decode: the window is then shown as unavailable.
 */
export type WindowServerMessage =
  | Readonly<{ type: 'hello'; width: number; height: number; title: string }>
  | Readonly<{ type: 'unsupported' }>
  | Readonly<{ type: 'title'; title: string }>
  | Readonly<{ type: 'closed' }>;

/** A text message, or `null` for one a viewer ignores (unparsable, unknown `type`, malformed fields). */
export function decodeWindowServerText(text: string): WindowServerMessage | null {
  let value: unknown;
  try { value = JSON.parse(text); } catch { return null; }
  const hello = helloSchema.safeParse(value);
  if (hello.success) {
    const { version, codec, width, height, title } = hello.data;
    if (version !== WINDOW_STREAM_VERSION || codec !== 'jpeg') return { type: 'unsupported' };
    return { type: 'hello', width, height, title };
  }
  const title = titleSchema.safeParse(value);
  if (title.success) return { type: 'title', title: title.data.title };
  if (closedSchema.safeParse(value).success) return { type: 'closed' };
  return null;
}

/** Viewer → server input. Coordinates and distances are window pixels. */
export type WindowInput =
  | Readonly<{ type: 'pointer'; x: number; y: number }>
  | Readonly<{ type: 'button'; button: number; pressed: boolean }>
  | Readonly<{ type: 'wheel'; dx: number; dy: number }>
  | Readonly<{ type: 'key'; code: string; pressed: boolean }>;

export function encodeWindowInput(input: WindowInput): string {
  return JSON.stringify(input);
}

/** The canvas's on-screen box, in CSS pixels (`getBoundingClientRect`). */
export type CanvasBox = Readonly<{ left: number; top: number; width: number; height: number }>;
export type WindowSize = Readonly<{ width: number; height: number }>;

/**
 * Where the window is drawn inside the canvas box: scaled to fit (`object-fit: contain`), centred.
 * `scale` is CSS pixels per window pixel.
 */
function fitted(box: CanvasBox, size: WindowSize): Readonly<{ scale: number; left: number; top: number }> {
  if (box.width <= 0 || box.height <= 0 || size.width <= 0 || size.height <= 0) return { scale: 1, left: box.left, top: box.top };
  const scale = Math.min(box.width / size.width, box.height / size.height);
  return {
    scale,
    left: box.left + (box.width - size.width * scale) / 2,
    top: box.top + (box.height - size.height * scale) / 2,
  };
}

/** A client point over the canvas as a window pixel. The server clamps points outside the window. */
export function toWindowPoint(clientX: number, clientY: number, box: CanvasBox, size: WindowSize): Readonly<{ x: number; y: number }> {
  const fit = fitted(box, size);
  return { x: (clientX - fit.left) / fit.scale, y: (clientY - fit.top) / fit.scale };
}

/** `WheelEvent.deltaMode` values. */
const DOM_DELTA_LINE = 1;
const DOM_DELTA_PAGE = 2;
const LINE_HEIGHT_PX = 16;

/**
 * A wheel event's deltas as window pixels. Pixels and lines (16 CSS px) scale like a point; a page
 * is one window width or height, whatever the canvas box.
 */
export function toWindowWheel(
  delta: Readonly<{ deltaX: number; deltaY: number; deltaMode: number }>, box: CanvasBox, size: WindowSize,
): Readonly<{ dx: number; dy: number }> {
  if (delta.deltaMode === DOM_DELTA_PAGE) return { dx: delta.deltaX * size.width, dy: delta.deltaY * size.height };
  const { scale } = fitted(box, size);
  const unit = delta.deltaMode === DOM_DELTA_LINE ? LINE_HEIGHT_PX : 1;
  return { dx: (delta.deltaX * unit) / scale, dy: (delta.deltaY * unit) / scale };
}

/** `PointerEvent.buttons` bits, in bit order, with the `MouseEvent.button` index each one is. */
const BUTTON_BITS = Object.freeze([
  Object.freeze([0b00001, 0]), Object.freeze([0b00010, 2]), Object.freeze([0b00100, 1]),
  Object.freeze([0b01000, 3]), Object.freeze([0b10000, 4]),
] as const);

/**
 * The `button` messages that take the window from one `buttons` mask to the next, one per changed
 * button. Browsers report a second button of a chord on `pointermove`, not `pointerdown`, so the
 * mask is the only complete record of what is held.
 */
export function buttonChanges(previous: number, next: number): WindowInput[] {
  return BUTTON_BITS
    .filter(([bit]) => (previous & bit) !== (next & bit))
    .map(([bit, button]) => ({ type: 'button', button, pressed: (next & bit) !== 0 }));
}

const FIRST_RETRY_MS = 500;
const LAST_RETRY_MS = 10_000;

/** The wait before reconnect attempt `attempt` (0-based): doubling from 0.5 s, capped at 10 s. */
export function windowStreamRetryDelay(attempt: number): number {
  return Math.min(LAST_RETRY_MS, FIRST_RETRY_MS * 2 ** Math.max(0, attempt));
}
