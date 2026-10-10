// The `window` block (#2530): a live window that speaks window-stream protocol v1 at `src`. The
// system owns the socket, frames and input; this owns how the block sits in the document. A frame
// is shown as live only while the session that drew it is open: otherwise the canvas is dimmed
// under "unavailable" and takes no input, so an old frame never passes for the window.

import type { WindowBlockPayload } from '../../../../../core/domain/report.ts';
import { useWindowStream, type WindowStreamOptions, type WindowStreamStatus } from '../../../systems/window-stream/public.tsx';
import styles from './window.module.css';

const MIN_HEIGHT = 120;
const MAX_HEIGHT = 2000;
const DEFAULT_HEIGHT = 480;

function clampHeight(height: number | null | undefined): number {
  if (height == null || Number.isNaN(height)) return DEFAULT_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height));
}

const STATUS_TEXT: Readonly<Record<Exclude<WindowStreamStatus, 'live'>, string>> = Object.freeze({
  connecting: 'connecting…',
  unavailable: 'unavailable',
});

export function ReportWindowBlock({ payload, stream }: {
  payload: WindowBlockPayload;
  /** Test seam: the socket the system opens. */
  stream?: WindowStreamOptions;
}) {
  const { canvasRef, status, windowTitle } = useWindowStream(payload.src, stream);
  const title = payload.title != null && payload.title.trim() !== '' ? payload.title : payload.src;
  return (
    <figure className={styles.figure}>
      <div className={styles.stage} data-nc-state={status} style={{ blockSize: `${clampHeight(payload.height)}px` }}>
        <canvas
          ref={canvasRef}
          className={styles.canvas}
          tabIndex={0}
          aria-label={`${title}: live window${status === 'live' ? '' : `, ${STATUS_TEXT[status]}`}`}
        />
        {status !== 'live' && <div className={styles.status}><span className={styles.label} role="status">{STATUS_TEXT[status]}</span></div>}
      </div>
      <figcaption className={styles.caption}>
        {windowTitle !== null && windowTitle !== '' && windowTitle !== title ? `${title} · ${windowTitle}` : title}
      </figcaption>
    </figure>
  );
}
