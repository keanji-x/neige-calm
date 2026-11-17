import { describe, expect, it } from 'vitest';
import type { ReportTaskRow } from '../domain/report.js';
import { NEUTRAL_ACTIVITY, type CardWire } from '../domain/track.js';
import { deriveTrackPageView } from './track-page.js';

const worker = (id: string): CardWire => ({
  id, track_id: 'track', kind: 'terminal', title: id, sort: 0,
  payload: { terminal_presentation: { source: 'manual', in_workbench: false } },
  deletable: true, created_at: 0, updated_at: 0,
});
const task = (blockId: string, workerCardId: string | null, status = 'done'): ReportTaskRow => ({
  blockId, key: blockId, state: 'ready', declaration: null, status, statusDetail: null,
  kind: 'terminal', workerCardId, pendingReason: null,
});

describe('Progress after worker deletion', () => {
  it.each(['done', 'failed', 'running'])('removes a %s worker entry while retaining its task source', status => {
    const tasks = [task('build', 'worker', status)];
    const before = deriveTrackPageView({ cards: [worker('worker')], tasks,
      activity: NEUTRAL_ACTIVITY, openableCards: new Set(['worker']), existingCardIds: new Set(['worker']) });
    expect(before.desktop.rowModules[0].rows.map(row => row.id)).toEqual(['task:build']);
    const after = deriveTrackPageView({ cards: [], tasks, activity: NEUTRAL_ACTIVITY,
      openableCards: new Set<string>(), existingCardIds: new Set<string>() });
    expect(after.desktop.rowModules[0].rows).toEqual([]);
    expect(after.rowModules[0].rows.map(row => row.id)).toEqual(['build']);
  });

  it('keeps workers that exist outside the visible or openable card subset', () => {
    const view = deriveTrackPageView({ cards: [], tasks: [task('headless-task', 'headless')],
      activity: NEUTRAL_ACTIVITY, openableCards: new Set<string>(), existingCardIds: new Set(['headless']) });
    expect(view.desktop.rowModules[0].rows.map(row => row.id)).toEqual(['task:headless-task']);
  });

  it('keeps tasks with no assigned worker', () => {
    const view = deriveTrackPageView({ cards: [], tasks: [task('queued', null, 'pending'), task('spawn-failed', null, 'failed')],
      activity: NEUTRAL_ACTIVITY, openableCards: new Set<string>(), existingCardIds: new Set<string>() });
    expect(view.desktop.rowModules[0].rows.map(row => row.id)).toEqual(['task:queued', 'task:spawn-failed']);
  });

  it.each(['new-worker', null])('uses the current execution worker %s instead of the deleted verdict worker', workerCardId => {
    const current = { ...task('recovered', 'deleted-worker'), execution: {
      attemptId: 'attempt-2', generation: 2, status: 'running', label: 'Running', statusDetail: null,
      workerCardId, blockingReason: null, createdAtMs: 1, finishedAtMs: null,
    } };
    const view = deriveTrackPageView({ cards: [worker('new-worker')], tasks: [current],
      activity: NEUTRAL_ACTIVITY, openableCards: new Set(['new-worker']), existingCardIds: new Set(['new-worker']) });
    expect(view.desktop.rowModules[0].rows.map(row => row.id)).toEqual(['task:recovered']);
  });

  it('removes shared deleted-worker entries and preserves other workers', () => {
    const view = deriveTrackPageView({ cards: [worker('retained')],
      tasks: [task('older', 'deleted'), task('newer', 'deleted'), task('retained-task', 'retained')],
      activity: NEUTRAL_ACTIVITY, openableCards: new Set(['retained']), existingCardIds: new Set(['retained']) });
    expect(view.desktop.rowModules[0].rows.map(row => row.id)).toEqual(['task:retained-task']);
    expect(view.rowModules[0].rows).toHaveLength(3);
  });
});
