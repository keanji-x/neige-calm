import mobileStyles from './mobile-composer.module.css';
import { memo } from 'react';
import type { ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { OutgoingMessage } from './outgoing-message.tsx';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { Reply, type ReplyImageFiles, type ReplyLinkRenderer } from './reply.tsx';
import styles from './thread.module.css';

/** Only the declared visual inputs of one ordinary message. Transcript grouping,
 * navigation and delivery policy remain in their owners; fields capture values
 * rather than assuming a caller preserves a message object's identity. */
type MessageEntryProps = Readonly<{
  id: string;
  mobile: boolean;
  atMs: number;
  /** Stable for this thread viewport/timezone; only a rendered row formats it. */
  timestampFormatter: Intl.DateTimeFormat | null;
  entryKey: string;
  author: ConversationTurn['author'];
  human: ConversationTurn['human'];
  text: string;
  attachments: ConversationTurn['attachments'];
  opens: boolean;
  gapLabel: string | null;
  queued: boolean;
  edited: boolean;
  replacement: boolean;
  working: boolean;
  imageFiles: ReplyImageFiles | null;
  renderLink?: ReplyLinkRenderer;
}>;

export const MessageEntry = memo(function MessageEntry({ id, entryKey, author, human, text, attachments,
  opens, gapLabel, queued, edited, replacement, working, imageFiles, renderLink, mobile, atMs, timestampFormatter,
}: MessageEntryProps) {
  return (
    <div
      className={[opens ? styles.exchange : '', mobile && author !== 'agent' ? mobileStyles.userTurn : ''].filter(Boolean).join(' ') || undefined}
      data-nc-entry={entryKey}
      {...(opens ? { 'data-nc-exchange': id } : {})}
    >
      {/* A time only where the conversation restarted. */}
      {gapLabel !== null && (
        <p className={styles.gap}>{gapLabel}</p>
      )}
      {author !== 'agent' ? (
        <>
          <OutgoingMessage speaker={author} text={text} mobile={mobile} queued={queued} edited={edited} replacement={replacement} />
          {/* Attribution follows the bubble so a real source handoff preserves its position. */}
          {author !== 'you' && <p className={styles.queuedNote}>{author === 'human' ? human?.displayName : 'Unknown author'}</p>}
          {/* `alt=""` and `aria-hidden`: the transcript has no description of the image to offer, and the count is said once in text above. */}
          {(attachments ?? []).length > 0 && (
            <ul className={styles.attachments} data-nc-turn-attachments="" {...(edited ? { 'data-nc-editing': '' } : {})}>
              {(attachments ?? []).map((attachment) => (
                <li key={attachment.id} className={styles.attachment}>
                  <img src={attachment.url} alt="" />
                </li>
              ))}
            </ul>
          )}
          {queued && (
            /* `role="status"`: it appears in response to the reader's own press, answering "did that go anywhere?". */
            <p className={styles.queuedNote} data-nc-queued-note="" role="status">
              Queued · sends when this turn ends
            </p>
          )}
          {replacement && <p className={styles.queuedNote}>Replaces the marked message above</p>}
        </>
      ) : (
        <div className={`${styles.reply} ${mobile ? mobileStyles.reply : ''}`} data-nc-turn="agent">
          <Reply text={text} imageFiles={imageFiles} renderLink={renderLink} />
          {timestampFormatter !== null && <time className={mobileStyles.timestamp} dateTime={new Date(atMs).toISOString()}>{timestampFormatter.format(atMs)}</time>}
          {working && <ActivityIndicator state="working" motion="thinking" />}
        </div>
      )}
    </div>
  );
});
