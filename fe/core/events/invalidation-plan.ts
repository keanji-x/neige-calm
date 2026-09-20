import type { Area, WireEvent } from '../api/schemas.js';

export type EventKind = WireEvent['ev'];
export type EventOf<K extends EventKind> = Extract<WireEvent, { ev: K }>;
export type QueryKey = readonly unknown[];

export type CacheWrite = Readonly<{ key: QueryKey; mode: 'replace-existing-area'; value: Area }>;
export type InvalidationPlan = Readonly<{
  invalidate: readonly QueryKey[];
  remove: readonly QueryKey[];
  writeThrough: readonly CacheWrite[];
}>;
export type InvalidationContext = Readonly<{
  findTrackOwningCard(cardId: string): string | null;
}>;

type PlannedPolicy<K extends EventKind = EventKind> = Readonly<{
  type: 'plan';
  plan(event: EventOf<K>, context: InvalidationContext): InvalidationPlan;
}>;
type NoopPolicy = Readonly<{ type: 'noop'; reason: string }>;
export type InvalidationPolicy<K extends EventKind = EventKind> = PlannedPolicy<K> | NoopPolicy;
type PolicyMap = { readonly [K in EventKind]: InvalidationPolicy<K> };

const emptyContext: InvalidationContext = Object.freeze({ findTrackOwningCard: () => null });

export function noop(reason: string): NoopPolicy {
  if (reason.length === 0) throw new TypeError('A no-op invalidation policy requires a reason');
  return { type: 'noop', reason };
}

function plan<K extends EventKind>(
  create: (event: EventOf<K>, context: InvalidationContext) => InvalidationPlan,
): PlannedPolicy<K> {
  return { type: 'plan', plan: create };
}

function result(
  invalidate: readonly QueryKey[],
  remove: readonly QueryKey[] = [],
  writeThrough: readonly CacheWrite[] = [],
): InvalidationPlan {
  return { invalidate, remove, writeThrough };
}

function trackFiles(trackId: string | null): QueryKey {
  return trackId === null ? ['track-files'] : ['track-files', trackId];
}

function trackFilesDerived(trackId: string | null): readonly QueryKey[] {
  return [trackFiles(trackId), trackId === null ? ['track-report'] : ['track-report', trackId]];
}

export type TrackFilesDerivedKind =
  | 'worker_session.started' | 'worker_session.status_changed' | 'worker_session.superseded'
  | 'terminal.deleted' | 'codex.hook' | 'claude.hook'
  | 'codex.worker_requested' | 'terminal.worker_requested'
  | 'task.completed' | 'task.failed' | 'task.execution_settled' | 'task.file_publication_settled' | 'task.candidate_verification_settled' | 'task.git_delivery_settled' | 'task.dispatched' | 'task.gate_result';

/** Every kind that can change what a track's workspace looks like. */
export const TRACK_FILES_DERIVED_KINDS = Object.freeze([
  'worker_session.started', 'worker_session.status_changed', 'worker_session.superseded',
  'terminal.deleted', 'codex.hook', 'claude.hook',
  'codex.worker_requested', 'terminal.worker_requested',
  'task.completed', 'task.failed', 'task.execution_settled', 'task.file_publication_settled', 'task.candidate_verification_settled', 'task.git_delivery_settled', 'task.dispatched', 'task.gate_result',
] as const);

/**
 * Keyed by track; the bare prefix only when the track cannot be resolved (a key with no
 * active observer only marks entries stale, so that fallback is cheap).
 */
function trackConversations(trackId: string | null): QueryKey {
  return trackId === null ? ['track-conversations'] : ['track-conversations', trackId];
}

/**
 * The list's `state` comes from `worker_sessions.state` and its `updated_at` from every harness
 * snapshot persist; `track.lifecycle_changed` is deliberately NOT a caller (it ends sessions via
 * `worker_session.superseded`, already here). The caller set is pinned from both sides in the test.
 */
function conversationLists(trackId: string | null): readonly QueryKey[] {
  return [trackConversations(trackId)];
}

function derivedTrackId(data: unknown, context: InvalidationContext): string | null {
  if (typeof data !== 'object' || data === null) return null;
  const value = data as { track_id?: unknown; card_id?: unknown };
  if (typeof value.track_id === 'string' && value.track_id.length > 0) return value.track_id;
  if (typeof value.card_id === 'string' && value.card_id.length > 0) {
    return context.findTrackOwningCard(value.card_id);
  }
  return null;
}

/**
 * The kinds that invalidate a track's task verdicts (`['track-report', …]`): the workspace kinds
 * minus the two hooks (a hook fires ~twice per tool call and writes no `tasks` row, and the report
 * query re-projects the whole document), plus the non-derived events whose effect can reach other tracks.
 * A function because `no-module-runtime-state` rejects a module-level binding initialised by a call.
 */
export function taskVerdictInvalidatingKinds(): readonly EventKind[] {
  return [
    ...TRACK_FILES_DERIVED_KINDS.filter((kind) => kind !== 'codex.hook' && kind !== 'claude.hook'),
    'track.report_edited', 'track.updated', 'track.deleted', 'area.deleted',
    'card.added', 'card.deleted', 'plan.updated',
  ];
}

/** The three `worker_session.*` kinds share one plan: this card's session moved. */
function runtimePlan(cardId: string, context: InvalidationContext): InvalidationPlan {
  const trackId = context.findTrackOwningCard(cardId);
  return result([
    ...(trackId === null ? [] : [['track', trackId]]),
    ['overlays', 'card'],
    ...trackFilesDerived(trackId),
    ...conversationLists(trackId),
  ]);
}

export function defineInvalidationPolicies<T extends PolicyMap>(value: T): T {
  return value;
}

function policies(): PolicyMap {
  return defineInvalidationPolicies({
  'area.updated': plan((event) => result(
    [['areas']],
    [],
    [{ key: ['areas'], mode: 'replace-existing-area', value: event.data }],
  )),
  'area.deleted': plan(() => result([['areas'], ['overlays', 'track'], ['track-report']])),
  /* The report key is deliberately broad: a root track's tree budget changes the admission
       diagnosis of its child tracks, but the event carries only the updated root id. */
  'track.updated': plan((event) => result([
    ['tracks', 'area', event.data.area_id], ['track', event.data.id],
    ['track-files', event.data.id], ['tracks-range'], ['track-report'],
  ])),
  'track.deleted': plan((event) => result(
    [
      ['tracks', 'area', event.data.area_id], ['overlays', 'track'], ['tracks-range'],
      ['track-report'],
    ],
    [['track', event.data.id], ['track-report', event.data.id]],
  )),
  'track.lifecycle_changed': plan((event) => result([
    ['tracks', 'area', event.data.area_id], ['track', event.data.id],
    ['track-files', event.data.id], ['tracks-range'],
  ])),
  'card.added': plan((event) => result([
    ['track', event.data.track_id], ['track-files', event.data.track_id],
    ['track-report'], ...conversationLists(event.data.track_id),
  ])),
  /* `['planner-run', id]` is keyed by the CARD. Without it a second tab's model picker keeps
       showing the previous model and sends under it; the mutation's own invalidation repairs only its tab. */
  'card.updated': plan((event) => result([
    ['track', event.data.track_id], ['track-files', event.data.track_id],
    ['planner-run', event.data.id],
    ...conversationLists(event.data.track_id),
  ])),
  /* No conversation key: nothing drops a deleted conversation's row today. */
  'card.deleted': plan((event) => result([
    ['track', event.data.track_id], ['track-files', event.data.track_id], ['track-report'],
  ])),
  'worker_session.started': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'worker_session.status_changed': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'worker_session.superseded': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'harness.item.added': plan((event) => result([['harness-items', event.data.card_id]])),
  /* `harness-items` is here because the phase event IS the delivery signal for the per-turn
   * outcome row: the kernel writes it before the snapshot commit and emits no `harness.item.added`. */
  'harness.phase.changed': plan((event) => result([
    ['planner-run', event.data.card_id], ['harness-items', event.data.card_id],
    ...conversationLists(event.data.track_id),
    // Harness observations update runtime activity without a separate worker status event.
    ['track', event.data.track_id],
  ])),
  'harness.transcript.cleared': plan((event) => result([
    ['harness-items', event.data.card_id], ['planner-run', event.data.card_id],
  ])),
  'harness.user_message.enqueued': plan((event) => result([
    ['harness-items', event.data.card_id], ['planner-run', event.data.card_id],
    ...conversationLists(event.data.track_id),
  ])),
  /* `harness-items` is here for `steered` (the kernel writes its transcript row once codex has
   * taken it) and `restored` (whose completion sweep deletes that row again). */
  'harness.queue.changed': plan((event) => result([
    ['planner-run', event.data.card_id], ['harness-items', event.data.card_id],
    ...conversationLists(event.data.track_id),
  ])),
  /* `['today-launchpad']` carries `report_has_noninitial_content` (Today's empty-state predicate) and
   * `['track', id]` is where the document is read from; nothing generated protects either line. */
  'track.report_edited': plan((event) => result([
    ['track-files', event.data.track_id], ['track-report'], ['track-backlinks'],
    ['today-launchpad'], ['track', event.data.track_id],
  ])),
  'overlay.set': plan((event, context) => {
    const keys: QueryKey[] = [];
    if (event.data.entity_kind === 'track' || event.data.entity_kind === 'card') keys.push(['overlays', event.data.entity_kind]);
    if (event.data.entity_kind === 'track') keys.push(['track', event.data.entity_id]);
    if (event.data.entity_kind === 'card') {
      const trackId = context.findTrackOwningCard(event.data.entity_id);
      if (trackId !== null) keys.push(['track', trackId]);
    }
    return result(keys);
  }),
  'overlay.deleted': plan((event, context) => {
    const keys: QueryKey[] = [];
    if (event.data.entity_kind === 'track' || event.data.entity_kind === 'card') keys.push(['overlays', event.data.entity_kind]);
    if (event.data.entity_kind === 'track') keys.push(['track', event.data.entity_id]);
    if (event.data.entity_kind === 'card') {
      const trackId = context.findTrackOwningCard(event.data.entity_id);
      if (trackId !== null) keys.push(['track', trackId]);
    }
    return result(keys);
  }),
  'terminal.deleted': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'plugin.state': noop('No plugin list query exists.'),
  'plugin.tool.registered': noop('No plugin-tool catalog query exists.'),
  /* Workspace only — a hook writes no `tasks` row, and it fires per tool call. */
  'codex.hook': plan((event, context) => result([trackFiles(derivedTrackId(event.data, context))])),
  'claude.hook': plan((event, context) => result([trackFiles(derivedTrackId(event.data, context))])),
  'codex.worker_requested': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'terminal.worker_requested': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.completed': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.failed': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.file_publication_settled': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.candidate_verification_settled': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.git_delivery_settled': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.execution_settled': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'plan.updated': plan((event) => result(
    typeof event.data.agent_message === 'string' ? [['track-report', event.data.track_id]] : [],
  )),
  'task.dispatched': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.context_frozen': noop('Frozen task context has no query consumer.'),
  'task.context_advanced': noop('Context advancement has no query consumer.'),
  'workspace.leased': noop('Workspace leases have no query consumer.'),
  'workspace.released': noop('Workspace releases have no query consumer.'),
  'forge.pr.merged': noop('Forge merge rows have no query consumer.'),
  'review.round': noop('Review rounds have no query consumer.'),
  'ratify.requested': noop('Ratification requests have no query consumer.'),
  'ratify.resolved': noop('Ratification decisions have no query consumer.'),
  'proposal.submitted': noop('The proposal UI is withdrawn.'),
  'proposal.resolved': noop('The proposal UI is withdrawn.'),
  'forge.scan.completed': noop('Forge scan rows have no query consumer.'),
  'forge.pr.opened': noop('Opened PR rows have no query consumer.'),
  'forge.pr.diff.read': noop('Diff-read rows have no query consumer.'),
  'forge.pr.checks': noop('Forge check rows have no query consumer.'),
  'forge.issue.read': noop('Issue-read rows have no query consumer.'),
  'forge.issue.closed': noop('Issue-close rows have no query consumer.'),
  'worktree.provisioned': noop('Worktree rows have no query consumer.'),
  'worktree.committed': noop('Worktree rows have no query consumer.'),
  'worktree.removed': noop('Worktree rows have no query consumer.'),
  'task.gate_result': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  });
}

export function invalidationPlanFor(
  event: WireEvent,
  context: InvalidationContext = emptyContext,
): InvalidationPlan {
  const policy: InvalidationPolicy | undefined = policies()[event.ev];
  if (policy === undefined) return result([]);
  if (policy.type === 'noop') return result([]);
  return policy.plan(event, context);
}
