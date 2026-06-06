import type { XtermWheelTarget } from './xtermAdapter';

// Fallback routing for xterm shells that are not registry cards. Built-in
// cards declare `wheelTarget` on their entries; Today's bespoke terminal
// panel still uses this WeakMap because it has no card id.
const xtermRegistry = new WeakMap<HTMLElement, XtermWheelTarget>();

export function registerXtermShell(
  shell: HTMLElement,
  target: XtermWheelTarget,
): void {
  xtermRegistry.set(shell, target);
}

export function unregisterXtermShell(shell: HTMLElement): void {
  xtermRegistry.delete(shell);
}

export function getXtermForShell(
  shell: HTMLElement,
): XtermWheelTarget | undefined {
  return xtermRegistry.get(shell);
}
