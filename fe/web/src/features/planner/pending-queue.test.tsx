// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
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
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    const notice = await screen.findByText(/changed while you were editing it/);
    expect(notice.textContent).toContain('not saved');
    // Still open, now holding the server's text rather than the reader's.
    expect(screen.getByRole<HTMLTextAreaElement>('textbox', { name: 'Edit queued message' }).value)
      .toBe('somebody else wrote this');
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
});
