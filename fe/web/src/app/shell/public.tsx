// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.
//
// The shell owns the workspace read *and* the cove/wave mutations, and hands
// the rail plain callbacks: `Sidebar` stays presentational, so a test can drive
// it without a QueryClient. Sign-out is not implemented here — whoever owns the
// session passes it in.
//
// It also owns the **New wave dialog**, for the same reason it owns the
// mutations: two surfaces open it — every cove row's `+` in the rail, and the
// cove page's WAVES module head — and they must open one dialog with one set of
// strings. The rail is a sibling of the outlet, so a dialog living inside the
// cove route was unreachable from it; the shell is the nearest place that sees
// both, and it already holds `useWorkspace` + `useWaveMutations`, which is
// everything the dialog needs. The form is title-only; `cove_id` is the
// opener's cove (hidden), and the POST omits `cwd` / `attach_folder`.

import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { NewWaveForm, type NewWaveDraft } from '../../features/cove/new-wave/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { requestMobilePageRoot, subscribeMobileSecondary } from '../../ui/mobile-page/public.ts';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, useCoveMutations, useWaveMutations, useWorkspace,
} from '../providers/queries.ts';
import { useCurrentPath, useGo } from '../router/navigation.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { RAIL_COLLAPSE_QUERY } from '../../styles/breakpoints.ts';
import { MobileCoves } from './mobile-coves.tsx';
import { MobilePages } from './mobile-pages.tsx';
import { Sidebar } from './sidebar.tsx';
import styles from './shell.module.css';

export type AppShellProps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  onOpenSettings: () => void;
  onSignOut: () => void;
  /** Pinned by tests so `pinned_at` assertions are stable. */
  nowMs?: number;
  userLabel?: string;
}>;

/**
 * The one escape a route has into the shell's dialog.
 *
 * A context and not a prop because the cove route renders inside `<Outlet />`:
 * there is no prop path from here to it, and the alternative — a second dialog
 * inside the route — is the thing this change removed. It carries a single
 * callback, so a consumer cannot come to depend on the shell's internals.
 */
const RequestNewWaveContext = createContext<((coveId: string) => void) | null>(null);

type MobileReportNavigation = Readonly<{
  backLabel: string;
  backFromReport: () => void;
}>;

const MobileReportNavigationContext = createContext<MobileReportNavigation | null>(null);

/** Opens the shell's New wave dialog for `coveId` (hidden on the request). */
export function useRequestNewWave(): (coveId: string) => void {
  const request = useContext(RequestNewWaveContext);
  // Outside the shell there is no dialog to open. Routes always render inside
  // it; a no-op keeps a stray consumer (a test rendering a page bare) from
  // throwing on a control it is not exercising.
  return request ?? noRequestNewWave;
}

function noRequestNewWave(): void { /* no shell above this consumer */ }

function noMobileReportBack(): void { /* no shell above this consumer */ }

export function useMobileReportNavigation(): MobileReportNavigation {
  return useContext(MobileReportNavigationContext)
    ?? { backLabel: 'Pages', backFromReport: noMobileReportBack };
}

type MobileSection = 'pages' | 'coves';
type MobileReportSource = Readonly<{ kind: 'pages' }> | Readonly<{ kind: 'cove'; coveId: string }>;

export function AppShell({ transport, unauthorized, onOpenSettings, onSignOut, nowMs, userLabel }: AppShellProps) {
  const workspace = useWorkspace(transport, unauthorized);
  const coveMutations = useCoveMutations(transport, unauthorized);
  const waveMutations = useWaveMutations(transport, unauthorized);
  const currentPath = useCurrentPath();
  const go = useGo();
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
  const [narrowRail, setNarrowRail] = useState(() => globalThis.matchMedia?.(RAIL_COLLAPSE_QUERY).matches ?? false);
  const [mobileSection, setMobileSection] = useState<MobileSection | null>(null);
  const mobileNavOpen = mobileSection !== null;
  const [mobileSecondaryOpen, setMobileSecondaryOpen] = useState(false);
  const [mobileReportSource, setMobileReportSource] = useState<MobileReportSource>({ kind: 'pages' });
  const [mobileCoveRestoreId, setMobileCoveRestoreId] = useState<string | null>(null);
  const shellSecondaryOpen = mobileSecondaryOpen
    || (currentPath.includes('/wave/') && mobileSection === null);
  const mobileNavigationRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const media = globalThis.matchMedia?.(RAIL_COLLAPSE_QUERY);
    if (!media) return;
    const sync = () => setNarrowRail(media.matches);
    sync();
    media.addEventListener('change', sync);
    return () => media.removeEventListener('change', sync);
  }, []);
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

  useEffect(() => subscribeMobileSecondary(setMobileSecondaryOpen), []);

  useEffect(() => {
    if (!narrowRail && mobileNavOpen) setMobileSection(null);
  }, [mobileNavOpen, narrowRail]);

  /*
   * One state, and `null` is closed. The cove is the opener's cove — the user
   * does not pick one. The POST still requires `cove_id` this slice.
   */
  const [newWaveCoveId, setNewWaveCoveId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  // Both wave mutations need the cove id to invalidate the right list, and the
  // rail only knows wave ids; the workspace read already has the mapping.
  const coveIdOf = (waveId: string): string | undefined =>
    workspace.waves.find((wave) => wave.id === waveId)?.coveId;

  const requestNewWave = (coveId: string) => {
    setCreateError(null);
    setNewWaveCoveId(coveId);
  };

  const navigateFromRail = (target: Parameters<typeof go>[0]) => {
    setMobileSection(null);
    go(target);
  };

  const todayDockSelected = mobileSection === null && currentPath === '/';
  const meDockSelected = mobileSection === null && currentPath.startsWith('/settings');
  const pagesDockSelected = mobileSection === 'pages'
    || (mobileSection === null && !todayDockSelected && !meDockSelected);
  const mobileNavigationLabel = mobileSection === 'pages' ? 'Pages' : 'Coves';
  const backFromReport = () => {
    setMobileSecondaryOpen(false);
    if (mobileReportSource.kind === 'cove') {
      setMobileCoveRestoreId(mobileReportSource.coveId);
      setMobileSection('coves');
      return;
    }
    setMobileSection('pages');
  };
  const mobileReportNavigation: MobileReportNavigation = {
    backLabel: mobileReportSource.kind === 'cove' ? 'Waves' : 'Pages',
    backFromReport,
  };

  const submitNewWave = (draft: NewWaveDraft) => {
    if (newWaveCoveId === null) return;
    setCreating(true);
    setCreateError(null);
    void waveMutations.create({
      cove_id: newWaveCoveId,
      title: draft.title,
      theme: readHostThemeRgb(),
    }).then((wave) => {
      setNewWaveCoveId(null);
      go({ name: 'wave', waveId: wave.id });
    }).catch((error: unknown) => {
      setCreateError(error instanceof ApiError ? error.message : 'Could not create the wave.');
    }).finally(() => { setCreating(false); });
  };

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
                  setMobileReportSource({ kind: 'pages' });
                  setMobileSection(null);
                  go({ name: 'wave', waveId });
                }}
              />
            ) : mobileSection === 'coves' ? (
              <MobileCoves
                coves={workspace.coves}
                wavesByCove={workspace.wavesByCove}
                initialCoveId={mobileCoveRestoreId}
                onOpenWave={(waveId) => {
                  const coveId = coveIdOf(waveId);
                  setMobileReportSource(coveId === undefined ? { kind: 'pages' } : { kind: 'cove', coveId });
                  setMobileSection(null);
                  go({ name: 'wave', waveId });
                }}
              />
            ) : null
          ) : <Sidebar
            collapsed={narrowRail ? false : railCollapsed}
            onToggleCollapsed={() => {
              if (narrowRail) setMobileSection(null);
              else setManualRailCollapsed(!railCollapsed);
            }}
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
            onNewWave={(coveId) => { setMobileSection(null); requestNewWave(coveId); }}
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
            onOpenSettings={() => { setMobileSection(null); onOpenSettings(); }}
            onSignOut={() => { setMobileSection(null); onSignOut(); }}
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
          <MobileReportNavigationContext.Provider value={mobileReportNavigation}>
            <RequestNewWaveContext.Provider value={requestNewWave}>
              <Outlet />
            </RequestNewWaveContext.Provider>
          </MobileReportNavigationContext.Provider>
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
      >
        <button
          type="button"
          className={styles.mobileDockItem}
          aria-current={pagesDockSelected ? 'page' : undefined}
          aria-controls="mobile-workspace-navigation"
          aria-expanded={mobileSection === 'pages'}
          onClick={() => {
            requestMobilePageRoot();
            setMobileSection(mobileSection === 'pages' ? null : 'pages');
          }}
        >
          <span className={styles.mobileDockIcon} data-nc-dock-icon="pages" aria-hidden="true" />
          <span>Pages</span>
        </button>
        <button
          type="button"
          className={styles.mobileDockItem}
          aria-current={todayDockSelected ? 'page' : undefined}
          onClick={() => { setMobileSection(null); go({ name: 'today' }); }}
        >
          <span className={styles.mobileDockIcon} data-nc-dock-icon="today" aria-hidden="true" />
          <span>Today</span>
        </button>
        <button
          type="button"
          className={styles.mobileDockItem}
          aria-current={mobileSection === 'coves' ? 'page' : undefined}
          aria-controls="mobile-workspace-navigation"
          aria-expanded={mobileSection === 'coves'}
          onClick={() => {
            requestMobilePageRoot();
            if (mobileSection !== 'coves') setMobileCoveRestoreId(null);
            setMobileSection(mobileSection === 'coves' ? null : 'coves');
          }}
        >
          <span className={styles.mobileDockIcon} data-nc-dock-icon="coves" aria-hidden="true" />
          <span>Coves</span>
        </button>
        <button
          type="button"
          className={styles.mobileDockItem}
          aria-current={meDockSelected ? 'page' : undefined}
          onClick={() => { setMobileSection(null); onOpenSettings(); }}
        >
          <span className={styles.mobileDockIcon} data-nc-dock-icon="me" aria-hidden="true" />
          <span>Me</span>
        </button>
      </nav>
      <Dialog open={newWaveCoveId !== null} onClose={() => setNewWaveCoveId(null)} title="New wave">
        {newWaveCoveId !== null && (
          <NewWaveForm
            submitting={creating}
            error={createError}
            onCancel={() => setNewWaveCoveId(null)}
            onSubmit={submitNewWave}
          />
        )}
      </Dialog>
    </div>
  );
}
