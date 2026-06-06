export type WheelDecision =
  | { kind: 'consume' }
  | { kind: 'pass'; reason: 'edge' | 'passthrough' };

export interface XtermWheelTarget {
  root: HTMLElement;
  decide(deltaY: number, deltaMode: number): WheelDecision;
  apply(deltaY: number, deltaMode: number): void;
}

export interface XtermWheelState {
  modes?: {
    mouseTrackingMode?: string;
  };
  buffer?: {
    active?: {
      type?: string;
      viewportY?: number;
      baseY?: number;
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
    decide: (deltaY, deltaMode) => {
      const term = terminalRef.current;
      if (!term) return { kind: 'consume' };
      if (shouldPassThroughToXterm(term)) {
        return { kind: 'pass', reason: 'passthrough' };
      }

      const lines = deltaYToLines(deltaY, deltaMode);
      if (lines === 0) return { kind: 'pass', reason: 'edge' };

      const active = term.buffer?.active;
      const viewportY = active?.viewportY;
      const baseY = active?.baseY;
      if (lines < 0 && typeof viewportY === 'number' && viewportY === 0) {
        return { kind: 'pass', reason: 'edge' };
      }
      if (
        lines > 0 &&
        typeof viewportY === 'number' &&
        typeof baseY === 'number' &&
        viewportY === baseY
      ) {
        return { kind: 'pass', reason: 'edge' };
      }

      return { kind: 'consume' };
    },
    apply: (deltaY, deltaMode) => {
      const term = terminalRef.current;
      if (!term || shouldPassThroughToXterm(term)) return;
      const lines = deltaYToLines(deltaY, deltaMode);
      if (lines !== 0) term.scrollLines(lines);
    },
  };
}
