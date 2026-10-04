import { nativeViewPayloadSchema } from '../../../../../core/domain/report-view.ts';
import type { LiveViewBlockPayload } from '../../../../../core/domain/report.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { LivePlaceholder } from '../live/placeholder.tsx';
import { NativeReportView } from './public.tsx';

/** Overlay resolution changes the data source, while presentation uses the inline contract. */
export function ReportLiveViewBlock({ payload, resolveOverlay, onOpenSourceLink }: {
  payload: LiveViewBlockPayload;
  resolveOverlay?: (source: string) => unknown;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  if (resolveOverlay === undefined) return <LivePlaceholder state="detached" source={payload.source} />;
  const raw = resolveOverlay(payload.source);
  if (raw === undefined) return <LivePlaceholder state="pending" source={payload.source} />;
  const parsed = nativeViewPayloadSchema.safeParse(raw);
  if (!parsed.success || parsed.data.version !== payload.version) {
    return <LivePlaceholder state="unavailable" source={payload.source}
      reason="its data does not match the declared version and composition" />;
  }
  return <NativeReportView payload={parsed.data} onOpenSourceLink={onOpenSourceLink} />;
}
