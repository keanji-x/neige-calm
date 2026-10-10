// #2516: an owner-created Claude card's Update and Resume, in the head of the real board with the production stylesheets.
// The PTY transport is outside this test: the surface is a leaf that counts its mounts, one per attach.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page as browserPage } from 'vitest/browser';

import '../../../styles/entry.css';
import type { CardRuntimeView } from '../../../../../core/api/schemas.ts';
import type { ClaudeCardPort } from '../../../../../core/domain/claude-card.ts';
import { ApiError } from '../../../../../core/domain/failure-class.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { registerAvailableBuiltinCards } from './register.ts';

const mounts = vi.hoisted(() => ({ count: 0 }));
vi.mock('../../terminal/surface.tsx', async () => {
  const { useEffect } = await import('react');
  return {
    TerminalSurface: () => {
      useEffect(() => { mounts.count += 1; }, []);
      return <div data-testid="pty">terminal output</div>;
    },
  };
});

afterEach(() => { cleanup(); mounts.count = 0; });

type CardCase = Readonly<{ id: string; owner: boolean; status: CardRuntimeView['status']; version?: string; runtime?: string }>;

function wire({ id, owner, status, version, runtime = `rt-${id}` }: CardCase): CardWire {
  return {
    id, track_id: 'w1', kind: 'claude', title: null, sort: 1, deletable: true, created_at: 1, updated_at: 2,
    payload: owner ? { owner_created: true } : {},
    runtime: {
      worker_session_id: runtime, kind: 'claude', status, terminal_id: `pty-${id}`,
      ...(version === undefined ? {} : { claude_version: version }),
    },
  };
}

function fakePort(installed: readonly string[], restart: ClaudeCardPort['restart'] = () => new Promise(() => {})) {
  let reads = 0;
  return {
    installedVersion: vi.fn(() => Promise.resolve({ version: installed[Math.min(reads++, installed.length - 1)] ?? null })),
    refreshInstalledVersion: vi.fn(() => Promise.resolve()),
    restart: vi.fn(restart),
  } satisfies ClaudeCardPort;
}

function board(port: ClaudeCardPort, cases: readonly CardCase[]) {
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const host = createCardHost(registry, { claude: port });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const view = (current: readonly CardCase[]) => <QueryClientProvider client={client}>
    <BoardHost host={host} visible activeCardId={null} onRemoveCard={() => undefined} items={current.map((cardCase, index) => {
      const card = registry.resolve(wire(cardCase));
      if (card === null) throw new Error(`unresolved ${cardCase.id}`);
      return { card, title: 'Claude', originalIndex: index, deletable: true, activity: null, notice: null };
    })} />
  </QueryClientProvider>;
  const rendered = render(view(cases));
  return { rerender: (next: readonly CardCase[]) => rendered.rerender(view(next)) };
}

function cell(id: string): HTMLElement {
  const found = Array.from(document.querySelectorAll<HTMLElement>('[data-nc-card-id]'))
    .find((element) => element.dataset.ncCardId === id);
  if (found === undefined) throw new Error(`no cell ${id}`);
  return found;
}

function head(id: string): HTMLElement {
  return cell(id).querySelector<HTMLElement>('[data-nc-card-drag]')!;
}

function action(id: string): HTMLButtonElement | null {
  return cell(id).querySelector<HTMLButtonElement>('[data-nc-claude-card-action]');
}

describe('Claude card Update and Resume', () => {
  it('shows Update and Resume on owner cards only, as the state and versions say, at the right of the head', async () => {
    await browserPage.viewport(1600, 1000);
    board(fakePort(['2.1.280']), [
      { id: 'update', owner: true, status: 'running', version: '2.1.259' },
      { id: 'current', owner: true, status: 'running', version: '2.1.280' },
      { id: 'resume', owner: true, status: 'exited', version: '2.1.259' },
      { id: 'worker-exited', owner: false, status: 'exited', version: '2.1.259' },
      { id: 'worker-old', owner: false, status: 'running', version: '2.1.259' },
    ]);
    await waitFor(() => expect(action('update')?.textContent).toBe('Update'));
    expect(action('resume')?.textContent).toBe('Resume');
    expect(action('current')).toBeNull();
    expect(action('worker-exited')).toBeNull();
    expect(action('worker-old')).toBeNull();

    // Top right: in the head, right of the title, clear of the close button.
    const updateHead = head('update');
    const button = action('update')!.getBoundingClientRect();
    const title = within(updateHead).getByText('Claude').getBoundingClientRect();
    const close = within(updateHead).getByRole('button', { name: 'Delete card Claude' }).getBoundingClientRect();
    const box = updateHead.getBoundingClientRect();
    expect(button.top).toBeGreaterThanOrEqual(box.top);
    expect(button.bottom).toBeLessThanOrEqual(box.bottom);
    expect(button.left).toBeGreaterThan(title.right);
    expect(button.right).toBeLessThanOrEqual(close.left);
    expect(button.left).toBeGreaterThan(box.left + box.width / 2);
  });

  it('runs one restart per press, shows it pending, then attaches to the new runtime', async () => {
    let finish: (card: CardWire) => void = () => {};
    const port = fakePort(['2.1.280'], () => new Promise<CardWire>((resolve) => { finish = resolve; }));
    const running = { id: 'c1', owner: true, status: 'running', version: '2.1.259' } as const;
    const { rerender } = board(port, [running]);
    await waitFor(() => expect(action('c1')?.textContent).toBe('Update'));
    expect(mounts.count).toBe(1);

    // Two presses before the pending state renders: only the first may send.
    const button = action('c1')!;
    act(() => { button.click(); button.click(); });
    expect(port.restart).toHaveBeenCalledTimes(1);
    expect(port.restart).toHaveBeenCalledWith('c1');
    expect(action('c1')?.textContent).toContain('Updating…');

    const restarted = { ...running, version: '2.1.280', runtime: 'rt-c1-next' } as const;
    await act(() => { finish(wire(restarted)); return Promise.resolve(); });
    rerender([restarted]);
    await waitFor(() => expect(action('c1')).toBeNull());
    expect(mounts.count).toBe(2);
  });

  it('says why a restart was refused, in the head', async () => {
    const refused = new ApiError({ kind: 'http', status: 409, code: 'conflict', message: 'a task owns its terminal' });
    const port = fakePort(['2.1.280'], () => Promise.reject(refused));
    board(port, [{ id: 'c1', owner: true, status: 'exited', version: '2.1.259' }]);
    await waitFor(() => expect(action('c1')?.textContent).toBe('Resume'));
    await userEvent.click(action('c1')!);
    const alert = await waitFor(() => {
      const shown = head('c1').querySelector<HTMLElement>('[role="alert"]');
      expect(shown?.textContent).toBe('a task owns its terminal');
      return shown!;
    });
    expect(head('c1').contains(alert)).toBe(true);
    expect(action('c1')?.textContent).toBe('Resume');
  });

  it('asks the server to read the CLI again when the page regains focus, then shows Update from its answer', async () => {
    const port = fakePort(['2.1.259', '2.1.280']);
    board(port, [{ id: 'c1', owner: true, status: 'running', version: '2.1.259' }]);
    await waitFor(() => expect(port.installedVersion).toHaveBeenCalledTimes(1));
    expect(action('c1')).toBeNull();
    expect(port.refreshInstalledVersion).not.toHaveBeenCalled();

    act(() => { window.dispatchEvent(new Event('focus')); });
    await waitFor(() => expect(action('c1')?.textContent).toBe('Update'));
    expect(port.refreshInstalledVersion).toHaveBeenCalledTimes(1);
    expect(port.installedVersion).toHaveBeenCalledTimes(2);
    expect(port.refreshInstalledVersion.mock.invocationCallOrder[0])
      .toBeLessThan(port.installedVersion.mock.invocationCallOrder[1]);
  });
});
