import { lazy, Suspense, useCallback, useEffect } from 'react';
import { z } from 'zod';
import type { TermLine } from '../../types';
import type { CardEntry } from '../registry';
import { dlog } from '../../util/debug';
import { useTheme } from '../../app/theme';
import { useState } from '../../shared/state';
import { CardHead } from '../CardHead';
import { CardStatusDot } from '../../shared/components/CardStatusDot';
import { CardExitBadge } from '../../shared/components/CardExitBadge';
import type { Role } from '../../api/generated-terminal';
import type { ExitChange, XtermViewHandle } from '../../XtermView';
import { createTerminalCard, getTerminalForCard } from '../../api/calm';
import { useCardInstanceCtx } from '../registry';
import {
  TERMINAL_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';

declare module '../../types' {
  interface WaveCardDataMap {
    terminal: TerminalCardData;
  }
}

export interface TerminalCardData {
  type: 'terminal';
  id?: string;
  idempotencyKey?: string;
  title?: string | null;
  lines: TermLine[];
  terminalId?: string;
  unsupportedVersion?: number;
}

// xterm.js + the fit addon plus its CSS bring real weight (~150 KB raw).
// Only load the renderer when a terminal card actually goes live; the
// static-`lines` flavor that ships before the kernel patches in a
// `terminal_id` doesn't need any of it.
const XtermView = lazy(() =>
  import('../../XtermView').then((m) => ({ default: m.XtermView })),
);

function createXtermRefSlot(): { current: XtermViewHandle | null } {
  return { current: null };
}

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
  idempotency_key: z.string().optional(),
});

function TerminalCard({
  card,
  onClose,
}: {
  card: TerminalCardData;
  onClose?: () => void;
}) {
  const { id: cardId, title, lines, terminalId, unsupportedVersion } = card;
  const { resolved: theme } = useTheme();
  const [xtermRefSlot] = useCardInstanceCtx().useCardSlot<{
    current: XtermViewHandle | null;
  }>('xtermRef', createXtermRefSlot);
  const setXtermRef = useCallback(
    (handle: XtermViewHandle | null) => {
      xtermRefSlot.current = handle;
    },
    [xtermRefSlot],
  );
  // Daemon-assigned role lifted out of `<XtermView>` so the head can render
  // an "observing" pill in its status slot when this client doesn't hold
  // write. `null` until handshake completes, and reset to `null` on
  // disconnect/teardown — see XtermView's `onRoleChange` calls.
  const [role, setRole] = useState<Role | null>(null);
  // #306 — exit info for the header badge. Seeded on mount from the
  // terminal row's REST data (so a refreshed page with an already-
  // exited terminal shows the badge immediately, without waiting for
  // the WS attach / close), then overridden by `<XtermView>`'s
  // `onExitChange` whenever the daemon emits a new exit event over
  // the live channel.
  const [exit, setExit] = useState<ExitChange | null>(null);
  useEffect(() => {
    if (!cardId || !terminalId) return;
    let cancelled = false;
    (async () => {
      try {
        const t = await getTerminalForCard(cardId);
        if (cancelled) return;
        // The kernel guarantees both fields are present on the wire (the
        // Terminal struct treats them as required, see model.rs comments).
        // We still nullable-guard `exit_code` because that branch is
        // legitimate semantics ("no numeric code yet"), and seed the
        // badge only when at least one of the two fields says "exited".
        const hasExit =
          t.exit_code !== null && t.exit_code !== undefined;
        const wasSignaled = !!t.signal_killed;
        if (hasExit || wasSignaled) {
          setExit({
            exit_code: hasExit ? (t.exit_code as number) : null,
            signal_killed: wasSignaled,
          });
        }
      } catch {
        // Card may have just been minted (no terminal row yet) or
        // deleted concurrently. Either way, render with no badge —
        // a subsequent `onExitChange` from the live WS will replace
        // this no-op once the daemon hits child-exit.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [cardId, terminalId]);
  if (unsupportedVersion !== undefined) {
    return (
      <div className="term term-unsupported-version">
        <CardHead
          card={card}
          className="card-drag-handle"
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
        card={card}
        className="card-drag-handle"
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
              {/*
                #306 — exit badge sits LEFT of the working dot so its
                anchor doesn't shift when the role pill appears /
                disappears (left-justified anchor at the dot stays put).
                Rendered only when the daemon's reported an exit; an
                still-running terminal carries the working dot alone.
              */}
              {exit && <CardExitBadge exit={exit} />}
              {/*
                Don't show a "Working" dot once we have exit info — the
                badge already says the process is finished. Keeping the
                green dot would read as "still running" and contradict
                the badge.
              */}
              {!exit && <CardStatusDot state="Working" />}
            </>
          ) : undefined
        }
      />
      <div className="term-body">
        {live ? (
          <Suspense fallback={<div className="term-line k-cursor">Loading terminal…</div>}>
            <XtermView
              ref={setXtermRef}
              terminalId={terminalId!}
              theme={theme}
              onRoleChange={setRole}
              onExitChange={setExit}
            />
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

export const TerminalEntry: CardEntry<
  TerminalCardData,
  Record<string, never>
> = {
  type: 'terminal',
  Component: TerminalCard,
  defaultSize: { w: 6, h: 10, minW: 4, minH: 6 },
  refreshBacking: 'none',
  createController({ card }) {
    return {
      onVisibleChange(visible) {
        dlog('TerminalCard', 'visibility', { cardId: card.id, visible });
      },
    };
  },
  wheelTarget(_card, instance) {
    const [xtermRefSlot] = instance.useCardSlot<{
      current: XtermViewHandle | null;
    }>('xtermRef', createXtermRefSlot);
    return { kind: 'xterm', ref: xtermRefSlot };
  },
  claim: { mode: 'exact', kind: 'terminal' },
  title: (card) => card.title || 'terminal',
  accessibleName: (card) => (card.title ? `Terminal: ${card.title}` : 'Terminal'),
  create: {
    mode: 'atomic',
    async submit(waveId, _input, ctx) {
      const card = await createTerminalCard(waveId, { theme: ctx.themeRgb });
      return { cardId: card.id, raw: card };
    },
  },
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
        title: k.title,
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
      idempotencyKey: parsed.data.idempotency_key,
      title: k.title,
      lines: [],
      terminalId: parsed.data.terminal_id,
    };
  },
  addPanel: { label: 'terminal' },
};
