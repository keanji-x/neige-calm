import '../../styles/entry.css';
import { createRef } from 'react';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import { useState } from '../state/public.ts';
import { Sheet } from './public.tsx';

afterEach(cleanup);
it('cancels a real captured touch without dismissing and ignores its stale release', async () => {
  await page.viewport(390, 844);
  const close = vi.fn();
  render(<Sheet enabled open label="Touch sheet" frame={{ heightPx: 600, bottomInsetPx: 0 }}
    desktopFooterRef={createRef()} dialogRef={createRef()} panelRef={createRef()} finalFocusRef={createRef()}
    focusInput={false} resolveInitialFocus={() => null} onCloseRequest={close} onFooterHeight={() => {}}
    footer={<input aria-label="Touch draft" />}><button>Local control</button></Sheet>);
  const panel = document.querySelector<HTMLElement>('[data-nc-sheet-panel]')!;
  await expect.poll(() => panel.getAnimations().length).toBe(0);
  const handle = document.querySelector<HTMLElement>('[data-nc-sheet-handle]')!;
  // CDP supplies trusted touch/pointer admission and travel; inject cancellation
  // before the native release using the actual captured id, never a second path.
  const seen: boolean[] = [];
  const cancel = (event: PointerEvent) => {
    seen.push(event.isTrusted);
    act(() => { handle.dispatchEvent(new PointerEvent('pointercancel', { bubbles: true, pointerId: event.pointerId, isPrimary: true, pointerType: 'touch' })); });
  };
  handle.addEventListener('pointerup', cancel, { capture: true, once: true });
  await commands.swipe('[data-nc-sheet-handle]', 350);
  await expect.poll(() => panel.getAnimations().length).toBe(0);
  expect(seen).toEqual([true]); expect(close).not.toHaveBeenCalled();
  expect(panel.closest('dialog')!.open).toBe(true); expect(panel.style.transform).toBe('');
});

function FocusFixture() {
  const [open, setOpen] = useState(true);
  const finalFocusRef = createRef<HTMLButtonElement>();
  return <><button ref={finalFocusRef}>Default touch focus</button><button>External touch focus</button>
    <Sheet enabled open={open} label="Touch focus" frame={{ heightPx: 600, bottomInsetPx: 0 }}
      desktopFooterRef={createRef()} dialogRef={createRef()} panelRef={createRef()} finalFocusRef={finalFocusRef}
      focusInput={false} resolveInitialFocus={() => null} onCloseRequest={() => setOpen(false)} onFooterHeight={() => {}}
      footer={<input aria-label="Touch focus draft" />}><button onClick={() => setOpen(false)}>Close touch sheet</button></Sheet></>;
}
it.each([
  { width: 320, close: 'button' }, { width: 390, close: 'Escape' },
])('preserves real external focus on coarse exit at $width via $close', async ({ width, close }) => {
  await page.viewport(width, 844); render(<FocusFixture />);
  const dialog = page.getByRole('dialog', { name: 'Touch focus' }).element() as HTMLDialogElement;
  const panel = document.querySelector<HTMLElement>('[data-nc-sheet-panel]')!;
  await expect.poll(() => panel.getAnimations().length).toBe(0);
  const draft = page.getByRole('textbox', { name: 'Touch focus draft' }).element() as HTMLElement;
  draft.focus(); expect(document.activeElement).toBe(draft);
  if (close === 'button') await page.getByRole('button', { name: 'Close touch sheet' }).click();
  else await userEvent.keyboard('{Escape}');
  expect(dialog.open).toBe(true); expect(panel.inert).toBe(true);
  expect(panel.getAnimations().some(animation => animation.playState === 'running')).toBe(true);
  const external = page.getByRole('button', { name: 'External touch focus' }).element() as HTMLElement;
  external.focus(); expect(document.activeElement).toBe(external); expect(dialog.open).toBe(true);
  await expect.poll(() => dialog.open).toBe(false);
  await expect.poll(() => panel.textContent).toBe('');
  expect(document.activeElement).toBe(external);
});
