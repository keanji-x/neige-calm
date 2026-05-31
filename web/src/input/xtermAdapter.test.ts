import { describe, expect, it } from 'vitest';
import {
  shouldPassThroughToXterm,
  wheelDeltaToLines,
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

describe('wheelDeltaToLines', () => {
  it('converts pixel deltas to signed scroll lines', () => {
    const down = new WheelEvent('wheel', { deltaY: 120 });
    const up = new WheelEvent('wheel', { deltaY: -40 });

    expect(wheelDeltaToLines(down)).toBe(3);
    expect(wheelDeltaToLines(up)).toBe(-1);
  });

  it('preserves line-mode wheel direction', () => {
    const ev = new WheelEvent('wheel', {
      deltaY: -2,
      deltaMode: WheelEvent.DOM_DELTA_LINE,
    });

    expect(wheelDeltaToLines(ev)).toBe(-2);
  });
});
