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
// everything the dialog needs. `cove_id` is the opener's cove (hidden).
//
// The form's Folder field is optional (#1147 S3): no folder ⇒ the POST omits
// `cwd` *and* `attach_folder` and the kernel mints its own managed workspace;
// a folder ⇒ both go out, and the wave is attached to a directory the kernel
// will never move or delete. The shell owns that translation because it also
// owns the failure it can produce — the structured 409 below.
//
// #1209 added the template picker, so the shell also owns the
// `GET /api/wave-templates` read — for the same reason it owns the mutations,
// and because `features/**` may not import `app/**`. That read is
// non-blocking by construction: the dialog opens, and creates, without it.

import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef, type CSSProperties } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { folderConflictMessage } from '../../../../core/domain/cove.ts';
import { NewWaveForm, type NewWaveDraft } from '../../features/cove/new-wave/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { createDirectoryLister } from '../providers/directory.ts';
import {
  ApiError, folderConflictOf, useCoveMutations, useWaveMutations, useWaveTemplates, useWorkspace,
} from '../providers/queries.ts';
import { routeParamFromPath, useCurrentPath, useGo, useWavePanelNavigation } from '../router/navigation.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
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
 * The one escape a route has into the shell's dialog.
 *
 * A context and not a prop because the cove route renders inside `<Outlet />`:
 * there is no prop path from here to it, and the alternative — a second dialog
 * inside the route — is the thing this change removed. It carries a single
 * callback, so a consumer cannot come to depend on the shell's internals.
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

/** Opens the shell's New wave dialog for `coveId` (hidden on the request). */
export function useRequestNewWave(): (coveId: string) => void {
  const request = useContext(RequestNewWaveContext);
  // Outside the shell there is no dialog to open. Routes always render inside
  // it; a no-op keeps a stray consumer (a test rendering a page bare) from
  // throwing on a control it is not exercising.
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
  /* The picker's read, bound once here: `features/**` may not import
     `app/**`, and `ui/directory-browser` may not know a transport exists, so
     the port is created at the composition layer and passed down. */
  const listDirectory = createDirectoryLister(transport, unauthorized);
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

  /*
   * One state, and `null` is closed. The cove is the opener's cove — the user
   * does not pick one. The POST still requires `cove_id` this slice.
   */
  const [newWaveCoveId, setNewWaveCoveId] = useState<string | null>(null);
  // Named explicitly rather than left to the dialog's first-focusable default,
  // which is the Close button (#1161).
  const newWaveTitleRef = useRef<HTMLInputElement | null>(null);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  /*
   * #1209 — the dialog's template list. Read here rather than inside the form
   * because `features/**` must not import `app/**`, and read unconditionally
   * rather than on open so the picker is populated the moment the dialog
   * appears instead of shifting a row into place after it.
   *
   * Nothing downstream of it is allowed to gate creation: `data ?? []` means a
   * pending or failed read is Blank-only, which is exactly the pre-#1209
   * dialog. The failure is passed along as a notice, not as an error.
   */
  const waveTemplates = useWaveTemplates(transport, unauthorized);

  // Both wave mutations need the cove id to invalidate the right list, and the
  // rail only knows wave ids; the workspace read already has the mapping.
  const coveIdOf = (waveId: string): string | undefined =>
    workspace.waves.find((wave) => wave.id === waveId)?.coveId;

  const requestNewWave = (coveId: string) => {
    setCreateError(null);
    setNewWaveCoveId(coveId);
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

  const submitNewWave = (draft: NewWaveDraft) => {
    if (newWaveCoveId === null) return;
    setCreating(true);
    setCreateError(null);
    void waveMutations.create({
      cove_id: newWaveCoveId,
      title: draft.title,
      theme: readHostThemeRgb(),
      // Spread, not two optional fields: Blank leaves both keys absent, and
      // `workflow_id: undefined` is not the same request as no `workflow_id`
      // for anything that inspects the object before it is serialized.
      ...(draft.workflow_id === undefined ? {} : { workflow_id: draft.workflow_id }),
      ...(draft.workflow_input === undefined ? {} : { workflow_input: draft.workflow_input }),
      /*
       * Both keys or neither. `cwd` without `attach_folder` means "this path is
       * already claimed by some cove", which the kernel answers with a 409
       * whenever it is not — so the omitted-flag default is a request that
       * fails for every folder the user has not already bound. `true` is what
       * "I picked this folder for this cove" means, and it is a no-op when this
       * cove already covers the path (`waves.rs`'s same-cove arm), so a second
       * wave in the same repository does not conflict with the first.
       *
       * The pre-flight `GET /api/coves/resolve` the legacy form ran is
       * deliberately not ported: its only effect was choosing `false` when some
       * cove already covered the path, and the kernel's in-transaction scan
       * already reaches that same answer without the round trip — and reaches
       * it atomically, which a client-side pre-check cannot.
       */
      ...(draft.cwd === undefined ? {} : { cwd: draft.cwd, attach_folder: true }),
    }).then((wave) => {
      setNewWaveCoveId(null);
      go({ name: 'wave', waveId: wave.id });
    }).catch((error: unknown) => {
      const conflict = folderConflictOf(error);
      if (conflict !== null) {
        // The 409 body names a cove by id and carries no `error` key, so the
        // generic message here would be the bare word "Conflict".
        const owner = workspace.coves.find((cove) => cove.id === conflict.cove_id);
        setCreateError(folderConflictMessage(conflict, owner?.name ?? null));
        return;
      }
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
            onNewWave={(coveId) => { closeMobileSection(); requestNewWave(coveId); }}
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
      {/* Owned here for the same reason the New wave dialog is: it has to stay
          mounted while the reader navigates *inside* it (General → Plugins is a
          route change), and the shell is the nearest thing above `<Outlet />`
          that survives one. See `settings-overlay.tsx`. */}
      <SettingsOverlay transport={transport} unauthorized={unauthorized} />
      <Dialog open={newWaveCoveId !== null} onClose={() => setNewWaveCoveId(null)} title="New wave"
        initialFocusRef={newWaveTitleRef}>
        {newWaveCoveId !== null && (
          <NewWaveForm
            titleRef={newWaveTitleRef}
            submitting={creating}
            error={createError}
            templates={waveTemplates.templates}
            templatesError={waveTemplates.error}
            listDirectory={listDirectory}
            onCancel={() => setNewWaveCoveId(null)}
            onSubmit={submitNewWave}
          />
        )}
      </Dialog>
    </div>
  );
}
