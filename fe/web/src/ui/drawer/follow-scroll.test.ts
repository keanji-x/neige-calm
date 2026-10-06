import { fireEvent } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createScrollFollower, withScrollRestoration } from './follow-scroll.ts';

function mounted() {
  const pane = document.createElement('div');
  const content = document.createElement('div');
  pane.append(content);
  document.body.append(pane);
  let height = 1000;
  let top = 0;
  Object.defineProperties(pane, {
    scrollHeight: { get: () => height },
    clientHeight: { value: 400 },
    scrollTop: { get: () => top, set: (value: number) => { top = Math.max(0, Math.min(value, height - 400)); } },
  });
  const away = vi.fn();
  const follower = createScrollFollower({ bottomSlack: 64, onAwayChange: away });
  const detach = follower.attach(pane, content);
  return { pane, content, follower, away, detach, grow: () => { height += 100; } };
}

afterEach(() => { document.body.replaceChildren(); vi.useRealTimers(); });

describe('drawer scroll intent', () => {
  it('ignores position changes with no input provenance', () => {
    const x = mounted();
    x.pane.scrollTop = 100;
    fireEvent.scroll(x.pane);
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('releases before an upward input can race content growth, even inside bottom slack', () => {
    const x = mounted();
    fireEvent.wheel(x.pane, { deltaY: -2 });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(600);
    x.pane.scrollTop = 598;
    fireEvent.scroll(x.pane);
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(598);
    x.detach();
  });

  it.each([{ ctrlKey: true }, { metaKey: true }])('keeps zoom gestures separate from scrolling (%j)', (modifiers) => {
    const x = mounted();
    fireEvent.wheel(x.pane, { deltaY: -100, ...modifiers });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('does not consume a nested scrollport or a cancelled scroll default', () => {
    const x = mounted();
    const nested = document.createElement('div');
    nested.style.overflowY = 'auto';
    Object.defineProperties(nested, { scrollTop: { value: 100 }, scrollHeight: { value: 800 }, clientHeight: { value: 100 } });
    x.content.append(nested);
    fireEvent.wheel(nested, { deltaY: -100 });
    const cancelled = new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: -100 });
    cancelled.preventDefault();
    x.pane.dispatchEvent(cancelled);
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('respects contained scroll chains even when the child is at its edge', () => {
    const x = mounted();
    const nested = document.createElement('div');
    nested.style.cssText = 'overflow-y:auto;overscroll-behavior-y:contain';
    x.content.append(nested);
    fireEvent.wheel(nested, { deltaY: -100 });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('allows input to chain from a child that has no room left', () => {
    const x = mounted();
    const nested = document.createElement('div');
    nested.style.overflowY = 'auto';
    x.content.append(nested);
    fireEvent.wheel(nested, { deltaY: -100 });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(600);
    x.detach();
  });

  it('ignores editing keys and widget space activation, but accepts pane navigation', () => {
    const x = mounted();
    const input = document.createElement('textarea');
    const button = document.createElement('button');
    x.content.append(input, button);
    fireEvent.keyDown(input, { key: 'Home' });
    fireEvent.keyDown(button, { key: ' ' });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    fireEvent.keyDown(x.pane, { key: 'PageUp' });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('resumes only after a downward gesture reaches the tail and settles', () => {
    vi.useFakeTimers();
    const x = mounted();
    fireEvent.wheel(x.pane, { deltaY: -200 });
    x.pane.scrollTop = 400;
    fireEvent.scroll(x.pane);
    fireEvent.wheel(x.pane, { deltaY: 200 });
    x.pane.scrollTop = 600;
    fireEvent.scroll(x.pane);
    x.pane.dispatchEvent(new Event('scrollend'));
    vi.advanceTimersByTime(40);
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('coordinates nested restorations and keeps history released when geometry reaches the tail', () => {
    const x = mounted();
    x.follower.navigate(() => { x.pane.scrollTop = 100; });
    withScrollRestoration([x.pane], () => withScrollRestoration([x.pane], () => {
      x.pane.scrollTop = 600;
      fireEvent.scroll(x.pane);
    }));
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(600);
    x.follower.followToEnd();
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('defers a following write until restoration ends, including when restoration throws', () => {
    const x = mounted();
    expect(() => withScrollRestoration([x.pane], () => {
      x.grow();
      x.follower.followGrowth();
      expect(x.pane.scrollTop).toBe(600);
      throw new Error('layout failed');
    })).toThrow('layout failed');
    expect(x.pane.scrollTop).toBe(700);
    x.detach();
  });

  it('cancels pending input frames and removes all input listeners on detach', () => {
    vi.useFakeTimers();
    const x = mounted();
    fireEvent.wheel(x.pane, { deltaY: 100 });
    x.pane.dispatchEvent(new Event('scrollend'));
    x.detach();
    const calls = x.away.mock.calls.length;
    vi.advanceTimersByTime(100);
    fireEvent.wheel(x.pane, { deltaY: -100 });
    fireEvent.keyDown(x.pane, { key: 'PageUp' });
    fireEvent.scroll(x.pane);
    expect(x.away).toHaveBeenCalledTimes(calls);
    expect(x.pane.scrollTop).toBe(600);
  });

  it('does not release on a touch tap, but releases when a finger starts scrolling', () => {
    const x = mounted();
    fireEvent.touchStart(x.pane, { touches: [{ clientY: 100 }] });
    fireEvent.touchEnd(x.pane, { touches: [] });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    fireEvent.touchStart(x.pane, { touches: [{ clientY: 100 }] });
    fireEvent.touchMove(x.pane, { touches: [{ clientY: 120 }] });
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    fireEvent.touchCancel(x.pane, { touches: [] });
    x.detach();
  });

  it('tracks only its scrollbar pointer and keeps a non-scrolling click pinned', () => {
    vi.useFakeTimers();
    const x = mounted();
    fireEvent.pointerDown(x.pane, { button: 0, pointerType: 'mouse', pointerId: 1 });
    fireEvent.pointerUp(document, { pointerId: 1 });
    vi.advanceTimersByTime(40);
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(700);
    fireEvent.pointerDown(x.pane, { button: 0, pointerType: 'mouse', pointerId: 2 });
    fireEvent.pointerUp(document, { pointerId: 3 });
    x.pane.scrollTop = 400;
    fireEvent.scroll(x.pane);
    fireEvent.pointerUp(document, { pointerId: 2 });
    vi.advanceTimersByTime(40);
    x.grow();
    x.follower.followGrowth();
    expect(x.pane.scrollTop).toBe(400);
    x.detach();
  });

});
