import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackSourceDetail } from '../../../../core/domain/report-source.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportSourcePreview } from './report-source.tsx';
import { ReportReferencePreview } from './report-reference.tsx';
import { createEvidencePreviewTransport, evidenceExamples } from '../shell/link-preview-data.ts';
import styles from '../shell/link-preview.module.css';
import '../../styles/entry.css';

afterEach(cleanup);

it('keeps the asynchronous cross-report preview stable beside a report with tables', async () => {
  await page.viewport(1600, 1000);
  const transport = createEvidencePreviewTransport();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const report = evidenceExamples();
  render(<QueryClientProvider client={client}><main className={styles.page}>
    <header className={styles.header}><h1>来源引用预览</h1></header>
    <div className={styles.document}>
      <ReportDocument report={report} empty={null} linkPreview={{ trackId: 'link-preview', report,
        files: { readFile: () => Promise.reject(new Error('Unexpected file read')), rawUrl: path => path },
        renderReference: target => <ReportReferencePreview transport={transport} unauthorized={unauthorized} target={target}
          onOpenSource={vi.fn()} onOpenFile={vi.fn()} onOpenLink={vi.fn()} />,
      }} />
    </div>
  </main></QueryClientProvider>);
  await page.getByRole('button', { name: '另一份报告', exact: true }).hover();
  await expect.element(page.getByRole('button', { name: '另一报告的来源', exact: true })).toBeVisible();
  const parent = page.getByRole('dialog', { name: 'Preview: 另一份报告', exact: true });
  const positions = new Set<string>();
  for (let frame = 0; frame < 20; frame++) {
    await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
    const { x, y, width, height } = parent.element().getBoundingClientRect();
    positions.add(JSON.stringify({ x, y, width, height }));
  }
  expect(positions.size).toBe(1);
  await page.getByRole('button', { name: '另一报告的来源', exact: true }).hover();
  await expect.element(page.getByRole('heading', { name: '另一报告保存的来源 · 示例', exact: true })).toBeVisible();
});

it('reads and scrolls a captured source beside the report without moving the report', async () => {
  await page.viewport(1440, 900);
  const source: TrackSourceDetail = {
    source_id: 'src_0971fbde', title: '巴克莱芯片压力测试，10月7日', provenance: 'summary',
    origin: { kind: 'plugin', plugin_id: 'research', tool: 'read', args_sha256: 'ab', args_canon: 'v1' },
    published_at: '2026-10-07', captured_at: '2026-10-07T08:00:00Z', body_sha256: 'cd', body_bytes: 8000,
    body: '# 摘要阅读\n\n' + '供需变化与盈利风险。\n\n'.repeat(100) + '引用结论。',
    quotes: [{ id: 'q1', text: '引用结论。', start: 0, end: 1 }],
  };
  const send = vi.fn<ApiTransportPort['send']>(() => Promise.resolve({ status: 200, statusText: 'OK', body: source }));
  const transport = { send };
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onOpenSource = vi.fn();
  const report = { summary: '', body: '# 来源与边界\n\n摘要层：[巴克莱芯片压力测试，10月7日](neige://source/src_0971fbde#q1)。', blocks: null };
  render(<QueryClientProvider client={client}><div data-testid="reading" style={{ position: 'absolute', left: 80, top: 160, width: 560,
    ['--document-start' as string]: '0px', ['--document-measure' as string]: '560px' }}>
    <ReportDocument report={report} empty={null} onOpenSourceLink={onOpenSource} linkPreview={{ trackId: 't1', report,
      files: { readFile: () => Promise.reject(new Error('Unexpected file read')), rawUrl: path => path },
      renderSource: target => <ReportSourcePreview transport={transport} trackId="t1" target={target} unauthorized={unauthorized} />,
    }} />
  </div></QueryClientProvider>);
  expect(send).not.toHaveBeenCalled();
  const reading = page.getByTestId('reading').element().querySelector('[data-nc-report-reading]')!.getBoundingClientRect();
  const scrollTop = document.documentElement.scrollTop;
  const link = page.getByRole('button', { name: source.title, exact: true });
  await link.hover();
  await expect.element(page.getByText('智堡摘要，非机构原文', { exact: true })).toBeVisible();
  expect(send.mock.calls[0]?.[0].path).toBe('/api/tracks/t1/sources/src_0971fbde');
  await expect.element(page.getByRole('heading', { name: '摘要阅读', exact: true })).toBeVisible();
  const preview = page.getByRole('dialog', { name: `Preview: ${source.title}` }).element();
  const bounds = preview.getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(reading.right);
  expect(bounds.right).toBeLessThanOrEqual(1440);
  expect(bounds.bottom).toBeLessThanOrEqual(900);
  const region = preview.querySelector<HTMLElement>('[role="region"]')!;
  expect(region.scrollHeight).toBeGreaterThan(region.clientHeight);
  region.scrollTop = region.scrollHeight;
  expect(region.scrollTop).toBeGreaterThan(0);
  expect(document.documentElement.scrollTop).toBe(scrollTop);
  expect(page.getByTestId('reading').element().querySelector('[data-nc-report-reading]')!.getBoundingClientRect().top).toBe(reading.top);
  await page.getByRole('dialog', { name: `Preview: ${source.title}` }).getByRole('button', { name: '原文', exact: true }).click();
  expect(preview.querySelector('mark')?.textContent).toBe('引用结论。');
  expect(document.documentElement.scrollTop).toBe(scrollTop);
  region.scrollTop = 0;
  await page.getByRole('dialog', { name: `Preview: ${source.title}` }).screenshot({ path: '../../../../test-results/neige-source-hover.png' });
  await page.getByRole('button', { name: 'Open in workspace', exact: true }).click();
  await expect.poll(() => page.getByRole('dialog', { name: `Preview: ${source.title}` }).query()).toBeNull();
  expect(onOpenSource).toHaveBeenCalledExactlyOnceWith({ destination: 'neige://source/src_0971fbde#q1', sourceId: 'src_0971fbde', quoteId: 'q1' });
});
