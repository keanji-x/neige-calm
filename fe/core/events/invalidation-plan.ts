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

/**
 * Every kind that can change what a wave's workspace looks like.
 *
 * NOT the same set as the task-verdict one below — see
 * `taskVerdictInvalidatingKinds`, which is this list minus the two hooks.
 */
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

/**
 * The wave conversation list (#1189 §5.5), keyed by the wave it belongs to.
 *
 * Unlike the cove list above, the id is NOT omitted, because here it *is*
 * derivable: every event this key hangs off carries either a `wave_id` or a
 * `card_id` an `InvalidationContext` can resolve, and the endpoint itself is
 * per-wave (`GET /api/waves/{wave_id}/conversations`, §4.1). The cove list's
 * prefix shape is a concession to a cove id that cannot be recovered, not a
 * house style to copy — invalidating `['wave-conversations']` wholesale on
 * every runtime tick would refetch the list of every wave the user has open.
 *
 * The prefix is still what comes back when the wave genuinely cannot be
 * resolved (a `runtime.*` event for a card no cached wave owns). That is the
 * honest answer to "some wave's list may have changed", and it costs nothing
 * when no wave-conversation query is mounted: invalidating a key with no
 * active observer only marks cache entries stale.
 */
function waveConversations(waveId: string | null): QueryKey {
  return waveId === null ? ['wave-conversations'] : ['wave-conversations', waveId];
}

/**
 * Both conversation lists, which are invalidated together and never apart.
 *
 * They are one key set because they were one defect (#1189 §5.5). A row's
 * `state` in either list is read from `worker_sessions.state` — the cove query
 * in `cove_conversations.rs`, the wave one mirroring it — and until this slice
 * both lists hung off card and harness events only. The three `runtime.*`
 * kinds are what actually move that column, `runtime.started` being the
 * `null → starting` transition that turns the dot on at all, so a session
 * could start, change status and be superseded with the list still showing
 * whatever it had. Adding the wave list without fixing the cove one would have
 * left the older list with the same stale `state`.
 *
 * `wave.lifecycle_changed` is deliberately NOT a caller. It does not write
 * `worker_sessions.state`; a wave reaching a terminal lifecycle ends its
 * sessions by superseding their runtimes, which emits `runtime.superseded` —
 * already here. A second trigger for one change buys a duplicate refetch of a
 * wholesale list and makes it impossible to prove either one does the work.
 *
 * `card.deleted` is knowingly absent from both lists and is not this slice's to
 * fix: nothing drops a deleted conversation's row today (#1140).
 *
 * The exact caller set is pinned from both sides in `invalidation-plan.test.ts`
 * against a list kept by hand there, so neither a missing nor an extra caller
 * can land silently.
 */
function conversationLists(waveId: string | null): readonly QueryKey[] {
  return [COVE_CONVERSATIONS, waveConversations(waveId)];
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

/**
 * The kinds that invalidate a wave's task verdicts (`['wave-report', …]`).
 *
 * A function, not a frozen const, only because `no-module-runtime-state` will
 * not accept a module-level binding whose initializer is a call.
 *
 * Derived from `WAVE_FILES_DERIVED_KINDS` rather than typed out again, minus
 * the two hooks — and the difference is the whole point. `codex.hook` fires per
 * CLI hook, roughly twice per tool call per running worker, and it writes no
 * `tasks` row: a hook is the agent telling the kernel what it just did, not the
 * scheduler moving a task. It does change the workspace, so it keeps its
 * `wave-files` key.
 *
 * Invalidation is not free here. `['wave-report', …]` resolves to a live query
 * on `GET /api/waves/{id}/report`, which loads the wave's CRDT, projects the
 * whole document and runs `task_diagnostics` — a predicate that issues a
 * data-dependent lookup per reference per declaration (see its comment in
 * `read.rs`). The frontend then throws away everything but `taskDiagnostics`.
 * Paying that twice per tool call, per worker, for a value that provably cannot
 * have changed, is the cost this exclusion removes.
 *
 * `staleTime` is deliberately NOT the fix and is not set on that query:
 * `invalidateQueries` refetches an active observer whatever its staleTime, so a
 * stale window would have suppressed nothing here.
 */
export function taskVerdictInvalidatingKinds(): readonly EventKind[] {
  return [
    ...WAVE_FILES_DERIVED_KINDS.filter((kind) => kind !== 'codex.hook' && kind !== 'claude.hook'),
    'wave.report_edited',
  ];
}

/**
 * The three `runtime.*` kinds share one plan, because they are one statement:
 * this card's session moved. They were already identical; they are now
 * identical *and* carrying the conversation lists, so a third copy that drifted
 * would silently drop a list from one transition only.
 */
function runtimePlan(cardId: string, context: InvalidationContext): InvalidationPlan {
  const waveId = context.findWaveOwningCard(cardId);
  return result([
    ...(waveId === null ? [] : [['wave', waveId]]),
    ['overlays', 'card'],
    ...waveFilesDerived(waveId),
    ...conversationLists(waveId),
  ]);
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
    ['wave', event.data.wave_id], ['wave-files', event.data.wave_id],
    ...conversationLists(event.data.wave_id),
  ])),
  'card.updated': plan((event) => result([
    ['wave', event.data.wave_id], ['wave-files', event.data.wave_id],
    ...conversationLists(event.data.wave_id),
  ])),
  /* No conversation key, on either list, and not an oversight — see the note on
     `conversationLists`. Dropping the deleted row is #1140's. */
  'card.deleted': plan((event) => result([['wave', event.data.wave_id], ['wave-files', event.data.wave_id]])),
  'runtime.started': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'runtime.status_changed': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'runtime.superseded': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'harness.item.added': plan((event) => result([['harness-items', event.data.card_id]])),
  'harness.phase.changed': plan((event) => result([
    ['spec-run', event.data.card_id], ...conversationLists(event.data.wave_id),
  ])),
  'harness.transcript.cleared': plan((event) => result([
    ['harness-items', event.data.card_id], ['spec-run', event.data.card_id],
  ])),
  'harness.user_message.enqueued': plan((event) => result([
    ['harness-items', event.data.card_id], ['spec-run', event.data.card_id],
    ...conversationLists(event.data.wave_id),
  ])),
  /*
   * #1253 §6 — four keys, and the last two are why the Today trigger visibly
   * does anything.
   *
   * `['today-launchpad']` carries `report_has_noninitial_content`, which is the
   * server-side predicate Today decides its empty state with: without it the
   * first summary an agent ever writes leaves the page reading "Nothing written
   * today yet." until a reload. `['wave', id]` is the wave detail the document
   * is read out of (`readWaveReport` locates the report card by kind), so
   * without it the region keeps drawing the previous body.
   *
   * **Nothing generated protects either line.** `PolicyMap` is exhaustive over
   * event *kinds*, not over query keys, so deleting one adds no missing kind
   * and no golden notices. The literal key list in
   * `invalidation-plan.contract.test.ts` is the only guard there is.
   */
  'wave.report_edited': plan((event) => result([
    ['wave-files', event.data.wave_id], ['wave-report', event.data.wave_id], ['wave-backlinks'],
    ['today-launchpad'], ['wave', event.data.wave_id],
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
  /* Workspace only — a hook writes no `tasks` row, and it fires per tool call.
     See `taskVerdictInvalidatingKinds` for what that key would have cost. */
  'codex.hook': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
  'claude.hook': plan((event, context) => result([waveFiles(derivedWaveId(event.data, context))])),
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
