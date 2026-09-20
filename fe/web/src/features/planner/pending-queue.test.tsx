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

/** Wait until a control will accept a press: Astryx's `clickAction` holds the control while its promise is unsettled, and a press inside that window is dropped rather than queued. */
async function pressable(find: () => HTMLButtonElement): Promise<void> {
  /* A getter, not an element: the strip re-renders while the write is in flight, so an element captured beforehand is detached and waiting on it waits forever. */
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

  it('counts the messages it cannot show, and offers them no controls', () => {
    renderQueue({ entries: [], overflow: 3 });
    expect(screen.getByText('3 queued messages are waiting but cannot be shown or edited here.'))
      .toBeTruthy();
    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });

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

  it('offers "Say it now" only when a steer handler is given', () => {
    renderQueue();
    expect(screen.queryByRole('button', { name: 'Say it now' })).toBeNull();
    expect(screen.getAllByRole('button')).toHaveLength(1);
    cleanup();

    const onSteer = vi.fn<NonNullable<PendingQueueProps['onSteer']>>(() => done);
    renderQueue({ onSteer });
    expect(screen.getByRole('button', { name: 'Say it now' })).toBeTruthy();
    expect(screen.getAllByRole('button')).toHaveLength(2);
  });

  it('hands the entry, at the revision shown, to the steer handler', async () => {
    const onSteer = vi.fn<NonNullable<PendingQueueProps['onSteer']>>(() => done);
    renderQueue({ entries: [entry({ rev: 4 })], onSteer });
    await userEvent.click(screen.getByRole('button', { name: 'Say it now' }));
    await waitFor(() => expect(onSteer).toHaveBeenCalledTimes(1));
    expect(onSteer.mock.calls[0]?.[0]).toMatchObject({ entry_id: 'e1', rev: 4 });
  });

  it('says the message stays queued when no turn took it', async () => {
    const onSteer = vi.fn<NonNullable<PendingQueueProps['onSteer']>>(
      () => Promise.resolve({ kind: 'not_running' }),
    );
    renderQueue({ onSteer });
    await userEvent.click(screen.getByRole('button', { name: 'Say it now' }));
    expect(await screen.findByText(/Still queued/)).toBeTruthy();
    expect(screen.getByText(/stays queued and will go with the next turn/)).toBeTruthy();
    expect(rows()).toHaveLength(1);
    expect(screen.getByRole('button', { name: 'Delete this message' })).toBeTruthy();
  });

  it('says the outcome is not known when codex never answered the steer', async () => {
    const onSteer = vi.fn<NonNullable<PendingQueueProps['onSteer']>>(
      () => Promise.resolve({ kind: 'unanswered' }),
    );
    renderQueue({ onSteer });
    await userEvent.click(screen.getByRole('button', { name: 'Say it now' }));
    expect(await screen.findByText(/Still queued — not confirmed/)).toBeTruthy();
    const notice = screen.getByText(/not known whether this message reached/);
    expect(notice.textContent).toMatch(/stays queued and will go with the next turn/);
    expect(notice.textContent).toMatch(/will also show up in the conversation/);
    expect(notice.textContent).not.toMatch(/nothing happened/);
    expect(rows()).toHaveLength(1);
  });

  it('locks every other control while a steer is in flight', async () => {
    let settle!: (outcome: PlannerQueueWriteOutcome) => void;
    const onSteer = vi.fn<NonNullable<PendingQueueProps['onSteer']>>(
      () => new Promise((resolve) => { settle = resolve; }),
    );
    renderQueue({
      entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })],
      onSteer,
    });
    await userEvent.click(screen.getAllByRole('button', { name: 'Say it now' })[0]);
    await waitFor(() => expect(onSteer).toHaveBeenCalledTimes(1));
    await waitFor(() => {
      for (const button of screen.getAllByRole<HTMLButtonElement>('button')) {
        expect(button.disabled || button.getAttribute('aria-disabled') === 'true').toBe(true);
      }
    });
    settle({ kind: 'done' });
    await Promise.resolve();
  });

  it('says nothing above the bubbles', () => {
    renderQueue({ entries: [entry(), entry({ entry_id: 'e2', text: 'and the diff' })] });
    expect(document.querySelector('[data-nc-pending-queue-caption]')).toBeNull();
    expect(screen.queryByText(/waiting to send when this turn ends/)).toBeNull();
  });

});
