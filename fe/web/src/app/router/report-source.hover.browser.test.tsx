import { cleanup, fireEvent, render } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { trackReportLinkUrl } from '../../../../core/domain/report.ts';
import type { TrackSourceDetail } from '../../../../core/domain/report-source.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { ReportSourcePreview } from './report-source.tsx';
import { ReportReferencePreview } from './report-reference.tsx';
import { placePreview } from '../../ui/hover-preview/placement.ts';
vi.mock('../../ui/hover-preview/placement.ts', { spy: true });
import { createEvidencePreviewTransport, evidenceExamples } from '../shell/link-preview-data.ts';
import styles from '../shell/link-preview.module.css';
import '../../styles/entry.css';

afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); vi.restoreAllMocks(); await commands.emulateReducedMotion(false); });

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
    const { x, y, width, height } = parent.element().closest('[data-nc-preview-slot]')!.getBoundingClientRect();
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

async function mountMobileReport() {
  await page.viewport(390, 844);
  const trackId = 'reference-demo';
  const reportUrl = trackReportLinkUrl(trackId);
  const blocks = [
    ['root', `# Surface acceptance\n\n[Open parent](${reportUrl}#parent)`],
    ['parent', `# Parent fixture\n\n[Open child](${reportUrl}#child)\n\n`
      + Array.from({ length: 12 }, (_, i) => `Parent paragraph ${i}. Reading material.`).join('\n\n')],
    ['child', `# Child fixture\n\n[Open grandchild](${reportUrl}#grandchild)\n\n`
      + Array.from({ length: 24 }, (_, i) => `Child paragraph ${i}. Scroll inside this child.`).join('\n\n')],
    ['grandchild', `# Grandchild fixture\n\n[Go parent](${reportUrl}#parent)`],
  ].map(([id, markdown]) => ({ id, kind: 'prose' as const, payload: { markdown } }));
  const report = { summary: '', body: '', blocks };
  const base = createEvidencePreviewTransport();
  const transport: ApiTransportPort = { send: async request => {
    if (request.path === '/api/areas') return { status: 200, statusText: 'OK', body: [{ id: 'examples', name: 'Examples', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }] };
    if (request.path === '/api/settings') return { status: 200, statusText: 'OK', body: {} };
    if (request.path.endsWith('/report')) return { status: 200, statusText: 'OK', body: { taskDiagnostics: [] } };
    if (request.path === '/api/areas/examples/tracks') return { status: 200, statusText: 'OK', body: [] };
    const response = await base.send(request);
    if (request.path !== '/api/tracks/reference-demo') return response;
    const body = response.body as { cards: { payload: unknown }[] };
    return { ...response, body: { ...body, cards: body.cards.map(card => ({ ...card,
      payload: { schemaVersion: 3, docRev: 1, ...report } })) } };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(), unauthorized, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`/track/${trackId}`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return { client, transport };
}

async function openReference(name: string, owner?: HTMLElement) {
  const button = owner === undefined ? page.getByRole('button', { name, exact: true }).first().element()
    : Array.from(owner.querySelectorAll<HTMLElement>('button')).find(button => button.textContent === name)!;
  button.focus(); await userEvent.keyboard('{ArrowDown}');
  const dialog = page.getByRole('dialog', { name: `Preview: ${name}`, exact: true });
  await expect.element(dialog).toBeVisible();
  return dialog.element() as HTMLElement;
}

it('converges at 390x844 with real long parent and child report references', async () => {
  await mountMobileReport();
  await expect.element(page.getByRole('button', { name: 'Open parent', exact: true }).first()).toBeVisible();
  const root = page.getByRole('button', { name: 'Open parent', exact: true }).first();
  await expect.element(root).toBeVisible();
  (root.element() as HTMLElement).focus();
  await userEvent.keyboard('{ArrowDown}');
  const parent = page.getByRole('dialog', { name: 'Preview: Open parent', exact: true });
  await expect.element(parent.getByRole('button', { name: 'Open child', exact: true })).toBeVisible();
  await new Promise<void>(resolve => { setTimeout(resolve, 500); });
  const before = parent.element().closest('[data-nc-preview-slot]')!.getBoundingClientRect();
  (parent.getByRole('button', { name: 'Open child', exact: true }).element() as HTMLElement).focus();
  await userEvent.keyboard('{ArrowDown}');
  const child = page.getByRole('dialog', { name: 'Preview: Open child', exact: true });
  await expect.element(child.getByRole('heading', { name: 'Child fixture', exact: true })).toBeVisible();
  await new Promise<void>(resolve => { setTimeout(resolve, 500); });
  expect(parent.element().closest('[data-nc-preview-slot]')!.getBoundingClientRect()).toEqual(before);
  const childSlot = child.element().closest('[data-nc-preview-slot]')!.getBoundingClientRect();
  const source = parent.getByRole('button', { name: 'Open child', exact: true }).element().getBoundingClientRect();
  expect(childSlot.left).toBeGreaterThanOrEqual(12); expect(childSlot.right).toBeLessThanOrEqual(378);
  expect(childSlot.top).toBeGreaterThanOrEqual(12); expect(childSlot.bottom).toBeLessThanOrEqual(832);
  expect(childSlot.left >= source.right || childSlot.right <= source.left
    || childSlot.top >= source.bottom || childSlot.bottom <= source.top).toBe(true);
  fireEvent(window, new Event('resize'));
  const rootBounds = root.element().parentElement!.getBoundingClientRect();
  const inputCalls = vi.mocked(placePreview).mock.calls;
  const parentInput = inputCalls.filter(([input]) => input.anchor.left === rootBounds.left && input.anchor.top === rootBounds.top).at(-1)![0];
  expect(parentInput.obstacles).toEqual([]);
  const inputCount = inputCalls.length;
  const changes: MutationRecord[] = [];
  const commits = new MutationObserver(records => changes.push(...records));
  commits.observe(parent.element().closest('[data-nc-preview-slot]')!, { attributes: true, subtree: true });
  commits.observe(child.element().closest('[data-nc-preview-slot]')!, { attributes: true, subtree: true });
  const frames = new Set<string>();
  for (let i = 0; i < 20; i++) {
    await new Promise<void>(resolve => { requestAnimationFrame(() => resolve()); });
    frames.add(JSON.stringify([parent.element().closest('[data-nc-preview-slot]')!.getBoundingClientRect(),
      child.element().closest('[data-nc-preview-slot]')!.getBoundingClientRect()]));
  }
  commits.disconnect();
  expect(vi.mocked(placePreview).mock.calls).toHaveLength(inputCount);
  expect(changes).toEqual([]);
  expect(frames.size).toBe(1);
});


it.each(['normal', 'reduced', 'switch'] as const)('revokes three real report levels at 390 under %s motion', async mode => {
  await commands.emulateReducedMotion(mode === 'reduced');
  await mountMobileReport();
  await expect.element(page.getByRole('button', { name: 'Open parent', exact: true }).first()).toBeVisible();
  for (const reason of ['scroll', 'outside', 'timer', 'navigation'] as const) {
    const parent = await openReference('Open parent');
    const child = await openReference('Open child', parent);
    let grandchild = await openReference('Open grandchild', child);
    const childRegion = child.querySelector<HTMLElement>('[role="region"]')!;
    fireEvent.scroll(childRegion);
    expect(parent.inert).toBe(false);
    // Child scroll revokes its descendant, then the grandchild can be opened anew.
    expect(grandchild.inert || !grandchild.isConnected).toBe(true);
    grandchild = await openReference('Open grandchild', child);
    const cards = [parent, child, grandchild];
    for (const card of cards) for (const animation of card.getAnimations()) { animation.pause(); animation.currentTime = 80; }
    if (mode === 'switch') await commands.emulateReducedMotion(true);
    if (reason === 'scroll') fireEvent.scroll(page.getByRole('button', { name: 'Open parent', exact: true }).first().element().closest('[data-nc-track-page]')!);
    if (reason === 'outside') fireEvent.pointerDown(document.body);
    if (reason === 'timer') {
      fireEvent.pointerLeave(parent, { relatedTarget: document.body, pointerType: 'mouse' });
      await expect.poll(() => parent.inert || !parent.isConnected).toBe(true);
    }
    if (reason === 'navigation') {
      const action = grandchild.querySelector<HTMLElement>('button')!;
      action.focus(); await userEvent.keyboard('{Enter}');
    }
    for (const card of cards) {
      expect(card.inert || !card.isConnected).toBe(true);
      if (card.isConnected) {
        expect(card.getAttribute('aria-hidden')).toBe('true');
        expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
        expect(card.hasAttribute('data-nc-escape-layer')).toBe(false);
        const box = card.getBoundingClientRect();
        expect(card.contains(document.elementFromPoint(box.x + 5, box.y + 5))).toBe(false);
      }
    }
    if (reason === 'navigation') break;
    const reopened = await openReference('Open parent');
    expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(1);
    expect(document.activeElement).toBe(reopened.querySelector('[role="region"]'));
    await userEvent.keyboard('{Escape}');
    expect(document.querySelectorAll('[data-nc-link-preview]')).toHaveLength(0);
    if (mode === 'switch') await commands.emulateReducedMotion(false);
  }
});


it('excludes retained descendant report ownership from background reading inputs', async () => {
  await mountMobileReport();
  await expect.element(page.getByRole('button', { name: 'Open parent', exact: true }).first()).toBeVisible();
  const parent = await openReference('Open parent');
  const child = await openReference('Open child', parent);
  fireEvent.scroll(document);
  for (const card of [parent, child]) card.getAnimations().forEach(animation => { animation.pause(); animation.currentTime = 40; });
  expect(parent.isConnected).toBe(true); expect(child.isConnected).toBe(true);
  const current = await openReference('Open grandchild');
  const calls = vi.mocked(placePreview);
  const withRetention = calls.mock.calls.at(-1)![0];
  const reading = Array.from(document.querySelectorAll<HTMLElement>('[data-nc-report-reading]'))
    .filter(element => element.closest('[data-nc-preview-owner]') === null).map(element => element.getBoundingClientRect());
  expect(withRetention.readingAreas).toEqual(reading);
  for (const card of [parent, child]) card.getAnimations().forEach(animation => animation.finish());
  await expect.poll(() => parent.isConnected || child.isConnected).toBe(false);
  fireEvent(window, new Event('resize'));
  expect(calls.mock.calls.at(-1)![0].readingAreas).toEqual(withRetention.readingAreas);
  expect(current.inert).toBe(false);
});

it('keeps a delayed real file resource in one mounted tree through narrow side width and actual frame growth', async () => {
  await page.viewport(1040, 900);
  let resolve!: (value: { path: string; text: string; size: number; truncated: boolean }) => void;
  const readFile = vi.fn(() => new Promise<{ path: string; text: string; size: number; truncated: boolean }>(done => { resolve = done; }));
  const report = { summary: '', body: '[Delayed file](./notes.md)', blocks: null };
  render(<section style={{ position: 'absolute', left: 200, top: 300, width: 504 }}>
    <ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ trackId: 'file', report,
      files: { readFile, rawUrl: path => path } }} />
  </section>);
  await page.getByRole('button', { name: 'Delayed file', exact: true }).hover();
  await expect.element(page.getByText('Loading file…', { exact: true })).toBeVisible();
  const card = page.getByRole('dialog').element() as HTMLElement;
  const actual = card.parentElement!;
  const slot = card.closest<HTMLElement>('[data-nc-preview-slot]')!;
  const reserved = slot.getBoundingClientRect();
  const oldFrame = actual.getBoundingClientRect();
  const motion = card.getAnimations()[0]; motion.pause(); motion.currentTime = 80;
  const before = new DOMMatrix(getComputedStyle(card).transform);
  const text = '# Delayed contents\n\n' + 'Long resource paragraph.\n\n'.repeat(90);
  resolve({ path: 'notes.md', text, size: text.length, truncated: false });
  await expect.element(page.getByRole('heading', { name: 'Delayed contents', exact: true })).toBeVisible();
  await expect.poll(() => actual.getBoundingClientRect().height).toBeGreaterThan(oldFrame.height);
  expect(slot.getBoundingClientRect()).toEqual(reserved);
  expect(card.getAnimations()).toHaveLength(1);
  const rebased = card.getAnimations()[0]; rebased.pause(); rebased.currentTime = 0;
  const after = new DOMMatrix(getComputedStyle(card).transform);
  expect(after.a).toBeCloseTo(before.a, 5);
  expect(after.e).toBeCloseTo(before.e, 3);
  expect(after.f).toBeCloseTo(before.f, 3);
  await page.viewport(1020, 900);
  await expect.poll(() => slot.getBoundingClientRect().width).toBeLessThan(reserved.width);
  expect(page.getByRole('heading', { name: 'Delayed contents', exact: true }).element().closest('[role="dialog"]')).toBe(card);
  expect(readFile).toHaveBeenCalledExactlyOnceWith('notes.md');
});
