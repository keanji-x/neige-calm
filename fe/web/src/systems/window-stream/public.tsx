import { useEffect } from 'react';

import { useState } from '../../ui/state/public.ts';

import { toWindowPoint, toWindowWheel, type WindowFrame } from '../../../../core/domain/window-stream.ts';
import {
  WindowStreamSession, windowStreamUrl, type WindowSocket, type WindowStreamStatus, type WindowStreamView,
} from './session.ts';

export type { WindowStreamStatus } from './session.ts';

export type WindowStreamOptions = Readonly<{
  /** Opens the socket; the default is the browser's `WebSocket`. */
  openSocket?: (url: string) => WindowSocket;
}>;

export type WindowStream = Readonly<{
  /** Attach to the one canvas that shows the window; frames are drawn into it and its input is sent. */
  canvasRef: (canvas: HTMLCanvasElement | null) => void;
  status: WindowStreamStatus;
  /** The window's own title from `hello`/`title`, or `null` before the first `hello`. */
  windowTitle: string | null;
}>;

function decodeJpeg(frame: WindowFrame): Promise<ImageBitmap> {
  return createImageBitmap(new Blob([frame.payload], { type: 'image/jpeg' }));
}

function drawInto(canvas: HTMLCanvasElement | null, bitmap: ImageBitmap, frame: WindowFrame): void {
  if (canvas === null) return;
  if (canvas.width !== frame.width) canvas.width = frame.width;
  if (canvas.height !== frame.height) canvas.height = frame.height;
  canvas.getContext('2d')?.drawImage(bitmap, 0, 0, frame.width, frame.height);
}

/**
 * The viewer of one window-stream protocol v1 socket at the same-origin path `src`.
 *
 * Lifecycle: the socket opens when a canvas is attached and closes when it is detached, `src`
 * changes, or the component unmounts; every pending reconnect timer and decode is dropped then.
 * Input is sent only while `status` is `live`; keys are sent only while the canvas has focus.
 */
export function useWindowStream(src: string, options: WindowStreamOptions = {}): WindowStream {
  const [canvas, setCanvas] = useState<HTMLCanvasElement | null>(null);
  const [view, setView] = useState<WindowStreamView>({ status: 'connecting', windowTitle: null });
  const openSocket = options.openSocket;

  useEffect(() => {
    if (canvas === null) return undefined;
    setView({ status: 'connecting', windowTitle: null });
    const session = new WindowStreamSession(windowStreamUrl(src, window.location), {
      openSocket: openSocket ?? ((url) => new WebSocket(url)),
      decode: decodeJpeg,
      draw: (bitmap, frame) => drawInto(canvas, bitmap, frame),
      view: setView,
    });
    const held = new Set<string>();
    const size = () => ({ width: canvas.width, height: canvas.height });
    const pointer = (event: PointerEvent) => {
      const { x, y } = toWindowPoint(event.clientX, event.clientY, canvas.getBoundingClientRect(), size());
      session.send({ type: 'pointer', x, y });
    };
    const press = (event: PointerEvent) => {
      canvas.focus();
      canvas.setPointerCapture(event.pointerId);
      event.preventDefault();
      pointer(event);
      session.send({ type: 'button', button: event.button, pressed: true });
    };
    const release = (event: PointerEvent) => {
      pointer(event);
      session.send({ type: 'button', button: event.button, pressed: false });
    };
    const wheel = (event: WheelEvent) => {
      event.preventDefault();
      const { dx, dy } = toWindowWheel(event, canvas.getBoundingClientRect(), size());
      session.send({ type: 'wheel', dx, dy });
    };
    const key = (pressed: boolean) => (event: KeyboardEvent) => {
      event.preventDefault();
      if (event.repeat || event.code === '') return;
      if (pressed) held.add(event.code); else held.delete(event.code);
      session.send({ type: 'key', code: event.code, pressed });
    };
    const keyDown = key(true);
    const keyUp = key(false);
    const blur = () => {
      for (const code of held) session.send({ type: 'key', code, pressed: false });
      held.clear();
    };
    const menu = (event: Event) => event.preventDefault();
    canvas.addEventListener('pointermove', pointer);
    canvas.addEventListener('pointerdown', press);
    canvas.addEventListener('pointerup', release);
    canvas.addEventListener('wheel', wheel, { passive: false });
    canvas.addEventListener('keydown', keyDown);
    canvas.addEventListener('keyup', keyUp);
    canvas.addEventListener('blur', blur);
    canvas.addEventListener('contextmenu', menu);
    session.start();
    return () => {
      session.stop();
      canvas.removeEventListener('pointermove', pointer);
      canvas.removeEventListener('pointerdown', press);
      canvas.removeEventListener('pointerup', release);
      canvas.removeEventListener('wheel', wheel);
      canvas.removeEventListener('keydown', keyDown);
      canvas.removeEventListener('keyup', keyUp);
      canvas.removeEventListener('blur', blur);
      canvas.removeEventListener('contextmenu', menu);
    };
  }, [canvas, src, openSocket]);

  return { canvasRef: setCanvas, status: view.status, windowTitle: view.windowTitle };
}
