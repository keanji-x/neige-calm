import { afterEach, expect, it } from 'vitest';
import { readMotionTransition, readSizeTransition } from './transition.ts';

function surface() {
  const element = document.createElement('div');
  element.style.cssText = '--motion-medium:.24s;--motion-snappy:150ms;--motion-quick:100ms;--motion-slow:1s;--ease-enter:cubic-bezier(.16,1,.3,1);--ease-exit:cubic-bezier(.4,0,1,1);--ease-layout:cubic-bezier(.22,1,.36,1);--ease-feedback:cubic-bezier(.2,0,0,1);--ease-emphasis:cubic-bezier(.4,0,.2,1)';
  document.body.append(element);
  return element;
}
afterEach(() => { document.body.replaceChildren(); });

it('reads enter, exit and layout recipes from the owning surface', () => {
  const element = surface();
  expect(readMotionTransition(element, 'enter')).toEqual({ duration: .24, ease: [.16, 1, .3, 1] });
  expect(readMotionTransition(element, 'exit')).toEqual({ duration: .15, ease: [.4, 0, 1, 1] });
  expect(readMotionTransition(element, 'layout')).toEqual({ duration: .24, ease: [.22, 1, .36, 1] });
});

it('reads disclosure, feedback and emphasis from declared recipes', () => {
  const element = surface();
  expect(readMotionTransition(element, 'disclosure')).toEqual({ duration: .15, ease: [.22, 1, .36, 1] });
  expect(readMotionTransition(element, 'feedback')).toEqual({ duration: .1, ease: [.2, 0, 0, 1] });
  expect(readMotionTransition(element, 'emphasis')).toEqual({ duration: 1, ease: [.4, 0, .2, 1] });
});

it('honors local token overrides including millisecond durations', () => {
  const element = surface();
  element.style.setProperty('--motion-medium', '120ms');
  element.style.setProperty('--ease-layout', 'cubic-bezier(0, -1, 1, 2)');
  expect(readMotionTransition(element, 'layout')).toEqual({ duration: .12, ease: [0, -1, 1, 2] });
});

it.each(['', '250', '-1s', 'Infinitys'])('rejects an invalid duration %j instead of falling back', value => {
  const element = surface();
  element.style.setProperty('--motion-medium', value);
  expect(() => readMotionTransition(element, 'layout')).toThrow('Invalid motion tokens');
});

it.each(['', 'ease', 'cubic-bezier(0,1,1)', 'cubic-bezier(-1,0,1,1)', 'cubic-bezier(0,0,2,1)', 'cubic-bezier(0,NaN,1,1)', 'cubic-bezier(,1,1,1)', 'cubic-bezier(0,0x1,1,1)'])('rejects an invalid curve %j', value => {
  const element = surface();
  element.style.setProperty('--ease-layout', value);
  expect(() => readMotionTransition(element, 'layout')).toThrow('Invalid motion tokens');
});

it('uses a bounded size recipe that is symmetric and honors surface duration overrides', () => {
  const element = surface();
  expect(readMotionTransition(element, 'size').ease).toEqual([.4, 0, .2, 1]);
  expect(readSizeTransition(element, 40, 168).duration).toBe(.24);
  expect(readSizeTransition(element, 40, 48).duration).toBeCloseTo(.156);
  expect(readSizeTransition(element, 40, 1040).duration).toBe(.36);
  expect(readSizeTransition(element, 1040, 40)).toEqual(readSizeTransition(element, 40, 1040));
  expect(readSizeTransition(element, 40, 40).duration).toBe(0);
  element.style.setProperty('--motion-medium', '120ms');
  expect(readSizeTransition(element, 40, 168).duration).toBe(.12);
});

it.each([NaN, Infinity, -1])('rejects invalid measured size %s', invalid => {
  const element = surface();
  expect(() => readSizeTransition(element, invalid, 40)).toThrow('Invalid intrinsic motion size');
  expect(() => readSizeTransition(element, 40, invalid)).toThrow('Invalid intrinsic motion size');
});
