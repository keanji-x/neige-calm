import { Badge } from '@astryxdesign/core/Badge';
import { sentMentionParts } from '../../../../../core/domain/mentions.ts';
import mobileStyles from './mobile-composer.module.css';
import styles from './thread.module.css';

/** The same readable outgoing text before and after a conversation exists.
 * Callers own delivery, grouping, attachments and identity; no row or clock is needed. */
export function OutgoingMessage({ text, mobile, queued = false, edited = false, replacement = false }: Readonly<{
  text: string;
  mobile: boolean;
  queued?: boolean;
  edited?: boolean;
  replacement?: boolean;
}>) {
  return (
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
  );
}
