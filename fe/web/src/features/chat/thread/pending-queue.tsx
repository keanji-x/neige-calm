// Queued conversation inputs follow the current status using the existing chat controls.
import { useLayoutEffect, useRef } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import type { PendingQueueEntry, PlannerQueueWriteOutcome } from '../../../../../core/domain/conversation.ts';
import type { PendingQueueSend } from '../../../../../core/domain/conversation-outbox.ts';
import { OutgoingMessage } from './outgoing-message.tsx';
import { MessageActions } from './message-actions.tsx';
import { useCompactViewport } from '../../../ui/viewport/public.ts';
import { useState } from '../../../ui/state/public.ts';
import { SizeMotion } from '../../../ui/motion/size.tsx';
import { CollapsePresence } from '../../../ui/motion/collapse.tsx';
import styles from './pending-queue.module.css';

export type PendingQueueProps = Readonly<{
  entries: readonly PendingQueueEntry[];
  sending?: readonly PendingQueueSend[];
  overflow: number;
  busy: boolean;
  onDelete: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
  onEdit: (entry: PendingQueueEntry) => void;
  canEdit: boolean;
  editingEntryId: string | null;
  savedEntry: PendingQueueEntry | null;
  onFocusComposer: () => void;
  onSteer?: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
}>;
const NO_SENDS: readonly PendingQueueSend[] = Object.freeze([]);
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


export function PendingQueue({ entries, sending = NO_SENDS, overflow, busy, onDelete, onEdit, canEdit, editingEntryId, savedEntry, onFocusComposer, onSteer }: PendingQueueProps) {
  const mobile = useCompactViewport();
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [writing, setWriting] = useState(false);
  const writeLease = useRef(false);
  const [saved, setSaved] = useState<ReadonlyMap<string, PendingQueueEntry>>(() => new Map());
  const [observedSave, setObservedSave] = useState(savedEntry);
  if (savedEntry !== observedSave) {
    setObservedSave(savedEntry);
    if (savedEntry !== null) setSaved(previous => new Map(previous).set(savedEntry.entry_id, savedEntry));
  }
  const [snapshot, setSnapshot] = useState(() => ({ source: entries, saved, rows: entries }));
  if (snapshot.source !== entries || snapshot.saved !== saved) {
    const activeIds = new Set(entries.map(entry => entry.entry_id));
    const effective = (entry: PendingQueueEntry) => {
      const accepted = saved.get(entry.entry_id);
      return accepted !== undefined && accepted.rev >= entry.rev ? accepted : entry;
    };
    // Active rows follow server order. Only departing ghosts retain their old neighbor.
    const rows = entries.map(effective);
    snapshot.rows.forEach((row, index) => {
      if (activeIds.has(row.entry_id)) return;
      const next = snapshot.rows.slice(index + 1).find(candidate => activeIds.has(candidate.entry_id));
      const position = next === undefined ? rows.length : rows.findIndex(candidate => candidate.entry_id === next.entry_id);
      rows.splice(position, 0, effective(row));
    });
    setSnapshot({ source: entries, saved, rows });
  }
  const root = useRef<HTMLElement>(null);
  const [focus, requestFocus] = useState<Readonly<{ kind: 'button' | 'header'; id: string }> | null>(null);
  useLayoutEffect(() => {
    if (focus === null) return;
    const row = [...(root.current?.querySelectorAll<HTMLElement>('[data-nc-pending-entry]') ?? [])]
      .find(row => row.dataset.ncPendingEntry === focus.id);
    const target = focus.kind === 'button' ? row?.querySelector<HTMLElement>('[aria-label="Edit queued message"]') : undefined;
    target?.focus({ preventScroll: true });
    if (target === undefined || document.activeElement !== target) root.current?.focus({ preventScroll: true });
  }, [focus]);
  const blocked = busy || writing;
  const ids = new Set(entries.map(entry => entry.entry_id));
  const count = entries.length + sending.length + overflow;
  const retained = snapshot.rows.length > 0;
  const visible = count > 0 || retained;
  const run = async (entry: PendingQueueEntry, action: () => Promise<PlannerQueueWriteOutcome>) => {
    if (blocked || writeLease.current) return;
    writeLease.current = true;
    setWriting(true); setRefusal(null);
    try {
      const outcome = await action();
      if (outcome.kind !== 'done') setRefusal({ entryId: entry.entry_id, outcome });
      else if (editingEntryId !== null) onFocusComposer();
      else requestFocus({ kind: 'header', id: '' });
    } catch (error) {
      setRefusal({ entryId: entry.entry_id, outcome: { kind: 'failed', message: error instanceof Error ? error.message : 'Could not change this message.' } });
    } finally { writeLease.current = false; setWriting(false); }
  };
  return <SizeMotion motionKey={visible}>
    {visible && <section ref={root} className={styles.queue} data-nc-pending-queue="" aria-label="Queued messages" tabIndex={-1}>
      <div className={styles.list}>
          {snapshot.rows.map(entry => {
            const isEditing = editingEntryId === entry.entry_id;
            const alive = ids.has(entry.entry_id);
            const failed = refusal?.entryId === entry.entry_id ? refusal.outcome : null;
            const current = failed?.kind === 'stale' ? { ...entry, text: failed.text, rev: failed.rev } : entry;
            return <CollapsePresence key={entry.entry_id} present={alive || isEditing} onExited={() => {
              setSnapshot(previous => ({ ...previous, rows: previous.rows.filter(row => row.entry_id !== entry.entry_id) }));
              setSaved(previous => { const next = new Map(previous); next.delete(entry.entry_id); return next; });
            }}>
              <SizeMotion motionKey={isEditing}><div className={styles.rowSpace}><div className={styles.row} data-nc-pending-entry={alive || isEditing ? entry.entry_id : undefined}>
                <div className={styles.content}>
                  <div className={styles.line}>
                    <div data-nc-pending-entry-text=""><OutgoingMessage text={current.text} mobile={mobile} edited={isEditing} /></div>
                    <div className={styles.actions}>
                      <MessageActions label="Queued message actions" actions={[
                        { id: 'edit', kind: 'edit', label: 'Edit queued message', tooltip: 'Edit this message in the composer.', disabled: blocked || !canEdit || !alive,
                          run: () => { onEdit(current); } },
                        { id: 'undo', kind: 'undo', label: 'Delete this message', tooltip: 'Remove this queued message.', disabled: blocked || isEditing,
                          run: () => { void run(current, () => onDelete(current)); } },
                        ...(onSteer === undefined ? [] : [{ id: 'steer', kind: 'return' as const, label: 'Say it now', tooltip: 'Send to the current response.', disabled: blocked || isEditing,
                          run: () => { void run(current, () => onSteer(current)); } }]),
                      ]} />
                    </div>
                  </div>
                  {failed !== null && <QueueWriteNotice outcome={failed} />}
                </div>
              </div></div></SizeMotion>
            </CollapsePresence>;
          })}
          {sending.map(message => <div key={message.id} className={styles.sending} data-nc-pending-send={message.id}>
            <div data-nc-queued={message.delivery === 'waiting' ? '' : undefined}><OutgoingMessage text={message.text} mobile={mobile} /></div>
            <span className={styles.delivery} role="status" data-nc-queued-note={message.delivery === 'waiting' ? '' : undefined}>{message.delivery === 'failed' ? '发送未完成' : message.delivery === 'paused' ? '已暂停' : message.delivery === 'checking' ? '等待确认…' : message.delivery === 'waiting' ? '等待处理…' : '正在发送…'}</span>
          </div>)}
          {overflow > 0 && <p className={styles.notice} role="status" data-nc-pending-overflow="">
            {`${overflow} ${entries.length > 0 ? 'more ' : ''}queued message` + `${overflow === 1 ? ' is' : 's are'} waiting but cannot be shown or edited here.`}
          </p>}
        </div>
    </section>}
  </SizeMotion>;
}

/** One refusal presentation shared by queue actions and the retained composer edit. */
export function QueueWriteNotice({ outcome }: Readonly<{ outcome: PlannerQueueWriteOutcome }>) {
  return noticeText(outcome) === null ? null : <div className={styles.notice} data-nc-pending-entry-notice="">
    <Banner status={noticeStatus(outcome)} title={noticeHeading(outcome)} description={noticeText(outcome)} />
  </div>;
}
