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
  asFolderConflict, coveListOperation, createCoveOperation, deleteCoveOperation,
  sortedCoves, toCove, updateCoveOperation, visibleCoves,
  type Cove, type CovePatchBody, type FolderConflict, type NewCoveBody,
} from '../../../../core/domain/cove.ts';
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
  todayLaunchpadOperation, type TodayLaunchpadWire,
} from '../../../../core/domain/today.ts';
import {
  createCardOperation, createCodexCardOperation, createTerminalCardOperation, createWaveOperation,
  deleteCardOperation, deleteWaveOperation, overlaysByKindOperation, putWaveTemplateOperation, toWave,
  updateWaveOperation, waveActivityFrom, waveDetailOperation, waveTemplatesOperation, wavesInCoveOperation,
  type CardWire, type NewCardBody, type NewCodexCardBody, type NewTerminalCardBody, type NewWaveBody,
  type OverlayWire, type Wave, type WaveDetailWire, type WavePatchBody, type WaveTemplate,
  type WaveTemplateGoalEdit,
} from '../../../../core/domain/wave.ts';
import {
  HARNESS_ITEMS_PAGE_LIMIT, harnessItemsOperation, interruptSpecOperation, sendSpecInputOperation,
  specRunOperation, coveConversationsOperation, createCoveConversationOperation,
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
 * know this class exists; the decode and the wording are `core/domain/cove.ts`.
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
  /* Settings › Plugins. Not reached by any event policy — see
     `pluginsQueryOptions` for why, and for what stands in for one. */
  plugins: () => ['plugins'] as const,
  /* #1209 — the New wave picker's list. Not invalidated by any event: the
     kernel's template keys are compile-time constants and the only thing that
     can move under them is a plugin starting or stopping, which changes an
     `input_schema` the dialog reads when it opens. */
  waveTemplates: () => ['wave-templates'] as const,
  /* #1253 §5.1 — the Today launchpad resolve. One entry, not keyed by wave:
     the kernel's partial unique index makes `purpose = 'launchpad'` a
     singleton, and the id is what this query is fetching.

     TODO(#1253 PR2): no event invalidates this key, and `wave.report_edited`
     does not invalidate `['wave', id]` either — so once `POST
     /api/today/summary` exists, a successful summary will change nothing on
     screen until a reload. Both keys need adding to that policy in PR2. It is
     inert in PR1: nothing here can change either value. */
  todayLaunchpad: () => ['today-launchpad'] as const,
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
 * The wave twin of `useCoveConversationMutations`, with the same two doors and
 * the same reasons for them.
 *
 * Not shared with the cove version through a parameterised helper: the two
 * endpoints have different paths, different derived namespaces and different
 * cache keys, and the only thing a shared helper would save is four lines of
 * plumbing at the cost of making "which list does a create write through to?"
 * a question you have to trace.
 */
export function useWaveConversationMutations(
  transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel,
): CoveConversationMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: ({ text, idempotencyKey }: { text: string; idempotencyKey: string }) =>
      runOperation(transport, createWaveConversationOperation(waveId, text, idempotencyKey), unauthorized),
    onSuccess: (row) => {
      /* Written through as well as invalidated, for the same reason the cove
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

/**
 * #1253 §5.1 — the Today page load's resolve. A pure read: it never
 * bootstraps, so the first paint of Today does not depend on codex being up.
 *
 * **404 is data, every other failure is an error, and the split is
 * load-bearing** (INV-TODAYDOC-002). "There is no launchpad yet" is the empty
 * state and arrives as `null`; a 500, a timeout or a schema mismatch must
 * reach the reader as an error box. Folding them together — returning `null`
 * for any failure — would make an unreachable server look exactly like a fresh
 * workspace, which is the silent-degradation this invariant exists to forbid.
 */
export function todayLaunchpadQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.todayLaunchpad(),
    queryFn: async (): Promise<TodayLaunchpadWire | null> => {
      const result = await performApiRequest(transport, todayLaunchpadOperation(), unauthorized);
      if (result.status === 'ready') return result.value;
      /* Duck-typed on `status` rather than on the failure kind, the same way
         the Today terminal's resolve chain is specified to read a 404
         (INV-TODAYTERM-006): transport and decode failures carry no status and
         must not be mistaken for "nothing there". */
      if ('status' in result.error && result.error.status === 404) return null;
      throw new ApiError(result.error);
    },
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
  /** Never `undefined`: for the New wave dialog, pending and failed both read
   *  as "Blank only". */
  templates: WaveTemplate[];
  /** A notice for the dialog, not a blocker. `null` while pending. */
  error: string | null;
  /** `false` while the first read is still in flight — see `useWaveTemplates`. */
  loaded: boolean;
  refetch: () => void;
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
    // #1230 — the Settings editor reads this list too, and there `[]` must not
    // be readable as "loaded and empty": rendering an empty form for a template
    // whose read has not landed is INV-SETTINGS-002's defect in another place.
    //
    // A **failed** read is not loaded either. The first cut wrote
    // `!query.isPending`, which is true once a read has errored — so a dead
    // server produced `loaded: true` with `templates: []`, and the editor said
    // "No template named small-change" instead of reporting the failure.
    loaded: !query.isPending && !query.isError,
    refetch: () => { void query.refetch(); },
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
  createCodex: (waveId: string, body: NewCodexCardBody) => Promise<CardWire>;
  createCard: (waveId: string, body: NewCardBody) => Promise<CardWire>;
  removeCard: (waveId: string, cardId: string, signal?: AbortSignal) => Promise<void>;
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
  const patchWave = async (waveId: string, coveId: string, body: WavePatchBody) =>
    toWave(await patch.mutateAsync({ waveId, coveId, body }));
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
    setPinned: (waveId, coveId, pinned, nowMs) =>
      patchWave(waveId, coveId, { pinned_at: pinned ? nowMs : null }),
    remove: async (waveId, coveId, signal) => { await remove.mutateAsync({ waveId, coveId, signal }); },
  };
}

/**
 * Saving a template invalidates the template list, which since #1230 is the
 * single read for both the New wave picker and the Settings editor. One
 * authority, one invalidation.
 */
export function useWaveTemplateMutation(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): (save: {
  id: string;
  title: string;
  edits: readonly WaveTemplateGoalEdit[];
  appends: readonly WaveTemplateGoalEdit[];
}) => Promise<WaveTemplate> {
  const client = useQueryClient();
  const mutation = useMutation({
    mutationFn: (save: {
      id: string;
      title: string;
      edits: readonly WaveTemplateGoalEdit[];
      appends: readonly WaveTemplateGoalEdit[];
    }) => runOperation(
      transport,
      putWaveTemplateOperation(save.id, { title: save.title, edits: save.edits, appends: save.appends }),
      unauthorized,
    ),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.waveTemplates() }); },
  });
  return (save) => mutation.mutateAsync(save);
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
