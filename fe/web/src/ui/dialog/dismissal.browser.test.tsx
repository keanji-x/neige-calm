import { page, userEvent } from 'vitest/browser';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { ConfirmDialog, Dialog } from './public.tsx';

afterEach(cleanup);

it('does not dismiss the production Dialog on a composing Escape', () => {
  const close = vi.fn();
  render(<Dialog purpose="info" origin={{ kind: 'surface-center' }} open title="Composition" onClose={close}><input aria-label="Composition input" /></Dialog>);
  screen.getByRole('textbox').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', isComposing: true, bubbles: true }));
  expect(close).not.toHaveBeenCalled();
});

it('claims a legacy IME key and then allows a single real Escape', async () => {
  const close = vi.fn();
  render(<Dialog purpose="info" origin={{ kind: 'surface-center' }} open title="IME" onClose={close}><input aria-label="Composing" /></Dialog>);
  await page.getByRole('textbox').click();
  const event = new KeyboardEvent('keydown', { key: 'Escape', keyCode: 229, bubbles: true, cancelable: true });
  screen.getByRole('textbox').dispatchEvent(event);
  expect(close).not.toHaveBeenCalled();
  expect(event.defaultPrevented).toBe(true);
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
});

it('dismisses a real nested native menu before its Dialog', async () => {
  const close = vi.fn();
  render(<Dialog purpose="info" origin={{ kind: 'surface-center' }} open title="Native child" onClose={close}>
    <DropdownMenu button={{ label: 'Nested actions' }}><DropdownMenuItem label="Native item" /></DropdownMenu>
  </Dialog>);
  await page.getByRole('button', { name: 'Nested actions' }).click();
  await expect.element(page.getByRole('menuitem', { name: 'Native item' })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  expect(close).not.toHaveBeenCalled();
  await expect.element(page.getByRole('menuitem', { name: 'Native item' })).not.toBeInTheDocument();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
});

it('keeps exactly one confirm label visible without resizing between ready and busy', async () => {
  const props = {
    origin: { kind: 'surface-center' } as const, open: true, title: 'Delete track',
    confirmLabel: 'Delete track', confirmBusyLabel: 'Deleting…',
    onConfirm: vi.fn(), onCancel: vi.fn(),
  };
  const { rerender } = render(<ConfirmDialog {...props} confirmState="ready" />);
  const button = screen.getByRole('button', { name: 'Delete track' });
  const ready = within(button).getByText('Delete track');
  const busy = within(button).getByText('Deleting…');
  const settledPlacement = async (visibleLabel: HTMLElement) => {
    await document.fonts.ready;
    await expect.poll(() => document.getAnimations().filter(animation => animation.playState === 'running').length).toBe(0);
    // Allow layout to flush after animation completion before measuring the visible text.
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    const buttonRect = button.getBoundingClientRect();
    const labelRect = visibleLabel.getBoundingClientRect();
    return { width: buttonRect.width, textTop: labelRect.top - buttonRect.top };
  };
  expect([ready, busy].map(label => label.checkVisibility({ visibilityProperty: true }))).toEqual([true, false]);
  const before = await settledPlacement(ready);
  expect(before.width).toBeGreaterThan(0);

  rerender(<ConfirmDialog {...props} confirmState="busy" />);
  const busyButton = screen.getByRole('button', { name: 'Deleting…' });
  const busyLabel = within(busyButton).getByText('Deleting…');
  expect([within(busyButton).getByText('Delete track'), busyLabel]
    .map(label => label.checkVisibility({ visibilityProperty: true }))).toEqual([false, true]);
  const after = await settledPlacement(busyLabel);
  // One CSS pixel allows fractional layout rounding, far less than a text-line jump.
  expect(Math.abs(after.textTop - before.textTop)).toBeLessThanOrEqual(1);
  expect(Math.abs(after.width - before.width)).toBeLessThanOrEqual(1);
});


it('allows explicit Escape from a form dialog', async () => {
  const close = vi.fn();
  render(<Dialog purpose="form" origin={{ kind: 'surface-center' }} open title="Draft" onClose={close}><input aria-label="Draft" /></Dialog>);
  await page.getByRole('textbox').click();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
});
