// useGo() — the shim that lets existing page bodies keep their
// `go(route)` callback shape while routing is actually driven by
// TanStack Router under the hood.
//
// Lives in its own file (not router.tsx) so CalmApp can import it
// without inducing a CalmApp ⇄ router circular: router.tsx imports
// CalmApp as its rootRoute component, so CalmApp can't import from
// router.tsx without a cycle.

import { useNavigate } from '@tanstack/react-router';
import type { Route as AppRoute } from '../types';

export function useGo() {
  const navigate = useNavigate();
  return (r: AppRoute) => {
    switch (r.name) {
      case 'today':
        void navigate({ to: '/' });
        return;
      case 'cove':
        void navigate({ to: '/cove/$coveId', params: { coveId: r.coveId } });
        return;
      case 'wave':
        void navigate({ to: '/wave/$waveId', params: { waveId: r.id } });
        return;
      case 'settings':
        void navigate({ to: '/settings' });
        return;
    }
  };
}
