import { MarkdownBlocks, markdownInlineText } from '../../../ui/markdown/public.tsx';
import { parse, sanitizeAstPolicy } from '../../../../../core/markdown/public.ts';
import { createElement, memo, useMemo, type ReactNode } from 'react';
import { GitHubPreviewLink, useGitHubPreviewsEnabled } from '../../../systems/github-links/public.tsx';

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

/** Stored and streamed replies share one renderer and Track-scoped capabilities. */
export const Reply = memo(function Reply({ text, imageFiles, renderLink }: { text: string; imageFiles: ReplyImageFiles | null; renderLink?: ReplyLinkRenderer }) {
  const previewsEnabled = useGitHubPreviewsEnabled();
  const result = useMemo(() => parse(text), [text]);
  if (result.status === 'failed') return <pre className={styles.raw}>{text}</pre>;
  const ast = sanitizeAstPolicy(result.value, { rawHtml: 'literal' });
  const headingIds = new Map<number, string>();
  const counts = new Map<string, number>();
  for (const block of ast.children) {
    if (block.type !== 'heading') continue;
    const slug = markdownInlineText(block.children, '').trim().toLowerCase().replace(/['"]/g, '')
      .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'section';
    const count = counts.get(slug) ?? 0;
    counts.set(slug, count + 1);
    headingIds.set(block.position.start.offset, count === 0 ? slug : `${slug}-${count}`);
  }
  return <MarkdownBlocks blocks={ast.children} presentation={{
    classes: { paragraph: styles.paragraph, list: styles.list, item: styles.item, check: styles.check,
      quote: styles.quote, inlineCode: styles.inlineCode, tableWrap: styles.tableWrap, table: styles.table,
      cell: styles.cell, rule: styles.rule },
    heading: (depth, offset, children) => createElement(`h${Math.min(depth + 2, 6)}`, { id: headingIds.get(offset), className: `${styles.heading} ${depth <= 3 ? styles.majorHeading : styles.minorHeading}` }, children),
    link: (href, _label, children) => {
      // Browsers ignore control characters within a scheme. Never turn unsafe destinations into links.
      const normalized = Array.from(href).filter(char => char.charCodeAt(0) > 31 && char.charCodeAt(0) !== 127).join('').trim();
      if (normalized === '' || /^(?:javascript|vbscript|data):/i.test(normalized)) return children;
      if (renderLink !== undefined) return renderLink({ href: normalized, children });
      if (previewsEnabled) return <GitHubPreviewLink href={normalized}>{children}</GitHubPreviewLink>;
      const external = normalized.startsWith('https://') || normalized.startsWith('http://');
      return <a className={styles.link} href={normalized} {...(external ? { target: '_blank', rel: 'noopener noreferrer' } : {})}>{children}</a>;
    },
    image: (src, alt) => <ReplyImage key={src} src={src} alt={alt} imageFiles={imageFiles} />,
  }} />;
});
