// #1505 PR4 — the messages a person typed while a turn was running.
//
// ── One bubble each, and one icon ─────────────────────────────────────────
//
// This used to be a stack of cards, each with the message in full, an inline
// `TextArea` when you edited it, and four named buttons. In a 364px drawer,
// directly above the thing you are typing into, that is a second composer
// sitting on top of the first one. What a queued message needs is to be
// recognisable — enough of its first line to know which one it is — and a way
// out. So: one bubble, one line, ellipsis, a cross.
//
// **A bubble, and deliberately NOT the composer's drawer surface.** The
// version before this one sat in `ChatComposerDrawer`, which tints, rounds and
// tucks itself behind the field — it makes the strip read as the top of the
// input box. These messages are not part of the box you are typing in; they
// are things already said and waiting. Discrete bubbles floating above it say
// that, and the composer keeps its own edges.
//
// There is no caption over them either. "3 messages are waiting to send when
// this turn ends" was a sentence explaining a picture that explains itself.
// What went with it is the words "when this turn ends", which is a real fact
// and now goes unsaid — worth knowing that is the trade.
//
// ── There is no edit, and no take-back ────────────────────────────────────
//
// A pencil that pulled a queued message back into the composer was built,
// reviewed three times, and removed. It is not hard because deleting is hard;
// it is hard because the recovered words have nowhere to live. `composerDraft`
// (`app/router/public.tsx`) is ONE string, shared across conversations and
// cleared when the drawer closes, so a recovery has no owner: it can land in a
// conversation it did not come from, or be wiped by a close, and the message
// it came from is already deleted by then. Three rounds of review found five
// distinct cells of that matrix, and the third round found them in the fixes
// for the second.
//
// Binding drafts to conversations is the fix and it is a change to the
// router's state model, not to this component. **Deferred on purpose, with the
// owner's decision**: shipping a delete-only strip is a smaller thing that is
// entirely true, and the alternative was an edit affordance that loses
// messages in ways a person cannot see.
//
// The compare-and-swap stays: a delete carries the revision it was read at and
// can be refused, and a refusal is shown rather than retried. `PATCH
// .../planner/input/{id}` is still served and the browser no longer calls it,
// the same way `POST /planner/reset` was left standing when #1139 removed its
// last caller.

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
  onDelete: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
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
    /* "as shown" is a promise about the bubble above this notice, and it is
       kept: a stale refusal carries the winner's text and the row renders THAT
       from then on (`text` below). It used to keep rendering the text this page
       was read at, so the sentence pointed at words the server had already
       replaced — and a retry then deleted the new message while handing back
       the old one. */
    return 'This message changed before your change could be applied, so nothing '
      + 'happened to it. It now reads as shown; try again if you still want to.';
  }
  if (outcome.kind === 'gone') {
    /* NOT "already sent": another actor deleting it produces this same answer,
       and the server does not say which happened. All that is known is that
       the queue no longer has it. */
    /* NOT "already sent": the same answer comes back when another actor
       removed it, and the server does not say which happened. All that is
       known is that the queue no longer has it. */
    return 'This message is no longer in the queue — it has either been sent or '
      + 'been removed somewhere else, and the server does not say which.';
  }
  /* The server's own sentence and nothing added to it: a delete that failed
     leaves the message exactly where it was, which the strip already shows. */
  if (outcome.kind === 'failed') return outcome.message;
  return null;
}

function noticeHeading(outcome: PlannerQueueWriteOutcome): string {
  if (outcome.kind === 'stale') return 'Nothing happened';
  if (outcome.kind === 'gone') return 'No longer in the queue';
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


export function PendingQueue({
  entries, overflow, busy, onDelete,
}: PendingQueueProps) {
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  /*
   * One lock for the whole strip, not one per button.
   *
   * Astryx's `clickAction` disables the control it is on while its promise is
   * unsettled, and that is all it does — which leaves every OTHER control
   * live. A queue write is a compare-and-swap against a revision this page was
   * read at, so two of them in flight together means the second is composed
   * against a page the first has already invalidated. The write that is in
   * flight is a fact about this card, so it is held for this card.
   *
   * **Raised in `onClick`, released in `clickAction`** — see the note on the
   * button.
   */
  const [writing, setWriting] = useState(false);
  if (entries.length === 0 && overflow === 0) return null;

  const settle = (entryId: string, outcome: PlannerQueueWriteOutcome): void => {
    setRefusal(outcome.kind === 'done' ? null : { entryId, outcome });
  };

  const blocked = busy || writing;
  return (
    <section className={styles.queue} data-nc-pending-queue="" aria-label="Queued messages">
      <VStack gap={1}>
        <List className={styles.list}>
          {entries.map((entry) => {
            const shown = refusal?.entryId === entry.entry_id ? refusal : null;
            const noticeLine = shown === null
              ? null
              : noticeText(shown.outcome);
            /* The revision the next write carries: the one the server reported
               if it has spoken about this entry, otherwise the one this page
               was read at. Without this a refused write retried against a
               revision it already knew was stale, forever. */
            const refused = shown?.outcome ?? null;
            const rev = refused?.kind === 'stale' ? refused.rev : entry.rev;
            /* And the TEXT that goes with that revision. A stale refusal is
               the server telling us what the entry says now; from that moment
               the row shows the winner's words and a retry hands those back.
               Reading `entry.text` here is how a retry deleted the new message
               and returned the old one. */
            const text = refused?.kind === 'stale' ? refused.text : entry.text;
            return (
              <li key={entry.entry_id} data-nc-pending-entry={entry.entry_id}>
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
                    {text}
                  </Text>
                  <IconButton
                    label="Delete this message"
                    icon={<Icon name="close" size="sm" />}
                    variant="ghost"
                    size="sm"
                    isDisabled={blocked}
                    /*
                     * The lock is raised in `onClick` and released in
                     * `clickAction`. Astryx runs `clickAction` inside
                     * `startTransition` (`Button.tsx`), and a state update made
                     * in a transition is non-urgent — measured: setting it
                     * there produced no locked render at all while the request
                     * was open, which is precisely the window it exists to
                     * cover. `onClick` runs before that transition starts.
                     */
                    onClick={() => { setWriting(true); }}
                    clickAction={async () => {
                      try {
                        settle(entry.entry_id, await onDelete({ ...entry, text, rev }));
                      } finally {
                        setWriting(false);
                      }
                    }}
                  />
                </div>
                {noticeLine !== null && refused !== null && (
                  <div className={styles.notice} data-nc-pending-entry-notice="">
                    <Banner
                      status={noticeStatus(refused)}
                      title={noticeHeading(refused)}
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
