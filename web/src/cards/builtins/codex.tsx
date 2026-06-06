// Codex (OpenAI) agent card — interactive TUI variant.
//
// Architecture:
//   - The backend spawns the codex CLI under the terminal renderer
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

import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  type CSSProperties,
} from 'react';
import { useState } from '../../shared/state';
import { z } from 'zod';
import type { FsmState } from '../../types';
import type { Role } from '../../api/generated-terminal';
import type { ExitChange, XtermViewHandle } from '../../XtermView';
import { sharedEventStream } from '../../api/events';
import { CardStatusDot } from '../../shared/components/CardStatusDot';
import { CardExitBadge } from '../../shared/components/CardExitBadge';
import { getTerminalForCard, resetSpecCard } from '../../api/calm';
import { useTheme } from '../../app/theme';
import { Icon } from '../../Icon';
import { IconButton } from '../../pages/_shared';
import { ConfirmDialog } from '../../ui/ConfirmDialog/ConfirmDialog';
import { CardHead } from '../CardHead';
import { useCardInstanceCtx, type CardEntry } from '../registry';
import {
  CLAUDE_PAYLOAD_SCHEMA_VERSION,
  CODEX_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';

declare module '../../types' {
  interface WaveCardDataMap {
    codex: CodexCardData;
    claude: ClaudeCardData;
  }
}

export interface CodexCardData {
  type: 'codex';
  id?: string;
  terminalId?: string;
  cwd?: string;
  iconBg?: string;
  iconFg?: string;
  unsupportedVersion?: number;
}

export interface ClaudeCardData {
  type: 'claude';
  id?: string;
  terminalId?: string;
  cwd?: string;
  iconBg?: string;
  iconFg?: string;
  claudeSessionId?: string;
  unsupportedVersion?: number;
}

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
  icon_bg: z.string().optional(),
  icon_fg: z.string().optional(),
});

const claudePayloadSchema = z.object({
  terminal_id: z.string().optional(),
  prompt: z.string().optional(),
  cwd: z.string().optional(),
  icon_bg: z.string().optional(),
  icon_fg: z.string().optional(),
  settings_path: z.string().optional(),
  claude_session_id: z.string().optional(),
});

type AgentProvider = 'codex' | 'claude';

type AgentCardLogoStyle = CSSProperties & {
  '--agent-card-logo-bg'?: string;
  '--agent-card-logo-fg'?: string;
};

function AgentCardLogo({
  provider,
  bg,
  fg,
}: {
  provider: AgentProvider;
  bg?: string;
  fg?: string;
}) {
  const style: AgentCardLogoStyle = {};
  if (bg) style['--agent-card-logo-bg'] = bg;
  if (fg) style['--agent-card-logo-fg'] = fg;
  return (
    <span
      className={`agent-card-logo agent-card-logo--${provider}`}
      style={style}
      aria-hidden="true"
    >
      {provider === 'claude' ? 'C' : 'GPT'}
    </span>
  );
}

function UnsupportedCodexCard({
  title,
  version,
  onClose,
}: {
  title: string;
  version: number;
  onClose?: () => void;
}) {
  return (
    <div className="codex-card codex-card-unsupported-version">
      <CardHead
        card={{ type: 'codex' }}
        className="card-drag-handle"
        title={title}
        onClose={onClose}
        closeAriaLabel="Remove panel"
      />
      <div className="codex-card-pty">
        <div className="codex-card-empty">
          Unsupported card payload version (got {version}, kernel supports{' '}
          {CODEX_PAYLOAD_SCHEMA_VERSION}); please refresh.
        </div>
      </div>
    </div>
  );
}

function CodexCard({
  card,
  onClose,
  deletable,
}: {
  card: CodexCardData | ClaudeCardData;
  onClose?: () => void;
  deletable?: boolean;
}) {
  // Early bail-out for unsupported versions. Split into its own component
  // so React's rules-of-hooks stay satisfied — the hook calls below only
  // run on the supported path.
  if (card.unsupportedVersion !== undefined) {
    return (
      <UnsupportedCodexCard
        title={card.type === 'claude' ? 'Claude' : 'Codex'}
        version={card.unsupportedVersion}
        onClose={onClose}
      />
    );
  }
  return (
    <CodexCardImpl
      card={card}
      onClose={onClose}
      deletable={deletable}
    />
  );
}

function CodexCardImpl({
  card,
  onClose,
  deletable,
}: {
  card: CodexCardData | ClaudeCardData;
  onClose?: () => void;
  deletable?: boolean;
}) {
  const cardId = card.id;
  const provider = card.type;
  const title = provider === 'claude' ? 'Claude' : 'Codex';
  const { resolved: theme } = useTheme();
  // FSM state owned by the kernel `card_fsm` task. Defaults to "Starting"
  // until the first overlay.set lands (the kernel writes one on the
  // session_start hook, so this placeholder is usually visible for a few
  // hundred ms at most).
  const [fsm, setFsm] = useState<FsmState>('Starting');
  // Human-readable "what is codex doing right now" label, derived from the
  // most recent codex.hook event. Independent of the FSM state because the
  // label is a string and the FSM is a closed enum.
  const [label, setLabel] = useState<string>('starting…');
  // Daemon-assigned role from the embedded `<XtermView>` handshake. Owners
  // (the common single-user case) render no badge; Observers get a small
  // "observing" pill in the head status slot. Cleared on disconnect — the
  // XtermView callback re-emits on every state transition.
  const [role, setRole] = useState<Role | null>(null);
  const ctx = useCardInstanceCtx();
  const [xtermRefSlot] = ctx.useInstance<{ current: XtermViewHandle | null }>(
    'xtermRef',
    { current: null },
  );
  const setXtermRef = useCallback(
    (handle: XtermViewHandle | null) => {
      xtermRefSlot.current = handle;
    },
    [xtermRefSlot],
  );
  // `card.id` is typed `string | undefined` in the kernel wire model, but a
  // mounted card always has one — gate the Reset button on its presence so
  // TS knows the API call site is safe, matching the `if (!cardId) return`
  // pattern the rest of this component already uses (see line ~200).
  const canResetSpecSession =
    provider === 'codex' && deletable === false && !!cardId;
  const [resetOpen, setResetOpen] = useState(false);
  const resetOpenRef = useRef(false);
  const [resetPending, setResetPending] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);
  useEffect(() => {
    resetOpenRef.current = resetOpen;
    if (!resetOpen) {
      setResetPending(false);
      setResetError(null);
    }
  }, [resetOpen]);
  // #306 — exit info for the header badge. Codex cards arguably need this
  // MORE than terminal cards (codex shouldn't ever exit cleanly during a
  // session; an unexpected exit is the kind of thing the user needs to
  // see at a glance). Mirrors the wiring in `terminal.tsx`: seeded on
  // mount from the terminal row's REST data so a refreshed page on an
  // already-exited codex shows the badge immediately, then overridden by
  // `<XtermView>`'s `onExitChange` whenever the daemon emits a new exit
  // event over the live channel.
  const [exit, setExit] = useState<ExitChange | null>(null);
  const terminalId = card.terminalId;
  useEffect(() => {
    if (!cardId || !terminalId) return;
    let cancelled = false;
    (async () => {
      try {
        const t = await getTerminalForCard(cardId);
        if (cancelled) return;
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
        // deleted concurrently. The live `onExitChange` from the WS
        // will replace this no-op once the daemon hits child-exit.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [cardId, terminalId]);

  useEffect(() => {
    if (!cardId) return;
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    // Second arg is the envelope `EventMeta` ({id, eventVersion}); codex
    // only cares about the payload, so we ignore it.
    const off = stream.on((ev) => {
      let hookPayload: unknown = null;
      if (provider === 'codex' && ev.ev === 'codex.hook' && ev.data.card_id === cardId) {
        hookPayload = ev.data.payload;
      } else if (
        provider === 'claude' &&
        ev.ev === 'claude.hook' &&
        ev.data.card_id === cardId
      ) {
        hookPayload = ev.data.payload;
      }
      if (hookPayload !== null) {
        const payload = (hookPayload ?? {}) as Record<string, unknown>;
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
  }, [cardId, provider]);

  const onConfirmReset = async () => {
    if (!cardId) return; // gated by canResetSpecSession above; defensive
    setResetPending(true);
    setResetError(null);
    try {
      await resetSpecCard(cardId);
      ctx.emit({ type: 'refresh' });
      setResetOpen(false);
    } catch (err) {
      if (resetOpenRef.current) {
        setResetError(err instanceof Error ? err.message : 'Reset failed');
      }
    } finally {
      setResetPending(false);
    }
  };

  return (
    <div className="codex-card">
      <CardHead
        card={card}
        className="card-drag-handle"
        title={title}
        icon={<AgentCardLogo provider={provider} bg={card.iconBg} fg={card.iconFg} />}
        onClose={onClose}
        closeAriaLabel="Remove panel"
        // Dot-only status (unified with Terminal). The visible label is gone;
        // the FSM state name + most recent hook summary live entirely in the
        // dot's `title` tooltip + `aria-label`. The bare `<span aria-live>`
        // wrapper keeps screen readers announcing state transitions — the
        // dot itself can't carry aria-live because its className flips
        // between `live-dot` and `undefined` on Working/Starting toggles,
        // which is the kind of churn that confuses some AT.
        status={
          <>
            {canResetSpecSession && (
              <>
                <IconButton
                  glyph={<Icon n="refresh" s={14} />}
                  label="Refresh terminal"
                  title="Refresh terminal (reconnect)"
                  tone="neutral"
                  onClick={() => ctx.emit({ type: 'refresh' })}
                />
                <IconButton
                  glyph={<Icon n="reset" s={14} />}
                  label="Reset spec session"
                  title="Reset spec session (kill daemon, new thread)"
                  tone="danger"
                  onClick={() => {
                    setResetError(null);
                    setResetOpen(true);
                  }}
                />
              </>
            )}
            {role === 'Observer' && (
              <span className="card-head-observing-pill">observing</span>
            )}
            {/*
              #306 — exit badge sits LEFT of the FSM dot so the dot's
              anchor stays put when the role pill / badge come and go.
              Mirrors terminal.tsx. Codex cards arguably need the badge
              MORE than terminal cards — a codex PTY that exits is
              always news the user should see (vs. a terminal `exit 0`
              which is the expected end-of-session). We still render
              the FSM dot beside it: the kernel's `card_fsm` task will
              push a `Done` overlay on its own debounce schedule, but
              the badge is the more direct signal in the brief window
              before that lands.
            */}
            {exit && <CardExitBadge exit={exit} />}
            <span aria-live="polite">
              <CardStatusDot state={fsm} title={`${fsm} — ${label}`} />
            </span>
          </>
        }
      />
      <div className="codex-card-pty">
        {terminalId ? (
          <Suspense fallback={<div className="codex-card-empty">Loading terminal…</div>}>
            <XtermView
              ref={setXtermRef}
              terminalId={terminalId}
              theme={theme}
              onRoleChange={setRole}
              onExitChange={setExit}
            />
          </Suspense>
        ) : (
          <div className="codex-card-empty">
            {title} is starting… waiting for PTY.
          </div>
        )}
      </div>
      <ConfirmDialog
        open={resetOpen}
        title="Reset spec session?"
        description={
          <>
            <p>
              This kills the current codex daemon and starts a new conversation. The wave&apos;s
              report and observation history are preserved, but the codex conversation transcript
              will be discarded. This cannot be undone.
            </p>
            {resetError && (
              <p role="alert" style={{ color: 'var(--warn)', marginTop: 8 }}>
                {resetError}
              </p>
            )}
          </>
        }
        confirmLabel="Reset session"
        cancelLabel="Cancel"
        destructive
        confirmDisabled={resetPending}
        onConfirm={onConfirmReset}
        onCancel={() => {
          setResetOpen(false);
          setResetError(null);
        }}
      />
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

function xtermRefSlotFor(
  instance: Parameters<NonNullable<CardEntry<CodexCardData>['wheelTarget']>>[1],
) {
  return instance.useInstance<{ current: XtermViewHandle | null }>('xtermRef', {
    current: null,
  })[0];
}

export const CodexEntry: CardEntry<CodexCardData> = {
  type: 'codex',
  Component: CodexCard,
  defaultSize: { w: 6, h: 12, minW: 4, minH: 8 },
  refreshBacking: 'controller',
  createController(ctx) {
    const xtermRefSlot = xtermRefSlotFor(ctx.instance);
    return {
      onRefresh() {
        xtermRefSlot.current?.refresh();
      },
    };
  },
  wheelTarget(_card, instance) {
    return { kind: 'xterm', ref: xtermRefSlotFor(instance) };
  },
  claim: { mode: 'exact', kind: 'codex' },
  title: () => 'Codex',
  accessibleName: () => 'Codex',
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
      iconBg: parsed.data.icon_bg,
      iconFg: parsed.data.icon_fg,
    };
  },
  addPanel: {
    label: 'codex',
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

export const ClaudeEntry: CardEntry<ClaudeCardData> = {
  type: 'claude',
  Component: CodexCard,
  defaultSize: { w: 6, h: 12, minW: 4, minH: 8 },
  refreshBacking: 'none',
  wheelTarget(_card, instance) {
    return { kind: 'xterm', ref: xtermRefSlotFor(instance) };
  },
  claim: { mode: 'exact', kind: 'claude' },
  title: () => 'Claude',
  accessibleName: () => 'Claude',
  fromKernel: (k) => {
    if (k.kind !== 'claude') return null;
    const candidate = k.payload ?? {};
    const version = payloadSchemaVersion(candidate);
    if (version > CLAUDE_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] claude payload schemaVersion=${version} unsupported (frontend supports ${CLAUDE_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'claude',
        id: k.id,
        unsupportedVersion: version,
      };
    }
    const parsed = claudePayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] claude payload invalid for ${k.id}:`, parsed.error.issues);
      return null;
    }
    return {
      type: 'claude',
      id: k.id,
      terminalId: parsed.data.terminal_id,
      cwd: parsed.data.cwd,
      iconBg: parsed.data.icon_bg,
      iconFg: parsed.data.icon_fg,
      claudeSessionId: parsed.data.claude_session_id,
    };
  },
  addPanel: {
    label: 'claude',
    createSchema: {
      fields: [
        {
          key: 'cwd',
          label: 'Working directory',
          type: 'directory',
        },
        {
          key: 'prompt',
          label: 'Prompt',
          type: 'textarea',
        },
      ],
    },
  },
};
