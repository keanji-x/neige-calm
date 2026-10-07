// The track page's source panel: the drawer a `neige://source/…` citation opens,
// with the query that fills it. Which citation is open is route-local state, not
// `?source=`: a source is read beside the document and changes nothing about where the reader is.

import { useQuery, type UseQueryResult } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReportSourceLinkTarget, SourceResolution } from '../../../../core/domain/report-source.ts';
import { ReportSourcePanel, reportSourcePanelTitle, SOURCE_PANEL_COPY } from '../../features/report/source/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { readFailureOf } from '../../../../core/domain/read-failure.ts';
import { trackSourceQueryOptions, type SourceRead } from '../providers/queries.ts';

/** Data wins while it exists; a `missing` read is data, so a dangling citation is never an error here. */
export function sourceResolutionOf(result: Pick<UseQueryResult<SourceRead>, 'data' | 'isError' | 'error'>): SourceResolution {
  if (result.data !== undefined) {
    return result.data.status === 'found' ? { status: 'ok', source: result.data.source } : { status: 'missing' };
  }
  if (result.isError) {
    return { status: 'error', failure: readFailureOf(result.error) };
  }
  return { status: 'loading' };
}

/** Shared query for the hover preview and the drawer; never read an invalid id. */
function useSource(transport: ApiTransportPort, trackId: string, target: ReportSourceLinkTarget | null, unauthorized: UnauthorizedChannel) {
  const sourceId = target?.sourceId ?? null;
  return useQuery({
    ...trackSourceQueryOptions(transport, trackId, sourceId ?? '', unauthorized),
    enabled: sourceId !== null,
  });
}

/** Mounted by HoverPreview only while visible, so rendering citations performs no reads. */
export function ReportSourcePreview({ transport, trackId, target, unauthorized }: {
  transport: ApiTransportPort;
  trackId: string;
  target: ReportSourceLinkTarget;
  unauthorized: UnauthorizedChannel;
}) {
  const query = useSource(transport, trackId, target, unauthorized);
  return <ReportSourcePanel target={target} resolution={sourceResolutionOf(query)} scrollToQuote={false}
    onRetry={() => { void query.refetch(); }} />;
}

export function ReportSourceDrawer({ transport, trackId, target, unauthorized, onClose }: {
  transport: ApiTransportPort;
  trackId: string;
  /** The open citation, or `null` when the drawer is shut. */
  target: ReportSourceLinkTarget | null;
  unauthorized: UnauthorizedChannel;
  onClose: () => void;
}) {
  const query = useSource(transport, trackId, target, unauthorized);
  const resolution = sourceResolutionOf(query);
  return (
    <Drawer
      open={target !== null}
      title={target === null ? '' : reportSourcePanelTitle(resolution)}
      mobileBackLabel="Report"
      closeLabel={SOURCE_PANEL_COPY.closeLabel}
      onClose={onClose}
    >
      {target !== null && (
        <ReportSourcePanel target={target} resolution={resolution} onRetry={() => { void query.refetch(); }} />
      )}
    </Drawer>
  );
}
