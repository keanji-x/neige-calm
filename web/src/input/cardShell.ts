import type { PointerEvent } from 'react';

const CARD_FOCUSABLE_SELECTOR =
  'input, textarea, select, button, a, [contenteditable="true"], [tabindex]:not([tabindex="-1"])';

export function handleWheelCardPointerDown(ev: PointerEvent<HTMLElement>) {
  const target = ev.target instanceof Element ? ev.target : null;
  if (!target) return;
  if (target.closest('.card-grid-close')) return;
  const nativeFocusable = target.closest(CARD_FOCUSABLE_SELECTOR);
  if (nativeFocusable && nativeFocusable !== ev.currentTarget) return;
  ev.currentTarget.focus({ preventScroll: true });
}
