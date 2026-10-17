import { GuideAnchor } from '../../../ui/floating-control/public.ts';
// The `html` block (#2585) and the file viewer's page view: one self-contained page from the Track
// workspace in a frame that may run scripts and nothing else. No `allow-same-origin` (the page is
// an opaque origin: no cookies, storage or API), no forms, popups, modals or top navigation. The
// kernel serves the page under the same sandbox as a CSP, so it stays contained even when opened
// on its own.
//
// An iframe cannot report an HTTP error, so the page is read as text first
// (`useReportFileResource`) and the frame is mounted only after that read succeeds.

import { Button } from '@astryxdesign/core/Button';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { reportFrameHeight } from '../../../../../core/domain/report-frame.ts';
import type { HtmlBlockPayload } from '../../../../../core/domain/report-html.ts';
import { FileReadError, useReportFileResource } from '../../../systems/fs-viewers/public.tsx';
import styles from './html.module.css';

const DEFAULT_HEIGHT = 480;

/** Scripts only. Adding `allow-same-origin` would hand the page this origin's cookies and API. */
export const HTML_FRAME_SANDBOX = 'allow-scripts';

/**
 * The frame itself, shared by the block and the file viewer. The mobile app's document policy
 * blocks every frame without telling the page, so there it says where the page can be seen instead.
 */
export function ReportHtmlFrame({ url, title, height }: Readonly<{
  url: string;
  title: string;
  /** In px; absent ⇒ the frame fills the box its parent gives it. */
  height?: number;
}>) {
  if (__NC_BUNDLED__) return <BundledNote height={height} />;
  return (
    <iframe
      className={height === undefined ? `${styles.frame} ${styles.fill}` : styles.frame}
      src={url}
      title={title}
      style={height === undefined ? undefined : { blockSize: `${height}px` }}
      sandbox={HTML_FRAME_SANDBOX}
      referrerPolicy="no-referrer"
      loading="lazy"
    />
  );
}

function BundledNote({ height }: Readonly<{ height?: number }>) {
  return <p className={styles.note} style={height === undefined ? undefined : { blockSize: `${height}px` }} role="note">
    Open this page on desktop: the mobile app does not show embedded pages.
  </p>;
}

export function ReportHtmlBlock({ payload, files, onOpen }: Readonly<{
  payload: HtmlBlockPayload;
  /** The Track workspace the page lives in. Absent ⇒ this view carries no workspace and the block says so. */
  files?: WorkspaceFilePort;
  /** Opens the page full size in the file viewer. */
  onOpen?: (path: string) => void;
}>) {
  const title = payload.title != null && payload.title.trim() !== '' ? payload.title : payload.path;
  const height = reportFrameHeight(payload.height, DEFAULT_HEIGHT);
  return (
    <figure className={styles.figure}>
      {/* The mobile app cannot show the frame, so it does not read the page either. */}
      {__NC_BUNDLED__
        ? <BundledNote height={height} />
        : files === undefined
          ? <p className={styles.note} style={{ blockSize: `${height}px` }} role="note">This view does not carry workspace files.</p>
          : <BlockPage path={payload.path} files={files} title={title} height={height} />}
      <figcaption className={styles.caption}>
        <span className={styles.title}>{title}</span>
        {onOpen !== undefined && (
          <GuideAnchor id="preview-open" placement="top">{(anchor, describedBy) => <Button ref={anchor} aria-describedby={describedBy} label={`Open ${payload.path}`} variant="ghost" size="sm" onClick={() => { onOpen(payload.path); }}>Open</Button>}</GuideAnchor>
        )}
      </figcaption>
    </figure>
  );
}

function BlockPage({ path, files, title, height }: Readonly<{
  path: string; files: WorkspaceFilePort; title: string; height: number;
}>) {
  const resource = useReportFileResource(path, files);
  switch (resource.kind) {
    case 'html': return <ReportHtmlFrame key={resource.text} url={resource.url} title={title} height={height} />;
    case 'loading': return <p className={styles.note} style={{ blockSize: `${height}px` }} role="status">Loading page…</p>;
    case 'error':
      return <div className={styles.unavailable}>
        <FileReadError failure={resource.failure} resource={resource.resource} onRetry={resource.retry} />
      </div>;
    // A page over the 2 MiB read cap reads as truncated source; the raw route would refuse it.
    case 'loaded':
    case 'image':
      return <p className={styles.note} style={{ blockSize: `${height}px` }} role="note">This page is larger than 2 MiB and is not shown inline.</p>;
  }
}
