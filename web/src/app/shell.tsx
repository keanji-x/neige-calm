// Shell utilities for the router shell.
//
// Historically this file owned a `CalmShellProvider` context that
// shuttled kernel state from CalmApp's `useKernel` down to the route
// components via React context. That moved when `useKernel` was
// replaced by TanStack Query hooks: every page now reads kernel data
// directly through `useCovesQuery` / `useWavesByCoveQuery` /
// `useWaveDetailQuery`, so there's nothing left to share.
//
// What stays here: `MissingShell`, the "the cove/wave you navigated to
// no longer exists" fallback. It's rendered by the route components in
// `router.tsx` when a param resolves to nothing.

import { Icon } from '../Icon';
import type { Route as AppRoute } from '../types';

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
