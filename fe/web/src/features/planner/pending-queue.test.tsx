// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  PendingQueueEntry, PlannerQueueWriteOutcome,
} from '../../../../core/domain/conversation.ts';
import { PendingQueue, type PendingQueueProps } from './pending-queue.tsx';

afterEach(cleanup);

const done: Promise<PlannerQueueWriteOutcome> = Promise.resolve({ kind: 'done' });

function entry(overrides: Partial<PendingQueueEntry> = {}): PendingQueueEntry {
  return {
    entry_id: 'e1', text: 'look at the report', rev: 0, queued_at_ms: 10,
    attachments: [], ...overrides,
  };
}

const IMAGE = {
  id: 'att-1', contentType: 'image/png', size: 12, url: '/api/cards/c1/planner/attachments/att-1',
};

function renderQueue(props: Partial<PendingQueueProps> = {}) {
  const onTakeBack = vi.fn<PendingQueueProps['onTakeBack']>(() => done);
  const onDelete = vi.fn<PendingQueueProps['onDelete']>(() => done);
  const onEcho = vi.fn<PendingQueueProps['onEcho']>();
  const view = render(<PendingQueue
    entries={[entry()]}
    overflow={0}
    busy={false}
    composerBusy={false}
    cardId="card-1"
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
    await waitFor(() => expect(onEcho).toHaveBeenCalledWith('card-1', 'look at the report'));
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
    /*
     * ACTIVATED, not merely hovered. Astryx switches a tooltipped disabled
     * button to `aria-disabled` so the reason stays keyboard-reachable — which
     * means the element is still clickable and still focusable, and an
     * implementation that kept the attribute while letting the handler run
     * would satisfy an assertion that only looked at the attribute.
     * `fireEvent`, because a real pointer is stopped by the vendor's
     * `pointer-events`, and what is under test is the handler behind it.
     */
    fireEvent.click(button);
    button.focus();
    fireEvent.keyDown(button, { key: 'Enter' });
    fireEvent.keyDown(button, { key: ' ' });
    await Promise.resolve();
    expect(onTakeBack).not.toHaveBeenCalled();
    expect(onEcho).not.toHaveBeenCalled();
  });

  /*
   * The guard is re-asked when the words actually move, not only when the
   * button was drawn. Start over an empty composer, type while the DELETE is
   * in flight, and the answer used to land on top of what had just been typed.
   */
  it('does not overwrite words typed while the take-back was in flight', async () => {
    let settle!: (outcome: PlannerQueueWriteOutcome) => void;
    const onTakeBack = vi.fn<PendingQueueProps['onTakeBack']>(
      () => new Promise((resolve) => { settle = resolve; }),
    );
    const { onEcho, rerender } = renderQueue({ onTakeBack });
    await userEvent.click(editButtons()[0]);
    await waitFor(() => expect(onTakeBack).toHaveBeenCalledTimes(1));

    /* The composer gains words while the request is open. */
    rerender(<PendingQueue
      entries={[entry()]} overflow={0} busy={false} composerBusy
      cardId="card-1" onTakeBack={onTakeBack} onDelete={vi.fn()} onEcho={onEcho}
    />);
    settle({ kind: 'done' });
    await Promise.resolve();
    await Promise.resolve();
    expect(onEcho).not.toHaveBeenCalled();
  });

  /*
   * A queued message is not only its words. The server ships the images with
   * the entry; handing back the text alone drops them, and for an image-only
   * message that is the whole message.
   */
  it('refuses to take back a message that carries images, and says why', async () => {
    const { onTakeBack } = renderQueue({ entries: [entry({ attachments: [IMAGE] })] });
    const button = editButtons()[0];
    expect(button.disabled || button.getAttribute('aria-disabled') === 'true').toBe(true);
    await userEvent.hover(button);
    expect(await screen.findByText(/carries images/)).toBeTruthy();
    fireEvent.click(button);
    await Promise.resolve();
    expect(onTakeBack).not.toHaveBeenCalled();
    /* The cross still works: deleting a message with pictures loses the
       pictures, which is what deleting means. */
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Delete this message' }).disabled)
      .toBe(false);
  });

  /*
   * One lock for the strip, not one per button. Astryx's `clickAction`
   * disables only the control it is on, so two pencils pressed in quick
   * succession issued two take-backs: both entries deleted, only the second
   * answer's words kept.
   */
  it('locks every control while one write is in flight', async () => {
    let settle!: (outcome: PlannerQueueWriteOutcome) => void;
    const onTakeBack = vi.fn<PendingQueueProps['onTakeBack']>(
      () => new Promise((resolve) => { settle = resolve; }),
    );
    renderQueue({
      entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })],
      onTakeBack,
    });
    await userEvent.click(editButtons()[0]);
    await waitFor(() => expect(onTakeBack).toHaveBeenCalledTimes(1));

    /* The SECOND row. Astryx already holds the control that was pressed; what
       this pins is that the untouched entry's controls went with it, which is
       the part `clickAction` does not do and the part the two-take-backs bug
       came through. */
    await waitFor(() => {
      const row = document.querySelectorAll('[data-nc-pending-entry]')[1] as HTMLElement;
      const buttons = [...row.querySelectorAll('button')];
      expect(buttons).toHaveLength(2);
      for (const button of buttons) {
        expect(button.disabled || button.getAttribute('aria-disabled') === 'true').toBe(true);
      }
    });
    settle({ kind: 'done' });
    await Promise.resolve();
  });

  /*
   * The words go back to the conversation they were taken from. This drawer is
   * reused across conversations, so a DELETE issued in A can answer while B is
   * open; the id travels with the echo so the caller can refuse it.
   */
  it('names the conversation the words came from', async () => {
    const { onEcho } = renderQueue({ cardId: 'card-7' });
    await userEvent.click(editButtons()[0]);
    await waitFor(() => expect(onEcho).toHaveBeenCalledWith('card-7', 'look at the report'));
  });

  /*
   * A stale refusal is the server saying what the entry reads NOW. From then
   * on the row shows those words and a retry hands those back — reading the
   * page's own stale text deleted the new message and returned the old one.
   */
  it('shows and returns the winner’s words after a lost race, not the ones it was read with', async () => {
    const onTakeBack = vi.fn<PendingQueueProps['onTakeBack']>()
      .mockResolvedValueOnce({ kind: 'stale', text: 'they rewrote it', rev: 9 })
      .mockResolvedValueOnce({ kind: 'done' });
    const { onEcho } = renderQueue({ entries: [entry({ rev: 3 })], onTakeBack });

    await userEvent.click(editButtons()[0]);
    await screen.findByText(/Nothing happened/);
    expect(screen.getByText('they rewrote it')).toBeTruthy();
    expect(screen.queryByText('look at the report')).toBeNull();

    await pressable(() => editButtons()[0]);
    await userEvent.click(editButtons()[0]);
    await waitFor(() => expect(onEcho).toHaveBeenCalledWith('card-1', 'they rewrote it'));
    expect(onTakeBack.mock.calls[1]?.[0]?.rev).toBe(9);
  });

  /*
   * The ambiguous outcome. A transport failure cannot tell "refused" from
   * "deleted, answer lost on the way back". Withholding the words is right for
   * the first and loses the message for the second, so it resolves toward the
   * recoverable error: hand them over, and say the queue may still hold it.
   */
  it('hands the words back on an ambiguous failure, and says the queue may still hold it', async () => {
    const { onEcho } = renderQueue({
      onTakeBack: vi.fn<PendingQueueProps['onTakeBack']>(
        () => Promise.resolve({ kind: 'failed', message: 'the connection dropped.' }),
      ),
    });
    await userEvent.click(editButtons()[0]);
    await waitFor(() => expect(onEcho).toHaveBeenCalledWith('card-1', 'look at the report'));
    expect(await screen.findByText(/check the queue above before sending them again/))
      .toBeTruthy();
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
