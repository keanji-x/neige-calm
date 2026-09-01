// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { WavePage } from './public.tsx';
import { card, renderPage, wave } from './test-fixtures.tsx';

afterEach(cleanup);

async function openCards(): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: 'Wave actions' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Cards' }));
}

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
  /* A declaration nobody has dispatched: no status, so no dot. A withdrawn or
     unreadable declaration also has no worker kind — see `deriveReportTasks` —
     which is what leaves those rows with no card affordance at all. */
  const task = (
    key: string,
    state: 'ready' | 'not-ready' | 'withdrawn' | 'unreadable',
    blockId = `b-${key}`,
  ): ReportTaskRow => ({
    blockId, key, state, workerCardId: null, status: null, statusDetail: null,
    kind: state === 'withdrawn' || state === 'unreadable' ? null : 'codex',
    declaration: state === 'ready' ? null
      : state === 'withdrawn' ? 'Withdrawn' : state === 'unreadable' ? 'Unreadable' : 'Not ready',
  });

  /** A task the kernel has a `tasks` row for: a status, and maybe a card. The
   *  kernel's reason for that status is the last, optional argument — it is
   *  what the failed row's dot has to say beyond the word `failed`. */
  const running = (
    key: string,
    status: string,
    workerCardId: string | null,
    kind: 'codex' | 'claude' | 'terminal' = 'codex',
    statusDetail: string | null = null,
  ): ReportTaskRow => ({
    blockId: `b-${key}`, key, state: 'ready', workerCardId, status, statusDetail, kind, declaration: null,
  });

  /* FOLDER used to hold this slot and was removed, not moved: `cove/new-wave`
     omits `cwd` from the create POST, so the kernel persists `$HOME` and every
     wave this front-end makes reported the same constant. The assertion is on
     the *label* rather than on the path, because the defect it guards against
     is the module coming back, not any particular path being shown. */
  it('has no Folder module: nobody chooses a wave cwd any more', () => {
    renderPage({ tasks: [] });
    expect(screen.queryByText('Folder')).toBeNull();
    expect(screen.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
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

  /*
   * The runtime column. Before it, four dispatched tasks were four identical
   * rows: the panel could say what had been declared and nothing about what was
   * happening. Each of these words answers a different question a user staring
   * at a working wave actually has.
   */
  /*
   * CHANGED EXPECTATION — the run used to be spelled out beside the key as one
   * word (`running · codex`). The status is now a dot, and the dot's *label* is
   * what carries the word: `role="img"` + `aria-label` puts it inside the row
   * button's own accessible name, so a screen reader reads the row once and
   * gets the status with it, and no reader anywhere is left with only a colour.
   *
   * That is the assertion here, and it is deliberately made through the
   * accessible name rather than through a class: a dot whose colour is right
   * and whose label is missing is exactly the failure this row must not have,
   * and it would pass a class assertion.
   */
  it('names the status on the row instead of spelling it out', () => {
    renderPage({
      tasks: [
        running('alpha', 'running', 'card-9', 'terminal'),
        running('beta', 'pending', null),
        running('delta', 'failed', 'card-4'),
      ],
    });
    expect(screen.getByRole('button', { name: /^alpha.?Status: running$/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /^beta.?Status: pending$/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /^delta.?Status: failed$/ })).toBeTruthy();
    /* The dot is a named graphic, and hovering it says the same word — the two
       carriers the colour is a shorthand for. */
    const dot = screen.getByRole('img', { name: 'Status: running' });
    expect(dot.getAttribute('title')).toBe('running');
    expect(dot.dataset.ncTaskStatus).toBe('running');
  });

  /*
   * ── The kernel's reason, on the same two carriers (#1147 / #1149) ─────
   *
   * `failed` is the word the reader can already see; *why* is what they were
   * about to go looking for, and #1147 made the kernel say it. The dot's hover
   * is where it lands, because it qualifies exactly the fact the dot states.
   *
   * Asserted on both carriers and in this order:
   *  - the accessible name still **begins** with the status word, so the one
   *    fact the colour is a shorthand for is never traded away for prose — a
   *    name that printed the reason alone would pass a "mentions the detail"
   *    assertion and fail the reader;
   *  - the `title` carries the same string, which is what a sighted pointer
   *    gets;
   *  - `data-nc-task-status` stays the bare status word, because it is what the
   *    colour selector keys on: folding the reason into it would leave a failed
   *    row uncoloured.
   */
  it('says why a failed task failed on the dot, without losing the status word', () => {
    renderPage({
      tasks: [
        running('alpha', 'running', 'card-9', 'terminal'),
        running('delta', 'failed', 'card-4', 'codex', 'wave 9a4c is not a git repository'),
      ],
    });
    const dot = screen.getByRole('img', { name: /^Status: failed/ });
    expect(dot.getAttribute('aria-label')).toBe('Status: failed — wave 9a4c is not a git repository');
    expect(dot.getAttribute('title')).toBe('failed — wave 9a4c is not a git repository');
    expect(dot.dataset.ncTaskStatus).toBe('failed');
    /* And the row it sits in reads as one sentence, key first. */
    expect(screen.getByRole('button', {
      name: /^delta.?Status: failed — wave 9a4c is not a git repository$/,
    })).toBeTruthy();
    /* A row the kernel said nothing about keeps the bare word — the separator
       is not printed with nothing after it. */
    expect(screen.getByRole('img', { name: 'Status: running' }).getAttribute('title')).toBe('running');
  });

  /* A row with no run has no dot at all: `Not ready` is a fact about the
     declaration, not a status, and giving it a coloured dot would state that
     something has been dispatched. */
  it('draws no status dot for a declaration the kernel has not dispatched', () => {
    renderPage({ tasks: [task('alpha', 'ready'), task('beta', 'not-ready'), task('gone', 'withdrawn')] });
    /* By name, not by role alone: the header's lifecycle badge is a named
       graphic too, and asserting "no img on the page" would pass for the wrong
       reason the day it moved. */
    expect(screen.queryAllByRole('img', { name: /^Status: / })).toEqual([]);
  });

  /*
   * The click-through, and the change #1149 makes to it: the *kind* is the card
   * affordance, not the row. "Which card is doing this" and "what does this
   * task say" are two questions, and the row used to answer only whichever one
   * the join decided — once a task was dispatched its declaration became
   * unreachable from the panel.
   *
   * Still a `<button>` and a callback (INV-A11Y-061), and still not nested:
   * this one is a sibling of the row's reveal button, not inside it.
   */
  it('opens the worker card from the kind, and only from the kind', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('alpha', 'running', 'card-9', 'terminal')], onOpenCard, onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: 'terminal' }));
    expect(onOpenCard).toHaveBeenCalledWith('card-9');
    expect(onOpenTask).not.toHaveBeenCalled();
  });

  /* And the rest of the row still reveals the block — for an assigned task too,
     which is the row that used to lose that landing entirely. */
  it('reveals the block from the row even when the task has a worker card', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('alpha', 'running', 'card-9', 'terminal')], onOpenCard, onOpenTask });
    await userEvent.click(screen.getByRole('button', { name: /^alpha.?Status: running$/ }));
    expect(onOpenTask).toHaveBeenCalledWith('b-alpha');
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  /*
   * The kind is a *label* when there is no card behind it. `app/router` clears
   * `workerCardId` for any card the registry cannot draw — a worker card of a
   * kind no entry claims, which is what a kernel newer than this bundle stamps
   * — and a button there would bounce the reader off the URL and land them
   * nowhere. `WavePage` is a pure renderer, so what it legislates is the
   * `workerCardId === null` branch itself, whichever kind arrives in it.
   */
  /*
   * ── CHANGED EXPECTATION ──────────────────────────────────────────────────
   *
   * This test used to close by asserting `onOpenTask` was NOT called either,
   * and called that inertness deliberate ("a click on the row's padding").
   * That was the dead zone, written down as the contract: the row's whole
   * premise is that clicking it reveals the block, and kind-as-label is a state
   * any dispatched row can be in, so the panel had a word sitting in a
   * clickable row doing nothing.
   *
   * What is actually true is that the kind stops being a *control* — no button
   * role, no card — while the row underneath it keeps its own action. The
   * second half of that is a layout fact (`.taskReveal::before` covers the row;
   * the span is deliberately left unpositioned so the sheet lies over it) and
   * jsdom has no layout, so it is asserted in `task-row.browser.test.tsx` by
   * hit test. What is left here is the half jsdom can see, and it is written so
   * it cannot silently become the old claim again.
   */
  it('renders the kind as plain text, not a control, when there is no card to open', async () => {
    const onOpenCard = vi.fn();
    const onOpenTask = vi.fn();
    renderPage({ tasks: [running('beta', 'running', null, 'codex')], onOpenCard, onOpenTask });
    expect(screen.queryByRole('button', { name: 'codex' })).toBeNull();
    const kind = screen.getByText('codex');
    expect(kind.tagName).toBe('SPAN');
    await userEvent.click(kind);
    expect(onOpenCard).not.toHaveBeenCalled();
  });

  /* A withdrawn row carries no kind at all, so there is nothing on it that
     could ever offer a card — the strongest form of "no card affordance", and
     the one that does not depend on `workerCardId` being cleared. */
  it('offers no kind and no card control on a withdrawn row', () => {
    renderPage({ tasks: [task('gone', 'withdrawn')] });
    expect(screen.queryByText('codex')).toBeNull();
    expect(screen.queryByText('terminal')).toBeNull();
    expect(screen.getAllByRole('button', { name: /gone/ }).length).toBe(1);
  });

  /*
   * The strike-through belongs to the *declaration*: `Withdrawn` is struck
   * because the block is struck. It cannot collide with a run —
   * `deriveReportTasks` hands a withdrawn row no status at all — and the
   * declaration slot no longer carries runtime words in any case.
   */
  it('strikes through a withdrawn declaration but not an ordinary one', () => {
    renderPage({ tasks: [task('gone', 'withdrawn')] });
    expect(screen.getByText('Withdrawn').className).toContain('taskWithdrawn');
    cleanup();
    renderPage({ tasks: [task('beta', 'not-ready')] });
    expect(screen.getByText('Not ready').className).not.toContain('taskWithdrawn');
  });
});

describe('WavePage card inventory', () => {
  it('separates Cards, Tasks, and Delete in the Wave actions menu', async () => {
    const onOpenTask = vi.fn();
    renderPage({
      tasks: [{
        blockId: 'task-1', key: 'mobile-layout', state: 'ready', declaration: null,
        status: null, statusDetail: null, kind: 'codex', workerCardId: null,
      }],
      onOpenTask,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Wave actions' }));
    expect(screen.getByRole('menuitem', { name: 'Cards' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Tasks' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Conversations' })).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Delete wave' })).toBeTruthy();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Tasks' }));
    expect(screen.getByRole('heading', { name: 'Tasks' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Cards' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'mobile-layout' }));
    expect(onOpenTask).toHaveBeenCalledWith('task-1');
  });

  it('moves the mobile Outline into its own list and returns to the selected report anchor', async () => {
    const onOpenOutline = vi.fn();
    renderPage({
      outlineItems: [{
        blockId: 'section-1', label: 'What changed', number: 1,
        children: [{ blockId: 'benchmark', label: 'Read path benchmark' }],
      }],
      onOpenOutline,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Wave actions' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Outline' }));
    expect(screen.getByRole('heading', { name: 'Outline' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Read path benchmark' }));
    expect(onOpenOutline).toHaveBeenCalledWith('benchmark');
    /*
     * The panel closes because the anchor navigation drops `?panel=` (#1191
     * §1.4) — one move, not a local flag plus a navigation. The page is a pure
     * renderer of `panel`, so the closing is asserted where the URL is real:
     * `app/router/mobile-report-navigation.test.tsx`.
     */
  });

  it('keeps quick Chat floating on Report and leaves Conversations as history only', async () => {
    const onQuickChat = vi.fn();
    renderPage({
      conversationList: <button type="button">Previous conversation</button>,
      conversationAction: <button type="button" aria-label="New conversation" onClick={onQuickChat}>Chat</button>,
      onStartConversation: onQuickChat,
    });
    const reportChat = document.querySelector<HTMLButtonElement>('[data-nc-mobile-report-chat]');
    expect(reportChat).toBeTruthy();
    expect(reportChat?.textContent).toBe('Chat');
    await userEvent.click(reportChat!);
    expect(onQuickChat).toHaveBeenCalledOnce();
    await userEvent.click(screen.getByRole('button', { name: 'Wave actions' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Conversations' }));
    expect(screen.getByRole('heading', { name: 'Conversations' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Previous conversation' })).toBeTruthy();
    expect(document.querySelector('[data-nc-mobile-report-chat]')).toBeNull();
  });

  it('treats the compact inventory as a pushed page with an explicit return to Report', async () => {
    const { container } = renderPage({ cards: [card({ id: 'k1', title: 'Build log' })] });
    const panel = container.querySelector('[data-nc-mobile-page]');
    expect(panel?.getAttribute('data-nc-mobile-page')).toBe('closed');

    await openCards();
    expect(panel?.getAttribute('data-nc-mobile-page')).toBe('open');
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    expect(panel?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  /*
   * The shell used to reach in through a `window` event to shut this panel.
   * It is a prop now: whoever owns the URL takes `?panel=` away, and the page
   * renders what it is given — including a POP the reader triggered with the
   * hardware Back button, which no event bus could have delivered.
   */
  it('renders whatever panel it is handed, and closes when that becomes null', () => {
    const props = {
      wave: wave(), cards: [card({ id: 'k1', title: 'Build log' })], tasks: [],
      onRenameWave: vi.fn(), onDeleteWave: vi.fn(),
    };
    const { container, rerender } = render(<WavePage {...props} panel="cards" />);
    expect(container.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('open');
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    rerender(<WavePage {...props} panel={null} />);
    expect(container.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('closed');
  });

  // §5.3 caps an empty state at one short sentence, so the old
  // "This wave has no cards yet" became "No cards yet." The assertion is on the
  // rendered string because that string *is* the contract here.
  it('says the wave has no cards yet when the list is empty', () => {
    renderPage({ cards: [] });
    expect(screen.getByText('No cards yet.')).toBeTruthy();
  });

  /*
   * CHANGED EXPECTATION — this used to assert the row printed the title *and
   * not* the kind, on the reasoning that a titled card says the same thing
   * twice. That held while the only titled cards were ones a user had named.
   * #1149 titles every worker card after its task key, so `title ?? kind`
   * deleted the words `codex` / `claude` / `terminal` from the panel on
   * precisely the rows where the kind is the fact the reader needs: three
   * workers named after three slices are otherwise indistinguishable. The name
   * leads and the kind follows in the quiet rank.
   */
  it('labels a card by its title and keeps the kind beside it', () => {
    renderPage({ cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })] });
    expect(screen.getByText('Build log')).toBeTruthy();
    expect(screen.getByText('terminal')).toBeTruthy();
    // One row, both words — not two rows, and not a kind that escaped the row.
    expect(screen.getByRole('button', { name: /^Build log.?terminal$/ })).toBeTruthy();
  });

  it('invokes onOpenCard with the wire id', async () => {
    const onOpenCard = vi.fn();
    renderPage({
      cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })],
      onOpenCard,
    });
    await userEvent.click(screen.getByRole('button', { name: /^Build log/ }));
    expect(onOpenCard).toHaveBeenCalledWith('k1');
  });

  it('opens a mobile Card detail page without entering Grid', async () => {
    const onOpenCard = vi.fn();
    renderPage({ cards: [card({ id: 'k1', title: 'Build log' })], onOpenCard });
    await openCards();
    const cardRow = screen.getByRole('button', { name: 'Build log' });
    await userEvent.click(cardRow);
    expect(onOpenCard).not.toHaveBeenCalled();
    expect(document.querySelector('[data-nc-mobile-page]')?.getAttribute('data-nc-mobile-page')).toBe('open');
    expect(screen.getByRole('heading', { name: 'Build log' })).toBeTruthy();
    expect(screen.getByText('Card ID').nextElementSibling?.textContent).toBe('k1');

    await userEvent.click(screen.getByRole('button', { name: 'Back to Cards' }));
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
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
