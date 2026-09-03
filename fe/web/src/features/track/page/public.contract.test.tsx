// @vitest-environment jsdom
//
// The four invariants of the track page that a refactor must not be allowed to
// quietly drop. Each is one `it`, and each has been mutation-verified: break
// the production line, watch the named test go red, restore.

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

    await userEvent.click(screen.getByRole('button', { name: /^Delete track / }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));

    // Still mounted, mid-flight.
    expect(screen.getByRole('dialog')).toBeTruthy();
    // CR-6 — busy, not `disabled`: a disabled element is not focusable, and
    // focus is on Confirm at this exact moment, so disabling it would drop
    // focus out of the dialog's trap mid-action.
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
    await screen.findByRole('button', { name: /^Delete track / });
  });

  it('closes the confirm and clears pending when onDeleteTrack rejects', async () => {
    const gate = deferred();
    const onDeleteTrack = vi.fn(() => gate.promise);
    renderPage({ onDeleteTrack });

    await userEvent.click(screen.getByRole('button', { name: /^Delete track / }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));

    gate.reject();
    await gate.promise.catch(() => undefined);

    // The dialog let go, and re-opening it hands back a live Confirm button.
    const reopen = await screen.findByRole('button', { name: /^Delete track / });
    expect(screen.queryByRole('dialog')).toBeNull();
    await userEvent.click(reopen);
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete track' }).getAttribute('aria-disabled')).toBeNull();
  });

  /*
   * Every row-shaped thing the page can draw, in one render, because the
   * invariant is about the page and not about one module. The TASKS rows are
   * here deliberately: the default fixture leaves `tasks: []`, so before this
   * the newest navigation on the page — a task row that clicks through to a
   * worker card — was checked against zero rendered rows. The assigned row is
   * the one that carries a destination, and a destination is what tempts an
   * `<a href>`.
   *
   * The rows also now carry TWO destinations each (#1149) — the row reveals the
   * block, the kind opens the worker card — so this render is what keeps both
   * of them under the invariant, and the nesting assertion below is what keeps
   * the second one from being expressed the one way HTML forbids.
   */
  const taskRows = () => ([
    { blockId: 'b-1', key: 'assigned', state: 'ready', workerCardId: 'card-9', status: 'running', statusDetail: null, kind: 'terminal', declaration: null },
    { blockId: 'b-2', key: 'queued', state: 'ready', workerCardId: null, status: 'pending', statusDetail: null, kind: 'codex', declaration: null },
    { blockId: 'b-3', key: 'gone', state: 'withdrawn', workerCardId: null, status: null, statusDetail: null, kind: null, declaration: 'Withdrawn' },
    { blockId: 'b-4', key: 'plain', state: 'ready', workerCardId: null, status: null, statusDetail: null, kind: 'claude', declaration: null },
  ] as const);

  it('renders no <a> element anywhere on the page (INV-A11Y-061)', () => {
    const { container } = renderPage({
      cards: [card({ id: 'k1' }), card({ id: 'k2', title: null, deletable: false })],
      tasks: taskRows(),
    });
    // Not vacuous: the rows really are on the page this assertion inspects.
    expect(container.querySelectorAll('[data-nc-task-inventory] li').length).toBe(4);
    expect(container.querySelectorAll('a').length).toBe(0);
  });

  /*
   * A `<button>` may not contain a `<button>`. It is not a style rule: the
   * inner one is dropped from the parsed tree by every browser's HTML parser,
   * so the affordance simply would not exist — and jsdom happily renders what a
   * browser would discard, which is why this is asserted on the shape rather
   * than left to a click test.
   *
   * The whole page is inspected, not the TASKS list, because the rule is the
   * page's; the assertion below is what makes it non-vacuous for the module
   * that just grew a second control per row.
   */
  it('nests no button inside another button (INV-A11Y-061)', () => {
    const { container } = renderPage({
      cards: [card({ id: 'k1' })],
      tasks: taskRows(),
      board: <div>grid</div>,
      onCloseBoard: () => undefined,
    });
    /* Two controls on the assigned row, one on the rows with no card. */
    expect(container.querySelectorAll('[data-nc-task-inventory] button').length).toBe(5);
    expect(container.querySelectorAll('button button').length).toBe(0);
  });

  it('renames exactly once with the trimmed title and never on an unchanged value', async () => {
    const onRenameTrack = vi.fn();

    // Unchanged value: committing must not fire. Fresh mounts on purpose —
    // EditableTitle suppresses a click for 300ms after an Enter commit (#288).
    renderPage({ track: track({ title: 'Alpha' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), 'Alpha{Enter}');
    expect(onRenameTrack).not.toHaveBeenCalled();
    cleanup();

    // Changed value, with surrounding whitespace the commit must trim.
    renderPage({ track: track({ title: 'Alpha' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '  Beta  {Enter}');
    expect(onRenameTrack).toHaveBeenCalledTimes(1);
    expect(onRenameTrack).toHaveBeenCalledWith('Beta');
  });

  /*
   * #1211 — an unnamed track. The header reads the fallback; the *editor* does
   * not, because the two are separate props now (`value` / `placeholder`).
   * Red when the page goes back to passing `trackDisplayTitle(track.title)` as
   * the value: the box would open holding `Untitled track`.
   */
  it('shows Untitled track in the header and opens an empty box on it', async () => {
    renderPage({ track: track({ title: '' }) });
    const title = screen.getByRole('button', { name: 'Rename track' });
    expect(title.textContent).toBe('Untitled track');
    await userEvent.click(title);
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Track title' }).value).toBe('');
  });

  /*
   * #1211 — clearing the name is a request on a track, because the spec agent's
   * `calm.track.rename` is a second namer that only fires while the title is
   * empty. On an area the same gesture is a cancel; the difference is the
   * explicit `emptyCommit` this page passes, and this is its track-side half.
   * Red when the page drops that prop or the primitive re-hardcodes 'cancel'.
   */
  it('asks to clear the name when the box is emptied and committed', async () => {
    const onRenameTrack = vi.fn();
    renderPage({ track: track({ title: 'Alpha' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '{Enter}');
    expect(onRenameTrack.mock.calls).toEqual([['']]);
  });

  /*
   * And the no-op that survives it: a track that is already unnamed has no
   * state change to ask for. Red when 'clear' is implemented as "always send
   * when empty".
   */
  it('asks for nothing when an already-unnamed track is committed empty', async () => {
    const onRenameTrack = vi.fn();
    renderPage({ track: track({ title: '' }), onRenameTrack });
    await userEvent.click(screen.getByRole('button', { name: 'Rename track' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '{Enter}');
    expect(onRenameTrack).not.toHaveBeenCalled();
  });
});
