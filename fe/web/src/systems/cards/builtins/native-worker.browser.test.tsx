import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import type { CardHostCapabilities } from '../contracts.ts';
import { CODEX_CARD_ENTRY } from './codex.ts';

afterEach(cleanup);

it('updates a narrow native review card without a terminal or input surface', async () => {
  await page.viewport(390, 844);
  const host = {
    lifecycle: {
      getSnapshot: () => ({ visible: true, focused: false, geometry: { w: 390, h: 844 }, refresh: 0 }),
      subscribe: () => () => {},
    },
    slots: { use: () => [{ current: null }] },
    emit: vi.fn(),
  } as unknown as CardHostCapabilities;
  const resolve = (status: string, report: unknown) => {
    const card = CODEX_CARD_ENTRY.fromKernel({ id: 'review', kind: 'codex', payload: {
      cwd: '/repo/.claude/worktrees/01234567890123456789012345678901/readonly-review',
      worker_presentation: { kind: 'native_only' },
      worker_snapshot: { task_id: 'review-task', goal: 'Review shared reader and writer lifetimes', status, report },
    } });
    if (card === null) throw new Error('valid native review must resolve');
    return card;
  };
  const Component = CODEX_CARD_ENTRY.component;
  const view = render(<Component card={resolve('running', { kind: 'pending' })} host={host} activity={null} />);
  await expect.element(page.getByRole('status')).toHaveTextContent('Reviewing…');
  view.rerender(<Component card={resolve('done', {
    kind: 'reported', outcome: 'completed', result: 'No blocking findings. Reader lifetimes remain protected until native work stops.',
  })} host={host} activity={null} />);
  await expect.element(page.getByRole('status')).toHaveTextContent('Review finished');
  await expect.element(page.getByText('No blocking findings.', { exact: false })).toBeVisible();
  expect(document.querySelector('[data-nc-terminal-card]')).toBeNull();
  expect(document.querySelector('input, textarea, [contenteditable="true"]')).toBeNull();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  await page.screenshot({ path: 'test-results/native-worker-review.png' });
});
