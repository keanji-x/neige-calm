import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import type { ModelCatalog } from '../../../../core/domain/conversation.ts';
import { renderPlannerSwitchFixture } from './planner-switch-fixture.tsx';

beforeEach(async () => { await page.viewport(1280, 900); await commands.emulateReducedMotion(true); });
afterEach(async () => {
  cleanup(); document.getElementById('root')?.remove(); document.documentElement.style.removeProperty('font-size');
  delete document.documentElement.dataset.theme;
  await commands.emulateReducedMotion(false);
});

it.each([[224, 16], [280, 16], [280, 20], [280, 32]])('keeps real Planner composer controls usable in a %ipx desktop sidebar at %ipx text', async (width, font) => {
  document.documentElement.style.fontSize = `${font}px`;
  const efforts = [{ reasoning_effort: 'low', description: null }, { reasoning_effort: 'high', description: null }];
  const model = 'gpt-6-astra-with-a-deliberately-long-identifier';
  const catalog: ModelCatalog = { models: [{ id: 'model', model, resolved_model: null, display_name: 'A deliberately long model name', description: '',
    is_default: true, supported_reasoning_efforts: efforts, default_reasoning_effort: 'high' }],
    default: { model, reasoning_effort: 'high', supported_reasoning_efforts: efforts }, default_source: 'config_read', source: 'live', fetched_at_ms: 1 };
  renderPlannerSwitchFixture({ reply: request => request.path.startsWith('/api/models?') ? { status: 200, statusText: 'OK', body: catalog } : undefined });
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  const input = page.getByRole('combobox', { name: 'Message' });
  await expect.element(input).toBeVisible();
  await expect.element(page.getByRole('button', { name: /^Reasoning effort:/ })).toBeVisible();
  await expect.element(page.getByRole('button', { name: /^Model:/ })).toHaveAccessibleName(new RegExp(model));
  const root = document.querySelector<HTMLElement>('[data-nc-composer]')!;
  // Geometry input only: preserve a desktop viewport while its conversation column narrows.
  const restingWidth = root.style.inlineSize;
  root.style.inlineSize = `${width}px`;
  await input.fill('A long draft with a link https://example.invalid/' + 'path-'.repeat(24));
  const controls = [page.getByRole('button', { name: /^Model:/ }), page.getByRole('button', { name: /^Reasoning effort:/ }),
    page.getByRole('button', { name: /^Approvals:/ }), page.getByRole('button', { name: /^Role:/ }), page.getByRole('button', { name: 'Send', exact: true })];
  const check = () => {
    const bounds = root.getBoundingClientRect();
    for (const control of controls) {
      const button = control.element(); const box = button.getBoundingClientRect();
      expect(box.width).toBeGreaterThan(20);
      expect(box.left).toBeGreaterThanOrEqual(bounds.left - 1);
      expect(box.right).toBeLessThanOrEqual(bounds.right + 1);
      const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
      expect(hit === button || (hit !== null && button.contains(hit))).toBe(true);
    }
    expect(input.element().scrollWidth).toBeLessThanOrEqual(input.element().clientWidth + 1);
  };
  check();
  expect(controls[0].element().getBoundingClientRect().width).toBeGreaterThan(40);
  await controls[0].click(); await expect.element(page.getByRole('menu')).toBeVisible(); await userEvent.keyboard('{Escape}');
  await expect.element(controls[0]).toHaveFocus();
  root.style.inlineSize = restingWidth; check();
  expect(input.element().textContent).toContain('https://example.invalid/');
});
