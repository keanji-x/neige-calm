// @vitest-environment jsdom
// Invariants for the cove page. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { CovePage } from './public.tsx';

afterEach(cleanup);

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function renderPage(overrides: Partial<Parameters<typeof CovePage>[0]> = {}) {
  const props = {
    cove: cove(),
    waveCount: 2,
    waveList: <div>wave list slot</div>,
    onRenameCove: vi.fn(),
    onDeleteCove: vi.fn(),
    onRequestNewWave: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<CovePage {...props} />) };
}

function deferred<T>() {
  let settle: { resolve: (value: T) => void; reject: (reason: unknown) => void } | null = null;
  const promise = new Promise<T>((resolve, reject) => { settle = { resolve, reject }; });
  return { promise, settle: settle as unknown as { resolve: (value: T) => void; reject: (reason: unknown) => void } };
}

describe('INV-CONFIRM-001 the destructive confirm cannot strand', () => {
  it('keeps Cancel enabled and Confirm disabled while the delete is pending', async () => {
    const pending = deferred<void>();
    renderPage({ onDeleteCove: () => pending.promise });

    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    // §6.13 — deleting a cove cascades to every wave inside it, so it is the
    // one operation in the product that earns a typed confirm. Confirm stays
    // `blocked` until the name matches; clicking it before that is a no-op, and
    // a suite that skips the typing is asserting against a dialog that never
    // armed.
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove' }));

    // Still mounted for the whole await.
    expect(screen.getByRole('dialog')).toBeTruthy();
    // CR-6 — busy, not `disabled`: focus is on Confirm at this moment and a
    // real `disabled` would drop it out of the dialog's trap mid-action.
    const confirm = screen.getByRole('button', { name: 'Deleting…' });
    expect(confirm).toHaveProperty('disabled', false);
    expect(confirm.getAttribute('aria-disabled')).toBe('true');
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', false);

    pending.settle.resolve();
  });

  it('closes and clears pending even when onDeleteCove rejects', async () => {
    const pending = deferred<void>();
    renderPage({ onDeleteCove: () => pending.promise });

    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    // §6.13 — deleting a cove cascades to every wave inside it, so it is the
    // one operation in the product that earns a typed confirm. Confirm stays
    // `blocked` until the name matches; clicking it before that is a no-op, and
    // a suite that skips the typing is asserting against a dialog that never
    // armed.
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove' }));
    pending.settle.reject(new Error('409'));
    await vi.waitFor(() => { expect(screen.queryByRole('dialog')).toBeNull(); });

    // Reopening must offer a usable Confirm: pending has to be cleared too, not
    // just `open`, or the second attempt is dead on arrival.
    // Reopening starts blocked again — the typed input is cleared with the
    // dialog, so a second attempt has to be re-armed rather than inheriting the
    // first one's confirmation.
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    expect(screen.getByRole('button', { name: 'Delete cove' })).toHaveProperty('disabled', true);
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    expect(screen.getByRole('button', { name: 'Delete cove' })).toHaveProperty('disabled', false);
  });
});

describe('INV-A11Y-061 no anchor navigation', () => {
  it('renders no <a> element anywhere, confirm dialog included', async () => {
    const { container } = renderPage();
    expect(container.querySelectorAll('a')).toHaveLength(0);
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    expect(document.body.querySelectorAll('a')).toHaveLength(0);
  });
});

describe('rename', () => {
  it('commits the trimmed name once and stays silent when unchanged', async () => {
    const onRenameCove = vi.fn();
    renderPage({ onRenameCove });

    await userEvent.click(screen.getByRole('button', { name: 'Rename cove' }));
    await userEvent.clear(screen.getByLabelText('Cove name'));
    await userEvent.type(screen.getByLabelText('Cove name'), '  Deep work  {Enter}');
    expect(onRenameCove.mock.calls).toEqual([['Deep work']]);

    // Fresh mount: EditableTitle suppresses a synthesized click for 300ms after
    // an Enter commit (#288), so re-entering edit mode in the same mount would
    // be testing that suppressor rather than this wiring.
    onRenameCove.mockClear();
    cleanup();
    renderPage({ onRenameCove });
    await userEvent.click(screen.getByRole('button', { name: 'Rename cove' }));
    await userEvent.type(screen.getByLabelText('Cove name'), '{Enter}');
    expect(onRenameCove).not.toHaveBeenCalled();
  });
});
