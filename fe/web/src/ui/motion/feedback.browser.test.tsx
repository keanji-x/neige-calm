import { cleanup, render, screen } from '@testing-library/react';
import { commands, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { ConfirmDialog } from '../dialog/public.tsx';
import { PageHeader } from '../page-header/public.tsx';
import { readMotionTransition } from './transition.ts';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });
it('shares feedback timing on actual confirmation controls and page header chrome', async () => {
  const cancel = vi.fn();
  render(<><PageHeader title="Feedback" /><ConfirmDialog open title="Confirm feedback" onConfirm={() => {}} onCancel={cancel} /></>);
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
  render(<ConfirmDialog open title="Reduced feedback" onConfirm={() => {}} onCancel={cancel} />);
  const button = screen.getByRole('button', { name: 'Cancel' });
  expect(parseFloat(getComputedStyle(button).transitionDuration)).toBeLessThan(.001);
  await userEvent.click(button);
  expect(cancel).toHaveBeenCalledOnce();
});
