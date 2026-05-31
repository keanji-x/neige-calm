export interface XtermWheelTarget {
  root: HTMLElement;
  mode(): 'scrollback' | 'passthrough';
  scrollback(deltaY: number, deltaMode: number): void;
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

export function shouldPassThroughToXterm(term: XtermWheelState): boolean {
  return (
    term.modes?.mouseTrackingMode !== undefined &&
    term.modes.mouseTrackingMode !== 'none'
  ) || term.buffer?.active?.type === 'alternate';
}

export function deltaYToLines(deltaY: number, deltaMode: number): number {
  if (deltaY === 0) return 0;
  if (deltaMode === WheelEvent.DOM_DELTA_LINE) {
    return Math.sign(deltaY) * Math.max(1, Math.round(Math.abs(deltaY)));
  }
  if (deltaMode === WheelEvent.DOM_DELTA_PAGE) {
    return Math.sign(deltaY) * Math.max(1, Math.round(Math.abs(deltaY) * 10));
  }
  return Math.sign(deltaY) * Math.max(1, Math.round(Math.abs(deltaY) / 16));
}

export function createXtermWheelTarget(args: {
  root: HTMLElement;
  terminalRef: { current: XtermScrollTerminal | null };
}): XtermWheelTarget {
  const { root, terminalRef } = args;
  return {
    root,
    mode: () => {
      const term = terminalRef.current;
      return term && shouldPassThroughToXterm(term)
        ? 'passthrough'
        : 'scrollback';
    },
    scrollback: (deltaY, deltaMode) => {
      const term = terminalRef.current;
      if (!term) return;
      const lines = deltaYToLines(deltaY, deltaMode);
      if (lines !== 0) term.scrollLines(lines);
    },
  };
}
