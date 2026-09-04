// Query and mutation wiring shared by app/router, app/shell and every feature
// slice.
//
// It lives under app/providers rather than under any one consumer because the
// router renders the shell and both need the same area/track reads: a queries
// module owned by either side would close a cycle that the `no-circular`
// dependency-cruiser rule rejects. Feature slices receive the resulting data
// and callbacks as props — `features/**` must not import `app/**`.

import {
  useMutation, useQueries, useQuery, useQueryClient, type QueryClient,
} from '@tanstack/react-query';
import { z } from 'zod';

import { performApiRequest } from '../../../../core/api/client.ts';
import type { ApiFailure, ApiOperation, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  asFolderConflict, areaListOperation, createAreaOperation, deleteAreaOperation,
  newestArea, sortedAreas, toArea, updateAreaOperation, visibleAreas,
  type Area, type AreaPatchBody, type FolderConflict, type NewAreaBody,
} from '../../../../core/domain/area.ts';
import {
  deriveReportTasks, hasLiveTaskRun, trackBacklinksOperation, trackTaskVerdictsOperation,
  type ReportBlock, type TaskVerdict, type TrackBacklinks,
} from '../../../../core/domain/report.ts';
import {
  patchPluginConfigOperation, pluginDetailOperation, pluginsOperation, reloadPluginOperation,
  setPluginEnabledOperation,
  type PluginApiFailure, type PluginConfigApplyResult, type PluginConfigSaveResult,
  type PluginConfigValue, type PluginDetail, type PluginListItem, type PluginRestartFacts,
} from '../../../../core/domain/plugins.ts';
import {
  putSettingsOperation, settingsOperation, type SettingsBag, type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import {
  todayLaunchpadOperation, todaySummaryOperation,
  type TodayLaunchpadWire, type TodaySummaryWire,
} from '../../../../core/domain/today.ts';
import {
  createCardOperation, createCodexCardOperation, createTerminalCardOperation, createTrackOperation,
  createTrackRecipeOperation, deleteCardOperation, deleteTrackOperation, deleteTrackRecipeOperation,
  overlaysByKindOperation, toTrack, updateTrackOperation, updateTrackRecipeOperation,
  trackActivityFrom, trackDetailOperation, trackRecipesOperation, trackTemplatesOperation,
  tracksInAreaOperation,
  type CardWire, type NewCardBody, type NewCodexCardBody, type NewTerminalCardBody, type NewTrackBody,
  type OverlayWire, type Track, type TrackDetailWire, type TrackPatchBody, type TrackRecipe,
  type TrackTemplate,
} from '../../../../core/domain/track.ts';
import {
  HARNESS_ITEMS_PAGE_LIMIT, harnessItemsOperation, interruptPlannerOperation, sendPlannerInputOperation,
  plannerRunOperation, createTrackConversationOperation, trackConversationsOperation,
  type Conversation,
} from '../../../../core/domain/conversation.ts';
import { useState } from '../../ui/state/public.ts';
import type { ServerVersionInfo } from './public.tsx';
import type { HarnessItem } from '../../../../core/api/generated/wire.ts';

export class ApiError extends Error {
  readonly failure: ApiFailure;

  constructor(failure: ApiFailure) {
    super(failure.message);
    this.name = 'ApiError';
    this.failure = failure;
  }
}

/**
 * The structured folder clash inside a rejected mutation, or `null`.
 *
 * Lives beside `ApiError` because unwrapping it is the only step that needs to
 * know this class exists; the decode and the wording are `core/domain/area.ts`.
 * `'body' in failure` is the narrowing: transport and decode failures never
 * carry one, and reading `.body` off the union without it does not compile.
 */
export function folderConflictOf(error: unknown): FolderConflict | null {
  if (!(error instanceof ApiError)) return null;
  return 'body' in error.failure ? asFolderConflict(error.failure.body) : null;
}

/** TanStack Query wants a rejected promise; core reports failures as data. */
export async function runOperation<T>(
  transport: ApiTransportPort,
  operation: ApiOperation<T>,
  unauthorized: UnauthorizedChannel | undefined,
): Promise<T> {
  const result = await performApiRequest(transport, operation, unauthorized);
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

// Key shapes match the legacy app so a cache dump reads the same in both.
export const queryKeys = Object.freeze({
  serverVersion: () => ['server-version'] as const,
  areas: () => ['areas'] as const,
  areaFolders: (areaId: string) => ['area-folders', areaId] as const,
  tracksInArea: (areaId: string) => ['tracks', areaId] as const,
  trackDetail: (trackId: string) => ['track', trackId] as const,
  trackBacklinks: (trackId: string) => ['track-backlinks', trackId] as const,
  /* Exactly the shape `core/events/invalidation-plan` already plans for
     `track.report_edited` and every `task.*` event, so naming it this way is
     what makes the TASKS panel live — see `trackReportPrefix` for the half of
     that the plan cannot key by track. */
  trackReport: (trackId: string) => ['track-report', trackId] as const,
  /**
   * The prefix `trackReport` extends, for the events the plan cannot key by
   * track.
   *
   * `task.dispatched` / `task.completed` / `task.failed` / `task.gate_result`
   * carry no `track_id` and no `card_id` **field**, so `derivedTrackId` — which
   * reads named fields and nothing else — returns null and the plan emits the
   * bare key. The track id is not absent from those events: `idempotency_key`
   * *is* the task id, and a task id is `"{track_id}:{key}"`
   * (`task_projection.rs`'s `format!("{track_id}:{}", declaration.key)`, echoed
   * by all four kinds in `calm-types/src/event.rs`). The plan deliberately does
   * not take it apart — `TrackId` is an opaque newtype with no format contract,
   * so parsing an id in the pure planning layer would be a guess dressed as a
   * fact, and a wrong split yields a key matching no cached query, i.e. a panel
   * that silently stops refreshing.
   *
   * Dropping the bare key instead would leave the four events that matter most
   * to this panel as the four that do not refresh it. A prefix invalidation
   * reaches whichever track report is cached — at most the open track's — and
   * costs nothing when none is.
   */
  trackReportPrefix: () => ['track-report'] as const,
  overlaysByKind: (entityKind: 'track' | 'card') => ['overlays', entityKind] as const,
  settings: () => ['settings'] as const,
  /* Settings › Plugins. Not reached by any event policy — see
     `pluginsQueryOptions` for why, and for what stands in for one. */
  plugins: () => ['plugins'] as const,
  /* #1284 S4 — one plugin's detail, read only by its configuration pane. Keyed
     by id and not folded into the list: the list carries no manifest by design,
     and a `config_schema` per row would make opening Settings fetch every
     plugin's schema to render nothing with them. */
  pluginDetail: (id: string) => ['plugin-detail', id] as const,
  /* #1209 — the New track picker's list. Not invalidated by any event: the
     kernel's template keys are compile-time constants and the only thing that
     can move under them is a plugin starting or stopping, which changes an
     `input_schema` the new-track page reads. */
  trackTemplates: () => ['track-templates'] as const,
  /* #1292 — the user's own recipes. Not invalidated by any event either, for a
     different reason than `trackTemplates`: recipe writes emit no `Event` at
     all (`routes/track_recipes.rs` — minting a variant would buy only "the
     other window refreshes by itself", and the `revision` CAS already stops
     that window from clobbering). The mutations below invalidate this key
     directly, which is what keeps the list and the picker current in the
     window that did the writing. */
  trackRecipes: () => ['track-recipes'] as const,
  /* #1253 §5.1 — the Today launchpad resolve. One entry, not keyed by track:
     the kernel's partial unique index makes `purpose = 'launchpad'` a
     singleton, and the id is what this query is fetching.

     PR2 put it on `track.report_edited`'s invalidation list, together with
     `['track', id]`. Both are needed and neither is generated: the first
     carries the empty-state predicate, the second carries the document, and
     `PolicyMap` is exhaustive over event kinds rather than over query keys, so
     no golden would have reported their absence. */
  todayLaunchpad: () => ['today-launchpad'] as const,
  harnessItems: (cardId: string) => ['harness-items', cardId] as const,
  plannerRun: (cardId: string) => ['planner-run', cardId] as const,
  /**
   * One track's conversation list (#1189 §4.1), keyed by its track.
   *
   * `GET /api/tracks/{track_id}/conversations` is per-track, and unlike the area
   * list the id *is* derivable from the events: the plan emits
   * `['track-conversations', trackId]` whenever `derivedTrackId` resolves one.
   *
   * **The query that registers this key lands in S5; the mapping is here
   * first, and that is deliberate, not dead code.** Invalidating a key with no
   * mounted query is a no-op in TanStack Query — it marks nothing and refetches
   * nothing — so the adapter may know a key before a query claims it. The
   * reverse order is the one that breaks: a query that mounts against a key no
   * adapter arm maps is silently never invalidated.
   */
  trackConversations: (trackId: string) => ['track-conversations', trackId] as const,
  /**
   * The prefix `trackConversations` extends, for the events that cannot name a
   * track.
   *
   * The three `runtime.*` kinds carry only a `card_id`, and
   * `findTrackOwningCard` answers from the cached track details — so a card in a
   * track nobody has open resolves to null and the plan emits the bare key. That
   * is the honest "some track's list may have changed", and dropping it here
   * would leave a genuinely open list stale for precisely the transitions that
   * move a row's `state`.
   *
   * It is a fallback and not the house shape: invalidating this prefix on every
   * runtime tick would refetch the list of every track the user has open, which
   * is why the plan keys by track whenever it can.
   */
  trackConversationsPrefix: () => ['track-conversations'] as const,
});

export function harnessItemsQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.harnessItems(cardId),
    queryFn: ({ pageParam }: { pageParam: number }) => runOperation(
      transport, harnessItemsOperation(cardId, pageParam, 'desc'), unauthorized,
    ),
    initialPageParam: 0,
    getNextPageParam: (page: HarnessItem[]) =>
      page.length === HARNESS_ITEMS_PAGE_LIMIT ? page[0]?.id : undefined,
  };
}

export function plannerRunQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.plannerRun(cardId),
    queryFn: () => runOperation(transport, plannerRunOperation(cardId), unauthorized),
  };
}

export function usePlannerMutations(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  const client = useQueryClient();
  const refresh = () => Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }),
    client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }),
  ]).then(() => undefined);
  const refreshAfter = async <T,>(result: T): Promise<T> => {
    await refresh();
    return result;
  };
  return {
    send: (text: string) => runOperation(transport, sendPlannerInputOperation(cardId, text), unauthorized).then(refreshAfter),
    interrupt: () => runOperation(transport, interruptPlannerOperation(cardId), unauthorized).then(refreshAfter),
    /* No `reset` — see the note where `resetPlannerOperation` used to be in
       `core/domain/conversation.ts`. The endpoint is still served; nothing in
       the browser calls it. */
  };
}

export type ConversationMutations = Readonly<{
  /**
   * Mint a conversation and deliver its first message.
   *
   * The key is supplied by the caller because it identifies the *draft*, not
   * the attempt: pressing send again after a timeout must reuse it, or the
   * retry mints a second conversation.
   */
  create: (text: string, idempotencyKey: string) => Promise<Conversation>;
  /** Re-read the list and hand back what it now holds. */
  refresh: () => Promise<Conversation[]>;
}>;

/**
 * One track's assistant conversations (#1189 §4.1).
 *
 * The key it registers is `queryKeys.trackConversations(trackId)`, which the
 * event bridge has mapped since S4 — so the list goes live the moment this
 * query mounts, with no second refresh path of its own.
 */
export function trackConversationsQueryOptions(
  transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.trackConversations(trackId),
    queryFn: (): Promise<Conversation[]> =>
      runOperation(transport, trackConversationsOperation(trackId), unauthorized),
  };
}

/**
 * Creates and refreshes one Track's assistant-conversation list.
 */
export function useTrackConversationMutations(
  transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel,
): ConversationMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: ({ text, idempotencyKey }: { text: string; idempotencyKey: string }) =>
      runOperation(transport, createTrackConversationOperation(trackId, text, idempotencyKey), unauthorized),
    onSuccess: (row) => {
      /* Written through as well as invalidated: the drawer switches to this row
         in the same tick and a list that does not hold it yet renders with no
         active row. */
      client.setQueryData<Conversation[]>(queryKeys.trackConversations(trackId), (current) => {
        const rows = current ?? [];
        return rows.some((candidate) => candidate.id === row.id)
          ? rows.map((candidate) => candidate.id === row.id ? row : candidate)
          : [...rows, row];
      });
      void client.invalidateQueries({ queryKey: queryKeys.trackConversations(trackId) });
      /* The card is new on the track, so the track's own detail — which is where
         the CARDS panel, the grid and the Today open-request all read cards
         from — is now one card short of the truth. */
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
    },
  });
  return {
    create: (text, idempotencyKey) => create.mutateAsync({ text, idempotencyKey }),
    refresh: () => client.fetchQuery({
      ...trackConversationsQueryOptions(transport, trackId, unauthorized),
      staleTime: 0,
    }),
  };
}

const serverVersionSchema = z.object({
  webCompatVersion: z.number(),
  minWebCompatVersion: z.number(),
  syncEventVersion: z.number(),
  dbInstanceId: z.string(),
});

export function serverVersionOperation(): ApiOperation<ServerVersionInfo> {
  return { method: 'GET', path: '/api/version', responseSchema: serverVersionSchema };
}

/** Sign-out is a server-side session kill; the caller reloads afterwards so
 *  every persisted cache restarts from an unauthenticated probe. */
export function logoutOperation(): ApiOperation<undefined> {
  return { method: 'POST', path: '/api/auth/logout', responseSchema: z.undefined() };
}

// ---------- reads ----------

export function areaListQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.areas(),
    queryFn: async (): Promise<Area[]> =>
      sortedAreas(visibleAreas((await runOperation(transport, areaListOperation(), unauthorized)).map(toArea))),
  };
}

export function trackOverlaysQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.overlaysByKind('track'),
    queryFn: (): Promise<OverlayWire[]> => runOperation(transport, overlaysByKindOperation('track'), unauthorized),
  };
}

export function tracksInAreaQueryOptions(transport: ApiTransportPort, areaId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.tracksInArea(areaId),
    queryFn: async (): Promise<Track[]> =>
      (await runOperation(transport, tracksInAreaOperation(areaId), unauthorized)).map((wire) => toTrack(wire)),
  };
}

export function trackDetailQueryOptions(transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackDetail(trackId),
    queryFn: (): Promise<TrackDetailWire> => runOperation(transport, trackDetailOperation(trackId), unauthorized),
  };
}

/**
 * Who cites this track (§8.3).
 *
 * Its own cache entry rather than a field on the detail: backlinks are written
 * by *other* tracks, so they go stale on edits this track never sees, and folding
 * them into the detail would tie the document's freshness to theirs.
 */
export function trackBacklinksQueryOptions(transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackBacklinks(trackId),
    queryFn: (): Promise<TrackBacklinks> => runOperation(transport, trackBacklinksOperation(trackId), unauthorized),
  };
}

/**
 * The track's task verdicts (§8.3) — what the kernel's task projection says
 * about each declared task: schedulable, status, and the worker card it was
 * dispatched onto.
 *
 * Its own cache entry rather than a field on the detail for the same reason
 * the backlinks are: the track detail is a card read, and these change on every
 * dispatch and every gate result without any card being written. It is also
 * the key the event plan already names, which is what makes the panel live.
 *
 * **Events alone do not keep it live, and the timer below is why.** The write
 * that stamps `worker_card_id` — `scheduler::mark_running` — emits nothing at
 * all, and it lands *after* `task.dispatched` and after every `runtime.*` a
 * worker adapter emits during its spawn. See `hasLiveTaskRun` for the full
 * accounting per worker kind, and for why this is a poll and not a new event.
 */
export function trackTaskVerdictsQueryOptions(
  transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel,
  /* The declarations this report actually has, because the timer below is
     about the *rows* the panel draws and a verdict is not a row — see
     `taskVerdictsRefetchInterval`. They arrive with the track detail, which is
     already in hand when this query is created. */
  blocks: readonly ReportBlock[] | null,
) {
  return {
    queryKey: queryKeys.trackReport(trackId),
    queryFn: (): Promise<TaskVerdict[]> => runOperation(transport, trackTaskVerdictsOperation(trackId), unauthorized),
    /*
     * See `taskVerdictsRefetchInterval` for when the timer runs at all.
     *
     * 3 seconds is priced off the endpoint, not chosen for roundness. Measured
     * on `GET /api/tracks/{id}/report` (debug build, in-memory SQLite, this
     * box): a 3-task / 2-prose report answers in p50 14.8 ms, a 24-task /
     * 12-prose one in p50 104 ms — the cost is dominated by the per-declaration
     * projection, as `taskVerdictInvalidatingKinds` describes. At the measured
     * worst case that is ~3.5% of one core, for one open track, only while
     * something is running; and it is O(1) in the number of workers, which is
     * the property the rejected fix (letting `codex.hook` invalidate this key)
     * did not have — hooks arrive about twice per tool call *per worker*.
     * Against a run measured in tens of seconds at least, 3 s of staleness on
     * "which card is this task on" is below the threshold at which a reader
     * would reach for the refresh button.
     */
    refetchInterval: taskVerdictsRefetchInterval(blocks),
  };
}

/** The live poll, once the read has landed at least once. */
const TASK_VERDICT_POLL_MS = 3000;
/** The recovery poll, while the read has never landed at all. Deliberately far
 *  slower than the live one: nothing is being *tracked* here, the only job is
 *  to notice that the endpoint came back. */
const TASK_VERDICT_RECOVERY_POLL_MS = 15_000;
/**
 * How many failed loads the recovery poll will sit through before giving up —
 * about a minute at the interval above, and then silence.
 *
 * Bounded because the two things that make this read fail are not alike. A
 * restarting or briefly unreachable server is transient and a minute of retries
 * clears it. But `GET /api/tracks/{id}/report` also fails *permanently* for a
 * track that no longer exists (`resolve_report_for_track` → `NotFound`) and for a
 * track whose `track-report` card is missing (the same function's invariant
 * violation → 500, `track_report.rs`), and neither of those is going to get
 * better by being asked again. An unconditional poll on "no data" would leave a
 * stale or deleted tab hitting a dead route every few seconds for as long as it
 * stayed open.
 */
const TASK_VERDICT_RECOVERY_ATTEMPTS = 4;

/**
 * The timer, over both of the states this query can be in.
 *
 * **Data in hand** — poll only while the track holds a task inside the eventless
 * window, and stop the moment none does (`false` is react-query's "no timer").
 * A settled track, a track that never dispatched anything, and a track whose page
 * is closed all cost exactly nothing. This branch also covers a *failed
 * refetch*: react-query keeps the last good data, so a live run stays live
 * across a blip and the timer that will re-fetch it keeps running.
 *
 * **No data at all** — the initial load failed and react-query exhausted its
 * retries, so `data` is `undefined`, `hasLiveTaskRun` is vacuously false, and
 * the query would sit there with no timer forever: nothing in the page ever
 * asks again, and a track that was mid-dispatch when the load failed would show
 * declaration words and no click-through until the tab was reloaded. A bounded
 * recovery poll converges without turning a permanently dead route into a
 * permanent load — see `TASK_VERDICT_RECOVERY_ATTEMPTS`. `errorUpdateCount` is
 * the counter to read rather than `failureCount`, which counts *retries within*
 * one attempt and is reset on success; `errorUpdateCount` counts errors
 * observed and so ticks once per exhausted attempt. It is `0` while the very
 * first fetch is still in flight, which is why that case takes no timer either:
 * a request is already on the wire.
 *
 * **Curried on the declarations** because the live branch is a question about
 * the panel's rows, not about the wire. The kernel emits a verdict for a
 * declaration that has been *deleted* (`blockId: ''`, naming no block here), so
 * an in-flight status can produce no row at all, and a timer keyed on the raw
 * verdicts would keep refetching every 3 s with nothing on screen that could
 * ever change. Joining first costs one pass over the declarations per interval
 * decision and makes "costs nothing outside that window" true as written.
 */
export function taskVerdictsRefetchInterval(blocks: readonly ReportBlock[] | null) {
  return (query: { state: { data?: TaskVerdict[]; errorUpdateCount: number } }): number | false => {
    const { data, errorUpdateCount } = query.state;
    if (data !== undefined) {
      return hasLiveTaskRun(deriveReportTasks(blocks, data)) ? TASK_VERDICT_POLL_MS : false;
    }
    return errorUpdateCount > 0 && errorUpdateCount <= TASK_VERDICT_RECOVERY_ATTEMPTS
      ? TASK_VERDICT_RECOVERY_POLL_MS
      : false;
  };
}

/**
 * #1253 §5.1 — the Today page load's resolve. A pure read: it never
 * bootstraps, so the first paint of Today does not depend on codex being up.
 *
 * **`null` is data; any failure is an error** (INV-TODAYDOC-002). "There is no
 * launchpad yet" arrives as a 200 with a null body and becomes the empty
 * state; a 500, a timeout or a schema mismatch reaches the reader as an error
 * box. There is no status-code special case left to get wrong — this used to
 * convert a 404 into `null` here, and the endpoint now says `null` itself.
 * Folding the two together — treating any failure as "nothing yet" — would
 * make an unreachable server look exactly like a fresh workspace, which is the
 * silent degradation this invariant exists to forbid.
 */
export function todayLaunchpadQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.todayLaunchpad(),
    queryFn: (): Promise<TodayLaunchpadWire | null> =>
      runOperation(transport, todayLaunchpadOperation(), unauthorized),
  };
}

/**
 * The Today trigger (#1253 D5), as one mutation.
 *
 * `failure` is handed back as an `ApiFailure` rather than as a sentence,
 * because the wording is `core/domain/today`'s job: `todaySummaryFailure`
 * matches on the machine-readable `code` there, where it is unit-testable and
 * away from React.
 *
 * **`onSuccess` deliberately does not touch the document's keys.** A 200 means
 * the message was enqueued, not that the agent has written anything — the write
 * lands later as a `track.report_edited` event, which the bridge turns into
 * `['today-launchpad']` and `['track', id]`. Refetching either here would fetch
 * the *old* report, and worse, it would hide a broken invalidation chain behind
 * a lucky refresh: the page would appear to update after a press even with both
 * keys missing from the policy, which is the exact defect §6 exists to prevent.
 * An earlier version invalidated `['track', id]` here while this comment claimed
 * it did not; the code was what moved.
 *
 * What IS true immediately is that the launchpad now carries a conversation, so
 * the conversation lists — and only those — are invalidated. That "and only
 * those" is asserted, not asserted-in-prose:
 * `today-summary-write.contract.test.tsx` drives this hook and asserts the
 * invalidated set by EQUALITY, so a third key added here turns it red too —
 * "and only those" is the claim, and a denylist of the two keys that would hurt
 * most would not have been it. Without that guard the document tests in
 * `app/router` could pass on a refetch from here instead of on the invalidation
 * policy they exist to pin.
 */
export type TodaySummaryMutation = Readonly<{
  write: () => void;
  pending: boolean;
  failure: ApiFailure | null;
}>;

export function useTodaySummaryMutation(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
): TodaySummaryMutation {
  const client = useQueryClient();
  const mutation = useMutation({
    mutationFn: (): Promise<TodaySummaryWire> =>
      runOperation(transport, todaySummaryOperation(), unauthorized),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: queryKeys.trackConversationsPrefix() });
    },
  });
  return {
    write: () => { mutation.mutate(); },
    pending: mutation.isPending,
    failure: mutation.error instanceof ApiError ? mutation.error.failure : null,
  };
}

export function settingsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.settings(),
    queryFn: (): Promise<SettingsBag> => runOperation(transport, settingsOperation(), unauthorized),
  };
}

/**
 * #1209 — templates for the new-track page.
 *
 * `retry: false` makes a failed roster visible instead of leaving either
 * consumer spinning. New Track can continue with an explicit No template when
 * no saved default must be resolved; a saved but unresolved Area default stays
 * blocked until the reader clears or replaces it. The Area editor likewise
 * keeps the missing value visible rather than silently changing the setting.
 */
export function trackTemplatesQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackTemplates(),
    queryFn: (): Promise<TrackTemplate[]> => runOperation(transport, trackTemplatesOperation(), unauthorized),
    retry: false,
  };
}

export type TrackTemplates = Readonly<{
  /** Never `undefined`; `loaded` and `error` distinguish empty, pending, and failed. */
  templates: TrackTemplate[];
  /** Visible roster failure. Whether it blocks depends on the saved selection. */
  error: string | null;
  /** `false` while the first read is still in flight — see `useTrackTemplates`. */
  loaded: boolean;
  refetch: () => void;
}>;

/**
 * Shared template roster for New Track and the Area editor. A hook and not raw
 * `useQuery` at either call site keeps loading/error semantics in one place and
 * lets shell contract tests mock the same provider boundary as the workspace
 * and mutation hooks.
 */
export function useTrackTemplates(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): TrackTemplates {
  const query = useQuery(trackTemplatesQueryOptions(transport, unauthorized));
  return {
    templates: query.data ?? [],
    error: query.isError ? 'Could not load templates.' : null,
    // `[]` must not be readable as "loaded and empty" — both template pills
    // render a different affordance for "no templates" than for "not yet".
    //
    // A **failed** read is not loaded either. The first cut wrote
    // `!query.isPending`, which is true once a read has errored — so a dead
    // server produced `loaded: true` with `templates: []`, i.e. a picker
    // claiming the server has no templates instead of reporting the failure.
    //
    // New Track uses this to fail closed for an unresolved saved default; the
    // Area editor uses it to label a saved id as pending vs unavailable.
    loaded: !query.isPending && !query.isError,
    refetch: () => { void query.refetch(); },
  };
}

/**
 * #1292 — the user's own recipes, for the New track picker and the manage
 * route.
 *
 * `retry: false` for the same reason as `trackTemplatesQueryOptions`: this
 * list feeds the app's only track-creation entry point, and a read that fails
 * must degrade that page to "built-ins only" rather than leave it spinning.
 */
export function trackRecipesQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackRecipes(),
    queryFn: (): Promise<TrackRecipe[]> => runOperation(transport, trackRecipesOperation(), unauthorized),
    retry: false,
  };
}

export type TrackRecipes = Readonly<{
  /** Never `undefined`: for the picker, pending and failed both read as "no
   *  recipes of mine", which is a fully working state. */
  recipes: TrackRecipe[];
  /** A notice, not a blocker. `null` while pending. */
  error: string | null;
  /** `false` while the first read is in flight **or** after it failed — the
   *  manage route's "you have no recipes yet" copy is a claim about the
   *  server, and a failed read is not entitled to make it. Same rule, and the
   *  same past defect, as `useTrackTemplates`. */
  loaded: boolean;
}>;

export function useTrackRecipes(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): TrackRecipes {
  const query = useQuery(trackRecipesQueryOptions(transport, unauthorized));
  return {
    recipes: query.data ?? [],
    error: query.isError ? 'Could not load your recipes.' : null,
    loaded: !query.isPending && !query.isError,
  };
}

export type TrackRecipeMutations = Readonly<{
  create: (body: { title: string; body: string }) => Promise<TrackRecipe>;
  /**
   * Whole-document `PUT` gated on `if_revision`. **Resolves with the stored
   * row**, which is not always the bytes sent — the write boundary re-renders
   * every fence, drops tombstones and normalizes the task privilege fields.
   * Callers render the resolution, never their own draft.
   *
   * Rejects with an `ApiError` whose `failure.status` is 409 when the recipe
   * moved under the writer.
   */
  save: (recipeId: string, body: { title: string; body: string; if_revision: number }) => Promise<TrackRecipe>;
  remove: (recipeId: string) => Promise<void>;
}>;

export function useTrackRecipeMutations(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): TrackRecipeMutations {
  const client = useQueryClient();
  const invalidate = () => { void client.invalidateQueries({ queryKey: queryKeys.trackRecipes() }); };
  const create = useMutation({
    mutationFn: (body: { title: string; body: string }) => runOperation(
      transport, createTrackRecipeOperation(body), unauthorized,
    ),
    onSuccess: invalidate,
  });
  const save = useMutation({
    mutationFn: (variables: { recipeId: string; body: { title: string; body: string; if_revision: number } }) =>
      runOperation(transport, updateTrackRecipeOperation(variables.recipeId, variables.body), unauthorized),
    /* Invalidate but do **not** write the response through to the cache here.
       The response is also the editor's next rendered state, and it reaches
       the editor as the promise's value; writing it into the list as well
       would give the same fact two homes with no third party to keep them in
       step.

       `onSettled`, not `onSuccess`, for the reason `remove` gives below and
       for one more: a save rejected with a 409 means the list is holding a
       revision the server has moved past, and the editor's conflict notice
       tells the reader to close and reopen the recipe to start from the
       current version — which only produces a current version if something
       refetched. On success the refetch this queues is what updates the
       list's row; on a 409 it is what makes that instruction true. */
    onSettled: invalidate,
  });
  const remove = useMutation({
    mutationFn: (recipeId: string) => runOperation(transport, deleteTrackRecipeOperation(recipeId), unauthorized),
    /* `onSettled`, not `onSuccess`: a delete that failed because the row was
       already gone leaves the list holding a row that does not exist, and
       refetching is how the reader finds out. */
    onSettled: invalidate,
  });
  return {
    create: (body) => create.mutateAsync(body),
    save: (recipeId, body) => save.mutateAsync({ recipeId, body }),
    remove: async (recipeId) => { await remove.mutateAsync(recipeId); },
  };
}

export type Workspace = Readonly<{
  areas: Area[];
  tracksByArea: ReadonlyMap<string, Track[]>;
  tracks: Track[];
  areasLoading: boolean;
  overlaysLoading: boolean;
  areasError: Error | null;
  overlaysError: Error | null;
  trackErrorsByArea: ReadonlyMap<string, Error>;
  tracksLoadingByArea: ReadonlyMap<string, boolean>;
  retryAreas: () => void;
  retryOverlays: () => void;
  retryTracks: (areaId: string) => void;
}>;

/**
 * INV-APP-084 — the area → tracks fan-out is a page-level `useQueries`, never a
 * route loader await. One slow area must not block the calendar; each area's
 * list also stays its own cache entry, so a track moving between areas
 * invalidates two lists instead of the whole workspace.
 *
 * The workspace-wide track-overlay read is folded in here so every surface —
 * sidebar buckets, Today's counters, area lists — sees the same
 * `anyCardNeedsInput` / progress / eta / now, rather than only the track the
 * user happens to have open.
 */
export function useWorkspace(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Workspace {
  const areasQuery = useQuery(areaListQueryOptions(transport, unauthorized));
  const overlaysQuery = useQuery(trackOverlaysQueryOptions(transport, unauthorized));
  const areas = areasQuery.data ?? [];
  const overlays = overlaysQuery.data ?? [];
  const trackQueries = useQueries({
    queries: areas.map((area) => tracksInAreaQueryOptions(transport, area.id, unauthorized)),
  });
  const tracksByArea = new Map<string, Track[]>();
  const trackErrorsByArea = new Map<string, Error>();
  const tracksLoadingByArea = new Map<string, boolean>();
  const tracks: Track[] = [];
  for (const [index, area] of areas.entries()) {
    const query = trackQueries[index];
    tracksLoadingByArea.set(area.id, query?.isLoading ?? false);
    if (query?.error instanceof Error) trackErrorsByArea.set(area.id, query.error);
    if (query?.data !== undefined) {
      const rows = query.data.map((track) => ({ ...track, ...trackActivityFrom(track.id, overlays) }));
      tracksByArea.set(area.id, rows);
      tracks.push(...rows);
    }
  }
  return {
    areas, tracksByArea, tracks, areasLoading: areasQuery.isLoading,
    overlaysLoading: overlaysQuery.isLoading,
    areasError: areasQuery.error instanceof Error ? areasQuery.error : null,
    overlaysError: overlaysQuery.error instanceof Error ? overlaysQuery.error : null,
    trackErrorsByArea,
    tracksLoadingByArea,
    retryAreas: () => { void areasQuery.refetch(); },
    retryOverlays: () => { void overlaysQuery.refetch(); },
    retryTracks: (areaId) => {
      const index = areas.findIndex((area) => area.id === areaId);
      if (index >= 0) void trackQueries[index]?.refetch();
    },
  };
}

/** Route loaders prime only this one list; see INV-APP-084 above. */
export function prefetchAreaList(client: QueryClient, transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Promise<Area[]> {
  return client.ensureQueryData(areaListQueryOptions(transport, unauthorized));
}

// ---------- mutations ----------
//
// Every mutation invalidates. A mutation may additionally write its response
// through to the cache first, but only when that response *is* the new cache
// value (an id-keyed row the server just returned) and the very next render
// needs it. The invalidation still follows and reconciles;
// a write-through that guessed, or that stood in for one, would only widen the
// window in which the cache and the server disagree.

export type AreaMutations = Readonly<{
  create: (body: NewAreaBody) => Promise<Area>;
  update: (areaId: string, body: AreaPatchBody) => Promise<Area>;
  remove: (areaId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useAreaMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): AreaMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewAreaBody) => runOperation(transport, createAreaOperation(body), unauthorized),
    onSuccess: (wire) => {
      const created = toArea(wire);
      client.setQueryData<Area[]>(queryKeys.areas(), (current) => {
        if (current === undefined) return current;
        const existing = current.find((area) => area.id === created.id);
        return sortedAreas([
          ...current.filter((area) => area.id !== created.id),
          existing === undefined ? created : newestArea(existing, created),
        ]);
      });
    },
    // A lost response does not prove the POST rolled back. Await the refetch
    // before the caller exposes retry UI, so that UI first observes the latest
    // Area list. This narrows the uncertainty window; POST itself is not an
    // idempotent API and this client-side reconciliation does not pretend it is.
    onSettled: () => client.invalidateQueries({ queryKey: queryKeys.areas() }),
  });
  const update = useMutation({
    mutationFn: ({ areaId, body }: { areaId: string; body: AreaPatchBody }) =>
      runOperation(transport, updateAreaOperation(areaId, body), unauthorized),
    onSuccess: (wire) => {
      const updated = toArea(wire);
      // The Area editor closes as soon as mutateAsync resolves, and its row's
      // New Track action is immediately usable. Write the authoritative PATCH
      // response through before that close; otherwise a click in the refetch
      // window snapshots stale defaults into NewTrackForm's local state.
      client.setQueryData<Area[]>(queryKeys.areas(), (current) => current?.map(
        (area) => area.id === updated.id
          ? newestArea(area, updated)
          : area,
      ));
    },
    // Success and failure both reconcile with the server. A transport failure
    // may still follow a committed write, so failure cannot leave cached Area
    // defaults authoritative.
    onSettled: () => client.invalidateQueries({ queryKey: queryKeys.areas() }),
  });
  const remove = useMutation({
    mutationFn: ({ areaId, signal }: { areaId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteAreaOperation(areaId), signal }, unauthorized),
    onSuccess: (_result, { areaId }) => {
      // The area is gone; its track list can never resolve again, so drop it
      // instead of leaving a permanently-stale entry behind.
      client.removeQueries({ queryKey: queryKeys.tracksInArea(areaId) });
    },
    // Abort only ends the client wait: the server may already have committed.
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.areas() }); },
  });
  return {
    create: async (body) => toArea(await create.mutateAsync(body)),
    update: async (areaId, body) => toArea(await update.mutateAsync({ areaId, body })),
    remove: async (areaId, signal) => { await remove.mutateAsync({ areaId, signal }); },
  };
}

export type TrackMutations = Readonly<{
  create: (body: NewTrackBody) => Promise<Track>;
  patch: (trackId: string, areaId: string, body: TrackPatchBody) => Promise<Track>;
  setPinned: (trackId: string, areaId: string, pinned: boolean, nowMs: number) => Promise<Track>;
  createTerminal: (trackId: string, body: NewTerminalCardBody) => Promise<CardWire>;
  createCodex: (trackId: string, body: NewCodexCardBody) => Promise<CardWire>;
  createCard: (trackId: string, body: NewCardBody) => Promise<CardWire>;
  removeCard: (trackId: string, cardId: string, signal?: AbortSignal) => Promise<void>;
  remove: (trackId: string, areaId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useTrackMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): TrackMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewTrackBody) => runOperation(transport, createTrackOperation(body), unauthorized),
    onSuccess: (track, body) => {
      void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(track.area_id) });
      // Explicit `attach_folder` still mints a area_folders row. Drop any
      // cached list so a later folders read cannot serve a stale empty array.
      if (body.attach_folder) {
        client.removeQueries({ queryKey: queryKeys.areaFolders(body.area_id) });
      }
    },
  });
  const patch = useMutation({
    mutationFn: ({ trackId, body }: { trackId: string; areaId: string; body: TrackPatchBody }) =>
      runOperation(transport, updateTrackOperation(trackId, body), unauthorized),
    onSuccess: (track, variables) => {
      // Prefer the area the server just reported: a patch can move the track.
      void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(track.area_id) });
      if (track.area_id !== variables.areaId) {
        void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(variables.areaId) });
      }
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(variables.trackId) });
    },
  });
  const remove = useMutation({
    mutationFn: ({ trackId, signal }: { trackId: string; areaId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteTrackOperation(trackId), signal }, unauthorized),
    onSuccess: (_result, variables) => {
      client.removeQueries({ queryKey: queryKeys.trackDetail(variables.trackId) });
    },
    // Reconcile both list-derived surfaces even if abort raced a committed DELETE.
    onSettled: (_result, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(variables.areaId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('track') });
    },
  });
  /*
   * The three card creates answer with the row the kernel just wrote, and the
   * very next render needs it — the caller navigates to `?card=<id>`, and the
   * board can only draw a card the detail cache already holds. So each one
   * writes through and then invalidates, which is the write-through rule at the
   * top of this section, not an exception to it.
   */
  const addCardToDetail = (card: CardWire): void => {
    client.setQueryData(queryKeys.trackDetail(card.track_id), (previous: TrackDetailWire | undefined) => {
      if (previous === undefined) return previous;
      if (previous.cards.some((existing) => existing.id === card.id)) return previous;
      return { ...previous, cards: [...previous.cards, card] };
    });
    void client.invalidateQueries({ queryKey: queryKeys.trackDetail(card.track_id) });
  };
  const createTerminal = useMutation({
    mutationFn: ({ trackId, body }: { trackId: string; body: NewTerminalCardBody }) =>
      runOperation(transport, createTerminalCardOperation(trackId, body), unauthorized),
    onSuccess: addCardToDetail,
  });
  const createCodex = useMutation({
    mutationFn: ({ trackId, body }: { trackId: string; body: NewCodexCardBody }) =>
      runOperation(transport, createCodexCardOperation(trackId, body), unauthorized),
    onSuccess: addCardToDetail,
  });
  const createCard = useMutation({
    mutationFn: ({ trackId, body }: { trackId: string; body: NewCardBody }) =>
      runOperation(transport, createCardOperation(trackId, body), unauthorized),
    onSuccess: addCardToDetail,
  });
  /*
   * Delete drops the row from the cached detail before the refetch lands: the
   * card's surface is unmounted by that write, and leaving it on screen until
   * the round-trip returns would keep a PTY attached to a card the kernel has
   * already torn down.
   *
   * `onSettled`, not `onSuccess`, for the invalidation — an aborted wait says
   * nothing about whether the server committed.
   */
  const removeCard = useMutation({
    mutationFn: ({ cardId, signal }: { trackId: string; cardId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteCardOperation(cardId), signal }, unauthorized),
    onSuccess: (_result, { trackId, cardId }) => {
      client.setQueryData(queryKeys.trackDetail(trackId), (previous: TrackDetailWire | undefined) => {
        if (previous === undefined) return previous;
        const cards = previous.cards.filter((existing) => existing.id !== cardId);
        return cards.length === previous.cards.length ? previous : { ...previous, cards };
      });
    },
    onSettled: (_result, _error, { trackId }) => {
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('track') });
    },
  });
  const patchTrack = async (trackId: string, areaId: string, body: TrackPatchBody) =>
    toTrack(await patch.mutateAsync({ trackId, areaId, body }));
  return {
    create: async (body) => toTrack(await create.mutateAsync(body)),
    patch: patchTrack,
    createTerminal: async (trackId, body) => createTerminal.mutateAsync({ trackId, body }),
    createCodex: async (trackId, body) => createCodex.mutateAsync({ trackId, body }),
    createCard: async (trackId, body) => createCard.mutateAsync({ trackId, body }),
    removeCard: async (trackId, cardId, signal) => {
      await removeCard.mutateAsync({ trackId, cardId, signal });
    },
    // `pinned_at` is both the flag and the ordering key, so unpinning is a
    // null write rather than a delete of some separate row.
    setPinned: (trackId, areaId, pinned, nowMs) =>
      patchTrack(trackId, areaId, { pinned_at: pinned ? nowMs : null }),
    remove: async (trackId, areaId, signal) => { await remove.mutateAsync({ trackId, areaId, signal }); },
  };
}

/**
 * Settings › Plugins — the installed list.
 *
 * `retry: false` for the same reason the template read has it: this list is a
 * screen the reader is looking at, and a failed read must say so and offer
 * Retry rather than sit spinning through three silent attempts.
 *
 * **`plugin.state` does not refresh this list.** `core/events/invalidation-plan`
 * maps that event to `noop('No plugin list query exists.')` — a reason that was
 * true until this query was added, and `core/events` is a frozen module whose
 * change needs its own issue. Without help, enabling a plugin would leave the
 * row reading `spawning` for as long as the pane stayed open, which is exactly
 * the transition the reader is waiting on.
 *
 * So the query polls *only while some row is in motion*: a `spawning` or
 * `installing` plugin is a state the kernel is actively leaving, and every
 * other state is one nothing will change without a write from this screen. The
 * poll therefore stops on its own, and a settled list costs nothing.
 */
/* Frozen array, not a `Set`: `architecture/no-module-runtime-state` rejects a
   module-level `new`, and two entries do not need a hash lookup. */
const PLUGIN_TRANSIENT_STATES = Object.freeze(['spawning', 'installing'] as const);

export function pluginsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.plugins(),
    queryFn: (): Promise<PluginListItem[]> => runOperation(transport, pluginsOperation(), unauthorized),
    retry: false,
    /*
     * Polls **only while a row is in motion**, and only while this pane holds
     * the query — leaving Settings drops the observer and the interval with it.
     * That visibility is the bound.
     *
     * A counted cap was tried and was worse: `dataUpdateCount` also counts the
     * invalidation every enable/disable fires, it lives on the cached query
     * rather than on this visit, and it never resets — so a handful of toggles
     * exhausted the budget and left the poll permanently off, which is the one
     * failure mode this exists to prevent.
     */
    refetchInterval: (query: { state: { data?: PluginListItem[] } }) =>
      (query.state.data ?? []).some((plugin) =>
        (PLUGIN_TRANSIENT_STATES as readonly string[]).includes(plugin.state))
        ? PLUGIN_POLL_MS
        : false as const,
  };
}

const PLUGIN_POLL_MS = 2000;

export type PluginMutations = Readonly<{
  /** The plugins a lifecycle write is in flight for. */
  pendingIds: ReadonlySet<string>;
  /** The last failure per plugin, so one plugin's error cannot label another. */
  errors: ReadonlyMap<string, string>;
  setEnabled: (id: string, enabled: boolean) => void;
}>;

/**
 * Enable / disable.
 *
 * **Per plugin, not per hook.** A single `useMutation` exposes only the latest
 * call's `variables` and `error`, so toggling two plugins in quick succession
 * moved the spinner onto the second one — the first row snapped back to its
 * server value mid-flight — and a failure on the first was attributed to
 * whichever call happened last, or lost entirely. The pending set and the
 * error map are keyed by plugin id, which is the only thing that makes two
 * concurrent writes describable.
 *
 * The response is **not** written through to the cached list: enable answers as
 * soon as the row flips, while the supervisor is still bringing the process up,
 * so its `state` is a snapshot that is already stale by the time it lands. An
 * invalidation asks the kernel what is actually true.
 */
export function usePluginMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): PluginMutations {
  const client = useQueryClient();
  /* A **count** per id, not membership: the switch stays usable while a write
     is in flight, so one plugin can have two. With a set, the first response
     cleared the marker while the second write was still out, and the row read
     idle mid-flight. */
  const [pending, setPending] = useState<ReadonlyMap<string, number>>(() => new Map());
  const [errors, setErrors] = useState<ReadonlyMap<string, string>>(() => new Map());
  const write = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      runOperation(transport, setPluginEnabledOperation(id, enabled), unauthorized),
    onMutate: ({ id }) => {
      setPending((current) => new Map(current).set(id, (current.get(id) ?? 0) + 1));
      setErrors((current) => {
        if (!current.has(id)) return current;
        const next = new Map(current);
        next.delete(id);
        return next;
      });
    },
    onError: (error, { id }) => {
      setErrors((current) => new Map(current)
        .set(id, error instanceof Error ? error.message : 'Could not change this plugin.'));
    },
    onSettled: (_data, _error, { id }) => {
      setPending((current) => {
        const next = new Map(current);
        const left = (current.get(id) ?? 1) - 1;
        if (left <= 0) next.delete(id); else next.set(id, left);
        return next;
      });
      void client.invalidateQueries({ queryKey: queryKeys.plugins() });
    },
  });
  return {
    pendingIds: new Set(pending.keys()),
    errors,
    setEnabled: (id, enabled) => { write.mutate({ id, enabled }); },
  };
}

/**
 * Settings › Plugins › one plugin's configuration — the detail read.
 *
 * `retry: false` for the reason the list has it: this is a screen the reader is
 * looking at, and a failed read has to say so and offer Retry rather than sit
 * spinning through three silent attempts.
 *
 * It does not poll and it is not refetched on focus. The pane holds a draft of
 * the operator's edits, and a background refetch that re-seeded it mid-typing
 * would be indistinguishable from the app throwing their work away. Every write
 * from that pane invalidates this key explicitly, which is the only moment the
 * stored document can change under it.
 */
export function pluginDetailQueryOptions(
  transport: ApiTransportPort,
  id: string,
  unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.pluginDetail(id),
    queryFn: (): Promise<PluginDetail> => runOperation(transport, pluginDetailOperation(id), unauthorized),
    retry: false,
    refetchOnWindowFocus: false,
  };
}

/**
 * The kernel's refusal, reduced to what #1284's tables are keyed on.
 *
 * Transport and decode failures carry no `code` — there is no HTTP body to read
 * one from — so they get one here rather than being handed to the domain as a
 * third shape it would have to special-case. `transport_failure` is not a
 * kernel code and no branch matches it, which is correct: it falls through to
 * "the plugin did not come back and here is what we know", which is exactly
 * what a request that never arrived leaves behind.
 */
function pluginFailureOf(error: unknown): PluginApiFailure {
  if (error instanceof ApiError) {
    const { failure } = error;
    return failure.kind === 'transport' || failure.kind === 'decode'
      ? { code: 'transport_failure', message: failure.message }
      : { code: failure.code, message: failure.message };
  }
  return { code: 'transport_failure', message: 'The request could not be completed.' };
}

export type PluginConfigMutations = Readonly<{
  save: (
    id: string,
    patch: Readonly<Record<string, PluginConfigValue | null>>,
    options: Readonly<{ reset: boolean }>,
  ) => Promise<PluginConfigSaveResult>;
  applyRestart: (
    id: string,
    patch: Readonly<Record<string, PluginConfigValue | null>>,
    options: Readonly<{ reset: boolean }>,
  ) => Promise<PluginConfigApplyResult>;
}>;

/**
 * The two writes the configuration pane offers, and the read #1284 §2.4
 * requires **after** the second one.
 *
 * Both resolve rather than reject. A rejected promise carries a message and
 * nothing else, and every branch of §2.2 and §2.4 turns on the kernel's `code`
 * or on the plugin's state afterwards — so a thrown `Error` would arrive at the
 * pane with the one field that cannot distinguish "nothing was saved, retry"
 * from "saved, and the plugin is now down".
 *
 * This hook classifies nothing. It returns facts — the refusal, and the state
 * and `last_error` read back after a restart — and `core/domain/plugins` owns
 * the tables that read them. That is what keeps the wording in one place
 * instead of one place per caller.
 */
export function usePluginConfigMutations(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): PluginConfigMutations {
  const client = useQueryClient();
  const refresh = (id: string) => Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.plugins() }),
    client.invalidateQueries({ queryKey: queryKeys.pluginDetail(id) }),
  ]);

  const write = async (
    id: string,
    patch: Readonly<Record<string, PluginConfigValue | null>>,
    options: Readonly<{ reset: boolean }>,
  ): Promise<PluginConfigSaveResult> => {
    try {
      await runOperation(transport, patchPluginConfigOperation(id, patch, options), unauthorized);
      return { ok: true };
    } catch (error) {
      return { ok: false, failure: pluginFailureOf(error) };
    }
  };

  return {
    save: async (id, patch, options) => {
      const result = await write(id, patch, options);
      await refresh(id);
      return result;
    },
    applyRestart: async (id, patch, options) => {
      /* An empty patch with no reset asked for is not a write: Apply & restart
         is also how an operator makes an *earlier* Save take effect, and
         PATCHing `{}` to do it would take the lifecycle lock for nothing and
         could 409 the restart it exists to perform. */
      if (Object.keys(patch).length > 0 || options.reset) {
        const saved = await write(id, patch, options);
        if (!saved.ok) {
          await refresh(id);
          return { saved: false, failure: saved.failure };
        }
      }
      /*
       * §2.4 wants the plugin's state **read back after the attempt**, and that
       * is a second request on *both* branches, not only the failing one.
       *
       * A 2xx `reload` answers with the detail as of the moment the handler
       * returned; a connector's bring-up can complete — or fail — after it. So
       * a 200 saying `running` followed by a detail saying `unavailable` with a
       * `last_error` is an ordinary sequence, and trusting the POST body alone
       * would confirm "restarted with it" over the top of the one diagnostic
       * that exists. Reading back on the success branch too is what makes the
       * verdict come from the plugin rather than from the response to the
       * command.
       *
       * The read-back is best-effort in the same sense on both branches: if it
       * cannot be made, the caller falls back to what it already knows, which
       * is the POST's own detail after a 2xx and nothing at all after a
       * refusal.
       */
      const readBack = async (fallback: PluginRestartFacts): Promise<PluginRestartFacts> => {
        try {
          const after = await runOperation(transport, pluginDetailOperation(id), unauthorized);
          return { ...fallback, state: after.state, lastError: after.last_error };
        } catch {
          return fallback;
        }
      };

      try {
        const detail = await runOperation(transport, reloadPluginOperation(id), unauthorized);
        const restart = await readBack({
          failure: null,
          state: detail.state,
          lastError: detail.last_error,
        });
        await refresh(id);
        return { saved: true, restart };
      } catch (error) {
        const failure = pluginFailureOf(error);
        /*
         * §2.4 — the refusal is not the verdict, so read the plugin back.
         *
         * A reload stops the plugin before it re-reads anything, so a non-200
         * covers three different endings: the lock was held and nothing
         * happened at all; a connector's bring-up failed and it is sitting in
         * its normal `unavailable` terminal state with the reason in
         * `last_error`; or an `app` was stopped and did not start. Only the
         * plugin's own state tells them apart. A detail read that itself fails
         * leaves `state` unknown, and the outcome table falls back to the
         * refusal's own message — which is worse than the truth, and better
         * than a guess.
         */
        const restart = await readBack({ failure, state: 'unknown' });
        await refresh(id);
        return { saved: true, restart };
      }
    },
  };
}

export function useSettingsMutation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): (patch: SettingsPatch) => Promise<SettingsBag> {
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: (patch: SettingsPatch) => runOperation(transport, putSettingsOperation(patch), unauthorized),
    /*
     * Invalidate; do **not** write the response through.
     *
     * The PUT answers with the whole bag, which used to be written straight
     * into the cache to avoid a refetch. That is only sound while writes cannot
     * overlap, and Settings › Network commits per field: change a proxy, leave
     * the field, change it again, leave again, and the first response can land
     * last. Written through, its older bag becomes the cache — measured: the
     * field reverted to the earlier value, under a green tick, while the server
     * held the newer one, and every other reader of `['settings']` saw the
     * stale bag until something refetched.
     *
     * A refetch cannot invert like that: it asks after the write settled, and
     * the last answer is the server's own state.
     */
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.settings() }); },
  });
  return (patch) => save.mutateAsync(patch);
}
