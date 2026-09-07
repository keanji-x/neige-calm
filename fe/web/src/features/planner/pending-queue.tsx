// #1505 PR4 — the messages a person typed while a turn was running.
//
// ── One bubble each, and two icons ────────────────────────────────────────
//
// This used to be a stack of cards, each with the message in full, an inline
// `TextArea` when you edited it, and four named buttons. In a 364px drawer,
// directly above the thing you are typing into, that is a second composer
// sitting on top of the first one. What a queued message actually needs is to
// be recognisable — enough of its first line to know which one it is — and two
// ways out. So: one bubble, one line, ellipsis, a pencil and a cross.
//
// **A bubble, and deliberately NOT the composer's drawer surface.** The
// version before this one sat in `ChatComposerDrawer`, which tints, rounds and
// tucks itself behind the field — it makes the strip read as the top of the
// input box. These messages are not part of the box you are typing in; they
// are things already said and waiting. Discrete bubbles floating above it say
// that, and the composer keeps its own edges.
//
// There is no caption over them either. "3 messages are waiting to send when
// this turn ends" was a sentence explaining a picture that explains itself:
// bubbles above the field are queued messages, and every reader who has ever
// seen one knows it. What went with it is the words "when this turn ends",
// which is a real fact and now goes unsaid — worth knowing that is the trade.
//
// ── Edit takes the message BACK; it does not edit it in place ─────────────
//
// The pencil deletes the entry and puts its words in the composer. That is the
// whole of it, and the reason is that it leaves no state that has to be
// explained: a message you are working on is in your composer, a message that
// is waiting is in this strip, and nothing is ever in both. The version this
// replaced edited in place through a compare-and-swap, which meant an open
// editor, a revision to carry, a conflict to narrate when somebody else won
// the race, and a "use their version" affordance to resolve it — five states
// for a thing a person thinks of as "let me have that back".
//
// **The cost, stated because it is real: a message taken back and sent again
// goes to the END of the queue.** In-place editing kept its position. If that
// ordering ever matters to somebody, this is the trade that was made and the
// place to reverse it.
//
// The compare-and-swap has NOT gone away — a take-back is still a delete, and
// a delete still carries the revision it was read at and can still be refused.
// What is gone is the *editor* on top of it. `PUT /planner/input/:id` is still
// served and nothing in the browser calls it any more, the same way
// `POST /planner/reset` was left standing when #1139 removed its last caller.

import { Banner } from '@astryxdesign/core/Banner';
import { IconButton } from '@astryxdesign/core/IconButton';
import { List } from '@astryxdesign/core/List';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';

import type {
  PendingQueueEntry, PlannerQueueWriteOutcome,
} from '../../../../core/domain/conversation.ts';
import { Icon } from '../../ui/icon/public.tsx';
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
  /**
   * What the composer currently holds.
   *
   * Not for display — it decides whether a take-back is offered at all. The
   * pencil moves words INTO the composer, and doing that over a half-written
   * sentence destroys it. With something already typed the pencil is disabled
   * and says why, which is the only outcome here that loses nothing: the
   * message stays queued and the sentence stays typed.
   */
  composerBusy: boolean;
  /** Remove the entry from the queue and hand its words back. */
  onTakeBack: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
  onDelete: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
  /** Put these words in the composer. Called only after a take-back that the
   *  server confirmed, so the message cannot be in two places. */
  onEcho: (text: string) => void;
}>;

/**
 * The last refusal, and which entry it was about.
 *
 * The `stale` variant carries the revision the server reported, and the next
 * write on that entry uses it: the page in the cache is behind by definition
 * at that moment, and the refresh that would fix it is fire-and-forget and may
 * fail, so a retry that re-sends `entry.rev` is guaranteed to lose again.
 */
type Refusal = Readonly<{ entryId: string; outcome: PlannerQueueWriteOutcome }>;

/**
 * What a refusal says.
 *
 * All three are now about something that did not happen to a message still
 * sitting in the strip — there is no open editor left for a refusal to be
 * about, so there is no second wording and no "your text is still below".
 */
function noticeText(outcome: PlannerQueueWriteOutcome): string | null {
  if (outcome.kind === 'stale') {
    return 'This message changed before your change could be applied, so nothing '
      + 'happened to it. It now reads as shown; try again if you still want to.';
  }
  if (outcome.kind === 'gone') return 'This message already left the queue.';
  if (outcome.kind === 'failed') return outcome.message;
  return null;
}

function noticeHeading(outcome: PlannerQueueWriteOutcome): string {
  if (outcome.kind === 'stale') return 'Nothing happened';
  if (outcome.kind === 'gone') return 'Already sent';
  return 'Could not be changed';
}

/**
 * `stale` is a race the reader can still win by trying again; `gone` is the
 * queue having moved on without them, with nothing to retry; `failed` is the
 * server refusing.
 */
function noticeStatus(outcome: PlannerQueueWriteOutcome): 'warning' | 'info' | 'error' {
  if (outcome.kind === 'stale') return 'warning';
  if (outcome.kind === 'gone') return 'info';
  return 'error';
}

const COMPOSER_BUSY_REASON =
  'Send or clear what you are writing first — taking this back would replace it.';

export function PendingQueue({
  entries, overflow, busy, composerBusy, onTakeBack, onDelete, onEcho,
}: PendingQueueProps) {
  const [refusal, setRefusal] = useState<Refusal | null>(null);

  if (entries.length === 0 && overflow === 0) return null;

  const settle = (entryId: string, outcome: PlannerQueueWriteOutcome): void => {
    setRefusal(outcome.kind === 'done' ? null : { entryId, outcome });
  };

  return (
    <section className={styles.queue} data-nc-pending-queue="" aria-label="Queued messages">
      <VStack gap={1}>
        <List className={styles.list}>
          {entries.map((entry) => {
            const shown = refusal?.entryId === entry.entry_id ? refusal.outcome : null;
            const noticeLine = shown === null ? null : noticeText(shown);
            /* The revision the next write carries: the one the server reported
               if it has spoken about this entry, otherwise the one this page
               was read at. Without this a refused write retried against a
               revision it already knew was stale, forever. */
            const rev = shown?.kind === 'stale' ? shown.rev : entry.rev;
            return (
              <li key={entry.entry_id} data-nc-pending-entry={entry.entry_id}>
                {/* A grid and not an `HStack`, and this is the reason rather
                    than a preference: `Text maxLines={1}` truncates by going
                    `white-space: nowrap`, so its min-content is the WHOLE
                    message. In a flex row that minimum propagates up through
                    the drawer's grid item — measured at 712px inside a 290px
                    drawer — and the two icon buttons ended up past the right
                    edge, where the drawer's `overflow: hidden` cut them off
                    the screen. `minmax(0, 1fr)` is a track that content cannot
                    blow out; `min-inline-size: 0` on the item alone did not
                    fix it. */}
                <div className={styles.bubble}>
                  {/* One line and an ellipsis. `hasTruncateTooltip` gives the
                      whole message back on hover, and only when it was
                      actually shortened — so a short one gets no hover that
                      repeats what is already on screen. */}
                  <Text
                    className={styles.text}
                    maxLines={1}
                    hasTruncateTooltip
                    data-nc-pending-entry-text=""
                  >
                    {entry.text}
                  </Text>
                  <IconButton
                    label="Edit this message"
                    icon={<Icon name="pencil" size="sm" />}
                    variant="ghost"
                    size="sm"
                    isDisabled={busy || composerBusy}
                    tooltip={composerBusy ? COMPOSER_BUSY_REASON : undefined}
                    clickAction={async () => {
                      const outcome = await onTakeBack({ ...entry, rev });
                      settle(entry.entry_id, outcome);
                      /* Only a take-back the server confirmed hands the words
                         over. A refused one leaves the message where it is,
                         and echoing it anyway would put it in two places. */
                      if (outcome.kind === 'done') onEcho(entry.text);
                    }}
                  />
                  <IconButton
                    label="Delete this message"
                    icon={<Icon name="close" size="sm" />}
                    variant="ghost"
                    size="sm"
                    isDisabled={busy}
                    clickAction={async () => {
                      settle(entry.entry_id, await onDelete({ ...entry, rev }));
                    }}
                  />
                </div>
                {noticeLine !== null && shown !== null && (
                  <div className={styles.notice} data-nc-pending-entry-notice="">
                    <Banner
                      status={noticeStatus(shown)}
                      title={noticeHeading(shown)}
                      description={noticeLine}
                    />
                  </div>
                )}
              </li>
            );
          })}
        </List>
        {overflow > 0 && (
          /* The one line of prose left, and it earns its place: these are real
             messages that will really be sent and that nothing here can
             address, so without it a person who typed eleven and sees three
             has been misinformed. "more" only when there is something for them
             to be more THAN. */
          <Text as="p" type="supporting" role="status" data-nc-pending-overflow="">
            {`${overflow} ${entries.length > 0 ? 'more ' : ''}queued message`
              + `${overflow === 1 ? ' is' : 's are'} waiting but cannot be shown or edited here.`}
          </Text>
        )}
      </VStack>
    </section>
  );
}
