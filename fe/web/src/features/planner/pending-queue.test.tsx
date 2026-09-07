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
  const onTakeBack = vi.fn<PendingQueueProps['onTakeBack']>(() => done);
  const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => done);
  const onEcho = vi.fn<PendingQueueProps['onEcho']>();
  const view = render(<PendingQueue
    entries={[entry()]}
    overflow={0}
    busy={false}
    composerBusy={false}
    onTakeBack={onTakeBack}
    onDelete={onDelete}
    onEcho={onEcho}
    {...props}
  />);
  return { onTakeBack, onDelete, onEcho, ...view };
}

function rows(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[data-nc-pending-entry]')];
}

function editButtons(): HTMLButtonElement[] {
  return screen.getAllByRole<HTMLButtonElement>('button', { name: 'Edit this message' });
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
   * The whole of the new interaction. Edit is a take-back: the entry leaves
   * the queue and its words go to the composer, in that order, so the message
   * is never in both places at once.
   */
  it('takes the message back and hands its words to the composer', async () => {
    const { onTakeBack, onEcho, onDelete } = renderQueue();
    await userEvent.click(editButtons()[0]);
    await waitFor(() => expect(onEcho).toHaveBeenCalledWith('look at the report'));
    expect(onTakeBack).toHaveBeenCalledTimes(1);
    expect(onTakeBack.mock.calls[0]?.[0]?.entry_id).toBe('e1');
    /* A take-back is a delete of its own; it must not ALSO go through the
       delete button's path, which would be two writes for one press. */
    expect(onDelete).not.toHaveBeenCalled();
  });

  /*
   * The ordering above is the invariant, and this is the half that can go
   * wrong silently: echo the words after a REFUSED take-back and the message
   * is in the queue and in the composer, so sending it posts it twice.
   */
  it('does not hand the words over when the take-back was refused', async () => {
    const { onEcho } = renderQueue({
      onTakeBack: vi.fn<PendingQueueProps['onTakeBack']>(
        () => Promise.resolve({ kind: 'stale', text: 'somebody else wrote this', rev: 7 }),
      ),
    });
    await userEvent.click(editButtons()[0]);
    await screen.findByText(/Nothing happened/);
    expect(onEcho).not.toHaveBeenCalled();
  });

  it('does not hand the words over when the message already left the queue', async () => {
    const { onEcho } = renderQueue({
      onTakeBack: vi.fn<PendingQueueProps['onTakeBack']>(() => Promise.resolve({ kind: 'gone' })),
    });
    await userEvent.click(editButtons()[0]);
    await screen.findByText(/Already sent/);
    expect(onEcho).not.toHaveBeenCalled();
  });

  /*
   * Taking a message back REPLACES the composer's contents. Offering it over
   * a half-written sentence is the one outcome that destroys something the
   * person cannot get back, so it is refused before it happens rather than
   * apologised for after.
   */
  it('refuses to take a message back over words already being written', async () => {
    const { onTakeBack, onEcho } = renderQueue({ composerBusy: true });
    const button = editButtons()[0];
    expect(button.disabled || button.getAttribute('aria-disabled') === 'true').toBe(true);
    await userEvent.hover(button);
    expect(await screen.findByText(/Send or clear what you are writing first/)).toBeTruthy();
    expect(onTakeBack).not.toHaveBeenCalled();
    expect(onEcho).not.toHaveBeenCalled();
  });

  it('deletes without echoing anything', async () => {
    const { onDelete, onEcho, onTakeBack } = renderQueue();
    await userEvent.click(screen.getByRole('button', { name: 'Delete this message' }));
    await waitFor(() => expect(onDelete).toHaveBeenCalledTimes(1));
    expect(onEcho).not.toHaveBeenCalled();
    expect(onTakeBack).not.toHaveBeenCalled();
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

  it('blocks both controls while another write on this card is unanswered', () => {
    renderQueue({ busy: true });
    for (const name of ['Edit this message', 'Delete this message']) {
      const button = screen.getByRole<HTMLButtonElement>('button', { name });
      expect(button.disabled || button.getAttribute('aria-disabled') === 'true').toBe(true);
    }
  });

  /*
   * Entries this page does not carry: written before #1505 PR1 (no id, never
   * gains one) or past the page budget. Nothing here can address them, so
   * they are counted and given no buttons — but they are counted, because a
   * person who typed eleven and sees three has been misinformed.
   */
  it('counts the messages it cannot show, and offers them no controls', () => {
    renderQueue({ entries: [], overflow: 3 });
    expect(screen.getByText(/3 more queued messages are waiting but cannot be shown/))
      .toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Edit this message' })).toBeNull();
  });

  it('counts unaddressable messages in the caption too', () => {
    renderQueue({ entries: [entry()], overflow: 2 });
    expect(screen.getByText('3 messages are waiting to send when this turn ends.')).toBeTruthy();
  });

  /* The singular used to require `overflow === 0`, so this said "1 messages". */
  it('says one message when the only one waiting is unaddressable', () => {
    renderQueue({ entries: [], overflow: 1 });
    expect(screen.getByText('One message is waiting to send when this turn ends.')).toBeTruthy();
  });
});
