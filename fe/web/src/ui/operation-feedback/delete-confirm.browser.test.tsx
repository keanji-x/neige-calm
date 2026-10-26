import { act, cleanup, render, screen } from '@testing-library/react';
import { useRef } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import '../../styles/entry.css';
import { DeleteConfirmDialog, useDeleteConfirm } from './public.tsx';

afterEach(cleanup);
it('carries delete identity and explicit origin together while Cancel aborts without waiting for paint', async () => {
  let finish!: () => void;
  let signal: AbortSignal | undefined;
  const done = vi.fn();
  const perform = vi.fn((_id: string, value: AbortSignal) => {
    signal = value;
    return new Promise<void>(resolve => { finish = resolve; });
  });
  function Entry() {
    const ref = useRef<HTMLSpanElement | null>(null);
    const deletion = useDeleteConfirm(perform, () => 'Delete failed', done);
    return <><span ref={ref} style={{ display: 'inline-flex', margin: 30 }}>
      <button onClick={() => deletion.request('owned-id', { kind: 'anchor', anchor: { ref }, unavailable: 'surface-center' })}>Delete item</button>
    </span><DeleteConfirmDialog confirm={deletion} copy={{ title: 'Delete owned item', confirmLabel: 'Delete' }} /></>;
  }
  render(<Entry />);
  const opener = screen.getByRole('button', { name: 'Delete item' });
  await page.getByRole('button', { name: 'Delete item' }).click();
  const panel = screen.getByRole('dialog');
  const animation = panel.getAnimations()[0]; await animation.ready;
  animation.pause(); animation.currentTime = 0;
  await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  const frame = panel.parentElement!.getBoundingClientRect();
  const source = opener.parentElement!.getBoundingClientRect();
  const x = source.left + source.width / 2; const y = source.top + source.height / 2;
  const point = new DOMPoint(x - frame.left, y - frame.top).matrixTransform(new DOMMatrix(getComputedStyle(panel).transform));
  expect(frame.left + point.x).toBeCloseTo(x, 3);
  expect(frame.top + point.y).toBeCloseTo(y + .56, 3);
  animation.play();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect.poll(() => perform.mock.calls.length).toBe(1);
  expect(perform.mock.calls[0][0]).toBe('owned-id');
  expect(signal?.aborted).toBe(false);
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  expect(signal?.aborted).toBe(true);
  expect(panel.isConnected).toBe(false);
  expect(panel.getAnimations()).toHaveLength(0);
  await act(async () => { finish(); await Promise.resolve(); });
  expect(done).not.toHaveBeenCalled();
  await expect.element(page.getByRole('button', { name: 'Delete item' })).toHaveFocus();
});
