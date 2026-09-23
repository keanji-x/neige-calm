// The track page's preview resolver (#1780): one polled read of the track's registered
// previews, answered per `preview` block by its `key`. Only a report that has a preview
// block polls at all.

import { useQuery, type UseQueryResult } from '@tanstack/react-query';
import { useCallback, useMemo } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { PreviewResolution, ReportBlock, TrackPreviews } from '../../../../core/domain/report.ts';
import { ApiError, trackPreviewsQueryOptions } from '../providers/queries.ts';

export type PreviewResolver = (key: string) => PreviewResolution;

/** Data wins while it exists: a failed poll after a good read keeps the last answer on screen. */
export function previewResolutionOf(
  result: Pick<UseQueryResult<TrackPreviews>, 'data' | 'isError' | 'error'>, key: string,
): PreviewResolution {
  if (result.data !== undefined) {
    const preview = result.data.previews.find((candidate) => candidate.key === key);
    return preview === undefined ? { status: 'missing' } : { status: 'registered', preview };
  }
  if (result.isError) {
    const message = result.error instanceof ApiError ? result.error.failure.message : String(result.error);
    return { status: 'error', message };
  }
  return { status: 'loading' };
}

export function useReportPreviewResolver(
  transport: ApiTransportPort, trackId: string, blocks: readonly ReportBlock[] | null,
  unauthorized: UnauthorizedChannel,
): PreviewResolver {
  const hasPreview = useMemo(() => (blocks ?? []).some((block) => block.kind === 'preview'), [blocks]);
  const { data, isError, error } = useQuery({
    ...trackPreviewsQueryOptions(transport, trackId, unauthorized),
    enabled: hasPreview,
  });
  return useCallback(
    (key: string) => previewResolutionOf({ data, isError, error }, key),
    [data, isError, error],
  );
}
