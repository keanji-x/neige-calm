// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.
//
// The shell owns the workspace read *and* the area/track mutations, and hands
// the rail plain callbacks: `Sidebar` stays presentational, so a test can drive
// it without a QueryClient. Sign-out is not implemented here — whoever owns the
// session passes it in.
//
// It no longer owns a New track dialog (#1211). Starting a track is a route now
// (`/area/{id}/new`, owned by `app/router`), and each Area group exposes the
// route through its own `+`.

import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef } from 'react';

import { useUiPreferences } from '../providers/ui-preferences.tsx';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { visibleTracks, type Track } from '../../../../core/domain/track.ts';
import { visibleAreas } from '../../../../core/domain/area.ts';
import type { Area, NewAreaBody } from '../../../../core/domain/area.ts';
import {
  AreaEditorForm, type AreaEditorPatch, type AreaEditorValues,
} from '../../features/area/editor/public.tsx';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { createDirectoryLister } from '../providers/directory.ts';
import {
  ApiError, AreaCreatePreflightError, OfflineSubmissionError, useAreaMutations, useTrackMutations, useTrackTemplates, useWorkspace,
} from '../providers/queries.ts';
import { mintIdempotencyKey } from '../router/idempotency-key.ts';
import { routeParamFromPath, useCurrentPath, useGo, useRouteCardId, useRouteFilePath, useTrackPanelNavigation } from '../router/navigation.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { MobileWorkspaceHeader } from './mobile-header.tsx';
import { MobileTracks } from './mobile-tracks.tsx';
import { MobilePages } from './mobile-pages.tsx';
import { SettingsOverlay, settingsSectionForPath } from './settings-overlay.tsx';
import { Sidebar } from './sidebar.tsx';
import styles from './shell.module.css';

export type AppShellProps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  onOpenSettings: () => void;
  /** Settings › Plugins, from the rail's account menu. */
  onOpenPlugins: () => void;
  onSignOut: () => void;
  /** Pinned by tests so `pinned_at` assertions are stable. */
  nowMs?: number;
  userLabel?: string;
}>;

/** Routes can reopen an Area's Tracks or recent Pages; the primary entry starts at Areas. */
type MobileSection = Readonly<{ kind: 'areas'; areaId: string | undefined }> | Readonly<{ kind: 'pages' }> | Readonly<{ kind: 'tracks'; areaId: string | undefined }>;
type OpenMobileSection = (section: MobileSection) => void;

const MobileSectionContext = createContext<OpenMobileSection | null>(null);

/** App owns header geometry; the feature owns the existing action menu and focus. */
const MobileHeaderActionsContext = createContext<HTMLElement | null>(null);
export function useMobileHeaderActionsHost(): HTMLElement | null {
  return useContext(MobileHeaderActionsContext);
}

type MobileTrackChoices = Readonly<{ tracks: readonly Track[]; loading: boolean; error: string | null; onRetry: () => void }>;
type ReadMobileTrackChoices = (areaId: string) => MobileTrackChoices;
const MobileTrackChoicesContext = createContext<ReadMobileTrackChoices | null>(null);
export function useMobileTrackChoices(): ReadMobileTrackChoices | null { return useContext(MobileTrackChoicesContext); }

const MobileHeaderTitleContext = createContext<HTMLElement | null>(null);
export function useMobileHeaderTitleHost(): HTMLElement | null {
  return useContext(MobileHeaderTitleContext);
}


type AreaCreateRequest = Readonly<{ body: NewAreaBody; key: string }>;

const UNCONFIRMED_AREA = 'Creation could not be confirmed. Try again to safely check the same area.';

type AreaEditorTarget = Readonly<{ kind: 'create' }> | Readonly<{ kind: 'edit'; area: Area }>;

function randomAreaColor(): string {
  return AREA_PALETTE[Math.floor(Math.random() * AREA_PALETTE.length)] ?? AREA_PALETTE[0];
}

function noOpenMobileSection(): void { /* no shell above this consumer */ }

/** Opens one of the shell's mobile workspace sheets. */
export function useOpenMobileSection(): OpenMobileSection {
  return useContext(MobileSectionContext) ?? noOpenMobileSection;
}

export function AppShell({
  transport, unauthorized, onOpenSettings, onOpenPlugins, onSignOut, nowMs, userLabel,
}: AppShellProps) {
  const workspace = useWorkspace(transport, unauthorized);
  const areaMutations = useAreaMutations(transport, unauthorized);
  const trackMutations = useTrackMutations(transport, unauthorized);
  const templates = useTrackTemplates(transport, unauthorized);
  const listDirectory = createDirectoryLister(transport, unauthorized);
  const [areaCreateRequest, setAreaCreateRequest] = useState<AreaCreateRequest | null>(null);
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
  // The report's panel is a history *destination* (§1.1), so the shell leaves
  // it the same way the report does — see `clearReportPanel`.
  const { closePanel } = useTrackPanelNavigation();
  const readError = workspace.areasError !== null
    ? `Areas ${workspace.areas.length > 0 ? 'could not be refreshed' : 'are unavailable'}: ${workspace.areasError.message}`
    : workspace.trackErrorsByArea.values().next().value?.message ?? null;
  const readLoading = workspace.areasLoading
    || [...workspace.tracksLoadingByArea.values()].some(Boolean);
  const retryRead = () => {
    workspace.retryAreas(); workspace.retryOverlays();
    for (const area of workspace.areas) workspace.retryTracks(area.id);
  };

  /*
   * The collapsed flag lives here, not inside `Sidebar`, because collapsing is
   * a *grid* change: the rail may swap its contents for an icon strip, but
   * unless this element's `grid-template-columns` also changes, the column
   * stays 200px wide and the button appears to do nothing. That was the bug.
   *
   * The choice is tri-state: `null` follows the viewport, while either boolean
   * is an explicit user choice and wins at every width. Thus the narrow-screen
   * Expand control changes the UI immediately and widening never inherits a
   * click that appeared to do nothing.
   */
  const preferences = useUiPreferences();
  const manualRailCollapsed = preferences.railCollapsed();
  // The third copy of the compact-viewport subscription used to be inlined
  // right here, under a different name (#1191 §3.2).
  const narrowRail = useCompactViewport();
  const [mobileSection, setMobileSection] = useState<MobileSection | null>(null);
  const mobileNavOpen = mobileSection !== null;
  const mobileSectionKind = mobileSection?.kind;
  const [mobileHeaderActionsHost, setMobileHeaderActionsHost] = useState<HTMLDivElement | null>(null);
  const [mobileHeaderTitleHost, setMobileHeaderTitleHost] = useState<HTMLDivElement | null>(null);
  const mobileOpenerRef = useRef<HTMLElement | null>(null);
  const routeTrackId = routeParamFromPath(currentPath, '/track/');
  const routeAreaId = routeParamFromPath(currentPath, '/area/');
  const areas = visibleAreas(workspace.areas);
  const activeAreaId = routeAreaId ?? workspace.tracks.find((track) => track.id === routeTrackId)?.areaId;
  const activeArea = areas.find((area) => area.id === activeAreaId) ?? areas[0];
  const homeAreaId = areas[0]?.id;
  useEffect(() => {
    if (narrowRail && currentPath === '/' && homeAreaId !== undefined) {
      go({ name: 'new-track', areaId: homeAreaId }, { replace: true });
    }
  }, [narrowRail, currentPath, homeAreaId, go]);
  const mobileNavigationRef = useRef<HTMLDivElement | null>(null);
  const railCollapsed = manualRailCollapsed ?? narrowRail;

  useEffect(() => {
    if (!mobileNavOpen) return;
    mobileNavigationRef.current?.focus({ preventScroll: true });
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !event.defaultPrevented) {
        const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
        if (layers.item(layers.length - 1) === mobileNavigationRef.current) setMobileSection(null);
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
      mobileOpenerRef.current?.focus({ preventScroll: true });
    };
  }, [mobileNavOpen]);

  useEffect(() => {
    if (!narrowRail && mobileNavOpen) setMobileSection(null);
  }, [mobileNavOpen, narrowRail]);
  useEffect(() => {
    if (mobileSectionKind !== undefined) {
      const activeBack = mobileNavigationRef.current?.querySelector<HTMLElement>('[data-nc-workspace-page]:not([aria-hidden="true"]) header button');
      (activeBack ?? mobileNavigationRef.current)?.focus({ preventScroll: true });
    }
  }, [mobileSectionKind]);

  // Both track mutations need the area id to invalidate the right list, and the
  // rail only knows track ids; the workspace read already has the mapping.
  const areaIdOf = (trackId: string): string | undefined =>
    workspace.tracks.find((track) => track.id === trackId)?.areaId;

  /* #1211 — a navigation, and nothing else. It also closes any open mobile
     sheet, for the same reason every other rail navigation does: the sheet is
     an overlay on the surface being left. */
  const requestNewTrack = (areaId: string) => {
    closeMobileSection();
    go({ name: 'new-track', areaId });
  };

  /*
   * Leaving the report layer drops `?panel=` (#1191 §2.1), and it is
   * `closePanel()` — the same marker double-branch the report's own Back uses
   * (§1.1) — not a bare `replace`.
   *
   * An unconditional `replace` was the §0.3 defect on this second exit: opening
   * a panel is a `push`, `replace` does not merge with the entry before it, so
   * every "open a panel, then press Back to Pages" cycle left one more
   * identical `/track/w1` on the stack and cost the reader one more hardware
   * Back to escape the report. The exit is genuinely reachable with a panel
   * open — the report's Back button lives in `<main>`, which is only `inert`
   * while a sheet is showing — and
   * `mobile-report-navigation.test.tsx` drives the three-cycle gesture.
   *
   * Only track routes have a report panel to close.
   */
  const clearReportPanel = () => {
    if (routeTrackId !== undefined) closePanel(routeTrackId);
  };

  const closeMobileSection = () => setMobileSection(null);

  const openMobileSection: OpenMobileSection = (section) => {
    mobileOpenerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setMobileSection(section);
    clearReportPanel();
  };

  const requestCreateArea = () => {
    setAreaEditorError(areaCreateRequest === null ? null : UNCONFIRMED_AREA);
    setAreaEditorTarget({ kind: 'create' });
  };
  const requestEditArea = (area: Area) => {
    setAreaEditorError(null);
    setAreaEditorTarget({ kind: 'edit', area });
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
    if (target.kind === 'create') {
      const creation = areaCreateRequest ?? {
        key: mintIdempotencyKey(),
        body: {
          name: values.name,
          color: randomAreaColor(),
          default_template_id: values.defaultTemplateId,
          default_cwd: values.defaultCwd,
        },
      };
      setAreaCreateRequest(creation);
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
      if (target.kind === 'create') setAreaCreateRequest(null);
      setAreaEditorTarget(null);
    }).catch((failure: unknown) => {
      const rejected = failure instanceof AreaCreatePreflightError || failure instanceof OfflineSubmissionError
        || (failure instanceof ApiError && (failure.failure.kind === 'unauthorized'
          || (failure.failure.kind === 'http' && [400, 403, 404, 422, 429].includes(failure.failure.status))));
      // A refused retry says nothing about an earlier unconfirmed POST.
      const unconfirmed = target.kind === 'create' && (areaCreateRequest !== null || !rejected);
      if (target.kind === 'create' && !unconfirmed) setAreaCreateRequest(null);
      const reason = failure instanceof Error ? failure.message
        : `Could not ${target.kind === 'create' ? 'create' : 'update'} the area.`;
      setAreaEditorError(unconfirmed ? `${reason} ${UNCONFIRMED_AREA}` : reason);
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
    <div className={`${styles.shell} ${narrowRail && (settingsOpen || mobileOverlayRoute) ? styles.settingsPageShell : ''} ${railCollapsed ? styles.shellCollapsed : styles.shellExpanded}`}>
      {narrowRail && !settingsOpen && !mobileOverlayRoute && <MobileWorkspaceHeader
        areas={areas}
        activeArea={activeArea}
        navigationOpen={mobileNavOpen}
        onOpenNavigation={() => openMobileSection({ kind: 'areas', areaId: activeArea?.id })}
        onSelectArea={requestNewTrack}
        onCreateArea={requestCreateArea}
        actionsHostRef={setMobileHeaderActionsHost}
        titleHostRef={setMobileHeaderTitleHost}
        hasTrack={routeTrackId !== undefined}
      />}
      <div
        ref={mobileNavigationRef}
        id="mobile-workspace-navigation"
        className={`${styles.navigation} ${mobileNavOpen ? styles.navigationOpen : ''}`}
        role={narrowRail ? 'dialog' : undefined}
        aria-modal={narrowRail ? true : undefined}
        aria-label={narrowRail ? mobileNavigationLabel : undefined}
        aria-hidden={narrowRail && !mobileNavOpen ? true : undefined}
        data-nc-escape-layer={narrowRail && mobileNavOpen ? '' : undefined}
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
                  // The sheets are the only writers of `?from=` (#1191 §1.3):
                  // this is the surface the reader will be returned to.
                  go({ name: 'track', trackId, from: 'pages' });
                }}
              />
            ) : mobileSection !== null ? (
              <MobileTracks
                view={mobileSection.kind === 'tracks' ? 'tracks' : 'areas'}
                onSelectArea={(areaId) => setMobileSection({ kind: 'tracks', areaId })}
                onBack={mobileSection.kind === 'tracks' ? () => setMobileSection({ kind: 'areas', areaId: navigationArea?.id }) : closeMobileSection}
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
                onOpenTrack={(trackId) => {
                  closeMobileSection();
                  go({ name: 'track', trackId, from: areaIdOf(trackId) === undefined ? 'pages' : 'area' });
                }}
              />
            ) : null
          ) : <Sidebar
            /* `narrowRail === false` is the branch this element is in, so the
               two `narrowRail` tests that used to guard these were constants —
               one always false, one never reached (#1191 §2.3). */
            collapsed={railCollapsed}
            onToggleCollapsed={() => preferences.setRailCollapsed(!railCollapsed)}
            areas={workspace.areas}
            tracksByArea={workspace.tracksByArea}
            tracks={workspace.tracks}
            currentPath={currentPath}
            readError={readError}
            readLoading={readLoading || workspace.overlaysLoading}
            activityError={workspace.overlaysError?.message ?? null}
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

        </div>
      </div>
      <main className={styles.main} inert={narrowRail && mobileNavOpen} aria-hidden={narrowRail && mobileNavOpen ? true : undefined}>
        {/* One flex item. Routes compose ErrorBox + page + Drawer as siblings;
            `:first-child` on `.main` would flex the banner, not the page. */}
        <div key={currentPath} className={styles.stage} hidden={narrowRail && settingsOpen}>
          <MobileSectionContext.Provider value={openMobileSection}>
            <MobileHeaderActionsContext.Provider value={narrowRail ? mobileHeaderActionsHost : null}>
              <MobileHeaderTitleContext.Provider value={narrowRail ? mobileHeaderTitleHost : null}>
                <MobileTrackChoicesContext.Provider value={(areaId) => ({
                  tracks: areas.some((area) => area.id === areaId) ? visibleTracks(workspace.tracksByArea.get(areaId) ?? []) : [],
                  loading: workspace.areasLoading || workspace.tracksLoadingByArea.get(areaId) === true,
                  error: workspace.areasError?.message ?? workspace.trackErrorsByArea.get(areaId)?.message ?? null,
                  onRetry: retryRead,
                })}><Outlet /></MobileTrackChoicesContext.Provider>
              </MobileHeaderTitleContext.Provider>
            </MobileHeaderActionsContext.Provider>
          </MobileSectionContext.Provider>
        </div>
        {/* Above the keyed route stage so settings panes survive tab changes and resizing. */}
        <SettingsOverlay transport={transport} unauthorized={unauthorized} />
      </main>
      <Dialog
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
              if (areaEditorTarget.kind === 'create') setAreaCreateRequest(null);
              closeAreaEditor();
            }}
            onSubmit={submitAreaEditor}
          />
        )}
      </Dialog>
    </div>
  );
}
