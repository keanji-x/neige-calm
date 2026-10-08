import { Children, isValidElement, useCallback, useEffect, useMemo, type ReactNode } from 'react';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import type { ReportLinkTarget } from '../../../../core/domain/report.ts';
import type { ReportFileLinkTarget } from '../../../../core/domain/report-file.ts';
import type { ReportSourceLinkTarget } from '../../../../core/domain/report-source.ts';
import { ProseLink } from '../../features/report/document/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ReportReferencePreview } from './report-reference.tsx';
import { ReportSourceDrawer, ReportSourcePreview } from './report-source.tsx';

function linkLabel(children: ReactNode): string {
  return Children.toArray(children).map(child => typeof child === 'string' || typeof child === 'number' ? String(child)
    : isValidElement<{ children?: ReactNode }>(child) ? linkLabel(child.props.children) : '').join('');
}

/** Compose the report-owned Markdown link contract into chat, without a feature-to-feature dependency.
 * One source drawer per conversation pane; hover reads mount only when the preview is visible. */
export function useConversationLinks({ transport, unauthorized, trackId, root, files, onOpenLink, onOpenFile }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
  trackId: string | null; root: string | null; files: WorkspaceFilePort | null;
  onOpenLink: (target: ReportLinkTarget) => void;
  onOpenFile: (trackId: string, target: ReportFileLinkTarget) => void;
}>) {
  const [selection, setSelection] = useState<{
    conversationTrackId: string | null; trackId: string; target: ReportSourceLinkTarget;
  } | null>(null);
  useEffect(() => { setSelection(null); }, [trackId]);
  const openSource = useCallback((owner: string, target: ReportSourceLinkTarget) => {
    setSelection({ conversationTrackId: trackId, trackId: owner, target });
  }, [trackId]);
  const renderLink = useMemo(() => ({ href, children }: { href: string; children: ReactNode }) => (
    <ProseLink destination={href} label={linkLabel(children) || href} fileRoot={root ?? undefined}
      onOpenLink={onOpenLink} onOpenFileLink={trackId === null ? undefined : target => onOpenFile(trackId, target)}
      onOpenSourceLink={trackId === null ? undefined : target => openSource(trackId, target)}
      linkPreview={trackId === null || files === null ? undefined : { trackId, files, report: null,
        renderSource: target => <ReportSourcePreview transport={transport} unauthorized={unauthorized} trackId={trackId} target={target} />,
        renderReference: target => <ReportReferencePreview transport={transport} unauthorized={unauthorized} target={target}
          onOpenLink={onOpenLink} onOpenFile={onOpenFile} onOpenSource={openSource} />,
      }}>{children}</ProseLink>
  ), [root, trackId, files, transport, unauthorized, onOpenLink, onOpenFile, openSource]);
  const current = selection?.conversationTrackId === trackId ? selection : null;
  return { renderLink, sourceDrawer: current === null ? null : <ReportSourceDrawer transport={transport} unauthorized={unauthorized}
    trackId={current.trackId} target={current.target} onClose={() => setSelection(null)} /> };
}
