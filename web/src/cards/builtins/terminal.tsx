import { lazy, Suspense } from 'react';
import { z } from 'zod';
import type { TerminalCardData } from '../../types';
import type { CardEntry } from '../registry';
import { dlog } from '../../util/debug';
import {
  TERMINAL_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';

// xterm.js + the fit addon plus its CSS bring real weight (~150 KB raw).
// Only load the renderer when a terminal card actually goes live; the
// static-`lines` flavor that ships before the kernel patches in a
// `terminal_id` doesn't need any of it.
const XtermView = lazy(() =>
  import('../../XtermView').then((m) => ({ default: m.XtermView })),
);

/**
 * Wire shape for a `kind: "terminal"` card's `payload`. Server-side it's
 * minted by `POST /api/terminals` and contains the kernel `Terminal.id` so
 * the client can attach the live PTY. Empty payload is tolerated — a card
 * created before the terminal spawned still renders (as the static
 * `lines`-only flavor) until the kernel patches `terminal_id` in.
 */
const terminalPayloadSchema = z.object({
  // Tier A: kernel stamps `schemaVersion` on every write; older rows
  // omit it (treated as 1 by `payloadSchemaVersion`). zod tolerates
  // the unknown key by default, so we keep this schema focused on the
  // shape we actually consume.
  terminal_id: z.string().optional(),
});

function TerminalCard({ card }: { card: TerminalCardData }) {
  const { title, lines, terminalId, unsupportedVersion } = card;
  if (unsupportedVersion !== undefined) {
    return (
      <div className="term term-unsupported-version">
        <div className="term-head card-drag-handle">
          <span className="term-title">{title || 'terminal'}</span>
        </div>
        <div className="term-body">
          <div className="term-line k-warn">
            Unsupported card payload version (got {unsupportedVersion}, kernel
            supports {TERMINAL_PAYLOAD_SCHEMA_VERSION}); please refresh.
          </div>
        </div>
      </div>
    );
  }
  const live = !!terminalId;
  dlog('TerminalCard', 'render', { id: card.id, live, terminalId });
  return (
    <div className={'term' + (live ? ' live' : '')}>
      <div className="term-head card-drag-handle">
        <span className="term-dot" />
        <span className="term-dot b" />
        <span className="term-dot c" />
        <span className="term-title">
          {title || 'terminal'}
          {live && <span className="term-live-pip"> · live</span>}
        </span>
      </div>
      <div className="term-body">
        {live ? (
          <Suspense fallback={<div className="term-line k-cursor">Loading terminal…</div>}>
            <XtermView terminalId={terminalId!} />
          </Suspense>
        ) : (
          <>
            {lines.map((l, i) => (
              <div key={i} className={'term-line k-' + l.kind}>
                {l.text}
              </div>
            ))}
            <div className="term-line k-cursor">
              <span className="term-cursor" />
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export const TerminalEntry: CardEntry<TerminalCardData> = {
  type: 'terminal',
  Component: TerminalCard,
  defaultSize: { w: 6, h: 10, minW: 4, minH: 6 },
  fromKernel: (k) => {
    if (k.kind !== 'terminal') return null;
    dlog('TerminalEntry.fromKernel', { id: k.id, payload: k.payload });
    // A `null` payload is legal here — predates the kernel attaching a
    // `terminal_id`. Treat as empty object so the optional field stays
    // undefined; non-object payloads on a `terminal` card are an error.
    const candidate = k.payload ?? {};
    // Tier A schemaVersion check. Missing field is treated as v1 (the
    // only version that exists today). Anything newer than this build
    // knows about: warn + render fallback, but still return a card so
    // the grid layout doesn't collapse around it.
    const version = payloadSchemaVersion(candidate);
    if (version > TERMINAL_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] terminal payload schemaVersion=${version} unsupported (frontend supports ${TERMINAL_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'terminal',
        id: k.id,
        title: 'terminal',
        lines: [],
        unsupportedVersion: version,
      };
    }
    const parsed = terminalPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] terminal payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'terminal',
      id: k.id,
      title: 'terminal',
      lines: [],
      terminalId: parsed.data.terminal_id,
    };
  },
  addPanel: { label: 'New terminal', icon: 'terminal' },
};
