// The `preview` block (#1780): a live dev server, embedded from calm's preview gateway. The block
// names a registration by `key`; `resolve`, injected by `app/router`, says whether that key is
// registered, on which gateway port, and whether the dev server answers.
//
// Unlike the `app` block's quiet frame, this one wears chrome: a window bar (title, a Desktop |
// Mobile toggle, fullscreen) and a size line underneath. The `app` block shows a finished page, and
// any frame around it is decoration; a preview simulates a device, so the frame is information —
// which viewport the page believes it has, and how far it was shrunk to fit the column. The page
// gets the device's true CSS-pixel box (1920 × 1080 or 390 × 844; its media queries see the real
// width) and the stage scales the whole box down; nothing is laid out at a width nobody picked.
//
// The frame is another origin (same host, the gateway port), so `allow-same-origin` lets the dev
// server keep its own storage and HMR socket without reaching this document. The gateway admits
// only the owner's calm session and speaks plain http, so an https page (a tunnel, a proxy) cannot
// load it at all: the block says so instead of drawing a frame the browser will block.

import { useEffect, useLayoutEffect, useRef, type ReactNode } from 'react';

import { useState } from '../../../ui/state/public.ts';

import type { PreviewBlockPayload, PreviewResolution } from '../../../../../core/domain/report.ts';
import styles from './preview.module.css';
import {
  PREVIEW_PRESETS, presetSize, readPreset, viewportScale, writePreset, type PreviewPresetId, type PreviewViewportStore,
} from './viewport.ts';

export type { PreviewViewportStore } from './viewport.ts';

const MIN_HEIGHT = 120;
const MAX_HEIGHT = 2000;
const DEFAULT_HEIGHT = 360;

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

export function ReportPreviewBlock({ payload, resolve, viewports }: {
  payload: PreviewBlockPayload;
  /** The app's read of this track's previews, by key. Absent ⇒ the surface carries no previews and the block says so. */
  resolve?: (key: string) => PreviewResolution;
  /** Where the reader's device choice is remembered, by key. Absent ⇒ the choice lasts this mount only. */
  viewports?: PreviewViewportStore;
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
        src={previewFrameUrl(window.location, resolution.preview.port, payload.path)}
        viewportKey={payload.key} viewports={viewports} />;
  }
}

function nonBlank(value: string | null | undefined): string | null {
  return value != null && value.trim() !== '' ? value : null;
}

/** The window bar: decorative traffic dots, the title, and whatever controls the state allows. */
function PreviewBar({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <figcaption className={styles.bar}>
      <span className={styles.dots} aria-hidden="true"><span /><span /><span /></span>
      <span className={styles.title}>{title}</span>
      {children}
    </figcaption>
  );
}

function PreviewNotice({ title, height, text }: { title: string; height: number; text: string }) {
  return (
    <figure className={styles.figure}>
      <PreviewBar title={title} />
      <div className={styles.placeholder} style={{ blockSize: `${height}px` }} role="note">{text}</div>
    </figure>
  );
}

type StageBox = Readonly<{ width: number; height: number }>;

function PreviewFrame({ title, height, src, viewportKey, viewports }: {
  title: string; height: number; src: string; viewportKey: string; viewports: PreviewViewportStore | undefined;
}) {
  const figure = useRef<HTMLElement>(null);
  const stage = useRef<HTMLDivElement>(null);
  const [preset, setPreset] = useState<PreviewPresetId>(() => readPreset(viewports, viewportKey));
  const [fullscreen, setFullscreen] = useState(false);
  const [box, setBox] = useState<StageBox | null>(null);

  useEffect(() => {
    const onChange = () => { setFullscreen(figure.current !== null && document.fullscreenElement === figure.current); };
    document.addEventListener('fullscreenchange', onChange);
    return () => { document.removeEventListener('fullscreenchange', onChange); };
  }, []);

  // The stage's own box: its width is the column (or the screen, fullscreen), its height counts only
  // in fullscreen, where the stage fills what the bar leaves.
  useLayoutEffect(() => {
    const element = stage.current;
    if (element === null) return;
    const measure = () => {
      const next = { width: element.clientWidth, height: element.clientHeight };
      setBox((previous) => previous?.width === next.width && previous.height === next.height ? previous : next);
    };
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => { observer.disconnect(); };
  }, []);

  const size = presetSize(preset);
  const maxHeight = fullscreen ? box?.height ?? height : height;
  const scale = viewportScale(size, box?.width ?? null, maxHeight);

  return (
    <figure ref={figure} className={styles.figure}>
      <PreviewBar title={title}>
        <span className={styles.controls}>
          <span className={styles.toggle} role="group" aria-label="Device">
            {PREVIEW_PRESETS.map((option) => (
              <button key={option.id} type="button" className={styles.segment} aria-pressed={preset === option.id}
                onClick={() => { writePreset(viewports, viewportKey, option.id); setPreset(option.id); }}>
                {option.label}
              </button>
            ))}
          </span>
          <button type="button" className={styles.control} aria-label="Fullscreen" title="Fullscreen"
            onClick={() => { void figure.current?.requestFullscreen?.().catch(() => undefined); }}>⛶</button>
        </span>
      </PreviewBar>
      <div ref={stage} className={styles.stage}>
        {/* The scaled box, so the column holds exactly what is drawn: no dead space under a shrunk device. */}
        <div className={styles.viewport}
          style={{ inlineSize: `${size.width * scale}px`, blockSize: `${size.height * scale}px` }}>
          <iframe
            className={styles.device}
            style={{ inlineSize: `${size.width}px`, blockSize: `${size.height}px`, transform: `scale(${scale})` }}
            src={src}
            title={title}
            sandbox={PREVIEW_SANDBOX}
            allow="fullscreen"
            referrerPolicy="no-referrer"
          />
        </div>
      </div>
      <p className={styles.size}>{`${size.width} × ${size.height} · ${Math.round(scale * 100)}%`}</p>
    </figure>
  );
}
