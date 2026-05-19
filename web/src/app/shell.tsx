// CalmShell context — the "everything the page components need" object
// that CalmApp passes down through an <Outlet />.
//
// We use a context instead of plumbing props from rootRoute's component
// because TanStack Router's <Outlet /> renders the child route directly;
// there's no built-in prop-pass equivalent of React Router's `<Outlet
// context=... />` for code-based child routes. Context is the simplest
// idiomatic seam.
//
// Scope: layout + kernel handles + currently-mounted-route helpers.
// Data is still owned by useKernel — this is just a transport layer
// so route components can compose CovePage / WavePage props without
// re-implementing the adaptation logic.

import { createContext, useContext, type ReactNode } from 'react';
import { Icon } from '../Icon';
import type { Cove, Route as AppRoute, Wave } from '../types';
import type { AddPanelKind } from '../ui';
import type { useKernel } from '../hooks/useKernel';
import type { useTodayTerminal } from '../hooks/useTodayTerminal';

export interface CalmShellValue {
  k: ReturnType<typeof useKernel>;
  todayTerm: ReturnType<typeof useTodayTerminal>;
  coves: Cove[];
  waves: Wave[];
  /** Derive the UI Wave with cards for the given id, or null if detail
   *  hasn't loaded yet. CalmApp triggers the fetch on route change. */
  currentWave: (waveId: string) => Wave | null;
  /** Card creation routed back through the kernel. Single source of truth
   *  so WaveComponent doesn't reimplement the terminal-bootstrap dance. */
  addCard: (waveId: string, type: AddPanelKind) => Promise<void>;
  /** Card removal — index is the position within the *currently-routed*
   *  wave's detail.cards. We pass the routed waveId so the helper can
   *  look up the kernel card row. */
  removeCard: (waveId: string, idx: number, routedWaveId: string) => Promise<void>;
}

const Ctx = createContext<CalmShellValue | null>(null);

export function CalmShellProvider({
  value,
  children,
}: {
  value: CalmShellValue;
  children: ReactNode;
}) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useCalmShell(): CalmShellValue {
  const v = useContext(Ctx);
  if (!v) {
    throw new Error(
      'useCalmShell must be used inside <CalmShellProvider>. ' +
        'A route component rendered outside of <CalmApp /> as its layout?',
    );
  }
  return v;
}

export function MissingShell({
  label,
  onGo,
}: {
  label: string;
  onGo: (r: AppRoute) => void;
}) {
  return (
    <div className="col">
      <p className="synth">That {label} isn't here anymore.</p>
      <button
        className="go outline"
        onClick={() => onGo({ name: 'today' })}
        style={{ alignSelf: 'flex-start' }}
      >
        <Icon n="back" s={13} /> Back to Today
      </button>
    </div>
  );
}
