import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { Suspense } from 'react';
import { createPortal } from 'react-dom';
import { TerminalSurface } from '../../../systems/terminal/surface.tsx';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { createCardHost, createCardRegistry, type CardEntry } from '../../../systems/cards/public.ts';
import { useState } from '../../../ui/state/public.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { CardGridOverlay } from './public.tsx';

type MenuCard = Readonly<{ type: 'dismissal-test'; id: string }>;
declare module '../../../systems/cards/registry.js' { interface CardDataMap { dismissalTest: MenuCard } }

afterEach(cleanup);

function setup(terminal = false, menuInitiallyOpen = false) {
  const registry = createCardRegistry();
  const entry: CardEntry<MenuCard> = {
    type: 'dismissal-test', defaultSize: { w: 12, h: 4, minW: 4, minH: 3 },
    title: () => 'Menu', accessibleName: () => 'Menu', create: { mode: 'kernel-minted-only' },
    component: function MenuContent() {
      const [open, setOpen] = useState(menuInitiallyOpen);
      if (terminal) return <Suspense fallback={null}><TerminalSurface card={{ id: 'terminal', terminalId: 'dismissal-test' }} /></Suspense>;
      return createPortal(<DropdownMenu isMenuOpen={open} onOpenChange={setOpen} button={{ label: 'Grid actions' }}><DropdownMenuItem label="Grid item" /></DropdownMenu>, document.body);
    },
  };
  registry.register(entry);
  const close = vi.fn();
  const host = createCardHost(registry);
  const overlay = (dismissible: boolean) => <CardGridOverlay open host={host} activeCardId={null} onClose={dismissible ? close : undefined}
    items={[{ card: { type: 'dismissal-test', id: 'menu' }, title: 'Menu', originalIndex: 0, activity: null, notice: null }]} />;
  const view = render(overlay(!menuInitiallyOpen));
  return { close, enableDismissal: () => { view.rerender(overlay(true)); } };
}

it.each([{ isComposing: true }, { keyCode: 229 }])('claims composing grid Escape %j', (ime) => {
  const { close } = setup();
  const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true, ...ime });
  document.querySelector('[data-nc-card-grid]')!.dispatchEvent(event);
  expect(close).not.toHaveBeenCalled();
  expect(event.defaultPrevented).toBe(true);
});

it('dismisses the grid menu before the grid', async () => {
  const { close, enableDismissal } = setup(false, true);
  await expect.element(page.getByRole('menuitem', { name: 'Grid item' })).toBeVisible();
  // Installing a dismiss callback must not overtake an already-open portal menu.
  enableDismissal();
  await userEvent.keyboard('{Escape}');
  expect(close).not.toHaveBeenCalled();
  await expect.element(page.getByRole('menuitem', { name: 'Grid item' })).not.toBeInTheDocument();
  await userEvent.keyboard('{Escape}');
  expect(close).toHaveBeenCalledOnce();
  expect(screen.getByRole('button', { name: 'Grid actions' })).toBeTruthy();
});

it('keeps the grid open when the real terminal consumes Escape', async () => {
  const { close } = setup(true);
  const input = await waitFor(() => {
    const element = document.querySelector<HTMLTextAreaElement>('[data-nc-terminal-input]');
    expect(element).not.toBeNull();
    return element!;
  });
  input.focus();
  await userEvent.keyboard('{Escape}');
  expect(close).not.toHaveBeenCalled();
});

it('dismisses a reopened confirmation before a grid opened after its first close', async () => {
  const host = createCardHost(createCardRegistry());
  const closeGrid = vi.fn();
  const closeDialog = vi.fn();
  const overlay = (dialog: boolean, grid: boolean) => <>
    <CardGridOverlay open={grid} host={host} items={[]} activeCardId={null} onClose={closeGrid} />
    <ConfirmDialog origin={{ kind: 'surface-center' }} open={dialog} title="Delete card"
      onConfirm={vi.fn()} onCancel={closeDialog} />
  </>;
  const view = render(overlay(true, false));
  await expect.element(page.getByRole('dialog', { name: 'Delete card' })).toBeVisible();
  view.rerender(overlay(false, false));
  view.rerender(overlay(false, true));
  view.rerender(overlay(true, true));
  await expect.element(page.getByRole('dialog', { name: 'Delete card' })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  expect(closeDialog).toHaveBeenCalledOnce();
  expect(closeGrid).not.toHaveBeenCalled();
});
