import type { ReactNode } from 'react';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import type { ReportLinkTarget, TrackReport } from '../../../../../core/domain/report.ts';
import { FileReadError, useReportFileResource } from '../../../systems/fs-viewers/public.tsx';
import { HoverPreview } from '../../../ui/hover-preview/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './content.module.css';

export type ReportLinkPreviewResources = Readonly<{
  files: WorkspaceFilePort;
  trackId: string;
  report: TrackReport | null;
}>;

export type PreviewDestination =
  | Readonly<{ kind: 'file'; path: string }>
  | Readonly<{ kind: 'web'; url: string; image: boolean }>
  | Readonly<{ kind: 'reference'; destination: string; target?: ReportLinkTarget }>;

/** External resources are opt-in and HTTP(S) only. Never admit credentials or
 * resolve a protocol-relative/relative URL against the authenticated app origin. */
export function externalPreviewUrl(destination: string): string | null {
  if (!/^https?:\/\//i.test(destination) || Array.from(destination).some((character) => character.charCodeAt(0) <= 32 || character.charCodeAt(0) === 127 || character === '\\')) return null;
  try {
    const url = new URL(destination);
    if (url.username !== '' || url.password !== '') return null;
    return url.href;
  } catch { return null; }
}

export function ReportLinkPreview({ destination, resources, label, trigger, renderMarkdown, onOpen }: Readonly<{
  destination: PreviewDestination;
  resources?: ReportLinkPreviewResources;
  label: string;
  trigger: (pin: () => void) => ReactNode;
  renderMarkdown: (text: string, basePath?: string) => ReactNode;
  onOpen?: () => void;
}>) {
  const identity = destination.kind === 'file' ? destination.path : destination.kind === 'web' ? destination.url : destination.destination;
  return <HoverPreview key={`${resources?.trackId ?? ''}:${identity}`} title={label} trigger={trigger} getReadingSurface={readingSurface} getAvoidSurfaces={() => readingAreas()}>
    {destination.kind === 'file' && (resources === undefined
      ? <p>Open this file to read its contents.</p>
      : <FileContent path={destination.path} files={resources.files} renderMarkdown={renderMarkdown} />)}
    {destination.kind === 'web' && <ExternalContent key={destination.url} url={destination.url} image={destination.image} label={label} />}
    {destination.kind === 'reference' && <ReferenceContent destination={destination} resources={resources} renderMarkdown={renderMarkdown} />}
    {onOpen !== undefined && <button type="button" className={styles.action} onClick={onOpen}>Open in workspace</button>}
  </HoverPreview>;
}

function readingSurface(trigger: HTMLElement): HTMLElement | null {
  return trigger.closest<HTMLElement>('[data-nc-report-reading]');
}

function readingAreas(): readonly HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>('[data-nc-report-reading]'))
    .filter((element) => element.getClientRects().length > 0);
}

function FileContent({ path, files, renderMarkdown }: Readonly<{
  path: string;
  files: WorkspaceFilePort;
  renderMarkdown: (text: string, basePath?: string) => ReactNode;
}>) {
  const resource = useReportFileResource(path, files);
  const basePath = path.slice(0, Math.max(0, path.lastIndexOf('/')));
  return <div className={styles.content}>
    <p className={styles.destination}>{path}</p>
    {resource.kind === 'loading' && <p role="status">Loading file…</p>}
    {resource.kind === 'error' && <FileReadError failure={resource.failure} onRetry={resource.retry} />}
    {resource.kind === 'image' && <img className={styles.image} src={resource.url} alt={path} onLoad={resource.onLoad} onError={resource.onError} />}
    {resource.kind === 'loaded' && <>
      {resource.truncated && <p>Showing the first 2 MiB of this file.</p>}
      {resource.format === 'markdown'
        ? renderMarkdown(resource.text, basePath)
        : <pre className={styles.source}><code>{resource.text}</code></pre>}
    </>}
  </div>;
}

function ReferenceContent({ destination, resources, renderMarkdown }: Readonly<{
  destination: Extract<PreviewDestination, { kind: 'reference' }>;
  resources?: ReportLinkPreviewResources;
  renderMarkdown: (text: string, basePath?: string) => ReactNode;
}>) {
  const target = destination.target;
  const report = target !== undefined && target.trackId === resources?.trackId ? resources.report : null;
  const block = report?.blocks?.find((candidate) => candidate.id === target?.blockId);
  const text = block?.kind === 'prose' ? block.payload.markdown
    : report !== null && target?.blockId === null ? report.body || report.summary : null;
  return <div className={styles.content}>
    <p className={styles.destination}>{destination.destination}</p>
    {text ? renderMarkdown(text, '') : <p>Open this reference in the workspace to read its contents.</p>}
  </div>;
}

function ExternalContent({ url, image, label }: Readonly<{ url: string; image: boolean; label: string }>) {
  const [loaded, setLoaded] = useState(false);
  const [failed, setFailed] = useState(false);
  const [ready, setReady] = useState(false);
  return <div className={styles.content}>
    <p className={styles.destination}>{url}</p>
    {!loaded && <>
      <p>{image ? 'Load this image to preview it.' : 'Load this webpage to browse it here. Some sites only allow opening in a new tab.'}</p>
      <button type="button" className={styles.action} onClick={() => setLoaded(true)}>{image ? 'Load image' : 'Load webpage'}</button>
    </>}
    {loaded && !ready && !failed && <p role="status">Loading preview…</p>}
    {loaded && image && !failed && <img className={styles.image} src={url} alt={label} referrerPolicy="no-referrer"
      onLoad={() => setReady(true)} onError={() => setFailed(true)} />}
    {loaded && !image && <>
      <iframe className={styles.frame} src={url} title={label} sandbox="allow-scripts" referrerPolicy="no-referrer" onLoad={() => setReady(true)} />
      <p>If this page cannot be displayed, open it in a new tab.</p>
    </>}
    {failed && <p role="alert">Could not load this image. You can still open the original.</p>}
    <a className={styles.action} href={url} target="_blank" rel="noopener noreferrer">Open in new tab ↗</a>
  </div>;
}
