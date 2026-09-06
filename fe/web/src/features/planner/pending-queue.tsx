// #1505 PR4 — the messages a person typed while a turn was running, and the
// two things they could never do with them: change one, or take one back.
//
// This is a pure presentation module. It is handed the queue page and two
// write callbacks; it owns no query, no transport and no card id, so the same
// component serves the drawer and any test that wants to drive one entry
// through every outcome. The writes are compare-and-swap: each entry carries
// the `rev` it was read at, and a refusal is reported to the reader rather
// than retried, because a silent retry would overwrite text they never saw.

import { Button } from '@astryxdesign/core/Button';
import { TextArea } from '@astryxdesign/core/TextArea';
import type {
  PendingQueueEntry, PlannerQueueWriteOutcome,
} from '../../../../core/domain/conversation.ts';
import { useState } from '../../ui/state/public.ts';
import styles from './pending-queue.module.css';

export type PendingQueueProps = Readonly<{
  entries: readonly PendingQueueEntry[];
  /**
   * Queued user messages this page does not carry.
   *
   * Two different reasons land in the same count, and the server does not
   * separate them (`page_pending_entries`, `routes/cards.rs`): an entry
   * written before #1505 PR1, which has no id and never gains one, and an
   * entry past the page's own length/byte budget. Neither can be addressed
   * from what this component was given — the first has nothing to address,
   * the second was not sent — so both are counted and neither gets buttons.
   *
   * Rendered rather than hidden: they are real messages that will really be
   * sent, and a person who typed eleven and sees three has been misinformed.
   */
  overflow: number;
  /** Blocks the controls while any write on this card is unanswered. */
  busy: boolean;
  onEdit: (entry: PendingQueueEntry, text: string) => Promise<PlannerQueueWriteOutcome>;
  onDelete: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
}>;

type RowState = Readonly<{
  entryId: string;
  /** The draft being edited, or `null` while the row is just being read. */
  draft: string | null;
  notice: PlannerQueueWriteOutcome | null;
  /**
   * The revision the next Save must be written against.
   *
   * `null` until something tells us better, in which case the entry's own
   * `rev` from the queue page is used. After a 409 it holds the revision the
   * server just reported, which is NOT the same thing: the page in the cache
   * is by definition behind at that moment, and the refresh that fixes it may
   * still be in flight or may fail. Saving `entry.rev` again would 409 again.
   */
  rev: number | null;
}>;

function noticeText(outcome: PlannerQueueWriteOutcome): string | null {
  if (outcome.kind === 'stale') {
    return 'This message changed while you were editing it, so your version was not saved. '
      + 'Your text is still below — save it again to overwrite theirs, or use theirs instead.';
  }
  if (outcome.kind === 'gone') {
    return 'This message already left the queue, so it could not be changed.';
  }
  if (outcome.kind === 'failed') return outcome.message;
  return null;
}

export function PendingQueue({ entries, overflow, busy, onEdit, onDelete }: PendingQueueProps) {
  const [row, setRow] = useState<RowState | null>(null);

  if (entries.length === 0 && overflow === 0) return null;

  const settle = (entryId: string, outcome: PlannerQueueWriteOutcome): void => {
    setRow((current) => {
      const open = current?.entryId === entryId ? current : null;
      /*
       * A lost race KEEPS the reader's draft.
       *
       * The first version of this replaced the draft with the server's text,
       * which meant the sentence the reader had just typed existed nowhere —
       * not in state, not in the notice, with no undo — and the notice saying
       * "your version was not saved" was true and useless. Their words are the
       * one thing here that cannot be fetched again.
       *
       * What the 409 does change is the revision the next Save carries: the
       * one the server just reported, not the one on the cached page. The
       * winning text is offered beside the notice, so choosing it is a
       * deliberate act rather than something that happens to them.
       */
      if (outcome.kind === 'stale') {
        return { entryId, draft: open?.draft ?? null, notice: outcome, rev: outcome.rev };
      }
      if (outcome.kind === 'done') return open === null ? current : null;
      /* A refusal is reported even when no editor was open — a delete is a
         write too, and a delete that did not happen is exactly the thing this
         surface must not let pass silently. */
      return { entryId, draft: open?.draft ?? null, notice: outcome, rev: open?.rev ?? null };
    });
  };

  return (
    <section className={styles.queue} data-nc-pending-queue="" aria-label="Queued messages">
      <p className={styles.caption} data-nc-pending-queue-caption="">
        {entries.length + overflow === 1
          ? 'One message is waiting to send when this turn ends.'
          : `${entries.length + overflow} messages are waiting to send when this turn ends.`}
      </p>
      <ul className={styles.list}>
        {entries.map((entry) => {
          const open = row?.entryId === entry.entry_id ? row : null;
          const draft = open?.draft ?? null;
          const notice = open?.notice == null ? null : noticeText(open.notice);
          const lostRace = open?.notice?.kind === 'stale' ? open.notice : null;
          /* The revision a Save would be written against: the one the server
             reported if it has spoken, otherwise the one this page was read
             at. Never `entry.rev` after a 409 — that value is known stale. */
          const saveRev = open?.rev ?? entry.rev;
          return (
            <li key={entry.entry_id} className={styles.item} data-nc-pending-entry={entry.entry_id}>
              {draft === null
                ? <p className={styles.text}>{entry.text}</p>
                : (
                  <TextArea
                    label="Edit queued message"
                    isLabelHidden
                    rows={3}
                    value={draft}
                    isDisabled={busy}
                    onChange={(value: string) => {
                      setRow((current) => current?.entryId === entry.entry_id
                        ? { ...current, draft: value } : current);
                    }}
                  />
                )}
              {notice !== null && (
                <p className={styles.notice} role="status" data-nc-pending-entry-notice="">{notice}</p>
              )}
              {lostRace !== null && (
                <div className={styles.theirs} data-nc-pending-entry-theirs="">
                  <p className={styles.text}>{lostRace.text}</p>
                  <Button
                    label="Use their version"
                    variant="ghost"
                    size="sm"
                    isDisabled={busy}
                    onClick={() => {
                      setRow((current) => current?.entryId === entry.entry_id
                        ? { ...current, draft: lostRace.text } : current);
                    }}
                  />
                </div>
              )}
              <div className={styles.actions}>
                {draft === null
                  ? (
                    <>
                      <Button
                        label="Edit"
                        variant="ghost"
                        size="sm"
                        isDisabled={busy}
                        /* `onClick`, not `clickAction`: opening an editor is
                           local state, and Astryx's action slot shows a
                           spinner and disables the control until its promise
                           settles, which is a lie about a synchronous
                           toggle. The two writes below do use it. */
                        onClick={() => {
                          setRow({
                            entryId: entry.entry_id, draft: entry.text, notice: null, rev: null,
                          });
                        }}
                      />
                      <Button
                        label="Delete"
                        variant="ghost"
                        size="sm"
                        isDisabled={busy}
                        clickAction={async () => {
                          settle(entry.entry_id, await onDelete({ ...entry, rev: saveRev }));
                        }}
                      />
                    </>
                  )
                  : (
                    <>
                      <Button
                        label="Save"
                        variant="primary"
                        size="sm"
                        isDisabled={busy || draft.trim() === ''}
                        clickAction={async () => {
                          settle(entry.entry_id, await onEdit({ ...entry, rev: saveRev }, draft));
                        }}
                      />
                      <Button
                        label="Cancel"
                        variant="ghost"
                        size="sm"
                        isDisabled={busy}
                        onClick={() => { setRow(null); }}
                      />
                    </>
                  )}
              </div>
            </li>
          );
        })}
      </ul>
      {overflow > 0 && (
        <p className={styles.overflow} role="status" data-nc-pending-overflow="">
          {overflow === 1
            ? '1 more queued message is waiting but cannot be shown or edited here.'
            : `${overflow} more queued messages are waiting but cannot be shown or edited here.`}
        </p>
      )}
    </section>
  );
}
