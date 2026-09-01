// Query and mutation wiring shared by app/router, app/shell and every feature
// slice.
//
// It lives under app/providers rather than under any one consumer because the
// router renders the shell and both need the same cove/wave reads: a queries
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
  coveListOperation, createCoveOperation, deleteCoveOperation,
  sortedCoves, toCove, updateCoveOperation, visibleCoves,
  type Cove, type CovePatchBody, type NewCoveBody,
} from '../../../../core/domain/cove.ts';
import {
  deriveReportTasks, hasLiveTaskRun, waveBacklinksOperation, waveTaskVerdictsOperation,
  type ReportBlock, type TaskVerdict, type WaveBacklinks,
} from '../../../../core/domain/report.ts';
import {
  putSettingsOperation, settingsOperation, type SettingsBag, type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import {
  createTerminalCardOperation, createWaveOperation, deleteWaveOperation, overlaysByKindOperation, toWave,
  updateWaveOperation, waveActivityFrom, waveDetailOperation, waveTemplatesOperation, wavesInCoveOperation,
  type CardWire, type NewTerminalCardBody, type NewWaveBody, type OverlayWire, type Wave,
  type WaveDetailWire, type WavePatchBody, type WaveTemplate,
} from '../../../../core/domain/wave.ts';
import {
  HARNESS_ITEMS_PAGE_LIMIT, harnessItemsOperation, interruptSpecOperation, sendSpecInputOperation,
  specRunOperation, coveConversationsOperation, createCoveConversationOperation,
  type Conversation,
} from '../../../../core/domain/conversation.ts';
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
  coves: () => ['coves'] as const,
  coveFolders: (coveId: string) => ['cove-folders', coveId] as const,
  wavesInCove: (coveId: string) => ['waves', coveId] as const,
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
  /* #1209 — the New wave picker's list. Not invalidated by any event: the
     kernel's template keys are compile-time constants and the only thing that
     can move under them is a plugin starting or stopping, which changes an
     `input_schema` the dialog reads when it opens. */
  waveTemplates: () => ['wave-templates'] as const,
  harnessItems: (cardId: string) => ['harness-items', cardId] as const,
  specRun: (cardId: string) => ['spec-run', cardId] as const,
  /* The event bridge can only invalidate the `['cove-conversations']` prefix —
     no event carries a cove id and no cached row can supply one — so this key
     must keep the cove id in second position for that prefix to reach it. */
  coveConversations: (coveId: string) => ['cove-conversations', coveId] as const,
  /**
   * The prefix `coveConversations` extends — the only shape the event bridge
   * can name, and therefore the only thing that keeps this list live.
   *
   * `COVE_CONVERSATIONS` in `core/events/invalidation-plan` is the bare key by
   * construction: no conversation-writing event carries a `cove_id`, and a cove
   * chat wave's detail is never fetched, so no cached row can supply one
   * either. Naming the prefix here is what lets the adapter map that plan key
   * instead of dropping it — without this entry the cove drawer's `state` dots
   * never move until something else refetches the list.
   *
   * A prefix invalidation reaches whichever cove's list is cached — at most the
   * open drawer's — and costs nothing when none is.
   */
  coveConversationsPrefix: () => ['cove-conversations'] as const,
  /**
   * One wave's conversation list (#1189 §4.1), keyed by its wave.
   *
   * `GET /api/waves/{wave_id}/conversations` is per-wave, and unlike the cove
   * list the id *is* derivable from the events: the plan emits
   * `['wave-conversations', waveId]` whenever `derivedWaveId` resolves one.
   *
   * **The query that registers this key lands in S5; the mapping is here
   * first, and that is deliberate, not dead code.** Invalidating a key with no
   * mounted query is a no-op in TanStack Query — it marks nothing and refetches
   * nothing — so the adapter may know a key before a query claims it. The
   * reverse order is the one that breaks: a query that mounts against a key no
   * adapter arm maps is silently never invalidated, which is exactly the defect
   * this pair of entries fixes for the cove list.
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

export function coveConversationsQueryOptions(
  transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.coveConversations(coveId),
    queryFn: (): Promise<Conversation[]> =>
      runOperation(transport, coveConversationsOperation(coveId), unauthorized),
  };
}

export type CoveConversationMutations = Readonly<{
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

export function useCoveConversationMutations(
  transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel,
): CoveConversationMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: ({ text, idempotencyKey }: { text: string; idempotencyKey: string }) =>
      runOperation(transport, createCoveConversationOperation(coveId, text, idempotencyKey), unauthorized),
    onSuccess: (row) => {
      /* Written through as well as invalidated: the drawer switches to this row
         in the same tick, and a list that does not contain it yet would render
         the panel with no active row until the refetch lands. A replayed key
         answers with a row that is already there, so the write is by id. */
      client.setQueryData<Conversation[]>(queryKeys.coveConversations(coveId), (current) => {
        const rows = current ?? [];
        return rows.some((candidate) => candidate.id === row.id)
          ? rows.map((candidate) => candidate.id === row.id ? row : candidate)
          : [...rows, row];
      });
      void client.invalidateQueries({ queryKey: queryKeys.coveConversations(coveId) });
    },
  });
  return {
    create: (text, idempotencyKey) => create.mutateAsync({ text, idempotencyKey }),
    refresh: () => client.fetchQuery({
      ...coveConversationsQueryOptions(transport, coveId, unauthorized),
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

export function coveListQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.coves(),
    queryFn: async (): Promise<Cove[]> =>
      sortedCoves(visibleCoves((await runOperation(transport, coveListOperation(), unauthorized)).map(toCove))),
  };
}

export function waveOverlaysQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.overlaysByKind('wave'),
    queryFn: (): Promise<OverlayWire[]> => runOperation(transport, overlaysByKindOperation('wave'), unauthorized),
  };
}

export function wavesInCoveQueryOptions(transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.wavesInCove(coveId),
    queryFn: async (): Promise<Wave[]> =>
      (await runOperation(transport, wavesInCoveOperation(coveId), unauthorized)).map((wire) => toWave(wire)),
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

export function settingsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.settings(),
    queryFn: (): Promise<SettingsBag> => runOperation(transport, settingsOperation(), unauthorized),
  };
}

/**
 * #1209 — templates for the New wave dialog.
 *
 * `retry: false` and a plain failure are the point: the dialog degrades to
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
  /** Never `undefined`: pending and failed both read as "Blank only". */
  templates: WaveTemplate[];
  /** A notice for the dialog, not a blocker. `null` while pending. */
  error: string | null;
}>;

/**
 * The New wave dialog's template list, collapsed to the two things the dialog
 * can act on. A hook and not raw `useQuery` at the call site so the shell's
 * contract tests keep mocking exactly one module (`providers/queries`) —
 * the same shape `useWorkspace` and the mutation hooks already have.
 */
export function useWaveTemplates(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): WaveTemplates {
  const query = useQuery(waveTemplatesQueryOptions(transport, unauthorized));
  return {
    templates: query.data ?? [],
    error: query.isError ? 'Could not load templates.' : null,
  };
}

export type Workspace = Readonly<{
  coves: Cove[];
  wavesByCove: ReadonlyMap<string, Wave[]>;
  waves: Wave[];
  covesLoading: boolean;
  overlaysLoading: boolean;
  covesError: Error | null;
  overlaysError: Error | null;
  waveErrorsByCove: ReadonlyMap<string, Error>;
  wavesLoadingByCove: ReadonlyMap<string, boolean>;
  retryCoves: () => void;
  retryOverlays: () => void;
  retryWaves: (coveId: string) => void;
}>;

/**
 * INV-APP-084 — the cove → waves fan-out is a page-level `useQueries`, never a
 * route loader await. One slow cove must not block the calendar; each cove's
 * list also stays its own cache entry, so a wave moving between coves
 * invalidates two lists instead of the whole workspace.
 *
 * The workspace-wide wave-overlay read is folded in here so every surface —
 * sidebar buckets, Today's counters, cove lists — sees the same
 * `anyCardNeedsInput` / progress / eta / now, rather than only the wave the
 * user happens to have open.
 */
export function useWorkspace(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Workspace {
  const covesQuery = useQuery(coveListQueryOptions(transport, unauthorized));
  const overlaysQuery = useQuery(waveOverlaysQueryOptions(transport, unauthorized));
  const coves = covesQuery.data ?? [];
  const overlays = overlaysQuery.data ?? [];
  const waveQueries = useQueries({
    queries: coves.map((cove) => wavesInCoveQueryOptions(transport, cove.id, unauthorized)),
  });
  const wavesByCove = new Map<string, Wave[]>();
  const waveErrorsByCove = new Map<string, Error>();
  const wavesLoadingByCove = new Map<string, boolean>();
  const waves: Wave[] = [];
  for (const [index, cove] of coves.entries()) {
    const query = waveQueries[index];
    wavesLoadingByCove.set(cove.id, query?.isLoading ?? false);
    if (query?.error instanceof Error) waveErrorsByCove.set(cove.id, query.error);
    if (query?.data !== undefined) {
      const rows = query.data.map((wave) => ({ ...wave, ...waveActivityFrom(wave.id, overlays) }));
      wavesByCove.set(cove.id, rows);
      waves.push(...rows);
    }
  }
  return {
    coves, wavesByCove, waves, covesLoading: covesQuery.isLoading,
    overlaysLoading: overlaysQuery.isLoading,
    covesError: covesQuery.error instanceof Error ? covesQuery.error : null,
    overlaysError: overlaysQuery.error instanceof Error ? overlaysQuery.error : null,
    waveErrorsByCove,
    wavesLoadingByCove,
    retryCoves: () => { void covesQuery.refetch(); },
    retryOverlays: () => { void overlaysQuery.refetch(); },
    retryWaves: (coveId) => {
      const index = coves.findIndex((cove) => cove.id === coveId);
      if (index >= 0) void waveQueries[index]?.refetch();
    },
  };
}

/** Route loaders prime only this one list; see INV-APP-084 above. */
export function prefetchCoveList(client: QueryClient, transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Promise<Cove[]> {
  return client.ensureQueryData(coveListQueryOptions(transport, unauthorized));
}

// ---------- mutations ----------
//
// Every mutation invalidates. A mutation may additionally write its response
// through to the cache first, but only when that response *is* the new cache
// value (an id-keyed row the server just returned) and the very next render
// needs it — see `useCoveConversationMutations`, where the drawer switches to
// the new row in the same tick. The invalidation still follows and reconciles;
// a write-through that guessed, or that stood in for one, would only widen the
// window in which the cache and the server disagree.

export type CoveMutations = Readonly<{
  create: (body: NewCoveBody) => Promise<Cove>;
  rename: (coveId: string, body: CovePatchBody) => Promise<Cove>;
  remove: (coveId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useCoveMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): CoveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewCoveBody) => runOperation(transport, createCoveOperation(body), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  const rename = useMutation({
    mutationFn: ({ coveId, body }: { coveId: string; body: CovePatchBody }) =>
      runOperation(transport, updateCoveOperation(coveId, body), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  const remove = useMutation({
    mutationFn: ({ coveId, signal }: { coveId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteCoveOperation(coveId), signal }, unauthorized),
    onSuccess: (_result, { coveId }) => {
      // The cove is gone; its wave list can never resolve again, so drop it
      // instead of leaving a permanently-stale entry behind.
      client.removeQueries({ queryKey: queryKeys.wavesInCove(coveId) });
    },
    // Abort only ends the client wait: the server may already have committed.
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  return {
    create: async (body) => toCove(await create.mutateAsync(body)),
    rename: async (coveId, body) => toCove(await rename.mutateAsync({ coveId, body })),
    remove: async (coveId, signal) => { await remove.mutateAsync({ coveId, signal }); },
  };
}

export type WaveMutations = Readonly<{
  create: (body: NewWaveBody) => Promise<Wave>;
  patch: (waveId: string, coveId: string, body: WavePatchBody) => Promise<Wave>;
  setPinned: (waveId: string, coveId: string, pinned: boolean, nowMs: number) => Promise<Wave>;
  createTerminal: (waveId: string, body: NewTerminalCardBody) => Promise<CardWire>;
  remove: (waveId: string, coveId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useWaveMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): WaveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewWaveBody) => runOperation(transport, createWaveOperation(body), unauthorized),
    onSuccess: (wave, body) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) });
      // Explicit `attach_folder` still mints a cove_folders row. Drop any
      // cached list so a later folders read cannot serve a stale empty array.
      if (body.attach_folder) {
        client.removeQueries({ queryKey: queryKeys.coveFolders(body.cove_id) });
      }
    },
  });
  const patch = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; coveId: string; body: WavePatchBody }) =>
      runOperation(transport, updateWaveOperation(waveId, body), unauthorized),
    onSuccess: (wave, variables) => {
      // Prefer the cove the server just reported: a patch can move the wave.
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) });
      if (wave.cove_id !== variables.coveId) {
        void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(variables.coveId) });
      }
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
  });
  const remove = useMutation({
    mutationFn: ({ waveId, signal }: { waveId: string; coveId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteWaveOperation(waveId), signal }, unauthorized),
    onSuccess: (_result, variables) => {
      client.removeQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
    // Reconcile both list-derived surfaces even if abort raced a committed DELETE.
    onSettled: (_result, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(variables.coveId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
    },
  });
  const createTerminal = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; body: NewTerminalCardBody }) =>
      runOperation(transport, createTerminalCardOperation(waveId, body), unauthorized),
    onSuccess: (card) => {
      client.setQueryData(queryKeys.waveDetail(card.wave_id), (previous: WaveDetailWire | undefined) => {
        if (previous === undefined) return previous;
        if (previous.cards.some((existing) => existing.id === card.id)) return previous;
        return { ...previous, cards: [...previous.cards, card] };
      });
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(card.wave_id) });
    },
  });
  const patchWave = async (waveId: string, coveId: string, body: WavePatchBody) =>
    toWave(await patch.mutateAsync({ waveId, coveId, body }));
  return {
    create: async (body) => toWave(await create.mutateAsync(body)),
    patch: patchWave,
    createTerminal: async (waveId, body) => createTerminal.mutateAsync({ waveId, body }),
    // `pinned_at` is both the flag and the ordering key, so unpinning is a
    // null write rather than a delete of some separate row.
    setPinned: (waveId, coveId, pinned, nowMs) =>
      patchWave(waveId, coveId, { pinned_at: pinned ? nowMs : null }),
    remove: async (waveId, coveId, signal) => { await remove.mutateAsync({ waveId, coveId, signal }); },
  };
}

export function useSettingsMutation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): (patch: SettingsPatch) => Promise<SettingsBag> {
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: (patch: SettingsPatch) => runOperation(transport, putSettingsOperation(patch), unauthorized),
    // PUT answers with the full bag, so writing it through avoids a refetch
    // that would briefly render the pre-save values.
    onSuccess: (bag) => { client.setQueryData(queryKeys.settings(), bag); },
  });
  return (patch) => save.mutateAsync(patch);
}
