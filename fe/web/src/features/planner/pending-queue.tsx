// #1505 PR4 — the messages a person typed while a turn was running, and the
// two things they could never do with them: change one, or take one back.
//
// This is a pure presentation module. It is handed the queue page and two
// write callbacks; it owns no query, no transport and no card id, so the same
// component serves the drawer and any test that wants to drive one entry
// through every outcome. The writes are compare-and-swap: each entry carries
// the `rev` it was read at, and a refusal is reported to the reader rather
// than retried, because a silent retry would overwrite text they never saw.

import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { HStack } from '@astryxdesign/core/HStack';
import { List } from '@astryxdesign/core/List';
import { Text } from '@astryxdesign/core/Text';
import { TextArea } from '@astryxdesign/core/TextArea';
import { VStack } from '@astryxdesign/core/VStack';
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

/*
 * Two facts live here, they have different owners, and every bug this
 * component has had came from storing them in one place.
 *
 *   - {@link OpenEditor} — THE READER'S WORDS. Owned by the reader, keyed to
 *     the entry they opened. Nothing the server says may write it.
 *   - {@link Refusal} — WHAT THE SERVER LAST SAID about one entry, including
 *     the revision a 409 reported. Owned by the server, keyed to the entry the
 *     write was about, and true whether or not an editor happens to be open.
 *
 * The matrix this shape has to satisfy — {refusal on the open entry, on
 * another entry, with no editor open} × {the reader has typed, has not}:
 *
 * | refusal target | typed | draft        | wording     | "Use theirs" | next write's rev |
 * |----------------|-------|--------------|-------------|--------------|------------------|
 * | the open entry | yes   | kept         | editing     | offered      | server-reported  |
 * | the open entry | no    | kept         | editing     | offered      | server-reported  |
 * | another entry  | yes   | kept         | non-editing | no           | server-reported  |
 * | another entry  | no    | kept         | non-editing | no           | server-reported  |
 * | no editor open | yes   | UNREACHABLE — typing requires an open editor    |||
 * | no editor open | no    | none to keep | non-editing | no           | server-reported  |
 *
 * Five reachable cells, and they collapse to two rules once the ownership is
 * right: a refusal NEVER writes a draft, and a refusal ALWAYS records the
 * revision for its entry. Both are properties of where the values live, not of
 * branches in `settle`.
 *
 * The three defects, all the same mistake at different depths: the draft was
 * overwritten with the server's text; then kept but stored beside the notice,
 * so a refusal on another entry destroyed it; then separated, but with the
 * server's revision left INSIDE the editor — so a fact about an entry only
 * existed while the reader happened to be editing that entry, and a refused
 * delete with no editor open retried against a revision it already knew was
 * stale, forever. That last one is the cell with no test, which is why two
 * readings of the neighbouring cell could disagree without either being
 * obviously wrong.
 */
type OpenEditor = Readonly<{ entryId: string; draft: string }>;

/**
 * The last refusal, and which entry it was about.
 *
 * The `stale` variant carries the revision the server reported, and reading it
 * from HERE rather than from the editor is what makes the bottom row of the
 * matrix work: the page in the cache is behind by definition at that moment,
 * and the refresh that would fix it is fire-and-forget and may fail, so a
 * retry that re-sends `entry.rev` is guaranteed to lose again.
 */
type Refusal = Readonly<{ entryId: string; outcome: PlannerQueueWriteOutcome }>;

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

/**
 * The heading over that sentence, and the colour it is said in.
 *
 * Split out because a refusal now renders as a `Banner`, and a Banner is a
 * heading plus a body plus a status — where the previous version was one grey
 * paragraph in `var(--text-warning, var(--text-3))`. `--text-warning` is not
 * defined in `tokens.css` at all (the warning family is `--warn`, `--warn-text`,
 * `--warn-border`, `--warn-soft`), so that fallback was not a fallback: the
 * notice rendered as ordinary secondary text in every theme, always.
 * "Your change was not saved" looked exactly like the message it was about,
 * which is the one thing it must not look like.
 *
 * The three statuses are not decoration: `stale` is a race the reader can
 * still win by trying again (warning), `gone` is the queue having moved on
 * without them and nothing to retry (info), and `failed` is the server
 * refusing (error).
 */
function noticeHeading(outcome: PlannerQueueWriteOutcome, editing: boolean): string {
  if (outcome.kind === 'stale') return editing ? 'Not saved' : 'Nothing happened';
  if (outcome.kind === 'gone') return 'Already sent';
  return 'Could not be changed';
}

function noticeStatus(outcome: PlannerQueueWriteOutcome): 'warning' | 'info' | 'error' {
  if (outcome.kind === 'stale') return 'warning';
  if (outcome.kind === 'gone') return 'info';
  return 'error';
}

export function PendingQueue({ entries, overflow, busy, onEdit, onDelete }: PendingQueueProps) {
  const [editor, setEditor] = useState<OpenEditor | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);

  if (entries.length === 0 && overflow === 0) return null;

  const settle = (entryId: string, outcome: PlannerQueueWriteOutcome): void => {
    /*
     * Every refusal is recorded against its own entry — that is the whole of
     * the bottom matrix row, and it is why this is not a branch.
     */
    setRefusal(outcome.kind === 'done' ? null : { entryId, outcome });
    /*
     * The editor is closed only by a write that SUCCEEDED, and only its own.
     * A refusal never touches it, so no cell of the matrix can reach a
     * reader's unsaved sentence.
     */
    if (outcome.kind === 'done') {
      setEditor((current) => current?.entryId === entryId ? null : current);
    }
  };

  const total = entries.length + overflow;
  return (
    <section className={styles.queue} data-nc-pending-queue="" aria-label="Queued messages">
      {/* `muted` rather than the default card: this sits between the transcript
          and the composer, and a bordered white card there reads as a third
          surface competing with both. */}
      <Card variant="muted" padding={3}>
        <VStack gap={3}>
          <Text as="p" type="supporting" data-nc-pending-queue-caption="">
            {total === 1
              ? 'One message is waiting to send when this turn ends.'
              : `${total} messages are waiting to send when this turn ends.`}
          </Text>
          <List className={styles.list}>
            {entries.map((entry) => {
              /* Whether THIS reader is editing THIS entry. A presentation
                 question, and the only thing that question decides. */
              const draft = editor?.entryId === entry.entry_id ? editor.draft : null;
              /* What the server last said about THIS entry. Independent of the
                 above, which is the point — see the matrix on `OpenEditor`. */
              const shown = refusal?.entryId === entry.entry_id ? refusal.outcome : null;
              const noticeLine = shown === null ? null : noticeText(shown, draft !== null);
              /* The winner's text is offered as a REPLACEMENT, so it is only
                 offered where there is something to replace. With no editor open
                 the notice quotes the entry itself, which the row already shows. */
              const lostRace = shown?.kind === 'stale' && draft !== null ? shown : null;
              /* The revision the next write carries, edit or delete alike: the one
                 the server reported if it has spoken about this entry, otherwise
                 the one this page was read at. Read from the refusal and NOT from
                 the editor — a refused delete has no editor, and taking it from
                 there is how a retry ended up re-sending a revision it already
                 knew was stale, forever. */
              const saveRev = shown?.kind === 'stale' ? shown.rev : entry.rev;
              return (
                <li key={entry.entry_id} data-nc-pending-entry={entry.entry_id}>
                  {/* Each entry gets its own card inside the muted region.
                      Without it the rows ran together — a message, its two
                      buttons, then the next message, all on one flat ground,
                      with nothing saying where one ended. */}
                  <Card padding={2}>
                    <VStack gap={1.5} align="stretch">
                    {draft === null
                      ? <Text as="p" className={styles.text}>{entry.text}</Text>
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
                    {noticeLine !== null && shown !== null && (
                      <div data-nc-pending-entry-notice="">
                        <Banner
                          status={noticeStatus(shown)}
                          title={noticeHeading(shown, draft !== null)}
                          description={noticeLine}
                        />
                      </div>
                    )}
                    {lostRace !== null && (
                      <Card variant="muted" padding={2} data-nc-pending-entry-theirs="">
                        <VStack gap={1.5} align="start">
                          <Text as="p" type="supporting" className={styles.text}>
                            {lostRace.text}
                          </Text>
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
                        </VStack>
                      </Card>
                    )}
                    <HStack gap={1} align="center">
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
                                setEditor({ entryId: entry.entry_id, draft: entry.text });
                                setRefusal(null);
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
                              onClick={() => { setEditor(null); setRefusal(null); }}
                            />
                          </>
                        )}
                      </HStack>
                    </VStack>
                  </Card>
                </li>
              );
            })}
          </List>
          {overflow > 0 && (
            <Text as="p" type="supporting" role="status" data-nc-pending-overflow="">
              {overflow === 1
                ? '1 more queued message is waiting but cannot be shown or edited here.'
                : `${overflow} more queued messages are waiting but cannot be shown or edited here.`}
            </Text>
          )}
        </VStack>
      </Card>
    </section>
  );
}
