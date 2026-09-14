// The track page's source panel (#1669 §2.5): the drawer a `neige://source/…`
// citation opens, with the query that fills it.
//
// The query lives here and not in the panel for the reason the series
// resolver gives: `features/**` cannot import `app/**`, so the panel cannot
// own its query, and the query's rules — its key, its 404-is-a-state turn —
// are the app's. The panel receives a `SourceResolution` and paints it.
//
// The drawer is `ui/drawer`, the same card the conversation uses, on the
// same right-rail track. Which citation is open is **route-local state**,
// not `?source=` in the URL, and that is a deliberate choice against the file
// viewer's `?file=`: the file viewer is an *overlay over the document* — it
// replaces what the reader is looking at, so Back must close it and a link
// to it must reproduce it. A source is read *beside* the document, like a
// conversation, whose selection is also not in the URL; it changes nothing
// about where the reader is. Leaving the track drops it, which is right: a
// citation belongs to the report it sits in.

import { useQuery, type UseQueryResult } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReportSourceLinkTarget, SourceResolution } from '../../../../core/domain/report-source.ts';
import { SOURCE_PANEL_COPY } from '../../features/report/source/copy.ts';
import { ReportSourcePanel, reportSourcePanelTitle } from '../../features/report/source/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { ApiError, trackSourceQueryOptions, type SourceRead } from '../providers/queries.ts';

/**
 * The query's state as the panel reads it. Data wins while it exists — a
 * refetch that failed after a good read keeps the row on screen; only a
 * query with nothing yet distinguishes loading from failed. A `missing` read
 * is data (see `SourceRead`), so a dangling citation is never an error here.
 */
export function sourceResolutionOf(result: Pick<UseQueryResult<SourceRead>, 'data' | 'isError' | 'error'>): SourceResolution {
  if (result.data !== undefined) {
    return result.data.status === 'found' ? { status: 'ok', source: result.data.source } : { status: 'missing' };
  }
  if (result.isError) {
    const message = result.error instanceof ApiError ? result.error.failure.message : String(result.error);
    return { status: 'error', message };
  }
  return { status: 'loading' };
}

export function ReportSourceDrawer({ transport, trackId, target, unauthorized, onClose }: {
  transport: ApiTransportPort;
  trackId: string;
  /** The open citation, or `null` when the drawer is shut. */
  target: ReportSourceLinkTarget | null;
  unauthorized: UnauthorizedChannel;
  onClose: () => void;
}) {
  /*
   * One query, keyed by the citation's source id, disabled when the link
   * names none (a malformed citation has nothing to fetch — the panel says
   * so from the target alone). Hooks must run unconditionally, so the
   * disabled query still exists under a placeholder key it never fetches.
   */
  const sourceId = target?.sourceId ?? null;
  const query = useQuery({
    ...trackSourceQueryOptions(transport, trackId, sourceId ?? '', unauthorized),
    enabled: sourceId !== null,
  });
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
