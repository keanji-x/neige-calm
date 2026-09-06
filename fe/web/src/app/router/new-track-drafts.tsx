import { createContext, useContext, useEffect, useSyncExternalStore, type ReactNode } from 'react';
import type { Area } from '../../../../core/domain/area.ts';
import type { NewTrackBodyWithFirstMessage, NewTrackBodyWithoutFirstMessage } from '../../../../core/domain/track.ts';
import type { NewTrackFormState } from '../../features/area/new-track/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { mintIdempotencyKey } from './idempotency-key.ts';

export type TrackCreationRequest =
  | Readonly<{ body: NewTrackBodyWithFirstMessage; key: string }>
  | Readonly<{ body: NewTrackBodyWithoutFirstMessage; key?: never }>;

export type NewTrackSession = Readonly<{
  area: Area;
  form: NewTrackFormState | null;
  key: string;
  creating: boolean;
  request: TrackCreationRequest | null;
  createdTrackId: string | null;
  error: string | null;
  canRetryAsNewTrack: boolean;
  folderConflict: Readonly<{ areaId: string; areaName: string; cwd: string }> | null;
}>;

function createDraftStore() {
  const drafts = new Map<string, NewTrackSession>();
  const listeners = new Set<() => void>();
  const notify = () => { for (const listener of listeners) listener(); };
  return {
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; },
    get: (areaId: string) => drafts.get(areaId) ?? null,
    ensure: (area: Area) => {
      if (drafts.has(area.id)) return;
      drafts.set(area.id, { area, form: null, key: mintIdempotencyKey(), creating: false,
        request: null, createdTrackId: null, error: null, canRetryAsNewTrack: false, folderConflict: null });
      notify();
    },
    update: (areaId: string, patch: Partial<NewTrackSession>) => {
      const current = drafts.get(areaId);
      if (current === undefined) return;
      drafts.set(areaId, { ...current, ...patch });
      notify();
    },
    forget: (areaId: string) => { drafts.delete(areaId); notify(); },
  };
}

const DraftContext = createContext<ReturnType<typeof createDraftStore> | null>(null);

/** One tab's unfinished intentions; route navigation never clears them. */
export function NewTrackDraftProvider({ children }: { children: ReactNode }) {
  const [store] = useState(createDraftStore);
  return <DraftContext.Provider value={store}>{children}</DraftContext.Provider>;
}

export function useNewTrackSession(areaId: string, area: Area | undefined) {
  const store = useContext(DraftContext);
  if (store === null) throw new Error('NewTrackDraftProvider is required');
  const session = useSyncExternalStore(store.subscribe, () => store.get(areaId));
  useEffect(() => { if (area !== undefined) store.ensure(area); }, [area, store]);
  return { store, session };
}
