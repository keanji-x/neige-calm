import { retireAreaQueries } from './area-query-lifecycle.ts';
import { queryKeys } from './query-keys.ts';
export { queryKeys } from './query-keys.ts';
import { conversationItemsOperation, conversationRunOperation, keepConversationInputOperation } from '../../../../core/domain/conversation-display.ts';
import { conversationLiveOperation } from '../../../../core/domain/conversation-live.ts';
import type { ConversationCursor, ConversationLiveReplies, HarnessLiveReplies } from '../../../../core/api/generated/wire.ts';
// Query and mutation wiring shared by app/router, app/shell and every feature slice. It lives under
// app/providers because the router renders the shell and both need the same reads (`no-circular`).

import { conversationInputMetadata } from '../../../../core/events/invalidation-plan.ts';
import {
  useQueries, useQuery, useQueryClient, type QueryClient, type UseQueryResult,
} from '@tanstack/react-query';
import { hasReadFailure } from './query-read-feedback.ts';
import { useMemo, useRef } from 'react';
import { admitTransport, useRecoveryMutation } from './recovery-mutation.ts';
import { z } from 'zod';
import type { AgentProvider } from '../../../../core/api/generated/wire.ts';

import { performApiRequest } from '../../../../core/api/client.ts';
import type { ApiOperation, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  asFolderConflict, areaListOperation, areaCreationCapabilityOperation, createAreaOperation, deleteAreaOperation,
  newestArea, sortedAreas, toArea, updateAreaOperation, visibleAreas,
  type Area, type AreaPatchBody, type FolderConflict, type NewAreaBody,
} from '../../../../core/domain/area.ts';
import {
  deriveReportTasks, hasLiveTaskRun, trackBacklinksOperation, trackPreviewsOperation, trackTaskVerdictsOperation,
  type ReportBlock, type TaskVerdict, type TrackBacklinks, type TrackPreviews,
} from '../../../../core/domain/report.ts';
import {
  staleRevBodySchema, trackReportSeriesOperation, type ResolvedSeries, type SeriesDetail,
} from '../../../../core/domain/report-series.ts';
import { trackSourceOperation, type TrackSourceDetail } from '../../../../core/domain/report-source.ts';
import { dismissActivityItemOperation } from '../../../../core/domain/activity.ts';
import { answerAskOperation } from '../../../../core/domain/ask.ts';
import { probeFailureText, readErrorText } from '../../../../core/domain/read-failure.ts';
import {
  checkConnectorOperation, CONNECTOR_CHECK_TEXT, type ConnectorCheckResult,
  installConnectorOperation, installLocalPathOperation, patchPluginConfigOperation,
  pluginDetailOperation, pluginsOperation, reloadPluginOperation, setPluginEnabledOperation,
  uninstallPluginOperation, PLUGIN_TOGGLE_FAILURES, PLUGIN_TOGGLE_TEXT, PLUGIN_UNINSTALL_FAILURES,
  type ConnectorInstallDraft, type InstalledPlugin, type PluginConfigApplyResult,
  type PluginConfigSaveResult, type PluginConfigValue, type PluginDetail, type PluginListItem,
  type PluginRestartFacts,
} from '../../../../core/domain/plugins.ts';
import {
  putSettingsOperation, settingsOperation, type SettingsBag, type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import {
  todayLaunchpadEnsureOperation, todayLaunchpadOperation, todayReportResetOperation,
  type TodayLaunchpadEnsureWire, type TodayLaunchpadWire, type TodayReportResetWire,
} from '../../../../core/domain/today.ts';
import {
  createCardOperation, createClaudeCardOperation, createCodexCardOperation, createTerminalCardOperation, setTerminalWorkbenchOperation, createTrackOperation,
  createTrackRecipeOperation, deleteCardOperation, deleteTrackOperation, deleteTrackRecipeOperation,
  overlaysByKindOperation, recipeSaveAttempts, recipeSaveLanded, toTrack, updateTrackOperation, updateTrackRecipeOperation,
  NEUTRAL_ACTIVITY, sortAreaTracksByRecent, trackActivitiesFrom, trackDetailOperation, trackRecipeOperation, trackRecipesOperation, trackTemplatesOperation,
  tracksInAreaOperation,
  type CardWire, type NewCardBody, type NewClaudeCardBody, type NewCodexCardBody, type NewTerminalCardBody,
  type NewTrackBody, type OverlayWire,
  type Track, type TrackDetailWire, type TrackPatchBody, type TrackRecipe, type TrackTemplate,
} from '../../../../core/domain/track.ts';
import {
  compactPlannerOperation, interruptPlannerOperation, sendPlannerInputOperation,
  createTrackConversationOperation, trackConversationsOperation, setConversationArchiveOperation,
  createSerialWriter, deletePlannerInputOperation, editPlannerInputOperation, steerPlannerInputOperation,
  modelCatalogOperation, plannerQueueWriteFailure, setPlannerModelOperation,
  uploadPlannerAttachmentOperation, PLANNER_ATTACHMENT_FAILURES, UPLOAD_TEXT,
  type Conversation, type ModelCatalogScope, type ModelSelection, type ModelSelectionResult,
  type PlannerQueueWriteOutcome, type PlannerQueueEditOutcome, type PendingQueueEntry, type PlannerInputMutation,
} from '../../../../core/domain/conversation.ts';
import { harnessLiveOperation } from '../../../../core/domain/conversation-live.ts';
import { setPlannerPermissionModeOperation } from '../../../../core/domain/planner-permission-mode.ts';
import { restartPlannerOperation } from '../../../../core/domain/conversation-restart.ts';
import {
  ApiError, classifyFailure, DELETE_FAILURES, DELETE_TEXT, refusalText, writeFailureOf, writeFailureText,
} from '../../../../core/domain/failure-class.ts';
import { useState } from '../../ui/state/public.ts';
import { useCommittedCallback } from '../../ui/state/committed-callback.ts';
import type { ServerVersionInfo } from './public.tsx';
import type {
  AskAnswer, PlannerPermissionMode, SetPlannerPermissionModeResponse, UploadAttachmentResponse,
} from '../../../../core/api/generated/wire.ts';
import { cancelThenInvalidate } from '../events/query-refresh.ts';

/** A failed capability preflight guarantees no Area POST was submitted. */
export class AreaCreatePreflightError extends Error {
  constructor(message: string) {
    super(`${message} No new create request was sent.`);
    this.name = 'AreaCreatePreflightError';
  }
}

/** The structured folder clash inside a rejected mutation, or `null`; the decode and wording are `core/domain/area.ts`. */
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
  const checkpoint = transport.recovery?.checkpoint();
  const result = await performApiRequest(transport, operation, unauthorized);
  checkpoint?.();
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

export type TranscriptPosition = Readonly<{ kind: 'latest' } | { kind: 'before'; cursor: ConversationCursor }>;

export function conversationHistoryQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: [...queryKeys.harnessItems(cardId),'conversation_v1'],
    queryFn: async ({ position, signal }: { position: TranscriptPosition; signal?: AbortSignal }) => {
      const cursor = position.kind === 'latest' ? null : position.cursor;
      const page = await runOperation(transport, { ...conversationItemsOperation(cardId, cursor), signal }, unauthorized);
      if (cursor !== null && (page.items.some(item => item.cursor.position >= cursor.position)
        || (page.next_cursor !== null && page.next_cursor.position >= cursor.position))) {
        throw new Error('Conversation history did not advance to an earlier page.');
      }
      return page;
    },
  };
}

export function harnessLiveQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel, canonical:boolean) {
  return {
    queryKey: [...queryKeys.harnessLive(cardId),canonical ? 'conversation_v1':'historical'],
    queryFn: async ({ signal }: { signal: AbortSignal }):Promise<ConversationLiveReplies|HarnessLiveReplies> => canonical
      ? runOperation(transport,{...conversationLiveOperation(cardId),signal},unauthorized)
      : runOperation(transport,{...harnessLiveOperation(cardId),signal},unauthorized),
  };
}

export function plannerRunQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: [...queryKeys.plannerRun(cardId),'conversation_v1'],
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      runOperation(transport, { ...conversationRunOperation(cardId), signal }, unauthorized),
  };
}

/** No `staleTime` of its own: codex keeps a 300 s disk cache behind this. */
export function modelCatalogQueryOptions(transport: ApiTransportPort, scope: ModelCatalogScope, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.modelCatalog(scope),
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      runOperation(transport, { ...modelCatalogOperation(scope), signal }, unauthorized),
  };
}

/**
 * One serialised settings writer per mounted card, held across renders: a fresh writer each render would have nothing
 * in flight to serialise against. Re-created when the card changes. Each call is admitted at the press.
 */
function useCardSettingWriter<A, R>(
  transport: ApiTransportPort, cardId: string, operation: (cardId: string, value: A) => ApiOperation<R>,
  unauthorized: UnauthorizedChannel,
): (value: A) => Promise<R> {
  const writer = useRef<{ cardId: string; write: (value: A) => Promise<R> } | null>(null);
  if (writer.current === null || writer.current.cardId !== cardId) {
    const serial = createSerialWriter((intent: { value: A; transport: ApiTransportPort }) =>
      runOperation(intent.transport, operation(cardId, intent.value), unauthorized));
    writer.current = {
      cardId,
      write: (value: A) => {
        try { return serial({ value, transport: admitTransport(transport) }); }
        catch (error) { return Promise.reject(error instanceof Error ? error : new Error('连接尚未恢复')); }
      },
    };
  }
  return writer.current.write;
}

export function usePlannerMutations(transport: ApiTransportPort, cardId: string, trackId: string | null, unauthorized: UnauthorizedChannel) {
  const client = useQueryClient();
  const setModelWrite = useCardSettingWriter<ModelSelection, ModelSelectionResult>(
    transport, cardId, setPlannerModelOperation, unauthorized);
  const setPermissionModeWrite = useCardSettingWriter<PlannerPermissionMode, SetPlannerPermissionModeResponse>(
    transport, cardId, setPlannerPermissionModeOperation, unauthorized);
  const transcriptKey = queryKeys.harnessItems(cardId);
  const refreshTranscript = () => client.invalidateQueries({ queryKey: transcriptKey });
  const refreshAfter = <T,>(result: T): T => {
    /* A 200 from the write is the acknowledgement; refetch is reconciliation and its failure must not
           turn an accepted send into a failed write. Not awaited: a hung read must not retain the send lease. */
    void Promise.all([
      refreshTranscript(),
      cancelThenInvalidate(client, queryKeys.initialInputPrefix()),
      client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }),
    ]).catch(() => undefined);
    return result;
  };
  const refreshInputMetadata = (admitted: ApiTransportPort = transport, error?: unknown) => {
    let canRead = !(error instanceof ApiError && error.failure.kind === 'unauthorized');
    try { admitted.recovery?.capture(); } catch { canRead = false; }
    for (const queryKey of conversationInputMetadata(trackId)) {
      // Recovery owns read admission and resumption. Retain stale metadata while denied;
      // do not turn a refused/unknown write into a business read outside that permission.
      if (canRead) void cancelThenInvalidate(client, queryKey);
      else void client.invalidateQueries({ queryKey, refetchType: 'none' }).catch(() => undefined);
    }
  };
  /* As `refreshAfter`, but each read is cancelled first: the query layer would otherwise hand back a read still in
     flight from before the answer; input acceptance must reconcile against actual reads started after its ACK. */
  const refreshAfterInput = <T,>(result: T, resetTranscript: boolean): T => {
    if (resetTranscript) void client.resetQueries({ queryKey: transcriptKey }).catch(() => undefined);
    else void cancelThenInvalidate(client, transcriptKey);
    for (const queryKey of [queryKeys.plannerRun(cardId), queryKeys.initialInputPrefix()]) void cancelThenInvalidate(client, queryKey);
    refreshInputMetadata();
    return result;
  };
  const refreshAfterQueueWrite = <T extends Readonly<{ kind: string }>>(result: T): T => {
    if (result.kind === 'done') return refreshAfterInput(result, false);
    refreshInputMetadata();
    return refreshAfter(result);
  };
  return {
    keep: (args: import('../../../../core/api/generated/wire.ts').KeepConversationInputRequest, key: string) =>
      Promise.resolve().then(() => admitTransport(transport))
        .then(admitted => runOperation(admitted, keepConversationInputOperation(cardId, args, key), unauthorized))
        .then(result => refreshAfterInput(result, false), (error: unknown) => {
          refreshAfterInput(undefined, false);
          throw error;
        }),
    /* `admitted` is the caller's: a keyed send is admitted at the press, and each retry is admitted again.
       `answered` runs when the 200 is in hand, before the refresh starts its reads. */
    send: (text: string, attachments: readonly string[], idempotencyKey: string, replacesTurn: string | null,
      admitted: ApiTransportPort, answered: () => void) =>
      runOperation(admitted, sendPlannerInputOperation(cardId, text, attachments, idempotencyKey, replacesTurn), unauthorized)
        .then((sent) => { answered(); return refreshAfterInput(sent, replacesTurn !== null); }, (error: unknown) => {
          // A refusal may expose a stale revision; a lost answer may follow a durable write.
          refreshInputMetadata(admitted, error);
          throw error;
        }),
    /* Admitted at the press, as every chat write is: a stop that cannot leave the browser is not sent (#2068). */
    compact: () => Promise.resolve().then(() => admitTransport(transport))
      .then((admitted) => runOperation(admitted, compactPlannerOperation(cardId), unauthorized)).then(refreshAfter),
    interrupt: () => Promise.resolve().then(() => admitTransport(transport))
      .then((admitted) => runOperation(admitted, interruptPlannerOperation(cardId), unauthorized)).then(refreshAfter),
    /* Settles once the run read started after the answer has landed, so what follows reads the new session, not the
       wedge it replaced (#2192). A failure refreshes too: one whose outcome is unknown may have started a session. */
    restart: () => {
      const reread = () => {
        void refreshTranscript().catch(() => undefined);
        return cancelThenInvalidate(client, queryKeys.plannerRun(cardId));
      };
      return Promise.resolve().then(() => admitTransport(transport))
        .then((admitted) => runOperation(admitted, restartPlannerOperation(cardId), unauthorized))
        .then(async (restarted) => { await reread(); return restarted; }, async (error: unknown) => { await reread(); throw error; });
    },
    /* Resolves rather than rejects on a refusal: a lost compare-and-swap and a drained entry are answers
     * the reader has to be shown. The refresh runs on every path — a 409 proves the cached page is behind. */
    editQueued: (entry: PendingQueueEntry, text: string, answered: (saved: PlannerInputMutation & Readonly<{ text: string }>) => void): Promise<PlannerQueueEditOutcome> =>
      runOperation(transport, editPlannerInputOperation(cardId, entry.entry_id, text, entry.rev), unauthorized)
        .then((saved): PlannerQueueEditOutcome => { answered(saved); return { kind: 'done', entry: { ...entry, text: saved.text, rev: saved.rev } }; })
        .catch((error: unknown) => plannerQueueWriteFailure(
          error instanceof ApiError ? error.failure : null, 'Could not edit the queued message.',
        )).then(refreshAfterQueueWrite),
    deleteQueued: (entryId: string, ifEntryRev: number, answered: (removed: PlannerInputMutation) => void): Promise<PlannerQueueWriteOutcome> =>
      runOperation(transport, deletePlannerInputOperation(cardId, entryId, ifEntryRev), unauthorized)
        .then((removed): PlannerQueueWriteOutcome => { answered(removed); return { kind: 'done' }; })
        .catch((error: unknown) => plannerQueueWriteFailure(
          error instanceof ApiError ? error.failure : null, 'Could not remove the queued message.',
        ))
        .then(refreshAfterQueueWrite),
    /* Same classification as the delete, plus the steer's own 409s (`not_running`, `unanswered`); on a
     * 200 the refresh is what makes the transcript pick up the row the kernel wrote. */
    steerQueued: (entryId: string, ifEntryRev: number, answered: () => void): Promise<PlannerQueueWriteOutcome> =>
      runOperation(transport, steerPlannerInputOperation(cardId, entryId, ifEntryRev), unauthorized)
        .then((): PlannerQueueWriteOutcome => { answered(); return { kind: 'done' }; })
        .catch((error: unknown) => plannerQueueWriteFailure(
          error instanceof ApiError ? error.failure : null, 'Could not send the queued message now.',
        ))
        .then(refreshAfterQueueWrite),
    /* The stored selection is read back from `planner-run`; the catalog goes with it because the default
     * this card follows is part of that answer. */
    setModel: (selection: ModelSelection): Promise<ModelSelectionResult> =>
      setModelWrite(selection)
        .then((result) => {
          void client.invalidateQueries({ queryKey: queryKeys.modelCatalog({ kind: 'card', cardId }) })
            .catch(() => undefined);
          return refreshAfter(result);
        }),
    /* The stored mode is read back from `planner-run`, as the model is. */
    setPermissionMode: (mode: PlannerPermissionMode): Promise<SetPlannerPermissionModeResponse> =>
      setPermissionModeWrite(mode).then(refreshAfter),
    /* No `refreshAfter`: an upload changes nothing any query holds until a send names it. Every failure, the
     * admission's included, rejects with the sentence the strip shows, read against the route's table. */
    uploadAttachment: async (readBytes: () => Promise<Uint8Array>, contentType: string) => {
      const failed = (error: unknown) => new Error(
        refusalText(writeFailureOf(error), PLANNER_ATTACHMENT_FAILURES, UPLOAD_TEXT.refused) ?? UPLOAD_TEXT.unknown);
      let admitted: ApiTransportPort;
      let uploaded: UploadAttachmentResponse;
      try {
        admitted = admitTransport(transport);
        const bytes = await readBytes();
        uploaded = await runOperation(admitted, uploadPlannerAttachmentOperation(cardId, bytes, contentType), unauthorized);
      } catch (error) { throw failed(error); }
      // Keep admission attached to the value until the feature actually consumes it.
      return () => {
        try { admitted.recovery?.checkpoint()(); } catch (error) { throw failed(error); }
        return uploaded;
      };
    },
  };
}

import { initialInputObservationOperation } from '../../../../core/domain/conversation-initial-input.ts';
import type { ConversationInitialInputObservation } from '../../../../core/api/generated/wire.ts';
import type { SideConversation, CreatedConversation } from '../../../../core/domain/conversation.ts';

export type ConversationMutations = Readonly<{
  archive: (cardId: string, archived: boolean) => Promise<void>;
  /** The key identifies the DRAFT, not the attempt: a retry after a timeout must reuse it, or it mints a second conversation. */
  create: (text: string, idempotencyKey: string, provider: AgentProvider, selection: ModelSelection, side: SideConversation | undefined,
    answered: (result: CreatedConversation) => void) => Promise<CreatedConversation>;
  inspect: (cardId: string, idempotencyKey: string) => Promise<ConversationInitialInputObservation>;
  /** Re-read the list and hand back what it now holds. */
  refresh: (beforePublish: (rows: readonly Conversation[]) => void) => Promise<Conversation[]>;
}>;

/** One track's assistant conversations; the event bridge already maps this key, so the list is live the moment it mounts. */
export function trackConversationsQueryOptions(
  transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.trackConversations(trackId),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<Conversation[]> =>
      runOperation(transport, { ...trackConversationsOperation(trackId), signal }, unauthorized),
  };
}

/** Creates and refreshes one Track's assistant-conversation list. */
export function useTrackConversationMutations(
  transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel,
): ConversationMutations {
  const client = useQueryClient();
  const create = useRecoveryMutation(transport, {
    mutationFn: ({ text, idempotencyKey, provider, selection, side }: { text: string; idempotencyKey: string; provider: AgentProvider; selection: ModelSelection; side?: SideConversation; answered: (result: CreatedConversation) => void }, transport: ApiTransportPort) =>
      runOperation(transport, createTrackConversationOperation(trackId, text, idempotencyKey, provider, selection, side), unauthorized),
    onSuccess: (result, { answered }) => {
      answered(result);
      const row = result.conversation;
      /* Written through as well as invalidated: the drawer switches to this row in the same tick. */
      client.setQueryData<Conversation[]>(queryKeys.trackConversations(trackId), (current) => {
        const rows = current ?? [];
        return rows.some((candidate) => candidate.id === row.id)
          ? rows.map((candidate) => candidate.id === row.id ? row : candidate)
          : [...rows, row];
      });
      void client.invalidateQueries({ queryKey: queryKeys.trackConversations(trackId) });
      /* The track detail is where the CARDS panel, the grid and the Today open-request read cards from. */
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
    },
  });
  const archive = useRecoveryMutation(transport, {
    mutationFn: ({ cardId, archived }: { cardId: string; archived: boolean }, transport: ApiTransportPort) =>
      runOperation(transport, setConversationArchiveOperation(trackId, cardId, archived), unauthorized),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: queryKeys.trackConversations(trackId) });
    },
  });
  const changeArchive = useCommittedCallback(trackId,
    (cardId: string, archived: boolean) => archive.mutateAsync({ cardId, archived }));
  return {
    archive: changeArchive,
    create: (text, idempotencyKey, provider, selection, side, answered) => create.mutateAsync({ text, idempotencyKey, provider, selection, side, answered }),
    inspect: (cardId, key) => runOperation(transport, initialInputObservationOperation(trackId, cardId, key), unauthorized),
    refresh: async (beforePublish) => {
      const admitted = admitTransport(transport);
      const rows = await runOperation(admitted, trackConversationsOperation(trackId), unauthorized);
      admitted.recovery?.checkpoint()();
      // A manual cache write does not stop the list's older retryer from overwriting it.
      // Fence that existing read before receipt adoption and its canonical row publish together.
      await client.cancelQueries({ queryKey: queryKeys.trackConversations(trackId) });
      admitted.recovery?.checkpoint()();
      beforePublish(rows);
      client.setQueryData(queryKeys.trackConversations(trackId), rows);
      return rows;
    },
  };
}

const serverVersionSchema = z.object({
  assistantProviders: z.boolean().optional(),
  conversationCreateModel: z.boolean().optional(),
  conversationInitialInput: z.boolean().optional(),
  conversationSide: z.boolean().optional(),
  webCompatVersion: z.number(),
  minWebCompatVersion: z.number(),
  syncEventVersion: z.number(),
  dbInstanceId: z.string(),
  databaseId: z.string().optional(),
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
    // The shell owns an explicit Retry action; automatic retry backoff would
    // replace the known read error with an empty pending view on invalidation.
    retry: false,
    queryKey: queryKeys.areas(),
    queryFn: async ({ signal }: { signal: AbortSignal }): Promise<Area[]> =>
      sortedAreas(visibleAreas((await runOperation(transport, { ...areaListOperation(), signal }, unauthorized)).map(toArea))),
  };
}

export function trackOverlaysQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.overlaysByKind('track'),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<OverlayWire[]> =>
      runOperation(transport, { ...overlaysByKindOperation('track'), signal }, unauthorized),
  };
}

export function tracksInAreaQueryOptions(transport: ApiTransportPort, areaId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.tracksInArea(areaId),
    queryFn: async ({ signal }: { signal: AbortSignal }): Promise<Track[]> =>
      (await runOperation(transport, { ...tracksInAreaOperation(areaId), signal }, unauthorized)).map((wire) => toTrack(wire)),
    // Preserve Query's browser default of three retries for transient reads. Only typed
    // session/access/missing failures are terminal; runOperation still notifies on 401.
    retry: (failureCount: number, error: unknown) => {
      if (error instanceof ApiError) {
        const failure = error.failure;
        if (failure.kind === 'unauthorized'
          || (failure.kind === 'http' && (failure.status === 403 || failure.status === 404))) return false;
      }
      return failureCount < 3;
    },
  };
}

export function trackDetailQueryOptions(transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackDetail(trackId),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<TrackDetailWire> =>
      runOperation(transport, { ...trackDetailOperation(trackId), signal }, unauthorized),
  };
}

/** Who cites this track. Its own cache entry: backlinks are written by OTHER tracks, so they go stale on edits this track never sees. */
export function trackBacklinksQueryOptions(transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackBacklinks(trackId),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<TrackBacklinks> =>
      runOperation(transport, { ...trackBacklinksOperation(trackId), signal }, unauthorized),
  };
}

/** How often an open report with a `preview` block re-reads the registrations and their liveness. */
export const TRACK_PREVIEWS_POLL_MS = 5000;

/** The track's registered previews, each with a just-probed `live` bit. Polled: nothing announces a registration or a dev server coming up. */
export function trackPreviewsQueryOptions(transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackPreviews(trackId),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<TrackPreviews> =>
      runOperation(transport, { ...trackPreviewsOperation(trackId), signal }, unauthorized),
    refetchInterval: TRACK_PREVIEWS_POLL_MS,
  };
}

/**
 * The track's task verdicts. Its own cache entry: these change on every dispatch and gate result without
 * any card being written. Events alone do not keep it live — `scheduler::mark_running` stamps
 * `worker_card_id` without emitting anything — hence the timer.
 */
export function trackTaskVerdictsQueryOptions(
  transport: ApiTransportPort, trackId: string, unauthorized: UnauthorizedChannel,
  /* The declarations this report has: the timer is about the ROWS the panel draws, and a verdict is not a row. */
  blocks: readonly ReportBlock[] | null,
) {
  return {
    queryKey: queryKeys.trackReport(trackId),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<TaskVerdict[]> =>
      runOperation(transport, { ...trackTaskVerdictsOperation(trackId), signal }, unauthorized),
    /* 3 s is priced off the endpoint (p50 14.8 ms for a small report, 104 ms for a 24-task one): ~3.5% of
     * one core for one open track, only while something is running, and O(1) in the number of workers. */
    refetchInterval: taskVerdictsRefetchInterval(blocks),
  };
}

/** The live poll, once the read has landed at least once. */
const TASK_VERDICT_POLL_MS = 3000;
/** The recovery poll, while the read has never landed at all; far slower, since nothing is being tracked. */
const TASK_VERDICT_RECOVERY_POLL_MS = 15_000;
/**
 * Bounded: the report route also fails permanently (deleted track 404s, missing `track-report` card
 * 500s), and an unconditional poll on "no data" would hit a dead route forever.
 */
const TASK_VERDICT_RECOVERY_ATTEMPTS = 4;

/** With data: poll only while the panel holds a live row. With no data: a bounded recovery poll read off `errorUpdateCount` (`failureCount` resets on success).
 * Curried on the declarations because verdicts for deleted declarations (`blockId: ''`) produce no row, so a timer keyed on raw verdicts would refetch with nothing on screen. */
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
 * The Today page load's resolve, a pure read. `null` is data ("no launchpad yet"); any failure is an
 * error — folding them would make an unreachable server look like a fresh workspace.
 */
export function todayLaunchpadQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.todayLaunchpad(),
    queryFn: (): Promise<TodayLaunchpadWire | null> =>
      runOperation(transport, todayLaunchpadOperation(), unauthorized),
  };
}

/** Its failure is the caller's to read, through `LAUNCHPAD_ENSURE_FAILURES`. */
export type TodayLaunchpadEnsureMutation = Readonly<{
  ensure: () => Promise<TodayLaunchpadEnsureWire>;
  pending: boolean;
}>;

/** Materialise the launchpad after the reader presses `+`; the resolve is reconciled in the background because it owns the empty-state predicate. */
export function useTodayLaunchpadEnsureMutation(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
): TodayLaunchpadEnsureMutation {
  const client = useQueryClient();
  const mutation = useRecoveryMutation(transport, {
    mutationFn: (_variables: void, transport: ApiTransportPort): Promise<TodayLaunchpadEnsureWire> =>
      runOperation(transport, todayLaunchpadEnsureOperation(), unauthorized),
    onSettled: () => {
      void client.invalidateQueries({ queryKey: queryKeys.todayLaunchpad() });
    },
  });
  return { ensure: () => mutation.mutateAsync(), pending: mutation.isPending };
}

/**
 * Reset today's report; its failure is the caller's to read, through `REPORT_RESET_FAILURES`. The document's keys are
 * invalidated on settle: an unknown answer may still follow a reset that happened.
 */
export type TodayReportResetMutation = Readonly<{
  /** Awaitable so `useDeleteConfirm` can own the outcome. */
  reset: () => Promise<void>;
}>;

export function useTodayReportResetMutation(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
): TodayReportResetMutation {
  const client = useQueryClient();
  const mutation = useRecoveryMutation(transport, {
    mutationFn: (_variables: void, transport: ApiTransportPort): Promise<TodayReportResetWire> =>
      runOperation(transport, todayReportResetOperation(), unauthorized),
    onSettled: (reset) => {
      const trackId = reset?.track_id ?? client.getQueryData<TodayLaunchpadWire | null>(queryKeys.todayLaunchpad())?.track_id;
      void client.invalidateQueries({ queryKey: queryKeys.todayLaunchpad() });
      if (trackId !== undefined) void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
    },
  });
  return { reset: async () => { await mutation.mutateAsync(); } };
}

export function settingsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.settings(),
    queryFn: (): Promise<SettingsBag> => runOperation(transport, settingsOperation(), unauthorized),
    // Settings writes emit no event. Poll only while this query is observed so
    // another client's change reaches the open pane without a browser refresh.
    refetchInterval: 15_000,
  };
}

/** Templates for the new-track page. `retry: false` makes a failed roster visible instead of leaving either consumer spinning. */
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

/** Shared template roster for New Track and the Area editor, so loading/error semantics live in one place. */
export function useTrackTemplates(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): TrackTemplates {
  const query = useQuery(trackTemplatesQueryOptions(transport, unauthorized));
  return {
    templates: query.data ?? [],
    error: query.isError ? readErrorText(query.error, 'Could not load templates.') : null,
    // `[]` must not read as "loaded and empty", and a FAILED read is not loaded either: `!isPending` alone
    // is true once a read has errored, making a dead server look like a server with no templates.
    loaded: !query.isPending && !query.isError,
    refetch: () => { void query.refetch(); },
  };
}

/** The user's own recipes. `retry: false`: a failed read must degrade the picker to "built-ins only" rather than spin. */
export function trackRecipesQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.trackRecipes(),
    queryFn: (): Promise<TrackRecipe[]> => runOperation(transport, trackRecipesOperation(), unauthorized),
    retry: false,
  };
}

export type TrackRecipes = Readonly<{
  refreshing: boolean;
  refetch: () => void;
  /** Never `undefined`: for the picker, pending and failed both read as "no recipes of mine". */
  recipes: TrackRecipe[];
  /** A notice, not a blocker. Retained during a retry after the first failure. */
  error: string | null;
  /** `false` while the first read is in flight OR after it failed: "no recipes yet" is a claim about the server. */
  loaded: boolean;
}>;

export function useTrackRecipes(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): TrackRecipes {
  const query = useQuery(trackRecipesQueryOptions(transport, unauthorized));
  return {
    refreshing: query.isFetching,
    refetch: () => { void query.refetch(); },
    recipes: query.data ?? [],
    error: hasReadFailure(query) ? readErrorText(query.error, 'Could not load your recipes.') : null,
    loaded: !query.isPending && !query.isError,
  };
}

export type TrackRecipeMutations = Readonly<{
  /** Keyed per save intent: a retry under the key answers with the recipe its first attempt saved. */
  create: (body: { title: string; body: string }, idempotencyKey: string) => Promise<TrackRecipe>;
  /**
   * Whole-document `PUT` gated on `if_revision`. Resolves with the STORED row, which may differ from the
   * bytes sent; rejects as `RECIPE_SAVE_FAILURES` reads it, a 409 being stale unless it answers a retry of a save
   * whose outcome was unknown and the recipe read back holds exactly what that save sent.
   */
  save: (recipeId: string, body: { title: string; body: string; if_revision: number }) => Promise<TrackRecipe>;
  remove: (recipeId: string) => Promise<void>;
}>;

export function useTrackRecipeMutations(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): TrackRecipeMutations {
  const client = useQueryClient();
  const [saves] = useState(() => recipeSaveAttempts());
  const invalidate = () => { void client.invalidateQueries({ queryKey: queryKeys.trackRecipes() }); };
  const create = useRecoveryMutation(transport, {
    mutationFn: ({ body, key }: { body: { title: string; body: string }; key: string }, transport: ApiTransportPort) => runOperation(
      transport, createTrackRecipeOperation(body, key), unauthorized,
    ),
    /* `onSettled`: a create whose answer was lost may have made the recipe, and the list is how the reader finds it. */
    onSettled: invalidate,
  });
  const save = useRecoveryMutation(transport, {
    mutationFn: ({ recipeId, body }: { recipeId: string; body: { title: string; body: string; if_revision: number } }, transport: ApiTransportPort) =>
      saves(
        { recipeId, body },
        () => runOperation(transport, updateTrackRecipeOperation(recipeId, body), unauthorized),
        async () => recipeSaveLanded(await runOperation(transport, trackRecipeOperation(recipeId), unauthorized), body),
      ),
    /* Invalidate but do not write the response through: it reaches the editor as the promise's value, and
           two homes for one fact would drift. `onSettled` so a 409 also refetches the current revision. */
    onSettled: invalidate,
  });
  const remove = useRecoveryMutation(transport, {
    mutationFn: (recipeId: string, transport: ApiTransportPort) => runOperation(transport, deleteTrackRecipeOperation(recipeId), unauthorized),
    /* `onSettled`: a delete that failed because the row was already gone still needs the list refetched. */
    onSettled: invalidate,
  });
  return {
    create: (body, key) => create.mutateAsync({ body, key }),
    save: (recipeId, body) => save.mutateAsync({ recipeId, body }),
    remove: async (recipeId) => { await remove.mutateAsync(recipeId); },
  };
}

export type Workspace = Readonly<{
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  tracks: readonly Track[];
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

// Query structurally shares these plain arrays independently: read status can change
// without replacing unchanged data. The outer useQueries result is not a memo key.
function combineWorkspaceTracks(results: UseQueryResult<Track[]>[]) {
  return {
    data: results.map(query => query.data),
    reads: results.map(query => ({ pending: query.isPending, error: query.error, refetch: query.refetch })),
  };
}

const NO_AREAS: readonly Area[] = Object.freeze([]);

/**
 * The area → tracks fan-out is a page-level `useQueries`, never a route loader await: one slow area
 * must not block the calendar. The workspace-wide overlay read is folded in so every surface sees the same activity.
 */
export function useWorkspace(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Workspace {
  const areasQuery = useQuery(areaListQueryOptions(transport, unauthorized));
  const overlaysQuery = useQuery(trackOverlaysQueryOptions(transport, unauthorized));
  const areas = areasQuery.data ?? NO_AREAS;
  const trackQueries = useQueries({
    queries: areas.map((area) => tracksInAreaQueryOptions(transport, area.id, unauthorized)),
    combine: combineWorkspaceTracks,
  });
  const activity = useMemo(() => trackActivitiesFrom(overlaysQuery.data ?? []), [overlaysQuery.data]);
  const { tracksByArea, tracks } = useMemo(() => {
    const tracksByArea = new Map<string, readonly Track[]>();
    const tracks: Track[] = [];
    for (const [index, area] of areas.entries()) {
      const data = trackQueries.data[index];
      if (data === undefined) continue;
      const rows = data.map(track => ({ ...track, ...(activity.get(track.id) ?? NEUTRAL_ACTIVITY) }));
      tracksByArea.set(area.id, sortAreaTracksByRecent(rows));
      tracks.push(...rows);
    }
    return { tracksByArea, tracks };
  }, [activity, areas, trackQueries.data]);
  const { trackErrorsByArea, tracksLoadingByArea } = useMemo(() => {
    const trackErrorsByArea = new Map<string, Error>();
    const tracksLoadingByArea = new Map<string, boolean>();
    for (const [index, area] of areas.entries()) {
      const query = trackQueries.reads[index];
      tracksLoadingByArea.set(area.id, query?.pending ?? false);
      if (query?.error instanceof Error) trackErrorsByArea.set(area.id, query.error);
    }
    return { trackErrorsByArea, tracksLoadingByArea };
  }, [areas, trackQueries.reads]);
  return {
    areas, tracksByArea, tracks, areasLoading: areasQuery.isPending,
    overlaysLoading: overlaysQuery.isLoading,
    areasError: areasQuery.error instanceof Error ? areasQuery.error : null,
    overlaysError: overlaysQuery.error instanceof Error ? overlaysQuery.error : null,
    trackErrorsByArea,
    tracksLoadingByArea,
    retryAreas: () => { void areasQuery.refetch(); },
    retryOverlays: () => { void overlaysQuery.refetch(); },
    retryTracks: (areaId) => {
      const index = areas.findIndex((area) => area.id === areaId);
      if (index >= 0) void trackQueries.reads[index]?.refetch();
    },
  };
}

/** Route loaders prime only this one list. */
export function prefetchAreaList(client: QueryClient, transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Promise<void> {
  // Prefetch failures belong to the Areas query; rejecting a loader here would replace the entire app.
  void client.prefetchQuery(areaListQueryOptions(transport, unauthorized));
  // A paused offline query must not hold the route commit either.
  return Promise.resolve();
}

// ---------- mutations ----------
//
// Every mutation invalidates. A write-through may precede it only when the response IS the new cache
// value (an id-keyed row the server just returned) and the very next render needs it.

export type AreaMutations = Readonly<{
  create: (body: NewAreaBody, idempotencyKey: string) => Promise<Area>;
  update: (areaId: string, body: AreaPatchBody) => Promise<Area>;
  remove: (areaId: string, signal?: AbortSignal) => Promise<void>;
}>;

/** A delete answered done (404, `DELETE_FAILURES`): the row is gone, so its cache entry goes as on success. */
function deleteDone(error: unknown): boolean {
  return error instanceof ApiError && classifyFailure(error.failure, DELETE_FAILURES) === 'done';
}

export function useAreaMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): AreaMutations {
  const client = useQueryClient();
  const create = useRecoveryMutation(transport, {
    mutationFn: async ({ body, idempotencyKey }: { body: NewAreaBody; idempotencyKey: string }, transport: ApiTransportPort) => {
      try {
        const capability = await runOperation(transport, areaCreationCapabilityOperation(), unauthorized);
        if (capability !== 'supported') {
          throw new AreaCreatePreflightError('Update the server to enable safe Area creation.');
        }
      } catch (failure: unknown) {
        if (failure instanceof AreaCreatePreflightError) throw failure;
        throw new AreaCreatePreflightError('Could not check Area creation support.');
      }
      return runOperation(transport, createAreaOperation(body, idempotencyKey), unauthorized);
    },
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
    // A lost response can follow a committed creation; the retained creation key makes the next POST safe.
    onSettled: () => client.invalidateQueries({ queryKey: queryKeys.areas() }),
  });
  const update = useRecoveryMutation(transport, {
    mutationFn: ({ areaId, body }: { areaId: string; body: AreaPatchBody }, transport: ApiTransportPort) =>
      runOperation(transport, updateAreaOperation(areaId, body), unauthorized),
    onSuccess: (wire) => {
      const updated = toArea(wire);
      // The Area editor closes as soon as mutateAsync resolves; without the write-through a click in the
      // refetch window snapshots stale defaults into NewTrackForm's local state.
      client.setQueryData<Area[]>(queryKeys.areas(), (current) => current?.map(
        (area) => area.id === updated.id
          ? newestArea(area, updated)
          : area,
      ));
    },
    // A transport failure may still follow a committed write, so failure cannot leave cached defaults authoritative.
    onSettled: () => client.invalidateQueries({ queryKey: queryKeys.areas() }),
  });
  const remove = useRecoveryMutation(transport, {
    mutationFn: ({ areaId, signal }: { areaId: string; signal?: AbortSignal }, transport: ApiTransportPort) =>
      runOperation(transport, { ...deleteAreaOperation(areaId), signal }, unauthorized),
    // The area is gone, also when the delete was answered done; its track list can never resolve again.
    onSuccess: (_result, { areaId }) => { retireAreaQueries(client, areaId); },
    onError: (error, { areaId }) => {
      if (deleteDone(error)) retireAreaQueries(client, areaId);
    },
    // Abort only ends the client wait: the server may already have committed.
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.areas() }); },
  });
  return {
    create: async (body, idempotencyKey) => toArea(await create.mutateAsync({ body, idempotencyKey })),
    update: async (areaId, body) => toArea(await update.mutateAsync({ areaId, body })),
    remove: async (areaId, signal) => { await remove.mutateAsync({ areaId, signal }); },
  };
}

export type TrackMutations = Readonly<{
  /** Mint `idempotencyKey` ONCE per draft: a fresh key on a retry mints a second track. */
  create: (body: NewTrackBody, idempotencyKey: string) => Promise<Track>;
  patch: (trackId: string, areaId: string, body: TrackPatchBody) => Promise<Track>;
  setPinned: (trackId: string, areaId: string, pinned: boolean, nowMs: number) => Promise<Track>;
  createTerminal: (trackId: string, body: NewTerminalCardBody, idempotencyKey: string) => Promise<CardWire>;
  setTerminalWorkbench: (trackId: string, cardId: string, inWorkbench: boolean) => Promise<CardWire>;
  createCodex: (trackId: string, body: NewCodexCardBody, idempotencyKey: string) => Promise<CardWire>;
  createClaude: (trackId: string, body: NewClaudeCardBody, idempotencyKey: string) => Promise<CardWire>;
  createCard: (trackId: string, body: NewCardBody, idempotencyKey: string) => Promise<CardWire>;
  removeCard: (trackId: string, cardId: string, signal?: AbortSignal) => Promise<void>;
  remove: (trackId: string, areaId: string, signal?: AbortSignal) => Promise<void>;
  /** Dismiss one notification item by its kernel key. A `404` rejects too: `DISMISS_FAILURES` reads it as done. */
  dismissActivityItem: (trackId: string, key: string) => Promise<void>;
  /** Answer one ask, one answer per question in order. A `409` rejects too: `ANSWER_ASK_FAILURES` reads it as done. */
  answerAsk: (trackId: string, askId: number, answers: readonly AskAnswer[]) => Promise<void>;
}>;

export function useTrackMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): TrackMutations {
  const client = useQueryClient();
  const create = useRecoveryMutation(transport, {
    mutationFn: (variables: { body: NewTrackBody; idempotencyKey: string }, transport: ApiTransportPort) =>
      runOperation(transport, createTrackOperation(variables.body, variables.idempotencyKey), unauthorized),
    onSuccess: (track, { body }) => {
      void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(track.area_id) });
      // Explicit `attach_folder` mints an area_folders row; drop any cached list so a later read cannot serve a stale empty array.
      if (body.attach_folder) {
        client.removeQueries({ queryKey: queryKeys.areaFolders(body.area_id) });
      }
    },
  });
  const patch = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, body }: { trackId: string; areaId: string; body: TrackPatchBody }, transport: ApiTransportPort) =>
      runOperation(transport, updateTrackOperation(trackId, body), unauthorized),
    // A patch can move the track: the area the server just reported is refetched too.
    onSuccess: (track, variables) => {
      if (track.area_id !== variables.areaId) void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(track.area_id) });
    },
    // No write-through: the detail's Close / Reopen capabilities are the server's to derive, so the page keeps its last
    // detail until the refetch (or the `track.updated` event's) replaces it whole. `onSettled`: an unknown answer may
    // still follow a stored patch.
    onSettled: (_track, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(variables.areaId) });
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(variables.trackId) });
    },
  });
  const remove = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, signal }: { trackId: string; areaId: string; signal?: AbortSignal }, transport: ApiTransportPort) =>
      runOperation(transport, { ...deleteTrackOperation(trackId), signal }, unauthorized),
    onSuccess: (_result, variables) => {
      client.removeQueries({ queryKey: queryKeys.trackDetail(variables.trackId) });
    },
    onError: (error, variables) => {
      if (deleteDone(error)) client.removeQueries({ queryKey: queryKeys.trackDetail(variables.trackId) });
    },
    // Reconcile both list-derived surfaces even if abort raced a committed DELETE.
    onSettled: (_result, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.tracksInArea(variables.areaId) });
      void cancelThenInvalidate(client, queryKeys.overlaysByKind('track'));
    },
  });
  /* The card creates answer with the row the kernel just wrote and the next render needs it: the caller
   * navigates to `?card=<id>` and the board can only draw a card the detail cache already holds. */
  const addCardToDetail = (card: CardWire): void => {
    client.setQueryData(queryKeys.trackDetail(card.track_id), (previous: TrackDetailWire | undefined) => {
      if (previous === undefined) return previous;
      if (previous.cards.some((existing) => existing.id === card.id)) return previous;
      return { ...previous, cards: [...previous.cards, card] };
    });
    void client.invalidateQueries({ queryKey: queryKeys.trackDetail(card.track_id) });
  };
  const createTerminal = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, body, key }: { trackId: string; body: NewTerminalCardBody; key: string }, transport: ApiTransportPort) =>
      runOperation(transport, createTerminalCardOperation(trackId, body, key), unauthorized),
    onSuccess: addCardToDetail,
  });
  const setTerminalWorkbench = useRecoveryMutation(transport, {
    mutationFn: ({ cardId, inWorkbench }: { trackId: string; cardId: string; inWorkbench: boolean }, port: ApiTransportPort) =>
      runOperation(port, setTerminalWorkbenchOperation(cardId, inWorkbench), unauthorized),
    onSuccess: (card) => {
      client.setQueryData(queryKeys.trackDetail(card.track_id), (previous: TrackDetailWire | undefined) => previous === undefined ? previous
        : { ...previous, cards: previous.cards.map(existing => existing.id === card.id ? card : existing) });
    },
    onSettled: (_card, _error, { trackId }) => {
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
    },
  });
  const createCodex = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, body, key }: { trackId: string; body: NewCodexCardBody; key: string }, transport: ApiTransportPort) =>
      runOperation(transport, createCodexCardOperation(trackId, body, key), unauthorized),
    onSuccess: addCardToDetail,
  });
  const createClaude = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, body, key }: { trackId: string; body: NewClaudeCardBody; key: string }, transport: ApiTransportPort) =>
      runOperation(transport, createClaudeCardOperation(trackId, body, key), unauthorized),
    onSuccess: addCardToDetail,
  });
  const createCard = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, body, key }: { trackId: string; body: NewCardBody; key: string }, transport: ApiTransportPort) =>
      runOperation(transport, createCardOperation(trackId, body, key), unauthorized),
    onSuccess: addCardToDetail,
  });
  /* Delete drops the row from the cached detail before the refetch lands: leaving it on screen would keep
   * a PTY attached to a card the kernel has torn down. A delete answered done (404, `DELETE_FAILURES`) is gone the same
   * way. `onSettled`: an aborted wait says nothing about commit. */
  const dropCard = (_result: unknown, { trackId, cardId }: { trackId: string; cardId: string }) => {
    client.setQueryData(queryKeys.trackDetail(trackId), (previous: TrackDetailWire | undefined) => {
      if (previous === undefined) return previous;
      const cards = previous.cards.filter((existing) => existing.id !== cardId);
      return cards.length === previous.cards.length ? previous : { ...previous, cards };
    });
  };
  const removeCard = useRecoveryMutation(transport, {
    mutationFn: ({ cardId, signal }: { trackId: string; cardId: string; signal?: AbortSignal }, transport: ApiTransportPort) =>
      runOperation(transport, { ...deleteCardOperation(cardId), signal }, unauthorized),
    onSuccess: dropCard,
    onError: (error, variables) => { if (deleteDone(error)) dropCard(undefined, variables); },
    onSettled: (_result, _error, { trackId }) => {
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
      void cancelThenInvalidate(client, queryKeys.overlaysByKind('track'));
    },
  });
  /* No cache write and no invalidation: the projector's `overlay.set` is what drops the row, and a
   * refetch fired now would race the recompute and read the item back. */
  const dismissItem = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, key }: { trackId: string; key: string }, transport: ApiTransportPort) =>
      runOperation(transport, dismissActivityItemOperation(trackId, key), unauthorized),
  });
  /* An answer may apply a lifecycle action; refresh detail/lists even after an uncertain response.
   * The projector's overlay remains authoritative for removing the question itself. */
  const answerAsk = useRecoveryMutation(transport, {
    mutationFn: ({ trackId, askId, answers }: { trackId: string; askId: number; answers: readonly AskAnswer[] }, transport: ApiTransportPort) =>
      runOperation(transport, answerAskOperation(trackId, askId, answers), unauthorized),
    onSettled: (_answer, _error, { trackId }) => {
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
      void client.invalidateQueries({ queryKey: queryKeys.trackLists() });
    },
  });
  const patchTrack = async (trackId: string, areaId: string, body: TrackPatchBody) =>
    toTrack(await patch.mutateAsync({ trackId, areaId, body }));
  return {
    create: async (body, idempotencyKey) => toTrack(await create.mutateAsync({ body, idempotencyKey })),
    patch: patchTrack,
    createTerminal: async (trackId, body, key) => createTerminal.mutateAsync({ trackId, body, key }),
    setTerminalWorkbench: async (trackId, cardId, inWorkbench) => setTerminalWorkbench.mutateAsync({ trackId, cardId, inWorkbench }),
    createCodex: async (trackId, body, key) => createCodex.mutateAsync({ trackId, body, key }),
    createClaude: async (trackId, body, key) => createClaude.mutateAsync({ trackId, body, key }),
    createCard: async (trackId, body, key) => createCard.mutateAsync({ trackId, body, key }),
    removeCard: async (trackId, cardId, signal) => {
      await removeCard.mutateAsync({ trackId, cardId, signal });
    },
    // `pinned_at` is both the flag and the ordering key, so unpinning is a
    // null write rather than a delete of some separate row.
    setPinned: (trackId, areaId, pinned, nowMs) =>
      patchTrack(trackId, areaId, { pinned_at: pinned ? nowMs : null }),
    remove: async (trackId, areaId, signal) => { await remove.mutateAsync({ trackId, areaId, signal }); },
    dismissActivityItem: async (trackId, key) => { await dismissItem.mutateAsync({ trackId, key }); },
    answerAsk: async (trackId, askId, answers) => { await answerAsk.mutateAsync({ trackId, askId, answers }); },
  };
}

/**
 * Settings › Plugins — the installed list. `retry: false` so a failed read says so and offers Retry.
 * `plugin.state` does not refresh this list, so the query polls only while some row is in motion.
 */
/* Frozen array, not a `Set`: `no-module-runtime-state` rejects a module-level `new`. */
const PLUGIN_TRANSIENT_STATES = Object.freeze(['spawning', 'installing'] as const);

export function pluginsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.plugins(),
    queryFn: (): Promise<PluginListItem[]> => runOperation(transport, pluginsOperation(), unauthorized),
    retry: false,
    /* Polls only while a row is in motion and only while this pane holds the query. A counted cap was
     * worse: `dataUpdateCount` also counts every enable/disable invalidation and never resets. */
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
  /** The plugins whose LAST enable/disable succeeded. Per plugin, last-write-wins, disjoint from `errors` by construction. */
  effectBoundaryIds: ReadonlySet<string>;
  setEnabled: (id: string, enabled: boolean) => void;
  /** Remove the plugin. The confirmation is the pane's. */
  uninstall: (id: string) => void;
}>;

/**
 * Enable / disable, tracked PER PLUGIN: a single `useMutation` exposes only the latest call's state,
 * so two quick toggles mislabel each other. The response is not written through — enable answers while
 * the supervisor is still bringing the process up. `effectBoundaryIds` says a successful write did not
 * reach an already-running conversation (codex binds `dynamicTools` at thread start); set on success only, both directions.
 */
export function usePluginMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): PluginMutations {
  const client = useQueryClient();
  /* A COUNT per id, not membership: the switch stays usable while a write is in flight, so one plugin can have two. */
  const [pending, setPending] = useState<ReadonlyMap<string, number>>(() => new Map());
  const [errors, setErrors] = useState<ReadonlyMap<string, string>>(() => new Map());
  /* Membership, not a count: "has this plugin's last write settled successfully" is a yes/no. */
  const [boundary, setBoundary] = useState<ReadonlySet<string>>(() => new Set());
  const acquirePending = (id: string) => {
    setPending((current) => new Map(current).set(id, (current.get(id) ?? 0) + 1));
    return () => setPending((current) => {
      const next = new Map(current);
      const left = (current.get(id) ?? 1) - 1;
      if (left <= 0) next.delete(id); else next.set(id, left);
      return next;
    });
  };
  /* `null` is `done`: the answer proves the intent already holds, so the row shows nothing. */
  const showFailure = (id: string, text: string | null) => {
    if (text !== null) setErrors((current) => new Map(current).set(id, text));
  };
  const write = useRecoveryMutation(transport, {
    acquireLocal: ({ id }) => acquirePending(id),
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }, transport: ApiTransportPort) =>
      runOperation(transport, setPluginEnabledOperation(id, enabled), unauthorized),
    onMutate: ({ id }) => {
      setErrors((current) => {
        if (!current.has(id)) return current;
        const next = new Map(current);
        next.delete(id);
        return next;
      });
      /* Withdrawn while the next write is out: the sentence is about a SETTLED change. */
      setBoundary((current) => {
        if (!current.has(id)) return current;
        const next = new Set(current);
        next.delete(id);
        return next;
      });
    },
    onSuccess: (_data, { id }) => {
      setBoundary((current) => (current.has(id) ? current : new Set(current).add(id)));
    },
    onError: (error, { id }) => { showFailure(id, writeFailureText(PLUGIN_TOGGLE_FAILURES, PLUGIN_TOGGLE_TEXT)(error)); },
    onSettled: () => {
      void client.invalidateQueries({ queryKey: queryKeys.plugins() });
    },
  });
  /* Uninstall shares the per-id `pending` and `errors` maps: the row has one place to put a sentence. A
   * separate mutation because a removal must not set the effect-boundary line. */
  const remove = useRecoveryMutation(transport, {
    acquireLocal: acquirePending,
    mutationFn: (id: string, transport: ApiTransportPort) => runOperation(transport, uninstallPluginOperation(id), unauthorized),
    onMutate: (id) => {
      setErrors((current) => {
        if (!current.has(id)) return current;
        const next = new Map(current);
        next.delete(id);
        return next;
      });
    },
    onError: (error, id) => { showFailure(id, writeFailureText(PLUGIN_UNINSTALL_FAILURES, DELETE_TEXT)(error)); },
    onSettled: () => {
      void client.invalidateQueries({ queryKey: queryKeys.plugins() });
    },
  });
  return {
    pendingIds: new Set(pending.keys()),
    errors,
    effectBoundaryIds: boundary,
    setEnabled: (id, enabled) => { write.mutate({ id, enabled }); },
    uninstall: (id) => { remove.mutate(id); },
  };
}

/**
 * The two install sources, as one write through the recovery runner: refused at the press while offline, rejecting
 * with the write's error for the form to read through its table. The credential rides only in the request: a settled
 * mutation is reset and kept by no cache (`gcTime: 0`).
 */
export type PluginInstallMutation = Readonly<{
  pending: boolean;
  checkConnector: (draft: ConnectorInstallDraft) => Promise<ConnectorCheckResult>;
  installConnector: (draft: ConnectorInstallDraft) => Promise<void>;
  installLocalPath: (path: string) => Promise<void>;
}>;

export function usePluginInstall(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): PluginInstallMutation {
  const client = useQueryClient();
  const install = useRecoveryMutation(transport, {
    gcTime: 0,
    mutationFn: (operation: ApiOperation<InstalledPlugin>, admitted: ApiTransportPort) => runOperation(admitted, operation, unauthorized),
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.plugins() }); },
  });
  const run = (operation: ApiOperation<InstalledPlugin>) => install.mutateAsync(operation)
    .finally(() => { install.reset(); }).then(() => undefined);
  return {
    pending: install.isPending,
    checkConnector: async (draft) => {
      try {
        const result = await runOperation(transport, checkConnectorOperation(draft), unauthorized);
        return { ok: true, tools: result.tools };
      } catch (error) {
        return { ok: false, message: probeFailureText(error, CONNECTOR_CHECK_TEXT) };
      }
    },
    installConnector: (draft) => run(installConnectorOperation(draft)),
    installLocalPath: (path) => run(installLocalPathOperation(path)),
  };
}

/**
 * One plugin's configuration — the detail read. No poll and no focus refetch: the pane holds a draft of
 * the operator's edits, and a background refetch would re-seed it mid-typing.
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
 * The two configuration writes, each through the recovery runner: refused at the press while offline, never sent. Both
 * resolve rather than reject, carrying the rejection as thrown for the pane to read through the route's table, and the
 * plugin's state afterwards. A settled mutation is reset and kept by no cache: a value may be a credential.
 */
export function usePluginConfigMutations(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): PluginConfigMutations {
  const client = useQueryClient();
  type ConfigWrite = Readonly<{ id: string; patch: Readonly<Record<string, PluginConfigValue | null>>; options: Readonly<{ reset: boolean }> }>;
  /* Settles after the re-read, which the runner skips for an intent that lost ownership (and then rejects it). */
  const refresh = (id: string) => Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.plugins() }),
    client.invalidateQueries({ queryKey: queryKeys.pluginDetail(id) }),
  ]);
  const write = useRecoveryMutation(transport, {
    gcTime: 0,
    mutationFn: ({ id, patch, options }: ConfigWrite, admitted: ApiTransportPort) =>
      runOperation(admitted, patchPluginConfigOperation(id, patch, options), unauthorized),
    onSettled: (_data, _error, { id }) => refresh(id),
  });
  const restart = useRecoveryMutation(transport, {
    gcTime: 0,
    mutationFn: async (id: string, admitted: ApiTransportPort): Promise<PluginRestartFacts> => {
      /* Read the plugin's state back after the attempt on BOTH branches: a 2xx `reload` answers as of the
       * handler's return, and a connector's bring-up can fail after it. Best-effort; falls back to what is known. */
      const readBack = async (fallback: PluginRestartFacts): Promise<PluginRestartFacts> => {
        try {
          const after = await runOperation(admitted, pluginDetailOperation(id), unauthorized);
          return { ...fallback, state: after.state, lastError: after.last_error };
        } catch {
          return fallback;
        }
      };
      try {
        const detail = await runOperation(admitted, reloadPluginOperation(id), unauthorized);
        return await readBack({ rejection: null, state: detail.state, lastError: detail.last_error });
      } catch (error) {
        /* The refusal is not the verdict: a non-200 covers a held lock, a failed bring-up sitting in
         * `unavailable`, or a stopped `app`, and only the plugin's own state tells them apart. */
        return readBack({ rejection: { error }, state: 'unknown' });
      }
    },
    onSettled: (_data, _error, id) => refresh(id),
  });

  return {
    save: async (id, patch, options) => {
      try {
        await write.mutateAsync({ id, patch, options });
        return { ok: true };
      } catch (error) {
        return { ok: false, error };
      } finally { write.reset(); }
    },
    applyRestart: async (id, patch, options) => {
      /* An empty patch with no reset is not a write: PATCHing `{}` would take the lifecycle lock for nothing and could 409 the restart. */
      if (Object.keys(patch).length > 0 || options.reset) {
        try { await write.mutateAsync({ id, patch, options }); }
        catch (error) { return { saved: false, error }; }
        finally { write.reset(); }
      }
      try {
        return { saved: true, restart: await restart.mutateAsync(id) };
      } catch (error) {
        // A restart never sent reads as one that did not run. A previous acknowledgement does not prove the plugin's current state once this attempt lost ownership of its readback.
        return { saved: true, restart: { rejection: { error }, state: 'unknown' } };
      } finally { restart.reset(); }
    },
  };
}

export function useSettingsMutation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): (patch: SettingsPatch) => Promise<SettingsBag> {
  const client = useQueryClient();
  const save = useRecoveryMutation(transport, {
    mutationFn: (patch: SettingsPatch, transport: ApiTransportPort) => runOperation(transport, putSettingsOperation(patch), unauthorized),
    /* Invalidate; do not write the response through. Settings › Network commits per field, so an older
     * PUT response can land last and would revert the field under a green tick. */
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.settings() }); },
  });
  return (patch) => save.mutateAsync(patch);
}

/** The wire, or the 409 turned into a value: `stale-rev` is a condition the next `['track', id]` refetch fixes, not an error. */
export type SeriesRead = ResolvedSeries | Readonly<{ status: 'stale-rev'; current_rev: number }>;

/** A resolved row does not move for at least this long. */
const SERIES_STALE_MS = 5 * 60 * 1000;
/** While the row is `pending`: the kernel's lane usually answers within a
 *  few seconds of the first read. */
const SERIES_PENDING_POLL_MS = 3000;
/** …and after two minutes of that, something is slow (a lane backed up, a
 *  plugin restarting) and the poll backs off. */
const SERIES_PENDING_SLOW_POLL_MS = 30_000;
/** Two minutes, counted in polls rather than wall time: react-query pauses the timer while the tab is hidden. */
const SERIES_PENDING_FAST_POLLS = 120_000 / SERIES_PENDING_POLL_MS;

function staleRevOf(error: unknown): SeriesRead | null {
  if (!(error instanceof ApiError)) return null;
  const { failure } = error;
  if (failure.kind !== 'http' || failure.status !== 409) return null;
  const body = staleRevBodySchema.safeParse(failure.body);
  return body.success ? { status: 'stale-rev', current_rev: body.data.current_rev } : null;
}

/** Poll only while the row is `pending`: row writes emit no event. Every other state stops the timer. */
export function seriesRefetchInterval(
  query: { state: { data?: SeriesRead; dataUpdateCount: number } },
): number | false {
  const { data, dataUpdateCount } = query.state;
  if (data === undefined || data.status !== 'pending') return false;
  return dataUpdateCount > SERIES_PENDING_FAST_POLLS ? SERIES_PENDING_SLOW_POLL_MS : SERIES_PENDING_POLL_MS;
}

/** The row, or the 404 turned into a value: a dangling citation is a state the design admits, not an error to retry. */
export type SourceRead =
  | Readonly<{ status: 'found'; source: TrackSourceDetail }>
  | Readonly<{ status: 'missing' }>;

function isNotFound(error: unknown): boolean {
  return error instanceof ApiError && error.failure.kind === 'http' && error.failure.status === 404;
}

/** One captured source. Default `staleTime`: anchors are append-only, so every open re-reads behind the cached copy. No focus refetch. */
export function trackSourceQueryOptions(
  transport: ApiTransportPort, trackId: string, sourceId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.trackSource(trackId, sourceId),
    queryFn: async ({ signal }: { signal: AbortSignal }): Promise<SourceRead> => {
      try {
        const source = await runOperation(transport, { ...trackSourceOperation(trackId, sourceId), signal }, unauthorized);
        return { status: 'found', source };
      } catch (error) {
        if (!isNotFound(error)) throw error;
        return { status: 'missing' };
      }
    },
    refetchOnWindowFocus: false,
  };
}

/** One `chart.series` block's resolved data. `detail` defaults to `full`; the key carries `rev`, not `detail`. */
export function trackReportSeriesQueryOptions(
  transport: ApiTransportPort, trackId: string, blockId: string, rev: number,
  unauthorized: UnauthorizedChannel, detail: SeriesDetail = 'full',
) {
  return {
    queryKey: queryKeys.trackReportSeries(trackId, blockId, rev),
    queryFn: async ({ signal }: { signal: AbortSignal }): Promise<SeriesRead> => {
      try {
        return await runOperation(
          transport, { ...trackReportSeriesOperation(trackId, blockId, rev, detail), signal }, unauthorized,
        );
      } catch (error) {
        const stale = staleRevOf(error);
        if (stale === null) throw error;
        return stale;
      }
    },
    staleTime: SERIES_STALE_MS,
    refetchInterval: seriesRefetchInterval,
  };
}
