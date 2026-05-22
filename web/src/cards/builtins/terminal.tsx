import { lazy, Suspense } from 'react';
import { z } from 'zod';
import type { TerminalCardData } from '../../types';
import type { CardEntry } from '../registry';
import { dlog } from '../../util/debug';
import { useTheme } from '../../app/theme';
import { useState } from '../../shared/state';
import { CardHead } from '../CardHead';
import { CardStatusDot } from '../../shared/components/CardStatusDot';
import type { Role } from '../../api/generated-terminal';
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

function TerminalCard({
  card,
  onClose,
}: {
  card: TerminalCardData;
  onClose?: () => void;
}) {
  const { title, lines, terminalId, unsupportedVersion } = card;
  const { resolved: theme } = useTheme();
  // Daemon-assigned role lifted out of `<XtermView>` so the head can render
  // an "observing" pill in its status slot when this client doesn't hold
  // write. `null` until handshake completes, and reset to `null` on
  // disconnect/teardown — see XtermView's `onRoleChange` calls.
  const [role, setRole] = useState<Role | null>(null);
  if (unsupportedVersion !== undefined) {
    return (
      <div className="term term-unsupported-version">
        <CardHead
          className="term-head card-drag-handle"
          title={title || 'terminal'}
          onClose={onClose}
          closeAriaLabel="Remove panel"
        />
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
      <CardHead
        className="term-head card-drag-handle"
        title={title || 'terminal'}
        onClose={onClose}
        closeAriaLabel="Remove panel"
        // Status indicator unified with Codex: a `Working` dot when the PTY is
        // attached, nothing when it isn't. The absence-of-dot reads as "not
        // connected yet" without needing a dim placeholder, and matches the
        // dot-only visual language now shared across cards (see Codex below).
        //
        // Observer-only "observing" pill renders before the dot when the
        // daemon assigned this client read-only access. Owners (the common
        // single-user case) get no pill — just the dot — keeping the head
        // calm by default. Dot stays rightmost so its anchor position is
        // unchanged across roles.
        status={
          live ? (
            <>
              {role === 'Observer' && (
                <span className="card-head-observing-pill">observing</span>
              )}
              <CardStatusDot state="Working" />
            </>
          ) : undefined
        }
      />
      <div className="term-body">
        {live ? (
          <Suspense fallback={<div className="term-line k-cursor">Loading terminal…</div>}>
            <XtermView terminalId={terminalId!} theme={theme} onRoleChange={setRole} />
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
