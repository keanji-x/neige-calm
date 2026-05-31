import type { XtermWheelTarget } from './xtermAdapter';

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
