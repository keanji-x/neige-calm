import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  type CSSProperties,
  type ReactNode,
} from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { z } from 'zod';
import type { components, operations } from '../../api/generated';
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

type HarnessItem = components['schemas']['HarnessItem'];
type HarnessItemsQuery = operations['get_harness_items']['parameters']['query'];
type JsonRecord = Record<string, unknown>;

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

function isRecord(value: unknown): value is JsonRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function stringField(record: JsonRecord | null, key: string): string | null {
  const value = record?.[key];
  return typeof value === 'string' ? value : null;
}

function textFromContent(value: unknown): string | null {
  if (typeof value === 'string') return value;
  if (!Array.isArray(value)) return null;
  const parts = value
    .map((part) => {
      if (typeof part === 'string') return part;
      if (!isRecord(part)) return null;
      const text = stringField(part, 'text');
      if (text !== null) return text;
      const content = part.content;
      return typeof content === 'string' ? content : null;
    })
    .filter((part): part is string => part !== null && part.length > 0);
  return parts.length > 0 ? parts.join('\n') : null;
}

function formatUnknown(value: unknown): string {
  if (typeof value === 'string') return value;
  if (value === undefined || value === null) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function parseHarnessParams(raw: string): {
  item: JsonRecord | null;
  parseError: string | null;
} {
  try {
    const parsed: unknown = JSON.parse(raw);
    const envelope = isRecord(parsed) ? parsed : null;
    const item = envelope && isRecord(envelope.item) ? envelope.item : null;
    return { item, parseError: null };
  } catch (err) {
    return {
      item: null,
      parseError: err instanceof Error ? err.message : 'Invalid JSON',
    };
  }
}

function itemText(item: JsonRecord | null): string | null {
  if (!item) return null;
  return (
    stringField(item, 'text') ??
    textFromContent(item.content) ??
    textFromContent(item.message)
  );
}

function itemOutput(item: JsonRecord | null): string {
  if (!item) return '';
  return (
    stringField(item, 'output') ??
    stringField(item, 'text') ??
    textFromContent(item.content) ??
    formatUnknown(item.output ?? item.content ?? item)
  );
}

function itemQuery(item: JsonRecord | null): string {
  if (!item) return '';
  const action = isRecord(item.action) ? item.action : null;
  return (
    stringField(item, 'query') ??
    stringField(action, 'query') ??
    stringField(item, 'text') ??
    ''
  );
}

function itemCommand(item: JsonRecord | null): string {
  if (!item) return '';
  const action = isRecord(item.action) ? item.action : null;
  return (
    stringField(item, 'command') ??
    stringField(item, 'cmd') ??
    stringField(action, 'command') ??
    stringField(action, 'cmd') ??
    ''
  );
}

function functionName(item: JsonRecord | null): string {
  if (!item) return 'tool';
  const fn = isRecord(item.function) ? item.function : null;
  return stringField(item, 'name') ?? stringField(fn, 'name') ?? 'tool';
}

function functionArgs(item: JsonRecord | null): string {
  if (!item) return '';
  const fn = isRecord(item.function) ? item.function : null;
  return formatUnknown(item.arguments ?? item.args ?? fn?.arguments ?? fn?.args);
}

function normalizedItemType(itemType: string | null | undefined): string {
  switch (itemType) {
    case 'agentMessage':
      return 'agent_message';
    case 'userMessage':
      return 'user_message';
    case 'functionCall':
      return 'function_call';
    case 'functionCallOutput':
      return 'function_call_output';
    case 'webSearch':
      return 'web_search';
    case 'localShell':
      return 'local_shell';
    default:
      return itemType ?? 'unknown';
  }
}

function isAtBottom(node: HTMLDivElement | null): boolean {
  if (!node) return true;
  return node.scrollHeight - node.scrollTop - node.clientHeight <= 32;
}

function mergeHarnessRows(
  prev: Map<number, HarnessItem>,
  rows: HarnessItem[],
): { map: Map<number, HarnessItem>; changed: boolean } {
  if (rows.length === 0) return { map: prev, changed: false };
  let next: Map<number, HarnessItem> | null = null;

  const ensureNext = () => {
    if (!next) next = new Map(prev);
    return next;
  };

  for (const row of rows.slice().sort((a, b) => a.id - b.id)) {
    const current = next ?? prev;
    if (
      row.method === 'item/started' &&
      row.item_uuid &&
      Array.from(current.values()).some(
        (existing) =>
          existing.item_uuid === row.item_uuid &&
          existing.method === 'item/completed',
      )
    ) {
      continue;
    }
    const target = ensureNext();
    if (row.method === 'item/completed' && row.item_uuid) {
      for (const [id, existing] of target) {
        if (
          id !== row.id &&
          existing.item_uuid === row.item_uuid &&
          existing.method === 'item/started' &&
          existing.id < row.id
        ) {
          target.delete(id);
        }
      }
    }
    target.set(row.id, row);
  }

  return { map: next ?? prev, changed: next !== null };
}

async function fetchHarnessItems(
  cardId: string,
  query: HarnessItemsQuery,
  signal?: AbortSignal,
): Promise<HarnessItem[]> {
  const qs = new URLSearchParams();
  if (query?.after_id !== undefined && query.after_id !== null) {
    qs.set('after_id', String(query.after_id));
  }
  if (query?.limit !== undefined && query.limit !== null) {
    qs.set('limit', String(query.limit));
  }
  if (query?.direction !== undefined && query.direction !== null) {
    qs.set('direction', query.direction);
  }
  const suffix = qs.toString();
  const res = await fetch(
    `/api/cards/${encodeURIComponent(cardId)}/harness/items${
      suffix ? `?${suffix}` : ''
    }`,
    { credentials: 'include', signal },
  );
  if (!res.ok) {
    throw new Error(res.statusText || `HTTP ${res.status}`);
  }
  return (await res.json()) as HarnessItem[];
}

function TimelinePre({ children }: { children: string }) {
  return (
    <pre
      style={{
        margin: 0,
        padding: 'var(--space-3)',
        borderRadius: 'var(--radius-sm)',
        background: 'var(--surface-chip)',
        color: 'var(--text-1)',
        fontFamily: 'var(--font-code)',
        fontSize: 'var(--text-sm)',
        lineHeight: 'var(--leading-loose)',
        whiteSpace: 'pre-wrap',
        wordBreak: 'break-word',
        overflow: 'auto',
      }}
    >
      {children}
    </pre>
  );
}

function SpecMarkdown({ children }: { children: string }) {
  return (
    <div
      className="wave-report-section-body"
      style={{
        color: 'var(--text-1)',
        overflowWrap: 'anywhere',
      }}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown>
    </div>
  );
}

function TimelineBubble({
  children,
  tone = 'default',
  attribution,
}: {
  children: ReactNode;
  tone?: 'default' | 'muted' | 'user';
  attribution?: string;
}) {
  const isMuted = tone === 'muted';
  const isUser = tone === 'user';
  return (
    <div
      style={{
        maxWidth: '100%',
        padding: 'var(--space-3) var(--space-4)',
        borderRadius: 'var(--radius-md)',
        border: `1px solid ${isUser ? 'var(--hairline-strong)' : 'var(--hairline)'}`,
        background: isMuted || isUser ? 'var(--paper-2)' : 'var(--paper)',
        color: isMuted ? 'var(--text-3)' : 'var(--text-1)',
        fontSize: 'var(--text-sm)',
        lineHeight: 'var(--leading-loose)',
      }}
    >
      {attribution ? (
        <div
          style={{
            marginBottom: 'var(--space-1)',
            color: 'var(--text-3)',
            fontSize: 'var(--text-xs)',
            fontWeight: 700,
            letterSpacing: 0,
            textTransform: 'uppercase',
          }}
        >
          {attribution}
        </div>
      ) : null}
      {children}
    </div>
  );
}

function HarnessItemView({ row }: { row: HarnessItem }) {
  const { item, parseError } = parseHarnessParams(row.params);
  const itemType = normalizedItemType(row.item_type);
  if (parseError) {
    return (
      <TimelineBubble tone="muted">
        [{itemType}] could not parse params: {parseError}
      </TimelineBubble>
    );
  }

  switch (itemType) {
    case 'agent_message': {
      const text = itemText(item);
      return (
        <TimelineBubble tone={text ? 'default' : 'muted'}>
          {text ? <SpecMarkdown>{text}</SpecMarkdown> : <em>Thinking...</em>}
        </TimelineBubble>
      );
    }
    case 'user_message': {
      const text = itemText(item);
      return (
        <TimelineBubble tone={text ? 'user' : 'muted'} attribution="user">
          {text ? <SpecMarkdown>{text}</SpecMarkdown> : <em>(empty message)</em>}
        </TimelineBubble>
      );
    }
    case 'reasoning': {
      const text = itemText(item) ?? itemOutput(item);
      return (
        <details
          style={{
            border: '1px solid var(--hairline)',
            borderRadius: 'var(--radius-md)',
            background: 'var(--paper-2)',
            padding: 'var(--space-3) var(--space-4)',
          }}
        >
          <summary
            style={{
              cursor: 'pointer',
              color: 'var(--text-2)',
              fontSize: 'var(--text-sm)',
              fontWeight: 600,
              letterSpacing: 0,
            }}
          >
            Reasoning
          </summary>
          {text ? (
            <div style={{ marginTop: 'var(--space-3)' }}>
              <SpecMarkdown>{text}</SpecMarkdown>
            </div>
          ) : null}
        </details>
      );
    }
    case 'function_call': {
      return (
        <TimelineBubble>
          Called <strong>{functionName(item)}</strong>(
          <code>{functionArgs(item)}</code>)
        </TimelineBubble>
      );
    }
    case 'function_call_output': {
      return <TimelinePre>{itemOutput(item)}</TimelinePre>;
    }
    case 'web_search': {
      return <TimelineBubble>Searched: {itemQuery(item) || '(empty query)'}</TimelineBubble>;
    }
    case 'local_shell': {
      const command = itemCommand(item);
      const output = itemOutput(item);
      const body = `${command ? `$ ${command}` : '$'}${output ? `\n${output}` : ''}`;
      return <TimelinePre>{body}</TimelinePre>;
    }
    default:
      return (
        <TimelineBubble tone="muted">
          [{itemType}]
        </TimelineBubble>
      );
  }
}

export function ChatTimeline({ cardId }: { cardId?: string }) {
  const [items, setItems] = useState<Map<number, HarnessItem>>(() => new Map());
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [freshFetchKey, setFreshFetchKey] = useState<{
    dedupe: string;
    transcriptClearedCount: number;
  }>(() => ({ dedupe: 'initial', transcriptClearedCount: 0 }));
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const pendingScrollRef = useRef(false);
  const freshFetchDedupeRef = useRef(freshFetchKey.dedupe);
  const rows = useMemo(
    () => Array.from(items.values()).sort((a, b) => a.id - b.id),
    [items],
  );

  useEffect(() => {
    freshFetchDedupeRef.current = freshFetchKey.dedupe;
  }, [freshFetchKey.dedupe]);

  const requestFreshFetch = useCallback((runtimeId: string | null | undefined) => {
    setFreshFetchKey((prev) => {
      const runtimeKey = runtimeId && runtimeId.length > 0 ? runtimeId : null;
      if (runtimeKey && prev.dedupe === runtimeKey) return prev;
      const transcriptClearedCount = runtimeKey
        ? prev.transcriptClearedCount
        : prev.transcriptClearedCount + 1;
      return {
        dedupe: runtimeKey ?? `transcript-cleared:${transcriptClearedCount}`,
        transcriptClearedCount,
      };
    });
  }, []);

  useEffect(() => {
    if (pendingScrollRef.current) {
      const node = scrollRef.current;
      if (node) node.scrollTop = node.scrollHeight;
      pendingScrollRef.current = false;
    }
  }, [rows]);

  useEffect(() => {
    setItems(new Map());
    setError(null);
    if (!cardId) {
      setLoading(false);
      return;
    }

    const controller = new AbortController();
    let cancelled = false;
    const requestKey = freshFetchKey.dedupe;
    setLoading(true);
    fetchHarnessItems(cardId, { direction: 'desc', limit: 100 }, controller.signal)
      .then((loaded) => {
        if (cancelled || freshFetchDedupeRef.current !== requestKey) return;
        pendingScrollRef.current = true;
        setItems((prev) => mergeHarnessRows(prev, loaded).map);
        setLoading(false);
      })
      .catch((err) => {
        if (
          cancelled ||
          controller.signal.aborted ||
          freshFetchDedupeRef.current !== requestKey
        ) {
          return;
        }
        setError(err instanceof Error ? err.message : 'Failed to load conversation');
        setLoading(false);
      });

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [cardId, freshFetchKey.dedupe]);

  useEffect(() => {
    if (!cardId) return;
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    const controller = new AbortController();
    let cancelled = false;
    const off = stream.on((ev) => {
      if (
        ev.ev === 'harness.transcript.cleared' &&
        ev.data.card_id === cardId
      ) {
        requestFreshFetch(ev.data.runtime_id);
        return;
      }
      if (ev.ev !== 'harness.item.added' || ev.data.card_id !== cardId) return;
      const shouldScroll = isAtBottom(scrollRef.current);
      const requestKey = freshFetchDedupeRef.current;
      void fetchHarnessItems(
        cardId,
        { after_id: ev.data.item_db_id - 1, limit: 1 },
        controller.signal,
      )
        .then((loaded) => {
          if (
            cancelled ||
            loaded.length === 0 ||
            freshFetchDedupeRef.current !== requestKey
          ) {
            return;
          }
          if (shouldScroll) pendingScrollRef.current = true;
          setItems((prev) => mergeHarnessRows(prev, loaded).map);
        })
        .catch(() => {
          if (!cancelled && shouldScroll) pendingScrollRef.current = false;
        });
    });
    return () => {
      cancelled = true;
      off();
      controller.abort();
      // Keep the card topic subscribed on the shared stream; subscriptions
      // are sticky across reconnects, matching Codex/Xterm card behavior.
    };
  }, [cardId, requestFreshFetch]);

  const onScroll = () => {
    if (isAtBottom(scrollRef.current)) {
      pendingScrollRef.current = false;
    }
  };

  return (
    <div
      data-testid="spec-chat-timeline"
      aria-label="Spec chat timeline"
      ref={scrollRef}
      onScroll={onScroll}
      style={{
        height: '100%',
        overflow: 'auto',
        display: 'flex',
        flexDirection: 'column',
        gap: 'var(--space-3)',
        paddingBottom: 'var(--space-4)',
      }}
    >
      {loading && rows.length === 0 && (
        <div className="wave-report-empty" style={{ padding: 0 }}>
          <em>Loading conversation...</em>
        </div>
      )}
      {error && rows.length === 0 && (
        <div className="wave-report-empty" role="alert" style={{ padding: 0 }}>
          {error}
        </div>
      )}
      {!loading && !error && rows.length === 0 && (
        <div className="wave-report-empty" style={{ padding: 0 }}>
          <em>No conversation items yet.</em>
        </div>
      )}
      {rows.map((row) => (
        <div key={row.id}>
          <HarnessItemView row={row} />
        </div>
      ))}
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
  const [timelineVersion, setTimelineVersion] = useState(0);

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
      setPhase(null);
      setTimelineVersion((version) => version + 1);
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
        <ChatTimeline key={`${cardId ?? 'pending'}:${timelineVersion}`} cardId={cardId} />
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
