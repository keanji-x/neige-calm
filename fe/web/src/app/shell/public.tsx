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

import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef, type CSSProperties } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { Area } from '../../../../core/domain/area.ts';
import {
  AreaEditorForm, type AreaEditorPatch, type AreaEditorValues,
} from '../../features/area/editor/public.tsx';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { createDirectoryLister } from '../providers/directory.ts';
import {
  useAreaMutations, useTrackMutations, useTrackTemplates, useWorkspace,
} from '../providers/queries.ts';
import { routeParamFromPath, useCurrentPath, useGo, useTrackPanelNavigation } from '../router/navigation.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { DOCK_ITEMS, dockSelection, type MobileSection } from './dock.ts';
import { MobileAreas } from './mobile-areas.tsx';
import { MobilePages } from './mobile-pages.tsx';
import { SettingsOverlay } from './settings-overlay.tsx';
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

/**
 * The workspace sheet a route asks the shell to open, and — for Areas — the
 * area it should already be drilled into.
 *
 * A context because the track route renders inside `<Outlet />`. It replaces `MobileReportNavigationContext`
 * (#1191 §2.3), which carried a *label* and a *closure over shell state* —
 * `mobileReportSource` — so the shell and the report each held half of one
 * decision. The return surface now rides in the URL as `?from=`, the label is
 * derived from it by the route, and the only thing left to hand across the
 * boundary is this verb.
 */
type OpenMobileSection = (section: MobileSection, areaId?: string | null) => void;

const MobileSectionContext = createContext<OpenMobileSection | null>(null);

type AreaEditorTarget = Readonly<{ kind: 'create' }> | Readonly<{ kind: 'edit'; area: Area }>;

function randomAreaColor(): string {
  return AREA_PALETTE[Math.floor(Math.random() * AREA_PALETTE.length)] ?? AREA_PALETTE[0];
}

function noOpenMobileSection(): void { /* no shell above this consumer */ }

/** Opens one of the shell's mobile workspace sheets. */
export function useOpenMobileSection(): OpenMobileSection {
  return useContext(MobileSectionContext) ?? noOpenMobileSection;
}

/**
 * Which area the Areas sheet has drilled into, **and** the motion that took it
 * there — one state, because it is one transition (#1191 §2.2).
 *
 * These were two `useState`s in `mobile-areas.tsx`, coupled at every move; the
 * id then had to be lifted here so `from=area` could restore it, and lifting
 * only half would have handed one transition to two owners — the exact shape
 * this change exists to delete. Lifting it also loses "unmounting resets it",
 * so every exit below clears it explicitly.
 */
type AreaSelection = Readonly<{ areaId: string | null; motion: 'none' | 'forward' | 'back' }>;
const NO_AREA_SELECTED: AreaSelection = Object.freeze({ areaId: null, motion: 'none' });

export function AppShell({
  transport, unauthorized, onOpenSettings, onOpenPlugins, onSignOut, nowMs, userLabel,
}: AppShellProps) {
  const workspace = useWorkspace(transport, unauthorized);
  const areaMutations = useAreaMutations(transport, unauthorized);
  const trackMutations = useTrackMutations(transport, unauthorized);
  const templates = useTrackTemplates(transport, unauthorized);
  const listDirectory = createDirectoryLister(transport, unauthorized);
  const [areaEditorTarget, setAreaEditorTarget] = useState<AreaEditorTarget | null>(null);
  const [areaEditorPending, setAreaEditorPending] = useState(false);
  const [areaEditorError, setAreaEditorError] = useState<string | null>(null);
  const areaEditorNameRef = useRef<HTMLInputElement | null>(null);
  const currentPath = useCurrentPath();
  const go = useGo();
  // The report's panel is a history *destination* (§1.1), so the shell leaves
  // it the same way the report does — see `clearReportPanel`.
  const { closePanel } = useTrackPanelNavigation();
  const readError = workspace.areasError
    ?? workspace.trackErrorsByArea.values().next().value ?? null;

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
  const [manualRailCollapsed, setManualRailCollapsed] = useState<boolean | null>(null);
  // The third copy of the compact-viewport subscription used to be inlined
  // right here, under a different name (#1191 §3.2).
  const narrowRail = useCompactViewport();
  const [mobileSection, setMobileSection] = useState<MobileSection | null>(null);
  const mobileNavOpen = mobileSection !== null;
  const [areaSelection, setAreaSelection] = useState<AreaSelection>(NO_AREA_SELECTED);
  const routeTrackId = routeParamFromPath(currentPath, '/track/');
  /*
   * "A full-bleed secondary page is showing", derived here and nowhere else
   * (#1191 §2.1). It used to be a `window` CustomEvent that three modules
   * published and this one subscribed to, plus a second source of truth about
   * being on a track (`currentPath.includes('/track/')`, which also matched
   * `/area/x/track-notes`).
   *
   * **Two conditions OR'd, never a ternary.** #1191 §0.4: a
   * `onTrackRoute ? … : …` shape returns on the first branch while the reader is
   * on `/track/x` with the Areas sheet drilled into an area — the pathname is
   * still the track's — and the dock reappears over a secondary page.
   */
  const shellSecondaryOpen = (routeTrackId !== undefined && mobileSection === null)
    || (mobileSection === 'areas' && areaSelection.areaId !== null);
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
      const focusable = mobileNavigationRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      if (focusable === undefined || focusable.length === 0) return;
      const first = focusable.item(0);
      const last = focusable.item(focusable.length - 1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault(); last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault(); first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [mobileNavOpen]);

  useEffect(() => {
    if (!narrowRail && mobileNavOpen) setMobileSection(null);
  }, [mobileNavOpen, narrowRail]);

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
   * Still guarded on actually being on a track, so a Today/Settings dock press
   * does not navigate.
   */
  const clearReportPanel = () => {
    if (routeTrackId !== undefined) closePanel(routeTrackId);
  };

  /*
   * Closing a sheet closes the *section*, and deliberately leaves the area
   * drill-in alone. The selection is only ever read under
   * `mobileSection === 'areas'` — the secondary formula above conjoins it and
   * so does the render below — so a leftover area is unobservable, and clearing
   * it at each of the six exits would be six places to forget.
   */
  const closeMobileSection = () => setMobileSection(null);

  /*
   * The one entry into a sheet, and the single site that resets the drill-in.
   * `areaId` defaults to `null`, which *is* the product rule "pressing Areas in
   * the dock always lands on the area root list" — stated once, where it can be
   * read, instead of inferred from what every exit remembered to clear.
   */
  const openMobileSection: OpenMobileSection = (section, areaId = null) => {
    setMobileSection(section);
    setAreaSelection(areaId === null ? NO_AREA_SELECTED : { areaId, motion: 'none' });
    clearReportPanel();
  };

  const requestCreateArea = () => {
    setAreaEditorError(null);
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
    const write = target.kind === 'create'
      ? areaMutations.create({
        name: values.name,
        color: randomAreaColor(),
        default_template_id: values.defaultTemplateId,
        default_cwd: values.defaultCwd,
      })
      : areaMutations.update(target.area.id, {
        ...(patch?.name === undefined ? {} : { name: patch.name }),
        ...(patch?.defaultTemplateId === undefined
          ? {} : { default_template_id: patch.defaultTemplateId }),
        ...(patch?.defaultCwd === undefined ? {} : { default_cwd: patch.defaultCwd }),
      });
    void write.then(() => {
      setAreaEditorTarget(null);
    }).catch((failure: unknown) => {
      setAreaEditorError(
        failure instanceof Error ? failure.message : `Could not ${target.kind === 'create' ? 'create' : 'update'} the area.`,
      );
    }).finally(() => { setAreaEditorPending(false); });
  };

  const navigateFromRail = (target: Parameters<typeof go>[0]) => {
    closeMobileSection();
    go(target);
  };

  const selectedDockKey = dockSelection(mobileSection, currentPath);
  // The sheet's accessible name is the dock label that opens it — stated once,
  // in `DOCK_ITEMS`, so the two can never disagree.
  const mobileNavigationLabel = DOCK_ITEMS.find((item) => item.opensSection === mobileSection)?.label ?? 'Pages';

  return (
    <div className={`${styles.shell} ${railCollapsed ? styles.shellCollapsed : styles.shellExpanded} ${shellSecondaryOpen ? styles.shellMobileSecondary : ''}`}>
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
          {narrowRail ? (
            mobileSection === 'pages' ? (
              <MobilePages
                areas={workspace.areas}
                tracks={workspace.tracks}
                onOpenTrack={(trackId) => {
                  closeMobileSection();
                  // The sheets are the only writers of `?from=` (#1191 §1.3):
                  // this is the surface the reader will be returned to.
                  go({ name: 'track', trackId, from: 'pages' });
                }}
              />
            ) : mobileSection === 'areas' ? (
              <MobileAreas
                areas={workspace.areas}
                tracksByArea={workspace.tracksByArea}
                selectedAreaId={areaSelection.areaId}
                motion={areaSelection.motion}
                onSelectArea={(areaId) => setAreaSelection({ areaId, motion: 'forward' })}
                onBack={() => setAreaSelection({ areaId: null, motion: 'back' })}
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
            onToggleCollapsed={() => setManualRailCollapsed(!railCollapsed)}
            areas={workspace.areas}
            tracksByArea={workspace.tracksByArea}
            tracks={workspace.tracks}
            currentPath={currentPath}
            readError={readError?.message ?? null}
            readLoading={workspace.areasLoading || workspace.overlaysLoading
              || [...workspace.tracksLoadingByArea.values()].some(Boolean)}
            activityError={workspace.overlaysError?.message ?? null}
            onRetryRead={() => {
              workspace.retryAreas(); workspace.retryOverlays();
              for (const area of workspace.areas) workspace.retryTracks(area.id);
            }}
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
      <main className={styles.main} inert={narrowRail && mobileNavOpen} aria-hidden={narrowRail && mobileNavOpen ? true : undefined}>
        {/* One flex item. Routes compose ErrorBox + page + Drawer as siblings;
            `:first-child` on `.main` would flex the banner, not the page. */}
        <div key={currentPath} className={styles.stage}>
          <MobileSectionContext.Provider value={openMobileSection}>
            <Outlet />
          </MobileSectionContext.Provider>
        </div>
      </main>
      {/* Pages and Areas are deliberately different indexes. Pages will group
          reports by recency/pin; this prototype keeps the current report as
          that tab's root. Areas uses list → Track-list mobile navigation. */}
      <nav
        className={`${styles.mobileDock} ${shellSecondaryOpen ? styles.mobileDockHidden : ''}`}
        aria-label="Primary"
        aria-hidden={shellSecondaryOpen ? true : undefined}
        inert={shellSecondaryOpen}
        /* The column count is `DOCK_ITEMS.length`, not a `4` written twice: the
           grid used to hard-code it, so adding a fifth destination would have
           silently overflowed the strip (#1191 §3.3). */
        style={{ '--mobile-dock-count': DOCK_ITEMS.length } as CSSProperties}
      >
        {DOCK_ITEMS.map((item) => (
          <button
            key={item.key}
            type="button"
            className={styles.mobileDockItem}
            aria-current={selectedDockKey === item.key ? 'page' : undefined}
            /* Only the two items that actually operate the sheet claim it. */
            aria-controls={item.opensSection === undefined ? undefined : 'mobile-workspace-navigation'}
            aria-expanded={item.opensSection === undefined ? undefined : mobileSection === item.opensSection}
            onClick={() => {
              // No area argument: pressing Areas in the dock is always the root
              // list, never wherever the reader was last drilled to (§2.2).
              if (item.opensSection !== undefined) { openMobileSection(item.opensSection); return; }
              closeMobileSection();
              if (item.key === 'today') go({ name: 'today' });
              else onOpenSettings();
            }}
          >
            <AstryxIcon icon={item.icon} size="md" color="inherit" />
            <span>{item.label}</span>
          </button>
        ))}
      </nav>
      {/* Owned here because it has to stay mounted while the reader navigates
          *inside* it (General → Plugins is a route change), and the shell is the
          nearest thing above `<Outlet />` that survives one. See
          `settings-overlay.tsx`. */}
      <SettingsOverlay transport={transport} unauthorized={unauthorized} />
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
              : { name: '', defaultTemplateId: null, defaultCwd: null }}
            submitting={areaEditorPending}
            error={areaEditorError}
            templates={templates.templates}
            templatesLoaded={templates.loaded}
            templatesError={templates.error}
            listDirectory={listDirectory}
            nameInputRef={areaEditorNameRef}
            submitLabel={areaEditorTarget.kind === 'edit' ? 'Save changes' : 'Create area'}
            onCancel={closeAreaEditor}
            onSubmit={submitAreaEditor}
          />
        )}
      </Dialog>
    </div>
  );
}
