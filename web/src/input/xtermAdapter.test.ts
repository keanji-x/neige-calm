import { describe, expect, it, vi } from 'vitest';
import {
  createXtermWheelTarget,
  deltaYToLines,
  shouldPassThroughToXterm,
  type XtermScrollTerminal,
  type XtermWheelState,
} from './xtermAdapter';

function termState(
  mouseTrackingMode: string | undefined,
  bufferType: string | undefined,
): XtermWheelState {
  return {
    modes:
      mouseTrackingMode === undefined ? undefined : { mouseTrackingMode },
    buffer:
      bufferType === undefined ? undefined : { active: { type: bufferType } },
  };
}

describe('shouldPassThroughToXterm', () => {
  it('passes wheel events through when mouse reporting is enabled', () => {
    expect(shouldPassThroughToXterm(termState('any', 'normal'))).toBe(true);
  });

  it('passes wheel events through when the alternate buffer is active', () => {
    expect(shouldPassThroughToXterm(termState('none', 'alternate'))).toBe(true);
  });

  it('uses scrollback when mouse reporting is off and the normal buffer is active', () => {
    expect(shouldPassThroughToXterm(termState('none', 'normal'))).toBe(false);
  });

  it('falls back to scrollback when optional xterm state is absent', () => {
    expect(shouldPassThroughToXterm({})).toBe(false);
  });
});

describe('deltaYToLines', () => {
  it('converts pixel deltas to signed scroll lines', () => {
    expect(deltaYToLines(120, WheelEvent.DOM_DELTA_PIXEL)).toBe(8);
    expect(deltaYToLines(-16, WheelEvent.DOM_DELTA_PIXEL)).toBe(-1);
  });

  it('converts line-mode deltas directly to lines', () => {
    expect(deltaYToLines(-2, WheelEvent.DOM_DELTA_LINE)).toBe(-2);
    expect(deltaYToLines(0.2, WheelEvent.DOM_DELTA_LINE)).toBe(1);
  });

  it('converts page-mode deltas to larger line chunks', () => {
    expect(deltaYToLines(1, WheelEvent.DOM_DELTA_PAGE)).toBe(10);
    expect(deltaYToLines(-2, WheelEvent.DOM_DELTA_PAGE)).toBe(-20);
  });
});

describe('createXtermWheelTarget', () => {
  function terminal(
    state: XtermWheelState = {},
    scrollLines: (amount: number) => void = () => undefined,
  ): XtermScrollTerminal {
    return {
      ...state,
      scrollLines,
    };
  }

  it("decides pass with reason 'no-terminal' when terminal handle is absent", () => {
    const scrollLines = vi.fn();
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: { current: null },
    });

    expect(target.decide(120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'no-terminal',
    });
    expect(() => target.apply(120, WheelEvent.DOM_DELTA_PIXEL)).not.toThrow();
    expect(scrollLines).not.toHaveBeenCalled();
  });

  it("decides pass with reason 'edge' at top for wheel up", () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          buffer: { active: { type: 'normal', viewportY: 0, baseY: 8 } },
        }),
      },
    });

    expect(target.decide(-120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'edge',
    });
  });

  it("decides pass with reason 'edge' at bottom for wheel down", () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          buffer: { active: { type: 'normal', viewportY: 8, baseY: 8 } },
        }),
      },
    });

    expect(target.decide(120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'edge',
    });
  });

  it("decides pass with reason 'edge' for zero delta", () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          buffer: { active: { type: 'normal', viewportY: 3, baseY: 8 } },
        }),
      },
    });

    expect(target.decide(0, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'edge',
    });
  });

  it("decides pass with reason 'edge' for wheel down on empty buffer (viewportY===baseY===0)", () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          buffer: { active: { type: 'normal', viewportY: 0, baseY: 0 } },
        }),
      },
    });

    expect(target.decide(120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'edge',
    });
  });

  it("decides pass with reason 'passthrough' when mouse reporting is enabled", () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({ modes: { mouseTrackingMode: 'any' } }),
      },
    });

    expect(target.decide(120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'passthrough',
    });
  });

  it("decides pass with reason 'passthrough' when alternate buffer is active", () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          buffer: { active: { type: 'alternate', viewportY: 3, baseY: 8 } },
        }),
      },
    });

    expect(target.decide(120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'pass',
      reason: 'passthrough',
    });
  });

  it('decides consume in normal buffer mid-state', () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          modes: { mouseTrackingMode: 'none' },
          buffer: { active: { type: 'normal', viewportY: 3, baseY: 8 } },
        }),
      },
    });

    expect(target.decide(120, WheelEvent.DOM_DELTA_PIXEL)).toEqual({
      kind: 'consume',
    });
  });

  it('apply scrolls converted lines', () => {
    const scrollLines = vi.fn();
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal(
          { buffer: { active: { type: 'normal', viewportY: 3, baseY: 8 } } },
          scrollLines,
        ),
      },
    });

    target.apply(120, WheelEvent.DOM_DELTA_PIXEL);

    expect(scrollLines).toHaveBeenCalledWith(8);
  });
});
