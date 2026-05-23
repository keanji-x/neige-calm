// Codex (OpenAI) agent card — interactive TUI variant.
//
// Architecture:
//   - The backend spawns the codex CLI under our `calm-session-daemon`
//     PTY infrastructure (same path Terminal cards use). Its TUI renders
//     into the embedded xterm.js view below.
//   - Hook events (PreToolUse / PostToolUse / Stop / ...) stream over the
//     WS event bus on `card:<card_id>` as `codex.hook` envelopes. We use
//     them for the human-readable status label ("PreToolUse: Bash — ls").
//   - The per-card FSM state (Starting / Idle / Working / AwaitingInput /
//     Errored / Done) is owned by the **kernel** `card_fsm` task: it
//     watches the same hook stream, runs a debounced 6-state FSM, and
//     publishes the result as `Overlay { entity_kind:"card", kind:"status",
//     payload:{state} }`. The card subscribes to overlay.set on its own
//     topic and renders the dot from that — there is intentionally no
//     local FSM here, so wave-union (the kernel computes it server-side)
//     and per-card dot agree by construction.

import { lazy, Suspense, useEffect } from 'react';
import { useState } from '../../shared/state';
import { z } from 'zod';
import type { CodexCardData, FsmState } from '../../types';
import { sharedEventStream } from '../../api/events';
import { CardStatusDot } from '../../shared/components/CardStatusDot';
import { useTheme } from '../../app/theme';
import { CardHead } from '../CardHead';
import type { CardEntry } from '../registry';
import {
  CODEX_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';

// Lazy-load xterm.js + addon — same pattern as the Terminal card so the
// two cards share a single code-split chunk for the renderer.
const XtermView = lazy(() =>
  import('../../XtermView').then((m) => ({ default: m.XtermView })),
);

const codexPayloadSchema = z.object({
  terminal_id: z.string().optional(),
  // Hands-free seed prompt (#110 superseded). When set server-side, codex
  // boots with the composer pre-filled and the kernel injects `\r` once
  // `session_start` fires; the card itself doesn't read this field, but we
  // keep it in the schema so payload validation passes.
  prompt: z.string().optional(),
  model: z.string().optional(),
  cwd: z.string().optional(),
});

function UnsupportedCodexCard({ version }: { version: number }) {
  return (
    <div className="codex-card codex-card-unsupported-version">
      <CardHead className="codex-card-head card-drag-handle" title="Codex" />
      <div className="codex-card-pty">
        <div className="codex-card-empty">
          Unsupported card payload version (got {version}, kernel supports{' '}
          {CODEX_PAYLOAD_SCHEMA_VERSION}); please refresh.
        </div>
      </div>
    </div>
  );
}

function CodexCard({ card }: { card: CodexCardData }) {
  // Early bail-out for unsupported versions. Split into its own component
  // so React's rules-of-hooks stay satisfied — the hook calls below only
  // run on the supported path.
  if (card.unsupportedVersion !== undefined) {
    return <UnsupportedCodexCard version={card.unsupportedVersion} />;
  }
  return <CodexCardImpl card={card} />;
}

function CodexCardImpl({ card }: { card: CodexCardData }) {
  const cardId = card.id;
  const { resolved: theme } = useTheme();
  // eslint-disable-next-line no-console
  console.warn('[#177 CodexCardImpl render]', { theme, cardId: card.id });
  // FSM state owned by the kernel `card_fsm` task. Defaults to "Starting"
  // until the first overlay.set lands (the kernel writes one on the
  // session_start hook, so this placeholder is usually visible for a few
  // hundred ms at most).
  const [fsm, setFsm] = useState<FsmState>('Starting');
  // Human-readable "what is codex doing right now" label, derived from the
  // most recent codex.hook event. Independent of the FSM state because the
  // label is a string and the FSM is a closed enum.
  const [label, setLabel] = useState<string>('starting…');

  useEffect(() => {
    if (!cardId) return;
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    // Second arg is the envelope `EventMeta` ({id, eventVersion}); codex
    // only cares about the payload, so we ignore it.
    const off = stream.on((ev) => {
      if (ev.ev === 'codex.hook' && ev.data.card_id === cardId) {
        const payload = (ev.data.payload ?? {}) as Record<string, unknown>;
        const eventName = String(payload.hook_event_name ?? 'unknown');
        const toolName =
          typeof payload.tool_name === 'string' ? payload.tool_name : '';
        setLabel(summarizeHook(payload, eventName, toolName));
        return;
      }
      if (ev.ev === 'overlay.set') {
        const o = ev.data;
        if (
          o.entity_kind === 'card' &&
          o.entity_id === cardId &&
          o.kind === 'status'
        ) {
          const p = o.payload as Record<string, unknown> | null;
          const s = p && typeof p.state === 'string' ? p.state : null;
          if (s && isFsmState(s)) setFsm(s);
        }
      }
    });
    return () => {
      off();
      // Topic intentionally NOT removed — see XtermView's terminal flow
      // for the same reasoning (sticky subscriptions on the shared stream).
    };
  }, [cardId]);

  return (
    <div className="codex-card">
      <CardHead
        className="codex-card-head card-drag-handle"
        title="Codex"
        status={
          <div className="codex-status-bar" aria-live="polite">
            <span className="codex-status-label" title={`${fsm} — ${label}`}>
              {fsm}: {label}
            </span>
            <CardStatusDot state={fsm} title={`${fsm} — ${label}`} />
          </div>
        }
      />
      <div className="codex-card-pty">
        {card.terminalId ? (
          <Suspense fallback={<div className="codex-card-empty">Loading terminal…</div>}>
            <XtermView terminalId={card.terminalId} theme={theme} />
          </Suspense>
        ) : (
          <div className="codex-card-empty">
            Codex is starting… waiting for PTY.
          </div>
        )}
      </div>
    </div>
  );
}

function isFsmState(s: string): s is FsmState {
  return (
    s === 'Starting' ||
    s === 'Idle' ||
    s === 'Working' ||
    s === 'AwaitingInput' ||
    s === 'Errored' ||
    s === 'Done'
  );
}

function summarizeHook(
  payload: Record<string, unknown>,
  eventName: string,
  toolName: string,
): string {
  // Cheap one-liner. Same shape as the prior list-row summary so the
  // language stays consistent. `eventName` always leads so the user has a
  // discriminator even when tool_name is absent.
  if (toolName) {
    const input = payload.tool_input;
    if (input && typeof input === 'object') {
      const cmd = (input as Record<string, unknown>).command;
      if (typeof cmd === 'string') {
        return `${eventName}: ${toolName} — ${truncate(cmd, 60)}`;
      }
      const path = (input as Record<string, unknown>).path;
      if (typeof path === 'string') {
        return `${eventName}: ${toolName} — ${path}`;
      }
    }
    return `${eventName}: ${toolName}`;
  }
  const prompt = payload.user_prompt;
  if (typeof prompt === 'string') {
    return `${eventName}: ${truncate(prompt, 60)}`;
  }
  return eventName;
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
}

export const CodexEntry: CardEntry<CodexCardData> = {
  type: 'codex',
  Component: CodexCard,
  defaultSize: { w: 6, h: 12, minW: 4, minH: 8 },
  fromKernel: (k) => {
    if (k.kind !== 'codex') return null;
    const candidate = k.payload ?? {};
    // Tier A schemaVersion check; see TerminalEntry for the full rationale.
    const version = payloadSchemaVersion(candidate);
    if (version > CODEX_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] codex payload schemaVersion=${version} unsupported (frontend supports ${CODEX_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'codex',
        id: k.id,
        unsupportedVersion: version,
      };
    }
    const parsed = codexPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] codex payload invalid for ${k.id}:`, parsed.error.issues);
      return null;
    }
    return {
      type: 'codex',
      id: k.id,
      terminalId: parsed.data.terminal_id,
      cwd: parsed.data.cwd,
    };
  },
  addPanel: {
    label: 'New codex',
    icon: 'spark',
    createSchema: {
      // Interactive codex handles permission / model selection inside its
      // own slash-command UX, so the schema-form is now just cwd.
      fields: [
        {
          key: 'cwd',
          label: 'Working directory',
          type: 'directory',
        },
      ],
    },
  },
};
