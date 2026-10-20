import { page, userEvent } from 'vitest/browser';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { Dialog } from './public.tsx';

afterEach(cleanup);

it('does not dismiss the production Dialog on a composing Escape', () => {
  const close = vi.fn();
  render(<Dialog origin={{ kind: 'surface-center' }} open title="Composition" onClose={close}><input aria-label="Composition input" /></Dialog>);
  screen.getByRole('textbox').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', isComposing: true, bubbles: true }));
  expect(close).not.toHaveBeenCalled();
});

it('claims a legacy IME key and then allows a single real Escape', async () => {
  const close = vi.fn();
  render(<Dialog origin={{ kind: 'surface-center' }} open title="IME" onClose={close}><input aria-label="Composing" /></Dialog>);
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
  render(<Dialog origin={{ kind: 'surface-center' }} open title="Native child" onClose={close}>
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
