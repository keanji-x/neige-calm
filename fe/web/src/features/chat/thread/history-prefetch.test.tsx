// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useHistoryPrefetch, type HistoryPrefetch } from './history-prefetch.ts';

const frames = new Map<number, FrameRequestCallback>();
const idle = new Map<number, IdleRequestCallback>();
let sequence = 0;
const up = { source: 'wheel', direction: 'up', top: 0, height: 400 } as const;
function paints() {
  for (let i = 0; i < 2; i++) {
    const callbacks = [...frames.values()]; frames.clear();
    callbacks.forEach(callback => callback(0));
  }
}
function flush() {
  paints();
  const callbacks = [...idle.values()]; idle.clear();
  callbacks.forEach(callback => callback({ didTimeout: false, timeRemaining: () => 50 }));
}
function setup(initial: Partial<HistoryPrefetch> = {}) {
  const load = vi.fn(() => true);
  const state = { active: true, cursor: 100, busy: false, failed: false, load, ...initial };
  const hook = renderHook((props: HistoryPrefetch) => useHistoryPrefetch(props), { initialProps: state });
  return { ...hook, load, state };
}
beforeEach(() => {
  frames.clear(); idle.clear(); vi.useFakeTimers();
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id));
  vi.stubGlobal('requestIdleCallback', (callback: IdleRequestCallback) => { idle.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelIdleCallback', (id: number) => idle.delete(id));
});
afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); });

it('does nothing without input, including short content and idle busy transitions', () => {
  const { result, rerender, state, load } = setup();
  flush(); vi.advanceTimersByTime(1000); flush();
  rerender({ ...state, busy: true }); rerender(state); flush();
  expect(load).not.toHaveBeenCalled();
  act(() => result.current({ ...up, height: 0 })); flush();
  expect(load).toHaveBeenCalledTimes(1);
});

it('spends one cursor authorization even under repeated input and completion reentry', () => {
  const { result, rerender, state, load } = setup();
  for (let i = 0; i < 100; i++) result.current(up);
  flush(); expect(load).toHaveBeenCalledTimes(1);
  result.current(up); flush();
  rerender({ ...state, busy: true }); rerender({ ...state, cursor: 40 }); flush();
  expect(load).toHaveBeenCalledTimes(1);
  result.current(up); flush(); expect(load).toHaveBeenCalledTimes(2);
});

it('retains one unexpired input while newest is busy and resumes on idle without polling', () => {
  const { result, rerender, state, load } = setup({ busy: true });
  result.current(up); paints();
  expect(idle.size).toBe(0); expect(load).not.toHaveBeenCalled();
  act(() => { vi.advanceTimersByTime(100); });
  rerender({ ...state, busy: false }); flush();
  expect(load).toHaveBeenCalledTimes(1);
});

it('revokes pending input on inactivity, reverse, return, outside near top and hidden', () => {
  for (const cancel of ['stop', 'down', 'return', 'far', 'hidden'] as const) {
    const { result, rerender, state, load, unmount } = setup({ busy: true });
    result.current(up);
    if (cancel === 'stop') act(() => { vi.advanceTimersByTime(251); });
    if (cancel === 'down') result.current({ ...up, direction: 'down' });
    if (cancel === 'return') result.current({ ...up, direction: null });
    if (cancel === 'far') result.current({ ...up, top: 401 });
    if (cancel === 'hidden') rerender({ ...state, active: false });
    rerender({ ...state, busy: false }); flush();
    expect(load, cancel).not.toHaveBeenCalled(); unmount();
  }
});

it('rejects obsolete input across cursor changes, A B A visits and unmount', () => {
  const { result, rerender, state, load, unmount } = setup({ busy: true });
  result.current(up); const oldInput = result.current;
  rerender({ ...state, active: false }); rerender(state);
  result.current(up); const nextInput = result.current;
  rerender({ ...state, cursor: 40 });
  oldInput(up); nextInput(up);
  rerender({ ...state, cursor: 40, busy: false });
  expect(load).not.toHaveBeenCalled();
  const lastInput = result.current; unmount(); lastInput(up);
  expect(load).not.toHaveBeenCalled();
});

it('pauses after exhausted errors and does not create input when retry clears them', () => {
  const { result, rerender, state, load } = setup({ busy: true });
  result.current(up); rerender({ ...state, failed: true, busy: false });
  rerender({ ...state, busy: false }); flush();
  expect(load).not.toHaveBeenCalled();
  result.current(up); flush(); expect(load).toHaveBeenCalledTimes(1);
});

it('accepts the declared upward keys while excluding widget space navigation', () => {
  for (const key of ['PageUp', 'Home', 'ArrowUp', ' ']) {
    const { result, load, unmount } = setup();
    result.current({ ...up, source: 'keyboard', key }); flush();
    expect(load).toHaveBeenCalledTimes(key === ' ' ? 0 : 1); unmount();
  }
});

it('starts an idle query on input without waiting for paint or idle capacity', () => {
  const { result, load } = setup();
  result.current(up);
  expect(load).toHaveBeenCalledTimes(1);
  vi.advanceTimersByTime(251);
  flush();
  expect(load).toHaveBeenCalledTimes(1);
});
