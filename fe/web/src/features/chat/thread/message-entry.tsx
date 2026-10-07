import mobileStyles from './mobile-composer.module.css';
import { memo } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import type { ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { sentMentionParts } from '../../../../../core/domain/mentions.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { Reply, type ReplyImageFiles } from './reply.tsx';
import styles from './thread.module.css';

/** Only the declared visual inputs of one ordinary message. Transcript grouping,
 * navigation and delivery policy remain in their owners; fields capture values
 * rather than assuming a caller preserves a message object's identity. */
type MessageEntryProps = Readonly<{
  id: string;
  mobile: boolean;
  atMs: number;
  entryKey: string;
  author: ConversationTurn['author'];
  text: string;
  attachments: ConversationTurn['attachments'];
  opens: boolean;
  gapLabel: string | null;
  queued: boolean;
  edited: boolean;
  replacement: boolean;
  working: boolean;
  imageFiles: ReplyImageFiles | null;
}>;

export const MessageEntry = memo(function MessageEntry({ id, entryKey, author, text, attachments,
  opens, gapLabel, queued, edited, replacement, working, imageFiles, mobile, atMs,
}: MessageEntryProps) {
  return (
    <div
      className={[opens ? styles.exchange : '', mobile && author === 'you' ? mobileStyles.userTurn : ''].filter(Boolean).join(' ') || undefined}
      data-nc-entry={entryKey}
      {...(opens ? { 'data-nc-exchange': id } : {})}
    >
      {/* A time only where the conversation restarted. */}
      {gapLabel !== null && (
        <p className={styles.gap}>{gapLabel}</p>
      )}
      {author === 'you' ? (
        <>
          {/* The caption is outside the message; persisted references recover their display pills. */}
          <p
            className={`${styles.said} ${mobile ? mobileStyles.said : ''}`}
            data-nc-turn="you"
            {...(queued ? { 'data-nc-queued': '' } : {})}
            {...(edited ? { 'data-nc-editing': '' } : {})}
            {...(replacement ? { 'data-nc-replacement': '' } : {})}
          >{sentMentionParts(text).map((part, index) => part.label === null ? part.text : (
            <span key={index} data-nc-sent-mention="" title={part.text}>
              <Badge className={styles.mentionPill}
                label={<span className={styles.mentionLabel}>{part.label}</span>} />
            </span>
          ))}</p>
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
          <Reply text={text} imageFiles={imageFiles} />
          {mobile && <time className={mobileStyles.timestamp} dateTime={new Date(atMs).toISOString()}>{new Date(atMs).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false })}</time>}
          {working && <ActivityIndicator state="working" motion="thinking" />}
        </div>
      )}
    </div>
  );
});
