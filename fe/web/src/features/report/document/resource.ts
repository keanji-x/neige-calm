import { useEffect } from 'react';
import { useState } from '../../../ui/state/public.ts';
import type { PreparedReport, ReportPreparationCache, ReportPreparationPort, ReportSource } from './preparation.ts';

export type VisibleReport = Readonly<{ source: ReportSource; preparation: PreparedReport }>;
type State =
  | Readonly<{ status: 'waiting' | 'preparing'; visible: VisibleReport | null; error: null }>
  | Readonly<{ status: 'ready'; visible: VisibleReport; error: null }>
  | Readonly<{ status: 'failed'; visible: VisibleReport | null; error: Error }>;
export type ReportPreparationResource = State & Readonly<{ retry: () => void }>;
type Snapshot = Readonly<{
  selectionKey: string;
  source: ReportSource;
  cache: ReportPreparationCache;
  enabled: boolean;
  state: State;
}>;

function stateFor(source: ReportSource, cache: ReportPreparationCache, enabled: boolean, previous: VisibleReport | null): State {
  const preparation = cache.peek(source);
  return preparation === undefined
    ? { status: enabled ? 'preparing' : 'waiting', visible: previous, error: null }
    : { status: 'ready', visible: { source, preparation }, error: null };
}

/** Report-owned preparation state. Mount permission and read/restore policy remain with the caller. */
export function useReportPreparation(
  selectionKey: string, source: ReportSource, cache: ReportPreparationCache,
  execute: ReportPreparationPort, enabled: boolean,
): ReportPreparationResource {
  const [snapshot, setSnapshot] = useState<Snapshot>(() => ({
    selectionKey, source, cache, enabled, state: stateFor(source, cache, enabled, null),
  }));
  let current = snapshot;
  const cached = cache.peek(source);
  const sameSelection = snapshot.selectionKey === selectionKey && snapshot.cache === cache;
  if (!sameSelection || snapshot.source !== source || snapshot.enabled !== enabled
    || (cached !== undefined && snapshot.state.visible?.preparation !== cached)) {
    current = { selectionKey, source, cache, enabled,
      state: stateFor(source, cache, enabled, sameSelection ? snapshot.state.visible : null) };
    setSnapshot(current);
  }

  useEffect(() => {
    if (!current.enabled || current.state.status !== 'preparing') return;
    const controller = new AbortController();
    const lease = current;
    void cache.prepare(current.source, execute, controller.signal).then(
      preparation => {
        if (controller.signal.aborted) return;
        setSnapshot(previous => previous !== lease ? previous : { ...previous,
          state: { status: 'ready', visible: { source: lease.source, preparation }, error: null } });
      },
      cause => {
        if (controller.signal.aborted) return;
        const error = cause instanceof Error ? cause : new Error(String(cause));
        setSnapshot(previous => previous !== lease ? previous : { ...previous,
          state: { status: 'failed', visible: previous.state.visible, error } });
      },
    );
    return () => { controller.abort(); };
  }, [cache, current, execute]);

  const retry = () => {
    setSnapshot(previous => previous !== current ? previous : { ...previous,
      state: stateFor(source, cache, enabled, previous.state.visible) });
  };
  return { ...current.state, retry };
}
