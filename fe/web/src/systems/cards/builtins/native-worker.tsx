import type { AgentProvider } from '../../../../../core/api/generated/wire.ts';
import { ReadOnlyCode } from '../../../ui/code/public.tsx';
import { useRef, useSyncExternalStore } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Button } from '@astryxdesign/core/Button';
import { ActivityCard } from '../../../ui/activity-card/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { readErrorText } from '../../../../../core/domain/read-failure.ts';
import { providerPresentation } from '../../../../../core/domain/provider-presentation.ts';
import { workerSelectionDescription } from '../../../../../core/domain/task-selection.ts';
import { trackDetail } from '../../../../../core/events/query-keys.ts';
import { CardHead } from '../ui/card-head.tsx';
import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' { interface CardDataMap { 'native-worker': NativeWorkerCard } }
export type NativeWorkerCard = Readonly<{ type: 'native-worker'; id: string; title: string | null;
  trackId: string; taskKey: string; attemptId: string; provider: AgentProvider | null }>;
export function isNativeWorkerPayload(payload: unknown): payload is Record<string, unknown> {
  return typeof payload === 'object' && payload !== null && 'acp_worker' in payload && payload.acp_worker === true;
}
/** Uses the kernel's existing flow projection and task identity; never opens a terminal socket. */
export function NativeWorkerCardView({ card, host, onRemove, header, notice }: CardComponentProps<NativeWorkerCard>) {
  const port = host.nativeWorker;
  const visible = useSyncExternalStore(listener => host.lifecycle.subscribe(listener), () => host.lifecycle.getSnapshot().visible);
  const [text, setText] = useState('');
  const [pending, setPending] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);
  const inFlight = useRef(false);
  const retry = useRef<{ text: string; key: string } | null>(null);
  const flow = useQuery({ queryKey: [...trackDetail(card.trackId), 'worker-flow', card.id, card.attemptId],
    queryFn: ({ signal }) => port!.read(card.trackId, card.taskKey, card.id, card.attemptId, signal), enabled: port != null && visible,
    retry: false, refetchInterval: visible ? 1500 : false,
  });
  const current = flow.data?.attempt;
  const same = flow.data?.currentAttemptId === card.attemptId;
  const selection = current?.worker_selection;
  const active = same && (current?.status === 'running' || current?.status === 'dispatched') && selection?.cleanup_pending !== true;
  const presentation = providerPresentation(card.provider);
  const act = async (cancel: boolean) => {
    if (port == null || inFlight.current || !active) return;
    inFlight.current = true; setPending(true); setFeedback(null);
    try {
      if (cancel) {
        await port.cancel(card.trackId, card.taskKey, card.attemptId);
        setFeedback('Cancellation requested · awaiting native round settlement');
      } else {
        const submission = retry.current?.text === text ? retry.current : { text, key: crypto.randomUUID() };
        retry.current = submission;
        await port.message(card.trackId, card.taskKey, card.attemptId, submission.key, submission.text);
        retry.current = null; setText(''); setFeedback('User message queued · the Worker must read it before reporting');
      }
    } catch (error) { setFeedback(readErrorText(error, 'Worker action could not be completed.')); }
    finally { inFlight.current = false; setPending(false); }
  };
  return <ActivityCard header={<CardHead header={header} title={card.title ?? `${presentation?.label ?? card.provider ?? 'Unknown provider'} Worker`}
      onDelete={onRemove} deleteLabel={`Delete card ${card.title ?? card.taskKey}`} />}
    details={<>
      <p><Icon name={presentation?.icon ?? 'agent'} size="sm" /> {presentation?.label ?? card.provider ?? 'Unknown provider'} · {current?.status ?? 'Loading execution…'}</p>
      <p>Requested model: {selection === undefined ? '未记录' : selection.requested_model ?? 'Provider default'} · effort: {selection === undefined ? '未记录' : selection.requested_effort ?? 'Provider default'}</p>
      <p>{selection === undefined ? 'Effective model / effort: 未记录' : workerSelectionDescription(selection)}</p>
      {selection?.cleanup_pending === true && <p role="status">{selection.turn_phase === 'unknown' ? 'Unknown · cleanup pending; workspace remains occupied' : 'Awaiting native round settlement; workspace remains occupied'}</p>}
      {notice !== null && <p role="status">{notice}</p>}
      {port == null && <p role="status">Worker flow is unavailable in this host.</p>}
      {flow.error !== null && <p role="alert">{readErrorText(flow.error, 'Worker flow could not be read.')}</p>}
    </>}
    activityLabel="Worker flow" activity={<ReadOnlyCode text={flow.data?.flow ?? 'Waiting for captured Worker activity…'} source={{ kind: 'language', value: 'markdown' }} />}
    controls={<>
      <label>User message<textarea aria-label="User message" value={text} disabled={!active || pending} onChange={event => setText(event.target.value)} /></label>
      <Button label="Queue message" isDisabled={!active || pending || text.trim() === ''} onClick={() => { void act(false); }} />
      <Button label="Cancel task" isDisabled={!active || pending || current?.status !== 'running'} onClick={() => { void act(true); }} />
      {feedback !== null && <p role="status">{feedback}</p>}
    </>} />;
}
export const NATIVE_WORKER_CARD_ENTRY = Object.freeze({ type: 'native-worker', headless: false,
  component: (props: CardComponentProps<NativeWorkerCard>) => <NativeWorkerCardView {...props} />, defaultSize: Object.freeze({ w: 6, h: 12, minW: 4, minH: 6 }),
  title: (card: NativeWorkerCard) => card.title ?? `${providerPresentation(card.provider)?.label ?? card.provider ?? 'Unknown provider'} Worker`,
  accessibleName: (card: NativeWorkerCard) => `${providerPresentation(card.provider)?.label ?? card.provider ?? 'Unknown provider'} Worker`,
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
  fromKernel: (card: KernelCardInput): NativeWorkerCard | null => {
    if (!isNativeWorkerPayload(card.payload)) return null;
    const payload = card.payload;
    if (typeof payload.track_id !== 'string' || typeof payload.task_key !== 'string' || typeof payload.idempotency_key !== 'string') return null;
    return Object.freeze({ type: 'native-worker', id: card.id, title: null, trackId: payload.track_id,
      taskKey: payload.task_key, attemptId: payload.idempotency_key, provider: card.runtime?.provider ?? null });
  },
}) satisfies CardEntry<NativeWorkerCard>;
