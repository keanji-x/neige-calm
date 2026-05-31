export interface XtermWheelTarget {
  root: HTMLElement;
  canHandleWheel(): boolean;
  routeWheel(ev: WheelEvent): void;
}

export interface XtermWheelState {
  modes?: {
    mouseTrackingMode?: string;
  };
  buffer?: {
    active?: {
      type?: string;
    };
  };
}

export interface XtermScrollTerminal extends XtermWheelState {
  scrollLines(amount: number): void;
}

const syntheticWheels = new WeakSet<WheelEvent>();

export function markSyntheticWheel(ev: WheelEvent): void {
  syntheticWheels.add(ev);
}

export function isSyntheticWheel(ev: WheelEvent): boolean {
  return syntheticWheels.has(ev);
}

export function shouldPassThroughToXterm(term: XtermWheelState): boolean {
  return (
    term.modes?.mouseTrackingMode !== undefined &&
    term.modes.mouseTrackingMode !== 'none'
  ) || term.buffer?.active?.type === 'alternate';
}

export function wheelDeltaToLines(ev: WheelEvent): number {
  const unit =
    ev.deltaMode === WheelEvent.DOM_DELTA_LINE
      ? 1
      : ev.deltaMode === WheelEvent.DOM_DELTA_PAGE
        ? 24
        : 1 / 40;
  const raw = ev.deltaY * unit;
  if (raw === 0) return 0;
  const lines = Math.sign(raw) * Math.max(1, Math.round(Math.abs(raw)));
  return Math.max(-120, Math.min(120, lines));
}

export function cloneWheelEvent(ev: WheelEvent, view: Window | null): WheelEvent {
  return new WheelEvent('wheel', {
    bubbles: true,
    cancelable: true,
    composed: true,
    view: ev.view ?? view ?? undefined,
    detail: ev.detail,
    screenX: ev.screenX,
    screenY: ev.screenY,
    clientX: ev.clientX,
    clientY: ev.clientY,
    ctrlKey: ev.ctrlKey,
    shiftKey: ev.shiftKey,
    altKey: ev.altKey,
    metaKey: ev.metaKey,
    button: ev.button,
    buttons: ev.buttons,
    relatedTarget: ev.relatedTarget,
    deltaX: ev.deltaX,
    deltaY: ev.deltaY,
    deltaZ: ev.deltaZ,
    deltaMode: ev.deltaMode,
  });
}

export function createXtermWheelTarget(args: {
  root: HTMLElement;
  terminalRef: { current: XtermScrollTerminal | null };
}): XtermWheelTarget {
  const { root, terminalRef } = args;
  return {
    root,
    canHandleWheel: () => true,
    routeWheel: (ev) => {
      const term = terminalRef.current;
      if (!term) return;
      if (shouldPassThroughToXterm(term)) {
        const cloned = cloneWheelEvent(ev, root.ownerDocument.defaultView);
        markSyntheticWheel(cloned);
        const dispatchTarget =
          root.querySelector<HTMLElement>('.xterm-screen') ?? root;
        dispatchTarget.dispatchEvent(cloned);
        return;
      }
      const lines = wheelDeltaToLines(ev);
      if (lines !== 0) term.scrollLines(lines);
    },
  };
}
