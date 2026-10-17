import { useMemo } from 'react';
import { usePreparedContent, type PreparationStore } from '../../../systems/content-preparation/public.ts';
import type { PreparedReport, ReportPreparationCache, ReportPreparationPort, ReportSource } from './preparation.ts';

export type PreparedReportCandidate = Readonly<{ source: ReportSource; preparation: PreparedReport }>;

/** Typed report adapter. A prepared candidate is not evidence of an actual content commit. */
export function useReportPreparation(
  selectionKey: string, source: ReportSource, cache: ReportPreparationCache,
  execute: ReportPreparationPort, enabled: boolean,
) {
  const store = useMemo<PreparationStore<ReportSource, PreparedReport>>(() => ({
    peek: cache.peek,
    prepare: (input, signal) => cache.prepare(input, execute, signal),
  }), [cache, execute]);
  const resource = usePreparedContent(selectionKey, source, store, enabled);
  const reportCandidate = ({ input, value }: NonNullable<typeof resource.candidate>): PreparedReportCandidate => ({ source: input, preparation: value });
  return resource.status === 'ready'
    ? { ...resource, candidate: reportCandidate(resource.candidate) }
    : { ...resource, candidate: resource.candidate === null ? null : reportCandidate(resource.candidate) };
}

export type ReportPreparationResource = ReturnType<typeof useReportPreparation>;
