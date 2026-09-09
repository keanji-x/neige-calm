import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import { recipePreviewSchema } from '../../../../../core/domain/recipe-preview.ts';
import type { ReportLayout } from '../../../../../core/domain/report-layout.ts';
import { RecipeEditor, type RecipeDraft, type RecipeWriteOutcome } from './public.tsx';

afterEach(cleanup);

function fixture(revision: number) {
  const charts: ReportLayout = { version: 1, columns: 2, gap: 'wide', surface: 'plain', items: [
    { kind: 'chart', title: '组合走势', span: 1, chart: 'line', height: 240, color: '#4a5f9b', x: 'at', y: 'total',
      data: { source: 'neige://plugin/market/history' }, ranges: [30, 90], defaultRange: 90 },
    { kind: 'chart', title: '持仓权重', span: 1, chart: 'donut', height: 220, color: '#4a5f9b', x: 'asset', y: 'value',
      data: { source: 'neige://plugin/market/holdings' } },
  ] };
  const table = (title: string, label: string): ReportLayout => ({ version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [
    { kind: 'table', title, span: 1, data: { rows: [] }, columns: [{ key: 'label', label, format: 'text', digits: 0 }] },
  ] });
  const layouts = [charts, table('持仓', '股票'), table('日志', '交易事件')];
  if (revision > 1) layouts.reverse();
  // Input fixture serialization only. The component receives a typed server
  // projection; it never parses these fences. Rust tests cover the compiler.
  const body = layouts.map(layout => `\`\`\`neige-block layout\n${JSON.stringify(layout, null, 2)}\n\`\`\``).join('\n\n');
  const recipe = { id: 'saved-preview', revision, title: revision === 1 ? '投资组合' : '调整后的组合', body, created_at: 1, updated_at: revision };
  const preview = recipePreviewSchema.parse({ id: recipe.id, revision, payload: { summary: recipe.title, body,
    blocks: layouts.map((payload, index) => ({ id: `saved-${revision}-${index}`, rev: 1, kind: 'layout', payload })),
  } });
  return { recipe, preview };
}

it('renders saved native tables and honest chart frames, then uses the saved title/order revision', async () => {
  await page.viewport(1200, 1000);
  const user = userEvent.setup();
  const before = fixture(1);
  const after = fixture(2);
  const write = vi.fn<(draft: RecipeDraft) => Promise<RecipeWriteOutcome>>().mockResolvedValue({ kind: 'saved', recipe: after.recipe });
  const load = vi.fn((_id: string, revision: number) => Promise.resolve({ kind: 'ready' as const, preview: revision === 1 ? before.preview : after.preview }));
  const { container } = render(<RecipeEditor recipe={before.recipe} theme="light" onWrite={write} onPreview={load}
    onDelete={null} onClose={() => {}} onCreated={null}/>);
  const line = await screen.findByRole('figure', { name: '组合走势 · 折线图' }, { timeout: 5_000 });
  const donut = await screen.findByRole('figure', { name: '持仓权重 · 环形图' }, { timeout: 5_000 });
  expect(line.getBoundingClientRect().height).toBe(240);
  expect(donut.getBoundingClientRect().height).toBe(220);
  expect(within(line).getByRole('status').textContent).toContain('等待数据');
  expect(screen.getAllByRole('table')).toHaveLength(2);
  expect(container.querySelector('[data-nc-recipe-rendered] pre')).toBeNull();
  expect(container.querySelector('iframe')).toBeNull();
  await page.screenshot({ path: '../../../../../test-results/saved-recipe-native-preview.png' });

  await user.click(screen.getByRole('button', { name: 'Edit' }));
  await user.clear(screen.getByRole('textbox', { name: 'Recipe title' }));
  await user.type(screen.getByRole('textbox', { name: 'Recipe title' }), after.recipe.title);
  await page.getByRole('textbox', { name: 'Recipe body, Markdown' }).fill(after.recipe.body);
  await user.click(screen.getByRole('button', { name: 'Save' }));
  expect(write.mock.calls[0][0]).toEqual({ title: after.recipe.title, body: after.recipe.body, if_revision: 1 });
  expect(await screen.findByRole('heading', { name: after.recipe.title, level: 1 })).toBeTruthy();
  await screen.findByRole('figure', { name: '组合走势 · 折线图' }, { timeout: 5_000 });
  expect(within(screen.getAllByRole('table')[0]).getByRole('columnheader', { name: '交易事件' })).toBeTruthy();
  expect(load.mock.calls.some(call => call[1] === 2)).toBe(true);
});

it('keeps a saved app block inert through the actual Recipe preview composition', async () => {
  const { recipe } = fixture(1);
  const preview = recipePreviewSchema.parse({ id: recipe.id, revision: recipe.revision, payload: {
    summary: recipe.title, body: 'Saved app', blocks: [{ id: 'app', kind: 'app', payload: { src: '/preview-resource-must-not-load', title: '应用配置', height: 160 } }],
  } });
  const { container } = render(<RecipeEditor recipe={recipe} theme="light" onPreview={() => Promise.resolve({ kind: 'ready', preview })}
    onWrite={() => Promise.resolve({ kind: 'saved', recipe })} onDelete={null} onClose={() => {}} onCreated={null}/>);
  expect(await screen.findByText('嵌入应用在此预览中不加载。')).toBeTruthy();
  expect(container.querySelector('iframe')).toBeNull();
});
