import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page as browserPage } from 'vitest/browser';

import '../../../styles/entry.css';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { registerAvailableBuiltinCards } from './register.ts';

// The PTY transport is outside this layout regression. Keep its mounted leaf
// observable while exercising the production board, card and stylesheets.
vi.mock('../../terminal/surface.tsx', () => ({
  TerminalSurface: () => <div data-testid="retained-output">Last terminal output</div>,
}));

afterEach(cleanup);

it('preserves attached terminal geometry and output when its session exits', async () => {
  // The Grid is a desktop surface; mobile routes do not offer it.
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const host = createCardHost(registry);
  const board = (status: 'running' | 'exited') => {
    const card = registry.resolve({
      id: 'card-1', kind: 'terminal',
      payload: { cwd: '/repo/worker-checkout', gate_cwd: '/repo/gate-checkout' },
      runtime: { runtime_id: 'run-1', kind: 'terminal', status, terminal_id: 'pty-1' },
    });
    if (card === null) throw new Error('Missing terminal');
    return <BoardHost host={host} items={[
      { card, title: 'Terminal', originalIndex: 0, deletable: true },
    ]} visible activeCardId="card-1" />;
  };
  const { rerender } = render(board('running'));
  const output = screen.getByTestId('retained-output');
  const body = output.parentElement!;
  await waitFor(() => expect(getComputedStyle(body).padding).toBe('0px'));
  const before = body.getBoundingClientRect();
  expect(getComputedStyle(body).display).toBe('flex');
  expect(getComputedStyle(body).overflow).toBe('hidden');

  rerender(board('exited'));
  expect(screen.getByText('Session exited.')).toBeTruthy();
  expect(screen.getByText('/repo/worker-checkout')).toBeTruthy();
  expect(screen.getByText('/repo/gate-checkout')).toBeTruthy();
  expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
  expect(screen.getByTestId('retained-output')).toBe(output);
  expect(getComputedStyle(body).padding).toBe('0px');
  expect(getComputedStyle(body).display).toBe('flex');
  expect(getComputedStyle(body).overflow).toBe('hidden');
  const after = body.getBoundingClientRect();
  expect(after.width).toBeCloseTo(before.width, 1);
  expect(after.height).toBeCloseTo(before.height, 1);
});
