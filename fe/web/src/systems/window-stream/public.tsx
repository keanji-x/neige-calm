import { useEffect } from 'react';

import { useState } from '../../ui/state/public.ts';

import { buttonChanges, toWindowPoint, toWindowWheel, type WindowFrame } from '../../../../core/domain/window-stream.ts';
import {
  WindowStreamSession, windowStreamUrl, type WindowSocket, type WindowStreamStatus, type WindowStreamView,
} from './session.ts';

export type { WindowSocket, WindowStreamStatus } from './session.ts';

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
    const heldKeys = new Set<string>();
    let heldButtons = 0;
    /* Held input belongs to one session: when it ends there is no socket to release it on, and the
       next session must not hear a stale release. */
    const forget = () => { heldKeys.clear(); heldButtons = 0; };
    const session = new WindowStreamSession(windowStreamUrl(src, window.location), {
      openSocket: openSocket ?? ((url) => new WebSocket(url)),
      decode: decodeJpeg,
      draw: (bitmap, frame) => drawInto(canvas, bitmap, frame),
      view: (next) => {
        if (next.status !== 'live') forget();
        setView(next);
      },
    });
    const size = () => ({ width: canvas.width, height: canvas.height });
    const buttonsTo = (mask: number) => {
      for (const input of buttonChanges(heldButtons, mask)) session.send(input);
      heldButtons = mask;
    };
    const releaseButtons = () => buttonsTo(0);
    const releaseAll = () => {
      for (const code of heldKeys) session.send({ type: 'key', code, pressed: false });
      heldKeys.clear();
      releaseButtons();
    };
    const moveTo = (event: PointerEvent) => {
      const { x, y } = toWindowPoint(event.clientX, event.clientY, canvas.getBoundingClientRect(), size());
      session.send({ type: 'pointer', x, y });
    };
    /* The window follows the `buttons` mask only for a press the canvas owns: one that began with a
       `pointerdown` here. A drag that started elsewhere and crosses the canvas moves the pointer and
       presses nothing. A chord's second button arrives on `pointermove` while the first is held. */
    const pointer = (event: PointerEvent) => {
      if (!session.isLive()) return;
      moveTo(event);
      if (heldButtons !== 0) buttonsTo(event.buttons);
    };
    const press = (event: PointerEvent) => {
      canvas.focus();
      if (!session.isLive()) return;
      event.preventDefault();
      try { canvas.setPointerCapture(event.pointerId); } catch { /* the pointer is no longer active */ }
      moveTo(event);
      buttonsTo(event.buttons);
    };
    /* Without capture a press that leaves the canvas would stay held remotely with nothing to end it. */
    const leave = (event: PointerEvent) => {
      if (!canvas.hasPointerCapture(event.pointerId)) releaseButtons();
    };
    const wheel = (event: WheelEvent) => {
      if (!session.isLive()) return;
      event.preventDefault();
      const { dx, dy } = toWindowWheel(event, canvas.getBoundingClientRect(), size());
      session.send({ type: 'wheel', dx, dy });
    };
    /* Keys are taken only while live: a dead block must not trap Tab or swallow page keys. */
    const key = (pressed: boolean) => (event: KeyboardEvent) => {
      if (!session.isLive()) return;
      event.preventDefault();
      if (event.repeat || event.code === '') return;
      if (pressed) heldKeys.add(event.code); else heldKeys.delete(event.code);
      session.send({ type: 'key', code: event.code, pressed });
    };
    const keyDown = key(true);
    const keyUp = key(false);
    const menu = (event: Event) => { if (session.isLive()) event.preventDefault(); };
    const listeners: ReadonlyArray<readonly [string, EventListener, AddEventListenerOptions?]> = [
      ['pointermove', pointer as EventListener],
      ['pointerdown', press as EventListener],
      ['pointerup', pointer as EventListener],
      ['pointerleave', leave as EventListener],
      ['pointercancel', releaseButtons],
      ['lostpointercapture', releaseButtons],
      ['blur', releaseAll],
      ['wheel', wheel as EventListener, { passive: false }],
      ['keydown', keyDown as EventListener],
      ['keyup', keyUp as EventListener],
      ['contextmenu', menu],
    ];
    for (const [type, listener, options] of listeners) canvas.addEventListener(type, listener, options);
    session.start();
    return () => {
      session.stop();
      for (const [type, listener] of listeners) canvas.removeEventListener(type, listener);
    };
  }, [canvas, src, openSocket]);

  return { canvasRef: setCanvas, status: view.status, windowTitle: view.windowTitle };
}
