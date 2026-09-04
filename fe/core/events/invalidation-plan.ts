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
  | 'runtime.started' | 'runtime.status_changed' | 'runtime.superseded'
  | 'terminal.deleted' | 'codex.hook' | 'claude.hook'
  | 'codex.worker_requested' | 'terminal.worker_requested'
  | 'task.completed' | 'task.failed' | 'task.dispatched' | 'task.gate_result';

/**
 * Every kind that can change what a track's workspace looks like.
 *
 * NOT the same set as the task-verdict one below — see
 * `taskVerdictInvalidatingKinds`, which is this list minus the two hooks.
 */
export const TRACK_FILES_DERIVED_KINDS = Object.freeze([
  'runtime.started', 'runtime.status_changed', 'runtime.superseded',
  'terminal.deleted', 'codex.hook', 'claude.hook',
  'codex.worker_requested', 'terminal.worker_requested',
  'task.completed', 'task.failed', 'task.dispatched', 'task.gate_result',
] as const);

/**
 * The track conversation list (#1189 §5.5), keyed by the track it belongs to.
 *
 * The id is derivable: every event this key hangs off carries either a `track_id` or a
 * `card_id` an `InvalidationContext` can resolve, and the endpoint itself is
 * per-track (`GET /api/tracks/{track_id}/conversations`, §4.1). Invalidating
 * `['track-conversations']` wholesale on
 * every runtime tick would refetch the list of every track the user has open.
 *
 * The prefix is still what comes back when the track genuinely cannot be
 * resolved (a `runtime.*` event for a card no cached track owns). That is the
 * honest answer to "some track's list may have changed", and it costs nothing
 * when no wave-conversation query is mounted: invalidating a key with no
 * active observer only marks cache entries stale.
 */
function trackConversations(trackId: string | null): QueryKey {
  return trackId === null ? ['track-conversations'] : ['track-conversations', trackId];
}

/**
 * The conversation list's `state` is read from `worker_sessions.state`. The
 * three `runtime.*`
 * kinds are what actually move that column, `runtime.started` being the
 * `null → starting` transition that turns the dot on at all, so a session
 * could start, change status and be superseded with the list still showing
 * whatever it had. Adding the track list without fixing the area one would have
 * left the older list with the same stale `state`.
 *
 * `track.lifecycle_changed` is deliberately NOT a caller. It does not write
 * `worker_sessions.state`; a track reaching a terminal lifecycle ends its
 * sessions by superseding their runtimes, which emits `runtime.superseded` —
 * already here. A second trigger for one change buys a duplicate refetch of a
 * wholesale list and makes it impossible to prove either one does the work.
 *
 * `card.deleted` is knowingly absent from the list and is not this slice's to
 * fix: nothing drops a deleted conversation's row today (#1140).
 *
 * The exact caller set is pinned from both sides in `invalidation-plan.test.ts`
 * against a list kept by hand there, so neither a missing nor an extra caller
 * can land silently.
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
 * The kinds that invalidate a track's task verdicts (`['track-report', …]`).
 *
 * A function, not a frozen const, only because `no-module-runtime-state` will
 * not accept a module-level binding whose initializer is a call.
 *
 * Derived from `TRACK_FILES_DERIVED_KINDS` rather than typed out again, minus
 * the two hooks — and the difference is the whole point. `codex.hook` fires per
 * CLI hook, roughly twice per tool call per running worker, and it writes no
 * `tasks` row: a hook is the agent telling the kernel what it just did, not the
 * scheduler moving a task. It does change the workspace, so it keeps its
 * `track-files` key.
 *
 * Three non-derived events deliberately join the list. `track.updated` carries
 * task budget, planner ceiling, and root tree budget changes. `track.deleted`
 * changes the surviving tree's membership and therefore its effective budget.
 * Both use the broad report prefix because their effect can reach child or
 * sibling tracks. A `plan.updated` carrying `agent_message` is the standalone
 * pending-task cancellation path and can use its explicit track id; projection
 * events omit that field and ride with one of the broader events above.
 *
 * Invalidation is not free here. `['track-report', …]` resolves to a live query
 * on `GET /api/tracks/{id}/report`, which loads the track's CRDT, projects the
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
    ...TRACK_FILES_DERIVED_KINDS.filter((kind) => kind !== 'codex.hook' && kind !== 'claude.hook'),
    'track.report_edited', 'track.updated', 'track.deleted', 'plan.updated',
  ];
}

/**
 * The three `runtime.*` kinds share one plan, because they are one statement:
 * this card's session moved. They were already identical; they are now
 * identical *and* carrying the conversation lists, so a third copy that drifted
 * would silently drop a list from one transition only.
 */
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
  'area.deleted': plan(() => result([['areas'], ['overlays', 'track']])),
  /* `track.updated` carries the task-budget / planner-ceiling PATCH event.
     The report key is deliberately broad: a root track's tree budget changes
     the admission diagnosis of its child tracks, but the event carries only
     the updated root id. React Query refetches only active observers. */
  'track.updated': plan((event) => result([
    ['tracks', 'area', event.data.area_id], ['track', event.data.id],
    ['track-files', event.data.id], ['tracks-range'], ['track-report'],
  ])),
  'track.deleted': plan((event) => result(
    [
      ['tracks', 'area', event.data.area_id], ['overlays', 'track'], ['tracks-range'],
      ['track-report'],
    ],
    [['track', event.data.id]],
  )),
  'track.lifecycle_changed': plan((event) => result([
    ['tracks', 'area', event.data.area_id], ['track', event.data.id],
    ['track-files', event.data.id], ['tracks-range'],
  ])),
  'card.added': plan((event) => result([
    ['track', event.data.track_id], ['track-files', event.data.track_id],
    ...conversationLists(event.data.track_id),
  ])),
  'card.updated': plan((event) => result([
    ['track', event.data.track_id], ['track-files', event.data.track_id],
    ...conversationLists(event.data.track_id),
  ])),
  /* No conversation key, and not an oversight — see the note on
     `conversationLists`. Dropping the deleted row is #1140's. */
  'card.deleted': plan((event) => result([['track', event.data.track_id], ['track-files', event.data.track_id]])),
  'runtime.started': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'runtime.status_changed': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'runtime.superseded': plan((event, context) => runtimePlan(event.data.card_id, context)),
  'harness.item.added': plan((event) => result([['harness-items', event.data.card_id]])),
  'harness.phase.changed': plan((event) => result([
    ['planner-run', event.data.card_id], ...conversationLists(event.data.track_id),
  ])),
  'harness.transcript.cleared': plan((event) => result([
    ['harness-items', event.data.card_id], ['planner-run', event.data.card_id],
  ])),
  'harness.user_message.enqueued': plan((event) => result([
    ['harness-items', event.data.card_id], ['planner-run', event.data.card_id],
    ...conversationLists(event.data.track_id),
  ])),
  /*
   * #1253 §6 — four keys, and the last two are why the Today trigger visibly
   * does anything.
   *
   * `['today-launchpad']` carries `report_has_noninitial_content`, which is the
   * server-side predicate Today decides its empty state with: without it the
   * first summary an agent ever writes leaves the page reading "Nothing written
   * today yet." until a reload. `['track', id]` is the track detail the document
   * is read out of (`readTrackReport` locates the report card by kind), so
   * without it the region keeps drawing the previous body.
   *
   * **Nothing generated protects either line.** `PolicyMap` is exhaustive over
   * event *kinds*, not over query keys, so deleting one adds no missing kind
   * and no golden notices. The literal key list in
   * `invalidation-plan.contract.test.ts` is the only guard there is.
   */
  'track.report_edited': plan((event) => result([
    ['track-files', event.data.track_id], ['track-report', event.data.track_id], ['track-backlinks'],
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
  /* Workspace only — a hook writes no `tasks` row, and it fires per tool call.
     See `taskVerdictInvalidatingKinds` for what that key would have cost. */
  'codex.hook': plan((event, context) => result([trackFiles(derivedTrackId(event.data, context))])),
  'claude.hook': plan((event, context) => result([trackFiles(derivedTrackId(event.data, context))])),
  'codex.worker_requested': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'terminal.worker_requested': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.completed': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
  'task.failed': plan((event, context) => result(trackFilesDerived(derivedTrackId(event.data, context)))),
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
