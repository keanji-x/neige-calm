import { cleanup, render, screen } from '@testing-library/react';
import { ToastViewport, useToast } from '@astryxdesign/core/Toast';
import { useRef } from 'react';
import { commands, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { ConfirmDialog } from '../dialog/public.tsx';
import { PageHeader } from '../page-header/public.tsx';
import { readMotionTransition } from '../../../../tools/vitest/read-motion-transition.ts';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });
it('shares feedback timing on actual confirmation controls and page header chrome', async () => {
  const cancel = vi.fn();
  render(<><PageHeader title="Feedback" /><ConfirmDialog origin={{ kind: 'surface-center' }} open title="Confirm feedback" onConfirm={() => {}} onCancel={cancel} /></>);
  const button = screen.getByRole('button', { name: 'Cancel' });
  const feedback = readMotionTransition(button, 'feedback');
  const style = getComputedStyle(button);
  expect(style.transitionDuration.split(', ').every(value => parseFloat(value) === feedback.duration)).toBe(true);
  expect(style.transitionTimingFunction).toContain(`cubic-bezier(${feedback.ease.join(', ')})`);
  const header = document.querySelector<HTMLElement>('[data-nc-header-rows]')!;
  expect(parseFloat(getComputedStyle(header).transitionDuration)).toBe(feedback.duration);
  expect(getComputedStyle(header).transitionTimingFunction).toBe(`cubic-bezier(${feedback.ease.join(', ')})`);
  await userEvent.click(button);
  expect(cancel).toHaveBeenCalledOnce();
});
it('keeps confirmation actions immediate under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  const cancel = vi.fn();
  render(<ConfirmDialog origin={{ kind: 'surface-center' }} open title="Reduced feedback" onConfirm={() => {}} onCancel={cancel} />);
  const button = screen.getByRole('button', { name: 'Cancel' });
  expect(parseFloat(getComputedStyle(button).transitionDuration)).toBeLessThan(.001);
  await userEvent.click(button);
  expect(cancel).toHaveBeenCalledOnce();
});

it('keeps undeclared flow and pseudo-element transitions static under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  const view = render(<div data-testid="static-flow" style={{ width: 100, height: 40 }} />);
  const flow = screen.getByTestId('static-flow');
  for (const pseudo of [null, '::before', '::after']) {
    expect(getComputedStyle(flow, pseudo).transitionProperty).toBe('none');
  }
  expect(flow.getBoundingClientRect().width).toBe(100);
  view.rerender(<div data-testid="static-flow" style={{ width: 180, height: 40 }} />);
  expect(flow.getBoundingClientRect().width).toBe(180);
  expect(flow.getAnimations()).toHaveLength(0);
});

function ToastControls({ onHide }: { onHide: () => void }) {
  const toast = useToast();
  const dismiss = useRef(() => {});
  return <>
    <button type="button" onClick={() => {
      dismiss.current = toast({ body: <span data-testid="toast-body">Saved</span>, isAutoHide: false, onHide });
    }}>Show toast</button>
    <button type="button" onClick={() => dismiss.current()}>Dismiss toast</button>
  </>;
}

it('completes declared vendor toast dismissal under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  const onHide = vi.fn();
  render(<ToastViewport><ToastControls onHide={onHide} /></ToastViewport>);
  await userEvent.click(screen.getByRole('button', { name: 'Show toast' }));
  expect(screen.getByTestId('toast-body').getBoundingClientRect().height).toBeGreaterThan(0);
  await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  await expect.poll(() => document.getAnimations().length).toBe(0);
  const dismiss = screen.getByRole('button', { name: 'Dismiss toast' });
  await userEvent.click(dismiss);
  await expect.poll(() => screen.queryByTestId('toast-body')).toBeNull();
  expect(onHide).toHaveBeenCalledExactlyOnceWith('manual');
  expect(document.activeElement).toBe(dismiss);
});
