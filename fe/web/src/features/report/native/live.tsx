import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import type { LiveViewBlockPayload } from '../../../../../core/domain/report.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { NativeReportView } from './public.tsx';

/** Overlay resolution changes the data source, while presentation uses the inline contract. */
export function ReportLiveViewBlock({ payload, resolveOverlay, onOpenSourceLink }: {
  payload: LiveViewBlockPayload;
  resolveOverlay?: (source: string) => unknown;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  if (resolveOverlay === undefined) return <p>This view does not carry live data.</p>;
  const raw = resolveOverlay(payload.source);
  if (raw === undefined) return <p>Waiting for {payload.source}.</p>;
  const parsed = nativeViewPayloadSchema.safeParse(raw);
  if (!parsed.success || parsed.data.version !== payload.version) {
    return <p role="status">Live view data does not match the declared version and composition.</p>;
  }
  return <NativeReportView payload={parsed.data} onOpenSourceLink={onOpenSourceLink} />;
}
