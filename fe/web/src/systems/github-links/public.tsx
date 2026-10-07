import { createContext, useContext, useEffect, type ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import { HoverPreview } from '../../ui/hover-preview/public.tsx';
import { parseGitHubReferenceUrl, type GitHubReference } from '../../../../core/domain/issue-url.ts';
import type { GitHubPreviewPort } from '../../../../core/domain/github-preview.ts';
import { useState } from '../../ui/state/public.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { PreviewSummary, PreviewTextLink } from '../../ui/hover-preview/summary.tsx';

const PreviewPort = createContext<GitHubPreviewPort | null>(null);

/** App-owned session/recovery-aware reads. Queries use the app's QueryClient and die with it.
 * No provider means no previews. GitHub data is text, never HTML or a nested Markdown document. */
export function GitHubPreviewProvider({ port, children }: { port: GitHubPreviewPort; children: ReactNode }) {
  return <PreviewPort.Provider value={port}>{children}</PreviewPort.Provider>;
}

function useDesktopHover(): boolean {
  const compact = useCompactViewport();
  const [fine, setFine] = useState(() => globalThis.matchMedia?.('(hover: hover) and (pointer: fine)').matches ?? false);
  useEffect(() => {
    const media = globalThis.matchMedia?.('(hover: hover) and (pointer: fine)');
    if (media === undefined) return;
    const sync = () => setFine(media.matches);
    sync(); media.addEventListener('change', sync);
    return () => media.removeEventListener('change', sync);
  }, []);
  return !compact && fine;
}

/** Let a host retain its original renderer on unsupported devices or without an injected port. */
export function useGitHubPreviewsEnabled(): boolean {
  const port = useContext(PreviewPort);
  const desktop = useDesktopHover();
  return port !== null && desktop;
}

/** Chat keeps its original link navigation. Desktop previews share the report's hover primitive. */
export function GitHubPreviewLink({ href, children }: { href: string; children: ReactNode }) {
  const target = parseGitHubReferenceUrl(href);
  const port = useContext(PreviewPort);
  const desktop = useDesktopHover();
  const external = href.startsWith('https://') || href.startsWith('http://');
  const link = <PreviewTextLink href={href} external={external}>{children}</PreviewTextLink>;
  if (target === null || port === null || !desktop) return link;
  return <HoverPreview key={target.url} title={`GitHub ${target.owner}/${target.name} #${target.number}`} trigger={() => link}>
    <Preview target={target} port={port} />
  </HoverPreview>;
}

/** Render inside an existing lazy preview. Unsupported devices/URLs keep the host's original content. */
export function GitHubPreviewContent({ href, fallback }: { href: string; fallback: ReactNode }) {
  const target = parseGitHubReferenceUrl(href);
  const port = useContext(PreviewPort);
  const desktop = useDesktopHover();
  return target === null || port === null || !desktop ? <>{fallback}</>
    : <Preview key={target.url} target={target} port={port} />;
}

function Preview({ target, port }: { target: GitHubReference; port: GitHubPreviewPort }) {
  const query = useQuery({
    queryKey: ['github-preview', target.owner, target.name, target.kind, target.number],
    queryFn: ({ signal }) => port.read(target, signal),
    staleTime: 60_000, gcTime: 60_000, retry: false, refetchOnWindowFocus: false,
  });
  const preview = query.data;
  return <PreviewSummary title={preview?.title}
    metadata={`${target.owner}/${target.name} · #${target.number}`}
    detail={preview === undefined ? undefined : `${preview.kind === 'pull' ? 'Pull request' : 'Issue'} · ${preview.state} · ${preview.author}`}
    tags={preview?.labels}
    footer={<>
      {query.isError && <button type="button" disabled={query.isFetching}
        onClick={() => { void query.refetch(); }}>{query.isFetching ? 'Retrying…' : 'Retry'}</button>}
      <a href={target.url} target="_blank" rel="noopener noreferrer">Open in GitHub ↗</a>
    </>}>
    {preview === undefined ? <p role="status">{query.isError
      ? 'Preview unavailable. Check GitHub sign-in, repository access, or rate limits.'
      : 'Loading GitHub preview…'}</p> : <>
      {preview.excerpt && <p>{preview.excerpt}</p>}
      {preview.changes !== null && <p>{preview.changes.changed_files} files · +{preview.changes.additions} / −{preview.changes.deletions}</p>}
      {query.isError && <p role="status">Could not refresh preview.</p>}
    </>}
  </PreviewSummary>;
}
