import { useCallback, useMemo } from 'react';
import { useMutation, useMutationState, useQuery, useQueryClient, type QueryState } from '@tanstack/react-query';
import type { HarnessItem as Row } from '../../../../core/api/generated/wire.ts';
import { HARNESS_ITEMS_PAGE_LIMIT as PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import type { TranscriptPosition } from '../providers/queries.ts';
import { numberReads } from './read-order.ts';
import { useState } from '../../ui/state/public.ts';

/** Coverage is an observation, never an inference from adjacent row ids.
 * Only an actual short/empty response supplies the start-of-history fact. */
type LowerBound = Readonly<{ kind: 'start' } | { kind: 'from'; rowId: number }>;
export type ObservedWindow<Upper extends TranscriptPosition = TranscriptPosition> = Readonly<{
  rows: Row[];
  coverage: Readonly<{ lower: LowerBound; upper: Upper }>;
}>;

/** One raw-observation cache. Retained windows are ordered by observation precedence,
 * not a replay plan. A compacted window can contain more than one HTTP budget. */
export type History = Readonly<{ latest: ObservedWindow<{ kind: 'latest' }>; retained: readonly ObservedWindow[] }>;

/** The actual latest response is the ledger carrier, including for equal-content reads.
 * Retained reading rows may supply positive source facts, but never latest freshness. */
export function latestObservation(data: History | undefined): Row[] | undefined {
  return data?.latest.rows;
}

type ReadOptions = {
  queryKey: readonly unknown[];
  queryFn: (context: { position: TranscriptPosition }) => Promise<Row[]>;
  structuralSharing: (previous: unknown, next: unknown) => unknown;
};
export type TranscriptReads = Readonly<{
  track: <Options extends Omit<ReadOptions, 'structuralSharing'>>(options: Options) => Options & {
    structuralSharing: (previous: unknown, next: unknown) => unknown;
  };
  startOf: (result: History | undefined) => number;
  latest: () => number;
}>;

/** Each view knows only its own actual reads; injected/other-owner carriers remain unknown (0). */
export function useTranscriptReads(nextRead: () => number): TranscriptReads {
  const [reads] = useState(() => {
    const { queryFn, structuralSharing, startOf, latest } = numberReads<Row[], History>(nextRead, latestObservation);
    return { track: <Options extends Omit<ReadOptions, 'structuralSharing'>>(options: Options) =>
      ({ ...options, queryFn: queryFn(options.queryFn), structuralSharing }), startOf, latest };
  });
  return reads;
}

/** Construct coverage only at the HTTP response boundary, before any compaction. */
export function observedWindow<Upper extends TranscriptPosition>(rows: Row[], upper: Upper): ObservedWindow<Upper> {
  return { rows, coverage: { upper, lower: rows.length < PAGE_LIMIT
    ? { kind: 'start' } : { kind: 'from', rowId: rows[0].id } } };
}

type BeforePosition = Extract<TranscriptPosition, { kind: 'before' }>;

export function earlierReadingPosition(data: History | undefined): BeforePosition | undefined {
  if (data === undefined) return undefined;
  const windows = [data.latest, ...data.retained];
  for (const [index, window] of windows.entries()) {
    const { lower } = window.coverage;
    if (lower.kind === 'start') return undefined;
    const next = windows[index + 1];
    if (next === undefined || ((next.rows.at(-1)?.id ?? 0) < lower.rowId
      && !(next.coverage.upper.kind === 'before' && next.coverage.upper.rowId === lower.rowId))) return { kind: 'before', rowId: lower.rowId };
  }
  return undefined;
}

function rowsOfWindows(windows: readonly ObservedWindow[]): Row[] {
  const rows = new Map<number, Row>();
  const covered: ObservedWindow['coverage'][] = [];
  for (const window of windows) {
    for (const row of window.rows) {
      if (!rows.has(row.id) && !covered.some(({ lower, upper }) =>
        (lower.kind === 'start' || row.id >= lower.rowId) && (upper.kind === 'latest' || row.id < upper.rowId))) rows.set(row.id, row);
    }
    covered.push(window.coverage);
  }
  return [...rows.values()].sort((a, b) => a.id - b.id);
}

/** Loaded reading rows also remain explicit positive raw sources for reconciliation.
 * A reading miss is never proof of persistent deletion outside observed coverage. */
export function historyRows(data: History | undefined): Row[] {
  return data === undefined ? [] : rowsOfWindows([data.latest, ...data.retained]);
}

function compactWindows(windows: readonly ObservedWindow[]): ObservedWindow[] {
  const compacted: ObservedWindow[] = [];
  for (const window of windows) {
    const previous = compacted.at(-1);
    const lower = previous?.coverage.lower;
    if (previous !== undefined && lower?.kind === 'from' && window.coverage.lower.kind === 'from'
      && ((window.rows.at(-1)?.id ?? 0) >= lower.rowId
        || (window.coverage.upper.kind === 'before' && window.coverage.upper.rowId === lower.rowId))) {
      compacted[compacted.length - 1] = { rows: rowsOfWindows([previous, window]),
        coverage: { upper: previous.coverage.upper, lower: window.coverage.lower } };
    } else compacted.push(window);
  }
  return compacted;
}

export function latestHistory(previous: History | undefined, rows: Row[]): History {
  const latest = observedWindow(rows, { kind: 'latest' });
  const lower = latest.coverage.lower;
  const retained = previous === undefined || lower.kind === 'start' ? []
    : compactWindows([previous.latest, ...previous.retained].filter(old => old.rows[0] !== undefined && old.rows[0].id < lower.rowId));
  return { latest, retained };
}

function earlierHistory(previous: History, cursor: number, rows: Row[]): History {
  const windows = [previous.latest, ...previous.retained];
  const index = windows.findIndex(window => window.coverage.lower.kind === 'from' && window.coverage.lower.rowId === cursor);
  if (index < 0) return previous;
  const window = observedWindow(rows, { kind: 'before', rowId: cursor });
  const lower = window.coverage.lower;
  const retained = lower.kind === 'start' ? [] : windows.slice(index + 1)
    .filter(old => old.rows[0] !== undefined && old.rows[0].id < lower.rowId);
  return { latest: previous.latest, retained: [...windows.slice(1, index + 1), window, ...retained] };
}

type EarlierIntent = { position: BeforePosition; snapshot: History; read: number; reads: TranscriptReads };

/** Use the admitting owner's ledger: another observer need not have numbered this snapshot. */
function currentEarlier(intent: EarlierIntent | undefined, state: QueryState<History> | undefined): boolean {
  return intent !== undefined && state?.fetchStatus === 'idle' && state.status === 'success' && !state.isInvalidated
    && state.data === intent.snapshot && intent.reads.startOf(state.data) === intent.read
    && earlierReadingPosition(state.data)?.rowId === intent.position.rowId;
}

/** One transcript cache. Query owns automatic latest reads; explicit reader input owns older reads.
 * A newest read supersedes an in-flight older read, even when its payload is structurally equal. */
export function useTranscriptHistory(options: ReadOptions, enabled: boolean, reads: TranscriptReads) {
  const client = useQueryClient();
  const { queryKey } = options;
  const history = useQuery({ queryKey, enabled,
    // Keep row/window sharing, but give each cache publication its own snapshot identity.
    // An unmounted owner's read ledger cannot observe a new owner's equal latest read.
    // Snapshot identity only rejects obsolete responses; it never supplies a read number.
    structuralSharing: (previous, next) => ({ ...(options.structuralSharing(previous, next) as History) }),
    queryFn: async () => {
      const page = await options.queryFn({ position: { kind: 'latest' } });
      return latestHistory(client.getQueryData<History>(queryKey), page);
    },
  });
  const mutationKey = useMemo(() => [...queryKey, 'earlier'], [queryKey]);
  const earlier = useMutation({
    mutationKey,
    gcTime: 0,
    retry: false,
    mutationFn: async (intent: EarlierIntent) => {
      const page = await options.queryFn({ position: intent.position });
      const state = client.getQueryState<History>(queryKey);
      if (!currentEarlier(intent, state)) return;
      client.setQueryData<History>(queryKey, previous => previous === undefined ? previous : earlierHistory(previous, intent.position.rowId, page));
    },
  });
  // Keep the public mutation identity, not a structurally shared copy of its snapshot.
  // Status subscribes all observers to lifecycle changes; admission reads the cache synchronously.
  const earlierStates = useMutationState({ filters: { mutationKey, exact: true }, select: mutation => ({
    mutation, status: mutation.state.status,
  }) });
  const position = earlierReadingPosition(history.data);
  const { mutateAsync } = earlier;
  const loadEarlier = useCallback(() => {
    const state = client.getQueryState<History>(queryKey);
    const next = earlierReadingPosition(state?.data);
    if (client.isMutating({ mutationKey, exact: true,
      predicate: mutation => currentEarlier(mutation.state.variables as EarlierIntent | undefined, state) }) > 0
      || state?.fetchStatus !== 'idle' || state.status !== 'success' || state.isInvalidated
      || next === undefined || state.data === undefined) return false;
    void mutateAsync({ position: next, snapshot: state.data, read: reads.startOf(state.data), reads }).catch(() => undefined);
    return true;
  }, [client, queryKey, mutationKey, mutateAsync, reads]);
  // An old cursor's lifecycle cannot keep a newer window busy or export its obsolete error.
  const state = client.getQueryState<History>(queryKey);
  // The exact owner key types these public cache variables.
  const activeEarlier = earlierStates.map(({ mutation }) => mutation.state)
    .filter(mutation => currentEarlier(mutation.variables as EarlierIntent | undefined, state));
  const earlierPending = activeEarlier.some(mutation => mutation.status === 'pending');
  const earlierError = activeEarlier.findLast(mutation => mutation.status === 'error')?.error ?? null;
  const latest = history.data?.latest;
  const retained = history.data?.retained;
  const loadedRows = useMemo(() => latest === undefined || retained === undefined ? [] : rowsOfWindows([latest, ...retained]), [latest, retained]);
  return { ...history, loadedRows, isFetching: history.isFetching || earlierPending,
    earlier: { cursor: position?.rowId ?? null, pending: earlierPending, failed: !earlierPending && earlierError !== null },
    error: history.error ?? (earlierPending ? null : earlierError), loadEarlier };
}
