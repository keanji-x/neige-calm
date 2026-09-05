import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import type { CardHostCapabilities } from '../contracts.ts';
import { CLAUDE_CARD_ENTRY } from './claude.ts';

afterEach(cleanup);

it('keeps the worker checkout readable inside a narrow card', async () => {
  await page.viewport(390, 844);
  const cwd = '/repo/.claude/worktrees/01234567890123456789012345678901/01234567890123456789012345678901';
  const card = CLAUDE_CARD_ENTRY.fromKernel({
    id: 'c1', kind: 'claude', payload: { cwd, gate_cwd: '/repo/explicit-gate-checkout' },
    runtime: { runtime_id: 'worker-runtime', kind: 'claude', status: 'exited' },
  });
  if (card === null) throw new Error('Claude worker must resolve');
  const host = {
    lifecycle: {
      getSnapshot: () => ({ visible: true, focused: false, geometry: { w: 390, h: 844 }, refresh: 0 }),
      subscribe: () => () => {},
    },
    slots: { use: () => [{ current: null }] },
    emit: vi.fn(),
  } as unknown as CardHostCapabilities;
  const Component = CLAUDE_CARD_ENTRY.component;
  render(<Component card={card} host={host} />);
  await expect.element(page.getByText('Working directory', { exact: true })).toBeVisible();
  await expect.element(page.getByText(cwd)).toBeVisible();
  await expect.element(page.getByText('Gate working directory')).toBeVisible();
  await expect.element(page.getByText('/repo/explicit-gate-checkout')).toBeVisible();
  await expect.element(page.getByText('Session exited.')).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  await page.screenshot();
});
