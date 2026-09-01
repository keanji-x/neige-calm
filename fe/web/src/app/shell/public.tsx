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
// everything the dialog needs. `cove_id` is the opener's cove (hidden), and
// the POST omits `cwd` / `attach_folder`.
//
// #1209 added the template picker, so the shell also owns the
// `GET /api/wave-templates` read — for the same reason it owns the mutations,
// and because `features/**` may not import `app/**`. That read is
// non-blocking by construction: the dialog opens, and creates, without it.

import { Outlet } from '@tanstack/react-router';
import { createContext, useContext, useEffect, useRef } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { NewWaveForm, type NewWaveDraft } from '../../features/cove/new-wave/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, useCoveMutations, useWaveMutations, useWaveTemplates, useWorkspace,
} from '../providers/queries.ts';
import { useCurrentPath, useGo } from '../router/navigation.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { RAIL_COLLAPSE_QUERY } from '../../styles/breakpoints.ts';
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

/** Opens the shell's New wave dialog for `coveId` (hidden on the request). */
export function useRequestNewWave(): (coveId: string) => void {
  const request = useContext(RequestNewWaveContext);
  // Outside the shell there is no dialog to open. Routes always render inside
  // it; a no-op keeps a stray consumer (a test rendering a page bare) from
  // throwing on a control it is not exercising.
  return request ?? noRequestNewWave;
}

function noRequestNewWave(): void { /* no shell above this consumer */ }

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
  useEffect(() => {
    const media = globalThis.matchMedia?.(RAIL_COLLAPSE_QUERY);
    if (!media) return;
    const sync = () => setNarrowRail(media.matches);
    sync();
    media.addEventListener('change', sync);
    return () => media.removeEventListener('change', sync);
  }, []);
  const railCollapsed = manualRailCollapsed ?? narrowRail;

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
    }).then((wave) => {
      setNewWaveCoveId(null);
      go({ name: 'wave', waveId: wave.id });
    }).catch((error: unknown) => {
      setCreateError(error instanceof ApiError ? error.message : 'Could not create the wave.');
    }).finally(() => { setCreating(false); });
  };

  return (
    <div className={`${styles.shell} ${railCollapsed ? styles.shellCollapsed : styles.shellExpanded}`}>
      <Sidebar
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
        onGo={go}
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
        onOpenSettings={onOpenSettings}
        onSignOut={onSignOut}
        userLabel={userLabel}
      />
      {/* The provider wraps the outlet only. The rail takes the same callback as
          a plain prop, so `Sidebar` stays presentational and its tests keep
          driving it with no context above them. */}
      <main className={styles.main}>
        {/* One flex item. Routes compose ErrorBox + page + Drawer as siblings;
            `:first-child` on `.main` would flex the banner, not the page. */}
        <div className={styles.stage}>
          <RequestNewWaveContext.Provider value={requestNewWave}>
            <Outlet />
          </RequestNewWaveContext.Provider>
        </div>
      </main>
      <Dialog open={newWaveCoveId !== null} onClose={() => setNewWaveCoveId(null)} title="New wave"
        initialFocusRef={newWaveTitleRef}>
        {newWaveCoveId !== null && (
          <NewWaveForm
            titleRef={newWaveTitleRef}
            submitting={creating}
            error={createError}
            templates={waveTemplates.templates}
            templatesError={waveTemplates.error}
            onCancel={() => setNewWaveCoveId(null)}
            onSubmit={submitNewWave}
          />
        )}
      </Dialog>
    </div>
  );
}
