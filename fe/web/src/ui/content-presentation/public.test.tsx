import { StrictMode, Suspense, use } from 'react';
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useContentAdmission } from './public.ts';

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
  const view = renderHook(({ id }) => useContentAdmission(id, true), { initialProps: { id: 'a' } });
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
  const view = renderHook(({ id }) => useContentAdmission(id, true), { initialProps });
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
  const view = renderHook(() => useContentAdmission('a', true), {
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
  const view = renderHook(({ available }) => useContentAdmission('a', available), {
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

it('cancels before the first callback when the selection closes', () => {
  const frames = animationFrames();
  const initialProps: { id: string | null } = { id: 'a' };
  const view = renderHook(({ id }) => useContentAdmission(id, true), { initialProps });
  expect(frames.pending.size).toBe(1);
  view.rerender({ id: null });
  expect(frames.pending.size).toBe(0);
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(false);
});

it('requires a fresh opportunity after an admitted selection loses eligibility', () => {
  const frames = animationFrames();
  const view = renderHook(({ eligible }) => useContentAdmission('a', eligible), {
    initialProps: { eligible: true },
  });
  frames.next();
  frames.next();
  expect(view.result.current).toBe(true);
  view.rerender({ eligible: false });
  expect(view.result.current).toBe(false);
  expect(frames.pending.size).toBe(0);
  view.rerender({ eligible: true });
  frames.next();
  expect(view.result.current).toBe(false);
  frames.next();
  expect(view.result.current).toBe(true);
});

it('keeps independent content regions in their own visits', () => {
  const frames = animationFrames();
  const first = renderHook(({ id }) => useContentAdmission(id, true), { initialProps: { id: 'a' } });
  const second = renderHook(() => useContentAdmission('a', true));
  frames.next();
  first.rerender({ id: 'b' });
  frames.next();
  expect(first.result.current).toBe(false);
  expect(second.result.current).toBe(true);
  frames.next();
  expect(first.result.current).toBe(true);
  first.unmount();
  expect(second.result.current).toBe(true);
  expect(frames.pending.size).toBe(0);
});

it('rejects an admission transition overtaken by a committed A→B→A replacement', async () => {
  const frames = animationFrames();
  const nextFrame = async () => {
    const callbacks = [...frames.pending.values()];
    frames.pending.clear();
    await act(async () => {
      for (const callback of callbacks) callback(0);
      await Promise.resolve();
    });
  };
  const parked = new Promise<never>(() => {});
  let suspended = false;
  const view = renderHook(({ id, hold }) => {
    const admitted = useContentAdmission(id, true);
    if (admitted && hold) { suspended = true; use(parked); }
    return admitted;
  }, {
    initialProps: { id: 'a', hold: true },
    wrapper: ({ children }) => <Suspense fallback={<p>Waiting</p>}>{children}</Suspense>,
  });
  await nextFrame();
  await nextFrame();
  expect(suspended).toBe(true);
  expect(view.result.current).toBe(false);
  await act(async () => { view.rerender({ id: 'b', hold: false }); await Promise.resolve(); });
  await act(async () => { view.rerender({ id: 'a', hold: false }); await Promise.resolve(); });
  expect(view.result.current).toBe(false);
  await nextFrame();
  expect(view.result.current).toBe(false);
  await nextFrame();
  expect(view.result.current).toBe(true);
});
