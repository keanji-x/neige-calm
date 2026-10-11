// @vitest-environment jsdom
//
// #2516: a page that stays focused still learns what the server's own 10-minute check found.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import type { ClaudeCardPort } from '../../../../../core/domain/claude-card.ts';
import type { CardHostCapabilities } from '../contracts.ts';
import { CLAUDE_CARD_ENTRY } from './claude.ts';

vi.mock('../../terminal/surface.tsx', () => ({ TerminalSurface: () => null }));

afterEach(() => { cleanup(); vi.useRealTimers(); });

it('re-reads the installed version every 10 minutes while the page stays focused, and shows Update', async () => {
  vi.useFakeTimers();
  const answers = ['2.1.259', '2.1.280'];
  const port = {
    installedVersion: vi.fn(() => Promise.resolve({ version: answers.shift() ?? '2.1.280' })),
    refreshInstalledVersion: vi.fn(() => Promise.resolve()),
    restart: vi.fn(() => new Promise<never>(() => {})),
  } satisfies ClaudeCardPort;
  const host = {
    cardId: 'c1',
    lifecycle: {
      getSnapshot: () => ({ visible: true, focused: true, geometry: { width: 0, height: 0, ready: false }, refreshEpoch: 0 }),
      subscribe: () => () => {},
    },
    slots: { get: () => undefined, set: () => {} },
    files: null, recovery: null, claude: port, emit: vi.fn(),
  } as unknown as CardHostCapabilities;
  const Component = CLAUDE_CARD_ENTRY.component;
  render(<QueryClientProvider client={new QueryClient()}>
    <Component
      card={{
        type: 'claude', id: 'c1', title: null, terminalId: 't1', sessionState: 'running', cwd: null, gateCwd: null,
        ownerCreated: true, workerSessionId: 'w1', runningVersion: '2.1.259',
      }}
      host={host} activity={null} notice={null}
    />
  </QueryClientProvider>);
  await act(() => vi.advanceTimersByTimeAsync(0));
  expect(port.installedVersion).toHaveBeenCalledTimes(1);
  expect(document.querySelector('[data-nc-claude-card-action]')).toBeNull();

  await act(() => vi.advanceTimersByTimeAsync(11 * 60 * 1000));
  expect(port.installedVersion).toHaveBeenCalledTimes(2);
  expect(port.refreshInstalledVersion).not.toHaveBeenCalled();
  expect(document.querySelector('[data-nc-claude-card-action]')?.textContent).toContain('Update');
});
