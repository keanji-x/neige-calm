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
// What is gone is the *editor* on top of it. The edit route is a PATCH (not a
// PUT: `routes/cards.rs` mounts `patch(...).delete(...)` on that path) and it
// is still served; nothing in the browser calls it any more, the same way
// `POST /planner/reset` was left standing when #1139 removed its last caller.
//
// -- What a take-back does NOT bring back ---------------------------------
//
// **Images.** A queued message can carry them (`PendingQueueEntry.attachments`,
// straight off `page_pending_entries`), and handing back only the words would
// silently drop them -- for an image-only message that is the entire message.
// So the pencil is refused on an entry carrying any, with a reason, and the
// cross still works. Restoring them would mean adopting server-held attachment
// ids into the composer's strip, and whether those ids survive their entry's
// deletion is a question about the kernel nobody has asked; guessing it is how
// the images get lost a second, quieter way. **KNOWN GAP, deliberate.**

import { Banner } from '@astryxdesign/core/Banner';
import { IconButton } from '@astryxdesign/core/IconButton';
import { List } from '@astryxdesign/core/List';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';

import type {
  PendingQueueEntry, PlannerQueueWriteOutcome,
} from '../../../../core/domain/conversation.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { useRef } from 'react';
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
  /**
   * Put these words in the composer.
   *
   * Carries the card the take-back was STARTED on. A write in flight outlives
   * the conversation it belongs to — the drawer is reused across
   * conversations, so a DELETE issued in A can answer while B is open — and
   * without the id the answer went into whichever composer happened to be on
   * screen, overwriting a draft that had nothing to do with it. Same ownership
   * rule the attachment strip already runs on `generation`.
   */
  onEcho: (cardId: string, text: string) => void;
  /** The conversation these entries belong to. See {@link onEcho}. */
  cardId: string;
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
       from then on (`shownText`). It used to keep rendering the text this page
       was read at, so the sentence pointed at words the server had already
       replaced — and a retry then deleted the new message while handing back
       the old one. */
    return 'This message changed before your change could be applied, so nothing '
      + 'happened to it. It now reads as shown; try again if you still want to.';
  }
  if (outcome.kind === 'gone') return 'This message already left the queue.';
  if (outcome.kind === 'failed') {
    /* The ambiguous one — see `echoOn`. Say what is uncertain rather than
       implying the message is safely where it was. */
    return `${outcome.message} Your words are back in the box; check the queue `
      + 'above before sending them again.';
  }
  return null;
}

/**
 * Whether a take-back's outcome licenses handing the words to the composer.
 *
 * `done` obviously. `failed` too, and that is the decision worth writing down:
 * a transport failure cannot distinguish "the server refused" from "the server
 * deleted it and the answer was lost on the way back". Withholding the words
 * is right for the first and loses the message outright for the second.
 * Handing them over is wrong for the first only in that the message is briefly
 * in two places — which the reader can see, and fix with the cross. **A
 * duplicate you can see beats a message you cannot get back**, so the
 * ambiguous outcome resolves toward the recoverable error.
 *
 * `stale` and `gone` are not ambiguous: the entry is definitely still queued,
 * or definitely already sent. Echoing either would duplicate a live message.
 */
function echoOn(outcome: PlannerQueueWriteOutcome): boolean {
  return outcome.kind === 'done' || outcome.kind === 'failed';
}

const IMAGES_REASON =
  'This message carries images, and taking it back would return only the words. '
  + 'Delete it and say it again, or leave it to send.';

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
  entries, overflow, busy, composerBusy, cardId, onTakeBack, onDelete, onEcho,
}: PendingQueueProps) {
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  /*
   * One lock for the whole strip, not one per button.
   *
   * Astryx's `clickAction` disables the control it is on while its promise is
   * unsettled, and that is all it does — which left every OTHER control live.
   * Two pencils pressed in quick succession therefore issued two take-backs:
   * both entries were deleted, and only the second answer's words survived,
   * because the first echo was overwritten by the second. The write that is in
   * flight is a fact about this card, so it is held for this card.
   *
   * **Raised in `onClick`, released in `clickAction`.** Astryx runs
   * `clickAction` inside `startTransition` (`Button.tsx`), and a state update
   * made in a transition is non-urgent — measured: setting it there produced
   * no locked render at all while the request was open, which is precisely the
   * window it exists to cover. `onClick` runs before that transition starts,
   * so the lock is up before the first `await`. Releasing it inside the
   * transition is fine; nothing is racing to observe the unlock.
   */
  const [writing, setWriting] = useState(false);
  /*
   * What the composer holds RIGHT NOW, readable from inside a settled promise.
   *
   * `composerBusy` as a prop is only ever the value at the moment of the
   * click. Start a take-back over an empty composer and type while the DELETE
   * is in flight, and the answer arrived and overwrote what had just been
   * typed. The guard has to be re-asked when the words are actually about to
   * move, which means reading it out of a ref rather than out of a closure.
   */
  const composerHasWords = useRef(composerBusy);
  composerHasWords.current = composerBusy;

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
            const shown = refusal?.entryId === entry.entry_id ? refusal.outcome : null;
            const noticeLine = shown === null ? null : noticeText(shown);
            /* The revision the next write carries: the one the server reported
               if it has spoken about this entry, otherwise the one this page
               was read at. Without this a refused write retried against a
               revision it already knew was stale, forever. */
            const rev = shown?.kind === 'stale' ? shown.rev : entry.rev;
            /* And the TEXT that goes with that revision. A stale refusal is
               the server telling us what the entry says now; from that moment
               the row shows the winner's words and a retry hands those back.
               Reading `entry.text` here is how a retry deleted the new message
               and returned the old one. */
            const text = shown?.kind === 'stale' ? shown.text : entry.text;
            const hasImages = entry.attachments.length > 0;
            /* Only the two refusals a reader can act on get a tooltip; the
               button's own label already says what it does. */
            const editReason = hasImages
              ? IMAGES_REASON
              : composerBusy ? COMPOSER_BUSY_REASON : undefined;
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
                    label="Edit this message"
                    icon={<Icon name="pencil" size="sm" />}
                    variant="ghost"
                    size="sm"
                    isDisabled={blocked || composerBusy || hasImages}
                    tooltip={editReason}
                    onClick={() => { setWriting(true); }}
                    clickAction={async () => {
                      try {
                        /* Re-asked here, not read off the render that drew the
                           button: the composer may have gained words since. */
                        if (composerHasWords.current || hasImages) return;
                        const outcome = await onTakeBack({ ...entry, text, rev });
                        settle(entry.entry_id, outcome);
                        if (!echoOn(outcome)) return;
                        /* Asked a second time, for the window the request was
                           open. Refusing here leaves the words unrecovered
                           rather than destroying what was typed instead — and
                           the entry is gone, so the reader is told by the
                           notice rather than silently. */
                        if (composerHasWords.current) return;
                        onEcho(cardId, text);
                      } finally {
                        setWriting(false);
                      }
                    }}
                  />
                  <IconButton
                    label="Delete this message"
                    icon={<Icon name="close" size="sm" />}
                    variant="ghost"
                    size="sm"
                    isDisabled={blocked}
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
