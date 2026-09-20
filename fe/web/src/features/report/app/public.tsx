// The `app` block — an embedded same-origin page in a sandboxed iframe. The origin is checked twice:
// the payload schema refuses anything but a same-origin absolute path, and this resolves the URL the
// way the browser will and asserts the origin again.

import type { AppBlockPayload } from '../../../../../core/domain/report.ts';
import styles from './app.module.css';

const MIN_HEIGHT = 120;
const MAX_HEIGHT = 2000;
const DEFAULT_HEIGHT = 360;

function clampHeight(height: number | null | undefined): number {
  if (height == null || Number.isNaN(height)) return DEFAULT_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height));
}

function isSameOrigin(src: string): boolean {
  try {
    return new URL(src, window.location.origin).origin === window.location.origin;
  } catch {
    return false;
  }
}

export function ReportAppBlock({ payload }: { payload: AppBlockPayload }) {
  const title = payload.title != null && payload.title.trim() !== '' ? payload.title : payload.src;

  if (!isSameOrigin(payload.src)) {
    return (
      <div className={styles.refused} role="note">
        embedded app refused: not a same-origin path
      </div>
    );
  }

  return (
    <figure className={styles.figure}>
      {/* `allow-scripts` without `allow-same-origin`: granting both would let the frame remove the sandbox attribute. */}
      <iframe
        className={styles.iframe}
        src={payload.src}
        title={title}
        style={{ blockSize: `${clampHeight(payload.height)}px` }}
        sandbox="allow-scripts allow-forms allow-popups"
        loading="lazy"
        referrerPolicy="no-referrer"
      />
      <figcaption className={styles.caption}>{title}</figcaption>
    </figure>
  );
}
