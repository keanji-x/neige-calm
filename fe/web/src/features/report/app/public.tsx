// The `app` block — an embedded same-origin page in a sandboxed iframe.
//
// **The one block in the document that keeps a border.** Everywhere else a box
// would be decoration; here it carries meaning — it marks an outsider held in
// a sandbox. Tables and charts are the document's own furniture; this is not.
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
    <div className={styles.frame}>
      <div className={styles.head}>
        <span className={styles.title}>{title}</span>
        <span className={styles.sandboxed}>sandboxed</span>
      </div>
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
    </div>
  );
}
