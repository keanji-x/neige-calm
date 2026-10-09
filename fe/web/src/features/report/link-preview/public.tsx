import type { ReactNode } from 'react';
import { GitHubPreviewContent } from '../../../systems/github-links/public.tsx';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import type { ReportLinkTarget, TrackReport } from '../../../../../core/domain/report.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { ReportSourceCitation } from '../source/public.tsx';
import { FileCodePreview, FileReadError, useReportFileResource } from '../../../systems/fs-viewers/public.tsx';
import { HoverPreview } from '../../../ui/hover-preview/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './content.module.css';

export type ReportLinkPreviewResources = Readonly<{
  files: WorkspaceFilePort;
  trackId: string;
  report: TrackReport | null;
  /** The app owns the authenticated, track-scoped source query and its cache. */
  renderSource?: (target: ReportSourceLinkTarget) => ReactNode;
  renderReference?: (target: ReportLinkTarget) => ReactNode;
}>;

export type PreviewDestination =
  | Readonly<{ kind: 'file'; path: string }>
  | Readonly<{ kind: 'web'; url: string; image: boolean }>
  | Readonly<{ kind: 'source'; destination: string; target: ReportSourceLinkTarget }>
  | Readonly<{ kind: 'reference'; destination: string; target: ReportLinkTarget }>;

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
  trigger: (activate: () => void, dismissForNavigation: () => void) => ReactNode;
  renderMarkdown: (text: string, basePath?: string) => ReactNode;
  onOpen?: () => void;
}>) {
  const identity = destination.kind === 'file' ? destination.path : destination.kind === 'web' ? destination.url : destination.destination;
  return <HoverPreview key={`${resources?.trackId ?? ''}:${identity}`} title={label} trigger={trigger} getReadingSurface={readingSurface} getAvoidSurfaces={() => readingAreas()}>
    {dismiss => <>
    {destination.kind === 'file' && (resources === undefined
      ? <p>Open this file to read its contents.</p>
      : <FileContent path={destination.path} files={resources.files} renderMarkdown={renderMarkdown} />)}
    {destination.kind === 'web' && <ExternalContent key={destination.url} url={destination.url} image={destination.image} label={label} />}
    {destination.kind === 'reference' && <ReferenceContent destination={destination} resources={resources} renderMarkdown={renderMarkdown} />}
    {destination.kind === 'source' && (resources?.renderSource !== undefined
      ? resources.renderSource(destination.target)
      : <div className={styles.content}><p className={styles.destination}>{destination.destination}</p><p>此页面未提供来源预览，请打开来源详情。</p></div>)}
    {onOpen !== undefined && <button type="button" className={styles.action} onClick={() => { dismiss(); onOpen(); }}>Open in workspace</button>}
    </>}
  </HoverPreview>;
}

/** One source trigger for prose, inline/live tables and native views. */
export function ReportSourceLinkPreview({ target, label, children, resources, onOpen }: Readonly<{
  target: ReportSourceLinkTarget;
  label: string;
  children: ReactNode;
  resources?: ReportLinkPreviewResources;
  onOpen?: (target: ReportSourceLinkTarget) => void;
}>) {
  return <ReportLinkPreview destination={{ kind: 'source', destination: target.destination, target }}
    resources={resources} label={label} renderMarkdown={() => null}
    trigger={(activate, dismiss) => <ReportSourceCitation target={target}
      onOpen={onOpen === undefined ? (resources?.renderSource === undefined ? undefined : activate)
        : () => { dismiss(); onOpen(target); }}>{children}</ReportSourceCitation>}
    onOpen={onOpen === undefined ? undefined : () => onOpen(target)} />;
}

function readingSurface(trigger: HTMLElement): HTMLElement | null {
  return trigger.closest<HTMLElement>('[data-nc-report-reading]');
}

function readingAreas(): readonly HTMLElement[] {
  // Rendered reports inside a preview belong to that card, not the underlying
  // reading column. Treating them as avoidance areas makes a card chase itself.
  return Array.from(document.querySelectorAll<HTMLElement>('[data-nc-report-reading]'))
    .filter((element) => element.getClientRects().length > 0 && element.closest('[data-nc-link-preview]') === null);
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
        ? <div className="calm-prose">{renderMarkdown(resource.text, basePath)}</div>
        : <div className={styles.source}><FileCodePreview path={path} text={resource.text} /></div>}
    </>}
  </div>;
}

function ReferenceContent({ destination, resources, renderMarkdown }: Readonly<{
  destination: Extract<PreviewDestination, { kind: 'reference' }>;
  resources?: ReportLinkPreviewResources;
  renderMarkdown: (text: string, basePath?: string) => ReactNode;
}>) {
  const target = destination.target;
  if (resources?.renderReference !== undefined) return <div className={styles.content}>{resources.renderReference(target)}</div>;
  const report = target.trackId === resources?.trackId ? resources.report : null;
  const block = report?.blocks?.find((candidate) => candidate.id === target?.blockId);
  const text = block?.kind === 'prose' ? block.payload.markdown
    : report !== null && target?.blockId === null ? report.body || report.summary : null;
  return <div className={styles.content}>
    <p className={styles.destination}>{destination.destination}</p>
    {text ? <div className="calm-prose">{renderMarkdown(text, '')}</div> : <p>Open this reference in the workspace to read its contents.</p>}
  </div>;
}

function ExternalContent(props: Readonly<{ url: string; image: boolean; label: string }>) {
  return props.image ? <WebContent {...props} />
    : <GitHubPreviewContent href={props.url} fallback={<WebContent {...props} />} />;
}

function WebContent({ url, image, label }: Readonly<{ url: string; image: boolean; label: string }>) {
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
