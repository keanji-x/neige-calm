import { createElement } from 'react';
import type { WorkerSnapshot, WorkerSnapshotStatus } from '../../../../../core/api/generated/wire.ts';
import { NativeWorkerCardView } from './native-worker-card.tsx';
import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { isAssistantHarnessPayload } from './assistant.ts';
import { isPlannerHarnessPayload } from './planner.ts';
import { TerminalCardView } from './terminal-card.tsx';
import { terminalSessionFromCard, cwdFromPayload, type TerminalCard } from './terminal.ts';

declare module '../registry.js' {
  interface CardDataMap {
    codex: CodexCard;
  }
}

type InteractiveCodexCard = Readonly<{
  type: 'codex';
  presentation: 'interactive_tui';
  id: string;
  title: string | null;
  terminalId: string | null;
  sessionState: TerminalCard['sessionState'];
  cwd: string | null;
  gateCwd: string | null;
}>;

export type CodexCard = InteractiveCodexCard | Readonly<{
  type: 'codex'; presentation: 'native_only'; id: string; title: string | null;
  cwd: string | null; snapshot: WorkerSnapshot;
}>;

const TASK_STATUSES = Object.freeze(['pending', 'dispatched', 'running', 'verifying', 'done', 'failed', 'canceled'] as const);
function record(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown> : null;
}
function nativeSnapshot(value: unknown): WorkerSnapshot | null {
  const source = record(value);
  if (source === null || typeof source.task_id !== 'string' || typeof source.goal !== 'string'
    || !TASK_STATUSES.some((status) => status === source.status)) return null;
  const report = record(source.report);
  if (report === null) return null;
  if (report.kind === 'pending') return Object.freeze({ task_id: source.task_id, goal: source.goal,
    status: source.status as WorkerSnapshotStatus, report: Object.freeze({ kind: 'pending' }) });
  if (report.kind !== 'reported' || (report.outcome !== 'completed' && report.outcome !== 'failed')
    || !Object.hasOwn(report, 'result')) return null;
  return Object.freeze({ task_id: source.task_id, goal: source.goal, status: source.status as WorkerSnapshotStatus,
    report: Object.freeze({ kind: 'reported' as const, outcome: report.outcome as 'completed' | 'failed', result: report.result }) });
}

const CODEX_FALLBACK_TITLE = 'codex';

export function isPlainChatPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { harness_profile?: unknown }).harness_profile === 'plain_chat';
}

export const CODEX_CARD_ENTRY = Object.freeze({
  type: 'codex',
  component: (props: CardComponentProps<CodexCard>) => props.card.presentation === 'native_only'
    ? createElement(NativeWorkerCardView, { card: props.card, activity: props.activity, onRemove: props.onRemove })
    : createElement(TerminalCardView, { ...props, card: props.card, fallbackTitle: CODEX_FALLBACK_TITLE }),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  title: (card: CodexCard) => card.title ?? 'Codex',
  accessibleName: (card: CodexCard) => card.title ?? 'Codex',
  /* `atomic`: `POST /api/tracks/:id/codex-cards` writes the row and spawns the daemon in one call. The submit is not implemented here: this module has no transport and may not acquire one; `app/router` owns the call. */
  create: Object.freeze({
    mode: 'atomic' as const,
    submit: (): Promise<{ cardId: string }> => Promise.reject(new Error('CodexCardSubmitViaTrackRoute')),
  }),
  addPanel: Object.freeze({
    label: 'codex',
    fields: Object.freeze([
      Object.freeze({ key: 'title', label: 'Title', kind: 'text' as const, placeholder: 'Codex' }),
      Object.freeze({
        key: 'cwd',
        label: 'Working directory',
        kind: 'directory' as const,
        hint: "Optional. Left empty, codex runs in the track's own directory.",
      }),
    ]),
  }),
  fromKernel: (card: KernelCardInput): CodexCard | null => {
    if (card.kind !== 'codex' || isPlannerHarnessPayload(card.payload)
      || isPlainChatPayload(card.payload) || isAssistantHarnessPayload(card.payload)) return null;
    const payload = record(card.payload);
    const presentation = record(payload?.worker_presentation);
    if (presentation?.kind === 'native_only') {
      const snapshot = nativeSnapshot(payload?.worker_snapshot);
      return snapshot === null ? null : Object.freeze({ type: 'codex', presentation: 'native_only',
        id: card.id, title: null, cwd: cwdFromPayload(card.payload), snapshot });
    }
    if (payload !== null && Object.hasOwn(payload, 'worker_presentation')
      && (presentation?.kind !== 'interactive_tui' || typeof presentation.terminal_id !== 'string')) return null;
    return Object.freeze({ type: 'codex', presentation: 'interactive_tui', id: card.id, title: null,
      ...terminalSessionFromCard(card), cwd: cwdFromPayload(card.payload), gateCwd: cwdFromPayload(card.payload, 'gate_cwd') });
  },
}) satisfies CardEntry<CodexCard>;
