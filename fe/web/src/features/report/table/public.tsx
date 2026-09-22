// A live source carries either the original table contract or an explicit native view.
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { reportLiveViewSchema } from '../../../../../core/domain/report-live-view.ts';
import {
  inlineTableBlockPayloadSchema, isLiveTablePayload, type TableBlockPayload,
} from '../../../../../core/domain/report.ts';
import { ReportLiveViewBlock } from '../rich/public.tsx';
import { InlineTable } from './inline.tsx';
import styles from './table.module.css';

export function ReportTableBlock({ payload, resolveLive, onOpenSourceLink }: {
  payload: TableBlockPayload;
  resolveLive?: (source: string) => unknown;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  if (isLiveTablePayload(payload)) {
    if (resolveLive === undefined) {
      return <LiveTableNotice caption={payload.caption} text="This table is live and this view does not carry live data." />;
    }
    const resolved = resolveLive(payload.source);
    if (resolved === undefined) {
      return <LiveTableNotice caption={payload.caption} text={`Waiting for ${payload.source} — nothing has been pushed here yet.`} />;
    }
    const view = reportLiveViewSchema.safeParse(resolved);
    if (view.success) return <ReportLiveViewBlock payload={view.data} onOpenSourceLink={onOpenSourceLink} />;
    const decoded = inlineTableBlockPayloadSchema.safeParse(resolved);
    if (!decoded.success) {
      return <LiveTableNotice caption={payload.caption} text={`${payload.source} holds something this build cannot read as a table.`} />;
    }
    return <InlineTable payload={decoded.data} fallbackCaption={payload.caption} onOpenSourceLink={onOpenSourceLink} />;
  }
  return <InlineTable payload={payload} onOpenSourceLink={onOpenSourceLink} />;
}

function LiveTableNotice({ caption, text }: { caption?: string | null; text: string }) {
  return (
    <div className={styles.wrap}>
      {caption != null && caption !== '' && <p className={styles.caption}>{caption}</p>}
      <p className={styles.caption}>{text}</p>
    </div>
  );
}
