// The `app` block — an embedded same-origin page in a sandboxed iframe.
//
// **The frame is quiet, and the caption does the talking.** It used to carry a
// mono title bar and an uppercase `SANDBOXED` — chrome vocabulary (§2.2) on
// top of content that already brings its own stylesheet, its own type stack
// and its own colours. Two foreign voices stacked on each other is how a
// browser renders an advertisement, and that is exactly what it read as.
//
// So it takes the same tint the task block and the code block take — the
// document's vocabulary for material it did not write — plus one hairline,
// which is the only block here that earns a boundary: the frame marks an
// outsider held in a sandbox. Its name goes underneath, in the same caption
// register as the chart's.
//
// The origin is checked twice: `appBlockPayloadSchema` refuses anything that
// is not a same-origin absolute path, and this resolves the URL the way the
// browser will and asserts the origin again. A regex and a resolver disagree
// about exactly the inputs an attacker would look for, so both run.
//
// Not implemented here, deliberately: the MCP `AppBridge` handshake the legacy
// card host performs (`ui/initialize` + theme pushes). It costs two packages
// and only matters for MCP-authored pages; a plain page never completes the
// handshake and renders identically without it. When an MCP app block has a
// writer, this is where the bridge attaches.

import type { AppBlockPayload } from '../../../../../core/domain/report.ts';
import styles from './app.module.css';

const MIN_HEIGHT = 120;
const MAX_HEIGHT = 2000;
const DEFAULT_HEIGHT = 360;

function clampHeight(height: number | undefined): number {
  if (height === undefined || Number.isNaN(height)) return DEFAULT_HEIGHT;
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
  const title = payload.title !== undefined && payload.title.trim() !== '' ? payload.title : payload.src;

  if (!isSameOrigin(payload.src)) {
    return (
      <div className={styles.refused} role="note">
        embedded app refused: not a same-origin path
      </div>
    );
  }

  return (
    <figure className={styles.figure}>
      {/*
        `allow-scripts` without `allow-same-origin`: the frame runs, and it runs
        in an opaque origin, so it cannot reach this document, its storage or
        its session. Granting both together would be the same as not sandboxing
        at all — the frame could simply remove the attribute.
      */}
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
