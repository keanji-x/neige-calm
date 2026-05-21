// Contract test for ConfirmDialog — locks the behavioral guarantees
// callers rely on. These tests exist so adjacent refactors (Dialog
// internals, focus-trap rework, calm.css cleanup) can't silently regress
// the Cancel-safe-default + Enter-routing contract that makes this
// primitive safe to drop in front of destructive mutations.
//
// Contract pieces under test:
//
//   A. Default focus lands on the Cancel button when the dialog opens.
//      A user who reflexively hits Enter on the dialog appearing MUST NOT
//      trigger the destructive action.
//
//   B. Esc fires onCancel, not onConfirm. Inherited from Dialog's
//      onClose, but ConfirmDialog routes onClose → onCancel and we want
//      that wiring asserted explicitly.
//
//   C. Clicking the overlay (outside the panel) fires onCancel.
//
//   D. Enter while Cancel is focused activates Cancel (native button
//      semantics — no preventDefault interferes).
//
//   E. Tab to Confirm then Enter activates Confirm.
//
//   F. Rapid-Enter on initial focus is safe: three Enter presses with no
//      tabbing never call onConfirm and call onCancel at least once.
//
// jsdom note: `fireEvent.keyDown(target, { key: 'Enter' })` does NOT
// synthesize the native "Enter on focused button → click" behavior we
// care about; userEvent.keyboard('{Enter}') does. All Enter-related
// tests below go through userEvent for that reason. Esc, by contrast,
// is intercepted by Dialog at the document level — fireEvent.keyDown on
// document still triggers the keydown listener, so we use that for B.

import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, render, screen, cleanup, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ConfirmDialog } from './ConfirmDialog';

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
});

/** Flush the requestAnimationFrame that Dialog uses to defer initial
 *  focus. Without this, `document.activeElement` is still whatever was
 *  focused before the dialog opened. */
async function flushInitialFocus() {
  await act(async () => {
    await new Promise<void>((r) => requestAnimationFrame(() => r()));
  });
}

describe('ConfirmDialog behavioral contract', () => {
  it('A. default focus lands on Cancel, not Confirm', async () => {
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );

    await flushInitialFocus();

    const cancel = screen.getByRole('button', { name: 'Cancel' });
    const confirm = screen.getByRole('button', { name: 'Confirm' });
    expect(document.activeElement).toBe(cancel);
    expect(document.activeElement).not.toBe(confirm);
  });

  it('B. Esc fires onCancel, not onConfirm', async () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await flushInitialFocus();

    // Dialog attaches its Esc handler at the document level (not on the
    // panel), so we dispatch keyDown on the document directly.
    await act(async () => {
      fireEvent.keyDown(document, { key: 'Escape' });
    });

    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('C. clicking the overlay fires onCancel, not onConfirm', async () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await flushInitialFocus();

    // The overlay closes via onMouseDown when the target IS the overlay
    // itself (clicks inside the panel stop propagation). Query for the
    // overlay through its class — the panel is `role="dialog"`, its
    // parent is the overlay.
    const panel = screen.getByRole('dialog');
    const overlay = panel.parentElement as HTMLElement | null;
    expect(overlay).not.toBeNull();
    expect(overlay!.classList.contains('modal-overlay')).toBe(true);

    await act(async () => {
      fireEvent.mouseDown(overlay!);
    });

    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('D. Enter on focused Cancel fires onCancel, not onConfirm', async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await flushInitialFocus();

    // Cancel already has focus (per contract A); pressing Enter should
    // activate it via native button semantics.
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(document.activeElement).toBe(cancel);

    await user.keyboard('{Enter}');

    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('E. Tab to Confirm then Enter fires onConfirm', async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await flushInitialFocus();

    // Cancel is focused; explicitly move focus to Confirm (avoids the
    // jsdom-vs-Dialog focus-trap interaction, which the Dialog tests
    // already cover separately). What this test pins down is the
    // "Enter on focused Confirm → onConfirm" wiring.
    const confirm = screen.getByRole('button', { name: 'Confirm' });
    confirm.focus();
    expect(document.activeElement).toBe(confirm);

    await user.keyboard('{Enter}');

    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onCancel).not.toHaveBeenCalled();
  });

  it('G. confirmDisabled blocks onConfirm but leaves Cancel functional', async () => {
    // The "stay open while pending" pattern relies on this: a call site
    // can keep the dialog mounted during an in-flight async confirm and
    // know that (1) a second click on the disabled Confirm button does
    // nothing, and (2) the user can still bail out via Cancel — the
    // Cancel-safe default contract is preserved even mid-await.
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        confirmDisabled
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await flushInitialFocus();

    const confirm = screen.getByRole('button', { name: 'Confirm' });
    const cancel = screen.getByRole('button', { name: 'Cancel' });

    expect((confirm as HTMLButtonElement).disabled).toBe(true);

    // Clicking the disabled Confirm must not fire onConfirm. userEvent
    // respects pointer-events / disabled semantics, so the click is a
    // no-op at the framework level — exactly what we want from prod.
    await user.click(confirm);
    expect(onConfirm).not.toHaveBeenCalled();

    // Cancel must still work — it is the user's escape hatch during a
    // pending confirm.
    await user.click(cancel);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it('F. rapid Enter mashing on initial focus is safe (onConfirm never called)', async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Delete wave"
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    await flushInitialFocus();

    // Without tabbing: focus is on Cancel. Three Enter presses must not
    // reach Confirm. (In production the parent typically flips `open`
    // false on the first onCancel; this test deliberately keeps `open`
    // true so we can observe all three keystrokes.)
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(document.activeElement).toBe(cancel);

    await user.keyboard('{Enter}');
    await user.keyboard('{Enter}');
    await user.keyboard('{Enter}');

    expect(onConfirm).not.toHaveBeenCalled();
    expect(onCancel.mock.calls.length).toBeGreaterThanOrEqual(1);
  });
});
