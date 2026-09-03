import type { QueryClient, QueryKey } from '@tanstack/react-query';
import { queryKeys } from '../api/queries';
import type { KernelArea, WireEvent } from '../api/wire';

export type EventKind = WireEvent['ev'];
export type EventOf<K extends EventKind> = Extract<WireEvent, { ev: K }>;

export interface InvalidationContext {
  qc: QueryClient;
  findTrackOwningCard(cardId: string): string | null;
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
  if (entity_kind === 'track' || entity_kind === 'card') {
    keys.push(queryKeys.overlaysByKind(entity_kind));
  }
  if (entity_kind === 'track') {
    keys.push(queryKeys.trackDetail(entity_id));
  }
  return keys;
}

function cardOverlayContextKeys(
  ev: EventOf<'overlay.set'> | EventOf<'overlay.deleted'>,
  ctx: InvalidationContext,
): QueryKey[] {
  if (ev.data.entity_kind !== 'card') return [];
  const trackId = ctx.findTrackOwningCard(ev.data.entity_id);
  return trackId ? [queryKeys.trackDetail(trackId)] : [];
}

type RuntimeCardEvent =
  | EventOf<'runtime.started'>
  | EventOf<'runtime.status_changed'>
  | EventOf<'runtime.superseded'>;

function runtimeCardContextKeys(
  ev: RuntimeCardEvent,
  ctx: InvalidationContext,
): QueryKey[] {
  const trackId = ctx.findTrackOwningCard(ev.data.card_id);
  return trackId ? [queryKeys.trackDetail(trackId)] : [];
}

const trackFilesKey = (trackId: unknown): QueryKey =>
  typeof trackId === 'string' && trackId.length > 0
    ? queryKeys.trackFiles(trackId)
    : ['track-files'];

type TrackFilesDerivedEvent =
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

function trackFilesDerivedEventKeys(
  ev: TrackFilesDerivedEvent,
  ctx: InvalidationContext,
): QueryKey[] {
  const data = ev.data as { track_id?: unknown; card_id?: unknown };
  if (typeof data.track_id === 'string' && data.track_id.length > 0) {
    return [trackFilesKey(data.track_id), queryKeys.trackReport(data.track_id)];
  }
  if (typeof data.card_id === 'string' && data.card_id.length > 0) {
    const trackId = ctx.findTrackOwningCard(data.card_id);
    return trackId
      ? [trackFilesKey(trackId), queryKeys.trackReport(trackId)]
      : [trackFilesKey(undefined), ['track-report']];
  }
  return [trackFilesKey(undefined), ['track-report']];
}

function runtimeContextKeys(
  ev: RuntimeCardEvent,
  ctx: InvalidationContext,
): QueryKey[] {
  return [
    ...runtimeCardContextKeys(ev, ctx),
    ...trackFilesDerivedEventKeys(ev, ctx),
  ];
}

const trackMutationKeys = (ev: EventOf<'track.updated'> | EventOf<'track.lifecycle_changed'>) => [
  queryKeys.tracksInArea(ev.data.area_id),
  queryKeys.trackDetail(ev.data.id),
  trackFilesKey(ev.data.id),
  ['tracks-range'],
];

const cardMutationKeys = (
  ev: EventOf<'card.added'> | EventOf<'card.updated'> | EventOf<'card.deleted'>,
) => [queryKeys.trackDetail(ev.data.track_id), trackFilesKey(ev.data.track_id)];

export const invalidationPolicies: { [K in EventKind]: InvalidationPolicy<K> } = definePolicies({
  'area.updated': {
    apply: (ev, { qc }) => {
      const updated = ev.data;
      qc.setQueryData<KernelArea[]>(queryKeys.areas(), (prev) => {
        if (!prev) return prev;
        const idx = prev.findIndex((c) => c.id === updated.id);
        if (idx === -1) return prev;
        const next = prev.slice();
        next[idx] = updated;
        return next;
      });
    },
    keys: () => [queryKeys.areas()],
  },
  'area.deleted': {
    keys: () => [queryKeys.areas(), queryKeys.overlaysByKind('track')],
  },
  'track.updated': {
    keys: trackMutationKeys,
  },
  'track.deleted': {
    keys: (ev) => [
      queryKeys.tracksInArea(ev.data.area_id),
      queryKeys.overlaysByKind('track'),
      ['tracks-range'],
    ],
    remove: (ev) => [queryKeys.trackDetail(ev.data.id)],
  },
  'track.lifecycle_changed': {
    keys: trackMutationKeys,
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
  'track.report_edited': {
    keys: (ev) => [
      trackFilesKey(ev.data.track_id),
      queryKeys.trackReport(ev.data.track_id),
      ['track-backlinks'],
    ],
    reason:
      'Report edits change the file and structured report projections and may change backlinks for any track.',
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
    requiresContext: trackFilesDerivedEventKeys,
    reason:
      "cards/<id>/runtime.json projects terminal runtime status; reaping a terminal invalidates that projection.",
  },
  'plugin.state': noop('No plugin list query exists yet.'),
  'plugin.tool.registered': noop('No plugin-tool catalog query exists yet.'),
  'codex.hook': {
    requiresContext: trackFilesDerivedEventKeys,
    reason: 'Codex card topic consumers handle codex hook payloads directly.',
  },
  'claude.hook': {
    requiresContext: trackFilesDerivedEventKeys,
    reason: 'Card topic consumers handle claude hook payloads directly.',
  },
  'codex.worker_requested': {
    requiresContext: trackFilesDerivedEventKeys,
    reason: 'Dispatcher consumes codex worker requests directly from the event bus.',
  },
  'terminal.worker_requested': {
    requiresContext: trackFilesDerivedEventKeys,
    reason: 'Dispatcher consumes terminal worker requests directly from the event bus.',
  },
  'task.completed': {
    requiresContext: trackFilesDerivedEventKeys,
    reason: 'Dispatcher and planner-agent waiters consume task completion directly.',
  },
  'task.failed': {
    requiresContext: trackFilesDerivedEventKeys,
    reason: 'Dispatcher and planner-agent waiters consume task failure directly.',
  },
  'plan.updated': noop(
    'No task-plan query exists yet; the PR-B scheduler consumes plan revisions server-side.',
  ),
  'task.dispatched': {
    requiresContext: trackFilesDerivedEventKeys,
    reason:
      'Scheduler claim record (#644 PR-B) — the runs views derive their requested-record from it; same surface task.completed/failed refresh.',
  },
  'task.context_frozen': noop(
    'Frozen task context is scheduler audit state; no React Query cache consumes it yet.',
  ),
  'task.context_advanced': noop(
    'Context advancement is scheduler audit state; later slices expose task diagnostics.',
  ),
  'workspace.leased': noop(
    'Workspace lease lifecycle is card-scoped; no React Query cache consumes lease rows yet.',
  ),
  'workspace.released': noop(
    'Workspace lease lifecycle is card-scoped; no React Query cache consumes lease rows yet.',
  ),
  'forge.pr.merged': noop(
    'Forge merge lifecycle is card/track-scoped; no React Query cache consumes forge merge rows yet.',
  ),
  'review.round': noop(
    'Review convergence rounds are planner-observed workflow history; no React Query cache consumes them yet.',
  ),
  'ratify.requested': noop(
    'Ratification requests are planner-observed workflow history; no React Query cache consumes them yet.',
  ),
  'ratify.resolved': noop(
    'Ratification decisions are planner-observed workflow history; no React Query cache consumes them yet.',
  ),
  'proposal.submitted': noop(
    'Historical proposal events remain parseable, but the proposal channel and its UI are withdrawn.',
  ),
  'proposal.resolved': noop(
    'Historical proposal events remain parseable, but the proposal channel and its UI are withdrawn.',
  ),
  'forge.scan.completed': noop(
    'Forge scan lifecycle is track-scoped; no React Query cache consumes forge scan rows yet.',
  ),
  'forge.pr.opened': noop(
    'Forge PR lifecycle is track-scoped; no React Query cache consumes opened PR rows yet.',
  ),
  'forge.pr.diff.read': noop(
    'Forge diff artifacts are persisted for workflow ordering; no React Query cache consumes diff-read rows yet.',
  ),
  'forge.pr.checks': noop(
    'Forge checks lifecycle is track-scoped; no React Query cache consumes checks rows yet.',
  ),
  'forge.issue.read': noop(
    'Forge issue read artifacts are persisted for workflow ordering; no React Query cache consumes issue-read rows yet.',
  ),
  'forge.issue.closed': noop(
    'Forge issue lifecycle is track-scoped; no React Query cache consumes issue-close rows yet.',
  ),
  'worktree.provisioned': noop(
    'Git worktree provisioning is card-scoped; no React Query cache consumes worktree rows yet.',
  ),
  'worktree.committed': noop(
    'Git worktree commit is card-scoped; no React Query cache consumes worktree rows yet.',
  ),
  'worktree.removed': noop(
    'Git worktree teardown is card-scoped; no React Query cache consumes worktree rows yet.',
  ),
  'task.gate_result': {
    requiresContext: trackFilesDerivedEventKeys,
    reason:
      'Gate-runner verdict (#644 PR-C) — flips the plan-task row done/failed; refreshes the same runs/track-files surface as task.completed/failed.',
  },
});
