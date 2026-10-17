import { useRouter } from '@tanstack/react-router';
import { Children, isValidElement, useCallback, useMemo, type ReactNode } from 'react';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import type { ReportLinkTarget } from '../../../../core/domain/report.ts';
import type { ReportFileLinkTarget } from '../../../../core/domain/report-file.ts';
import type { ReportSourceLinkTarget } from '../../../../core/domain/report-source.ts';
import { ReportTextLink } from '../../features/report/document/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ReportReferencePreview } from './report-reference.tsx';
import { ReportSourceDrawer, ReportSourcePreview } from './report-source.tsx';

function linkLabel(children: ReactNode): string {
  return Children.toArray(children).map(child => typeof child === 'string' || typeof child === 'number' ? String(child)
    : isValidElement<{ children?: ReactNode }>(child) ? linkLabel(child.props.children) : '').join('');
}

/** Compose the report-owned Markdown link contract into chat, without a feature-to-feature dependency.
 * One source drawer per conversation pane; hover reads mount only when the preview is visible. */
export function useConversationLinks({ transport, unauthorized, conversationId, trackId, root, files, onOpenLink, onOpenFile }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
  conversationId: string | null; trackId: string | null; root: string | null; files: WorkspaceFilePort | null;
  onOpenLink: (target: ReportLinkTarget) => void;
  onOpenFile: (trackId: string, target: ReportFileLinkTarget) => void;
}>) {
  const router = useRouter();
  const [state, setState] = useState<{
    conversationId: string | null; conversationTrackId: string | null;
    selection: { trackId: string; target: ReportSourceLinkTarget } | null;
  }>({ conversationId, conversationTrackId: trackId, selection: null });
  const sameConversation = state.conversationId === conversationId && state.conversationTrackId === trackId;
  // Clear in the switching render: neither B nor a return to A may expose A's selection.
  if (!sameConversation) setState({ conversationId, conversationTrackId: trackId, selection: null });
  const openSource = useCallback((owner: string, target: ReportSourceLinkTarget) => {
    setState({ conversationId, conversationTrackId: trackId, selection: { trackId: owner, target } });
  }, [conversationId, trackId]);
  const renderLink = useMemo(() => ({ href, children }: { href: string; children: ReactNode }) => {
    // The router owns the application namespace; rejected workspace paths never become anchors.
    const base = (router.options.basepath ?? '').replace(/\/$/, '');
    if (href.startsWith('/') && !href.startsWith('//')) {
      // URL treats backslashes as separators; do not reinterpret Markdown as app navigation.
      if (href.includes('\\')) return children;
      let url: URL;
      try { url = new URL(href, window.location.origin); }
      catch { return children; }
      if (url.origin === window.location.origin && (url.pathname === base || url.pathname.startsWith(`${base}/`))) {
        return <button type="button" onClick={() => { void router.navigate({ href: url.pathname + url.search + url.hash }); }}>{children}</button>;
      }
    }
    if (/^(mailto:|tel:|#)/i.test(href)) return <a href={href}>{children}</a>;
    return <ReportTextLink href={href} referenceRepository={null} label={linkLabel(children) || href} fileRoot={root ?? undefined}
      onOpenLink={onOpenLink} onOpenFileLink={trackId === null ? undefined : target => onOpenFile(trackId, target)}
      onOpenSourceLink={trackId === null ? undefined : target => openSource(trackId, target)}
      linkPreview={trackId === null || files === null ? undefined : { trackId, files, report: null,
        renderSource: target => <ReportSourcePreview transport={transport} unauthorized={unauthorized} trackId={trackId} target={target} />,
        renderReference: target => <ReportReferencePreview transport={transport} unauthorized={unauthorized} target={target}
          onOpenLink={onOpenLink} onOpenFile={onOpenFile} onOpenSource={openSource} />,
      }}>{children}</ReportTextLink>;
  }, [router, root, trackId, files, transport, unauthorized, onOpenLink, onOpenFile, openSource]);
  const current = sameConversation ? state.selection : null;
  return { renderLink, sourceDrawer: current === null ? null : <ReportSourceDrawer transport={transport} unauthorized={unauthorized}
    trackId={current.trackId} target={current.target} onClose={() => setState({ conversationId, conversationTrackId: trackId, selection: null })} /> };
}
