// @vitest-environment jsdom
// Invariants for the area page. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../../core/domain/area.ts';
import { AreaPage } from './public.tsx';

afterEach(cleanup);

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function renderPage(overrides: Partial<Parameters<typeof AreaPage>[0]> = {}) {
  const props = {
    area: area(),
    trackCount: 2,
    trackList: <div>track list slot</div>,
    onRenameArea: vi.fn(),
    onDeleteArea: vi.fn(),
    onRequestNewTrack: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<AreaPage {...props} />) };
}

function deferred<T>() {
  let settle: { resolve: (value: T) => void; reject: (reason: unknown) => void } | null = null;
  const promise = new Promise<T>((resolve, reject) => { settle = { resolve, reject }; });
  return { promise, settle: settle as unknown as { resolve: (value: T) => void; reject: (reason: unknown) => void } };
}

describe('INV-CONFIRM-001 the destructive confirm cannot strand', () => {
  it('keeps Cancel enabled and Confirm disabled while the delete is pending', async () => {
    const pending = deferred<void>();
    renderPage({ onDeleteArea: () => pending.promise });

    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    // §6.13 — deleting an area cascades to every track inside it, so it is the
    // one operation in the product that earns a typed confirm. Confirm stays
    // `blocked` until the name matches; clicking it before that is a no-op, and
    // a suite that skips the typing is asserting against a dialog that never
    // armed.
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));

    // Still mounted for the whole await.
    expect(screen.getByRole('dialog')).toBeTruthy();
    // CR-6 — busy, not `disabled`: focus is on Confirm at this moment and a
    // real `disabled` would drop it out of the dialog's trap mid-action.
    const confirm = screen.getByRole('button', { name: 'Deleting…' });
    expect(confirm).toHaveProperty('disabled', false);
    expect(confirm.getAttribute('aria-disabled')).toBe('true');
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveProperty('disabled', false);
    expect(screen.getByRole('dialog').textContent).toContain('Closing this dialog cancels the delete request.');
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).toBeNull();

    pending.settle.resolve();
  });

  it('closes and clears pending even when onDeleteArea rejects', async () => {
    const pending = deferred<void>();
    renderPage({ onDeleteArea: () => pending.promise });

    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    // §6.13 — deleting an area cascades to every track inside it, so it is the
    // one operation in the product that earns a typed confirm. Confirm stays
    // `blocked` until the name matches; clicking it before that is a no-op, and
    // a suite that skips the typing is asserting against a dialog that never
    // armed.
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    pending.settle.reject(new Error('409'));
    await vi.waitFor(() => { expect(screen.queryByRole('dialog')).toBeNull(); });

    // Reopening must offer a usable Confirm: pending has to be cleared too, not
    // just `open`, or the second attempt is dead on arrival.
    // Reopening starts blocked again — the typed input is cleared with the
    // dialog, so a second attempt has to be re-armed rather than inheriting the
    // first one's confirmation.
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    expect(screen.getByRole('button', { name: 'Delete area' })).toHaveProperty('disabled', true);
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    expect(screen.getByRole('button', { name: 'Delete area' })).toHaveProperty('disabled', false);
  });
});

describe('INV-A11Y-061 no anchor navigation', () => {
  it('renders no <a> element anywhere, confirm dialog included', async () => {
    const { container } = renderPage();
    expect(container.querySelectorAll('a')).toHaveLength(0);
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    expect(document.body.querySelectorAll('a')).toHaveLength(0);
  });
});

describe('rename', () => {
  it('commits the trimmed name once and stays silent when unchanged', async () => {
    const onRenameArea = vi.fn();
    renderPage({ onRenameArea });

    await userEvent.click(screen.getByRole('button', { name: 'Rename area' }));
    await userEvent.clear(screen.getByLabelText('Area name'));
    await userEvent.type(screen.getByLabelText('Area name'), '  Deep work  {Enter}');
    expect(onRenameArea.mock.calls).toEqual([['Deep work']]);

    // Fresh mount: EditableTitle suppresses a synthesized click for 300ms after
    // an Enter commit (#288), so re-entering edit mode in the same mount would
    // be testing that suppressor rather than this wiring.
    onRenameArea.mockClear();
    cleanup();
    renderPage({ onRenameArea });
    await userEvent.click(screen.getByRole('button', { name: 'Rename area' }));
    await userEvent.type(screen.getByLabelText('Area name'), '{Enter}');
    expect(onRenameArea).not.toHaveBeenCalled();
  });

  /*
   * #1211 — the area side of the empty-commit split, and the reason the new
   * semantics is an explicit prop rather than the primitive's default.
   *
   * The track header passes `emptyCommit="clear"` because a track has a second
   * namer (the spec agent's `calm.track.rename`, which only fires on an empty
   * title). An area has none: nothing but its owner will ever name it, so an
   * empty commit stays a cancel and no request leaves.
   *
   * Red when `'clear'` leaks into `EditableTitle`'s default, which is exactly
   * how a shared primitive would spread one page's new rule to the other.
   */
  it('never asks to clear an area name — the empty commit stays a cancel', async () => {
    const onRenameArea = vi.fn();
    renderPage({ onRenameArea });
    await userEvent.click(screen.getByRole('button', { name: 'Rename area' }));
    await userEvent.clear(screen.getByLabelText('Area name'));
    await userEvent.type(screen.getByLabelText('Area name'), '{Enter}');
    expect(onRenameArea).not.toHaveBeenCalled();
  });
});
