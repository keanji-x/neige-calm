// Query and mutation wiring shared by app/router, app/shell and every feature
// slice.
//
// It lives under app/providers rather than under any one consumer because the
// router renders the shell and both need the same area/wave reads: a queries
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
  sortedAreas, toArea, updateAreaOperation, visibleAreas,
  type Area, type AreaPatchBody, type FolderConflict, type NewAreaBody,
} from '../../../../core/domain/area.ts';
import {
  deriveReportTasks, hasLiveTaskRun, waveBacklinksOperation, waveTaskVerdictsOperation,
  type ReportBlock, type TaskVerdict, type WaveBacklinks,
} from '../../../../core/domain/report.ts';
import {
  pluginsOperation, setPluginEnabledOperation, type PluginListItem,
} from '../../../../core/domain/plugins.ts';
import {
  putSettingsOperation, settingsOperation, type SettingsBag, type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import {
  todayLaunchpadOperation, todaySummaryOperation,
  type TodayLaunchpadWire, type TodaySummaryWire,
} from '../../../../core/domain/today.ts';
import {
  createCardOperation, createCodexCardOperation, createTerminalCardOperation, createWaveOperation,
  deleteCardOperation, deleteWaveOperation, overlaysByKindOperation, toWave,
  updateWaveOperation, waveActivityFrom, waveDetailOperation, waveTemplatesOperation, wavesInAreaOperation,
  type CardWire, type NewCardBody, type NewCodexCardBody, type NewTerminalCardBody, type NewWaveBody,
  type OverlayWire, type Wave, type WaveDetailWire, type WavePatchBody, type WaveTemplate,
} from '../../../../core/domain/wave.ts';
import {
  HARNESS_ITEMS_PAGE_LIMIT, harnessItemsOperation, interruptSpecOperation, sendSpecInputOperation,
  specRunOperation, areaConversationsOperation, createAreaConversationOperation,
  createWaveConversationOperation, waveConversationsOperation,
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
  wavesInArea: (areaId: string) => ['waves', areaId] as const,
  waveDetail: (waveId: string) => ['wave', waveId] as const,
  waveBacklinks: (waveId: string) => ['wave-backlinks', waveId] as const,
  /* Exactly the shape `core/events/invalidation-plan` already plans for
     `wave.report_edited` and every `task.*` event, so naming it this way is
     what makes the TASKS panel live — see `waveReportPrefix` for the half of
     that the plan cannot key by wave. */
  waveReport: (waveId: string) => ['wave-report', waveId] as const,
  /**
   * The prefix `waveReport` extends, for the events the plan cannot key by
   * wave.
   *
   * `task.dispatched` / `task.completed` / `task.failed` / `task.gate_result`
   * carry no `wave_id` and no `card_id` **field**, so `derivedWaveId` — which
   * reads named fields and nothing else — returns null and the plan emits the
   * bare key. The wave id is not absent from those events: `idempotency_key`
   * *is* the task id, and a task id is `"{wave_id}:{key}"`
   * (`task_projection.rs`'s `format!("{wave_id}:{}", declaration.key)`, echoed
   * by all four kinds in `calm-types/src/event.rs`). The plan deliberately does
   * not take it apart — `WaveId` is an opaque newtype with no format contract,
   * so parsing an id in the pure planning layer would be a guess dressed as a
   * fact, and a wrong split yields a key matching no cached query, i.e. a panel
   * that silently stops refreshing.
   *
   * Dropping the bare key instead would leave the four events that matter most
   * to this panel as the four that do not refresh it. A prefix invalidation
   * reaches whichever wave report is cached — at most the open wave's — and
   * costs nothing when none is.
   */
  waveReportPrefix: () => ['wave-report'] as const,
  overlaysByKind: (entityKind: 'wave' | 'card') => ['overlays', entityKind] as const,
  settings: () => ['settings'] as const,
  /* Settings › Plugins. Not reached by any event policy — see
     `pluginsQueryOptions` for why, and for what stands in for one. */
  plugins: () => ['plugins'] as const,
  /* #1209 — the New wave picker's list. Not invalidated by any event: the
     kernel's template keys are compile-time constants and the only thing that
     can move under them is a plugin starting or stopping, which changes an
     `input_schema` the new-wave page reads. */
  waveTemplates: () => ['wave-templates'] as const,
  /* #1253 §5.1 — the Today launchpad resolve. One entry, not keyed by wave:
     the kernel's partial unique index makes `purpose = 'launchpad'` a
     singleton, and the id is what this query is fetching.

     PR2 put it on `wave.report_edited`'s invalidation list, together with
     `['wave', id]`. Both are needed and neither is generated: the first
     carries the empty-state predicate, the second carries the document, and
     `PolicyMap` is exhaustive over event kinds rather than over query keys, so
     no golden would have reported their absence. */
  todayLaunchpad: () => ['today-launchpad'] as const,
  harnessItems: (cardId: string) => ['harness-items', cardId] as const,
  specRun: (cardId: string) => ['spec-run', cardId] as const,
  /* The event bridge can only invalidate the `['area-conversations']` prefix —
     no event carries an area id and no cached row can supply one — so this key
     must keep the area id in second position for that prefix to reach it. */
  areaConversations: (areaId: string) => ['area-conversations', areaId] as const,
  /**
   * The prefix `areaConversations` extends — the only shape the event bridge
   * can name, and therefore the only thing that keeps this list live.
   *
   * `AREA_CONVERSATIONS` in `core/events/invalidation-plan` is the bare key by
   * construction: no conversation-writing event carries a `area_id`, and an area
   * chat wave's detail is never fetched, so no cached row can supply one
   * either. Naming the prefix here is what lets the adapter map that plan key
   * instead of dropping it — without this entry the area drawer's `state` dots
   * never move until something else refetches the list.
   *
   * A prefix invalidation reaches whichever area's list is cached — at most the
   * open drawer's — and costs nothing when none is.
   */
  areaConversationsPrefix: () => ['area-conversations'] as const,
  /**
   * One wave's conversation list (#1189 §4.1), keyed by its wave.
   *
   * `GET /api/waves/{wave_id}/conversations` is per-wave, and unlike the area
   * list the id *is* derivable from the events: the plan emits
   * `['wave-conversations', waveId]` whenever `derivedWaveId` resolves one.
   *
   * **The query that registers this key lands in S5; the mapping is here
   * first, and that is deliberate, not dead code.** Invalidating a key with no
   * mounted query is a no-op in TanStack Query — it marks nothing and refetches
   * nothing — so the adapter may know a key before a query claims it. The
   * reverse order is the one that breaks: a query that mounts against a key no
   * adapter arm maps is silently never invalidated, which is exactly the defect
   * this pair of entries fixes for the area list.
   */
  waveConversations: (waveId: string) => ['wave-conversations', waveId] as const,
  /**
   * The prefix `waveConversations` extends, for the events that cannot name a
   * wave.
   *
   * The three `runtime.*` kinds carry only a `card_id`, and
   * `findWaveOwningCard` answers from the cached wave details — so a card in a
   * wave nobody has open resolves to null and the plan emits the bare key. That
   * is the honest "some wave's list may have changed", and dropping it here
   * would leave a genuinely open list stale for precisely the transitions that
   * move a row's `state`.
   *
   * It is a fallback and not the house shape: invalidating this prefix on every
   * runtime tick would refetch the list of every wave the user has open, which
   * is why the plan keys by wave whenever it can.
   */
  waveConversationsPrefix: () => ['wave-conversations'] as const,
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

export function specRunQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.specRun(cardId),
    queryFn: () => runOperation(transport, specRunOperation(cardId), unauthorized),
  };
}

export function useSpecMutations(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  const client = useQueryClient();
  const refresh = () => Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }),
    client.invalidateQueries({ queryKey: queryKeys.specRun(cardId) }),
  ]).then(() => undefined);
  const refreshAfter = async <T,>(result: T): Promise<T> => {
    await refresh();
    return result;
  };
  return {
    send: (text: string) => runOperation(transport, sendSpecInputOperation(cardId, text), unauthorized).then(refreshAfter),
    interrupt: () => runOperation(transport, interruptSpecOperation(cardId), unauthorized).then(refreshAfter),
    /* No `reset` — see the note where `resetSpecOperation` used to be in
       `core/domain/conversation.ts`. The endpoint is still served; nothing in
       the browser calls it. */
  };
}

export function areaConversationsQueryOptions(
  transport: ApiTransportPort, areaId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.areaConversations(areaId),
    queryFn: (): Promise<Conversation[]> =>
      runOperation(transport, areaConversationsOperation(areaId), unauthorized),
  };
}

export type AreaConversationMutations = Readonly<{
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

export function useAreaConversationMutations(
  transport: ApiTransportPort, areaId: string, unauthorized: UnauthorizedChannel,
): AreaConversationMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: ({ text, idempotencyKey }: { text: string; idempotencyKey: string }) =>
      runOperation(transport, createAreaConversationOperation(areaId, text, idempotencyKey), unauthorized),
    onSuccess: (row) => {
      /* Written through as well as invalidated: the drawer switches to this row
         in the same tick, and a list that does not contain it yet would render
         the panel with no active row until the refetch lands. A replayed key
         answers with a row that is already there, so the write is by id. */
      client.setQueryData<Conversation[]>(queryKeys.areaConversations(areaId), (current) => {
        const rows = current ?? [];
        return rows.some((candidate) => candidate.id === row.id)
          ? rows.map((candidate) => candidate.id === row.id ? row : candidate)
          : [...rows, row];
      });
      void client.invalidateQueries({ queryKey: queryKeys.areaConversations(areaId) });
    },
  });
  return {
    create: (text, idempotencyKey) => create.mutateAsync({ text, idempotencyKey }),
    refresh: () => client.fetchQuery({
      ...areaConversationsQueryOptions(transport, areaId, unauthorized),
      staleTime: 0,
    }),
  };
}

/**
 * One wave's assistant conversations (#1189 §4.1).
 *
 * The key it registers is `queryKeys.waveConversations(waveId)`, which the
 * event bridge has mapped since S4 — so the list goes live the moment this
 * query mounts, with no second refresh path of its own.
 */
export function waveConversationsQueryOptions(
  transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.waveConversations(waveId),
    queryFn: (): Promise<Conversation[]> =>
      runOperation(transport, waveConversationsOperation(waveId), unauthorized),
  };
}

/**
 * The wave twin of `useAreaConversationMutations`, with the same two doors and
 * the same reasons for them.
 *
 * Not shared with the area version through a parameterised helper: the two
 * endpoints have different paths, different derived namespaces and different
 * cache keys, and the only thing a shared helper would save is four lines of
 * plumbing at the cost of making "which list does a create write through to?"
 * a question you have to trace.
 */
export function useWaveConversationMutations(
  transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel,
): AreaConversationMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: ({ text, idempotencyKey }: { text: string; idempotencyKey: string }) =>
      runOperation(transport, createWaveConversationOperation(waveId, text, idempotencyKey), unauthorized),
    onSuccess: (row) => {
      /* Written through as well as invalidated, for the same reason the area
         list is: the drawer switches to this row in the same tick and a list
         that does not hold it yet renders with no active row. */
      client.setQueryData<Conversation[]>(queryKeys.waveConversations(waveId), (current) => {
        const rows = current ?? [];
        return rows.some((candidate) => candidate.id === row.id)
          ? rows.map((candidate) => candidate.id === row.id ? row : candidate)
          : [...rows, row];
      });
      void client.invalidateQueries({ queryKey: queryKeys.waveConversations(waveId) });
      /* The card is new on the wave, so the wave's own detail — which is where
         the CARDS panel, the grid and the Today open-request all read cards
         from — is now one card short of the truth. */
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
    },
  });
  return {
    create: (text, idempotencyKey) => create.mutateAsync({ text, idempotencyKey }),
    refresh: () => client.fetchQuery({
      ...waveConversationsQueryOptions(transport, waveId, unauthorized),
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

export function waveOverlaysQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.overlaysByKind('wave'),
    queryFn: (): Promise<OverlayWire[]> => runOperation(transport, overlaysByKindOperation('wave'), unauthorized),
  };
}

export function wavesInAreaQueryOptions(transport: ApiTransportPort, areaId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.wavesInArea(areaId),
    queryFn: async (): Promise<Wave[]> =>
      (await runOperation(transport, wavesInAreaOperation(areaId), unauthorized)).map((wire) => toWave(wire)),
  };
}

export function waveDetailQueryOptions(transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.waveDetail(waveId),
    queryFn: (): Promise<WaveDetailWire> => runOperation(transport, waveDetailOperation(waveId), unauthorized),
  };
}

/**
 * Who cites this wave (§8.3).
 *
 * Its own cache entry rather than a field on the detail: backlinks are written
 * by *other* waves, so they go stale on edits this wave never sees, and folding
 * them into the detail would tie the document's freshness to theirs.
 */
export function waveBacklinksQueryOptions(transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.waveBacklinks(waveId),
    queryFn: (): Promise<WaveBacklinks> => runOperation(transport, waveBacklinksOperation(waveId), unauthorized),
  };
}

/**
 * The wave's task verdicts (§8.3) — what the kernel's task projection says
 * about each declared task: schedulable, status, and the worker card it was
 * dispatched onto.
 *
 * Its own cache entry rather than a field on the detail for the same reason
 * the backlinks are: the wave detail is a card read, and these change on every
 * dispatch and every gate result without any card being written. It is also
 * the key the event plan already names, which is what makes the panel live.
 *
 * **Events alone do not keep it live, and the timer below is why.** The write
 * that stamps `worker_card_id` — `scheduler::mark_running` — emits nothing at
 * all, and it lands *after* `task.dispatched` and after every `runtime.*` a
 * worker adapter emits during its spawn. See `hasLiveTaskRun` for the full
 * accounting per worker kind, and for why this is a poll and not a new event.
 */
export function waveTaskVerdictsQueryOptions(
  transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel,
  /* The declarations this report actually has, because the timer below is
     about the *rows* the panel draws and a verdict is not a row — see
     `taskVerdictsRefetchInterval`. They arrive with the wave detail, which is
     already in hand when this query is created. */
  blocks: readonly ReportBlock[] | null,
) {
  return {
    queryKey: queryKeys.waveReport(waveId),
    queryFn: (): Promise<TaskVerdict[]> => runOperation(transport, waveTaskVerdictsOperation(waveId), unauthorized),
    /*
     * See `taskVerdictsRefetchInterval` for when the timer runs at all.
     *
     * 3 seconds is priced off the endpoint, not chosen for roundness. Measured
     * on `GET /api/waves/{id}/report` (debug build, in-memory SQLite, this
     * box): a 3-task / 2-prose report answers in p50 14.8 ms, a 24-task /
     * 12-prose one in p50 104 ms — the cost is dominated by the per-declaration
     * projection, as `taskVerdictInvalidatingKinds` describes. At the measured
     * worst case that is ~3.5% of one core, for one open wave, only while
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
 * clears it. But `GET /api/waves/{id}/report` also fails *permanently* for a
 * wave that no longer exists (`resolve_report_for_wave` → `NotFound`) and for a
 * wave whose `wave-report` card is missing (the same function's invariant
 * violation → 500, `wave_report.rs`), and neither of those is going to get
 * better by being asked again. An unconditional poll on "no data" would leave a
 * stale or deleted tab hitting a dead route every few seconds for as long as it
 * stayed open.
 */
const TASK_VERDICT_RECOVERY_ATTEMPTS = 4;

/**
 * The timer, over both of the states this query can be in.
 *
 * **Data in hand** — poll only while the wave holds a task inside the eventless
 * window, and stop the moment none does (`false` is react-query's "no timer").
 * A settled wave, a wave that never dispatched anything, and a wave whose page
 * is closed all cost exactly nothing. This branch also covers a *failed
 * refetch*: react-query keeps the last good data, so a live run stays live
 * across a blip and the timer that will re-fetch it keeps running.
 *
 * **No data at all** — the initial load failed and react-query exhausted its
 * retries, so `data` is `undefined`, `hasLiveTaskRun` is vacuously false, and
 * the query would sit there with no timer forever: nothing in the page ever
 * asks again, and a wave that was mid-dispatch when the load failed would show
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
 * lands later as a `wave.report_edited` event, which the bridge turns into
 * `['today-launchpad']` and `['wave', id]`. Refetching either here would fetch
 * the *old* report, and worse, it would hide a broken invalidation chain behind
 * a lucky refresh: the page would appear to update after a press even with both
 * keys missing from the policy, which is the exact defect §6 exists to prevent.
 * An earlier version invalidated `['wave', id]` here while this comment claimed
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
      void client.invalidateQueries({ queryKey: queryKeys.waveConversationsPrefix() });
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
 * #1209 — templates for the new-wave page.
 *
 * `retry: false` and a plain failure are the point: the page degrades to
 * Blank-only when this read fails, and a retrying query would leave the entry
 * point spinning instead. Creating a wave must never depend on this list.
 */
export function waveTemplatesQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.waveTemplates(),
    queryFn: (): Promise<WaveTemplate[]> => runOperation(transport, waveTemplatesOperation(), unauthorized),
    retry: false,
  };
}

export type WaveTemplates = Readonly<{
  /** Never `undefined`: for the new-wave page, pending and failed both read
   *  as "Blank only". */
  templates: WaveTemplate[];
  /** A notice for the page, not a blocker. `null` while pending. */
  error: string | null;
  /** `false` while the first read is still in flight — see `useWaveTemplates`. */
  loaded: boolean;
  refetch: () => void;
}>;

/**
 * The new-wave page's template list, collapsed to the two things the page
 * can act on. A hook and not raw `useQuery` at the call site so the shell's
 * contract tests keep mocking exactly one module (`providers/queries`) —
 * the same shape `useWorkspace` and the mutation hooks already have.
 */
export function useWaveTemplates(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): WaveTemplates {
  const query = useQuery(waveTemplatesQueryOptions(transport, unauthorized));
  return {
    templates: query.data ?? [],
    error: query.isError ? 'Could not load templates.' : null,
    // `[]` must not be readable as "loaded and empty" — the New wave picker
    // renders a different affordance for "no templates" than for "not yet".
    //
    // A **failed** read is not loaded either. The first cut wrote
    // `!query.isPending`, which is true once a read has errored — so a dead
    // server produced `loaded: true` with `templates: []`, i.e. a picker
    // claiming the server has no templates instead of reporting the failure.
    //
    // #1300 S1 removed the Settings editor, which was this field's other
    // consumer. It stays because the picker is a consumer in its own right —
    // `new-wave/public.tsx` branches on it — not as a leftover.
    loaded: !query.isPending && !query.isError,
    refetch: () => { void query.refetch(); },
  };
}

export type Workspace = Readonly<{
  areas: Area[];
  wavesByArea: ReadonlyMap<string, Wave[]>;
  waves: Wave[];
  areasLoading: boolean;
  overlaysLoading: boolean;
  areasError: Error | null;
  overlaysError: Error | null;
  waveErrorsByArea: ReadonlyMap<string, Error>;
  wavesLoadingByArea: ReadonlyMap<string, boolean>;
  retryAreas: () => void;
  retryOverlays: () => void;
  retryWaves: (areaId: string) => void;
}>;

/**
 * INV-APP-084 — the area → waves fan-out is a page-level `useQueries`, never a
 * route loader await. One slow area must not block the calendar; each area's
 * list also stays its own cache entry, so a wave moving between areas
 * invalidates two lists instead of the whole workspace.
 *
 * The workspace-wide wave-overlay read is folded in here so every surface —
 * sidebar buckets, Today's counters, area lists — sees the same
 * `anyCardNeedsInput` / progress / eta / now, rather than only the wave the
 * user happens to have open.
 */
export function useWorkspace(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Workspace {
  const areasQuery = useQuery(areaListQueryOptions(transport, unauthorized));
  const overlaysQuery = useQuery(waveOverlaysQueryOptions(transport, unauthorized));
  const areas = areasQuery.data ?? [];
  const overlays = overlaysQuery.data ?? [];
  const waveQueries = useQueries({
    queries: areas.map((area) => wavesInAreaQueryOptions(transport, area.id, unauthorized)),
  });
  const wavesByArea = new Map<string, Wave[]>();
  const waveErrorsByArea = new Map<string, Error>();
  const wavesLoadingByArea = new Map<string, boolean>();
  const waves: Wave[] = [];
  for (const [index, area] of areas.entries()) {
    const query = waveQueries[index];
    wavesLoadingByArea.set(area.id, query?.isLoading ?? false);
    if (query?.error instanceof Error) waveErrorsByArea.set(area.id, query.error);
    if (query?.data !== undefined) {
      const rows = query.data.map((wave) => ({ ...wave, ...waveActivityFrom(wave.id, overlays) }));
      wavesByArea.set(area.id, rows);
      waves.push(...rows);
    }
  }
  return {
    areas, wavesByArea, waves, areasLoading: areasQuery.isLoading,
    overlaysLoading: overlaysQuery.isLoading,
    areasError: areasQuery.error instanceof Error ? areasQuery.error : null,
    overlaysError: overlaysQuery.error instanceof Error ? overlaysQuery.error : null,
    waveErrorsByArea,
    wavesLoadingByArea,
    retryAreas: () => { void areasQuery.refetch(); },
    retryOverlays: () => { void overlaysQuery.refetch(); },
    retryWaves: (areaId) => {
      const index = areas.findIndex((area) => area.id === areaId);
      if (index >= 0) void waveQueries[index]?.refetch();
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
// needs it — see `useAreaConversationMutations`, where the drawer switches to
// the new row in the same tick. The invalidation still follows and reconciles;
// a write-through that guessed, or that stood in for one, would only widen the
// window in which the cache and the server disagree.

export type AreaMutations = Readonly<{
  create: (body: NewAreaBody) => Promise<Area>;
  rename: (areaId: string, body: AreaPatchBody) => Promise<Area>;
  remove: (areaId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useAreaMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): AreaMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewAreaBody) => runOperation(transport, createAreaOperation(body), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.areas() }); },
  });
  const rename = useMutation({
    mutationFn: ({ areaId, body }: { areaId: string; body: AreaPatchBody }) =>
      runOperation(transport, updateAreaOperation(areaId, body), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.areas() }); },
  });
  const remove = useMutation({
    mutationFn: ({ areaId, signal }: { areaId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteAreaOperation(areaId), signal }, unauthorized),
    onSuccess: (_result, { areaId }) => {
      // The area is gone; its wave list can never resolve again, so drop it
      // instead of leaving a permanently-stale entry behind.
      client.removeQueries({ queryKey: queryKeys.wavesInArea(areaId) });
    },
    // Abort only ends the client wait: the server may already have committed.
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.areas() }); },
  });
  return {
    create: async (body) => toArea(await create.mutateAsync(body)),
    rename: async (areaId, body) => toArea(await rename.mutateAsync({ areaId, body })),
    remove: async (areaId, signal) => { await remove.mutateAsync({ areaId, signal }); },
  };
}

export type WaveMutations = Readonly<{
  create: (body: NewWaveBody) => Promise<Wave>;
  patch: (waveId: string, areaId: string, body: WavePatchBody) => Promise<Wave>;
  setPinned: (waveId: string, areaId: string, pinned: boolean, nowMs: number) => Promise<Wave>;
  createTerminal: (waveId: string, body: NewTerminalCardBody) => Promise<CardWire>;
  createCodex: (waveId: string, body: NewCodexCardBody) => Promise<CardWire>;
  createCard: (waveId: string, body: NewCardBody) => Promise<CardWire>;
  removeCard: (waveId: string, cardId: string, signal?: AbortSignal) => Promise<void>;
  remove: (waveId: string, areaId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useWaveMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): WaveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewWaveBody) => runOperation(transport, createWaveOperation(body), unauthorized),
    onSuccess: (wave, body) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInArea(wave.area_id) });
      // Explicit `attach_folder` still mints a area_folders row. Drop any
      // cached list so a later folders read cannot serve a stale empty array.
      if (body.attach_folder) {
        client.removeQueries({ queryKey: queryKeys.areaFolders(body.area_id) });
      }
    },
  });
  const patch = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; areaId: string; body: WavePatchBody }) =>
      runOperation(transport, updateWaveOperation(waveId, body), unauthorized),
    onSuccess: (wave, variables) => {
      // Prefer the area the server just reported: a patch can move the wave.
      void client.invalidateQueries({ queryKey: queryKeys.wavesInArea(wave.area_id) });
      if (wave.area_id !== variables.areaId) {
        void client.invalidateQueries({ queryKey: queryKeys.wavesInArea(variables.areaId) });
      }
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
  });
  const remove = useMutation({
    mutationFn: ({ waveId, signal }: { waveId: string; areaId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteWaveOperation(waveId), signal }, unauthorized),
    onSuccess: (_result, variables) => {
      client.removeQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
    // Reconcile both list-derived surfaces even if abort raced a committed DELETE.
    onSettled: (_result, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInArea(variables.areaId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
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
    client.setQueryData(queryKeys.waveDetail(card.wave_id), (previous: WaveDetailWire | undefined) => {
      if (previous === undefined) return previous;
      if (previous.cards.some((existing) => existing.id === card.id)) return previous;
      return { ...previous, cards: [...previous.cards, card] };
    });
    void client.invalidateQueries({ queryKey: queryKeys.waveDetail(card.wave_id) });
  };
  const createTerminal = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; body: NewTerminalCardBody }) =>
      runOperation(transport, createTerminalCardOperation(waveId, body), unauthorized),
    onSuccess: addCardToDetail,
  });
  const createCodex = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; body: NewCodexCardBody }) =>
      runOperation(transport, createCodexCardOperation(waveId, body), unauthorized),
    onSuccess: addCardToDetail,
  });
  const createCard = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; body: NewCardBody }) =>
      runOperation(transport, createCardOperation(waveId, body), unauthorized),
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
    mutationFn: ({ cardId, signal }: { waveId: string; cardId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteCardOperation(cardId), signal }, unauthorized),
    onSuccess: (_result, { waveId, cardId }) => {
      client.setQueryData(queryKeys.waveDetail(waveId), (previous: WaveDetailWire | undefined) => {
        if (previous === undefined) return previous;
        const cards = previous.cards.filter((existing) => existing.id !== cardId);
        return cards.length === previous.cards.length ? previous : { ...previous, cards };
      });
    },
    onSettled: (_result, _error, { waveId }) => {
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
    },
  });
  const patchWave = async (waveId: string, areaId: string, body: WavePatchBody) =>
    toWave(await patch.mutateAsync({ waveId, areaId, body }));
  return {
    create: async (body) => toWave(await create.mutateAsync(body)),
    patch: patchWave,
    createTerminal: async (waveId, body) => createTerminal.mutateAsync({ waveId, body }),
    createCodex: async (waveId, body) => createCodex.mutateAsync({ waveId, body }),
    createCard: async (waveId, body) => createCard.mutateAsync({ waveId, body }),
    removeCard: async (waveId, cardId, signal) => {
      await removeCard.mutateAsync({ waveId, cardId, signal });
    },
    // `pinned_at` is both the flag and the ordering key, so unpinning is a
    // null write rather than a delete of some separate row.
    setPinned: (waveId, areaId, pinned, nowMs) =>
      patchWave(waveId, areaId, { pinned_at: pinned ? nowMs : null }),
    remove: async (waveId, areaId, signal) => { await remove.mutateAsync({ waveId, areaId, signal }); },
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
