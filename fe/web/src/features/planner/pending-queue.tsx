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
// The pencil puts the words in the composer and THEN deletes the entry, and
// that order is the whole design rather than an implementation detail.
//
// Delete-then-echo has an asynchronous gap between "the message is gone" and
// "the words are back", and everything a person can do inside that gap is a
// way to lose them: type something (the echo would overwrite it, or be
// dropped, and either way one of the two texts is destroyed), or leave for
// another conversation (the echo arrives in a composer that is no longer the
// one it came from). Two review rounds found five distinct cells of that
// matrix; the second round found them in the *fixes* for the first.
//
// Echo-first has no gap. The composer is empty when the pencil is offered —
// that is enforced — so putting the words there is immediate and destroys
// nothing, and it happens in the conversation the reader is looking at because
// it happens before any `await`. If the delete is then refused, the words are
// taken back out again (below), and the worst case is that a message is
// briefly visible in two places, which the reader can see and undo. The version this
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
  /**
   * Take those words back out again, if they are still exactly the ones that
   * were put in.
   *
   * Called when the delete was refused, so the message is still queued and the
   * composer must not keep a second copy of it. "Still exactly the ones" is
   * the caller's job: a reader who has typed since owns that box, and silently
   * clearing what they wrote would be the loss this whole ordering exists to
   * avoid.
   */
  onWithdraw: (cardId: string, text: string) => void;
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
type Refusal = Readonly<{
  entryId: string;
  outcome: PlannerQueueWriteOutcome;
  /** Whether the refused write was the pencil, which leaves words in the box.
   *  The cross leaves none, and its notice may not say otherwise. */
  wordsKept: boolean;
}>;

/**
 * What a refusal says.
 *
 * All three are now about something that did not happen to a message still
 * sitting in the strip — there is no open editor left for a refusal to be
 * about, so there is no second wording and no "your text is still below".
 */
/**
 * What the answer says about the words already sitting in the composer.
 *
 * `keep` — the entry is gone (`done`), or may be (`failed`: a transport
 * failure cannot tell "the server refused" from "the server deleted it and the
 * answer was lost coming back"). Keeping them is wrong for a refusal only in
 * that the message is briefly in two places, which is visible and undoable;
 * withdrawing them is wrong for a deletion in that the message is gone. **A
 * duplicate you can see beats a message you cannot get back.**
 *
 * `withdraw` — the entry is definitely still going to be sent: `stale` means
 * nothing happened to it, `gone` means it has already left the queue. Leaving
 * the words in the box would invite sending the same message twice.
 */
function wordsAfter(outcome: PlannerQueueWriteOutcome): 'keep' | 'withdraw' {
  return outcome.kind === 'done' || outcome.kind === 'failed' ? 'keep' : 'withdraw';
}

const IMAGES_REASON =
  'This message carries images, and taking it back would return only the words. '
  + 'Delete it and say it again, or leave it to send.';

function noticeText(outcome: PlannerQueueWriteOutcome, wordsKept: boolean): string | null {
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
    return 'This message is no longer in the queue — it has either been sent or '
      + 'been removed somewhere else.';
  }
  if (outcome.kind === 'failed') {
    /* Only the pencil leaves words behind, and only it may say so. The cross
       hands nothing back, and telling its reader their words are "in the box"
       would be describing a screen they are not looking at. */
    return wordsKept
      ? `${outcome.message} Your words are in the box; check the queue above `
        + 'before sending them again, in case the message is still there.'
      : outcome.message;
  }
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
  entries, overflow, busy, composerBusy, cardId,
  onTakeBack, onDelete, onEcho, onWithdraw,
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

  const settle = (
    entryId: string, outcome: PlannerQueueWriteOutcome, wordsKept: boolean,
  ): void => {
    setRefusal(outcome.kind === 'done' ? null : { entryId, outcome, wordsKept });
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
              : noticeText(shown.outcome, shown.wordsKept);
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
                        /* Re-asked rather than read off the render that drew
                           the button — but asked BEFORE anything is awaited, so
                           the answer cannot go stale between the check and the
                           write. That ordering is the point; see the note at
                           the top of this file. */
                        if (composerHasWords.current || hasImages) return;
                        onEcho(cardId, text);
                        const outcome = await onTakeBack({ ...entry, text, rev });
                        settle(entry.entry_id, outcome, wordsAfter(outcome) === 'keep');
                        if (wordsAfter(outcome) === 'withdraw') onWithdraw(cardId, text);
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
                        settle(entry.entry_id, await onDelete({ ...entry, text, rev }), false);
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
