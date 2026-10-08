import { memo, useMemo, type ReactNode } from 'react';
import { GitHubPreviewLink, useGitHubPreviewsEnabled } from '../../../systems/github-links/public.tsx';
import { Markdown } from '@astryxdesign/core/Markdown';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { parseReportFileLink, reportFilePathRelativeToRoot } from '../../../../../core/domain/report-file.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './reply.module.css';

/** Bound by the app to the conversation's own Track. The server owns file containment. */
export type ReplyLinkRenderer = (props: Readonly<{ href: string; children: ReactNode }>) => ReactNode;

export type ReplyImageFiles = Readonly<{
  root: string;
  files: Pick<WorkspaceFilePort, 'rawUrl'>;
}>;

function ReplyImage({ src, alt, imageFiles }: {
  src: string; alt: string; imageFiles: ReplyImageFiles | null;
}) {
  const [failed, setFailed] = useState(false);
  // Preserve network images; local destinations must go through the Track-scoped port.
  const remote = /^(https?:\/\/|\/\/)/i.test(src);
  const target = remote ? null : parseReportFileLink(src);
  const relative = target === null || imageFiles === null ? null
    : reportFilePathRelativeToRoot(imageFiles.root, target);
  const url = remote ? src : relative === null ? null : imageFiles?.files.rawUrl(relative) ?? null;
  if (url === null || failed) {
    return <span className={styles.unavailable}>Image unavailable: {alt || src}</span>;
  }
  return <img className={styles.image} src={url} alt={alt} loading="lazy"
    onError={() => { setFailed(true); }} />;
}

/** Stored and streamed replies share one renderer. The app injects shared link rendering
 * and track-scoped image files; code fences and copied responses keep their original text.
 * `headingLevelStart={3}` leaves the page's h1 and sections' h2 above the reply. */
export const Reply = memo(function Reply({ text, imageFiles, renderLink }: { text: string; imageFiles: ReplyImageFiles | null; renderLink?: ReplyLinkRenderer }) {
  const previewsEnabled = useGitHubPreviewsEnabled();
  const components = useMemo(() => ({
    ...(renderLink !== undefined ? { link: renderLink } : previewsEnabled ? { link: ({ href, children }: { href: string; children: ReactNode }) => <GitHubPreviewLink href={href}>{children}</GitHubPreviewLink> } : {}),
    image: ({ src, alt }: { src: string; alt: string }) => (
      <ReplyImage key={src} src={src} alt={alt} imageFiles={imageFiles} />
    ),
  }), [imageFiles, previewsEnabled, renderLink]);
  return <Markdown density="compact" headingLevelStart={3} components={components}>{text}</Markdown>;
});
