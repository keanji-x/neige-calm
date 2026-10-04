// Both inline and live tables obey the same table-only contract.
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import {
  inlineTableBlockPayloadSchema, isLiveTablePayload, type TableBlockPayload,
} from '../../../../../core/domain/report.ts';
import { LivePlaceholder } from '../live/placeholder.tsx';
import { InlineTable } from './inline.tsx';

export function ReportTableBlock({ payload, resolveLive, onOpenSourceLink }: {
  payload: TableBlockPayload;
  resolveLive?: (source: string) => unknown;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
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
    return <InlineTable payload={decoded.data} fallbackCaption={payload.caption} onOpenSourceLink={onOpenSourceLink} />;
  }
  return <InlineTable payload={payload} onOpenSourceLink={onOpenSourceLink} />;
}
