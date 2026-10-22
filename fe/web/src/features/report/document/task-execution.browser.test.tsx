import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { deriveReportTasks, readTrackReport } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY } from '../../../../../core/domain/track.ts';
import { groupPanelRows } from '../../../../../core/view/panel-groups.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

it('reveals the failed task with a reachable cause on hover and neutral declaration readiness', async () => {
  await page.viewport(1000, 700);
  render(<div style={{ padding: 32 }}><ReportDocument
    report={{ summary: '', body: '', blocks: [
      { id: 'b-failed', kind: 'task', payload: {
        key: 'analyze-artifacts', kind: 'codex', declared_by: 'spec', ready: true,
        goal: 'Analyze the worker artifacts.',
      } },
      { id: 'b-declared', kind: 'task', payload: {
        key: 'summarize', kind: 'claude', declared_by: 'spec', ready: true,
        goal: 'Summarize the accepted analysis.',
      } },
    ] }}
    taskVerdicts={[{
      blockId: 'b-failed', key: 'analyze-artifacts', schedulable: true,
      status: 'failed', statusDetail: 'gate-red',
    }]}
    empty={null}
  /></div>);
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  const failed = document.querySelector<HTMLElement>('[data-nc-task-state="failed"] summary span[title]')!;
  await userEvent.hover(failed);
  const box = failed.getBoundingClientRect();
  const target = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
  expect(target?.closest('[title]')?.getAttribute('title')).toBe('failed — gate-red');
  expect(failed.innerText).toBe('failed');
  const declared = document.querySelector<HTMLElement>('[data-nc-task-state="ready"] summary')!;
  expect(declared.innerText).toContain('Declaration ready');
  const neutral = getComputedStyle(declared).getPropertyValue('--text-3').trim();
  expect(neutral).not.toBe('');
  const probe = document.createElement('span');
  probe.style.color = neutral;
  declared.append(probe);
  expect(getComputedStyle(declared.lastElementChild!.previousElementSibling!).color).toBe(getComputedStyle(probe).color);
  probe.remove();
  await page.screenshot();
});


it.each(['codex', 'claude'] as const)('keeps a completed %s review readable in task references and inventory', async (kind) => {
  const report = readTrackReport([{
    id: 'report', track_id: 't', kind: 'track-report', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { body: '', blocks: [{ id: 'b-review', kind: 'task', payload: {
      key: 'independent-review', kind, goal: 'Review the committed change without editing it.',
      access: 'read_only', result_contract: 'review-v1', ready: true, declared_by: 'user',
    } }] },
  }]);
  const tasks = deriveReportTasks(report?.blocks ?? null, [{
    blockId: 'b-review', key: 'independent-review', schedulable: true,
    status: 'done', workerCardId: 'review-worker',
  }]);
  const inventory = deriveTrackPageView({ cards: [], tasks, activity: NEUTRAL_ACTIVITY,
    openableCards: new Set(['review-worker']) }).rowModules.find(module => module.key === 'tasks')!;
  expect(groupPanelRows(inventory.rows, 'tasks').map(group => [group.key, group.rows.length]))
    .toEqual([['done', 1]]);
  expect(tasks[0]).toMatchObject({ key: 'independent-review', status: 'done', kind,
    workerCardId: 'review-worker' });

  render(<ReportDocument report={report} taskRows={tasks} empty={null} />);
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  await userEvent.click(document.querySelector('#b-review details > summary')!);
  await expect.element(page.getByText('Review the committed change without editing it.')).toBeVisible();
  await expect.element(page.getByText('done', { exact: true })).toBeVisible();
  expect(document.querySelector('[data-nc-task-state="done"]')).not.toBeNull();
  expect(document.body.textContent).not.toContain('unsupported block kind');
});

it('renders a compact declaration and refusal without worker-only fields', async () => {
  await page.viewport(1000, 700);
  const report = readTrackReport([{
    id: 'report', track_id: 't', kind: 'track-report', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { body: '', blocks: [{ id: 'b-compact', kind: 'task', payload: {
      key: 'compact-planner', kind: 'compact', context: 'Continue from verified task evidence.', ready: true, declared_by: 'spec',
    } }] },
  }]);
  const tasks = deriveReportTasks(report?.blocks ?? null, [{
    blockId: 'b-compact', key: 'compact-planner', schedulable: true,
    status: 'failed', statusDetail: 'compaction-refused: unavailable',
  }]);
  render(<ReportDocument report={report} taskRows={tasks} empty={null} />);
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  await userEvent.click(document.querySelector('#b-compact details > summary')!);
  await expect.element(page.getByText('compact', { exact: true })).toBeVisible();
  await expect.element(page.getByText('Planner agent', { exact: true })).toBeVisible();
  expect(document.querySelector('#b-compact details > summary [title]')?.getAttribute('title'))
    .toBe('failed — compaction-refused: unavailable');
  expect(document.body.textContent).not.toContain('Done when');
  expect(document.body.textContent).not.toContain('Checks');
  expect(document.body.textContent).not.toContain('unsupported block kind');
  await page.screenshot();
});
