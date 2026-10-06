// The messages a person typed while a turn was running: one bubble each, with edit/delete and
// "Say it now" offered only while the router passes `onSteer` (a running turn).

import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { IconButton } from '@astryxdesign/core/IconButton';
import { List } from '@astryxdesign/core/List';
import { TextArea } from '@astryxdesign/core/TextArea';
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
  /** Queued messages this page does not carry (no id, or past the page budget): counted, never given controls. */
  overflow: number;
  /** Blocks the controls while any write on this card is unanswered. */
  busy: boolean;
  onEdit: (entry: PendingQueueEntry, text: string) => Promise<PlannerQueueWriteOutcome>;
  onDelete: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
  /** Hand the entry to the turn running now; `undefined` means there is no such turn and the control is not drawn. */
  onSteer?: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
}>;

/** The last refusal and which entry it was about. A `stale` refusal carries the revision the server reported, and the next write uses it: a retry re-sending `entry.rev` is guaranteed to lose again. */
type Refusal = Readonly<{ entryId: string; outcome: PlannerQueueWriteOutcome }>;

/** What a refusal says. */
function noticeText(outcome: PlannerQueueWriteOutcome): string | null {
  if (outcome.kind === 'stale') {
    return 'This message changed before your change could be applied, so nothing '
      + 'happened to it. It now reads as shown; try again if you still want to.';
  }
  if (outcome.kind === 'gone') {
    /* Not "already sent": another actor removing it produces the same answer, and the server does not say which. */
    return 'This message is no longer in the queue — it has either been sent or '
      + 'been removed somewhere else, and the server does not say which.';
  }
  if (outcome.kind === 'not_running') {
    return 'The turn ended before this message could be handed to it, so nothing '
      + 'happened. It stays queued and will go with the next turn.';
  }
  if (outcome.kind === 'unanswered') {
    return 'Codex did not answer in time, so it is not known whether this message '
      + 'reached the current turn. It stays queued and will go with the next turn; '
      + 'if it did reach this one, it will also show up in the conversation.';
  }
  if (outcome.kind === 'failed') return outcome.message;
  return null;
}

function noticeHeading(outcome: PlannerQueueWriteOutcome): string {
  if (outcome.kind === 'stale') return 'Nothing happened';
  if (outcome.kind === 'gone') return 'No longer in the queue';
  if (outcome.kind === 'not_running') return 'Still queued';
  if (outcome.kind === 'unanswered') return 'Still queued — not confirmed';
  return 'Could not be changed';
}

/** `stale` is a race the reader can win by trying again; `gone` has nothing to retry; `not_running` has nothing to do;
 * `unanswered` is a doubt the reader should hold (the message may be said twice), so a warning like `stale`; `failed` is the server refusing. */
function noticeStatus(outcome: PlannerQueueWriteOutcome): 'warning' | 'info' | 'error' {
  if (outcome.kind === 'stale' || outcome.kind === 'unanswered') return 'warning';
  if (outcome.kind === 'gone' || outcome.kind === 'not_running') return 'info';
  return 'error';
}


export function PendingQueue({
  entries, overflow, busy, onEdit, onDelete, onSteer,
}: PendingQueueProps) {
  const [editing, setEditing] = useState<Readonly<{ entry: PendingQueueEntry; text: string }> | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  /* One lock for the whole strip: `refusal` holds one entry's answer, so two writes settling together would silently drop one refusal. Raised in `onClick`, released in `clickAction`. */
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
            /* The revision the next write carries: the one the server reported if it has spoken about this entry, otherwise the one this page was read at. */
            const refused = shown?.outcome ?? null;
            const rev = refused?.kind === 'stale' ? refused.rev : entry.rev;
            /* A stale refusal carries the winner's text, so the row shows that from then on rather than words the server has already replaced. */
            const text = refused?.kind === 'stale' ? refused.text : entry.text;
            return (
              <li key={entry.entry_id} data-nc-pending-entry={entry.entry_id}>
                {editing?.entry.entry_id === entry.entry_id ? (
                  <VStack gap={1}>
                    <TextArea label="Queued message" value={editing.text} isDisabled={blocked}
                      onChange={(value) => { setEditing({ ...editing, text: value }); }} />
                    {refused?.kind === 'stale' && (
                      <Text as="p">Current message: {refused.text}</Text>
                    )}
                    <Button label={refused?.kind === 'stale' ? 'Replace newer message' : 'Save message'} size="sm"
                      isDisabled={blocked || editing.text.trim() === ''}
                      onClick={() => { setWriting(true); }}
                      clickAction={async () => {
                        try {
                          const revision = refused?.kind === 'stale' ? refused.rev : editing.entry.rev;
                          const outcome = await onEdit({ ...editing.entry, rev: revision }, editing.text.trim());
                          settle(entry.entry_id, outcome);
                          if (outcome.kind === 'done' || outcome.kind === 'gone') setEditing(null);
                        } finally { setWriting(false); }
                      }} />
                    {refused?.kind === 'stale' && (
                      <Button label="Use current message" size="sm" variant="ghost" isDisabled={blocked}
                        onClick={() => {
                          setEditing({ entry: { ...entry, text: refused.text, rev: refused.rev }, text: refused.text });
                          setRefusal(null);
                        }} />
                    )}
                    <Button label="Cancel edit" size="sm" variant="ghost" isDisabled={blocked}
                      onClick={() => { setEditing(null); }} />
                  </VStack>
                ) : <div className={styles.bubble}>
                  <Text
                    className={styles.text}
                    maxLines={1}
                    hasTruncateTooltip
                    data-nc-pending-entry-text=""
                  >
                    {text}
                  </Text>
                  {onSteer !== undefined && (
                    <IconButton
                      label="Say it now"
                      icon={<Icon name="enter" size="sm" />}
                      variant="ghost"
                      size="sm"
                      isDisabled={blocked}
                      data-nc-pending-entry-steer=""
                      onClick={() => { setWriting(true); }}
                      clickAction={async () => {
                        try {
                          settle(entry.entry_id, await onSteer({ ...entry, text, rev }));
                        } finally {
                          setWriting(false);
                        }
                      }}
                    />
                  )}
                  <IconButton label="Edit this message" variant="ghost" size="sm" isDisabled={blocked}
                    icon={<Icon name="edit" size="sm" />}
                    onClick={() => { setEditing({ entry: { ...entry, text, rev }, text }); }} />
                  <IconButton
                    label="Delete this message"
                    icon={<Icon name="close" size="sm" />}
                    variant="ghost"
                    size="sm"
                    isDisabled={blocked}
                    /* Astryx runs `clickAction` inside `startTransition`, where a state update is non-urgent and produced no locked render at all; `onClick` runs before that transition starts. */
                    onClick={() => { setWriting(true); }}
                    clickAction={async () => {
                      try {
                        settle(entry.entry_id, await onDelete({ ...entry, text, rev }));
                      } finally {
                        setWriting(false);
                      }
                    }}
                  />
                </div>}
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
          <Text as="p" type="supporting" role="status" data-nc-pending-overflow="">
            {`${overflow} ${entries.length > 0 ? 'more ' : ''}queued message`
              + `${overflow === 1 ? ' is' : 's are'} waiting but cannot be shown or edited here.`}
          </Text>
        )}
      </VStack>
    </section>
  );
}
