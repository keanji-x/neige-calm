// The `preview` block (#1780): a live dev server, embedded from calm's preview gateway. The block
// names a registration by `key`; `resolve`, injected by `app/router`, says whether that key is
// registered, on which gateway port, and whether the dev server answers. Same register as the
// `app` block: a quiet frame, named underneath.
//
// The frame is another origin (same host, the gateway port), so `allow-same-origin` lets the dev
// server keep its own storage and HMR socket without reaching this document. The gateway admits
// only the owner's calm session and speaks plain http, so an https page (a tunnel, a proxy) cannot
// load it at all: the block says so instead of drawing a frame the browser will block.

import { useEffect, useRef } from 'react';

import { useState } from '../../../ui/state/public.ts';

import type { PreviewBlockPayload, PreviewResolution } from '../../../../../core/domain/report.ts';
import styles from './preview.module.css';

const MIN_HEIGHT = 120;
const MAX_HEIGHT = 2000;
const DEFAULT_HEIGHT = 360;

/** The width the mobile toggle narrows the frame to: a common phone viewport. */
export const PREVIEW_MOBILE_WIDTH_PX = 390;

/** The frame's sandbox: scripts and same-origin are safe together only because the frame is another origin. */
export const PREVIEW_SANDBOX = 'allow-scripts allow-same-origin allow-forms allow-popups allow-modals';

type PageLocation = Readonly<{ protocol: string; hostname: string }>;

function clampHeight(height: number | null | undefined): number {
  if (height == null || Number.isNaN(height)) return DEFAULT_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height));
}

/** The gateway listens on the host calm is reached by, on its own port; `path` defaults to the root. */
export function previewFrameUrl(page: PageLocation, port: number, path: string | null | undefined): string {
  return `${page.protocol}//${page.hostname}:${port}${path ?? '/'}`;
}

export function ReportPreviewBlock({ payload, resolve }: {
  payload: PreviewBlockPayload;
  /** The app's read of this track's previews, by key. Absent ⇒ the surface carries no previews and the block says so. */
  resolve?: (key: string) => PreviewResolution;
}) {
  const resolution = resolve?.(payload.key);
  const registered = resolution?.status === 'registered' ? resolution.preview : null;
  const title = nonBlank(payload.title) ?? nonBlank(registered?.title) ?? payload.key;
  const height = clampHeight(payload.height);

  if (resolution === undefined) {
    return <PreviewNotice title={title} height={height}
      text="This view does not carry previews." />;
  }
  if (window.location.protocol !== 'http:') {
    const port = registered === null ? '' : ` · port ${registered.port}`;
    return <PreviewNotice title={title} height={height} text={`预览仅 LAN 可用${port}`} />;
  }
  switch (resolution.status) {
    case 'loading':
      return <PreviewNotice title={title} height={height} text="Loading …" />;
    case 'error':
      return <PreviewNotice title={title} height={height} text={`Could not read previews: ${resolution.message}`} />;
    case 'missing':
      return <PreviewNotice title={title} height={height}
        text={`No preview is registered under “${payload.key}”.`} />;
    case 'registered':
      if (!resolution.preview.live) {
        return <PreviewNotice title={title} height={height}
          text={`Preview “${payload.key}” is offline — waiting for its dev server on port ${resolution.preview.port}.`} />;
      }
      // Offline renders no frame, so coming back live mounts a fresh one: the reload is the remount.
      return <PreviewFrame title={title} height={height}
        src={previewFrameUrl(window.location, resolution.preview.port, payload.path)} />;
  }
}

function nonBlank(value: string | null | undefined): string | null {
  return value != null && value.trim() !== '' ? value : null;
}

function PreviewNotice({ title, height, text }: { title: string; height: number; text: string }) {
  return (
    <figure className={styles.figure}>
      <div className={styles.placeholder} style={{ blockSize: `${height}px` }} role="note">{text}</div>
      <figcaption className={styles.caption}>{title}</figcaption>
    </figure>
  );
}

function PreviewFrame({ title, height, src }: { title: string; height: number; src: string }) {
  const figure = useRef<HTMLElement>(null);
  const [width, setWidth] = useState<'desktop' | 'mobile'>('desktop');
  const [fullscreen, setFullscreen] = useState(false);

  useEffect(() => {
    const onChange = () => { setFullscreen(figure.current !== null && document.fullscreenElement === figure.current); };
    document.addEventListener('fullscreenchange', onChange);
    return () => { document.removeEventListener('fullscreenchange', onChange); };
  }, []);

  return (
    <figure ref={figure} className={styles.figure}>
      <div className={styles.stage} style={fullscreen ? undefined : { blockSize: `${height}px` }}>
        <iframe
          className={styles.iframe}
          style={width === 'mobile' ? { inlineSize: `${PREVIEW_MOBILE_WIDTH_PX}px` } : undefined}
          src={src}
          title={title}
          sandbox={PREVIEW_SANDBOX}
          allow="fullscreen"
          referrerPolicy="no-referrer"
        />
      </div>
      <figcaption className={styles.caption}>
        <span className={styles.title}>{title}</span>
        <span className={styles.controls} role="group" aria-label="Preview width">
          <button type="button" className={styles.control} aria-pressed={width === 'desktop'}
            onClick={() => { setWidth('desktop'); }}>Desktop</button>
          <button type="button" className={styles.control} aria-pressed={width === 'mobile'}
            onClick={() => { setWidth('mobile'); }}>Mobile</button>
          <button type="button" className={styles.control}
            onClick={() => { void figure.current?.requestFullscreen?.().catch(() => undefined); }}>Fullscreen</button>
        </span>
      </figcaption>
    </figure>
  );
}
