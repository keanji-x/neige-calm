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

function runtimeCardContextKeys(
  ev:
    | EventOf<'runtime.started'>
    | EventOf<'runtime.status_changed'>
    | EventOf<'runtime.superseded'>,
  ctx: InvalidationContext,
): QueryKey[] {
  const waveId = ctx.findWaveOwningCard(ev.data.card_id);
  return waveId ? [queryKeys.waveDetail(waveId)] : [];
}

const waveMutationKeys = (ev: EventOf<'wave.updated'> | EventOf<'wave.lifecycle_changed'>) => [
  queryKeys.wavesInCove(ev.data.cove_id),
  queryKeys.waveDetail(ev.data.id),
  ['waves-range'],
];

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
    keys: (ev) => [queryKeys.waveDetail(ev.data.wave_id)],
  },
  'card.updated': {
    keys: (ev) => [queryKeys.waveDetail(ev.data.wave_id)],
  },
  'card.deleted': {
    keys: (ev) => [queryKeys.waveDetail(ev.data.wave_id)],
  },
  'runtime.started': {
    requiresContext: runtimeCardContextKeys,
    keys: () => [queryKeys.overlaysByKind('card')],
  },
  'runtime.status_changed': {
    requiresContext: runtimeCardContextKeys,
    keys: () => [queryKeys.overlaysByKind('card')],
  },
  'runtime.superseded': {
    requiresContext: runtimeCardContextKeys,
    keys: () => [queryKeys.overlaysByKind('card')],
    // No runtime-detail cache key exists yet; old runtime id removal is a
    // no-op for now. The registry can refine this when a consumer appears.
  },
  'harness.item.added': noop(
    'Spec ChatTimeline card-topic consumers handle harness item payloads directly.',
  ),
  'harness.phase.changed': noop(
    'SpecCard card-topic consumers handle harness phase updates directly.',
  ),
  'harness.transcript.cleared': noop(
    'Spec ChatTimeline card-topic consumers reset local transcript state directly.',
  ),
  'wave.report_edited': noop(
    'Companion card.updated invalidates the report card projection.',
  ),
  'overlay.set': {
    keys: overlayInvalidationKeys,
    requiresContext: cardOverlayContextKeys,
  },
  'overlay.deleted': {
    keys: overlayInvalidationKeys,
    requiresContext: cardOverlayContextKeys,
  },
  'terminal.deleted': noop(
    "Terminal rows are not read directly by the calendar, sidebar, or wave-list views.",
  ),
  'plugin.state': noop('No plugin list query exists yet.'),
  'codex.hook': noop(
    'Codex card topic consumers handle codex hook payloads directly.',
  ),
  'claude.hook': noop('Card topic consumers handle claude hook payloads directly.'),
  'codex.job_requested': noop(
    'Dispatcher consumes codex job requests directly from the event bus.',
  ),
  'terminal.job_requested': noop(
    'Dispatcher consumes terminal job requests directly from the event bus.',
  ),
  'task.completed': noop(
    'Dispatcher and spec-agent waiters consume task completion directly.',
  ),
  'task.failed': noop('Dispatcher and spec-agent waiters consume task failure directly.'),
  'spec_push.abandoned': {
    keys: (ev) => [
      queryKeys.wavesInCove(ev.data.cove_id),
      queryKeys.waveDetail(ev.data.wave_id),
      ['waves-range'],
    ],
  },
});
