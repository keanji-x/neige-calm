import { onlineManager, type QueryClient } from '@tanstack/react-query';
import type { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { observeOnlineStatus } from '../../systems/recovery/public.tsx';

/** Bundled assembly exclusively owns Query's network permission. Probes bypass
 * Query; mutations use networkMode=always plus immutable admission tickets.
 * Release restores the ordinary browser event source, including StrictMode. */
export function coordinateRecoveryQueries(access: RecoveryAccess, client: QueryClient): () => void {
  let physicalOnline = navigator.onLine;
  let generation = access.read().generation;
  const update = () => {
    const state = access.read();
    if (state.generation !== generation) {
      generation = state.generation;
      // Revert an in-flight read to prior data before abort rejection can
      // replace the retained page with an error. Writes never use this queue.
      void client.cancelQueries().catch(() => undefined);
    }
    onlineManager.setOnline(physicalOnline && (state.phase === 'syncing' || state.phase === 'connected'));
  };
  const unsubscribe = access.subscribe(update);
  onlineManager.setEventListener(() => observeOnlineStatus(online => { physicalOnline = online; update(); }));
  update();
  return () => {
    unsubscribe();
    onlineManager.setEventListener(setOnline => onlineManager.hasListeners() ? observeOnlineStatus(setOnline) : undefined);
    onlineManager.setOnline(physicalOnline);
  };
}
