// CalmApp — the layout shell rendered by the router's root route.
//
// What's here: TitleBar, Sidebar, theme toggle, the kernel hook (single
// mount across all routes), and the <Outlet /> where the matched route
// renders its page. The "which page renders" logic moved to
// `src/app/router.tsx`; this component no longer holds a `useState<Route>`
// of its own. URL drives selection.
//
// What we still own here:
//  - theme state (light/dark toggle on the TitleBar)
//  - useKernel + useTodayTerminal — single mount keeps the WS subscription
//    and per-browser today-terminal cache stable across navigation
//  - the adapted UI shapes (coves / waves) the page components consume
//  - the WS-driven wave_detail fetch trigger when the URL points at a
//    wave we haven't loaded yet
//
// Route components (in src/app/router.tsx) read all of the above via
// the CalmShellProvider context exposed below.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Outlet, useRouterState } from '@tanstack/react-router';
import { Sidebar, TitleBar } from './ui';
import { adaptCard, adaptCove, adaptWave } from './api/adapt';
import { useKernel } from './hooks/useKernel';
import { useTodayTerminal } from './hooks/useTodayTerminal';
import { CalmShellProvider, type CalmShellValue } from './app/shell';
import { useGo } from './app/navigation';
import type { Cove, Route as AppRoute, Wave, WaveCardData } from './types';
import type { AddPanelKind } from './ui';

export function CalmApp() {
  const [theme, setTheme] = useState<'light' | 'dark'>('light');

  const k = useKernel();
  const todayTerm = useTodayTerminal();
  const go = useGo();

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    return () => {
      delete document.documentElement.dataset.theme;
    };
  }, [theme]);

  // Derive the current AppRoute shape from the router's location so the
  // Sidebar's "highlight active" logic keeps working without props on
  // every route component. Subscribing via useRouterState ensures we
  // re-render on history changes (back / forward / programmatic nav).
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const route: AppRoute = useMemo(() => parseAppRoute(pathname), [pathname]);

  // ----- Derived UI shapes ------------------------------------------------

  const coves: Cove[] = useMemo(() => k.coves.map(adaptCove), [k.coves]);

  /**
   * UI `Wave[]` built from the flat per-cove fetches. `WavePage` only needs
   * fully-loaded cards when the user navigates into a wave; for the sidebar
   * and TodayPage we just need the kernel wave shape adapted with default
   * status/progress (overlays fold in once `wave_detail` is fetched).
   */
  const waves: Wave[] = useMemo(() => {
    const out: Wave[] = [];
    for (const list of k.wavesByCove.values()) {
      for (const w of list) {
        const detail = k.waveDetails.get(w.id);
        out.push(adaptWave(w, detail?.overlays ?? []));
      }
    }
    return out;
  }, [k.wavesByCove, k.waveDetails]);

  // For the currently-routed wave, fetch detail on demand and produce the
  // UI shape (with cards from detail).
  const currentWave = useCallback(
    (waveId: string): Wave | null => {
      const detail = k.waveDetails.get(waveId);
      if (!detail) return null;
      const uiWave = adaptWave(detail.wave, detail.overlays);
      uiWave.cards = detail.cards
        .map(adaptCard)
        .filter((c): c is WaveCardData => c !== null);
      return uiWave;
    },
    [k.waveDetails],
  );

  // Trigger wave_detail fetch when route lands on an unloaded wave.
  useEffect(() => {
    if (route.name === 'wave' && !k.waveDetails.has(route.id)) {
      void k.refetchWaveDetail(route.id);
    }
  }, [route, k]);

  // ----- Actions ----------------------------------------------------------

  const addCard = useCallback(
    async (waveId: string, type: AddPanelKind) => {
      if (type === 'terminal') {
        try {
          await k.createTerminalCard(waveId);
        } catch (err) {
          console.warn('[Calm] terminal create failed:', err);
        }
        return;
      }
      // doc / plan cards land in M3 with the plugin host. For now noop.
      console.warn(`[Calm] card kind '${type}' not yet wired to the kernel`);
    },
    [k],
  );

  const removeCard = useCallback(
    async (_waveId: string, idx: number, routedWaveId: string) => {
      const detail = k.waveDetails.get(routedWaveId);
      if (!detail) return;
      const targetCardKernel = detail.cards[idx];
      if (!targetCardKernel) return;
      try {
        await k.deleteCard(targetCardKernel.id);
      } catch (err) {
        console.warn('[Calm] card delete failed:', err);
      }
    },
    [k],
  );

  const shell: CalmShellValue = useMemo(
    () => ({ k, todayTerm, coves, waves, currentWave, addCard, removeCard }),
    [k, todayTerm, coves, waves, currentWave, addCard, removeCard],
  );

  return (
    <div className="win">
      <TitleBar
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
      />
      <div className="stage">
        <Sidebar
          coves={coves}
          waves={waves}
          route={route}
          onGo={go}
          onCreateCove={async (name, color) => {
            await k.createCove(name, color);
          }}
        />
        <main className="page">
          <div className="scroll">
            {k.error && <ErrorBanner err={k.error} />}
            {k.loading ? (
              <LoadingShell />
            ) : (
              <CalmShellProvider value={shell}>
                <Outlet />
              </CalmShellProvider>
            )}
          </div>
        </main>
      </div>
    </div>
  );
}

function parseAppRoute(pathname: string): AppRoute {
  if (pathname.startsWith('/cove/')) {
    const id = decodeURIComponent(pathname.slice('/cove/'.length).replace(/\/$/, ''));
    if (id) return { name: 'cove', coveId: id };
  }
  if (pathname.startsWith('/wave/')) {
    const id = decodeURIComponent(pathname.slice('/wave/'.length).replace(/\/$/, ''));
    if (id) return { name: 'wave', id };
  }
  return { name: 'today' };
}

function LoadingShell() {
  return (
    <div className="col">
      <p className="synth">Connecting to calm-server…</p>
    </div>
  );
}

function ErrorBanner({ err }: { err: Error }) {
  return (
    <div className="col" style={{ color: 'var(--warn, #c00)', marginBottom: 12 }}>
      <p className="synth">
        Kernel error: {err.message}. The page reflects the last successful read.
      </p>
    </div>
  );
}

export default CalmApp;
