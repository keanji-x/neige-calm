// @vitest-environment jsdom
import { act, cleanup, render, renderHook, screen } from '@testing-library/react';
import { Suspense, startTransition, use } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { useState } from './public.ts';
import { useCommittedCallback } from './committed-callback.ts';

afterEach(cleanup);

it('keeps event identity while invoking the newest committed handler and return value', () => {
  const first = vi.fn((value: number) => value + 1);
  const next = vi.fn((value: number) => value + 2);
  const { result, rerender } = renderHook(({ handler }) => useCommittedCallback('A', handler),
    { initialProps: { handler: first } });
  const event = result.current;
  expect(event(3)).toBe(4);
  rerender({ handler: next });
  expect(result.current).toBe(event);
  expect(event(3)).toBe(5);
  expect(first).toHaveBeenCalledTimes(1);
  expect(next).toHaveBeenCalledWith(3);
});

it('does not retarget old view events through A-B-A', () => {
  const { result, rerender } = renderHook(({ view, text }) => useCommittedCallback(view, () => text),
    { initialProps: { view: 'A', text: 'first A' } });
  const firstA = result.current;
  rerender({ view: 'B', text: 'B only' });
  const b = result.current;
  expect(b).not.toBe(firstA);
  expect(firstA()).toBe('first A');
  rerender({ view: 'A', text: 'new A' });
  expect(result.current).not.toBe(firstA);
  expect(result.current()).toBe('new A');
  expect(firstA()).toBe('first A');
  expect(b()).toBe('B only');
});

it('does not publish an abandoned render as the current event handler', async () => {
  const parked = new Promise<never>(() => {});
  const called: string[] = [];
  let suspend = () => {};
  function View() {
    const [version, setVersion] = useState('committed');
    suspend = () => { startTransition(() => { setVersion('uncommitted'); }); };
    const event = useCommittedCallback('A', () => { called.push(version); });
    if (version === 'uncommitted') use(parked);
    return <button type="button" onClick={event}>Act</button>;
  }
  render(<Suspense fallback={<p>Waiting</p>}><View /></Suspense>);
  await act(async () => { suspend(); await Promise.resolve(); });
  act(() => { screen.getByRole('button', { name: 'Act' }).click(); });
  expect(called).toEqual(['committed']);
});
