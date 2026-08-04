import { describe, expect, it, vi } from 'vitest';
import type { ReportBlock } from '../cards/builtins/wave-report';
import { performTaskAction } from './WaveReportPage';

const task = {
  id: 'b_task', kind: 'task', rev: 7,
  payload: { key: 'build', kind: 'codex', goal: 'Build', ready: true, declared_by: 'spec' },
} as ReportBlock;

function deps(answer: boolean) {
  return {
    confirm: vi.fn(() => answer),
    updateBlock: vi.fn(async () => ({})),
    deleteBlock: vi.fn(async () => ({})),
    patchWave: vi.fn(async () => ({})),
  };
}

describe('task card actions', () => {
  it('asks on deleting a spec task and tightens the wave when the user chooses yes', async () => {
    const calls = deps(true);
    await performTaskAction('w1', task, 'delete', calls);
    expect(calls.confirm).toHaveBeenCalledWith(expect.stringContaining('只删这条'));
    expect(calls.deleteBlock).toHaveBeenCalledWith('w1', 'b_task', 7);
    expect(calls.patchWave).toHaveBeenCalledWith({ automation_policy: 'declare-and-wait' });
  });

  it('only deletes the task when the user chooses that option', async () => {
    const calls = deps(false);
    await performTaskAction('w1', task, 'delete', calls);
    expect(calls.deleteBlock).toHaveBeenCalledOnce();
    expect(calls.patchWave).not.toHaveBeenCalled();
  });

  it('clears a rejection record without changing automation', async () => {
    const calls = deps(false);
    await performTaskAction('w1', { ...task, payload: { key: 'build', tombstone: {}, declared_by: 'spec', tombstoned_by: 'user' } } as ReportBlock, 'clear', calls);
    expect(calls.deleteBlock).toHaveBeenCalledOnce();
    expect(calls.patchWave).not.toHaveBeenCalled();
  });

  it('restores automation without deleting the rejection record', async () => {
    const calls = deps(false);
    await performTaskAction('w1', task, 'restore', calls);
    expect(calls.patchWave).toHaveBeenCalledWith({ automation_policy: 'auto-declare' });
    expect(calls.deleteBlock).not.toHaveBeenCalled();
  });
});
