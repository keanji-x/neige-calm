// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { WavePage } from './public.tsx';
import { card, renderPage, wave } from './test-fixtures.tsx';

afterEach(cleanup);

describe('WavePage header', () => {
  it('shows the wave title and the lifecycle badge', () => {
    renderPage({ wave: wave({ title: 'Ship the rewrite', lifecycle: 'blocked' }) });
    expect(screen.getByRole('button', { name: 'Rename wave' }).textContent).toBe('Ship the rewrite');
    expect(screen.getByRole('img', { name: 'Wave lifecycle: Blocked' })).toBeTruthy();
  });

  it('does not put Draft in the header', () => {
    renderPage({ wave: wave({ title: 'Ship the rewrite', lifecycle: 'draft' }) });
    expect(screen.queryByRole('img', { name: 'Wave lifecycle: Draft' })).toBeNull();
  });

  it('hides done and canceled, and still shows failed', () => {
    renderPage({ wave: wave({ lifecycle: 'done' }) });
    expect(screen.queryByRole('img', { name: 'Wave lifecycle: Done' })).toBeNull();
    cleanup();
    renderPage({ wave: wave({ lifecycle: 'canceled' }) });
    expect(screen.queryByRole('img', { name: 'Wave lifecycle: Canceled' })).toBeNull();
    cleanup();
    renderPage({ wave: wave({ lifecycle: 'failed' }) });
    expect(screen.getByRole('img', { name: 'Wave lifecycle: Failed' })).toBeTruthy();
  });

  it('falls back to the untitled label for a blank title', () => {
    renderPage({ wave: wave({ title: '  ' }) });
    expect(screen.getByRole('button', { name: 'Rename wave' }).textContent).toBe('Untitled wave');
  });

  /* The header is one row now. It used to carry "Today / ● atlas" above the
     title, restating in chrome what the rail states permanently — so the crumb,
     its back button and the cove dot are gone, and with them the page's whole
     reason to know which cove it is in. This asserts the *absence*, because the
     row is the kind of thing that gets added back by reflex. */
  it('carries no ancestor navigation of its own', () => {
    renderPage({ wave: wave({ title: 'Ship the rewrite' }) });
    expect(screen.queryByRole('button', { name: 'Back to cove' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Today' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Back to wave' })).toBeNull();
  });

  it('puts Back on the page title row when the card grid is open', async () => {
    const onCloseBoard = vi.fn();
    renderPage({
      board: <div data-nc-card-grid="">grid</div>,
      onCloseBoard,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Back to wave' }));
    expect(onCloseBoard).toHaveBeenCalledOnce();
  });
});

describe('WavePage task inventory', () => {
  const task = (
    key: string,
    state: 'ready' | 'not-ready' | 'withdrawn' | 'unreadable',
    blockId = `b-${key}`,
  ) => ({ blockId, key, state } as const);

  /* FOLDER used to hold this slot and was removed, not moved: `cove/new-wave`
     omits `cwd` from the create POST, so the kernel persists `$HOME` and every
     wave this front-end makes reported the same constant. The assertion is on
     the *label* rather than on the path, because the defect it guards against
     is the module coming back, not any particular path being shown. */
  it('has no Folder module: nobody chooses a wave cwd any more', () => {
    renderPage({ tasks: [] });
    expect(screen.queryByText('Folder')).toBeNull();
    expect(screen.getByText('Tasks')).toBeTruthy();
  });

  it('says no tasks are declared yet when the report has none', () => {
    renderPage({ tasks: [] });
    expect(screen.getByText('No tasks declared yet.')).toBeTruthy();
  });

  /* `Ready` is the ordinary case and prints nothing: a column in which every
     row carries a word is a column nobody reads. What the row must carry is the
     two states a reader would otherwise have to open the document to find. */
  it('names only the states that are not the ordinary one', () => {
    renderPage({ tasks: [task('alpha', 'ready'), task('beta', 'not-ready'), task('gone', 'withdrawn')] });
    expect(screen.getByRole('button', { name: 'alpha' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /beta.*Not ready/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /gone.*Withdrawn/ })).toBeTruthy();
    expect(screen.queryByText('Ready')).toBeNull();
  });

  /*
   * The fourth state, which the review found had no test at all: a task whose
   * payload this build cannot parse. `deriveReportTasks` names it by its block
   * id — the one literal still true about it — and the row says so rather than
   * pretending it is merely not ready. Deleting that branch left every other
   * case green.
   */
  it('names an unreadable task by its block id and says so', () => {
    renderPage({ tasks: [task('b_bf88', 'unreadable', 'b_bf88')] });
    const row = screen.getByRole('button', { name: /b_bf88.*Unreadable/ });
    expect(row).toBeTruthy();
    /* Not the word used for a task the agent simply has not finished. */
    expect(screen.queryByText('Not ready')).toBeNull();
  });

  /* The row is a pointer to the block, not a copy of it — it hands back the
     *block* id, which is what the reveal path takes, and not the task key. */
  it('opens a task by its block id, not by its key', async () => {
    const onOpenTask = vi.fn();
    renderPage({ tasks: [task('alpha', 'ready', 'b-17')], onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'alpha' }));
    expect(onOpenTask).toHaveBeenCalledWith('b-17');
  });
});

describe('WavePage card inventory', () => {
  // §5.3 caps an empty state at one short sentence, so the old
  // "This wave has no cards yet" became "No cards yet." The assertion is on the
  // rendered string because that string *is* the contract here.
  it('says the wave has no cards yet when the list is empty', () => {
    renderPage({ cards: [] });
    expect(screen.getByText('No cards yet.')).toBeTruthy();
  });

  // One label per row, not two. `kind` is the card's identity and `title` its
  // label; when a card has a title, printing the kind beside it says the same
  // thing twice in a 308px panel column.
  it('labels a card by its title, and does not also print the kind', () => {
    renderPage({ cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })] });
    expect(screen.getByRole('button', { name: 'Build log' })).toBeTruthy();
    expect(screen.queryByText('terminal')).toBeNull();
  });

  it('invokes onOpenCard with the wire id', async () => {
    const onOpenCard = vi.fn();
    renderPage({
      cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })],
      onOpenCard,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Build log' }));
    expect(onOpenCard).toHaveBeenCalledWith('k1');
  });

  it('falls back to the kind when a card has no title', () => {
    const { container } = render(<WavePage
      wave={wave()}
      cards={[card({ id: 'k1', kind: 'notes', title: null })]}
      tasks={[]}
      onRenameWave={vi.fn()}
      onDeleteWave={vi.fn()}
    />);
    // Exactly once: with no title the kind stands alone rather than twice.
    expect(container.textContent).toContain('notes');
    expect(screen.getAllByText('notes').length).toBe(1);
  });

  it('marks non-deletable cards as kernel-owned', () => {
    renderPage({ cards: [card({ id: 'k1', deletable: false }), card({ id: 'k2', deletable: true })] });
    expect(screen.getAllByText('kernel-owned').length).toBe(1);
  });

  // Deliberately gone. §5.3: an unbuilt region shows the *shape* of what is
  // coming, and nothing else — "no module path, no slice name, no apology".
  // The card list is built; there is nothing here to apologise for.
  it('does not apologise for unbuilt slices in the card panel', () => {
    const { container } = renderPage({ cards: [card({ id: 'k1' })] });
    expect(container.textContent).not.toMatch(/later slice/i);
  });
});

describe('WavePage delete', () => {
  it('does not open the confirm until the delete button is pressed', () => {
    renderPage();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('uses the shared destructive copy', async () => {
    renderPage();
    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    expect(screen.getByRole('dialog', { name: 'Delete this wave?' })).toBeTruthy();
    expect(screen.getByText(/This cannot be undone/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete wave' })).toBeTruthy();
  });

  it('cancelling closes the confirm without deleting', async () => {
    const onDeleteWave = vi.fn();
    renderPage({ onDeleteWave });
    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(onDeleteWave).not.toHaveBeenCalled();
  });

  it('confirming calls onDeleteWave and closes', async () => {
    const onDeleteWave = vi.fn(() => Promise.resolve());
    renderPage({ onDeleteWave });
    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
    expect(onDeleteWave).toHaveBeenCalledTimes(1);
    await screen.findByRole('button', { name: /^Delete wave / });
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
