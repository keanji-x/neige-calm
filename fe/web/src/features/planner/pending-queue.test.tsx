// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  PendingQueueEntry, PlannerQueueWriteOutcome,
} from '../../../../core/domain/conversation.ts';
import { PendingQueue, type PendingQueueProps } from './pending-queue.tsx';

afterEach(cleanup);

const done: Promise<PlannerQueueWriteOutcome> = Promise.resolve({ kind: 'done' });

function entry(overrides: Partial<PendingQueueEntry> = {}): PendingQueueEntry {
  return { entry_id: 'e1', text: 'look at the report', rev: 0, queued_at_ms: 10, ...overrides };
}

function renderQueue(props: Partial<Parameters<typeof PendingQueue>[0]> = {}) {
  const onEdit = vi.fn<PendingQueueProps['onEdit']>(() => done);
  const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => done);
  render(<PendingQueue
    entries={[entry()]}
    overflow={0}
    busy={false}
    onEdit={onEdit}
    onDelete={onDelete}
    {...props}
  />);
  return { onEdit, onDelete };
}

describe('PendingQueue', () => {
  it('shows the text of every queued message', () => {
    renderQueue({ entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })] });
    expect(screen.getByText('look at the report')).toBeTruthy();
    expect(screen.getByText('and the diff')).toBeTruthy();
  });

  it('renders nothing when there is no queue', () => {
    const { container } = render(<PendingQueue
      entries={[]} overflow={0} busy={false}
      onEdit={vi.fn()} onDelete={vi.fn()}
    />);
    expect(container.querySelector('[data-nc-pending-queue]')).toBeNull();
  });

  /* The revision is the whole compare-and-swap. Sending the text without it,
     or with a revision the reader was not shown, is a write that can silently
     overwrite somebody else's. */
  it('deletes against the revision the entry was read at', async () => {
    const { onDelete } = renderQueue({ entries: [entry({ rev: 4 })] });
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    await waitFor(() => { expect(onDelete).toHaveBeenCalledTimes(1); });
    expect(onDelete.mock.calls[0]?.[0]).toMatchObject({ entry_id: 'e1', rev: 4 });
  });

  it('edits against the revision the entry was read at', async () => {
    const { onEdit } = renderQueue({ entries: [entry({ rev: 7 })] });
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'Edit queued message' }), {
      target: { value: 'look at the diff instead' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => { expect(onEdit).toHaveBeenCalledTimes(1); });
    expect(onEdit.mock.calls[0]?.[0]).toMatchObject({ entry_id: 'e1', rev: 7 });
    expect(onEdit.mock.calls[0]?.[1]).toBe('look at the diff instead');
  });

  /*
   * The one this slice exists for. A lost race must not look like a save: the
   * reader has to be told their version did not land, and be shown what the
   * entry says now, on the revision a retry would be written against.
   */
  it('tells the reader when their edit lost the race, and shows what won', async () => {
    const onEdit = vi.fn<PendingQueueProps['onEdit']>(() => Promise.resolve(
      { kind: 'stale', text: 'somebody else wrote this', rev: 9 },
    ));
    render(<PendingQueue entries={[entry()]} overflow={0} busy={false} onEdit={onEdit} onDelete={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    // The draft is TYPED, and that is the whole point of the test. Saving an
    // untouched editor cannot see a discard: the value it asserts on would be
    // the server's text either way.
    fireEvent.change(screen.getByRole('textbox', { name: 'Edit queued message' }), {
      target: { value: 'actually check the CI logs first' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    const notice = await screen.findByText(/changed while you were editing it/);
    expect(notice.textContent).toContain('not saved');
    // The reader's sentence is the one thing here that cannot be fetched
    // again, so it is the one thing a refusal may not destroy.
    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Edit queued message' }).value)
      .toBe('actually check the CI logs first');
    // …and the text that won is offered beside it, not silently swapped in.
    const theirs = document.querySelector('[data-nc-pending-entry-theirs]');
    expect(theirs?.textContent).toContain('somebody else wrote this');
  });

  /* Taking the winning text is a deliberate act, and it is available. */
  it('replaces the draft with the winning text only when asked', async () => {
    const onEdit = vi.fn<PendingQueueProps['onEdit']>(() => Promise.resolve(
      { kind: 'stale', text: 'somebody else wrote this', rev: 9 },
    ));
    render(<PendingQueue entries={[entry()]} overflow={0} busy={false} onEdit={onEdit} onDelete={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'Edit queued message' }), {
      target: { value: 'mine' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText(/changed while you were editing it/);

    fireEvent.click(screen.getByRole('button', { name: 'Use their version' }));
    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Edit queued message' }).value)
      .toBe('somebody else wrote this');
  });

  /*
   * The second Save carries the revision the SERVER reported, not the one on
   * the cached page. The page is by definition behind at that moment, and the
   * refresh that would fix it may still be in flight or may fail — so re-using
   * `entry.rev` guarantees a second 409.
   */
  it('retries against the revision the 409 reported, not the cached one', async () => {
    const onEdit = vi.fn<PendingQueueProps['onEdit']>(() => Promise.resolve(
      { kind: 'stale', text: 'theirs', rev: 9 },
    ));
    render(<PendingQueue entries={[entry({ rev: 3 })]} overflow={0} busy={false} onEdit={onEdit} onDelete={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'Edit queued message' }), {
      target: { value: 'mine' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText(/changed while you were editing it/);

    /* Astryx's action slot disables the button while its promise is in
       flight, and a click on a disabled one is simply lost — so wait for it to
       come back rather than assume the first Save has settled. */
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Save' }).getAttribute('aria-busy')).not.toBe('true');
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => { expect(onEdit).toHaveBeenCalledTimes(2); });
    expect(onEdit.mock.calls[0]?.[0].rev).toBe(3);
    expect(onEdit.mock.calls[1]?.[0].rev).toBe(9);
  });

  /*
   * A refusal belongs to the entry it was about. Before this, the notice and
   * the open editor were one value, so a 409 on B replaced it wholesale and
   * the sentence being typed into A was gone with no undo — the same "a
   * refusal destroys the reader's words" defect the stale arm was fixed for,
   * one entry over.
   */
  it('does not destroy a draft on another entry when a write is refused', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => Promise.resolve(
      { kind: 'stale', text: 'B changed', rev: 4 },
    ));
    render(<PendingQueue
      entries={[entry(), entry({ entry_id: 'e2', text: 'the other one' })]}
      overflow={0} busy={false} onEdit={vi.fn()} onDelete={onDelete}
    />);

    /* Static selectors only (`architecture/no-class-dom-query`): take both
       rows once, in document order, and address them positionally. */
    const rows = document.querySelectorAll('[data-nc-pending-entry]');
    const rowA = rows[0] as HTMLElement;
    const rowB = rows[1] as HTMLElement;
    expect(rowA.textContent).toContain('look at the report');
    expect(rowB.textContent).toContain('the other one');

    fireEvent.click(within(rowA).getByRole('button', { name: 'Edit' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'Edit queued message' }), {
      target: { value: 'words I do not want to lose' },
    });
    fireEvent.click(within(rowB).getByRole('button', { name: 'Delete' }));

    await waitFor(() => { expect(onDelete).toHaveBeenCalledTimes(1); });
    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Edit queued message' }).value)
      .toBe('words I do not want to lose');
    /* …and the refusal is reported on the entry it happened to, not on A.
       Matched on a phrase both wordings share, so this case answers only the
       question it is about: which entry the notice landed on. */
    const after = document.querySelectorAll('[data-nc-pending-entry]');
    expect((after[1] as HTMLElement).textContent).toContain('This message changed');
    expect((after[0] as HTMLElement).textContent).not.toContain('This message changed');
  });

  /*
   * With no editor open there is no "your text" to point at, and a refused
   * DELETE did not fail to save anything — it failed to remove something. The
   * widened wording asserted a screen the reader is not looking at, and put a
   * "Use their version" replacement beside a row with nothing to replace.
   */
  it('does not claim the reader has text below when no editor is open', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => Promise.resolve(
      { kind: 'stale', text: 'somebody else wrote this', rev: 9 },
    ));
    render(<PendingQueue entries={[entry()]} overflow={0} busy={false} onEdit={vi.fn()} onDelete={onDelete} />);
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));

    const line = await screen.findByText(/nothing happened to it/);
    expect(line.textContent).not.toContain('Your text is still below');
    expect(document.querySelector('[data-nc-pending-entry-theirs]')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Use their version' })).toBeNull();
  });

  /*
   * ── The refusal matrix, one case per reachable cell ──────────────────────
   *
   * {refusal on the open entry, on another entry, with no editor open} ×
   * {the reader has typed, has not}. Five reachable cells; the sixth — typing
   * with no editor open — cannot be constructed, because typing is what having
   * an editor open means, and that is asserted below rather than left implied.
   *
   * The cells above this block cover "open entry / typed" (the draft survives
   * and the retry carries the reported revision) and "another entry / typed".
   * These are the three that had no case, and the last of them is where the
   * defect lived.
   */

  /* Cell: refusal on the open entry, reader has NOT typed. The editor still
     holds the text it was seeded with, and that is a draft like any other. */
  it('keeps an untouched editor open when its own entry is refused', async () => {
    const onEdit = vi.fn<PendingQueueProps['onEdit']>(() => Promise.resolve(
      { kind: 'stale', text: 'theirs', rev: 9 },
    ));
    render(<PendingQueue entries={[entry()]} overflow={0} busy={false} onEdit={onEdit} onDelete={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => { expect(onEdit).toHaveBeenCalledTimes(1); });

    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Edit queued message' }).value)
      .toBe('look at the report');
    expect(screen.getByRole('button', { name: 'Use their version' })).toBeTruthy();
  });

  /* Cell: refusal on another entry, reader has NOT typed. The untouched
     editor stays open on its own entry; the notice lands on the other row. */
  it('leaves an untouched editor alone when a different entry is refused', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => Promise.resolve(
      { kind: 'stale', text: 'B changed', rev: 4 },
    ));
    render(<PendingQueue
      entries={[entry(), entry({ entry_id: 'e2', text: 'the other one' })]}
      overflow={0} busy={false} onEdit={vi.fn()} onDelete={onDelete}
    />);
    const rows = document.querySelectorAll('[data-nc-pending-entry]');
    fireEvent.click(within(rows[0] as HTMLElement).getByRole('button', { name: 'Edit' }));
    fireEvent.click(within(rows[1] as HTMLElement).getByRole('button', { name: 'Delete' }));
    await waitFor(() => { expect(onDelete).toHaveBeenCalledTimes(1); });

    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Edit queued message' }).value)
      .toBe('look at the report');
    const after = document.querySelectorAll('[data-nc-pending-entry]');
    expect((after[1] as HTMLElement).textContent).toContain('This message changed');
  });

  /*
   * Cell: refusal with NO editor open — the one that had no test, and the one
   * that was broken.
   *
   * A refused DELETE must retry against the revision the server just reported.
   * Storing that revision inside the editor meant it existed only while the
   * reader happened to be editing, so this path re-sent the revision it
   * already knew was stale and could never succeed. The refresh that would
   * eventually fix the cached page is fire-and-forget and may simply fail.
   */
  it('retries a refused delete against the reported revision, with no editor open', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => Promise.resolve(
      { kind: 'stale', text: 'theirs', rev: 9 },
    ));
    render(<PendingQueue entries={[entry({ rev: 3 })]} overflow={0} busy={false} onEdit={vi.fn()} onDelete={onDelete} />);

    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    await waitFor(() => { expect(onDelete).toHaveBeenCalledTimes(1); });
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Delete' }).getAttribute('aria-busy')).not.toBe('true');
    });
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    await waitFor(() => { expect(onDelete).toHaveBeenCalledTimes(2); });

    expect(onDelete.mock.calls[0]?.[0].rev).toBe(3);
    expect(onDelete.mock.calls[1]?.[0].rev).toBe(9);
  });

  /* The unreachable cell, stated rather than assumed: with no editor open
     there is no textarea, so "the reader has typed" cannot be constructed. */
  it('offers nowhere to type until an editor is opened', () => {
    render(<PendingQueue entries={[entry()]} overflow={0} busy={false} onEdit={vi.fn()} onDelete={vi.fn()} />);
    expect(screen.queryByRole('textbox', { name: 'Edit queued message' })).toBeNull();
  });

  it('says so when the entry has already left the queue', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => Promise.resolve({ kind: 'gone' }));
    render(<PendingQueue entries={[entry()]} overflow={0} busy={false} onEdit={vi.fn()} onDelete={onDelete} />);
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    expect((await screen.findByText(/already left the queue/)).textContent).toBeTruthy();
  });

  /*
   * Queued messages this page does not carry — no id (queued before the kernel
   * gave entries one), or past the server's page budget. They are real and
   * they will really be sent, so hiding them would understate what the person
   * has waiting; nothing here can address them, so offering an Edit or a
   * Delete would be offering a button that cannot work.
   */
  it('counts unaddressable queued messages without offering to change them', () => {
    render(<PendingQueue entries={[]} overflow={3} busy={false} onEdit={vi.fn()} onDelete={vi.fn()} />);
    expect(screen.getByText(/3 more queued messages are waiting but cannot be shown or edited here/))
      .toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Delete' })).toBeNull();
  });

  it('counts the unaddressable ones in the total it announces', () => {
    render(<PendingQueue entries={[entry()]} overflow={2} busy={false} onEdit={vi.fn()} onDelete={vi.fn()} />);
    expect(screen.getByText('3 messages are waiting to send when this turn ends.')).toBeTruthy();
  });

  /* One unaddressable entry and nothing else still reads as one message. The
     singular used to require `overflow === 0`, so this said "1 messages". */
  it('says one message when the only one waiting is unaddressable', () => {
    render(<PendingQueue entries={[]} overflow={1} busy={false} onEdit={vi.fn()} onDelete={vi.fn()} />);
    expect(screen.getByText('One message is waiting to send when this turn ends.')).toBeTruthy();
  });
});
