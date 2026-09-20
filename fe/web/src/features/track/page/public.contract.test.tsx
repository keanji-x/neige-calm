// @vitest-environment jsdom
//
// Invariants of the track page a refactor must not quietly drop.

import { cleanup, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { card, renderPage, track } from './test-fixtures.tsx';

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

describe('TrackPage delete confirm contract', () => {
  it('keeps Cancel enabled and Confirm disabled while the delete is in flight', async () => {
    const gate = deferred();
    const onDeleteTrack = vi.fn(() => gate.promise);
    renderPage({ onDeleteTrack });

    await userEvent.click(screen.getByRole('button', { name: /^Track actions for / }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete track' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));

    expect(screen.getByRole('dialog')).toBeTruthy();
    // Busy, not `disabled`: a disabled element is not focusable, and focus is on Confirm at this moment.
    const confirm = screen.getByRole('button', { name: 'Deleting…' });
    expect(confirm.hasAttribute('disabled')).toBe(false);
    expect(confirm.getAttribute('aria-disabled')).toBe('true');
    expect(confirm.dataset.ncState).toBe('busy');
    expect(screen.getByRole('button', { name: 'Cancel' }).hasAttribute('disabled')).toBe(false);
    expect(screen.getByRole('dialog').textContent).toContain('Closing this dialog cancels the delete request.');
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).toBeNull();

    gate.resolve();
    await gate.promise;
    await screen.findByRole('button', { name: /^Track actions for / });
  });

  it('closes the confirm and clears pending when onDeleteTrack rejects', async () => {
    const gate = deferred();
    const onDeleteTrack = vi.fn(() => gate.promise);
    renderPage({ onDeleteTrack });

    await userEvent.click(screen.getByRole('button', { name: /^Track actions for / }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete track' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));

    gate.reject();
    await gate.promise.catch(() => undefined);

    const reopen = await screen.findByRole('button', { name: /^Track actions for / });
    expect(screen.queryByRole('dialog')).toBeNull();
    await userEvent.click(reopen);
    await userEvent.click(screen.getByRole('menuitem', { name: 'Delete track' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete track' }).getAttribute('aria-disabled')).toBeNull();
  });

  const taskRows = () => ([
    { blockId: 'b-1', key: 'assigned', state: 'ready', workerCardId: 'card-9', status: 'running', statusDetail: null, kind: 'terminal', declaration: null, pendingReason: null },
    { blockId: 'b-2', key: 'queued', state: 'ready', workerCardId: null, status: 'pending', statusDetail: null, kind: 'codex', declaration: null, pendingReason: null },
    { blockId: 'b-3', key: 'gone', state: 'withdrawn', workerCardId: null, status: null, statusDetail: null, kind: null, declaration: 'Withdrawn', pendingReason: null },
    { blockId: 'b-4', key: 'plain', state: 'ready', workerCardId: null, status: null, statusDetail: null, kind: 'claude', declaration: null, pendingReason: null },
  ] as const);

  it('renders no <a> element anywhere on the page (INV-A11Y-061)', () => {
    const { container } = renderPage({
      cards: [card({ id: 'k1' }), card({ id: 'k2', title: null, deletable: false })],
      tasks: taskRows(),
    });
    expect(container.querySelectorAll('[data-nc-task-inventory] li').length).toBe(4);
    expect(container.querySelectorAll('a').length).toBe(0);
  });

  /* A `<button>` inside a `<button>` is dropped by every browser's HTML parser, and jsdom happily renders what a browser would discard — so the shape is asserted, not a click. */
  it('nests no button inside another button (INV-A11Y-061)', () => {
    const { container } = renderPage({
      cards: [card({ id: 'k1' })],
      tasks: taskRows(),
      board: <div>grid</div>,
      onCloseBoard: () => undefined,
    });
    expect(container.querySelectorAll('[data-nc-task-inventory] button').length).toBe(5);
    expect(container.querySelectorAll('button button').length).toBe(0);
  });

  it('renames exactly once with the trimmed title and never on an unchanged value', async () => {
    const onRenameTrack = vi.fn();

    // Fresh mounts on purpose: EditableTitle suppresses a click for 300ms after an Enter commit.
    renderPage({ track: track({ title: 'Alpha' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), 'Alpha{Enter}');
    expect(onRenameTrack).not.toHaveBeenCalled();
    cleanup();

    renderPage({ track: track({ title: 'Alpha' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '  Beta  {Enter}');
    expect(onRenameTrack).toHaveBeenCalledTimes(1);
    expect(onRenameTrack).toHaveBeenCalledWith('Beta');
  });

  it('shows Untitled track in the header and opens an empty box on it', async () => {
    renderPage({ track: track({ title: '' }) });
    const title = screen.getByRole('button', { name: 'Rename track' });
    expect(title.textContent).toBe('Untitled track');
    await userEvent.click(title);
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Track title' }).value).toBe('');
  });

  /* Clearing the name is a request on a track: the planner's `calm.track.rename` only fires while the title is empty. On an area the same gesture is a cancel. */
  it('asks to clear the name when the box is emptied and committed', async () => {
    const onRenameTrack = vi.fn();
    renderPage({ track: track({ title: 'Alpha' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '{Enter}');
    expect(onRenameTrack.mock.calls).toEqual([['']]);
  });

  it('asks for nothing when an already-unnamed track is committed empty', async () => {
    const onRenameTrack = vi.fn();
    renderPage({ track: track({ title: '' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '{Enter}');
    expect(onRenameTrack).not.toHaveBeenCalled();
  });
});
