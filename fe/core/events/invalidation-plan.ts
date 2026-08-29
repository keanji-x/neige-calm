import type { Cove, WireEvent } from '../api/schemas.js';

export type EventKind = WireEvent['ev'];
export type EventOf<K extends EventKind> = Extract<WireEvent, { ev: K }>;
export type QueryKey = readonly unknown[];

export type CacheWrite = Readonly<{ key: QueryKey; mode: 'replace-existing-cove'; value: Cove }>;
export type InvalidationPlan = Readonly<{
  invalidate: readonly QueryKey[];
  remove: readonly QueryKey[];
  writeThrough: readonly CacheWrite[];
}>;
export type InvalidationContext = Readonly<{
  findWaveOwningCard(cardId: string): string | null;
}>;

type PlannedPolicy<K extends EventKind = EventKind> = Readonly<{
  type: 'plan';
  plan(event: EventOf<K>, context: InvalidationContext): InvalidationPlan;
}>;
type NoopPolicy = Readonly<{ type: 'noop'; reason: string }>;
export type InvalidationPolicy<K extends EventKind = EventKind> = PlannedPolicy<K> | NoopPolicy;
type PolicyMap = { readonly [K in EventKind]: InvalidationPolicy<K> };

const emptyContext: InvalidationContext = Object.freeze({ findWaveOwningCard: () => null });

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

function waveFiles(waveId: string | null): QueryKey {
  return waveId === null ? ['wave-files'] : ['wave-files', waveId];
}

function waveFilesDerived(waveId: string | null): readonly QueryKey[] {
  return [waveFiles(waveId), waveId === null ? ['wave-report'] : ['wave-report', waveId]];
}

export type WaveFilesDerivedKind =
  | 'runtime.started' | 'runtime.status_changed' | 'runtime.superseded'
  | 'terminal.deleted' | 'codex.hook' | 'claude.hook'
  | 'codex.worker_requested' | 'terminal.worker_requested'
  | 'task.completed' | 'task.failed' | 'task.dispatched' | 'task.gate_result';

export const WAVE_FILES_DERIVED_KINDS = Object.freeze([
  'runtime.started', 'runtime.status_changed', 'runtime.superseded',
  'terminal.deleted', 'codex.hook', 'claude.hook',
  'codex.worker_requested', 'terminal.worker_requested',
  'task.completed', 'task.failed', 'task.dispatched', 'task.gate_result',
] as const);

/**
 * The cove conversation list, invalidated without a cove id (#1098 §5.5).
 *
 * The id is not omitted for convenience — it is not derivable here.
 * `InvalidationContext` can resolve a card to its wave and nothing further, and
 * a cove chat wave's detail is never fetched (the wave is hidden), so there is
 * no cached row to read a cove id out of either. A prefix key is the honest
 * shape for "some cove's list may have changed"; it also costs nothing when no
 * cove route is mounted, because an invalidated key with no active query only
 * marks cache entries stale.
 *
 * Deliberately *not* invalidated from `harness.item.added` — it writes only
 * `harness_items`, never `worker_sessions`, so it cannot change a row of this
 * list, and it is the highest-frequency event the kernel emits. Nor from
 * `harness.transcript.cleared`: a reset always emits `harness.phase.changed`
 * too, so a second trigger would only make the first impossible to disprove.
 */
export const COVE_CONVERSATIONS: QueryKey = Object.freeze(['cove-conversations']);

function derivedWaveId(data: unknown, context: InvalidationContext): string | null {
  if (typeof data !== 'object' || data === null) return null;
  const value = data as { wave_id?: unknown; card_id?: unknown };
  if (typeof value.wave_id === 'string' && value.wave_id.length > 0) return value.wave_id;
  if (typeof value.card_id === 'string' && value.card_id.length > 0) {
    return context.findWaveOwningCard(value.card_id);
  }
  return null;
}

export function defineInvalidationPolicies<T extends PolicyMap>(value: T): T {
  return value;
}

function policies(): PolicyMap {
  return defineInvalidationPolicies({
  'cove.updated': plan((event) => result(
    [['coves']],
    [],
    [{ key: ['coves'], mode: 'replace-existing-cove', value: event.data }],
  )),
  'cove.deleted': plan(() => result([['coves'], ['overlays', 'wave']])),
  'wave.updated': plan((event) => result([
    ['waves', 'cove', event.data.cove_id], ['wave', event.data.id],
    ['wave-files', event.data.id], ['waves-range'],
  ])),
  'wave.deleted': plan((event) => result(
    [['waves', 'cove', event.data.cove_id], ['overlays', 'wave'], ['waves-range']],
    [['wave', event.data.id]],
  )),
  'wave.lifecycle_changed': plan((event) => result([
    ['waves', 'cove', event.data.cove_id], ['wave', event.data.id],
    ['wave-files', event.data.id], ['waves-range'],
  ])),
  'card.added': plan((event) => result([
    ['wave', event.data.wave_id], ['wave-files', event.data.wave_id], COVE_CONVERSATIONS,
  ])),
  'card.updated': plan((event) => result([
    ['wave', event.data.wave_id], ['wave-files', event.data.wave_id], COVE_CONVERSATIONS,
  ])),
  'card.deleted': plan((event) => result([['wave', event.data.wave_id], ['wave-files', event.data.wave_id]])),
  'runtime.started': plan((event, context) => {
    const waveId = context.findWaveOwningCard(event.data.card_id);
    return result([...(waveId === null ? [] : [['wave', waveId]]), ['overlays', 'card'], ...waveFilesDerived(waveId)]);
  }),
  'runtime.status_changed': plan((event, context) => {
    const waveId = context.findWaveOwningCard(event.data.card_id);
    return result([...(waveId === null ? [] : [['wave', waveId]]), ['overlays', 'card'], ...waveFilesDerived(waveId)]);
  }),
  'runtime.superseded': plan((event, context) => {
    const waveId = context.findWaveOwningCard(event.data.card_id);
    return result([...(waveId === null ? [] : [['wave', waveId]]), ['overlays', 'card'], ...waveFilesDerived(waveId)]);
  }),
  'harness.item.added': plan((event) => result([['harness-items', event.data.card_id]])),
  'harness.phase.changed': plan((event) => result([
    ['spec-run', event.data.card_id], COVE_CONVERSATIONS,
  ])),
  'harness.transcript.cleared': plan((event) => result([
    ['harness-items', event.data.card_id], ['spec-run', event.data.card_id],
  ])),
  'harness.user_message.enqueued': plan((event) => result([
    ['harness-items', event.data.card_id], ['spec-run', event.data.card_id], COVE_CONVERSATIONS,
  ])),
  'wave.report_edited': plan((event) => result([
    ['wave-files', event.data.wave_id], ['wave-report', event.data.wave_id], ['wave-backlinks'],
  ])),
  'overlay.set': plan((event, context) => {
    const keys: QueryKey[] = [];
    if (event.data.entity_kind === 'wave' || event.data.entity_kind === 'card') keys.push(['overlays', event.data.entity_kind]);
    if (event.data.entity_kind === 'wave') keys.push(['wave', event.data.entity_id]);
    if (event.data.entity_kind === 'card') {
      const waveId = context.findWaveOwningCard(event.data.entity_id);
      if (waveId !== null) keys.push(['wave', waveId]);
    }
    return result(keys);
  }),
  'overlay.deleted': plan((event, context) => {
    const keys: QueryKey[] = [];
    if (event.data.entity_kind === 'wave' || event.data.entity_kind === 'card') keys.push(['overlays', event.data.entity_kind]);
    if (event.data.entity_kind === 'wave') keys.push(['wave', event.data.entity_id]);
    if (event.data.entity_kind === 'card') {
      const waveId = context.findWaveOwningCard(event.data.entity_id);
      if (waveId !== null) keys.push(['wave', waveId]);
    }
    return result(keys);
  }),
  'terminal.deleted': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'plugin.state': noop('No plugin list query exists.'),
  'plugin.tool.registered': noop('No plugin-tool catalog query exists.'),
  'codex.hook': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'claude.hook': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'codex.worker_requested': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'terminal.worker_requested': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'task.completed': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'task.failed': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
  'plan.updated': noop('No task-plan query exists.'),
  'task.dispatched': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
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
  'task.gate_result': plan((event, context) => result(waveFilesDerived(derivedWaveId(event.data, context)))),
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
