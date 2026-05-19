// `useKernel` — single source of truth for kernel data in the UI.
//
// Subscribes to the WS event bus on mount; every relevant event patches
// local state so reads stay fresh without polling. Returns plain kernel
// shapes (`KernelCove[]` etc) — the components do the kernel → UI shape
// adaptation via `api/adapt.ts`. This keeps the hook itself free of
// design-vocabulary concerns and easy to test.

import { useCallback, useEffect, useRef, useState } from 'react';
import * as api from '../api/calm';
import { sharedEventStream } from '../api/events';
import type {
  KernelCard,
  KernelCove,
  KernelWave,
  KernelWaveDetail,
} from '../api/wire';

export interface KernelState {
  loading: boolean;
  error: Error | null;
  coves: KernelCove[];
  /** Map<coveId, waves[]> — populated lazily as the user opens coves. */
  wavesByCove: Map<string, KernelWave[]>;
  /** Map<waveId, detail> — populated as wave pages are opened. */
  waveDetails: Map<string, KernelWaveDetail>;
}

export interface KernelActions {
  refetchCoves: () => Promise<void>;
  refetchWavesIn: (coveId: string) => Promise<void>;
  refetchWaveDetail: (waveId: string) => Promise<void>;
  createCove: (name: string, color: string) => Promise<KernelCove>;
  renameCove: (coveId: string, name: string) => Promise<KernelCove>;
  createWave: (coveId: string, title: string) => Promise<KernelWave>;
  renameWave: (waveId: string, title: string) => Promise<KernelWave>;
  /**
   * Creates a terminal card in two phases: (1) POST the Card row with
   * `kind:"terminal"`, (2) POST `/api/cards/:id/terminal` to spawn the PTY,
   * then PATCH the card's payload with `{terminal_id}` so future readers
   * can render the live PTY without an extra lookup. Returns the final
   * Card row.
   */
  createTerminalCard: (waveId: string) => Promise<KernelCard>;
  /** Generic non-terminal card (currently unused; plugin cards land in M3). */
  createCard: (waveId: string, kind: string, payload?: unknown) => Promise<KernelCard>;
  /** Cascading delete — kernel removes the cove's waves and cards too. */
  deleteCove: (coveId: string) => Promise<void>;
  /** Cascading delete — kernel removes the wave's cards too. */
  deleteWave: (waveId: string) => Promise<void>;
  deleteCard: (cardId: string) => Promise<void>;
  /** Reorder via `sort` patch; caller supplies the new sort value. */
  setCardSort: (cardId: string, sort: number) => Promise<KernelCard>;
}

export function useKernel(): KernelState & KernelActions {
  const [coves, setCoves] = useState<KernelCove[]>([]);
  const [wavesByCove, setWavesByCove] = useState<Map<string, KernelWave[]>>(() => new Map());
  const [waveDetails, setWaveDetails] = useState<Map<string, KernelWaveDetail>>(
    () => new Map(),
  );
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  // Keep the live `waveDetails` keys in a ref so the WS handler can decide
  // "do we have this wave loaded?" without re-binding on every fetch.
  const loadedWavesRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    loadedWavesRef.current = new Set(waveDetails.keys());
  }, [waveDetails]);

  const refetchCoves = useCallback(async () => {
    try {
      const cs = await api.listCoves();
      setCoves(cs);
    } catch (e) {
      setError(e as Error);
    }
  }, []);

  const refetchWavesIn = useCallback(async (coveId: string) => {
    try {
      const ws = await api.wavesInCove(coveId);
      setWavesByCove((prev) => {
        const next = new Map(prev);
        next.set(coveId, ws);
        return next;
      });
    } catch (e) {
      setError(e as Error);
    }
  }, []);

  const refetchWaveDetail = useCallback(async (waveId: string) => {
    try {
      const d = await api.getWaveDetail(waveId);
      setWaveDetails((prev) => {
        const next = new Map(prev);
        next.set(waveId, d);
        return next;
      });
    } catch (e) {
      setError(e as Error);
    }
  }, []);

  // Initial load — coves + waves for each cove. We could lazy-load waves on
  // cove click, but the sidebar wants per-cove wave counts immediately.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const cs = await api.listCoves();
        if (cancelled) return;
        setCoves(cs);
        // Prefetch waves in parallel (sidebar shows running/waiting counts).
        const results = await Promise.all(
          cs.map((c) => api.wavesInCove(c.id).then((ws) => [c.id, ws] as const)),
        );
        if (cancelled) return;
        setWavesByCove(new Map(results));
        setLoading(false);
      } catch (e) {
        if (cancelled) return;
        setError(e as Error);
        setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // WS subscription — patch state on each event.
  useEffect(() => {
    const stream = sharedEventStream();
    stream.subscribe(['*']);
    const off = stream.on((ev) => {
      switch (ev.ev) {
        case 'cove.updated': {
          const c = ev.data;
          setCoves((prev) => upsertById(prev, c));
          break;
        }
        case 'cove.deleted': {
          const id = ev.data.id;
          setCoves((prev) => prev.filter((c) => c.id !== id));
          setWavesByCove((prev) => {
            if (!prev.has(id)) return prev;
            const next = new Map(prev);
            next.delete(id);
            return next;
          });
          break;
        }
        case 'wave.updated': {
          const w = ev.data;
          setWavesByCove((prev) => {
            const next = new Map(prev);
            const cur = next.get(w.cove_id) || [];
            next.set(w.cove_id, upsertById(cur, w));
            return next;
          });
          // If we have detail loaded, refresh it (cheaper than splicing).
          if (loadedWavesRef.current.has(w.id)) {
            void refetchWaveDetail(w.id);
          }
          break;
        }
        case 'wave.deleted': {
          const { id, cove_id } = ev.data;
          setWavesByCove((prev) => {
            const cur = prev.get(cove_id);
            if (!cur) return prev;
            const next = new Map(prev);
            next.set(cove_id, cur.filter((w) => w.id !== id));
            return next;
          });
          setWaveDetails((prev) => {
            if (!prev.has(id)) return prev;
            const next = new Map(prev);
            next.delete(id);
            return next;
          });
          break;
        }
        case 'card.added':
        case 'card.updated': {
          const c = ev.data;
          if (loadedWavesRef.current.has(c.wave_id)) {
            void refetchWaveDetail(c.wave_id);
          }
          break;
        }
        case 'card.deleted': {
          if (loadedWavesRef.current.has(ev.data.wave_id)) {
            void refetchWaveDetail(ev.data.wave_id);
          }
          break;
        }
        case 'overlay.set':
        case 'overlay.deleted': {
          const ek = ev.data.entity_kind;
          const eid = ev.data.entity_id;
          if (ek === 'wave' && loadedWavesRef.current.has(eid)) {
            void refetchWaveDetail(eid);
          } else if (ek === 'card') {
            // The overlay's entity is a card; we'd need to know which wave
            // it belongs to. Cheapest path: refresh any detail that lists
            // this card.
            for (const [wid, d] of waveDetailsForCard(eid)) {
              if (loadedWavesRef.current.has(wid)) {
                void refetchWaveDetail(wid);
              }
              // We only need one.
              break;
              void d;
            }
          }
          break;
        }
        case 'plugin.state': {
          // No UI yet; plugin status surface lands in M3.
          break;
        }
      }
    });
    return () => off();
  }, [refetchWaveDetail]);

  // Actions ----------------------------------------------------------------

  const createCove = useCallback(
    async (name: string, color: string) => api.createCove({ name, color }),
    [],
  );

  const renameCove = useCallback(
    async (coveId: string, name: string) => api.updateCove(coveId, { name }),
    [],
  );

  const createWave = useCallback(
    async (coveId: string, title: string) => api.createWave({ cove_id: coveId, title }),
    [],
  );

  const renameWave = useCallback(
    async (waveId: string, title: string) => api.updateWave(waveId, { title }),
    [],
  );

  const createCard = useCallback(
    async (waveId: string, kind: string, payload?: unknown) =>
      api.createCard(waveId, { kind, payload }),
    [],
  );

  const createTerminalCard = useCallback(async (waveId: string) => {
    const card = await api.createCard(waveId, { kind: 'terminal' });
    const term = await api.createTerminal(card.id, {});
    const patched = await api.updateCard(card.id, {
      payload: { terminal_id: term.id },
    });
    return patched;
  }, []);

  const deleteCard = useCallback(async (cardId: string) => {
    await api.deleteCard(cardId);
  }, []);

  const deleteCove = useCallback(async (coveId: string) => {
    await api.deleteCove(coveId);
    // The WS `cove.deleted` event drives the local-state purge so we
    // don't double-update here.
  }, []);

  const deleteWave = useCallback(async (waveId: string) => {
    await api.deleteWave(waveId);
  }, []);

  const setCardSort = useCallback(
    async (cardId: string, sort: number) => api.updateCard(cardId, { sort }),
    [],
  );

  return {
    loading,
    error,
    coves,
    wavesByCove,
    waveDetails,
    refetchCoves,
    refetchWavesIn,
    refetchWaveDetail,
    createCove,
    renameCove,
    createWave,
    renameWave,
    createCard,
    createTerminalCard,
    deleteCove,
    deleteWave,
    deleteCard,
    setCardSort,
  };

  // Helpers ----------------------------------------------------------------

  /**
   * Returns the loaded wave-details whose card list contains `cardId`.
   * Kept inside the hook closure so it sees the latest `waveDetails`
   * without re-binding the WS handler.
   */
  function waveDetailsForCard(cardId: string): Array<[string, KernelWaveDetail]> {
    const out: Array<[string, KernelWaveDetail]> = [];
    for (const [wid, d] of waveDetails) {
      if (d.cards.some((c) => c.id === cardId)) out.push([wid, d]);
    }
    return out;
  }
}

function upsertById<T extends { id: string }>(arr: T[], item: T): T[] {
  const idx = arr.findIndex((x) => x.id === item.id);
  if (idx === -1) return [...arr, item];
  const out = arr.slice();
  out[idx] = item;
  return out;
}
