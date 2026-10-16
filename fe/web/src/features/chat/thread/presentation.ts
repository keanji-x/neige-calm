import { startTransition, useEffect } from 'react';
import { useState } from '../../../ui/state/public.ts';

/**
 * Give a newly selected conversation's shell a rendering opportunity before its
 * transcript mounts. This gates presentation only: queries, sends and run state
 * keep their original owners. Once mounted, same-conversation updates are immediate.
 */
export function useThreadPresentation(conversationId: string | null, available: boolean): boolean {
  const targetId = available ? conversationId : null;
  const [visit, setVisit] = useState(() => ({ conversationId: targetId, ready: false }));
  // A→B→A is a new visit, including when an old animation callback arrives late.
  if (visit.conversationId !== targetId) setVisit({ conversationId: targetId, ready: false });

  useEffect(() => {
    if (visit.conversationId === null || visit.ready) return;
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

  return visit.conversationId === targetId && visit.ready;
}
