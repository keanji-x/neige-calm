// The track page's series resolver (#1628 D5): one query per `chart.series`
// block in the report, handed to `ReportDocument` as a lookup the way the
// live-table overlay resolver is.
//
// `useQueries` and not one query per block component: `features/**` cannot
// import `app/**`, so the block cannot own its query — and a lookup keyed by
// `(blockId, rev)` is exactly the shape the design's refresh story wants. A
// block whose payload changed arrives with a new `rev`, appears here under a
// new key and fetches; an unchanged block keeps its key, its cache and its
// timer. The document's own refetch (`['track', id]`) is what moves `rev`,
// which is why `track.report_edited` never touches the series prefix.

import { useQueries, type UseQueryResult } from '@tanstack/react-query';
import { useCallback, useMemo } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReportBlock } from '../../../../core/domain/report.ts';
import type { SeriesResolution } from '../../../../core/domain/report-series.ts';
import { ApiError, trackReportSeriesQueryOptions, type SeriesRead } from '../providers/queries.ts';

export type SeriesResolver = (blockId: string, rev: number) => SeriesResolution | undefined;

function seriesKey(blockId: string, rev: number): string {
  return `${blockId}@${rev}`;
}

/**
 * The query's state as the block reads it. Data wins while it exists — a
 * refetch in flight, or one that failed after a good read, keeps the last
 * row on screen rather than blanking the figure; only a query with nothing
 * yet distinguishes loading from failed.
 */
export function seriesResolutionOf(result: Pick<UseQueryResult<SeriesRead>, 'data' | 'isError' | 'error'>): SeriesResolution {
  if (result.data !== undefined) return result.data;
  if (result.isError) {
    const message = result.error instanceof ApiError ? result.error.failure.message : String(result.error);
    return { status: 'error', message };
  }
  return { status: 'loading' };
}

export function useReportSeriesResolver(
  transport: ApiTransportPort, trackId: string, blocks: readonly ReportBlock[] | null,
  unauthorized: UnauthorizedChannel,
): SeriesResolver {
  const seriesBlocks = useMemo(
    () => (blocks ?? []).flatMap((block) => (block.kind === 'chart.series' ? [{ id: block.id, rev: block.rev }] : [])),
    [blocks],
  );
  const combine = useCallback(
    (results: UseQueryResult<SeriesRead>[]) => new Map(results.map((result, index) => {
      const block = seriesBlocks[index];
      return [block === undefined ? '' : seriesKey(block.id, block.rev), seriesResolutionOf(result)] as const;
    })),
    [seriesBlocks],
  );
  const resolutions = useQueries({
    queries: seriesBlocks.map((block) =>
      trackReportSeriesQueryOptions(transport, trackId, block.id, block.rev, unauthorized)),
    combine,
  });
  return useCallback((blockId: string, rev: number) => resolutions.get(seriesKey(blockId, rev)), [resolutions]);
}
