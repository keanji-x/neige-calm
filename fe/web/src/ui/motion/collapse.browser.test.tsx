import '../../styles/entry.css';
import { cleanup, render, waitFor } from '@testing-library/react';
import { commands } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { CollapsePresence } from './collapse.tsx';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

it('reverses a departing flow item without scaling its text and cancels under reduced motion', async () => {
  const exited = vi.fn();
  const child = <div style={{ height: 120 }}><button>Keep the same item</button></div>;
  const view = render(<CollapsePresence present onExited={exited}>{child}</CollapsePresence>);
  const host = document.querySelector<HTMLElement>('[data-nc-collapse-presence]')!;
  const button = host.querySelector('button')!;
  await waitFor(() => expect(host.getAnimations()).toHaveLength(0));
  const full = host.getBoundingClientRect().height;
  view.rerender(<CollapsePresence present={false} onExited={exited}>{child}</CollapsePresence>);
  await waitFor(() => expect(host.getBoundingClientRect().height).toBeLessThan(full - 5));
  const painted = host.getBoundingClientRect().height;
  view.rerender(<CollapsePresence present onExited={exited}>{child}</CollapsePresence>);
  expect(Math.abs(host.getBoundingClientRect().height - painted)).toBeLessThan(full / 2);
  expect(host.querySelector('button')).toBe(button);
  expect(getComputedStyle(button).transform).toBe('none');
  await waitFor(() => expect(host.getAnimations()).toHaveLength(0));
  expect(host.getBoundingClientRect().height).toBeCloseTo(full, 0);
  expect(exited).not.toHaveBeenCalled();
  view.rerender(<CollapsePresence present={false} onExited={exited}>{child}</CollapsePresence>);
  await commands.emulateReducedMotion(true);
  await waitFor(() => expect(exited).toHaveBeenCalledTimes(1));
  expect(host.getAnimations()).toHaveLength(0);
  expect(host.getBoundingClientRect().height).toBeCloseTo(0, 0);
  expect(host.inert).toBe(true);
});

it('keeps an already visible message fully painted on admission', () => {
  render(<CollapsePresence present onExited={() => undefined}><div style={{ height: 120 }}>Confirmed message</div></CollapsePresence>);
  const host = document.querySelector<HTMLElement>('[data-nc-collapse-presence]')!;
  expect(host.getAnimations()).toHaveLength(0);
  expect(host.getBoundingClientRect().height).toBe(120);
  expect(getComputedStyle(host).opacity).toBe('1');
});
