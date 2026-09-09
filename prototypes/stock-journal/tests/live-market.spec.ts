import { expect, test } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { parse } from '../../../fe/core/markdown/public.ts';
// @ts-expect-error Plain market fixture.
import { marketFixture } from './market-fixture.mjs';
const body = readFileSync(new URL('../../../fe/web/src/features/report/recipe/examples/portfolio.md', import.meta.url), 'utf8');
const parsed = parse(body);
if (parsed.status !== 'ready') throw new Error('Invalid test Recipe');
const nodes = parsed.value.children;
const blocks = () => nodes.map((node, i) => node.type === 'code' && node.language === 'neige-block'
  ? { id: `b-${i}`, kind: 'layout', payload: JSON.parse(node.value) }
  : { id: `b-${i}`, kind: 'prose', payload: { markdown: body.slice(node.position.start.offset, node.position.end.offset) } });

test('native authentication precedes private reads', async ({ page }) => {
  const reads: string[] = [];
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (!path.startsWith('/api/')) return route.continue();
    reads.push(path);
    await route.fulfill({ status: 401, json: { error: 'Unauthorized', code: 'unauthorized' } });
  });
  await page.goto('/next/track/market-track?market=1');
  await expect(page.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
  expect(reads).toEqual(['/api/auth/whoami']);
});

test('saved layouts and two portfolios render independently and recipe writes reach the server', async ({ page }) => {
  const now = Date.now();
  const area = { id: 'a', name: '投资', color: '#567', sort: 1, kind: 'user', created_at: now, updated_at: now };
  const track = { id: 'market-track', area_id: 'a', title: '我的组合', sort: 1, lifecycle: 'working', cwd: '/workspace', archived_at: null,
    pinned_at: null, terminal_at: null, created_at: now, updated_at: now };
  const first = blocks(), second = blocks();
  first.filter(b => b.kind === 'layout')[1]!.payload.items[0].data.annotations.rows = [{ venue: 'US', asset: 'AAA', name: '研究资产', track: 'research-us', nextEvent: '下周复盘' }];
  const secondOverlays = marketFixture('second').overlays;
  secondOverlays[0].payload.rows[0].asset = 'SECOND';
  let savedRecipe: Record<string, unknown> | null = null;
  const detail = (id: string, saved: unknown[], overlays: unknown[]) => ({ track: { ...track, id }, can_resume: false, overlays,
    cards: [{ id: `report-${id}`, track_id: id, kind: 'track-report', title: null, sort: 0, deletable: false, created_at: now, updated_at: now,
      payload: { schemaVersion: 3, docRev: 42, summary: '已保存', body, blocks: saved } }] });
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (!path.startsWith('/api/')) return route.continue();
    if (path === '/api/track-recipes' && route.request().method() === 'POST') {
      savedRecipe = { ...route.request().postDataJSON(), id: 'saved-recipe', revision: 1, created_at: now, updated_at: now };
      return route.fulfill({ status: 201, json: savedRecipe });
    }
    const data: Record<string, unknown> = {
      '/api/auth/whoami': { userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'test-session' },
      '/api/version': { webCompatVersion: 24, minWebCompatVersion: 24, syncEventVersion: 1, dbInstanceId: 'portfolio-test' },
      '/api/areas': [area], '/api/areas/a/tracks': [track, { ...track, id: 'second' }], '/api/settings': {},
      '/api/tracks/market-track': detail('market-track', first, marketFixture().overlays),
      '/api/tracks/second': detail('second', second, secondOverlays),
      '/api/tracks/research-us': detail('research-us', [{ id: 'research', kind: 'prose', payload: { markdown: '# 原有投资逻辑' } }], marketFixture('research-us').overlays),
      '/api/track-recipes': savedRecipe ? [savedRecipe] : [], '/api/track-recipes/saved-recipe': savedRecipe,
    };
    if (path.endsWith('/report')) return route.fulfill({ json: { taskDiagnostics: [] } });
    if (path.endsWith('/backlinks')) return route.fulfill({ json: { backlinks: [], truncated: false, skipped_sources: 0 } });
    await route.fulfill({ json: data[path] ?? [] });
  });
  await page.goto('/next/track/market-track?market=1');
  await expect(page.getByRole('table').first()).toContainText('10.00 USD');
  await expect(page.getByRole('table').first()).toContainText('79.5%');
  await expect(page.getByRole('table').first()).toContainText('下周复盘');
  await expect(page.locator('.recharts-area-curve')).toBeVisible();
  await expect(page.locator('.recharts-pie-sector')).toHaveCount(2);
  await expect(page.locator('iframe')).toHaveCount(0);
  await page.getByRole('button', { name: '研究资产', exact: true }).click();
  await expect(page.getByRole('heading', { name: '原有投资逻辑', exact: true })).toBeVisible();
  await expect(page.locator('.recharts-pie-sector')).toHaveCount(0);
  await page.goto('/next/track/second?market=1');
  await expect(page.getByRole('table').first()).toContainText('SECOND');
  await expect(page.locator('.recharts-pie-sector')).toHaveCount(2);
  // Actual CAS writes are exercised by backend tests. No frontend projection
  // may replace the changed persisted configuration on a subsequent read.
  first.filter(b => b.kind === 'layout')[0]!.payload.items.reverse();
  first.filter(b => b.kind === 'layout')[0]!.payload.items[0].title = '我调整后的权重';
  await page.goto('/next/track/market-track?market=1');
  await page.reload();
  await expect(page.getByRole('region', { name: '我调整后的权重', exact: true })).toBeVisible();
  await page.goto('/next/recipes?market=1');
  await page.getByRole('button', { name: '投资组合模板', exact: true }).click();
  await expect(page.getByRole('textbox', { name: 'Recipe title', exact: true })).toHaveValue('投资组合');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toBeVisible();
  expect(savedRecipe).toMatchObject({ title: '投资组合', body });
});
