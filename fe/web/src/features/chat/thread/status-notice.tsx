import { useLayoutEffect, useMemo, useRef, type ReactNode } from 'react';
import { Divider } from '@astryxdesign/core/Divider';
import { Collapsible } from '@astryxdesign/core/Collapsible';
import { IconButton } from '@astryxdesign/core/IconButton';
import type { ConversationMetaClock } from '../../../../../core/domain/conversation-meta.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './meta.module.css';

export type CopyResponseAction = Readonly<{ id: string; text: string; run: () => Promise<void> }>;

function ActionIcon({ kind }: { kind: 'copy' | 'copied' | 'edit' | 'regenerate' }) {
  return <svg className={styles.icon} viewBox="0 0 24 24" fill="none" stroke="currentColor"
    strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
    {kind === 'copied' ? <path d="m5 12 4 4L19 6" /> : kind === 'copy' ? <><rect x="8" y="8" width="12" height="12" rx="2" /><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" /></>
      : kind === 'edit' ? <><path d="m16 3 5 5-12 12-6 1 1-6L16 3Z" /><path d="m14 5 5 5" /></>
        : <><path d="M20 7v5h-5" /><path d="M4 17v-5h5" /><path d="M6 7a7 7 0 0 1 12-1l2 3M18 17a7 7 0 0 1-12 1l-2-3" /></>}
  </svg>;
}

function elapsedText(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** One native metadata row; clocks are evidence, never inferred from transcript gaps. */
export function ThreadStatusNotice({ heading, children, clock, tone = 'neutral', outcome, copyAction = null, regenerateAction = null }: {
  heading: ReactNode;
  children?: ReactNode;
  clock: ConversationMetaClock;
  tone?: 'neutral' | 'warning' | 'error';
  outcome?: 'completed' | 'interrupted' | 'failed';
  copyAction?: CopyResponseAction | null;
  regenerateAction?: Readonly<{ id: string; run: () => Promise<void> }> | null;
}) {
  const stateRef = useRef<HTMLDivElement | null>(null);
  const lastFocused = useRef<HTMLElement | null>(null);
  const hasDetails = children !== undefined;
  useLayoutEffect(() => {
    const previous = lastFocused.current;
    if (previous !== null && !previous.isConnected && document.activeElement === document.body) {
      stateRef.current?.focus({ preventScroll: true });
    }
  }, [hasDetails]);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const copyId = copyAction?.id ?? null;
  const copyBody = copyAction?.text ?? null;
  const copyView = useMemo(() => copyId === null || copyBody === null ? null : { id: copyId, text: copyBody, active: false },
    [copyId, copyBody]);
  const committedCopy = useRef<typeof copyView>(null);
  useLayoutEffect(() => {
    committedCopy.current = copyView;
    return () => { committedCopy.current = null; };
  }, [copyView]);
  const [copyResult, setCopyResult] = useState<Readonly<{ view: NonNullable<typeof copyView>; kind: 'pending' | 'copied' | 'failed'; error: string | null }> | null>(null);
  const feedback = copyResult?.view === copyView ? copyResult : null;
  const performCopy = async () => {
    const view = copyView;
    const action = copyAction;
    if (view === null || action === null || committedCopy.current !== view || view.active) return;
    view.active = true;
    setCopyResult({ view, kind: 'pending', error: null });
    try {
      await action.run();
      if (committedCopy.current === view) setCopyResult({ view, kind: 'copied', error: null });
    } catch (reason) {
      if (committedCopy.current === view) setCopyResult({ view, kind: 'failed',
        error: reason instanceof Error && reason.message.trim() !== '' ? reason.message : 'Could not copy response.' });
    } finally { view.active = false; }
  };
  const committedRegenerate = useRef<typeof regenerateAction>(null);
  useLayoutEffect(() => {
    committedRegenerate.current = regenerateAction;
    return () => { committedRegenerate.current = null; };
  }, [regenerateAction]);
  const performRegenerate = async () => {
    if (regenerateAction === null || committedRegenerate.current !== regenerateAction) return;
    await regenerateAction.run();
  };
  const timestamp = clock.timestamp;
  const showTime = timestamp !== null && (hovered || focused);
  const timeText = timestamp === null ? null
    : new Date(timestamp.atMs).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
  const title = <span className={styles.summary}><span className={styles.label}>{heading}</span>
    {showTime && timestamp !== null ? <span className={styles.clock}>· <time dateTime={new Date(timestamp.atMs).toISOString()}
      aria-label={`${timestamp.kind === 'paused' ? 'Paused' : 'Finished'} at ${timeText}`}>{timeText}</time></span>
      : clock.elapsedMs !== null && <span className={styles.clock} data-nc-meta-duration="">· {elapsedText(clock.elapsedMs)}</span>}
  </span>;
  return <div className={`${styles.notice} ${tone === 'warning' ? styles.warning : tone === 'error' ? styles.error : ''}`}
    role="status" aria-label="Current response status" data-nc-current-meta="" data-nc-turn={outcome === undefined ? undefined : 'outcome'}
    data-nc-turn-outcome={outcome}>
    <Divider />
    <div className={styles.row}>
      <div ref={stateRef} className={styles.state} data-nc-meta-state="" tabIndex={timestamp !== null && !hasDetails ? 0 : -1}
        onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
        onFocus={(event) => { lastFocused.current = event.target; setFocused(true); }} onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) {
            setFocused(false);
            if (event.relatedTarget !== null || event.target.isConnected) lastFocused.current = null;
          }
        }}>
        {children === undefined ? <div className={styles.normal}>{title}</div>
          : <Collapsible defaultIsOpen={false} chevronPosition="start" trigger={title}>{children}</Collapsible>}
      </div>
      <div className={styles.actions} role="group" aria-label="Response actions">
        <IconButton label={copyAction === null ? 'Copy response (not available yet)' : feedback === null || feedback.kind === 'pending' ? 'Copy response'
          : feedback.kind === 'copied' ? 'Copied response' : `Copy failed: ${feedback.error}`}
          icon={<ActionIcon kind={feedback?.kind === 'copied' ? 'copied' : 'copy'} />} className={styles.action}
          variant="ghost" size="sm" isDisabled={copyAction === null || feedback?.kind === 'pending'} onClick={() => { void performCopy(); }} />
        <IconButton label="Edit response (not available yet)" icon={<ActionIcon kind="edit" />} className={styles.action} variant="ghost" size="sm" isDisabled />
        <IconButton label={regenerateAction === null ? "Regenerate response (not available now)" : "Regenerate response"} tooltip="Send the original prompt again in this conversation; keep existing history." icon={<ActionIcon kind="regenerate" />} className={styles.action} variant="ghost" size="sm" isDisabled={regenerateAction === null} clickAction={performRegenerate} />
      </div>
    </div>
  </div>;
}
