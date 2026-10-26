import type { DialogOrigin } from '../../ui/dialog/public.tsx';
import { useWorkspaceGuide, workspaceGuideFacts } from './workspace-guide.tsx';
import { sidebarTrackGroups } from './sidebar-track-groups.ts';
import { useMobileNavigationPresence } from './mobile-navigation-presence.ts';
import { floatingControlClassName, supportsAnchoredHints, GuideProvider, GuideAnchor } from '../../ui/floating-control/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { useVisibleViewport } from '../../ui/viewport/public.ts';
import { useCommittedCallback } from '../../ui/state/committed-callback.ts';
import { mobileFontClassName } from '../../ui/mobile-font/public.ts';
// The layout shell every route renders inside: the workspace rail plus the matched
// route's outlet. The shell owns the workspace read and the area/track mutations;
// `Sidebar` stays presentational.

import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef, useCallback, useLayoutEffect, useMemo, type ReactNode } from 'react';

import { useUiPreferences } from '../providers/ui-preferences.tsx';
import { useReadReceipts } from '../providers/read-receipts.tsx';
import { MARK_UNREAD_TEXT, READ_RECEIPT_FAILURES } from '../../../../core/domain/read-receipts.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT, trackDisplayTitle, userVisibleTracks } from '../../../../core/domain/track.ts';
import { OperationFeedback, useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import type { Track } from '../../../../core/domain/track.ts';
import { AREA_CREATE_FAILURES, AREA_CREATE_TEXT, AREA_PATCH_FAILURES, AREA_PATCH_TEXT, visibleAreas } from '../../../../core/domain/area.ts';
import { ApiError, classifyFailure, NotSentError, refusedText, writeFailureOf, writeFailureText } from '../../../../core/domain/failure-class.ts';
import type { Area, NewAreaBody } from '../../../../core/domain/area.ts';
import {
  AreaEditorForm, type AreaEditorPatch, type AreaEditorValues,
} from '../../features/area/editor/public.tsx';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { createDirectoryLister } from '../providers/directory.ts';
import { workspaceActivityErrorText, workspaceReadErrorText } from '../providers/query-read-feedback.ts';
import {
  AreaCreatePreflightError, useAreaMutations, useTrackMutations, useTrackTemplates, useWorkspace,
} from '../providers/queries.ts';
import { useKeyedIntent, type KeyedRequest } from '../providers/idempotency-key.ts';
import { routeParamFromPath, useCurrentPath, useGo, useNewTrackBack, useRouteCardId, useRouteFilePath, useTrackPanelNavigation } from '../router/navigation.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { MobileWorkspaceHeader } from './mobile-header.tsx';
import { MobileTracks } from './mobile-tracks.tsx';
import { MobilePages } from './mobile-pages.tsx';
import { SettingsOverlay, settingsSectionForPath } from './settings-overlay.tsx';
import { useDrawerWidthHost } from './drawer-width.tsx';
import type { DrawerResize } from '../../ui/drawer/public.tsx';
import { Sidebar } from './sidebar.tsx';
import { ProviderAuthenticationNotice } from './provider-authentication.tsx';
import styles from './shell.module.css';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { MobileHistory } from './mobile-history.tsx';
import { EdgeSwipe } from '../../ui/edge-swipe/public.tsx';
import { useMobileHistoryData } from './mobile-history-data.ts';

type Scope = Readonly<{
  track: Pick<Track, 'id' | 'areaId' | 'title'>;
  conversations: readonly Conversation[];
  selectedConversationId: string | null;
  onNew: () => void;
  onClose: () => void;
  onOpen: (conversation: Conversation) => void;
  conversationOpen: boolean;
  onArchive?: (cardId: string, archived: boolean) => Promise<void>;
}>;
type RegisteredScope = Scope & Readonly<{ owner: object }>;
const ScopeContext = createContext<Readonly<{ current: RegisteredScope | null; register: (scope: RegisteredScope | null) => void }> | null>(null);

export function MobileConversationProvider({ children }: Readonly<{ children: ReactNode }>) {
  const [current, setCurrent] = useState<RegisteredScope | null>(null);
  const register = useCallback((scope: RegisteredScope | null) => { setCurrent(scope); }, []);
  const value = useMemo(() => ({ current, register }), [current, register]);
  return <ScopeContext.Provider value={value}>{children}</ScopeContext.Provider>;
}

export function useMobileConversationScope() { return useContext(ScopeContext)?.current ?? null; }

/** The rendered route declares its owner; the shell never infers a daily/system Track from the URL. */
export function useMobileConversationOwner(owner: Scope) {
  const register = useContext(ScopeContext)?.register;
  const { track, conversations, selectedConversationId, conversationOpen } = owner;
  // One registration lifetime, including a fresh mount at the same pathname.
  const [identity, setIdentity] = useState(() => ({ trackId: track.id, token: {} }));
  if (identity.trackId !== track.id) setIdentity({ trackId: track.id, token: {} });
  const onNew = useCommittedCallback(track.id, owner.onNew);
  const onClose = useCommittedCallback(track.id, owner.onClose);
  const onOpen = useCommittedCallback(track.id, owner.onOpen);
  const onArchive = useCommittedCallback(track.id, async (cardId: string, archived: boolean) => { await owner.onArchive?.(cardId, archived); });
  const canArchive = owner.onArchive !== undefined;
  const scope = useMemo<RegisteredScope>(() => ({
    owner: identity.token, track: { id: track.id, areaId: track.areaId, title: track.title }, conversations, selectedConversationId, conversationOpen,
    onNew, onClose, onOpen, onArchive: canArchive ? onArchive : undefined,
  }), [identity.token, track.id, track.areaId, track.title, conversations, selectedConversationId, conversationOpen, onNew, onClose, onOpen, canArchive, onArchive]);
  useLayoutEffect(() => {
    register?.(scope);
    return () => { register?.(null); };
  }, [register, scope]);
}

export type AppShellProps = Readonly<{
  onOpenDemo?: () => void;
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  onOpenSettings: () => void;
  /** Settings › Plugins, from the rail's account menu. */
  onOpenPlugins: () => void;
  onSignOut: () => void;
  /** Pinned by tests so `pinned_at` assertions are stable. */
  nowMs?: number;
  userLabel?: string;
  initialWorkspaceGuide?: boolean;
  guideContent?: ReactNode;
  guideFacts?: Readonly<Record<string, boolean>>;
}>;

/** Routes can reopen an Area's Tracks or recent Pages; the primary entry starts at Areas. */
type MobileSection = Readonly<{ kind: 'areas'; areaId: string | undefined }> | Readonly<{ kind: 'pages' }> | Readonly<{ kind: 'tracks'; areaId: string | undefined; returnTo?: 'areas' | 'report' }>;
type OpenMobileSection = (section: MobileSection) => void;

const MobileSectionContext = createContext<OpenMobileSection | null>(null);

/** App owns header geometry; the feature owns the existing action menu and focus. */
const MobileHeaderActionsContext = createContext<HTMLElement | null>(null);
export function useMobileHeaderActionsHost(): HTMLElement | null {
  return useContext(MobileHeaderActionsContext);
}

const DrawerResizeContext = createContext<DrawerResize | undefined>(undefined);
export const DrawerResizeProvider = DrawerResizeContext.Provider;
/** The resize contract for the route's conversation drawer (`./drawer-width.tsx`); `undefined` outside the shell, which means no handle. */
export function useConversationDrawerResize(): DrawerResize | undefined { return useContext(DrawerResizeContext); }

const MobileHeaderTitleContext = createContext<HTMLElement | null>(null);
export function useMobileHeaderTitleHost(): HTMLElement | null {
  return useContext(MobileHeaderTitleContext);
}


type AreaEditorTarget = Readonly<{ kind: 'create'; origin: DialogOrigin }> | Readonly<{ kind: 'edit'; area: Area; origin: DialogOrigin }>;

function randomAreaColor(): string {
  return AREA_PALETTE[Math.floor(Math.random() * AREA_PALETTE.length)] ?? AREA_PALETTE[0];
}

function noOpenMobileSection(): void { /* no shell above this consumer */ }

/** Opens one of the shell's mobile workspace sheets. */
export function useOpenMobileSection(): OpenMobileSection {
  return useContext(MobileSectionContext) ?? noOpenMobileSection;
}

export function AppShell({
  transport, unauthorized, onOpenSettings, onOpenPlugins, onSignOut, nowMs, userLabel, initialWorkspaceGuide = false, onOpenDemo, guideContent, guideFacts,
}: AppShellProps) {
  const guideSupported = supportsAnchoredHints();
  const workspace = useWorkspace(transport, unauthorized);
  const areaMutations = useAreaMutations(transport, unauthorized);
  const trackMutations = useTrackMutations(transport, unauthorized);
  const pinFeedback = useOperationFeedback();
  const templates = useTrackTemplates(transport, unauthorized);
  const listDirectory = createDirectoryLister(transport, unauthorized);
  /* The create form is locked while a request is held, so every press resends the held key and body (#2131). */
  const areaIntent = useKeyedIntent<null, NewAreaBody>(() => true);
  const areaCreateRequest = areaIntent.held;
  const [areaEditorTarget, setAreaEditorTarget] = useState<AreaEditorTarget | null>(null);
  const [areaEditorPending, setAreaEditorPending] = useState(false);
  const [areaEditorError, setAreaEditorError] = useState<string | null>(null);
  const areaEditorNameRef = useRef<HTMLInputElement | null>(null);
  const currentPath = useCurrentPath();
  const settingsOpen = settingsSectionForPath(currentPath) !== null;
  const routeCardId = useRouteCardId();
  const routeFilePath = useRouteFilePath();
  const mobileOverlayRoute = typeof routeCardId === 'string' || typeof routeFilePath === 'string';
  const go = useGo();
  const newTrackBack = useNewTrackBack();
  // The report's panel is a history destination, so the shell leaves it the same way the report does.
  const { closePanel } = useTrackPanelNavigation();
  const readError = workspaceReadErrorText(workspace);
  const readLoading = workspace.areasLoading
    || [...workspace.tracksLoadingByArea.values()].some(Boolean);
  const retryRead = () => {
    workspace.retryAreas(); workspace.retryOverlays();
    for (const area of workspace.areas) workspace.retryTracks(area.id);
  };

  /* The collapsed flag lives here because collapsing is a grid change on this
   * element. Tri-state: `null` follows the viewport, either boolean is an explicit
   * choice and wins at every width. */
  const preferences = useUiPreferences();
  const guide = useWorkspaceGuide(preferences, initialWorkspaceGuide && guideSupported,
    visibleAreas(workspace.areas).length === 0 && !readLoading && readError === null,
    onOpenDemo === undefined && guideSupported && !readLoading && readError === null);
  const guideEnabled = onOpenDemo === undefined && guide.enabled && guideSupported;
  const manualRailCollapsed = preferences.railCollapsed();
  const drawerWidth = useDrawerWidthHost(preferences);
  const narrowRail = useCompactViewport();
  const [mobileSection, setMobileSection] = useState<MobileSection | null>(null);
  const mobileNavOpen = mobileSection !== null;
  const [historyOpen, setHistoryOpen] = useState(false);
  const conversationScope = useMobileConversationScope();
  const scopeOwner = conversationScope?.owner ?? null;
  const [historyOwner, setHistoryOwner] = useState(() => ({ path: currentPath, scopeOwner, token: {} }));
  if (historyOwner.path !== currentPath || historyOwner.scopeOwner !== scopeOwner) {
    setHistoryOwner({ path: currentPath, scopeOwner, token: {} });
  }
  const historyTracks = conversationScope === null ? [] : [conversationScope.track];
  const receipts = useReadReceipts();
  const readUnread = receipts.isUnread;
  const isUnread = useCallback((track: Track) => readUnread('track', track.id, track.activityAt), [readUnread]);
  const trackGroups = useMemo(() => sidebarTrackGroups(workspace.tracks, workspace.areas, isUnread),
    [workspace.tracks, workspace.areas, isUnread]);
  const history = useMobileHistoryData(transport, unauthorized, historyTracks, conversationScope?.conversations ?? [], narrowRail && historyOpen);
  const visibleViewport = useVisibleViewport(narrowRail);
  const mobileSectionKind = mobileSection?.kind;
  const [mobileHeaderActionsHost, setMobileHeaderActionsHost] = useState<HTMLDivElement | null>(null);
  const [mobileHeaderTitleHost, setMobileHeaderTitleHost] = useState<HTMLDivElement | null>(null);
  const workspaceReturnRef = useRef<HTMLButtonElement | null>(null);
  const [mobileReturnIntent, setMobileReturnIntent] = useState<
    | Readonly<{ kind: 'opener'; path: string; element: HTMLElement | null }>
    | Readonly<{ kind: 'workspace'; path: string }>
    | null
  >(null);
  const routeTrackId = routeParamFromPath(currentPath, '/track/');
  const routeAreaId = routeParamFromPath(currentPath, '/area/');
  const areas = visibleAreas(workspace.areas);
  const mobileGuideRevealed = useRef(false);
  useEffect(() => {
    if (!guideEnabled) { mobileGuideRevealed.current = false; return; }
    if (narrowRail && !readLoading && readError === null && areas.length === 0 && !mobileGuideRevealed.current) {
      mobileGuideRevealed.current = true;
      setMobileSection({ kind: 'areas', areaId: undefined });
    }
  }, [guideEnabled, narrowRail, areas.length, readLoading, readError]);
  const activeAreaId = routeAreaId ?? workspace.tracks.find((track) => track.id === routeTrackId)?.areaId;
  const activeArea = areas.find((area) => area.id === activeAreaId) ?? areas[0];
  const mobileNavigationFrameRef = useRef<HTMLDivElement | null>(null);
  const mobileNavigationRef = useRef<HTMLDivElement | null>(null);
  const finishMobileClose = useCallback(() => setMobileSection(null), []);
  const { close: closeMobileSection, cancel: cancelMobileExit, interactive: workspaceInteractive } = useMobileNavigationPresence(mobileNavigationRef, mobileNavigationFrameRef, finishMobileClose, narrowRail && mobileNavOpen);
  const railCollapsed = guideEnabled && areas.length === 0 && !narrowRail ? false : manualRailCollapsed ?? narrowRail;

  const mobileFocusScope = useRef<Readonly<{ path: string; host: HTMLElement | null; intent: typeof mobileReturnIntent }> | null>(null);
  useLayoutEffect(() => {
    if (!workspaceInteractive) {
      const scope = mobileFocusScope.current;
      mobileFocusScope.current = null;
      // DOM permissions have committed before focus returns. A new route or a
      // newer focus owner wins; paint completion never restores focus.
      if (scope !== null && narrowRail && scope.path === currentPath
        && (document.activeElement === document.body || scope.host?.contains(document.activeElement))) {
        const opener = scope.intent?.kind === 'workspace' ? workspaceReturnRef.current
          : scope.intent?.element;
        if (opener?.isConnected && opener.closest('[inert], [hidden], [aria-hidden="true"], :disabled, [aria-disabled="true"]') === null
          && opener.getClientRects().length > 0) opener.focus({ preventScroll: true });
      }
      return;
    }
    const scope = mobileFocusScope.current;
    if (scope === null || scope.intent !== mobileReturnIntent) {
      mobileFocusScope.current = { path: mobileReturnIntent?.path ?? currentPath, host: mobileNavigationRef.current, intent: mobileReturnIntent };
      if (scope === null) mobileNavigationRef.current?.focus({ preventScroll: true });
    }
    // Back requests navigation and workspace together. Either may commit first;
    // bind that return intent to its first destination, never later selections.
    if (mobileReturnIntent?.kind === 'workspace' && mobileFocusScope.current?.path === mobileReturnIntent.path
      && currentPath !== mobileReturnIntent.path) {
      mobileFocusScope.current = { ...mobileFocusScope.current, path: currentPath };
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !event.defaultPrevented) {
        const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
        if (layers.item(layers.length - 1) === mobileNavigationRef.current) closeMobileSection();
        return;
      }
      if (event.key !== 'Tab') return;
      const focusable = Array.from(mobileNavigationRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ) ?? []).filter((element) => element.closest('[inert], [hidden], [aria-hidden="true"]') === null);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (document.activeElement === mobileNavigationRef.current) {
        event.preventDefault(); (event.shiftKey ? last : first).focus(); return;
      }
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault(); last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault(); first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [closeMobileSection, workspaceInteractive, narrowRail, currentPath, mobileReturnIntent]);

  useEffect(() => {
    if (!narrowRail && mobileNavOpen) setMobileSection(null);
  }, [mobileNavOpen, narrowRail]);
  useLayoutEffect(() => {
    if (workspaceInteractive && mobileSectionKind !== undefined) {
      const activeBack = mobileNavigationRef.current?.querySelector<HTMLElement>('[data-nc-workspace-page]:not([aria-hidden="true"]) header button');
      (activeBack ?? mobileNavigationRef.current)?.focus({ preventScroll: true });
    }
  }, [mobileSectionKind, workspaceInteractive]);

  // Both track mutations need the area id to invalidate the right list, and the
  // rail only knows track ids; the workspace read already has the mapping.
  const areaIdOf = (trackId: string): string | undefined =>
    workspace.tracks.find((track) => track.id === trackId)?.areaId;

  /* A navigation, and nothing else; it closes any open mobile sheet, which is an
       overlay on the surface being left. */
  const requestNewTrack = (areaId: string) => {
    closeMobileSection();
    go({ name: 'new-track', areaId });
  };

  /* Leaving the report layer drops `?panel=` through `closePanel()`, not a bare
   * `replace`: opening a panel is a `push`, and `replace` does not merge with the
   * entry before it. Only track routes have a report panel to close. */
  const clearReportPanel = () => {
    if (routeTrackId !== undefined) closePanel(routeTrackId);
  };


  const openMobileSection = (section: Parameters<OpenMobileSection>[0], returnFocus: 'opener' | 'workspace' = 'opener') => {
    cancelMobileExit();
    conversationScope?.onClose();
    setMobileReturnIntent(returnFocus === 'workspace' ? { kind: 'workspace', path: currentPath }
      : { kind: 'opener', path: currentPath, element: document.activeElement instanceof HTMLElement ? document.activeElement : null });
    // Daily/system Tracks have no visible workspace entry. Their switch starts in the active user Area.
    if (section.kind === 'tracks' && section.returnTo === 'report') {
      const areaId = areas.find((area) => area.id === section.areaId)?.id ?? activeArea?.id;
      setMobileSection(areaId === undefined ? { kind: 'areas', areaId: undefined } : { ...section, areaId });
    } else setMobileSection(section);
    clearReportPanel();
  };

  const requestCreateArea = (origin: DialogOrigin) => {
    setAreaEditorError(areaCreateRequest === null ? null : AREA_CREATE_TEXT.unknown);
    setAreaEditorTarget({ kind: 'create', origin });
  };
  const requestEditArea = (area: Area, origin: DialogOrigin) => {
    setAreaEditorError(null);
    setAreaEditorTarget({ kind: 'edit', area, origin });
  };
  const closeAreaEditor = () => {
    if (areaEditorPending) return;
    setAreaEditorTarget(null);
    setAreaEditorError(null);
  };
  const submitAreaEditor = (values: AreaEditorValues) => {
    const target = areaEditorTarget;
    if (target === null || areaEditorPending) return;
    const patch: AreaEditorPatch | null = target.kind === 'create' ? null : {
      ...(values.name === target.area.name ? {} : { name: values.name }),
      ...(values.defaultTemplateId === target.area.defaultTemplateId
        ? {} : { defaultTemplateId: values.defaultTemplateId }),
      ...(values.defaultCwd === target.area.defaultCwd ? {} : { defaultCwd: values.defaultCwd }),
    };
    if (patch !== null && Object.keys(patch).length === 0) {
      setAreaEditorTarget(null);
      return;
    }
    setAreaEditorPending(true);
    setAreaEditorError(null);
    let write: Promise<Area>;
    let creation: KeyedRequest<null, NewAreaBody> | null = null;
    if (target.kind === 'create') {
      creation = areaIntent.request(null, () => ({
        name: values.name,
        color: randomAreaColor(),
        default_template_id: values.defaultTemplateId,
        default_cwd: values.defaultCwd,
      }));
      write = areaMutations.create(creation.body, creation.key);
    } else {
      write = areaMutations.update(target.area.id, {
        ...(patch?.name === undefined ? {} : { name: patch.name }),
        ...(patch?.defaultTemplateId === undefined
          ? {} : { default_template_id: patch.defaultTemplateId }),
        ...(patch?.defaultCwd === undefined ? {} : { default_cwd: patch.defaultCwd }),
      });
    }
    void write.then(() => {
      if (creation !== null) areaIntent.release(creation);
      setAreaEditorTarget(null);
    }).catch((failure: unknown) => {
      if (target.kind === 'edit') { setAreaEditorError(writeFailureText(AREA_PATCH_FAILURES, AREA_PATCH_TEXT)(failure)); return; }
      const kind = classifyFailure(failure instanceof ApiError ? failure.failure : null, AREA_CREATE_FAILURES);
      // A refusal that never left the browser, or an answer `AREA_CREATE_FAILURES` reads as one.
      const rejected = failure instanceof AreaCreatePreflightError || failure instanceof NotSentError
        || kind === 'rejected';
      // A refused retry says nothing about an earlier unconfirmed POST; a spent key ends the request.
      const unconfirmed = target.kind === 'create' && kind !== 'key-spent'
        && (areaCreateRequest !== null || !rejected);
      if (creation !== null && !unconfirmed) areaIntent.release(creation);
      // This attempt's own words only when it was answered or stopped before sending; an unknown answer is the fixed state.
      const said = failure instanceof AreaCreatePreflightError ? failure.message
        : rejected || kind === 'key-spent' ? refusedText(writeFailureOf(failure), AREA_CREATE_TEXT.refused) : null;
      setAreaEditorError(said === null ? AREA_CREATE_TEXT.unknown : unconfirmed ? `${said} ${AREA_CREATE_TEXT.unknown}` : said);
    }).finally(() => { setAreaEditorPending(false); });
  };

  const navigateFromRail = (target: Parameters<typeof go>[0]) => {
    closeMobileSection();
    go(target);
  };

  // Opening while Areas is loading has no id yet. Follow the current Area
  // when the read recovers; an explicitly requested Area still stays exact.
  const navigationAreaId = mobileSection?.kind === 'tracks' || mobileSection?.kind === 'areas'
    ? mobileSection.areaId ?? activeArea?.id : activeArea?.id;
  const navigationArea = areas.find((area) => area.id === navigationAreaId);
  const mobileNavigationLabel = mobileSection?.kind === 'pages' ? 'Pages' : 'Tracks and settings';

  return (
    <GuideProvider facts={{ ...workspaceGuideFacts(areas.length > 0, userVisibleTracks(workspace.tracks, workspace.areas).length > 0, userVisibleTracks(workspace.tracks, workspace.areas).find(track => track.id === routeTrackId), settingsOpen), ...guideFacts }} catalog={guide.catalog} sessionKey={guide.sessionKey} enabled={guideEnabled} isSeen={guide.isSeen} onSeen={guide.onSeen} onSkip={guide.skip}>
    {guideContent}
    <div className={`${styles.shell} ${narrowRail ? mobileFontClassName : ''} ${narrowRail && (settingsOpen || mobileOverlayRoute) ? styles.settingsPageShell : ''} ${railCollapsed ? styles.shellCollapsed : styles.shellExpanded}`}>
      {narrowRail && !settingsOpen && !mobileOverlayRoute && <MobileWorkspaceHeader
        areas={areas}
        activeArea={activeArea}
        navigationOpen={workspaceInteractive || historyOpen}
        historyTriggerRef={workspaceReturnRef}
        onBack={newTrackBack === null || routeAreaId === undefined ? undefined : () => {
          newTrackBack();
          openMobileSection({ kind: 'tracks', areaId: routeAreaId }, 'workspace');
        }}
        onOpenNavigation={() => setHistoryOpen(true)}
        onSelectArea={requestNewTrack}
        onCreateArea={requestCreateArea}
        actionsHostRef={setMobileHeaderActionsHost}
        titleHostRef={setMobileHeaderTitleHost}
      />}
      {narrowRail && !settingsOpen && !mobileOverlayRoute && <GuideAnchor id="workspace-open">{(anchor, describedBy) => <button ref={anchor} aria-describedby={describedBy} type="button"
        className={`${styles.workspaceOpener} ${floatingControlClassName}`} aria-label="Open workspace"
        hidden={workspaceInteractive || historyOpen || conversationScope?.conversationOpen === true}
        style={{ bottom: `calc(${visibleViewport.bottomInset}px + var(--space-4) + env(safe-area-inset-bottom))` }}
        onClick={() => openMobileSection({ kind: 'areas', areaId: activeArea?.id })}><Icon name="folder" /></button>}</GuideAnchor>}
      <EdgeSwipe enabled={narrowRail && !settingsOpen && !mobileOverlayRoute && !mobileNavOpen && !historyOpen} onSwipe={() => setHistoryOpen(true)} />
      {narrowRail && <MobileHistory owner={historyOwner.token} open={historyOpen} onOpenChange={setHistoryOpen}
        selectedConversationId={conversationScope?.selectedConversationId ?? null}
        trackGroups={trackGroups} currentTrackId={conversationScope?.track.id}
        isUnread={isUnread} tracksLoading={readLoading || workspace.overlaysLoading} tracksError={readError ?? workspaceActivityErrorText(workspace)} onRetryTracks={retryRead}
        onOpenTrack={(trackId) => { conversationScope?.onClose(); go({ name: 'track', trackId }); }}
        onArchive={conversationScope?.onArchive}
        conversations={history.conversations} loading={history.loading} failed={history.failed} onRetry={history.retry}
        onNew={conversationScope?.onNew ?? null} now={nowMs ?? Date.now()}
        onSelect={(row) => conversationScope?.onOpen(row)}
        onOpenSettings={onOpenSettings}
        scopeLabel={conversationScope === null ? '当前 Track' : `当前 Track · ${trackDisplayTitle(conversationScope.track.title)}`}
        accountLabel={userLabel ?? '账号'} accountInitial={userLabel?.slice(0, 1) ?? '我'} onSignOut={onSignOut} />}
      <div ref={mobileNavigationFrameRef} className={styles.navigationFrame}>
      <div
        ref={mobileNavigationRef}
        id="mobile-workspace-navigation"
        className={`${styles.navigation} ${mobileNavOpen ? styles.navigationOpen : ''}`}
        role={workspaceInteractive ? 'dialog' : undefined}
        aria-modal={workspaceInteractive ? true : undefined}
        aria-label={narrowRail ? mobileNavigationLabel : undefined}
        inert={narrowRail && !workspaceInteractive}
        aria-hidden={narrowRail && !workspaceInteractive ? true : undefined}
        data-nc-escape-layer={workspaceInteractive ? '' : undefined}
        tabIndex={narrowRail ? -1 : undefined}
      >
        <div className={styles.navigationPanel}>
          <div className={styles.navigationContent}>
          {narrowRail ? (
            mobileSection?.kind === 'pages' ? (
              <MobilePages
                areaId={navigationArea?.id}
                onEditArea={requestEditArea}
                onBack={closeMobileSection}
                onNewTrack={requestNewTrack}
                onCreateArea={requestCreateArea}
                onOpenSettings={() => { closeMobileSection(); onOpenSettings(); }}
                areas={workspace.areas}
                tracks={workspace.tracks}
                readError={readError}
                readLoading={readLoading}
                onRetryRead={retryRead}
                onOpenTrack={(trackId) => {
                  closeMobileSection();
                  // The sheets are the only writers of `?from=`: the surface the reader returns to.
                  go({ name: 'track', trackId, from: 'pages' });
                }}
              />
            ) : mobileSection !== null ? (
              <MobileTracks
                onOpenDemo={onOpenDemo}
                onStartGuide={onOpenDemo ?? (guideSupported ? guide.start : undefined)}
                view={mobileSection.kind === 'tracks' ? 'tracks' : 'areas'}
                onSelectArea={(areaId) => { cancelMobileExit(); setMobileSection({ kind: 'tracks', areaId }); }}
                onBack={mobileSection.kind === 'tracks' && mobileSection.returnTo !== 'report' ? () => setMobileSection({ kind: 'areas', areaId: navigationArea?.id }) : closeMobileSection}
                tracksBackLabel={mobileSection.kind === 'tracks' && mobileSection.returnTo === 'report' ? 'Report' : 'Areas'}
                onNewTrack={requestNewTrack}
                onOpenSettings={() => { closeMobileSection(); onOpenSettings(); }}
                currentTrackId={routeTrackId}
                areas={workspace.areas}
                tracksByArea={workspace.tracksByArea}
                readError={readError}
                readLoading={readLoading}
                onRetryRead={retryRead}
                areaId={navigationArea?.id}
                onCreateArea={requestCreateArea}
                onEditArea={requestEditArea}
                /* The rail's receipt, key for key: opening a track on the phone clears the
                                   rail's dot too, and vice versa. */
                trackActions={(track) => ({
                  areaPinned: preferences.areaTrackPinned(track.areaId, track.id),
                  onSetPinned: (id, next) => { void pinFeedback.run(trackMutations.setPinned(id, track.areaId, next, nowMs ?? Date.now()), writeFailureText(TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT.pin)); },
                  onSetAreaPinned: (id, next) => preferences.setAreaTrackPinned(track.areaId, id, next),
                  onMarkUnread: (id) => { void pinFeedback.run(receipts.markUnread('track', id), writeFailureText(READ_RECEIPT_FAILURES, MARK_UNREAD_TEXT)); },
                })}
                isUnread={isUnread}
                onOpenTrack={(trackId) => {
                  closeMobileSection();
                  go({ name: 'track', trackId, from: areaIdOf(trackId) === undefined ? 'pages' : 'area' });
                }}
              />
            ) : null
          ) : <Sidebar
            trackGroups={trackGroups}
            onOpenDemo={onOpenDemo}
            onStartGuide={onOpenDemo ?? (guideSupported ? guide.start : undefined)}
            guideActive={guideEnabled && areas.length === 0}
            collapsed={railCollapsed}
            onToggleCollapsed={() => preferences.setRailCollapsed(!railCollapsed)}
            areas={workspace.areas}
            tracksByArea={workspace.tracksByArea}
            tracks={workspace.tracks}
            currentPath={currentPath}
            readError={readError}
            readLoading={readLoading || workspace.overlaysLoading}
            activityError={workspaceActivityErrorText(workspace)}
            onRetryRead={retryRead}
            onGo={navigateFromRail}
            onRequestCreateArea={requestCreateArea}
            onRequestEditArea={requestEditArea}
            onDeleteArea={(areaId, signal) => areaMutations.remove(areaId, signal)}
            onNewTrack={requestNewTrack}
            onSetPinned={async (trackId, pinned) => {
              const areaId = areaIdOf(trackId);
              if (areaId === undefined) return;
              await trackMutations.setPinned(trackId, areaId, pinned, nowMs ?? Date.now());
            }}
            onDeleteTrack={async (trackId, signal) => {
              const areaId = areaIdOf(trackId);
              if (areaId === undefined) return;
              await trackMutations.remove(trackId, areaId, signal);
            }}
            onOpenSettings={() => { closeMobileSection(); onOpenSettings(); }}
            onOpenPlugins={() => { closeMobileSection(); onOpenPlugins(); }}
            onSignOut={() => { closeMobileSection(); onSignOut(); }}
            userLabel={userLabel}
          />}
          </div>

          <OperationFeedback feedback={pinFeedback} />
        </div>
      </div>
      </div>
      <main ref={drawerWidth.mainRef} className={styles.main} style={drawerWidth.style} inert={workspaceInteractive} aria-hidden={workspaceInteractive ? true : undefined}>
        {/* One flex item. Routes compose ErrorBox + page + Drawer as siblings;
            `:first-child` on `.main` would flex the banner, not the page. */}
        <div key={currentPath} className={styles.stage} hidden={narrowRail && settingsOpen}>
          <ProviderAuthenticationNotice transport={transport} unauthorized={unauthorized}
            onOpenPlanners={() => { closeMobileSection(); go({ name: 'settings-planners' }); }} />
          <MobileSectionContext.Provider value={openMobileSection}>
            <MobileHeaderActionsContext.Provider value={narrowRail ? mobileHeaderActionsHost : null}>
              <MobileHeaderTitleContext.Provider value={narrowRail ? mobileHeaderTitleHost : null}>
                <DrawerResizeProvider value={drawerWidth.resize}><Outlet /></DrawerResizeProvider>
              </MobileHeaderTitleContext.Provider>
            </MobileHeaderActionsContext.Provider>
          </MobileSectionContext.Provider>
        </div>
        {/* Above the keyed route stage so settings panes survive tab changes and resizing. */}
        <SettingsOverlay transport={transport} unauthorized={unauthorized} />
      </main>
      <Dialog origin={areaEditorTarget?.origin ?? { kind: 'surface-center' }}
        open={areaEditorTarget !== null}
        title={areaEditorTarget?.kind === 'edit' ? `Edit ${areaEditorTarget.area.name}` : 'New area'}
        onClose={closeAreaEditor}
        hideTitleRow
        hideClose={areaEditorPending}
        initialFocusRef={areaEditorNameRef}
      >
        {areaEditorTarget !== null && (
          <AreaEditorForm
            key={areaEditorTarget.kind === 'edit' ? areaEditorTarget.area.id : 'new-area'}
            initial={areaEditorTarget.kind === 'edit'
              ? {
                name: areaEditorTarget.area.name,
                defaultTemplateId: areaEditorTarget.area.defaultTemplateId,
                defaultCwd: areaEditorTarget.area.defaultCwd,
              }
              : {
                name: areaCreateRequest?.body.name ?? '',
                defaultTemplateId: areaCreateRequest?.body.default_template_id ?? null,
                defaultCwd: areaCreateRequest?.body.default_cwd ?? null,
              }}
            submitting={areaEditorPending}
            locked={areaEditorTarget.kind === 'create' && areaCreateRequest !== null}
            error={areaEditorError}
            templates={templates.templates}
            templatesLoaded={templates.loaded}
            templatesError={templates.error}
            listDirectory={listDirectory}
            nameInputRef={areaEditorNameRef}
            submitLabel={areaEditorTarget.kind === 'edit' ? 'Save changes'
              : areaCreateRequest === null ? 'Create area' : 'Try again'}
            cancelLabel={!areaEditorPending && areaEditorTarget.kind === 'create' && areaCreateRequest !== null ? 'Discard draft' : 'Cancel'}
            onCancel={() => {
              if (areaEditorPending) return;
              if (areaEditorTarget.kind === 'create' && areaCreateRequest !== null) areaIntent.release(areaCreateRequest);
              closeAreaEditor();
            }}
            onSubmit={submitAreaEditor}
          />
        )}
      </Dialog>
    </div>
    </GuideProvider>
  );
}
