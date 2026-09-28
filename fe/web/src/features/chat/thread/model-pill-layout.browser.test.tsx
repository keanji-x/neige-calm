// The Claude group's row layout in a real browser (#1822, owner layout 2026-09-28): one line per row,
// the name at the left and the resolved model small at the right, which truncates rather than overflow
// a phone.
import '../../../styles/entry.css';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import { FOLLOW_INSTALLATION_DEFAULT, type ModelCatalog } from '../../../../../core/domain/conversation.ts';
import { ModelPill } from './model-pill.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

const LONG = 'claude-opus-5-5-with-an-unusually-long-resolved-identifier-20260928[1m]';
const levels = [{ reasoning_effort: 'low', description: null }];

function entry(value: string, resolved: string, name: string): ModelCatalog['models'][number] {
  return { id: value, model: value, resolved_model: resolved, display_name: name, description: '', is_default: false,
    supported_reasoning_efforts: levels, default_reasoning_effort: null };
}

const CATALOG: ModelCatalog = {
  models: [entry('opus[1m]', LONG, 'Opus (1M context)'), entry('haiku', 'claude-haiku-4-5-20251001', 'Haiku')],
  default: { model: LONG, reasoning_effort: null, supported_reasoning_efforts: levels },
  default_source: 'claude_cli', source: 'live', fetched_at_ms: 1,
};

it.each([390, 1280])('each Claude row is one line, its resolved id at the right and cut before the menu overflows (%ipx)', async (width) => {
  await page.viewport(width, 844);
  render(<ModelPill provider="claude" selection={FOLLOW_INSTALLATION_DEFAULT} onChange={vi.fn()}
    groups={[{ provider: 'claude', availability: null, catalog: CATALOG }]} />);
  await userEvent.click(screen.getByRole('button', { name: /^Model:/ }));
  const menu = await screen.findByRole('menu');
  const bounds = menu.getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(width);
  const rights: number[] = [];
  for (const [name, resolved] of [['Default', LONG], ['Opus (1M context)', LONG], ['Haiku', 'claude-haiku-4-5-20251001']] as const) {
    const row = within(menu).getByRole('menuitem', { name: new RegExp(`^${name.replace(/[()]/g, '\\$&')}`) });
    const label = within(row).getByText(name, { exact: true });
    const id = within(row).getByTitle(resolved);
    const [labelBox, idBox, rowBox] = [label.getBoundingClientRect(), id.getBoundingClientRect(), row.getBoundingClientRect()];
    /* One line: the id sits beside the name, not beneath it, and ends at the row's right. */
    expect(Math.abs(idBox.top - labelBox.top)).toBeLessThan(4);
    expect(idBox.left).toBeGreaterThanOrEqual(labelBox.right);
    expect(idBox.right).toBeLessThanOrEqual(rowBox.right);
    /* The selected row's check mark takes its own room at the far right. */
    if (!(row.textContent ?? '').includes('Selected')) rights.push(idBox.right);
    expect(label.scrollWidth).toBeLessThanOrEqual(label.clientWidth);
  }
  /* Right-aligned: every id ends at the same edge, whatever the name before it. */
  expect(rights).toHaveLength(2);
  expect(Math.max(...rights) - Math.min(...rights)).toBeLessThan(1);
  /* The long id gives way with an ellipsis: its box is narrower than its text. */
  const long = within(menu).getAllByTitle(LONG)[0];
  expect(long.scrollWidth).toBeGreaterThan(long.clientWidth);
  expect(getComputedStyle(long).textOverflow).toBe('ellipsis');
  expect(document.documentElement.scrollWidth).toBe(width);
});
