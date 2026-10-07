import { useConversationDraftRetention, useConversationDraftAdoption, useRequestedConversationOpen, useConversationEscape, useConversationDraftAutoSend } from '../conversations/pane-lifecycle.ts';
import { createConversationDraftActions } from '../conversations/draft-actions.ts';
import { useConversationStore } from '../conversations/store.ts';
import type { ConversationCreationSource, ConversationRouteIntent, PlannerConversationScope } from '../conversations/contracts.ts';
import { Button } from '@astryxdesign/core/Button';
import type { PaneResizeGroup } from '../../ui/drawer/resize-group.ts';
import { sideConversationSnapshot } from '../../../../core/domain/side-conversation.ts';
import { writeClipboardText } from '../../ui/operation-feedback/clipboard.ts';
import { useConversationEdit } from '../conversations/edit.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { notSentMessage } from '../../../../core/domain/conversation-delivery.ts';
import { EMPTY_COMPOSER, isComposerEmpty } from '../../../../core/domain/conversation-composer.ts';
import { DELETE_FAILURES, DELETE_TEXT, NotSentError, writeFailureText } from '../../../../core/domain/failure-class.ts';
// Code-based TanStack Router setup, built inside a factory so a test can inject the
// transport and QueryClient; also the composition point for route-owned surfaces.

import {
  createRootRoute, createRoute, createRouter, redirect, useLocation, useRouterState, type AnyRoute,
} from '@tanstack/react-router';
import { useCallback, useEffect, useMemo, useRef, type Dispatch, type ReactNode, type SetStateAction } from 'react';
import { TrackViewProvider, useTrackViewState } from './track-view-state.tsx';
import { HStack } from '@astryxdesign/core/HStack';
import { useQuery, type QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { AgentProvider } from '../../../../core/api/generated/wire.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  ATTACHED_WORKSPACE_REASON, PlannerAttachButton, PlannerAttachmentDrawer,
  NO_UPLOAD, type AttachmentStore, usePlannerAttachments,
} from '../../features/planner/attachments.tsx';
import {
  trackOverlayPayload, plannerProviderOf, toTrack, trackActivityFrom, trackDisplayTitle,
  type Track, type TrackActivity, type TrackDetailWire,
} from '../../../../core/domain/track.ts';
import { cardActivityOf, type CardActivity } from '../../../../core/domain/activity.ts';
import type {
  BoardHostItem, CardAddMenuEntry, CardHost, CardRegistry,
} from '../../systems/cards/public.js';
import {
  cardAddMenuEntries, isAssistantHarnessPayload, isPlannerHarnessPayload, plannerCardIn, partitionTrackCards,
} from '../../systems/cards/public.js';
import { mintIdempotencyKey, useKeyedIntent, type KeyedRequest } from '../providers/idempotency-key.ts';
import footerStyles from './composer-footer.module.css';
import { TrackPage, type TrackInputNotification } from '../../features/track/page/public.tsx';
import { CardGridOverlay, TrackStage } from '../../features/track/grid/public.tsx';
import { AddCardMenu, NewCardForm, type NewCardValues } from '../../features/track/new-card/public.tsx';
import {
  cardCreateEnded, cardCreateFailureText, keyedCardBodyOf, sameCardDraft, sendCardCreate,
  type CardCreatePort, type CardDraft, type KeyedCardBody,
} from '../../features/track/new-card/create.ts';
import { ChatList } from '../../features/chat/list/public.tsx';
import {
  ChatComposer, ChatFooterError, ChatFooterNotice, ChatFooterRemedy, ChatThread,
} from '../../features/chat/thread/public.tsx';
import { ModelPill } from '../../features/chat/thread/model-pill.tsx';
import { ContextRing } from '../../features/chat/thread/context-ring.tsx';
import { useMentionTrigger } from '../../features/chat/thread/mention-trigger.tsx';
import { ReportBacklinks } from '../../features/report/backlinks/public.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { TaskRecovery, useCurrentTaskRows } from './task-recovery.tsx';
import { useReportPreviewResolver, useReportPreviewViewports } from './report-preview.ts';
import { useReportSeriesResolver } from './report-series.ts';
import { ReportSourceDrawer } from './report-source.tsx';
import { ReportEmpty } from '../../features/report/empty/public.tsx';
import { ReportFileViewer } from '../../features/report/file-viewer/public.tsx';
import { ReportOutline } from '../../features/report/outline/public.tsx';
import { RecentFiles } from '../../features/report/recent-files/public.tsx';
import { revealReportAnchor } from '../../features/report/anchor/public.ts';
import {
  backlinkCountsByBlock, deriveReportOutline, deriveReportTasks, readTrackReport, type ReportLinkTarget,
} from '../../../../core/domain/report.ts';
import {
  parseWorkspaceRelativeFilePath, type ReportFileLinkTarget,
} from '../../../../core/domain/report-file.ts';
import type { ReportSourceLinkTarget } from '../../../../core/domain/report-source.ts';
import {
  conversationName,
  trackConversationCardId,
  FOLLOW_INSTALLATION_DEFAULT,
  type Conversation,
  type PendingQueueEntry,
  type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { ConfirmDialog, Dialog } from '../../ui/dialog/public.tsx';
import { createDirectoryLister, createTrackWorkspaceFilesPort } from '../providers/directory.ts';
import { useMentionSearch } from '../providers/mentions.ts';
import { DELETE_CARD_COPY } from '../../ui/confirm-dialog/copy.ts';
import { OperationFeedback, useDeleteConfirm, useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { PanelAction } from '../../ui/panel-card/public.tsx';
import { useCommittedCallback } from '../../ui/state/committed-callback.ts';
import { useState } from '../../ui/state/public.ts';
import {
  modelCatalogQueryOptions, serverVersionOperation, runOperation,
  prefetchAreaList,

  useTrackConversationMutations, useTrackMutations,
  trackBacklinksQueryOptions, trackConversationsQueryOptions, trackDetailQueryOptions,
  trackTaskVerdictsQueryOptions,
} from '../providers/queries.ts';
import { NewTrackRoute } from './new-track-route.tsx';
import { DailyTodayRoute } from './daily-planner.tsx';
import { NewTrackDraftProvider } from './new-track-drafts.tsx';
import { RecipesRoute } from './recipes-route.tsx';
import { createUiPreferences, UiPreferencesProvider, useConversationViewTarget, useUiPreferences, useReadReceipt, type UiPreferences } from '../providers/ui-preferences.tsx';
import { TrackSelector } from '../shell/track-selector.tsx';
import { AppShell, useConversationDrawerResize, useOpenMobileSection, useMobileHeaderActionsHost, useMobileHeaderTitleHost, useMobileTrackChoices } from '../shell/public.tsx';
import {
  ConversationProvider, useConversationRegistry,
} from '../conversations/public.tsx';
import {
  renderedMobilePanel,
  useGo, useGoSameTrack, useRouteCardId, useRouteFilePath, useRouteFrom, useRouteHash,
  useRoutePanel, useRouteParam, useTrackFileNavigation,
  usePlannerOpenIntent, useTrackPanelNavigation, validateTrackSearch, type TrackSearch,
} from './navigation.ts';
import {
  createRecentFileHistory, type RecentFileHistory,
} from '../providers/recent-files.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { PendingRoute } from './pending-route.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { PendingQueue, PlannerAskDrawer } from '../../features/planner/public.ts';
import { openAsksOf, type OpenAsk } from '../../../../core/domain/ask.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';

export const APP_BASEPATH = '/next';

/** A conversation that is not a Planner's asks nothing; one frozen value, so the composer's memo holds. */
const NO_ASKS: readonly OpenAsk[] = Object.freeze([]);

/** A route-owned server list whose rows open in this panel's drawer. */
type ConversationPanelSource = ConversationCreationSource & Readonly<{
    /** The Track this draft belongs to. */
    scopeId: string;
    rows: readonly Conversation[];
    /** The kernel's per-card verdicts for the track these rows are on; required, because a caller that passes no overlay has a list on which nothing can ever be working. */
    cards: Readonly<Record<string, CardActivity>>;
    /** The Track these rows may be sent to. */
    rememberOn: string;
    /** Known on the Track route; Today resolves the open conversation's Track on demand. */
    workspaceRoot: string | null;
    scopeOf: (conversationId: string) => PlannerConversationScope | null;
    /** The one row a Planner reads and the Area whose `area/reports/` it reads, which is what `@` offers; `null` where no row is a Planner's. A track conversation is an Assistant, which `area/reports/` refuses.
     * `asks` are the track's open asks, shown above that row's composer, and `answerAsk` answers one. */
    planner: Readonly<{
      cardId: string; areaId: string; asks: readonly OpenAsk[];
      answerAsk: (askId: number, answers: readonly string[]) => Promise<void>;
    }> | null;
  }>;

/** The card runtime, created once at boot and injected. */
export type CardRuntime = Readonly<{ registry: CardRegistry; host: CardHost }>;

export type AppRouterDeps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  client: QueryClient;
  onSignOut: () => void;
  cards: CardRuntime;
  recentFiles?: RecentFileHistory;
  uiPreferences?: UiPreferences;
}>;

function renderNothing(): null { return null; }

export function createRouteTree(deps: AppRouterDeps): AnyRoute {
  const { transport, unauthorized, client, onSignOut, cards } = deps;
  const recentFiles = deps.recentFiles ?? createRecentFileHistory();
  const preferences = deps.uiPreferences ?? createUiPreferences();
  const rootRoute = createRootRoute({ component: () => (
    <UiPreferencesProvider preferences={preferences}>
      <TrackViewProvider><ShellRoute transport={transport} unauthorized={unauthorized} onSignOut={onSignOut} /></TrackViewProvider>
    </UiPreferencesProvider>
  ) });

  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    /** The index loader primes only the areas list; awaiting the area → tracks fan-out here would let one slow area block the route commit. */
    loader: () => prefetchAreaList(client, transport, unauthorized),
    validateSearch: (search: Record<string, unknown>) => ({ day: typeof search.day === 'string' ? search.day : undefined }),
    component: function DailyRoute() {
      const day = new URLSearchParams(useLocation({ select: (location) => location.searchStr })).get('day') ?? undefined;
      const go = useGo();
      return <DailyTodayRoute transport={transport} unauthorized={unauthorized} selectedDate={day}
        onOpenTrack={(trackId) => go({ name: 'track', trackId })}
        renderTrack={(detail, evidence, panelContent) => <TrackRouteBody panelContent={panelContent} reportEvidence={evidence} key={detail.track.id} transport={transport} unauthorized={unauthorized}
          track={toTrack(detail.track, trackActivityFrom(detail.track.id, detail.overlays))}
          canReopenTrack={detail.can_reopen} canCloseTrack={detail.can_close}
          cards={detail.cards} overlays={detail.overlays} cardRuntime={cards} recentFiles={recentFiles} />} />;
    },
  });

  const newTrackRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/area/$areaId/new',
    component: () => <NewTrackRoute transport={transport} unauthorized={unauthorized} />,
  });

  const trackRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/track/$trackId',
    validateSearch: (search: Record<string, unknown>): TrackSearch => validateTrackSearch(search),
    component: () => <TrackRoute
      transport={transport}
      unauthorized={unauthorized}
      cardRuntime={cards}
      recentFiles={recentFiles}
    />,
  });

  const recipesRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/recipes',
    component: () => <RecipesRoute transport={transport} unauthorized={unauthorized} />,
  });

  /* Every settings route renders nothing: the URL is the state and `SettingsOverlay`
       is its view. A route component remounts on every navigation, which would replay
       the panel's entrance animation on every click. */
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings',
    component: renderNothing,
  });

  const networkRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/network',
    component: renderNothing,
  });

  const pluginsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/plugins',
    component: renderNothing,
  });

  const plannersRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/planners',
    component: renderNothing,
  });

  const appearanceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/appearance',
    component: renderNothing,
  });

  const aboutRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/about',
    component: renderNothing,
  });

  const legacyTodayRoute = createRoute({ getParentRoute: () => rootRoute, path: '/today/legacy',
    validateSearch: (search: Record<string, unknown>) => ({ day: typeof search.day === 'string' ? search.day : undefined }),
    beforeLoad: ({ search }) => redirect({ to: '/', search, replace: true }) });
  return rootRoute.addChildren([
    indexRoute, legacyTodayRoute, newTrackRoute, trackRoute, recipesRoute, settingsRoute,
    networkRoute, pluginsRoute, plannersRoute, appearanceRoute, aboutRoute,
  ]);
}

export function createAppRouter(deps: AppRouterDeps) {
  return createRouter({
    routeTree: createRouteTree(deps),
    basepath: APP_BASEPATH,
    defaultPreload: false,
  });
}

function ShellRoute({ transport, unauthorized, onSignOut }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onSignOut: () => void }) {
  const go = useGo();
  return (
    <ConversationProvider><NewTrackDraftProvider>
      <AppShell
        transport={transport}
        unauthorized={unauthorized}
        onOpenSettings={() => go({ name: 'settings' })}
        onOpenPlugins={() => go({ name: 'settings-plugins' })}
        onSignOut={onSignOut}
      />
    </NewTrackDraftProvider></ConversationProvider>
  );
}

/**
 * The conversation module shared by Today and Track. A draft is a third open state,
 * not a `Conversation`: the card is minted by the first message, so until one is
 * sent there is no card id and nothing to fetch.
 */
/** Two independent cards share the existing creation/recovery path and registry. */
function useConversationPanel(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel, source: ConversationPanelSource,
  options?: { showTrack?: boolean; resizable?: boolean },
) {
  const registry = useConversationRegistry();
  const compact = useCompactViewport();
  const [sideError, setSideError] = useState<string | null>(null);
  const mainTarget = useConversationViewTarget(source.scopeId);
  const parentId = mainTarget[0]?.kind === 'row' ? mainTarget[0].id : null;
  const capabilities = useQuery({ queryKey: ['server-version'],
    queryFn: () => runOperation(transport, serverVersionOperation(), unauthorized), enabled: parentId !== null && !compact, retry: false });
  const sideSlot = `${source.scopeId}:side:${parentId ?? ''}`;
  const sideTarget = useConversationViewTarget(sideSlot);
  const sideDraft = registry.draftOf(sideSlot);
  const childId = sideTarget[0]?.kind === 'row' ? sideTarget[0].id : null;
  const child = source.rows.find((row) => row.id === childId);
  const belongsToParent = !compact && parentId !== null && (sideTarget[0]?.kind === 'draft'
    ? sideDraft?.side?.source_card_id === parentId : child?.sourceCardId === parentId);
  const ownedCardIds = useMemo(() => [parentId, belongsToParent ? childId : null]
    .filter((id): id is string => id !== null), [parentId, belongsToParent, childId]);
  const side = useConversationPane(transport, unauthorized, source, sideTarget,
    { ...options, ownedCardIds, inline: true, slotId: sideSlot, enabled: belongsToParent });
  const main = useConversationPane(transport, unauthorized, source, mainTarget,
    { ...options, ownedCardIds, sideError, stacked: true, showSideCommand: !compact, companion: belongsToParent && side.isOpen ? side.drawerFor : undefined,
      onSide: (parent, entries, question) => {
        if (compact) { setSideError('Side conversations are available on desktop.'); return false; }
        if (capabilities.data?.conversationSide !== true) {
          setSideError('Side conversations require a server that reports support. Update or reconnect, then try again.');
          return false;
        }
        setSideError(null);
        if (sideDraft !== null && sideDraft.sentText !== null) {
          sideTarget[1]({ kind: 'draft' });
          if (question !== '') { setSideError('This side draft still has an unsettled delivery. Continue or retry it before starting another.'); return false; }
          return;
        }
        if (question === '') {
          const saved = source.rows.find((row) => row.sourceCardId === parent.id);
          if (saved !== undefined) { sideTarget[1]({ kind: 'row', id: saved.id }); return; }
        }
        registry.startDraft({ scopeId: sideSlot, key: mintIdempotencyKey(),
          side: sideConversationSnapshot(parent.id, entries), model: FOLLOW_INSTALLATION_DEFAULT,
          text: question === '' ? null : question, autoSend: question !== '',
          sentText: null, creating: false, error: null, remedy: null });
        sideTarget[1]({ kind: 'draft' });
      },
    });
  return main;
}

function useConversationPane(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  source: ConversationPanelSource,
  target: ReturnType<typeof useConversationViewTarget>,
  options?: { showSideCommand?: boolean; stacked?: boolean; sideError?: string | null; ownedCardIds?: readonly string[]; showTrack?: boolean; resizable?: boolean; inline?: boolean; slotId?: string; enabled?: boolean;
    companion?: (group: PaneResizeGroup) => React.ReactNode; onSide?: (source: Conversation, entries: readonly TranscriptEntry[], question: string) => void | boolean },
) {
  /* Existing conversation selection survives navigation; unfinished drafts
     retain their separate ConversationProvider lifecycle. */
  const [openTarget, setOpenTarget] = target;
  /* The conversation whose composer this route was asked to put the caret in. Held
       here because the request is cleared in the same commit that opens the row;
       dropped when the drawer closes. */
  const [composerFocusFor, setComposerFocusFor] = useState<string | null>(null);
  const openRowId = options?.enabled === false ? null : openTarget?.kind === 'row' ? openTarget.id : null;
  /* A track conversation runs on Codex; Claude is a Planner-only backend (#1791). */
  const draftCatalog = useQuery({ ...modelCatalogQueryOptions(transport, { kind: 'provider', provider: 'codex' }, unauthorized),
    enabled: openTarget?.kind === 'draft' });
  const draftCapabilities = useQuery({ queryKey: ['server-version'],
    queryFn: () => runOperation(transport, serverVersionOperation(), unauthorized),
    enabled: openTarget?.kind === 'draft', retry: false });
  const supportsDraftModel = draftCapabilities.data?.conversationCreateModel === true;
  useEffect(() => { if (openRowId === null) setComposerFocusFor(null); }, [openRowId]);
  const scope: PlannerConversationScope | null = openRowId !== null
    ? source.scopeOf(openRowId)
    : null;
  // Shared cache with the Track route, also available for Today and side conversations.
  const imageTrackId = scope?.id ?? null;
  const imageTrack = useQuery({
    ...trackDetailQueryOptions(transport, imageTrackId ?? '', unauthorized),
    enabled: imageTrackId !== null && source.workspaceRoot === null,
  });
  const imageRoot = source.workspaceRoot
    ?? (imageTrack.data === undefined ? null : toTrack(imageTrack.data.track).agentCwd);
  const imageFiles = useMemo(() => imageRoot === null || imageTrackId === null ? null : ({
    root: imageRoot,
    files: createTrackWorkspaceFilesPort(transport, unauthorized, imageTrackId),
  }), [imageRoot, imageTrackId, transport, unauthorized]);
  const routeIntent: ConversationRouteIntent = {
    rows: source.rows, rememberOn: source.rememberOn, ownedCardIds: options?.ownedCardIds,
  };


  const rows = source.rows;
  const store = useConversationStore(transport, unauthorized, scope, routeIntent);
  /* `@` and the Planner's questions only in the Planner's own row; the track the drawer is on ranks its blocks first. */
  const plannerRow = source.planner !== null && openRowId === source.planner.cardId ? source.planner : null;
  const mentionTrigger = useMentionTrigger(useMentionSearch(transport, unauthorized,
    plannerRow === null ? null : plannerRow.areaId, source.scopeId));
  const registry = useConversationRegistry();
  /* The open conversation's own composer: words and images live in the registry per conversation,
       so closing keeps them and switching shows the other conversation's own. */
  const composerId = scope?.cardId ?? null;
  const composer = composerId === null ? EMPTY_COMPOSER : registry.composerOf(composerId);
  const { editComposer } = registry;
  const setComposerText = useCallback<Dispatch<SetStateAction<string>>>((action) => {
    if (composerId === null) return;
    editComposer(composerId, (current) => {
      const text = typeof action === 'function' ? action(current.text) : action;
      return text === current.text ? current : { ...current, text };
    });
  }, [composerId, editComposer]);
  const { editUpload } = registry;
  const uploadState = composerId === null ? NO_UPLOAD : registry.uploadOf(composerId);
  const attachmentStore = useMemo<AttachmentStore>(() => ({
    items: composer.attachments,
    update: (cardId, next) => editComposer(cardId, (current) => {
      const attachments = next(current.attachments);
      return attachments === current.attachments ? current : { ...current, attachments };
    }),
    upload: uploadState,
    editUpload,
  }), [composer.attachments, editComposer, editUpload, uploadState]);
  const attachments = usePlannerAttachments(store.uploadAttachment, composerId ?? '', attachmentStore);
  const [composerFocusRequest, setComposerFocusRequest] = useState(0);
  const shownComposer = useRef(composerId);
  shownComposer.current = composerId;
  const focusComposer = useCallback((conversationId: string) => {
    if (shownComposer.current === conversationId) setComposerFocusRequest((count) => count + 1);
  }, []);
  const edit = useConversationEdit({ conversationId: composerId, transcript: composerId === null ? [] : store.turnsOf(composerId),
    historyReady: store.historyReady, focusComposer });
  /* The one readiness every response action and the continue guidance share. */
  const canContinue = store.runReady && store.runError === null && store.historyReady && !store.sendBlocked && !store.working && !store.stopping;
  /* A conversation's provider is fixed for its life; its model picker offers that provider's group alone. */
  const scopeProvider: AgentProvider = scope === null ? 'codex' : scope.provider;
  const go = useGo();
  const open = store.conversations.find((conversation) => conversation.id === openRowId) ?? null;
  /* While an Edit is held nothing else acts on the conversation; its replace, once sent, blocks as any send does. */
  const respondable = canContinue && edit.held === null;
  const preferences = useUiPreferences();
  const drawerResize = useConversationDrawerResize();
  // Receipts compare the row's completion time, not `updatedAt`, which also moves
  // when the reader queues a message. `null` is never unread.
  const openActivity = rows.find(row => row.id === open?.id);
  useReadReceipt('conversation', openActivity?.id ?? null, openActivity?.lastTurnCompletedAt ?? 0,
    store.historyReady && !store.historyLoading && store.historyError === null);

  /* Only this route's slot is visible, reopenable or sendable here. */
  const sourceScopeId = options?.slotId ?? source.scopeId;
  const draft = registry.draftOf(sourceScopeId);
  const adoptedDraftId = registry.adoptedDraftIdOf(sourceScopeId);
  const creating = draft?.creating ?? false;
  const discardUnsentDraft = registry.discardUnsentDraft;
  /* The new conversation's composer lives in the registry per Track, so closing, `+` and leaving keep its words. */
  const newConversationText = registry.newConversationComposerOf(sourceScopeId);
  const { editNewConversationComposer } = registry;
  const setNewConversationText = useCallback<Dispatch<SetStateAction<string>>>((action) => {
    editNewConversationComposer(sourceScopeId, (current) => typeof action === 'function' ? action(current) : action);
  }, [editNewConversationComposer, sourceScopeId]);

  useConversationDraftRetention({ discardUnsentDraft, sourceScopeId });

  useConversationDraftAdoption({ adoptedDraftId, registry, rows, sourceScopeId, setOpenTarget });

  const { start, withDraft, sendDraft, retryDraft, sendAsNewConversation, closeDrawer } = createConversationDraftActions({
    registry, draft, creating, source, sourceScopeId, transport, supportsDraftModel,
    supportsSideConversation: draftCapabilities.data?.conversationSide === true,
    openDraft: () => setOpenTarget({ kind: 'draft' }), closeView: () => setOpenTarget(null),
    onGone: () => go({ name: 'today' }),
  });
  const startAnother = start;

  useRequestedConversationOpen({ registry, rows, setOpenTarget, setComposerFocusFor, inline: options?.inline });

  useConversationEscape({ open, store });

  /* A draft belonging to another Track is not open here: `draft` is read only
     from this route's provider slot. */
  const contextSourceId = draft?.side?.source_card_id ?? open?.sourceCardId;
  const contextSource = source.rows.find((row) => row.id === contextSourceId);
  const draftOpen = options?.enabled !== false && openTarget?.kind === 'draft' && draft !== null;

  useConversationDraftAutoSend({ draftOpen, draft, creating, registry, sendDraft });

  const existingId = open?.id ?? null;
  const newConversation = useCommittedCallback(existingId, startAnother);
  const interrupt = useCommittedCallback(existingId, store.interrupt);
  const compact = useCommittedCallback(existingId, store.compact);
  const deleteQueuedEntry = useCommittedCallback(existingId, store.deleteQueuedEntry);
  const steerQueuedEntry = useCommittedCallback(existingId, (entry: PendingQueueEntry) => {
    const steer = store.steerQueuedEntry;
    if (steer === undefined) return Promise.reject(new Error('Steering is no longer available in this view.'));
    return steer(entry);
  });
  const setModel = useCommittedCallback(existingId, store.setModel);
  const sendText = useCommittedCallback(existingId, (text: string) => open !== null
    && store.send(open.id, text, attachments.items, true, edit.replacesIn(open.id)) !== null);
  const sideQuestion = useCommittedCallback(existingId, (question: string) => {
    if (open === null || options?.onSide === undefined || !store.historyReady) return false;
    return options.onSide(open, store.turnsOf(open.id), question);
  });
  const plannerAsks = plannerRow?.asks ?? NO_ASKS;
  /* Without a Planner row the drawer lists no ask, so nothing can be answered from here: nothing is sent. */
  const answerAsk = useCommittedCallback(existingId, (askId: number, answers: readonly string[]) => plannerRow === null
    ? Promise.reject(new NotSentError()) : plannerRow.answerAsk(askId, answers));
  const attach = useCommittedCallback(existingId, attachments.attach);
  const removeAttachment = useCommittedCallback(existingId, attachments.remove);
  const { items: attachmentItems, ids: attachmentIds, busy: attachmentBusy, error: attachmentError, atCapacity } = attachments;
  const composerAttachments = useMemo(() => ({ items: attachmentItems, ids: attachmentIds, busy: attachmentBusy,
    error: attachmentError, atCapacity, attach, remove: removeAttachment }),
  [attachmentItems, attachmentIds, attachmentBusy, attachmentError, atCapacity, attach, removeAttachment]);
  const canSteer = store.steerQueuedEntry !== undefined;
  const hasSideConversation = options?.onSide !== undefined;
  const composerView = useMemo(() => ({
    compact: compact,
    compacting: store.compacting,
    attachmentsSupported: store.attachmentsSupported,
    contextUsage: store.contextUsage,
    deleteQueuedEntry: deleteQueuedEntry,
    historyReady: store.historyReady,
    interrupt: interrupt,
    model: store.model,
    modelCatalog: store.modelCatalog,
    pendingQueue: store.pendingQueue,
    pendingQueueOverflow: store.pendingQueueOverflow,
    queueWriteOut: store.queueWriteOut,
    sendBlocked: store.sendBlocked,
    sending: store.sending,
    setModel: setModel,
    steerQueuedEntry: canSteer ? steerQueuedEntry : undefined,
    stopping: store.stopping,
    working: store.working
  }), [compact, store.compacting, store.attachmentsSupported, store.contextUsage, deleteQueuedEntry, store.historyReady, interrupt, store.model, store.modelCatalog, store.pendingQueue, store.pendingQueueOverflow, store.queueWriteOut, store.sendBlocked, store.sending, setModel, store.stopping, store.working, canSteer, steerQueuedEntry]);
  const { replacing, bar: editingBar } = edit;
  const composerNode = useMemo(() => existingId === null ? null : (
            <ChatComposer
              /* Read at mount only, which is what makes it one-shot; the flag is dropped
                               when the drawer closes. */
              focusOnMount={composerFocusFor === existingId}
              focusRequest={composerFocusRequest}
              draft={{ text: composer.text, onChange: setComposerText }}
              disabled={composerView.sendBlocked || !composerView.historyReady || replacing}
              onCompact={scopeProvider === 'codex' && composerView.historyReady && editingBar === undefined ? composerView.compact : undefined}
              sendWaiting={replacing} {...(editingBar === undefined ? {} : { editing: editingBar })}
              /* The images stay with the composer until the store reports them delivered; the press waits out its Edit. */
              showSideCommand={options?.showSideCommand}
              onSideConversation={!hasSideConversation || !composerView.historyReady ? undefined : sideQuestion}
              onSend={sendText}
              allowEmptyText={composerAttachments.items.length > 0}
              /* The queue lives inside the composer, above the field: these messages have
                               not reached the model, so they are not part of the conversation behind it. */
              drawer={(
                <>
                  <PendingQueue
                    /* Its write lock and refusal are the shown conversation's, never carried into the next one. */
                    key={existingId}
                    entries={composerView.pendingQueue}
                    overflow={composerView.pendingQueueOverflow}
                    busy={composerView.sending || composerView.queueWriteOut}
                    onDelete={composerView.deleteQueuedEntry}
                    onSteer={composerView.steerQueuedEntry}
                  />
                  {/* Above the images: those belong to the message being written, so they stay next to its field. */}
                  <PlannerAskDrawer key={`asks-${existingId}`} asks={plannerAsks} onAnswer={answerAsk} />
                  <PlannerAttachmentDrawer attachments={composerAttachments} />
                </>
              )}
              /* Renders nothing until the harness has reported a usage frame. */
              sendAdornment={<ContextRing usage={composerView.contextUsage} />}
              /* `stopping` keeps Stop shown while the interrupt is in flight; `interrupt()`
                               already refuses a second one. */
              onStop={!composerView.compacting && (composerView.working || composerView.stopping) ? composerView.interrupt : undefined}
              onNewConversation={options?.inline === true ? undefined : newConversation}
              mentionTrigger={mentionTrigger}
              /* The kernel reads the selection when it hands a batch to codex, so a change
                               lands on the next turn not yet issued; a REFUSED turn is re-issued and
                               reads it again. */
              /* With `headerActions` unset Astryx does not render that row at all. */
              footerActions={(
                <HStack gap={1} align="center" className={footerStyles.group}>
                  <PlannerAttachButton
                    attachments={composerAttachments}
                    support={{
                      available: composerView.attachmentsSupported,
                      reason: ATTACHED_WORKSPACE_REASON,
                    }}
                    disabled={composerView.sendBlocked || !composerView.historyReady || replacing}
                  />
                  <ModelPill
                    /* Without a scope the catalog read is disabled, so no `unavailable` label can show. */
                    /* An existing conversation keeps issue-time handling: no availability gate here (#1817). */
                    groups={[{ provider: scopeProvider, catalog: composerView.modelCatalog, availability: null }]}
                    provider={scopeProvider}
                    selection={composerView.model}
                    onChange={composerView.setModel}
                    isDisabled={!composerView.historyReady}
                  />
                </HStack>
              )}
            />
  ), [existingId, composerFocusFor, composerFocusRequest, composer.text, setComposerText, composerView,
    replacing, editingBar, options?.showSideCommand, options?.inline, hasSideConversation, sideQuestion,
    sendText, composerAttachments, newConversation, mentionTrigger, scopeProvider, plannerAsks, answerAsk]);

  const renderDrawer = (resizeGroup: PaneResizeGroup | null = null) => (
      <Drawer
        resizeGroup={resizeGroup}
        id={open === null ? undefined : `conversation-${open.id}`}
        inline={options?.inline}
        stacked={options?.stacked}
        companion={options?.companion}
        closeLabel={options?.inline === true ? 'Close side conversation' : 'Close conversation'}
        open={open !== null || draftOpen}
        /* A draft has no name yet, and naming it after the words being typed
           would rename the drawer on every keystroke. */
        title={options?.inline === true ? 'Side conversation · Codex' : open !== null ? conversationName(open) : draftOpen ? 'Untitled' : ''}
        mobileBackLabel="Conversations"
        onClose={closeDrawer}
        resize={options?.resizable === false ? undefined : drawerResize}
        footer={draftOpen ? (
          <>
            {/* The strip is welded to the well's top edge, so it renders before the composer. */}
            {draft != null && (draft.error != null || draft.remedy !== null) && (
              <ChatFooterNotice>
                {draft.error != null && <ChatFooterError message={draft.error} />}
                {draft.remedy === 'retry' && (
                  <ChatFooterRemedy disabled={creating} onClick={retryDraft}>Try again</ChatFooterRemedy>
                )}
                {draft.remedy === 'new-conversation' && (
                  <ChatFooterRemedy disabled={creating} onClick={sendAsNewConversation}>
                    Send as a new conversation
                  </ChatFooterRemedy>
                )}
              </ChatFooterNotice>
            )}
            <ChatComposer disabled={creating} onSend={sendDraft} onNewConversation={options?.inline === true ? undefined : startAnother}
              mentionTrigger={mentionTrigger}
              draft={{ text: newConversationText, onChange: setNewConversationText }}
              /* A track conversation is not a Planner create: no availability gate here (#1817). */
              footerActions={<ModelPill groups={[{ provider: 'codex', catalog: draftCatalog.data ?? null, availability: null }]} provider="codex" selection={draft.model}
                onChange={model => withDraft(draft, current => current.creating || current.sentText !== null
                  ? current : { ...current, model })}
                isDisabled={creating || draft.sentText !== null || !supportsDraftModel} />} />
          </>
        ) : open === null ? undefined : (
          <>
            {/* Each query owns its read retry; accepted sends keep their reconciliation fence (#2068). */}
            {(store.historyError !== null || store.runError !== null) && (
              <ErrorBox
                message={[store.historyError, store.runError].filter((text) => text !== null).join(' ')}
                pending={(store.historyError !== null && store.historyLoading) || (store.runError !== null && store.runLoading)}
                actionLabel={store.historyError !== null && store.runError !== null ? 'Reload conversation'
                  : store.runError !== null ? 'Reload status' : 'Reload history'}
                description={store.runError === null ? undefined
                  : 'The last known status may be out of date. Reloading does not restart the conversation.'}
                onRetry={() => {
                  if (store.historyError !== null) store.retryHistory();
                  if (store.runError !== null) store.retryRun();
                }} />
            )}
            {store.failedSend !== null && (
              <ChatFooterNotice>
                {/* An unknown send says only that: why the answer was lost is the global connection indicator's to say. */}
                <ChatFooterError message={store.failedSend.delivery === 'unknown'
                  ? 'Delivery is unconfirmed.' : notSentMessage(store.failedSend.message)} />
                {store.failedSend.delivery === 'unknown' ? <>
                  {/* Safe without asking: the retry reuses the send's key, so a message that did arrive is not queued twice.
                     No Edit here — an edited message is a new send under a new key, and the first may have arrived. */}
                  <ChatFooterRemedy disabled={store.stalled || store.sending || !store.historyReady}
                    onClick={() => { if (store.failedSend !== null) store.retrySend(store.failedSend.key); }}>
                    Try again
                  </ChatFooterRemedy>
                  {/* Nothing of it comes back to the composer: it may already be delivered, and a read then shows it. */}
                  <ChatFooterRemedy onClick={() => { if (store.failedSend !== null) store.dismissFailedSend(store.failedSend.key); }}>
                    Dismiss
                  </ChatFooterRemedy>
                </> : <>
                  <ChatFooterRemedy disabled={store.stalled || store.sending || !store.historyReady}
                    onClick={() => { if (store.failedSend !== null) store.retrySend(store.failedSend.key); }}>
                    Try again
                  </ChatFooterRemedy>
                  {/* Its words and images go back to the composer together; nothing already there is lost. */}
                  <ChatFooterRemedy onClick={() => { if (store.failedSend !== null) store.discardFailedSend(store.failedSend.key); }}>
                    Edit
                  </ChatFooterRemedy>
                </>}
              </ChatFooterNotice>
            )}
            {options?.sideError != null && (
              <ChatFooterNotice><ChatFooterError message={options.sideError} /></ChatFooterNotice>
            )}
            {store.compacting && <ChatFooterNotice>Compacting conversation context…</ChatFooterNotice>}
            {store.actionError !== null && (
              <ChatFooterNotice><ChatFooterError message={store.actionError} /></ChatFooterNotice>
            )}
            {edit.notice !== null && <ChatFooterNotice tone={edit.notice.tone}>{edit.notice.lines.map((line) =>
              edit.notice?.tone === 'error' ? <ChatFooterError key={line} message={line} /> : <span key={line}>{line}</span>)}</ChatFooterNotice>}
            {store.restart.strip !== null && <ChatFooterNotice tone={store.restart.strip.tone}>
              {[...store.restart.strip.lines, ...store.restart.strip.errors].map((line) => store.restart.strip?.tone === 'error'
                ? <ChatFooterError key={line} message={line} /> : <span key={line}>{line}</span>)}
              {store.restart.strip.action !== null && <ChatFooterRemedy disabled={store.restart.pending}
                onClick={store.restart.start}>{store.restart.strip.action}</ChatFooterRemedy>}</ChatFooterNotice>}
            {/* Not an `alert`: nothing just happened, the condition was already true when
                            this page opened. */}
            {!store.stalled && store.blockedReason !== null && (
              <ChatFooterNotice>
                <ChatFooterError message={store.blockedReason} />
              </ChatFooterNotice>
            )}
            {composerNode}
          </>
        )}
      >
        {(draft?.side !== undefined && draftOpen || open?.sourceCardId !== undefined) && (
          <p>From {contextSource === undefined ? 'an earlier conversation' : conversationName(contextSource)}.
            {' '}Uses a snapshot of loaded text, not complete history. Replies use Codex.</p>
        )}
        {/* Shown back because the composer clears its field on send; nothing here
                    claims they arrived. */}
        {draftOpen && (draft?.text == null
          ? <p>Nothing said yet. What you write starts the conversation.</p>
          : <>
            <p data-nc-turn="you">{draft.text}</p>
            {creating && <p role="status">Sending…</p>}
          </>)}
        {open !== null && (
          <>
            {store.hasEarlier && (
              <button type="button" disabled={store.loadingEarlier} onClick={store.loadEarlier}>
                {store.loadingEarlier ? 'Loading…' : 'Load earlier'}
              </button>
            )}
            {!store.historyReady && store.historyError === null && (
              <p role="status">Loading conversation…</p>
            )}
            {/* Keyed on the conversation: the drawer stays mounted across a switch, and
                          without the key `ChatThread`'s follow-the-newest refs and rail state
                          would be A's applied to B. */}
            {/* Not while the first page is unknown and there is nothing to show:
                          `ChatThread` draws its empty state (with the live dot) for an empty list.
                          `turnsOf` is the second arm so a reopen whose query was collected still
                          shows the remembered transcript. */}
            {(store.historyReady || store.turnsOf(open.id).length > 0 || store.stalled || store.stopFeedback !== null) && (
              <ChatThread
                key={open.id}
                conversation={open}
                imageFiles={imageFiles}
                turns={store.turnsOf(open.id)} editing={edit.marked}
                replacement={store.failedSend?.replaces == null ? null : store.failedSend.echo.id}
                pending={store.pending.has(open.id)}
                cards={source.cards}
                stalled={store.stalled}
                statusUnconfirmed={!store.runReady || store.runError !== null}
                statusLoading={store.runLoading}
                copyText={edit.held === null ? writeClipboardText : undefined}
                regenerateMessage={respondable
                  ? async (message) => { await store.send(open.id, message.text, message.attachments ?? [], false, null); }
                  : undefined}
                editMessage={respondable && store.pendingQueue.length === 0 && store.pendingQueueOverflow === 0
                  && isComposerEmpty(composer) && !attachments.busy ? edit.start : undefined}
                canContinue={canContinue}
                stalledReason={store.blockedReason}
                stopFeedback={store.stopFeedback}
                runningAnchor={store.runningAnchor}
              />
            )}
          </>
        )}
      </Drawer>
  );

  const openConversation = useCommittedCallback(sourceScopeId, (conversation: Conversation) => {
    setOpenTarget({ kind: 'row', id: conversation.id });
  });
  const preferencesRevision = preferences.getSnapshot();
  const { ids: unreadIds } = useMemo(() => ({ revision: preferencesRevision,
    ids: new Set(rows.filter(row => preferences.isUnread('conversation', row.id,
      row.lastTurnCompletedAt ?? 0)).map(row => row.id)) }), [rows, preferences, preferencesRevision]);
  const listNode = useMemo(() => (
      <ChatList
        conversations={store.conversations}
        cards={source.cards}
        unreadIds={unreadIds}
        activeId={existingId}
        /* The two local echoes for the open row only, handed over as facts: the list
                   reads nothing off `Conversation.state`. */
        local={existingId === null ? null : { id: existingId, working: store.working, stalled: store.stalled }}
        showTrack={options?.showTrack ?? true}
        onOpen={openConversation}
      />
  ), [store.conversations, source.cards, unreadIds, existingId, store.working, store.stalled,
    options?.showTrack, openConversation]);

  return {
    isOpen: open !== null || draftOpen,
    close: closeDrawer,
    list: listNode,
    action: <PanelAction label="New conversation" onClick={start}><Icon name="plus" size="sm" /></PanelAction>,
    startConversation: start,
    drawer: renderDrawer(),
    drawerFor: renderDrawer,
  };
}

function TrackRoute({ transport, unauthorized, cardRuntime, recentFiles }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  cardRuntime: CardRuntime;
  recentFiles: RecentFileHistory;
}) {
  const trackId = useRouteParam('/track/');
  const registry = useConversationRegistry();
  const detail = useQuery({
    ...trackDetailQueryOptions(transport, trackId ?? '', unauthorized),
    enabled: trackId !== undefined,
  });
  /* One `Track` per detail read, not per render: the body memoises on it. */
  const detailData = detail.data;
  const track = useMemo(
    () => detailData === undefined ? null : toTrack(detailData.track, trackActivityFrom(detailData.track.id, detailData.overlays)),
    [detailData],
  );
  /* The card Today asked for, if this track has it and it is a conversation card at
   * all — BOTH conversation markers, not just the planner one. A fail-safe with no
   * live producer. */
  const requestedCard = detail.data?.cards.find((card) => card.id === registry.requestedOpenId
    && card.kind === 'codex'
    && (isPlannerHarnessPayload(card.payload) || isAssistantHarnessPayload(card.payload)));
  const detailMatchesRoute = trackId !== undefined && detail.data?.track.id === trackId;
  useEffect(() => {
    if (registry.requestedOpenId === null || detail.isLoading || detail.isFetching) return;
    if (!detailMatchesRoute || requestedCard === undefined) registry.clearOpenRequest();
  }, [detail.isFetching, detail.isLoading, detailMatchesRoute, registry, requestedCard]);

  if (!detail.data || track === null) {
    if (detail.isLoading || detail.isFetching) return null;
    if (detail.error instanceof Error) return <ErrorBox message={readErrorText(detail.error, 'This track could not be loaded.')} onRetry={() => { void detail.refetch(); }} />;
    return <PendingRoute label="Track" owner="features/track" missing />;
  }
  // `detail.data` can still be the previously-viewed track while this one
  // fetches; rendering it under this URL would show the wrong track.
  if (trackId !== undefined && detail.data.track.id !== trackId) return null;

  return (
    <TrackRouteBody
      key={track.id}
      transport={transport}
      unauthorized={unauthorized}
      track={track}
      canReopenTrack={detail.data.can_reopen}
      canCloseTrack={detail.data.can_close}
      cards={detail.data.cards}
      overlays={detail.data.overlays}
      cardRuntime={cardRuntime}
      recentFiles={recentFiles}
    />
  );
}

/**
 * The Notifications aside from `activity.items`: one row per item, in the kernel's order (newest
 * first) and in the kernel's words. No item names a card: both sources belong to the Planner.
 */
function trackNotifications(items: TrackActivity['attentionItems']): readonly TrackInputNotification[] {
  return items.map((item): TrackInputNotification => ({
    key: item.key,
    kind: item.source === 'ask' ? 'ask' : 'planner-down',
    text: item.text,
    atMs: item.atMs,
  }));
}

function TrackRouteBody({
  transport, unauthorized, track, canReopenTrack, canCloseTrack, cards, overlays, cardRuntime, recentFiles, reportEvidence, panelContent,
}: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  track: Track;
  canReopenTrack: boolean;
  canCloseTrack: boolean;
  cards: TrackDetailWire['cards'];
  overlays: TrackDetailWire['overlays'];
  cardRuntime: CardRuntime;
  recentFiles: RecentFileHistory;
  reportEvidence?: ReactNode;
  panelContent?: ReactNode;
}) {
  useTrackViewState(track.id);
  // The same key and comparison point the rail uses: the overlay's completion
  // high-water mark, never the row's `updatedAt`.
  // A completed navigation also acknowledges a repeated selection of this already rendered Track.
  const navigationCompletedAt = useRouterState({ select: (state) => state.loadedAt });
  useReadReceipt('track', track.id, track.activityAt ?? 0, true, navigationCompletedAt);
  const trackMutations = useTrackMutations(transport, unauthorized);
  const conversationMutations = useTrackConversationMutations(transport, track.id, unauthorized);
  const openMobileSection = useOpenMobileSection();
  const mobileHeaderActionsHost = useMobileHeaderActionsHost();
  const mobileHeaderTitleHost = useMobileHeaderTitleHost();
  const mobileTrackChoices = useMobileTrackChoices();
  const go = useGo();
  const goSameTrack = useGoSameTrack();
  const fileNavigation = useTrackFileNavigation();
  const { openPanel, closePanel } = useTrackPanelNavigation();
  const requestedCardId = useRouteCardId();
  const rawRequestedFilePath = useRouteFilePath();
  const requestedFilePath = requestedCardId === null ? rawRequestedFilePath : null;
  const [recentFilePaths, setRecentFilePaths] = useState<readonly string[]>(
    () => recentFiles.read(track.id),
  );
  const fileReturnFocusRef = useRef<HTMLElement | null>(null);
  const previousFilePathRef = useRef(requestedFilePath);
  useEffect(() => {
    const previous = previousFilePathRef.current;
    previousFilePathRef.current = requestedFilePath;
    if (previous === null || requestedFilePath !== null) return;
    requestAnimationFrame(() => {
      const opener = fileReturnFocusRef.current;
      fileReturnFocusRef.current = null;
      const target = opener?.isConnected === true
        ? opener
        : document.querySelector<HTMLElement>('[data-nc-report]');
      target?.focus({ preventScroll: true });
    });
  }, [requestedFilePath]);
  /* The URL is turned into props here: `features-no-app` keeps `TrackPage` a pure
   * renderer. A live `?card=` wins over a hand-edited `?panel=`. */
  const routePanel = useRoutePanel();
  /* Whether `?panel=` describes anything at all on this viewport. */
  const compactViewport = useCompactViewport();
  /* `?from=` is a property of this visit, so every same-track move hands it back
   * explicitly (`go` clears what it is not given); crossing tracks drops it. */
  const routeFrom = useRouteFrom() ?? undefined;
  const cardRegistry = cardRuntime.registry;
  // The same predicate the planner entry resolves by, imported rather than copied.
  const plannerCard = plannerCardIn(cards);
  const registry = useConversationRegistry();
  /* The track's assistant conversations; the server's list predicate is
   * `role == Assistant`, so the planner row is injected below. */
  const conversationsQuery = useQuery(trackConversationsQueryOptions(transport, track.id, unauthorized));
  const assistantRows = useMemo(() => conversationsQuery.data ?? [], [conversationsQuery.data]);
  const trackTitle = trackDisplayTitle(track.title);
  // Planner is the one conversation projected from a card; its `state` is the
  // drawer's baseline, not an indicator — the row's dot reads `activity.cards`.
  const plannerRow = useMemo<Conversation | null>(() => plannerCard === undefined ? null : {
    id: plannerCard.id,
    trackId: track.id,
    trackTitle,
    title: plannerCard.title,
    kind: 'shared-spec',
    state: plannerCard.runtime?.status ?? null,
    updatedAt: plannerCard.runtime?.updated_at_ms ?? plannerCard.updated_at,
    // The receipt's comparison point; absent on a legacy snapshot.
    lastTurnCompletedAt: plannerCard.runtime?.last_turn_completed_ms ?? null,
  }, [plannerCard, track.id, trackTitle]);
  /* Redeeming the planner-open intent: `armed` is already "this track, this visit",
   * and the planner card's id exists only once the detail has landed. `disarm()`
   * before the open, unconditionally: an intent left armed on this entry would
   * fire on the next visit to it (Back reaches one). */
  const plannerOpenIntent = usePlannerOpenIntent(track.id);
  useEffect(() => {
    if (!plannerOpenIntent.armed) return;
    plannerOpenIntent.disarm();
    if (plannerCard === undefined) return;
    registry.requestOpen(plannerCard.id, { focusComposer: true });
  }, [registry, plannerCard, plannerOpenIntent]);
  /* Every row carries the track's title, unconditionally and before the planner
       row is considered. */
  const placedRows = useMemo(
    () => assistantRows.map((row) => ({ ...row, trackTitle })),
    [assistantRows, trackTitle],
  );
  const rows = useMemo<readonly Conversation[]>(
    () => plannerRow === null ? placedRows : [plannerRow, ...placedRows],
    [placedRows, plannerRow],
  );
  const openAsks = useMemo(() => openAsksOf(track.attentionItems), [track.attentionItems]);
  /* `'rows'` unconditionally: an empty list with a `+` over it is the whole feature. */
  const chat = useConversationPanel(
    transport,
    unauthorized,
    {
      scopeId: track.id,
      rows,
      /* The track's own overlay-derived verdicts (`toTrack(detail, detailActivity)`). */
      cards: track.cards,
      /* These rows are on a track the reader can be sent to, so Today may hold and
               open them; the store checks each row's `trackId` against this. */
      rememberOn: track.id,
      workspaceRoot: track.agentCwd,
      derivedCardId: (idempotencyKey) => trackConversationCardId(track.id, idempotencyKey),
      scopeOf: (conversationId) => {
        const row = rows.find(candidate => candidate.id === conversationId);
        /* `id: row.trackId`, never `track.id`: this is the line the `rememberOn`
         * comparison rests on, and written the other way it would be a tautology. */
        return row === undefined ? null : {
          id: row.trackId,
          provider: plannerCard !== undefined && row.id === plannerCard.id ? plannerProviderOf(plannerCard.payload) : 'codex',
          title: trackTitle, cardId: row.id, cardTitle: row.title,
          updatedAt: row.updatedAt, kind: row.kind, state: row.state,
        };
      },
      create: conversationMutations.create,
      refresh: conversationMutations.refresh,
      planner: plannerCard === undefined ? null : {
        cardId: plannerCard.id, areaId: track.areaId, asks: openAsks,
        answerAsk: (askId, answers) => trackMutations.answerAsk(track.id, askId, answers),
      },
    },
    { showTrack: false },
  );
  /* The fallback clear: a request for a card this track has, while the list that
   * would open it could not be read. Not "the read failed", which would also
   * throw away an openable planner row. */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null || !conversationsQuery.isError) return;
    if (rows.some((row) => row.id === requestedOpenId)) return;
    registry.clearOpenRequest();
  }, [conversationsQuery.isError, registry, rows]);
  /* The runtime half of the TASKS panel, keyed `['track-report', trackId]`: the key
   * `invalidation-plan` refreshes on every `task.*` event and `track.report_edited`. */
  /* Read before the query: the poll's interval depends on the rows this report can produce. */
  const report = useMemo(() => readTrackReport(cards), [cards]);
  const reportBlocks = report?.blocks ?? null;
  const verdictsQuery = useQuery(
    trackTaskVerdictsQueryOptions(transport, track.id, unauthorized, reportBlocks),
  );
  const verdicts = verdictsQuery.data;
  const { outline, tasks: joinedTasks } = useMemo(() => ({
    outline: deriveReportOutline(reportBlocks),
    /* The verdicts land a round-trip after the declarations; `undefined` renders
           the statusless list rather than a hole. */
    tasks: deriveReportTasks(reportBlocks, verdicts),
  }), [reportBlocks, verdicts]);
  /* The CARDS module lists cards that have a surface; unclaimed kinds stay listed,
   * and `originalIndex` is bound before the filter so the merge keeps wire order. */
  const panelCards = useMemo(() => {
    const { visible, unknown } = partitionTrackCards(cardRegistry, cards);
    return [...visible, ...unknown]
      .sort((left, right) => left.originalIndex - right.originalIndex)
      .map((slot) => slot.wire);
  }, [cardRegistry, cards]);
  const gridItems: readonly BoardHostItem[] = useMemo(() => {
    const { visible } = partitionTrackCards(cardRegistry, cards);
    return [...visible]
      .sort((left, right) => {
        const sort = left.wire.sort - right.wire.sort;
        return sort !== 0 ? sort : left.originalIndex - right.originalIndex;
      })
      .map((slot) => Object.freeze({
        card: slot.card,
        title: slot.wire.title ?? slot.wire.kind,
        originalIndex: slot.originalIndex,
        /* The kernel's bit, carried straight through, so the board and the CARDS panel
                   cannot disagree about which cards are the kernel's. */
        deletable: slot.wire.deletable,
        /* The kernel's verdict for the card, from the same overlay the CARDS row reads. */
        activity: cardActivityOf(track, slot.card.id),
      }));
  }, [cardRegistry, cards, track]);
  const inputNotifications = useMemo(
    () => trackNotifications(track.attentionItems),
    [track.attentionItems],
  );
  /* Stable across renders that do not change the overlays, so a live table is
     not handed a new resolver identity on every keystroke elsewhere. */
  const resolveOverlay = useCallback(
    (source: string) => trackOverlayPayload(track.id, overlays, source),
    [track.id, overlays],
  );
  /* One query per `chart.series` block, keyed by the block's rev; opening the
       report is what makes the kernel fetch the data. */
  const resolveSeries = useReportSeriesResolver(transport, track.id, reportBlocks, unauthorized);
  /* One polled read of the track's previews, only while the report has a `preview` block. */
  const resolvePreview = useReportPreviewResolver(transport, track.id, reportBlocks, unauthorized);
  const previewViewports = useReportPreviewViewports(track.id);
  /* The cards this route can open, asked of the registry through the list the board
   * draws. A worker card whose kind no entry claims is `unknown` and its `?card=`
   * is bounced; its task keeps its `workerCardId` regardless, because the TASKS
   * row looks its activity verdict up by that id. */
  const tasks = useCurrentTaskRows(track.id, joinedTasks);
  const openableCards = useMemo(() => new Set(gridItems.map((item) => item.card.id)), [gridItems]);
  const knownCard = requestedCardId !== null
    && gridItems.some((item) => item.card.id === requestedCardId);
  useEffect(() => {
    if (requestedCardId === null || knownCard) return;
    // Bouncing an unopenable `?card=` must drop only that parameter: the panel,
    // return surface and block anchor describe where the reader is.
    goSameTrack(track.id, { card: undefined }, { replace: true });
  }, [goSameTrack, knownCard, requestedCardId, track.id]);
  /* `?panel=` is a compact concept: above the breakpoint `TrackPage` would put
   * `inert` on the desktop panel. Corrected here with `replace`, and the
   * injection below is gated on the viewport too, because on a cold start this
   * effect has not run at first paint. */
  useEffect(() => {
    if (compactViewport || routePanel === null) return;
    goSameTrack(track.id, { panel: undefined }, { replace: true });
  }, [compactViewport, goSameTrack, routePanel, track.id]);
  /* One confirm for both delete gestures. Nothing navigates on success: the
   * `knownCard` effect bounces `?card=<deleted>` off the URL, and a `go()` here
   * would race it. */
  const cardDeletion = useDeleteConfirm(
    (cardId, signal) => trackMutations.removeCard(track.id, cardId, signal), writeFailureText(DELETE_FAILURES, DELETE_TEXT),
  );

  /* Adding a card: the menu is the registry's own list, so this route only decides
   * which endpoint a kind takes — `atomic` kinds have an endpoint named after the
   * kind, `generic` ones go through `POST /api/tracks/:id/cards` with the entry's
   * `claim` kind. An unhandled strategy throws rather than creating nothing. */
  const addMenuEntries = useMemo(() => cardAddMenuEntries(cardRegistry), [cardRegistry]);
  const listDirectory = useMemo(
    () => createDirectoryLister(transport, unauthorized),
    [transport, unauthorized],
  );
  const reportFiles = useMemo(
    () => createTrackWorkspaceFilesPort(transport, unauthorized, track.id),
    [track.id, transport, unauthorized],
  );
  const [cardDraft, setCardDraft] = useState<CardAddMenuEntry | null>(null);
  const [creatingCard, setCreatingCard] = useState(false);
  /* A failed create has to be sayable with no dialog on screen: a fieldless kind
   * never opens one, so the message gets a route-level surface. */
  const cardCreateFeedback = useOperationFeedback();
  const newCardFieldRef = useRef<HTMLInputElement | null>(null);
  /* A create that lands after the reader has left must not steer them: one
   * `AbortController` per attempt, aborted on unmount, read before the navigation
   * and every state write. */
  const activeCardCreate = useRef<AbortController | null>(null);
  useEffect(() => () => { activeCardCreate.current?.abort(); }, []);

  /* One add-card intent is one `Idempotency-Key` (#2131): Try again, a Create of the same draft and a pick of the same
   * fieldless kind resend the held key and body, so a retry after a lost answer joins the card the first attempt made.
   * A final outcome releases it; another draft is a new intent. The policy is `new-card/create.ts`; this only wires it. */
  const cardIntent = useKeyedIntent<CardDraft, KeyedCardBody>(sameCardDraft);
  const cardCreatePort: CardCreatePort = {
    createTerminal: (body, key) => trackMutations.createTerminal(track.id, body, key),
    createCodex: (body, key) => trackMutations.createCodex(track.id, body, key),
    createCard: (body, key) => trackMutations.createCard(track.id, body, key),
  };

  /* The create navigates to the new card, the same landing `onOpenCard` gives. `resent`: the held request goes again. */
  const runCardCreate = (draft: CardDraft, keyed: KeyedRequest<CardDraft, KeyedCardBody> | null, resent: boolean) => {
    /* Only the newest gesture may own the landing: a superseded attempt is aborted so
           it neither steers nor clears a busy state that now belongs to the newer attempt. */
    activeCardCreate.current?.abort();
    const controller = new AbortController();
    activeCardCreate.current = controller;
    setCreatingCard(true);
    void cardCreateFeedback
      .run(
        Promise.resolve().then(() => sendCardCreate(cardCreatePort, draft, keyed)).then((card) => {
          if (keyed !== null) cardIntent.release(keyed);
          if (controller.signal.aborted) return;
          setCardDraft(null);
          goSameTrack(track.id, { card: card.id });
        }, (error: unknown) => {
          if (keyed !== null && cardCreateEnded(error, resent)) cardIntent.release(keyed);
          throw error;
        }),
        cardCreateFailureText(draft.entry.label, resent),
        () => controller.signal.aborted,
      )
      .finally(() => {
        /* Only the attempt that still owns the busy state may clear it; both ways of
                   losing ownership abort, so `aborted` is the whole test. */
        if (controller.signal.aborted) return;
        activeCardCreate.current = null;
        setCreatingCard(false);
      });
  };

  const submitNewCard = (entry: CardAddMenuEntry, values: NewCardValues) => {
    const draft: CardDraft = { entry, values };
    /* Read at click time from `<html data-theme>`, not `useTheme()`: subscribing
           would remount any live terminal on every theme toggle. A resent request keeps its first press's theme. */
    const body = keyedCardBodyOf(draft, readHostThemeRgb(), cardRegistry);
    if (body === null) { runCardCreate(draft, null, false); return; }
    const resent = cardIntent.held !== null && sameCardDraft(cardIntent.held.draft, draft);
    runCardCreate(draft, cardIntent.request(draft, () => body), resent);
  };
  const heldCardCreate = cardIntent.held;
  const retryCardCreate = heldCardCreate === null || creatingCard ? null
    : () => { runCardCreate(heldCardCreate.draft, heldCardCreate, true); };

  /* A kind with nothing to ask is created on the spot; one with fields opens the form. */
  const pickCardKind = (entry: CardAddMenuEntry) => {
    cardCreateFeedback.clear();
    if (entry.fields.length === 0) submitNewCard(entry, {});
    else setCardDraft(entry);
  };
  const backlinksQuery = useQuery(trackBacklinksQueryOptions(transport, track.id, unauthorized));
  const backlinks = backlinksQuery.data;

  /* A `neige://wave/…` citation. Same track reveals immediately (so an unchanged
   * hash still flashes) then records the hash; the route body is keyed by track
   * id, so the document is preserved. */
  const arrivalAnchorId = useRouteHash();
  const openReportLink = (target: ReportLinkTarget) => {
    if (target.trackId === track.id) {
      if (target.blockId !== null) revealReportAnchor(target.blockId);
      // Same track: a move within the report, so the return surface survives and the
      // panel closes.
      go({ name: 'track', trackId: target.trackId, blockId: target.blockId ?? undefined, from: routeFrom });
      return;
    }
    // Another track: a real navigation. Nothing carries over.
    go({ name: 'track', trackId: target.trackId, blockId: target.blockId ?? undefined });
  };

  const rememberReportFile = (target: ReportFileLinkTarget) => {
    const relativePath = parseWorkspaceRelativeFilePath(target.path)?.path ?? null;
    if (relativePath === null) return null;
    setRecentFilePaths(recentFiles.record(track.id, relativePath));
    return relativePath;
  };

  const openReportFile = (target: ReportFileLinkTarget) => {
    const relativePath = parseWorkspaceRelativeFilePath(target.path)?.path ?? null;
    if (relativePath === null) return;
    if (requestedFilePath === null && document.activeElement instanceof HTMLElement) {
      fileReturnFocusRef.current = document.activeElement;
    }
    fileNavigation.openFile(track.id, relativePath);
  };

  /* A `neige://source/…` citation opens the source panel: route-local state, not
   * the URL, keyed to this body so leaving the track drops it. Phone and desktop
   * share the target. */
  const [sourceTarget, setSourceTarget] = useState<ReportSourceLinkTarget | null>(null);
  const sourceOpen = sourceTarget !== null;
  const openReportSource = (target: ReportSourceLinkTarget) => { setSourceTarget(target); };
  const closeReportSource = () => { setSourceTarget(null); };

  const closeBoard = () => {
    if (requestedFilePath !== null) {
      fileNavigation.closeFile(track.id);
      return;
    }
    go({ name: 'track', trackId: track.id, from: routeFrom }, { replace: true });
  };

  const boardOpen = knownCard || requestedFilePath !== null;

  const openReportAnchor = (blockId: string) => {
    revealReportAnchor(blockId);
    go({ name: 'track', trackId: track.id, blockId, from: routeFrom });
  };

  return (
    <>
    <TrackStage>
    <TrackPage
      panelContent={panelContent}
      mobilePanelObscured={chat.isOpen || sourceOpen}
      mobileHeaderActionsHost={mobileHeaderActionsHost}
      mobileHeaderTitleHost={mobileHeaderTitleHost}
      mobileTitleReadView={mobileTrackChoices === null ? undefined : (controls) => <TrackSelector
        track={track} {...mobileTrackChoices(track.areaId)} controls={controls}
        onSelectTrack={(trackId) => go({ name: 'track', trackId, from: 'area' })} />}
      track={track}
      canReopenTrack={canReopenTrack}
      canCloseTrack={canCloseTrack}
      cards={panelCards}
      /* Derived from the report's own blocks, so the panel and the document
         cannot disagree about what tasks exist. */
      tasks={tasks}
      openableCards={openableCards}
      outlineItems={outline}
      /* Tasks and the mobile Outline share one anchor landing. The URL carries
         it too, so the reader can hand the destination to somebody else. */
      onOpenTask={openReportAnchor}
      onOpenOutline={openReportAnchor}
      cardsAction={<AddCardMenu entries={addMenuEntries} onSelect={pickCardKind} />}
      recentFiles={<RecentFiles
        paths={recentFilePaths}
        onOpen={(path) => openReportFile({ path })}
      />}
      onOpenCard={(cardId) => { go({ name: 'track', trackId: track.id, cardId, from: routeFrom }); }}
      onDeleteCard={cardDeletion.request}
      board={<>
        <CardGridOverlay
          open={knownCard}
          items={gridItems}
          host={cardRuntime.host}
          activeCardId={requestedCardId}
          onRemoveCard={cardDeletion.request}
          onClose={knownCard ? closeBoard : undefined}
        />
        {requestedFilePath !== null && (
          <ReportFileViewer
            key={requestedFilePath}
            path={requestedFilePath}
            files={reportFiles}
            linkPreview={{ files: reportFiles, trackId: track.id, report }}
            fileRoot={track.agentCwd}
            wide={gridItems.length === 0}
            onClose={closeBoard}
            onFileOpened={(path) => { rememberReportFile({ path }); }}
            onOpenFileLink={openReportFile}
          />
        )}
      </>}
      onCloseBoard={boardOpen ? closeBoard : undefined}
      /* Gated on the viewport too: the effect above cannot have run yet on a desktop
               cold start, and one render "open" is one render with the desktop panel `inert`. */
      panel={renderedMobilePanel(routePanel, {
        compact: compactViewport,
        overlayOpen: requestedCardId !== null || rawRequestedFilePath !== null,
      })}
      onOpenPanel={(kind) => { openPanel(track.id, kind); }}
      onClosePanel={() => { closePanel(track.id); }}
      /* `?from=` is the whole memory of how the reader got here; absent means Pages. */
      mobileBackLabel={routeFrom === 'area' ? 'Tracks' : 'Pages'}
      onMobileBack={() => {
        if (routeFrom === 'area') openMobileSection({ kind: 'tracks', areaId: track.areaId });
        else openMobileSection({ kind: 'pages' });
      }}
      report={<><ReportDocument
        report={report}
        /* `overlay.set` already invalidates this track's detail, so a plugin push
                   re-renders the block without the report being rewritten. */
        resolveOverlay={resolveOverlay}
        resolveSeries={resolveSeries}
        resolvePreview={resolvePreview}
        previewViewports={previewViewports}
        taskVerdicts={verdicts}
        taskRows={tasks}
        renderTaskExecution={(task, expanded) => <TaskRecovery
          key={`${track.id}:${task.key}`} trackId={track.id} taskKey={task.key} expanded={expanded}
          transport={transport} unauthorized={unauthorized}
          openableWorkerIds={openableCards}
          openWorker={(cardId) => { go({ name: 'track', trackId: track.id, cardId, from: routeFrom }); }}
        />}
        rail={<ReportOutline items={outline} />}
        backlinkCounts={backlinks === undefined ? undefined : backlinkCountsByBlock(backlinks.backlinks)}
        onOpenLink={openReportLink}
        onOpenFileLink={openReportFile}
        onOpenSourceLink={openReportSource}
        fileRoot={track.agentCwd}
        linkPreview={{ files: reportFiles, trackId: track.id, report }}
        arrivalAnchorId={arrivalAnchorId}
        empty={<ReportEmpty
          lead="This track has not taken shape yet."
          hints={[
            'Say what you want in the conversation — the agent works it out with you and writes it up here.',
            'It stays with the track, so it is here the next time you open it.',
          ]}
        />}
      />{reportEvidence}</>}
      backlinks={backlinks !== undefined && backlinks.backlinks.length > 0
        ? (
          <ReportBacklinks
            trackId={track.id}
            backlinks={backlinks}
            onOpen={(trackId, blockId) => {
              // Same track keeps the return surface; a backlink into another track
              // is a departure and carries nothing.
              go({ name: 'track', trackId, blockId, from: trackId === track.id ? routeFrom : undefined });
            }}
          />
        )
        : undefined}
      conversationList={chat.list}
      conversationAction={chat.action}
      onStartConversation={chat.startConversation}
      conversationOpen={chat.isOpen}
      inputNotifications={inputNotifications}
      /* Both kinds of item are the Planner's, so both reply to it: sending it a message is what
         answers an ask and what makes a stopped Planner continue. */
      onReply={plannerCard === undefined ? undefined : () => {
        registry.requestOpen(plannerCard.id, { focusComposer: true });
      }}
      onDismiss={(key) => trackMutations.dismissActivityItem(track.id, key)}
      onRenameTrack={(title) => trackMutations.patch(track.id, track.areaId, { title }).then(() => undefined)}
      onReopenTrack={() => trackMutations.patch(track.id, track.areaId, { closed: false }).then(() => undefined)}
      onCloseTrack={() => trackMutations.patch(track.id, track.areaId, { closed: true }).then(() => undefined)}
      onDeleteTrack={(signal) => trackMutations.remove(track.id, track.areaId, signal)}
      onTrackDeleted={() => go({ name: 'today' })}
    />
    </TrackStage>
    {/* Keyed by kind: switching kinds is a different form, and a shared mount
        would carry the previous kind's typed values into it. */}
    <Dialog
      open={cardDraft !== null}
      onClose={() => setCardDraft(null)}
      title={cardDraft === null ? '' : `New ${cardDraft.label} card`}
      initialFocusRef={newCardFieldRef}
    >
      {cardDraft !== null && (
        <NewCardForm
          key={cardDraft.type}
          entry={cardDraft}
          submitting={creatingCard}
          error={cardCreateFeedback.error}
          onRetry={retryCardCreate}
          listDirectory={listDirectory}
          firstFieldRef={newCardFieldRef}
          onCancel={() => setCardDraft(null)}
          onSubmit={(values) => submitNewCard(cardDraft, values)}
        />
      )}
    </Dialog>
    <ConfirmDialog
      open={cardDeletion.open}
      title={DELETE_CARD_COPY.title}
      description={DELETE_CARD_COPY.description}
      confirmLabel={DELETE_CARD_COPY.confirmLabel}
      confirmBusyLabel="Deleting…"
      confirmState={cardDeletion.pending ? 'busy' : 'ready'}
      onConfirm={cardDeletion.confirm}
      onCancel={cardDeletion.cancel}
    />
    <OperationFeedback feedback={cardDeletion.feedback} />
    {/* Only while the dialog is closed: `NewCardForm` renders the same `error` inline. */}
    {cardDraft === null && <OperationFeedback feedback={cardCreateFeedback}
      action={retryCardCreate === null ? undefined : <Button label="Try again" variant="ghost" onClick={retryCardCreate} />} />}
    {/* The source card is painted over the conversation's, which stays in the DOM
            underneath and so is `inert` for the duration. The wrapper is a static block
            so the drawer's absolute box still resolves against `.main`. */}
    <div data-nc-conversation-drawer-host="" inert={sourceOpen}>
      {chat.drawer}
    </div>
    <ReportSourceDrawer
      transport={transport}
      trackId={track.id}
      target={sourceOpen ? sourceTarget : null}
      unauthorized={unauthorized}
      onClose={closeReportSource}
    />
    </>
  );
}
