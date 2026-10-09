// Both inline and live tables obey the same table-only contract.
import type { ReactNode } from 'react';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import {
  inlineTableBlockPayloadSchema, isLiveTablePayload, type TableBlockPayload,
} from '../../../../../core/domain/report.ts';
import { LivePlaceholder } from '../live/placeholder.tsx';
import type { ReportLinkPreviewResources } from '../link-preview/public.tsx';
import { InlineTable } from './inline.tsx';

export function ReportTableBlock({ payload, resolveLive, onOpenSourceLink, linkPreview, renderText }: {
  renderText?: (text: string) => ReactNode;
  payload: TableBlockPayload;
  resolveLive?: (source: string) => unknown;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  linkPreview?: ReportLinkPreviewResources;
}) {
  if (isLiveTablePayload(payload)) {
    if (resolveLive === undefined) {
      return <LivePlaceholder state="detached" source={payload.source} caption={payload.caption} />;
    }
    const resolved = resolveLive(payload.source);
    if (resolved === undefined) {
      return <LivePlaceholder state="pending" source={payload.source} caption={payload.caption} />;
    }
    const decoded = inlineTableBlockPayloadSchema.safeParse(resolved);
    if (!decoded.success) {
      return <LivePlaceholder state="unavailable" source={payload.source} reason="this build cannot read it as a table"
        caption={payload.caption} />;
    }
    return <InlineTable renderText={renderText} payload={decoded.data} fallbackCaption={payload.caption} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
  }
  return <InlineTable renderText={renderText} payload={payload} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
}
