// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.
//
// The shell owns the workspace read *and* the cove/wave mutations, and hands
// the rail plain callbacks: `Sidebar` stays presentational, so a test can drive
// it without a QueryClient. Sign-out is not implemented here — whoever owns the
// session passes it in.
//
// It no longer owns a New wave dialog (#1211). Starting a wave is a route now
// (`/cove/{id}/new`, owned by `app/router`), so the two `+` surfaces — every
// cove row's in the rail, and the cove page's WAVES module head — both just
// navigate. What the shell kept is the *seam*: `RequestNewWaveContext`, because
// the cove page renders inside `<Outlet />` and has no prop path to `go`.

import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef, type CSSProperties } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { useState } from '../../ui/state/public.ts';
import { useCoveMutations, useWaveMutations, useWorkspace } from '../providers/queries.ts';
import { routeParamFromPath, useCurrentPath, useGo, useWavePanelNavigation } from '../router/navigation.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { DOCK_ITEMS, dockSelection, type MobileSection } from './dock.ts';
import { MobileCoves } from './mobile-coves.tsx';
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
 * The one escape a route has into "start a wave here".
 *
 * A context and not a prop because the cove route renders inside `<Outlet />`:
 * there is no prop path from here to it. It carries a single callback, so a
 * consumer cannot come to depend on the shell's internals — and since #1211
 * that callback is a plain navigation, which is why the shell no longer holds
 * any create state of its own.
 */
const RequestNewWaveContext = createContext<((coveId: string) => void) | null>(null);

/**
 * The workspace sheet a route asks the shell to open, and — for Coves — the
 * cove it should already be drilled into.
 *
 * A context and not a prop for the reason {@link RequestNewWaveContext} gives:
 * the wave route renders inside `<Outlet />`. It replaces `MobileReportNavigationContext`
 * (#1191 §2.3), which carried a *label* and a *closure over shell state* —
 * `mobileReportSource` — so the shell and the report each held half of one
 * decision. The return surface now rides in the URL as `?from=`, the label is
 * derived from it by the route, and the only thing left to hand across the
 * boundary is this verb.
 */
type OpenMobileSection = (section: MobileSection, coveId?: string | null) => void;

const MobileSectionContext = createContext<OpenMobileSection | null>(null);

/** Goes to the new-wave page for `coveId` (#1211). */
export function useRequestNewWave(): (coveId: string) => void {
  const request = useContext(RequestNewWaveContext);
  // Outside the shell there is nowhere to go. Routes always render inside it;
  // a no-op keeps a stray consumer (a test rendering a page bare) from throwing
  // on a control it is not exercising.
  return request ?? noRequestNewWave;
}

function noRequestNewWave(): void { /* no shell above this consumer */ }

function noOpenMobileSection(): void { /* no shell above this consumer */ }

/** Opens one of the shell's mobile workspace sheets. */
export function useOpenMobileSection(): OpenMobileSection {
  return useContext(MobileSectionContext) ?? noOpenMobileSection;
}

/**
 * Which cove the Coves sheet has drilled into, **and** the motion that took it
 * there — one state, because it is one transition (#1191 §2.2).
 *
 * These were two `useState`s in `mobile-coves.tsx`, coupled at every move; the
 * id then had to be lifted here so `from=cove` could restore it, and lifting
 * only half would have handed one transition to two owners — the exact shape
 * this change exists to delete. Lifting it also loses "unmounting resets it",
 * so every exit below clears it explicitly.
 */
type CoveSelection = Readonly<{ coveId: string | null; motion: 'none' | 'forward' | 'back' }>;
const NO_COVE_SELECTED: CoveSelection = Object.freeze({ coveId: null, motion: 'none' });

export function AppShell({
  transport, unauthorized, onOpenSettings, onOpenPlugins, onSignOut, nowMs, userLabel,
}: AppShellProps) {
  const workspace = useWorkspace(transport, unauthorized);
  const coveMutations = useCoveMutations(transport, unauthorized);
  const waveMutations = useWaveMutations(transport, unauthorized);
  const currentPath = useCurrentPath();
  const go = useGo();
  // The report's panel is a history *destination* (§1.1), so the shell leaves
  // it the same way the report does — see `clearReportPanel`.
  const { closePanel } = useWavePanelNavigation();
  const readError = workspace.covesError
    ?? workspace.waveErrorsByCove.values().next().value ?? null;

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
  const [coveSelection, setCoveSelection] = useState<CoveSelection>(NO_COVE_SELECTED);
  const routeWaveId = routeParamFromPath(currentPath, '/wave/');
  /*
   * "A full-bleed secondary page is showing", derived here and nowhere else
   * (#1191 §2.1). It used to be a `window` CustomEvent that three modules
   * published and this one subscribed to, plus a second source of truth about
   * being on a wave (`currentPath.includes('/wave/')`, which also matched
   * `/cove/x/wave-notes`).
   *
   * **Two conditions OR'd, never a ternary.** #1191 §0.4: a
   * `onWaveRoute ? … : …` shape returns on the first branch while the reader is
   * on `/wave/x` with the Coves sheet drilled into a cove — the pathname is
   * still the wave's — and the dock reappears over a secondary page.
   */
  const shellSecondaryOpen = (routeWaveId !== undefined && mobileSection === null)
    || (mobileSection === 'coves' && coveSelection.coveId !== null);
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

  // Both wave mutations need the cove id to invalidate the right list, and the
  // rail only knows wave ids; the workspace read already has the mapping.
  const coveIdOf = (waveId: string): string | undefined =>
    workspace.waves.find((wave) => wave.id === waveId)?.coveId;

  /* #1211 — a navigation, and nothing else. It also closes any open mobile
     sheet, for the same reason every other rail navigation does: the sheet is
     an overlay on the surface being left. */
  const requestNewWave = (coveId: string) => {
    closeMobileSection();
    go({ name: 'new-wave', coveId });
  };

  /*
   * Leaving the report layer drops `?panel=` (#1191 §2.1), and it is
   * `closePanel()` — the same marker double-branch the report's own Back uses
   * (§1.1) — not a bare `replace`.
   *
   * An unconditional `replace` was the §0.3 defect on this second exit: opening
   * a panel is a `push`, `replace` does not merge with the entry before it, so
   * every "open a panel, then press Back to Pages" cycle left one more
   * identical `/wave/w1` on the stack and cost the reader one more hardware
   * Back to escape the report. The exit is genuinely reachable with a panel
   * open — the report's Back button lives in `<main>`, which is only `inert`
   * while a sheet is showing — and
   * `mobile-report-navigation.test.tsx` drives the three-cycle gesture.
   *
   * Still guarded on actually being on a wave, so a Today/Settings dock press
   * does not navigate.
   */
  const clearReportPanel = () => {
    if (routeWaveId !== undefined) closePanel(routeWaveId);
  };

  /*
   * Closing a sheet closes the *section*, and deliberately leaves the cove
   * drill-in alone. The selection is only ever read under
   * `mobileSection === 'coves'` — the secondary formula above conjoins it and
   * so does the render below — so a leftover cove is unobservable, and clearing
   * it at each of the six exits would be six places to forget.
   */
  const closeMobileSection = () => setMobileSection(null);

  /*
   * The one entry into a sheet, and the single site that resets the drill-in.
   * `coveId` defaults to `null`, which *is* the product rule "pressing Coves in
   * the dock always lands on the cove root list" — stated once, where it can be
   * read, instead of inferred from what every exit remembered to clear.
   */
  const openMobileSection: OpenMobileSection = (section, coveId = null) => {
    setMobileSection(section);
    setCoveSelection(coveId === null ? NO_COVE_SELECTED : { coveId, motion: 'none' });
    clearReportPanel();
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
                coves={workspace.coves}
                waves={workspace.waves}
                onOpenWave={(waveId) => {
                  closeMobileSection();
                  // The sheets are the only writers of `?from=` (#1191 §1.3):
                  // this is the surface the reader will be returned to.
                  go({ name: 'wave', waveId, from: 'pages' });
                }}
              />
            ) : mobileSection === 'coves' ? (
              <MobileCoves
                coves={workspace.coves}
                wavesByCove={workspace.wavesByCove}
                selectedCoveId={coveSelection.coveId}
                motion={coveSelection.motion}
                onSelectCove={(coveId) => setCoveSelection({ coveId, motion: 'forward' })}
                onBack={() => setCoveSelection({ coveId: null, motion: 'back' })}
                onOpenWave={(waveId) => {
                  closeMobileSection();
                  go({ name: 'wave', waveId, from: coveIdOf(waveId) === undefined ? 'pages' : 'cove' });
                }}
              />
            ) : null
          ) : <Sidebar
            /* `narrowRail === false` is the branch this element is in, so the
               two `narrowRail` tests that used to guard these were constants —
               one always false, one never reached (#1191 §2.3). */
            collapsed={railCollapsed}
            onToggleCollapsed={() => setManualRailCollapsed(!railCollapsed)}
            coves={workspace.coves}
            wavesByCove={workspace.wavesByCove}
            waves={workspace.waves}
            currentPath={currentPath}
            readError={readError?.message ?? null}
            readLoading={workspace.covesLoading || workspace.overlaysLoading
              || [...workspace.wavesLoadingByCove.values()].some(Boolean)}
            activityError={workspace.overlaysError?.message ?? null}
            onRetryRead={() => {
              workspace.retryCoves(); workspace.retryOverlays();
              for (const cove of workspace.coves) workspace.retryWaves(cove.id);
            }}
            onGo={navigateFromRail}
            onCreateCove={async (name, color) => { await coveMutations.create({ name, color }); }}
            onDeleteCove={(coveId, signal) => coveMutations.remove(coveId, signal)}
            onNewWave={requestNewWave}
            onSetPinned={async (waveId, pinned) => {
              const coveId = coveIdOf(waveId);
              if (coveId === undefined) return;
              await waveMutations.setPinned(waveId, coveId, pinned, nowMs ?? Date.now());
            }}
            onDeleteWave={async (waveId, signal) => {
              const coveId = coveIdOf(waveId);
              if (coveId === undefined) return;
              await waveMutations.remove(waveId, coveId, signal);
            }}
            onOpenSettings={() => { closeMobileSection(); onOpenSettings(); }}
            onOpenPlugins={() => { closeMobileSection(); onOpenPlugins(); }}
            onSignOut={() => { closeMobileSection(); onSignOut(); }}
            userLabel={userLabel}
          />}
        </div>
      </div>
      {/* The provider wraps the outlet only. The rail takes the same callback as
          a plain prop, so `Sidebar` stays presentational and its tests keep
          driving it with no context above them. */}
      <main className={styles.main} inert={narrowRail && mobileNavOpen} aria-hidden={narrowRail && mobileNavOpen ? true : undefined}>
        {/* One flex item. Routes compose ErrorBox + page + Drawer as siblings;
            `:first-child` on `.main` would flex the banner, not the page. */}
        <div key={currentPath} className={styles.stage}>
          <MobileSectionContext.Provider value={openMobileSection}>
            <RequestNewWaveContext.Provider value={requestNewWave}>
              <Outlet />
            </RequestNewWaveContext.Provider>
          </MobileSectionContext.Provider>
        </div>
      </main>
      {/* Pages and Coves are deliberately different indexes. Pages will group
          reports by recency/pin; this prototype keeps the current report as
          that tab's root. Coves uses list → Wave-list mobile navigation. */}
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
              // No cove argument: pressing Coves in the dock is always the root
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
    </div>
  );
}
