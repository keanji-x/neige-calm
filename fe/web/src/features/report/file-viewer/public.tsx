import { useEffect, useRef } from 'react';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import type { ReportFileLinkTarget } from '../../../../../core/domain/report-file.ts';
import { useReportFileResource } from '../../../systems/fs-viewers/public.tsx';
import { ReportDocument } from '../document/public.tsx';
import styles from './report-file-viewer.module.css';

function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1);
}

function parentDirectory(path: string): string {
  const separator = path.lastIndexOf('/');
  return separator <= 0 ? '' : path.slice(0, separator);
}

/**
 * A file rendered as another document in the Report workspace. It deliberately
 * has no card head, directory tree or Diff mode: following evidence from prose
 * keeps the reader in the document vocabulary rather than moving them onto the
 * card board.
 */
export function ReportFileViewer({
  path, files, fileRoot, wide, onClose, onFileOpened, onOpenFileLink,
}: Readonly<{
  path: string;
  files: WorkspaceFilePort;
  fileRoot: string;
  /** Code, text and images may use the panel column when no card owns it. */
  wide: boolean;
  onClose: () => void;
  onFileOpened?: (path: string) => void;
  onOpenFileLink?: (target: ReportFileLinkTarget) => void;
}>) {
  const layerRef = useRef<HTMLDivElement | null>(null);
  const resource = useReportFileResource(path, files, onFileOpened);

  useEffect(() => { layerRef.current?.focus({ preventScroll: true }); }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) !== layerRef.current) return;
      onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); };
  }, [onClose]);

  return (
    <div
      ref={layerRef}
      className={styles.layer}
      data-nc-report-file-viewer=""
      data-nc-report-file-wide={wide ? '' : undefined}
      data-nc-escape-layer=""
      role="region"
      aria-label={`File ${path}`}
      tabIndex={-1}
    >
      <div className={`${styles.body} ${wide ? styles.bodyWide : ''}`}>
        <div className={styles.fileName} title={path}>{basename(path)}</div>
        {resource.kind === 'loading' && <p className={styles.state} role="status">Loading file…</p>}
        {resource.kind === 'error' && <p className={styles.error} role="alert">{resource.message}</p>}
        {resource.kind === 'image' && (
          <figure className={styles.imageWrap}>
            <img
              className={styles.image}
              src={resource.url}
              alt={resource.path}
              onLoad={resource.onLoad}
              onError={resource.onError}
            />
          </figure>
        )}
        {resource.kind === 'loaded' && resource.format === 'markdown' && (
          <div className={styles.markdown}>
            {resource.truncated && <p className={styles.notice}>Showing the first 2 MiB of this file.</p>}
            <ReportDocument
              report={{ summary: '', body: resource.text, blocks: null }}
              empty={<></>}
              fileRoot={fileRoot}
              fileBasePath={parentDirectory(resource.path)}
              onOpenFileLink={onOpenFileLink}
            />
          </div>
        )}
        {resource.kind === 'loaded' && resource.format === 'source' && (
          <div className={styles.sourceWrap}>
            {resource.truncated && <p className={styles.notice}>Showing the first 2 MiB of this file.</p>}
            <pre className={styles.source} data-nc-report-file-source=""><code>{resource.text}</code></pre>
          </div>
        )}
      </div>
    </div>
  );
}
