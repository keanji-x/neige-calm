// @vitest-environment jsdom
//
// The four invariants of the wave page that a refactor must not be allowed to
// quietly drop. Each is one `it`, and each has been mutation-verified: break
// the production line, watch the named test go red, restore.

import { cleanup, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { card, renderPage, wave } from './test-fixtures.tsx';

afterEach(cleanup);

/** A promise whose settlement the test drives. */
function deferred(): { promise: Promise<void>; resolve: () => void; reject: () => void } {
  let resolve = () => undefined as void;
  let reject = () => undefined as void;
  const promise = new Promise<void>((resolveFn, rejectFn) => {
    resolve = () => resolveFn();
    reject = () => rejectFn(new Error('delete failed'));
  });
  return { promise, resolve, reject };
}

describe('WavePage delete confirm contract', () => {
  it('keeps Cancel enabled and Confirm disabled while the delete is in flight', async () => {
    const gate = deferred();
    const onDeleteWave = vi.fn(() => gate.promise);
    renderPage({ onDeleteWave });

    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));

    // Still mounted, mid-flight.
    expect(screen.getByRole('dialog')).toBeTruthy();
    // CR-6 — busy, not `disabled`: a disabled element is not focusable, and
    // focus is on Confirm at this exact moment, so disabling it would drop
    // focus out of the dialog's trap mid-action.
    const confirm = screen.getByRole('button', { name: 'Deleting…' });
    expect(confirm.hasAttribute('disabled')).toBe(false);
    expect(confirm.getAttribute('aria-disabled')).toBe('true');
    expect(confirm.dataset.ncState).toBe('busy');
    expect(screen.getByRole('button', { name: 'Cancel' }).hasAttribute('disabled')).toBe(true);

    gate.resolve();
    await gate.promise;
    await screen.findByRole('button', { name: /^Delete wave / });
  });

  it('closes the confirm and clears pending when onDeleteWave rejects', async () => {
    const gate = deferred();
    const onDeleteWave = vi.fn(() => gate.promise);
    renderPage({ onDeleteWave });

    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));

    gate.reject();
    await gate.promise.catch(() => undefined);

    // The dialog let go, and re-opening it hands back a live Confirm button.
    const reopen = await screen.findByRole('button', { name: /^Delete wave / });
    expect(screen.queryByRole('dialog')).toBeNull();
    await userEvent.click(reopen);
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete wave' }).getAttribute('aria-disabled')).toBeNull();
  });

  it('renders no <a> element anywhere on the page (INV-A11Y-061)', () => {
    const { container } = renderPage({
      cards: [card({ id: 'k1' }), card({ id: 'k2', title: null, deletable: false })],
    });
    expect(container.querySelectorAll('a').length).toBe(0);
  });

  it('renames exactly once with the trimmed title and never on an unchanged value', async () => {
    const onRenameWave = vi.fn();

    // Unchanged value: committing must not fire. Fresh mounts on purpose —
    // EditableTitle suppresses a click for 300ms after an Enter commit (#288).
    renderPage({ wave: wave({ title: 'Alpha' }), onRenameWave });
    await userEvent.click(screen.getByRole('button', { name: 'Rename wave' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Wave title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Wave title' }), 'Alpha{Enter}');
    expect(onRenameWave).not.toHaveBeenCalled();
    cleanup();

    // Changed value, with surrounding whitespace the commit must trim.
    renderPage({ wave: wave({ title: 'Alpha' }), onRenameWave });
    await userEvent.click(screen.getByRole('button', { name: 'Rename wave' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Wave title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Wave title' }), '  Beta  {Enter}');
    expect(onRenameWave).toHaveBeenCalledTimes(1);
    expect(onRenameWave).toHaveBeenCalledWith('Beta');
  });
});
