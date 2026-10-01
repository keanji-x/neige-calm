import type { ReactNode } from 'react';
import { Divider } from '@astryxdesign/core/Divider';
import { Collapsible } from '@astryxdesign/core/Collapsible';
import type { ConversationStopFeedback } from '../../../../../core/domain/conversation-stop.ts';
import styles from './thread.module.css';

/** Terminal outcomes and transient notices share the approved native visual. */
export function ThreadStatusNotice({ heading, children, error = false }: {
  heading: ReactNode; children: ReactNode; error?: boolean;
}) {
  return <div className={`${styles.outcomeNotice} ${error ? styles.outcomeNoticeError : ''}`} role="status">
    <Divider />
    <Collapsible defaultIsOpen={false} trigger={heading}>{children}</Collapsible>
  </div>;
}

export function StopStatusNotice({ feedback }: { feedback: ConversationStopFeedback }) {
  const labels = {
    requesting: 'Requesting stop', stopping: 'Stopping response',
    unconfirmed: 'Stop unconfirmed', failed: 'Stop request failed',
  };
  const reason = feedback.kind === 'failed' ? feedback.message
    : feedback.kind === 'requesting' ? 'Waiting for the stop request to finish.'
    : feedback.kind === 'stopping' ? 'Waiting for the response to end.'
    : 'The response may still be starting or may already have ended.';
  return <div className={styles.outcome}>
    <ThreadStatusNotice error={feedback.kind === 'failed'}
      heading={<span className={styles.outcomeHeader}>
        <span className={styles.outcomeStatusLabel}>{labels[feedback.kind]}</span>
      </span>}>
      <p className={styles.outcomeReason}>{reason}</p>
    </ThreadStatusNotice>
  </div>;
}
