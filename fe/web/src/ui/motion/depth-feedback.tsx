import { useEffect } from 'react';
import { pressDepthFrame, TAP_VELOCITY } from './depth.ts';
import { playSpring, type SpringPlayback } from './spring.ts';

type Press = { element: HTMLElement; playback: SpringPlayback; sources: Set<string> };
type PointerPress = { id: number; x: number; y: number; press: Press };

/** One app-installed semantic adapter. Delegated events include newly mounted portals.
 * Owns effects only: never wraps content, changes layout styles, captures pointers or delays actions.
 */
export function installDepthFeedback(document: Document): () => void {
  const window = document.defaultView!;
  const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
  const presses = new Map<HTMLElement, Press>();
  let pointer: PointerPress | null = null;
  const keyboard = new Map<string, Press>();
  let frame = 0;
  const enabled = (element: HTMLElement) => element.isConnected
    && !element.matches(':disabled, [aria-disabled="true"]')
    && element.closest('[inert], [aria-disabled="true"]') === null;
  const control = (target: EventTarget | null) => {
    if (!(target instanceof Element) || target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return null;
    const element = target.closest<HTMLElement>('button, [role="button"]');
    return element && enabled(element) ? element : null;
  };
  const discard = (press: Press) => {
    press.playback.cancel();
    if (presses.get(press.element) === press) presses.delete(press.element);
    if (pointer?.press === press) pointer = null;
    for (const [key, owner] of keyboard) if (owner === press) keyboard.delete(key);
  };
  const monitor = () => {
    frame = 0;
    for (const press of presses.values()) if (!enabled(press.element)) discard(press);
    if (presses.size) frame = window.requestAnimationFrame(monitor);
  };
  const move = (element: HTMLElement, held: boolean): Press => {
    const previous = presses.get(element);
    const state = previous?.playback.sample() ?? { value: 0, velocity: held ? TAP_VELOCITY : 0 };
    previous?.playback.cancel();
    const playback = playSpring([element], state.value, held ? 1 : 0,
      state.velocity, pressDepthFrame);
    const press = previous ?? { element, playback, sources: new Set<string>() };
    press.playback = playback;
    presses.set(element, press);
    if (!frame) frame = window.requestAnimationFrame(monitor);
    void playback.finished.then(() => {
      if (presses.get(element) === press && press.playback === playback && press.sources.size === 0) discard(press);
    }, () => { /* A retarget or disposal owns the replacement. */ });
    return press;
  };
  const acquire = (element: HTMLElement, source: string) => {
    const previous = presses.get(element);
    const press = previous?.sources.size ? previous : move(element, true);
    press.sources.add(source);
    return press;
  };
  const release = (press: Press, source: string) => {
    press.sources.delete(source);
    if (press.sources.size) return;
    if (presses.get(press.element) !== press) return;
    if (preference.matches || !enabled(press.element)) { discard(press); return; }
    move(press.element, false);
  };
  const reset = () => {
    for (const press of presses.values()) discard(press);
    pointer = null; keyboard.clear();
    window.cancelAnimationFrame(frame); frame = 0;
  };
  const down = (event: PointerEvent) => {
    if (preference.matches || !event.isPrimary || event.button !== 0 || pointer) return;
    const element = control(event.target);
    if (!element) return;
    pointer = { id: event.pointerId, x: event.clientX, y: event.clientY, press: acquire(element, 'pointer') };
  };
  const end = (event: PointerEvent) => {
    if (pointer?.id !== event.pointerId) return;
    const { press } = pointer; pointer = null;
    release(press, 'pointer');
  };
  const leave = (event: PointerEvent) => {
    if (pointer?.id !== event.pointerId) return;
    if (event.relatedTarget instanceof Node && pointer.press.element.contains(event.relatedTarget)) return;
    end(event);
  };
  const drag = () => {
    if (!pointer) return;
    const { press } = pointer; pointer = null;
    if (press.sources.size > 1) release(press, 'pointer'); else discard(press);
  };
  const pointerMove = (event: PointerEvent) => {
    // Touch pans belong to scrolling; never capture or prevent their default behavior.
    if (pointer?.id === event.pointerId && event.pointerType === 'touch'
      && Math.hypot(event.clientX - pointer.x, event.clientY - pointer.y) > 8) drag();
  };
  const keyDown = (event: KeyboardEvent) => {
    if (preference.matches || event.repeat || keyboard.has(event.key) || (event.key !== ' ' && event.key !== 'Enter')) return;
    const element = control(event.target);
    if (element) keyboard.set(event.key, acquire(element, event.key));
  };
  const keyUp = (event: KeyboardEvent) => {
    const press = keyboard.get(event.key);
    if (!press) return;
    keyboard.delete(event.key); release(press, event.key);
  };
  const focusOut = (event: FocusEvent) => {
    for (const [key, press] of keyboard) {
      if (!(event.target instanceof Node) || !press.element.contains(event.target)) continue;
      if (event.relatedTarget instanceof Node && press.element.contains(event.relatedTarget)) continue;
      keyboard.delete(key); release(press, key);
    }
  };
  const reduced = () => { if (preference.matches) reset(); };
  document.addEventListener('pointerdown', down, true);
  document.addEventListener('pointerout', leave, true);
  document.addEventListener('pointermove', pointerMove, true);
  document.addEventListener('keydown', keyDown, true);
  document.addEventListener('keyup', keyUp, true);
  document.addEventListener('focusout', focusOut, true);
  document.addEventListener('dragstart', drag, true);
  window.addEventListener('pointerup', end, true);
  window.addEventListener('pointercancel', end, true);
  document.addEventListener('lostpointercapture', end, true);
  window.addEventListener('blur', reset);
  preference.addEventListener('change', reduced);
  return () => {
    document.removeEventListener('pointerdown', down, true);
    document.removeEventListener('pointerout', leave, true);
    document.removeEventListener('pointermove', pointerMove, true);
    document.removeEventListener('keydown', keyDown, true);
    document.removeEventListener('keyup', keyUp, true);
    document.removeEventListener('focusout', focusOut, true);
    document.removeEventListener('dragstart', drag, true);
    window.removeEventListener('pointerup', end, true);
    window.removeEventListener('pointercancel', end, true);
    document.removeEventListener('lostpointercapture', end, true);
    window.removeEventListener('blur', reset);
    preference.removeEventListener('change', reduced);
    reset();
  };
}

export function DepthFeedback() {
  useEffect(() => installDepthFeedback(document), []);
  return null;
}
