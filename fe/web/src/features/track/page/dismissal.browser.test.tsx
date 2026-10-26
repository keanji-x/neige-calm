import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { renderPage } from './test-fixtures.tsx';

afterEach(cleanup);

it.each([{ isComposing: true }, { keyCode: 229 }])('claims composing panel Escape %j', async (ime) => {
  await page.viewport(390, 844);
  const close = vi.fn();
  renderPage({ panel: 'conversations', onClosePanel: close });
  const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true, ...ime });
  document.querySelector('#mobile-track-panel')!.dispatchEvent(event);
  expect(close).not.toHaveBeenCalled();
  expect(event.defaultPrevented).toBe(true);
});

it('dismisses a native panel menu before its panel', async () => {
  await page.viewport(390, 844);
  const close = vi.fn();
  renderPage({ panel: 'conversations', onClosePanel: close, conversationList:
    <DropdownMenu button={{ label: 'Panel actions' }}><DropdownMenuItem label="Panel item" /></DropdownMenu> });
  await page.getByRole('button', { name: 'Panel actions', exact: true }).click();
  await expect.element(page.getByRole('menuitem', { name: 'Panel item' })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  expect(close).not.toHaveBeenCalled();
  await expect.element(page.getByRole('menuitem', { name: 'Panel item' })).not.toBeInTheDocument();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
});

it('leaves an obscured panel out of the Escape stack', async () => {
  await page.viewport(390, 844);
  const close = vi.fn();
  renderPage({ panel: 'conversations', mobilePanelObscured: true, onClosePanel: close });
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  expect(close).not.toHaveBeenCalled();
});

it('does not give desktop panel content an inactive mobile layer depth', async () => {
  await page.viewport(1200, 800);
  renderPage({ conversationContextList:
    <DropdownMenu button={{ label: 'Desktop panel actions' }}><DropdownMenuItem label="Desktop panel item" /></DropdownMenu> });
  await page.getByRole('button', { name: 'Desktop panel actions' }).click();
  await expect.element(page.getByRole('menuitem', { name: 'Desktop panel item' })).toBeVisible();
  const close = vi.fn();
  render(<Dialog open origin={{ kind: 'surface-center' }} title="Independent dialog" onClose={close}>Dialog content</Dialog>);
  await expect.element(page.getByRole('dialog', { name: 'Independent dialog' })
    .getByRole('button', { name: 'Close', exact: true })).toHaveFocus();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
});
