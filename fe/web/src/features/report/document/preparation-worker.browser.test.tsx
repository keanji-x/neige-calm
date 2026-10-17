import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { TrackReport } from '../../../../../core/domain/report.ts';
import { runWorkerTask } from '../../../systems/worker-task/public.ts';
import { prepareReportDocument, type PreparedReport } from './preparation.ts';
import { ReportDocument } from './public.tsx';
import workerUrl from './preparation.worker.ts?worker&url';
import '../../../styles/entry.css';

afterEach(cleanup);

it('prepares in the actual native Worker and renders its coherent cloned report and heading Maps', async () => {
  const report: TrackReport = { summary: '', body: '', blocks: [
    { id: 'first', kind: 'prose', payload: { markdown: '# Shared heading\n\n[Issue](https://github.com/example/project/issues/1).' } },
    { id: 'second', kind: 'prose', payload: { markdown: '# Shared heading\n\nFollow issue #2 and [File](./info.md).\n\nEntities: &amp; &NotEqualTilde; &copy; &#x1F642; &madeup; &notit; &semi;' } },
  ] };
  const preparation = await runWorkerTask<TrackReport | null, PreparedReport>(
    () => new Worker(workerUrl, { type: 'module' }),
    report, new AbortController().signal,
  );
  expect(preparation).toEqual(prepareReportDocument(report));
  expect(preparation.report).not.toBe(report);
  if (preparation.kind !== 'blocks') throw new Error('Expected prepared blocks');
  const prose = preparation.blocks[0].prose;
  if (prose === null || prose.result.status !== 'ready') throw new Error('Expected prepared prose');
  expect(prose.result.headingIds).toBeInstanceOf(Map);
  expect([...prose.result.headingIds.values()]).toEqual(['first-h1']);
  render(<ReportDocument report={preparation.report} preparation={preparation} empty={null}
    fileRoot="/workspace" onOpenFileLink={() => undefined} />);
  await expect.element(page.getByRole('heading', { name: 'Shared heading' }).first()).toBeVisible();
  const headings = [...document.querySelectorAll('[data-nc-report] h2')];
  expect(headings.map(heading => heading.id)).toEqual(['first-h1', 'second-h1']);
  await expect.element(page.getByRole('button', { name: 'File', exact: true })).toBeVisible();
  expect(document.querySelector('[data-nc-report] script')).toBeNull();
  expect(document.querySelector('[data-nc-report]')?.textContent).toContain('Entities: & ≂̸ © 🙂 &madeup; &notit; ;');
});
