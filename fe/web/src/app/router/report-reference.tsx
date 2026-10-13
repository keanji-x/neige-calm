import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { readFailureOf, readFailureText } from '../../../../core/domain/read-failure.ts';
import { readTrackReport, type ReportLinkTarget, type TrackReport } from '../../../../core/domain/report.ts';
import type { ReportSourceLinkTarget } from '../../../../core/domain/report-source.ts';
import type { ReportFileLinkTarget } from '../../../../core/domain/report-file.ts';
import { toTrack, trackOverlayPayload, type TrackDetailWire } from '../../../../core/domain/track.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { createTrackWorkspaceFilesPort } from '../providers/directory.ts';
import { trackDetailQueryOptions } from '../providers/queries.ts';
import { useReportSeriesResolver } from './report-series.ts';
import { ReportSourcePreview } from './report-source.tsx';

type Navigation = Readonly<{
  onOpenLink: (target: ReportLinkTarget) => void;
  onOpenSource: (trackId: string, target: ReportSourceLinkTarget) => void;
  onOpenFile: (trackId: string, target: ReportFileLinkTarget) => void;
}>;

/** Mounted only while hovering. Reuse the route's authenticated query, decoder and
 * document renderer; each nested file/source remains scoped to its owning Track. */
export function ReportReferencePreview({ transport, unauthorized, target, ...navigation }: Navigation & Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  target: ReportLinkTarget;
}>) {
  const query = useQuery(trackDetailQueryOptions(transport, target.trackId, unauthorized));
  if (query.data !== undefined) {
    const report = readTrackReport(query.data.cards);
    if (report === null) return <p>这份报告暂无内容。</p>;
    const block = target.blockId === null ? null : report.blocks?.find(candidate => candidate.id === target.blockId);
    if (target.blockId !== null && block === undefined) return <p>报告中没有这条引用指向的内容。</p>;
    return <ReferenceDocument transport={transport} unauthorized={unauthorized} target={target}
      detail={query.data} report={block == null ? report : { summary: '', body: '', blocks: [block] }} {...navigation} />;
  }
  if (query.isError) return <ErrorBox message={readFailureText(readFailureOf(query.error), '无法读取报告。')}
    onRetry={() => { void query.refetch(); }} />;
  return <p role="status">正在读取报告…</p>;
}

function ReferenceDocument({ transport, unauthorized, target, detail, report, onOpenLink, onOpenSource, onOpenFile }: Navigation & Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  target: ReportLinkTarget;
  detail: TrackDetailWire;
  report: TrackReport;
}>) {
  const files = useMemo(() => createTrackWorkspaceFilesPort(transport, unauthorized, target.trackId),
    [transport, unauthorized, target.trackId]);
  const resolveSeries = useReportSeriesResolver(transport, target.trackId, report.blocks, unauthorized);
  return <ReportDocument report={report} empty={<p>这份报告暂无内容。</p>} fileRoot={toTrack(detail.track).agentCwd}
    onOpenLink={onOpenLink} onOpenSourceLink={source => onOpenSource(target.trackId, source)}
    onOpenFileLink={file => onOpenFile(target.trackId, file)}
    resolveOverlay={source => trackOverlayPayload(target.trackId, detail.overlays, source)} resolveSeries={resolveSeries}
    linkPreview={{ files, trackId: target.trackId, report,
      renderSource: source => <ReportSourcePreview transport={transport} unauthorized={unauthorized} trackId={target.trackId} target={source} />,
      renderReference: reference => <ReportReferencePreview transport={transport} unauthorized={unauthorized} target={reference}
        onOpenLink={onOpenLink} onOpenSource={onOpenSource} onOpenFile={onOpenFile} />,
    }} />;
}
