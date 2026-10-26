import { useLayoutEffect, useMemo, useRef, type ReactNode } from 'react';
import { Divider } from '@astryxdesign/core/Divider';
import { Collapsible } from '@astryxdesign/core/Collapsible';
import { MessageActions } from './message-actions.tsx';
import type { ConversationMetaClock } from '../../../../../core/domain/conversation-meta.ts';
import { useState } from '../../../ui/state/public.ts';
import { restoreDrawerFocus } from '../../../ui/drawer/restore-focus.ts';
import styles from './meta.module.css';

export type CopyResponseAction = Readonly<{ id: string; text: string; run: () => Promise<void> }>;
/**
 * `id` names the response it acts on; `run` settles once the action is handed on, and any failure is the caller's to show:
 * Regenerate is a send, whose failure the outbox reads through its table and shows above the composer.
 */
export type ResponseAction = Readonly<{ id: string; run: () => Promise<void> }>;
/** Edit answers at once (the composer receives the message); its replace and any failure are the caller's. */
export type EditAction = Readonly<{ id: string; run: () => void }>;

type ActionView = { readonly key: string; active: boolean };
type ActionResult = Readonly<{ view: ActionView; kind: 'pending' | 'done' | 'failed' }>;

/**
 * One run at a time per view; a completion speaks only while its run is still the latest one. A failure is only a
 * state: its rejection's text is never shown.
 */
function useFencedAction(key: string | null) {
  const view = useMemo<ActionView | null>(() => key === null ? null : { key, active: false }, [key]);
  const [result, setResult] = useState<ActionResult | null>(null);
  const perform = async (run: () => Promise<void>) => {
    if (view === null || view.active) return;
    view.active = true;
    const started: ActionResult = { view, kind: 'pending' };
    const settle = (next: ActionResult) => setResult((current) => current === started ? next : current);
    setResult(started);
    try {
      await run();
      settle({ view, kind: 'done' });
    } catch {
      settle({ view, kind: 'failed' });
    } finally { view.active = false; }
  };
  return { feedback: result?.view === view ? result : null, perform };
}


function elapsedText(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** One native metadata row; clocks are evidence, never inferred from transcript gaps. */
export function ThreadStatusNotice({ historical = false, heading, children, clock, sending = false, tone = 'neutral', outcome, copyAction = null, editAction = null, regenerateAction = null, detailsExpanded, onDetailsExpandedChange }: {
  detailsExpanded?: boolean;
  onDetailsExpandedChange?: (expanded: boolean) => void;
  historical?: boolean;
  heading: ReactNode;
  children?: ReactNode;
  clock: ConversationMetaClock;
  sending?: boolean;
  tone?: 'neutral' | 'warning' | 'error';
  outcome?: 'completed' | 'interrupted' | 'failed';
  copyAction?: CopyResponseAction | null;
  editAction?: EditAction | null;
  regenerateAction?: ResponseAction | null;
}) {
  const stateRef = useRef<HTMLDivElement | null>(null);
  const lastFocused = useRef<HTMLElement | null>(null);
  const hasDetails = children !== undefined;
  useLayoutEffect(() => {
    const previous = lastFocused.current;
    if (previous !== null && !previous.isConnected && document.activeElement === document.body) {
      const target = stateRef.current;
      if (target !== null) restoreDrawerFocus(target);
    }
  }, [hasDetails]);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const copy = useFencedAction(copyAction === null ? null : JSON.stringify([copyAction.id, copyAction.text]));
  const feedback = copy.feedback;
  const regenerate = useFencedAction(regenerateAction?.id ?? null);
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
    role="status" aria-label={historical ? "Imported historical response" : "Current response status"} data-nc-current-meta={historical ? undefined : ""} data-nc-turn={outcome === undefined ? undefined : 'outcome'}
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
          : <Collapsible defaultIsOpen={false} isOpen={detailsExpanded} onOpenChange={onDetailsExpandedChange} chevronPosition="start" trigger={title}>{children}</Collapsible>}
        {sending && <div className={styles.label}>Sending…</div>}
      </div>
      {!historical && <MessageActions actions={[
        { id: 'copy', kind: feedback?.kind === 'done' ? 'copied' : 'copy',
          label: copyAction === null ? 'Copy response (not available yet)' : feedback === null || feedback.kind === 'pending' ? 'Copy response'
            : feedback.kind === 'done' ? 'Copied response' : 'Could not copy response',
          disabled: copyAction === null || feedback?.kind === 'pending',
          run: () => { if (copyAction !== null) void copy.perform(copyAction.run); } },
        { id: 'edit', kind: 'edit', label: editAction === null ? 'Edit message (not available now)' : 'Edit message',
          tooltip: 'Edit this message. Files are not reverted.', disabled: editAction === null,
          run: () => { editAction?.run(); } },
        { id: 'regenerate', kind: 'regenerate', label: regenerateAction === null ? 'Regenerate response (not available now)' : 'Regenerate response',
          tooltip: 'Send the original prompt again in this conversation; keep existing history.',
          disabled: regenerateAction === null || regenerate.feedback?.kind === 'pending',
          run: () => { if (regenerateAction !== null) void regenerate.perform(regenerateAction.run); } },
      ]} />}
    </div>
  </div>;
}
