import { afterEach, expect, it, vi } from 'vitest';
import { readContentSelection, sameContentSelection } from './selection.ts';

afterEach(() => { vi.restoreAllMocks(); document.body.replaceChildren(); });

function mount() {
  const content = document.createElement('div');
  const first = document.createTextNode('First passage');
  const second = document.createTextNode('Second passage');
  const outside = document.createTextNode('Outside');
  content.append(first, second); document.body.append(content, outside);
  const range = (node: Node, start: number, end: number) => {
    const value = document.createRange(); value.setStart(node, start); value.setEnd(node, end); return value;
  };
  const ranges = [range(first, 1, 4), range(second, 2, 6), range(outside, 0, 4)];
  // Chromium exposes one range. Supply only Selection's native range facts here
  // to exercise the reader's multi-range contract with actual DOM Ranges.
  vi.spyOn(document, 'getSelection').mockReturnValue({
    isCollapsed: false, rangeCount: ranges.length, getRangeAt: (index: number) => ranges[index],
  } as Selection);
  return { content, first, ranges };
}

it('compares every local range independently of range order and ignores outside ranges', () => {
  const { content, ranges } = mount();
  const before = readContentSelection(content); expect(before).toHaveLength(2);
  ranges.reverse();
  const reordered = readContentSelection(content);
  expect(sameContentSelection(content, before, reordered)).toBe(true);
  ranges[1].setEnd(ranges[1].endContainer, 7);
  expect(sameContentSelection(content, before, readContentSelection(content))).toBe(false);
});

it('copies boundary offsets instead of retaining the live native range', () => {
  const { content, first, ranges } = mount();
  const before = readContentSelection(content);
  ranges[0].setEnd(first, 5);
  expect(sameContentSelection(content, before, readContentSelection(content))).toBe(false);
  ranges[0].setEnd(first, 4);
  expect(sameContentSelection(content, before, readContentSelection(content))).toBe(true);
});
