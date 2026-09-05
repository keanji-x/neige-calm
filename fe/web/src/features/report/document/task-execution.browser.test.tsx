import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';
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
