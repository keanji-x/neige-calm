// useCardOverlay — read a single card overlay (status dot, progress, …).
//
// REST-seeded via the shared `['overlays', 'card']` snapshot query — the
// same `GET /api/overlays?entity_kind=card` source the Sidebar pattern
// uses for wave overlays — NOT a WS stream fold.
//
// The previous implementation folded `overlay.set` / `overlay.deleted`
// frames into local `useState` with no REST seed, so it only ever knew
// about overlays whose frames happened to arrive while it was mounted.
// That broke whenever history frames don't arrive (#854 / PR #867 review):
// an over-cap cold connect skips the replay backlog entirely, so every
// live codex/claude card's status dot fell back to "Starting" until the
// next kernel transition; a card mounted after replay finished missed the
// frames the same way. Reading through React Query fixes the class:
//
//   * mount → `useQuery` seeds from REST, independent of replay timing.
//   * `overlay.set` / `overlay.deleted` → eventBridge's
//     `invalidationPolicies` invalidate `['overlays', 'card']` → the
//     shared query refetches. One cache entry serves every card overlay
//     consumer, so a burst of overlay events costs one GET, not a
//     per-card fan-out.
//   * `_replay_complete` → eventBridge runs a defensive
//     `queryClient.invalidateQueries()` → refetch. This is what restores
//     correct card state right after an over-cap cold skip.
//   * `_snapshot_required` → eventBridge runs `queryClient.clear()` →
//     the next render refetches cold.
//
// `select` narrows the shared list to this `(cardId, overlayKind)` pair.
// The selected value is a reference into the cached array, so the hook
// only re-renders its consumer when the matching row actually changes.
//
// Matching semantics mirror the old fold: `entity_id` + `kind` only —
// deliberately plugin-agnostic (the fold applied whichever plugin's
// `overlay.set` arrived last; the REST list is unique per
// `(plugin_id, entity_kind, entity_id, kind)` and kernel-owned kinds have
// a single writer).

import { useQuery } from '@tanstack/react-query';
import { overlaysByKindQueryOptions } from '../api/queries';

export function useCardOverlay<T>(
  cardId: string | undefined,
  overlayKind: string,
): T | null {
  const query = useQuery({
    ...overlaysByKindQueryOptions('card'),
    enabled: cardId !== undefined,
    select: (overlays) =>
      overlays.find((o) => o.entity_id === cardId && o.kind === overlayKind) ??
      null,
  });
  if (cardId === undefined) return null;
  return (query.data?.payload as T | undefined) ?? null;
}
