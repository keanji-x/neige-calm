import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    'task-worker': TaskWorkerCard;
  }
}

export type TaskWorkerCard = Readonly<{ type: 'task-worker'; id: string }>;

export function isTaskWorkerHarnessPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { harness_profile?: unknown }).harness_profile === 'task_worker';
}

export const TASK_WORKER_CARD_ENTRY = Object.freeze({
  type: 'task-worker',
  component: () => null,
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Task worker',
  accessibleName: () => 'Task worker',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): TaskWorkerCard | null => (
    card.kind === 'codex' && isTaskWorkerHarnessPayload(card.payload)
      ? Object.freeze({ type: 'task-worker', id: card.id } as const)
      : null
  ),
}) satisfies CardEntry<TaskWorkerCard>;
