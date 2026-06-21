import type { QueryClient, QueryKey } from '@tanstack/react-query';
import { queryKeys } from '../api/queries';
import type { KernelCove, WireEvent } from '../api/wire';

export type EventKind = WireEvent['ev'];
export type EventOf<K extends EventKind> = Extract<WireEvent, { ev: K }>;

export interface InvalidationContext {
  qc: QueryClient;
  findWaveOwningCard(cardId: string): string | null;
}

export interface InvalidationPolicy<K extends EventKind = EventKind> {
  apply?: (ev: EventOf<K>, ctx: InvalidationContext) => void;
  keys?: (ev: EventOf<K>) => QueryKey[];
  requiresContext?: (ev: EventOf<K>, ctx: InvalidationContext) => QueryKey[];
  remove?: (ev: EventOf<K>, ctx: InvalidationContext) => QueryKey[];
  reason?: string;
}

export function definePolicies<T extends { [K in EventKind]: InvalidationPolicy<K> }>(
  value: T,
): T {
  return value;
}

export function noop<K extends EventKind>(reason: string): InvalidationPolicy<K> {
  return { reason };
}

export function overlayInvalidationKeys(
  ev: EventOf<'overlay.set'> | EventOf<'overlay.deleted'>,
): QueryKey[] {
  const { entity_kind, entity_id } = ev.data;
  const keys: QueryKey[] = [];
  if (entity_kind === 'wave' || entity_kind === 'card') {
    keys.push(queryKeys.overlaysByKind(entity_kind));
  }
  if (entity_kind === 'wave') {
    keys.push(queryKeys.waveDetail(entity_id));
  }
  return keys;
}

function cardOverlayContextKeys(
  ev: EventOf<'overlay.set'> | EventOf<'overlay.deleted'>,
  ctx: InvalidationContext,
): QueryKey[] {
  if (ev.data.entity_kind !== 'card') return [];
  const waveId = ctx.findWaveOwningCard(ev.data.entity_id);
  return waveId ? [queryKeys.waveDetail(waveId)] : [];
}

type RuntimeCardEvent =
  | EventOf<'runtime.started'>
  | EventOf<'runtime.status_changed'>
  | EventOf<'runtime.superseded'>;

function runtimeCardContextKeys(
  ev: RuntimeCardEvent,
  ctx: InvalidationContext,
): QueryKey[] {
  const waveId = ctx.findWaveOwningCard(ev.data.card_id);
  return waveId ? [queryKeys.waveDetail(waveId)] : [];
}

const waveFilesKey = (waveId: unknown): QueryKey =>
  typeof waveId === 'string' && waveId.length > 0
    ? queryKeys.waveFiles(waveId)
    : ['wave-files'];

type WaveFilesDerivedEvent =
  | RuntimeCardEvent
  | EventOf<'codex.hook'>
  | EventOf<'claude.hook'>
  | EventOf<'codex.worker_requested'>
  | EventOf<'terminal.worker_requested'>
  | EventOf<'task.dispatched'>
  | EventOf<'task.completed'>
  | EventOf<'task.failed'>
  | EventOf<'task.gate_result'>
  | EventOf<'terminal.deleted'>;

function waveFilesDerivedEventKeys(
  ev: WaveFilesDerivedEvent,
  ctx: InvalidationContext,
): QueryKey[] {
  const data = ev.data as { wave_id?: unknown; card_id?: unknown };
  if (typeof data.wave_id === 'string' && data.wave_id.length > 0) {
    return [waveFilesKey(data.wave_id)];
  }
  if (typeof data.card_id === 'string' && data.card_id.length > 0) {
    return [waveFilesKey(ctx.findWaveOwningCard(data.card_id))];
  }
  return [waveFilesKey(undefined)];
}

function runtimeContextKeys(
  ev: RuntimeCardEvent,
  ctx: InvalidationContext,
): QueryKey[] {
  return [
    ...runtimeCardContextKeys(ev, ctx),
    ...waveFilesDerivedEventKeys(ev, ctx),
  ];
}

const waveMutationKeys = (ev: EventOf<'wave.updated'> | EventOf<'wave.lifecycle_changed'>) => [
  queryKeys.wavesInCove(ev.data.cove_id),
  queryKeys.waveDetail(ev.data.id),
  waveFilesKey(ev.data.id),
  ['waves-range'],
];

const cardMutationKeys = (
  ev: EventOf<'card.added'> | EventOf<'card.updated'> | EventOf<'card.deleted'>,
) => [queryKeys.waveDetail(ev.data.wave_id), waveFilesKey(ev.data.wave_id)];

export const invalidationPolicies: { [K in EventKind]: InvalidationPolicy<K> } = definePolicies({
  'cove.updated': {
    apply: (ev, { qc }) => {
      const updated = ev.data;
      qc.setQueryData<KernelCove[]>(queryKeys.coves(), (prev) => {
        if (!prev) return prev;
        const idx = prev.findIndex((c) => c.id === updated.id);
        if (idx === -1) return prev;
        const next = prev.slice();
        next[idx] = updated;
        return next;
      });
    },
    keys: () => [queryKeys.coves()],
  },
  'cove.deleted': {
    keys: () => [queryKeys.coves(), queryKeys.overlaysByKind('wave')],
  },
  'wave.updated': {
    keys: waveMutationKeys,
  },
  'wave.deleted': {
    keys: (ev) => [
      queryKeys.wavesInCove(ev.data.cove_id),
      queryKeys.overlaysByKind('wave'),
      ['waves-range'],
    ],
    remove: (ev) => [queryKeys.waveDetail(ev.data.id)],
  },
  'wave.lifecycle_changed': {
    keys: waveMutationKeys,
  },
  'card.added': {
    keys: cardMutationKeys,
  },
  'card.updated': {
    keys: cardMutationKeys,
  },
  'card.deleted': {
    keys: cardMutationKeys,
  },
  'runtime.started': {
    requiresContext: runtimeContextKeys,
    keys: () => [queryKeys.overlaysByKind('card')],
  },
  'runtime.status_changed': {
    requiresContext: runtimeContextKeys,
    keys: () => [queryKeys.overlaysByKind('card')],
  },
  'runtime.superseded': {
    requiresContext: runtimeContextKeys,
    keys: () => [queryKeys.overlaysByKind('card')],
    // No runtime-detail cache key exists yet; old runtime id removal is a
    // no-op for now. The registry can refine this when a consumer appears.
  },
  'harness.item.added': noop(
    'Report view card-topic consumers handle harness item payloads directly.',
  ),
  'harness.phase.changed': noop(
    'Report page card-topic consumers handle harness phase updates directly.',
  ),
  'harness.transcript.cleared': noop(
    'Report view card-topic consumers reset local transcript state directly.',
  ),
  'harness.user_message.enqueued': noop(
    'Report view card-topic consumers observe queued user messages directly.',
  ),
  'wave.report_edited': {
    keys: (ev) => [waveFilesKey(ev.data.wave_id)],
    reason:
      'report.md in the wave file projection changes when the report is edited.',
  },
  'overlay.set': {
    keys: overlayInvalidationKeys,
    requiresContext: cardOverlayContextKeys,
  },
  'overlay.deleted': {
    keys: overlayInvalidationKeys,
    requiresContext: cardOverlayContextKeys,
  },
  'terminal.deleted': {
    requiresContext: waveFilesDerivedEventKeys,
    reason:
      "cards/<id>/runtime.json projects terminal runtime status; reaping a terminal invalidates that projection.",
  },
  'plugin.state': noop('No plugin list query exists yet.'),
  'plugin.tool.registered': noop('No plugin-tool catalog query exists yet.'),
  'workflow.registered': noop('No workflow catalog query exists yet.'),
  'codex.hook': {
    requiresContext: waveFilesDerivedEventKeys,
    reason: 'Codex card topic consumers handle codex hook payloads directly.',
  },
  'claude.hook': {
    requiresContext: waveFilesDerivedEventKeys,
    reason: 'Card topic consumers handle claude hook payloads directly.',
  },
  'codex.worker_requested': {
    requiresContext: waveFilesDerivedEventKeys,
    reason: 'Dispatcher consumes codex worker requests directly from the event bus.',
  },
  'terminal.worker_requested': {
    requiresContext: waveFilesDerivedEventKeys,
    reason: 'Dispatcher consumes terminal worker requests directly from the event bus.',
  },
  'task.completed': {
    requiresContext: waveFilesDerivedEventKeys,
    reason: 'Dispatcher and spec-agent waiters consume task completion directly.',
  },
  'task.failed': {
    requiresContext: waveFilesDerivedEventKeys,
    reason: 'Dispatcher and spec-agent waiters consume task failure directly.',
  },
  'plan.updated': noop(
    'No task-plan query exists yet; the PR-B scheduler consumes plan revisions server-side.',
  ),
  'task.dispatched': {
    requiresContext: waveFilesDerivedEventKeys,
    reason:
      'Scheduler claim record (#644 PR-B) — the runs views derive their requested-record from it; same surface task.completed/failed refresh.',
  },
  'workspace.leased': noop(
    'Workspace lease lifecycle is card-scoped; no React Query cache consumes lease rows yet.',
  ),
  'workspace.released': noop(
    'Workspace lease lifecycle is card-scoped; no React Query cache consumes lease rows yet.',
  ),
  'forge.pr.merged': noop(
    'Forge merge lifecycle is card/wave-scoped; no React Query cache consumes forge merge rows yet.',
  ),
  'forge.scan.completed': noop(
    'Forge scan lifecycle is wave-scoped; no React Query cache consumes forge scan rows yet.',
  ),
  'forge.pr.opened': noop(
    'Forge PR lifecycle is wave-scoped; no React Query cache consumes opened PR rows yet.',
  ),
  'forge.pr.diff.read': noop(
    'Forge diff artifacts are persisted for workflow ordering; no React Query cache consumes diff-read rows yet.',
  ),
  'forge.pr.checks': noop(
    'Forge checks lifecycle is wave-scoped; no React Query cache consumes checks rows yet.',
  ),
  'forge.issue.closed': noop(
    'Forge issue lifecycle is wave-scoped; no React Query cache consumes issue-close rows yet.',
  ),
  'worktree.provisioned': noop(
    'Git worktree provisioning is card-scoped; no React Query cache consumes worktree rows yet.',
  ),
  'worktree.removed': noop(
    'Git worktree teardown is card-scoped; no React Query cache consumes worktree rows yet.',
  ),
  'task.gate_result': {
    requiresContext: waveFilesDerivedEventKeys,
    reason:
      'Gate-runner verdict (#644 PR-C) — flips the plan-task row done/failed; refreshes the same runs/wave-files surface as task.completed/failed.',
  },
});
