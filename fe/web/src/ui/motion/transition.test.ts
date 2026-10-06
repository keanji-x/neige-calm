import { afterEach, expect, it } from 'vitest';
import { readMotionTransition } from './transition.ts';

function surface() {
  const element = document.createElement('div');
  element.style.cssText = '--motion-medium:.24s;--motion-snappy:150ms;--ease-enter:cubic-bezier(.16,1,.3,1);--ease-exit:cubic-bezier(.4,0,1,1);--ease-layout:cubic-bezier(.22,1,.36,1)';
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
