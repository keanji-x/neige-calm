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
  'card.added': plan((event) => result([['wave', event.data.wave_id], ['wave-files', event.data.wave_id]])),
  'card.updated': plan((event) => result([['wave', event.data.wave_id], ['wave-files', event.data.wave_id]])),
  'card.deleted': plan((event) => result([['wave', event.data.wave_id], ['wave-files', event.data.wave_id]])),
  'runtime.started': plan((event, context) => {
    const waveId = context.findWaveOwningCard(event.data.card_id);
    return result([...(waveId === null ? [] : [['wave', waveId]]), ['overlays', 'card'], waveFiles(waveId)]);
  }),
  'runtime.status_changed': plan((event, context) => {
    const waveId = context.findWaveOwningCard(event.data.card_id);
    return result([...(waveId === null ? [] : [['wave', waveId]]), ['overlays', 'card'], waveFiles(waveId)]);
  }),
  'runtime.superseded': plan((event, context) => {
    const waveId = context.findWaveOwningCard(event.data.card_id);
    return result([...(waveId === null ? [] : [['wave', waveId]]), ['overlays', 'card'], waveFiles(waveId)]);
  }),
  'harness.item.added': noop('Card-topic report consumers handle harness items directly.'),
  'harness.phase.changed': noop('Card-topic report consumers handle phase changes directly.'),
  'harness.transcript.cleared': noop('Report consumers reset transcript state directly.'),
  'harness.user_message.enqueued': noop('Report consumers observe queued messages directly.'),
  'wave.report_edited': plan((event) => result([['wave-files', event.data.wave_id], ['wave-backlinks']])),
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
  'terminal.deleted': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'plugin.state': noop('No plugin list query exists.'),
  'plugin.tool.registered': noop('No plugin-tool catalog query exists.'),
  'workflow.registered': noop('No workflow catalog query exists.'),
  'codex.hook': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'claude.hook': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'codex.worker_requested': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'terminal.worker_requested': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'task.completed': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'task.failed': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'plan.updated': noop('No task-plan query exists.'),
  'task.dispatched': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
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
    'task.gate_result': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  });
}

export function invalidationPlanFor(
  event: WireEvent,
  context: InvalidationContext = emptyContext,
): InvalidationPlan {
  const policy: InvalidationPolicy = policies()[event.ev];
  if (policy.type === 'noop') return result([]);
  return policy.plan(event, context);
}
