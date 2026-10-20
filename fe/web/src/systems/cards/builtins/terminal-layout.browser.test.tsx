import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page as browserPage } from 'vitest/browser';

import '../../../styles/entry.css';
import { Drawer } from '../../../ui/drawer/public.tsx';
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
      runtime: { worker_session_id: 'run-1', kind: 'terminal', status, terminal_id: 'pty-1' },
    });
    if (card === null) throw new Error('Missing terminal');
    return <BoardHost host={host} items={[
      { card, title: 'Terminal', originalIndex: 0, deletable: true, activity: null, notice: null },
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
  expect(document.querySelector('[data-nc-card-cell] [data-nc-activity]')).toBeNull();
  expect(screen.getByTestId('retained-output')).toBe(output);
  expect(getComputedStyle(body).padding).toBe('0px');
  expect(getComputedStyle(body).display).toBe('flex');
  expect(getComputedStyle(body).overflow).toBe('hidden');
  const after = body.getBoundingClientRect();
  expect(after.width).toBeCloseTo(before.width, 1);
  expect(after.height).toBeCloseTo(before.height, 1);
});

for (const theme of ['light', 'dark'] as const) {
  it(`matches conversation card corners and shadow in ${theme} theme`, async () => {
    await browserPage.viewport(1200, 800);
    const previousTheme = document.documentElement.getAttribute('data-theme');
    document.documentElement.setAttribute('data-theme', theme);
    try {
      const registry = createCardRegistry();
      registerAvailableBuiltinCards(registry);
      const card = registry.resolve({ id: 'chrome-terminal', kind: 'terminal', payload: {} });
      if (card === null) throw new Error('Missing terminal');
      render(<div style={{ position: 'relative', width: 1100, height: 700, containerType: 'inline-size' }}>
        <BoardHost host={createCardHost(registry)} items={[
          { card, title: 'Terminal', originalIndex: 0, activity: null, notice: null },
        ]} visible activeCardId={card.id} />
        <Drawer open title="Conversation" onClose={() => {}}>Conversation content</Drawer>
      </div>);
      const terminal = await waitFor(() => {
        const element = document.querySelector('[data-nc-terminal-card]');
        expect(element).not.toBeNull();
        return element!;
      });
      const expected = getComputedStyle(document.querySelector('[data-nc-drawer]')!);
      const actual = getComputedStyle(terminal);
      expect(expected.borderRadius).toBe('16px');
      expect(expected.boxShadow).not.toBe('none');
      expect(actual.borderRadius).toBe(expected.borderRadius);
      expect(actual.boxShadow).toBe(expected.boxShadow);
      expect(actual.borderWidth).toBe(expected.borderWidth);
      await browserPage.screenshot({ path: `../../../../test-results/terminal-conversation-${theme}.png` });
    } finally {
      if (previousTheme === null) document.documentElement.removeAttribute('data-theme');
      else document.documentElement.setAttribute('data-theme', previousTheme);
    }
  });
}
