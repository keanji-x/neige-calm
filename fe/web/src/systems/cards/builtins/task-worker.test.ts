import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { TaskWorkerCard } from './task-worker.js';
import { TASK_WORKER_CARD_ENTRY, isTaskWorkerHarnessPayload } from './task-worker.js';
import { registerAvailableBuiltinCards } from './register.js';

const callComponent = (entry: typeof TASK_WORKER_CARD_ENTRY) =>
  (entry.component as unknown as (props: unknown) => unknown)({});

describe('task worker card entry', () => {
  it('recognises a task worker only by the task_worker marker', () => {
    expect(TASK_WORKER_CARD_ENTRY.fromKernel?.({
      id: 'c1', kind: 'codex', payload: { harness_profile: 'task_worker' },
    })).toEqual({ type: 'task-worker', id: 'c1' });
    expect(TASK_WORKER_CARD_ENTRY.fromKernel?.({
      id: 'c2', kind: 'codex', payload: { harness_profile: 'task_worker', anything: 1 },
    })).toEqual({ type: 'task-worker', id: 'c2' });
  });

  /* A predicate widened to "has a `harness_profile`" would make every area chat card resolve as a task worker. */
  it('refuses the plain-chat marker and an unmarked codex card', () => {
    for (const payload of [
      {}, { harness_profile: 'plain_chat' }, { harness_profile: true },
      { harness_profile: 'assistant' }, { planner_harness: true },
    ]) {
      expect(
        TASK_WORKER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload }),
        JSON.stringify(payload),
      ).toBeNull();
    }
  });

  it('narrows non-object payloads instead of throwing', () => {
    for (const payload of [null, undefined, 'task_worker', 7, true, []]) {
      expect(() => TASK_WORKER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).not.toThrow();
      expect(TASK_WORKER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).toBeNull();
    }
    expect(isTaskWorkerHarnessPayload(null)).toBe(false);
    expect(isTaskWorkerHarnessPayload({ harness_profile: 'task_worker' })).toBe(true);
  });

  it('never claims a kernel kind other than codex', () => {
    for (const kind of ['terminal', 'claude', 'assistant']) {
      expect(TASK_WORKER_CARD_ENTRY.fromKernel?.({
        id: 'c', kind, payload: { harness_profile: 'task_worker' },
      })).toBeNull();
    }
  });

  it('is headless, 1x1 and kernel-minted only', () => {
    expect(callComponent(TASK_WORKER_CARD_ENTRY)).toBeNull();
    expect(TASK_WORKER_CARD_ENTRY.headless).toBe(true);
    expect(TASK_WORKER_CARD_ENTRY.defaultSize).toEqual({ w: 1, h: 1, minW: 1, minH: 1 });
    expect(TASK_WORKER_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
  });

  it('takes no claim, so it stays on the insertion-ordered fallback scan', () => {
    expect((TASK_WORKER_CARD_ENTRY as CardEntry<TaskWorkerCard>).claim).toBeUndefined();
  });

  it('resolves through a really booted registry, ahead of nothing and behind codex', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({
      id: 'c1', kind: 'codex', payload: { harness_profile: 'task_worker' },
    })?.type).toBe('task-worker');
    expect(registry.resolve({ id: 'c2', kind: 'codex', payload: {} })?.type).toBe('codex');
    expect(registry.resolve({ id: 'c3', kind: 'codex', payload: { planner_harness: true } })?.type).toBe('planner');
  });
});
