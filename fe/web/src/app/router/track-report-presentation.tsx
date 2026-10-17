import { useLayoutEffect, useMemo, useRef, type ReactNode } from 'react';
import type { CardWire } from '../../../../core/domain/track.ts';
import { useState } from '../../ui/state/public.ts';
import { useContentAdmission } from '../../ui/content-presentation/public.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { ReportDocument, type ReportDocumentProps } from '../../features/report/document/public.tsx';
import { useReportPreparation } from '../../features/report/document/resource.ts';
import type { ReportPreparationPort } from '../../features/report/document/preparation.ts';
import { revealReportAnchor } from '../../features/report/anchor/public.ts';
import { useReadReceipt } from '../providers/read-receipts.tsx';
import { useTrackReportCache } from './track-view-state.tsx';

/** Queries and controls remain live while this report's pure preparation is pending. */
export function useTrackReportPresentation(trackId: string, cards: readonly CardWire[], execute: ReportPreparationPort) {
  const cache = useTrackReportCache();
  const source = useMemo(() => cache.source(cards), [cache, cards]);
  const admitted = useContentAdmission(trackId, true);
  const resource = useReportPreparation(trackId, source, cache, execute, admitted);
  const committed = useRef<Readonly<{ trackId: string; value: NonNullable<typeof resource.candidate> }> | null>(null);
  const candidate = resource.status === 'ready' ? resource.candidate : null;
  const visible = candidate ?? (committed.current?.trackId === trackId ? committed.current.value : null);
  const presentable = admitted && visible !== null;
  // This effect observes the same complete atomic ReportDocument commit rendered below.
  // A cached candidate withheld by admission is not a fallback for a newer source.
  useLayoutEffect(() => {
    if (presentable && candidate !== null) committed.current = { trackId, value: candidate };
  }, [candidate, presentable, trackId]);
  const report = visible === null ? source.report : visible.preparation.report;
  const outline = visible?.preparation.outline ?? [];
  return { resource, visible, report, outline, presentable,
    current: presentable && resource.status === 'ready' && visible?.source === source };
}

type Presentation = ReturnType<typeof useTrackReportPresentation>;
type ReportRegionProps = Omit<ReportDocumentProps, 'report' | 'preparation' | 'arrivalAnchorId'> & Readonly<{
  presentation: Presentation;
  trackId: string;
  activityAt: number | null;
  openedAt: number;
  children?: ReactNode;
}>;

/** The read effect belongs to the actual mounted report/empty state, never the early shell. */
function ReportRead({ trackId, activityAt, openedAt, enabled, children }: Readonly<{
  trackId: string; activityAt: number | null; openedAt: number; enabled: boolean; children: ReactNode;
}>) {
  useReadReceipt('track', trackId, activityAt, enabled, openedAt);
  return <>{children}</>;
}

export function TrackReportRegion({ presentation, trackId, activityAt, openedAt, children, ...documentProps }: ReportRegionProps) {
  const { resource, visible, presentable, current } = presentation;
  return <section aria-label="Track report" aria-busy={resource.status !== 'failed' && !current}>
    {resource.status === 'failed'
      ? <ErrorBox message="The report could not be loaded." onRetry={resource.retry}
        description={presentable ? 'The last available report is still shown.' : undefined} />
      : !current && resource.status !== 'ready' && <p role="status">{presentable ? 'Updating report…' : 'Loading report…'}</p>}
    {presentable && visible !== null && <ReportRead trackId={trackId} activityAt={activityAt}
      openedAt={openedAt} enabled={current}>
      <ReportDocument {...documentProps} report={visible.preparation.report} preparation={visible.preparation} />
      {children}
    </ReportRead>}
  </section>;
}

/** A navigation intent is served once, after its current complete report commits. */
export function useTrackReportArrival(anchorId: string | null, intent: string | undefined, current: boolean) {
  const [arrival, setArrival] = useState(() => ({ anchorId, intent, request: { anchorId } }));
  let active = arrival;
  if (anchorId !== arrival.anchorId || (intent !== undefined && intent !== arrival.intent)) {
    // File/panel navigation may temporarily remove state and restore it on Back.
    // Keep the last explicit intent so that restoring it cannot replay a served hash.
    active = { anchorId, intent: intent ?? arrival.intent, request: { anchorId } };
    setArrival(active);
  }
  const request = active.request;
  const served = useRef<typeof request | null>(null);
  useLayoutEffect(() => {
    if (!current || request.anchorId === null || request.anchorId === '' || served.current === request) return;
    served.current = request;
    revealReportAnchor(request.anchorId);
  }, [current, request]);
}
