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

/**
 * The one open editor, if any.
 *
 * Separate from {@link Notice} because they belong to different entries: a
 * refusal on the row you just clicked Delete on must not reach into the row you
 * were typing in. Keeping them in one value is what let a 409 on B replace A's
 * state and destroy A's unsaved sentence with no undo.
 */
type OpenEditor = Readonly<{
  entryId: string;
  draft: string;
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

/** The last refusal, and which entry it was about. */
type Notice = Readonly<{ entryId: string; outcome: PlannerQueueWriteOutcome }>;

/**
 * What a refusal says, which depends on whether this reader has an editor open
 * on that entry.
 *
 * With no editor open there is no "your text" to point at, and a 409 on a
 * delete did not fail to save anything — it failed to remove something. Saying
 * otherwise describes a screen the reader is not looking at.
 */
function noticeText(outcome: PlannerQueueWriteOutcome, editing: boolean): string | null {
  if (outcome.kind === 'stale') {
    return editing
      ? 'This message changed while you were editing it, so your version was not saved. '
        + 'Your text is still below — save it again to overwrite theirs, or use theirs instead.'
      : 'This message changed before your change could be applied, so nothing happened to it. '
        + 'It now reads as shown above; try again if you still want to.';
  }
  if (outcome.kind === 'gone') {
    return 'This message already left the queue, so it could not be changed.';
  }
  if (outcome.kind === 'failed') return outcome.message;
  return null;
}

export function PendingQueue({ entries, overflow, busy, onEdit, onDelete }: PendingQueueProps) {
  const [editor, setEditor] = useState<OpenEditor | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);

  if (entries.length === 0 && overflow === 0) return null;

  const settle = (entryId: string, outcome: PlannerQueueWriteOutcome): void => {
    /*
     * A lost race KEEPS the reader's draft, and touches only the entry the
     * write was about.
     *
     * The first version replaced the draft with the server's text, so the
     * sentence just typed existed nowhere — not in state, not in the notice,
     * no undo. The second kept it but stored it beside the notice, so a
     * refusal on a DIFFERENT entry replaced the whole value and destroyed it
     * anyway. Their words are the one thing here that cannot be fetched again,
     * so the editor is now only ever written when it is the editor for this
     * entry.
     *
     * What a 409 does change is the revision the next Save carries: the one
     * the server just reported, not the one on the cached page.
     */
    setNotice(outcome.kind === 'done' ? null : { entryId, outcome });
    setEditor((current) => {
      if (current?.entryId !== entryId) return current;
      if (outcome.kind === 'done') return null;
      if (outcome.kind === 'stale') return { ...current, rev: outcome.rev };
      return current;
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
          const open = editor?.entryId === entry.entry_id ? editor : null;
          const draft = open?.draft ?? null;
          const shown = notice?.entryId === entry.entry_id ? notice.outcome : null;
          const noticeLine = shown === null ? null : noticeText(shown, draft !== null);
          /* The winner's text is offered as a REPLACEMENT, so it is only
             offered where there is something to replace. With no editor open
             the notice quotes the entry itself, which the row already shows. */
          const lostRace = shown?.kind === 'stale' && draft !== null ? shown : null;
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
                      setEditor((current) => current?.entryId === entry.entry_id
                        ? { ...current, draft: value } : current);
                    }}
                  />
                )}
              {noticeLine !== null && (
                <p className={styles.notice} role="status" data-nc-pending-entry-notice="">
                  {noticeLine}
                </p>
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
                      setEditor((current) => current?.entryId === entry.entry_id
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
                          setEditor({ entryId: entry.entry_id, draft: entry.text, rev: null });
                          setNotice(null);
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
                        onClick={() => { setEditor(null); setNotice(null); }}
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
