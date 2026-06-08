import { useEffect, useRef, type CSSProperties } from 'react';
import { z } from 'zod';
import { sharedEventStream } from '../../api/events';
import { resetSpecCard } from '../../api/calm';
import { Icon } from '../../Icon';
import { IconButton } from '../../pages/_shared';
import { useState } from '../../shared/state';
import { CardStatusDot } from '../../shared/components/CardStatusDot';
import { ConfirmDialog } from '../../ui/ConfirmDialog/ConfirmDialog';
import type { FsmState } from '../../types';
import { CardHead } from '../CardHead';
import { useCardStatusOverlay } from '../overlayRegistry';
import {
  useOptionalCardInstanceCtx,
  type CardAction,
  type CardComponentProps,
  type CardEntry,
  type CardInstanceCtx,
} from '../registry';
import {
  payloadSchemaVersion,
  SPEC_PAYLOAD_SCHEMA_VERSION,
} from './schemaVersions';

declare module '../../types' {
  interface WaveCardDataMap {
    spec: SpecCardData;
  }
}

export interface SpecCardData {
  type: 'spec';
  id?: string;
  goal?: string;
  iconBg?: string;
  iconFg?: string;
  unsupportedVersion?: number;
}

type SpecLogoStyle = CSSProperties & {
  '--agent-card-logo-bg'?: string;
  '--agent-card-logo-fg'?: string;
};

const specPayloadSchema = z.object({
  spec_harness: z.literal(true),
  schemaVersion: z.number().int().optional(),
  codex_source: z.string().optional(),
  push_watermark: z.number().optional(),
  prompt: z.string().optional(),
  icon_bg: z.string().optional(),
  icon_fg: z.string().optional(),
});

function SpecAgentLogo({ bg, fg }: { bg?: string; fg?: string }) {
  const style: SpecLogoStyle = {
    '--agent-card-logo-bg': bg ?? 'var(--agent-card-codex-logo-bg)',
    '--agent-card-logo-fg': fg ?? 'var(--agent-card-codex-logo-fg)',
    fontSize: 'var(--text-xs)',
  };
  return (
    <span className="agent-card-logo" style={style} aria-hidden="true">
      S
    </span>
  );
}

function isSpecHarnessPayload(payload: unknown): payload is Record<string, unknown> {
  return (
    payload !== null &&
    typeof payload === 'object' &&
    (payload as Record<string, unknown>).spec_harness === true
  );
}

function toFsmState(state: string | undefined): FsmState {
  switch (state) {
    case 'Starting':
    case 'Idle':
    case 'Working':
    case 'AwaitingInput':
    case 'Errored':
    case 'Done':
      return state;
    case 'starting':
      return 'Starting';
    case 'running':
      return 'Working';
    case 'idle':
      return 'Idle';
    case 'turn_pending':
      return 'AwaitingInput';
    case 'failed':
      return 'Errored';
    case 'exited':
    case 'superseded':
      return 'Done';
    default:
      return 'Starting';
  }
}

function humanizeToken(token: string): string {
  return token
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

function HarnessStateChip({
  state,
  rawState,
  phase,
}: {
  state: FsmState;
  rawState: string;
  phase: string | null;
}) {
  return (
    <span
      style={{
        display: 'inline-flex',
        flexDirection: 'column',
        alignItems: 'flex-end',
        gap: 3,
      }}
    >
      <span
        style={{
          display: 'inline-flex',
          alignItems: 'center',
          gap: 6,
          color: 'var(--text-2)',
          background: 'var(--paper-2)',
          border: '1px solid var(--hairline)',
          borderRadius: 'var(--radius-pill)',
          padding: '3px 8px',
          fontSize: 'var(--text-xs)',
          fontWeight: 600,
          letterSpacing: 0,
          lineHeight: 'var(--leading-none)',
        }}
      >
        <CardStatusDot state={state} title={`Spec ${rawState}`} />
        <span>{humanizeToken(rawState)}</span>
      </span>
      {phase && (
        <span
          style={{
            color: 'var(--text-3)',
            fontSize: 'var(--text-xs)',
            letterSpacing: 0,
            lineHeight: 'var(--leading-none)',
          }}
        >
          {humanizeToken(phase)}
        </span>
      )}
    </span>
  );
}

function GoalBanner({ goal }: { goal?: string }) {
  const text = goal?.trim() || 'No goal captured';
  return (
    <div className="wave-report-summary" aria-label="Spec goal">
      <strong style={{ color: 'var(--text)' }}>Goal:</strong>{' '}
      <span>{text}</span>
    </div>
  );
}

export function ChatTimeline({ cardId: _cardId }: { cardId?: string }) {
  return (
    <div
      className="wave-report-empty"
      data-testid="spec-chat-timeline"
      aria-label="Spec chat timeline"
    >
      <em>No conversation items yet.</em>
    </div>
  );
}

function UnsupportedSpecCard({
  version,
  onClose,
}: {
  version: number;
  onClose?: () => void;
}) {
  return (
    <div className="wave-report-card wave-report-card-unsupported-version">
      <CardHead
        card={{ type: 'spec', goal: '' }}
        className="card-drag-handle"
        title="Spec"
        onClose={onClose}
        closeAriaLabel="Remove panel"
      />
      <div className="wave-report-empty">
        Unsupported card payload version (got {version}, frontend supports{' '}
        {SPEC_PAYLOAD_SCHEMA_VERSION}); please refresh.
      </div>
    </div>
  );
}

export function SpecCard({ card, onClose }: CardComponentProps<SpecCardData>) {
  if (card.unsupportedVersion !== undefined) {
    return (
      <UnsupportedSpecCard
        version={card.unsupportedVersion}
        onClose={onClose}
      />
    );
  }
  return <SpecCardImpl card={card} onClose={onClose} />;
}

function SpecCardImpl({
  card,
  onClose,
}: {
  card: SpecCardData;
  onClose?: () => void;
}) {
  const cardId = card.id;
  const status = useCardStatusOverlay(cardId);
  const rawState = status?.state ?? 'Starting';
  const fsm = toFsmState(rawState);
  const [phase, setPhase] = useState<string | null>(null);
  const instanceCtx = useOptionalCardInstanceCtx();
  const [localResetOpen, setLocalResetOpen] = useState(false);
  const [resetOpen, setResetOpen] =
    instanceCtx?.useCardSlot<boolean>('resetOpen', false) ?? [
      localResetOpen,
      setLocalResetOpen,
    ];
  const resetOpenRef = useRef(resetOpen);
  const [resetPending, setResetPending] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);

  useEffect(() => {
    setPhase(null);
    if (!cardId) return;
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    const off = stream.on((ev) => {
      if (
        ev.ev === 'harness.phase.changed' &&
        ev.data.card_id === cardId
      ) {
        setPhase(ev.data.new_phase);
      }
    });
    return () => {
      off();
    };
  }, [cardId]);

  useEffect(() => {
    resetOpenRef.current = resetOpen;
    if (resetOpen) {
      setResetError(null);
    } else {
      setResetPending(false);
      setResetError(null);
    }
  }, [resetOpen]);

  const onConfirmReset = async () => {
    if (!cardId) return;
    setResetPending(true);
    setResetError(null);
    try {
      await resetSpecCard(cardId);
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
    <div className="wave-report-card">
      <CardHead
        card={card}
        className="card-drag-handle"
        title="Spec"
        icon={<SpecAgentLogo bg={card.iconBg} fg={card.iconFg} />}
        onClose={onClose}
        closeAriaLabel="Remove panel"
        status={<HarnessStateChip state={fsm} rawState={rawState} phase={phase} />}
      />
      <GoalBanner goal={card.goal} />
      <div className="wave-report-body">
        <ChatTimeline cardId={cardId} />
      </div>
      <ConfirmDialog
        open={resetOpen}
        title="Reset spec session?"
        description={
          <>
            <p>
              This kills the current spec daemon and starts a new conversation.
              The wave report and observation history are preserved, but the
              spec conversation transcript will be discarded. This cannot be
              undone.
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

function specResetActions(
  card: SpecCardData,
  ctx: CardInstanceCtx,
): CardAction[] {
  if (!card.id) return [];
  const [, setResetOpen] = ctx.useCardSlot<boolean>('resetOpen', false);
  return [
    {
      kind: 'imperative',
      id: 'reset-spec-session',
      placement: 'head',
      render() {
        return (
          <IconButton
            glyph={<Icon n="reset" s={14} />}
            label="Reset spec session"
            title="Reset spec session (kill daemon, new thread)"
            tone="danger"
            onClick={() => setResetOpen(true)}
          />
        );
      },
    },
  ];
}

export const SpecEntry: CardEntry<SpecCardData, never> = {
  type: 'spec',
  Component: SpecCard,
  defaultSize: { w: 6, h: 12, minW: 4, minH: 8 },
  refreshBacking: 'none',
  title: () => 'Spec',
  accessibleName: (card) =>
    card.goal?.trim() ? `Spec agent: ${card.goal}` : 'Spec agent',
  create: { mode: 'kernel-minted-only' },
  actions: specResetActions,
  fromKernel: (k) => {
    if (k.kind !== 'codex') return null;
    const candidate = k.payload ?? {};
    if (!isSpecHarnessPayload(candidate)) return null;
    const version = payloadSchemaVersion(candidate);
    if (version > SPEC_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] spec payload schemaVersion=${version} unsupported (frontend supports ${SPEC_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'spec',
        id: k.id,
        unsupportedVersion: version,
      };
    }
    const parsed = specPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] spec payload invalid for ${k.id}:`, parsed.error.issues);
      return null;
    }
    return {
      type: 'spec',
      id: k.id,
      goal: parsed.data.prompt,
      iconBg: parsed.data.icon_bg,
      iconFg: parsed.data.icon_fg,
    };
  },
};
