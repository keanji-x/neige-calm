import { expect, test } from '@playwright/test';
// The fixture uses the table-shaped outputs of the current market plugin.
// @ts-expect-error Plain data helper shared with the Node contract tests.
import { marketFixture } from './market-fixture.mjs';

test('native authentication protects live mode before any portfolio read', async ({ page }) => {
  const reads: string[] = [];
  await page.route('**/api/**', async route => {
    if (!new URL(route.request().url()).pathname.startsWith('/api/')) { await route.continue(); return; }
    reads.push(new URL(route.request().url()).pathname);
    await route.fulfill({ status: 401, json: { error: 'Unauthorized', code: 'unauthorized' } });
  });
  await page.goto('/next/track/market-track?market=1');
  await expect(page.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
  expect(reads).toEqual(['/api/auth/whoami']);
  await expect(page.locator('iframe[title="组合概览图表"]')).toHaveCount(0);
});

test('reads real-shaped market overlays into the native Report and chart sandbox', async ({ page }) => {
  const fixture = marketFixture();
  const now = Date.now();
  const area = { id: 'a', name: '投资', color: '#567', sort: 1, kind: 'user', created_at: now, updated_at: now };
  const track = { id: 'market-track', area_id: 'a', title: '我的组合', sort: 1, lifecycle: 'working', cwd: '/workspace', archived_at: null,
    pinned_at: null, terminal_at: null, created_at: now, updated_at: now };
  const writes: string[] = [];
  const portfolioMetadata = JSON.stringify({ assets: { 'US:AAA': { name: 'US:AAA', trackId: 'research-us', nextEvent: null } }, trades: [
    { id: 'trade-1', symbol: 'US:AAA', name: 'US:AAA', date: '2026-08-01', trackId: 'research-us', side: 'buy',
      quantity: 10, price: 9, currency: 'USD', fee: 1, reason: '研究后建仓' },
  ] });
  await page.route('**/api/**', async route => {
    if (!new URL(route.request().url()).pathname.startsWith('/api/')) { await route.continue(); return; }
    const request = route.request();
    if (request.method() !== 'GET') writes.push(request.url());
    const path = new URL(request.url()).pathname;
    const data: Record<string, unknown> = {
      '/api/auth/whoami': { userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'test-session' },
      '/api/version': { webCompatVersion: 24, minWebCompatVersion: 24, syncEventVersion: 1, dbInstanceId: 'portfolio-test' },
      '/api/areas': [area], '/api/areas/a/tracks': [track], '/api/settings': {},
      '/api/tracks/market-track': { track, cards: [], can_resume: false, overlays: fixture.overlays },
      '/api/tracks/market-track/report': { taskDiagnostics: [] },
      '/api/tracks/market-track/workspace/readfile': { path: '.neige-portfolio/metadata.json', text: portfolioMetadata,
        size: new TextEncoder().encode(portfolioMetadata).length, truncated: false },
      '/api/tracks/market-track/backlinks': { backlinks: [], truncated: false, skipped_sources: 0 },
      '/api/tracks/research-us': { track: { ...track, id: 'research-us', title: '个股研究' }, can_resume: false, overlays: marketFixture('research-us').overlays,
        cards: [{ id: 'research-report', track_id: 'research-us', kind: 'track-report', title: null, sort: 0, deletable: false,
          created_at: now, updated_at: now, payload: { schemaVersion: 3, docRev: 1, summary: '保留原研究', body: '',
            blocks: [{ id: 'thesis', kind: 'prose', payload: { markdown: '# 原有投资逻辑\n\n这份研究 Report 不应被持仓面板替换。' } }] } }] },
      '/api/tracks/research-us/report': { taskDiagnostics: [] },
      '/api/tracks/research-us/backlinks': { backlinks: [], truncated: false, skipped_sources: 0 },
    };
    await route.fulfill({ json: data[path] ?? [] });
  });
  await page.goto('/next/track/market-track?market=1');
  await expect(page.getByRole('heading', { name: '持仓明细', exact: true })).toBeVisible();
  await expect(page.getByRole('table').first()).toContainText('US:AAA');
  await expect(page.getByRole('table').first()).toContainText('10.00 USD');
  await expect(page.getByRole('table').first()).toContainText('79.5%');
  await expect(page.getByRole('table').last()).toContainText('研究后建仓');
  await expect(page.getByRole('table').last()).toContainText('9.00 USD');
  const figure = page.frameLocator('iframe[title="组合概览图表"]');
  await expect(figure.locator('.recharts-pie-sector')).toHaveCount(2);
  await expect(figure.locator('.recharts-area-curve')).toBeVisible();
  await expect(figure.locator('body')).not.toContainText('青松科技');
  expect(writes).toEqual([]);
  // A chart may not ask for another Track, and a parent-window event may not
  // impersonate a request sent by this sandbox.
  const received = await figure.locator('body').evaluate(async () => {
    let count = 0;
    const listener = (event: MessageEvent) => { if (event.data?.trackId === 'another-track') count += 1; };
    window.addEventListener('message', listener);
    parent.postMessage({ type: 'neige:portfolio-ready', trackId: 'another-track' }, '*');
    await new Promise(resolve => setTimeout(resolve, 120));
    window.removeEventListener('message', listener);
    return count;
  });
  expect(received).toBe(0);
  await figure.locator('body').evaluate(() => {
    (window as unknown as { portfolioReplies: number }).portfolioReplies = 0;
    window.addEventListener('message', event => {
      if (event.data?.type === 'neige:portfolio-snapshot') (window as unknown as { portfolioReplies: number }).portfolioReplies += 1;
    });
  });
  await page.evaluate(() => window.postMessage({ type: 'neige:portfolio-ready', trackId: 'market-track' }, '*'));
  const impersonatedReplies = await figure.locator('body').evaluate(async () => {
    await new Promise(resolve => setTimeout(resolve, 120));
    return (window as unknown as { portfolioReplies: number }).portfolioReplies;
  });
  expect(impersonatedReplies).toBe(0);
  await expect(figure.locator('.recharts-pie-sector')).toHaveCount(2);
  await page.getByRole('table').first().getByRole('button', { name: 'US:AAA', exact: true }).click();
  await expect(page.getByRole('heading', { name: '原有投资逻辑', exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByRole('heading', { name: '原有投资逻辑', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: '持仓明细', exact: true })).toHaveCount(0);
});
