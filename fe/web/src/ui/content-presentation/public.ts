import { startTransition, useEffect } from 'react';
import { useState } from '../state/public.ts';

/**
 * Permit a new eligible selection's content after a shell rendering opportunity.
 * This is mount permission, not data readiness, a commit, paint or authorization.
 * Same-selection updates stay immediate after admission; losing eligibility,
 * switching or unmounting cancels the pending visit.
 */
export function useContentAdmission(selectedKey: string | null, eligible: boolean): boolean {
  const targetId = eligible ? selectedKey : null;
  const [visit, setVisit] = useState(() => ({ selectedKey: targetId, ready: false }));
  // A→B→A is a new visit, including when an old animation callback arrives late.
  if (visit.selectedKey !== targetId) setVisit({ selectedKey: targetId, ready: false });

  useEffect(() => {
    if (visit.selectedKey === null || visit.ready) return;
    let nextFrame: number | null = null;
    const firstFrame = requestAnimationFrame(() => {
      nextFrame = requestAnimationFrame(() => {
        startTransition(() => {
          setVisit(current => current === visit ? { ...current, ready: true } : current);
        });
      });
    });
    return () => {
      cancelAnimationFrame(firstFrame);
      if (nextFrame !== null) cancelAnimationFrame(nextFrame);
    };
  }, [visit]);

  return visit.selectedKey === targetId && visit.ready;
}
