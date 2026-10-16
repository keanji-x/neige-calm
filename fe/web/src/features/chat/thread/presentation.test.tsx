import { StrictMode } from 'react';
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useThreadPresentation } from './presentation.ts';

function animationFrames() {
  const pending = new Map<number, FrameRequestCallback>();
  let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    pending.set(++sequence, callback);
    return sequence;
  });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { pending.delete(id); });
  return {
    pending,
    next: () => {
      const callbacks = [...pending.values()];
      pending.clear();
      act(() => { for (const callback of callbacks) callback(0); });
    },
  };
}

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it('leaves a rendering opportunity before mounting, and keeps same-target updates immediate', () => {
  const frames = animationFrames();
  const view = renderHook(({ id }) => useThreadPresentation(id, true), { initialProps: { id: 'a' } });
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(true);
  view.rerender({ id: 'a' });
  expect(view.result.current).toBe(true);
  expect(frames.pending.size).toBe(0);
});

it('rejects a late A callback after A→B→A, and cancels a closing visit', () => {
  const frames = animationFrames();
  const initialProps: { id: string | null } = { id: 'a' };
  const view = renderHook(({ id }) => useThreadPresentation(id, true), { initialProps });
  frames.next();
  const abandoned = [...frames.pending.values()][0];
  view.rerender({ id: 'b' });
  view.rerender({ id: 'a' });
  act(() => { abandoned(0); });
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(true);
  view.rerender({ id: 'b' });
  frames.next();
  view.rerender({ id: null });
  expect(frames.pending.size).toBe(0);
  expect(view.result.current).toBe(false);
});

it('owns and releases its callbacks under StrictMode', () => {
  const frames = animationFrames();
  const view = renderHook(() => useThreadPresentation('a', true), {
    wrapper: ({ children }) => <StrictMode>{children}</StrictMode>,
  });
  expect(frames.pending.size).toBe(1);
  frames.next();
  expect(frames.pending.size).toBe(1);
  view.unmount();
  expect(frames.pending.size).toBe(0);
});

it('starts the rendering boundary when late content becomes available', () => {
  const frames = animationFrames();
  const view = renderHook(({ available }) => useThreadPresentation('a', available), {
    initialProps: { available: false },
  });
  frames.next();
  frames.next();
  expect(frames.pending.size).toBe(0);
  expect(view.result.current).toBe(false);
  view.rerender({ available: true });
  frames.next();
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(true);
});
