import { describe, expect, it } from 'vitest';
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
  function terminal(state: XtermWheelState = {}): XtermScrollTerminal {
    return {
      ...state,
      scrollLines: () => undefined,
    };
  }

  it('reports passthrough mode when mouse reporting is enabled', () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({ modes: { mouseTrackingMode: 'any' } }),
      },
    });

    expect(target.mode()).toBe('passthrough');
  });

  it('reports passthrough mode when the alternate buffer is active', () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({ buffer: { active: { type: 'alternate' } } }),
      },
    });

    expect(target.mode()).toBe('passthrough');
  });

  it('reports scrollback mode otherwise', () => {
    const target = createXtermWheelTarget({
      root: document.createElement('div'),
      terminalRef: {
        current: terminal({
          modes: { mouseTrackingMode: 'none' },
          buffer: { active: { type: 'normal' } },
        }),
      },
    });

    expect(target.mode()).toBe('scrollback');
  });
});
