// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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

function renderQueue(props: Partial<PendingQueueProps> = {}) {
  const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => done);
  const view = render(<PendingQueue
    entries={[entry()]}
    overflow={0}
    busy={false}
    onDelete={onDelete}
    {...props}
  />);
  return { onDelete, ...view };
}

function rows(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-nc-pending-entry]')];
}

/**
 * Wait until a control will actually accept a press.
 *
 * Astryx's `clickAction` holds the control it is on for as long as its promise
 * is unsettled, and a press landing inside that window is dropped rather than
 * queued. A retry therefore has to wait for the button to let go, and a test
 * that does not wait is testing the vendor's dedupe, not the retry.
 */
async function pressable(find: () => HTMLButtonElement): Promise<void> {
  /* A getter and not an element: the strip re-renders while the write is in
     flight, so an element captured beforehand is detached by the time it would
     have been re-enabled, and waiting on it waits forever. */
  await waitFor(() => {
    const button = find();
    expect(button.disabled).toBe(false);
    expect(button.getAttribute('aria-busy')).not.toBe('true');
  });
}

describe('PendingQueue', () => {
  it('shows the text of every queued message', () => {
    renderQueue({ entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })] });
    expect(screen.getByText('look at the report')).toBeTruthy();
    expect(screen.getByText('and the diff')).toBeTruthy();
  });

  it('renders nothing when there is no queue', () => {
    const { container } = renderQueue({ entries: [] });
    expect(container.querySelector('[data-nc-pending-queue]')).toBeNull();
  });

  /*
   * The revision a refused write reports, and why it is read from the refusal
   * rather than from the row. The page in the cache is behind at that moment
   * and the refresh that would fix it is fire-and-forget, so a retry sending
   * `entry.rev` again is guaranteed to lose again — forever.
   */
  it('retries against the revision the server reported, not the stale one', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>()
      .mockResolvedValueOnce({ kind: 'stale', text: 'theirs', rev: 9 })
      .mockResolvedValueOnce({ kind: 'done' });
    renderQueue({ entries: [entry({ rev: 3 })], onDelete });
    await userEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await screen.findByText(/Nothing happened/);
    expect(onDelete.mock.calls[0]?.[0]?.rev).toBe(3);

    await pressable(() => screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(onDelete).toHaveBeenCalledTimes(2));
    expect(onDelete.mock.calls[1]?.[0]?.rev).toBe(9);
  });

  /* A refusal belongs to the entry it was about. It used to be stored beside
     the open editor, so a refusal on one row wiped state on another. */
  it('shows a refusal on its own row and not on any other', async () => {
    const onDelete = vi.fn<PendingQueueProps['onDelete']>()
      .mockResolvedValue({ kind: 'stale', text: 'theirs', rev: 9 });
    renderQueue({
      entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })],
      onDelete,
    });
    await userEvent.click(
      screen.getAllByRole('button', { name: 'Delete this message' })[1],
    );
    await waitFor(() => expect(rows()[1]?.textContent).toContain('Nothing happened'));
    expect(rows()[0]?.textContent).not.toContain('Nothing happened');
  });

  it('says the server’s own sentence when the write simply failed', async () => {
    renderQueue({
      onDelete: vi.fn<PendingQueueProps['onDelete']>(
        () => Promise.resolve({ kind: 'failed', message: 'the kernel is not accepting writes' }),
      ),
    });
    await userEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    expect(await screen.findByText(/the kernel is not accepting writes/)).toBeTruthy();
  });

  it('blocks the control while another write on this card is unanswered', () => {
    renderQueue({ busy: true });
    const button = screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' });
    expect(button.disabled || button.getAttribute('aria-disabled') === 'true').toBe(true);
  });

  /*
   * One lock for the strip, not one per button.
   *
   * A queue write is a compare-and-swap against the revision this page was
   * read at, so two in flight together means the second was composed against a
   * page the first has already invalidated. Astryx's `clickAction` holds only
   * the control it is on, so the other row's cross stayed live.
   */
  it('locks the other entry’s control while one delete is in flight', async () => {
    let settle!: (outcome: PlannerQueueWriteOutcome) => void;
    const onDelete = vi.fn<PendingQueueProps['onDelete']>(
      () => new Promise((resolve) => { settle = resolve; }),
    );
    renderQueue({
      entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })],
      onDelete,
    });
    await userEvent.click(screen.getAllByRole('button', { name: 'Delete this message' })[0]);
    await waitFor(() => expect(onDelete).toHaveBeenCalledTimes(1));
    await waitFor(() => {
      const other = rows()[1].querySelector('button');
      expect(other?.disabled || other?.getAttribute('aria-disabled') === 'true').toBe(true);
    });
    settle({ kind: 'done' });
    await Promise.resolve();
  });

  /*
   * Entries this page does not carry: written before #1505 PR1 (no id, never
   * gains one) or past the page budget. Nothing here can address them, so
   * they get no bubble and no buttons — but they are still counted, because a
   * person who typed eleven and sees three has been misinformed.
   *
   * This one line of prose is all that survived the caption's removal, and it
   * survived because it is the only place these messages exist at all.
   */
  it('counts the messages it cannot show, and offers them no controls', () => {
    renderQueue({ entries: [], overflow: 3 });
    expect(screen.getByText('3 queued messages are waiting but cannot be shown or edited here.'))
      .toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Edit this message' })).toBeNull();
  });

  /* "more" only where there is something for them to be more than — with no
     bubbles on screen there is nothing this count is in addition to. */
  it('says “more” only when some of the queue is actually shown', () => {
    renderQueue({ entries: [entry()], overflow: 2 });
    expect(screen.getByText('2 more queued messages are waiting but cannot be shown or edited here.'))
      .toBeTruthy();
  });

  it('agrees with itself about one', () => {
    renderQueue({ entries: [], overflow: 1 });
    expect(screen.getByText('1 queued message is waiting but cannot be shown or edited here.'))
      .toBeTruthy();
  });

  /*
   * The caption over the bubbles is gone at the owner's call — a sentence
   * explaining a picture that explains itself. This pins its absence so it
   * cannot creep back in as somebody's "helpful" addition.
   */
  it('says nothing above the bubbles', () => {
    renderQueue({ entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })] });
    expect(document.querySelector('[data-nc-pending-queue-caption]')).toBeNull();
    expect(screen.queryByText(/waiting to send when this turn ends/)).toBeNull();
  });

});
