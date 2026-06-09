import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { components } from '../../api/generated';
import type { KernelCard } from '../../api/wire';

const mocks = vi.hoisted(() => {
  type Listener = (ev: { ev: string; data: Record<string, unknown> }) => void;
  const fakeStream = {
    listeners: new Set<Listener>(),
    addTopic: vi.fn(),
    on(fn: Listener) {
      this.listeners.add(fn);
      return () => {
        this.listeners.delete(fn);
      };
    },
    emit(ev: { ev: string; data: Record<string, unknown> }) {
      for (const fn of this.listeners) fn(ev);
    },
    reset() {
      this.listeners.clear();
      this.addTopic.mockClear();
    },
  };
  return {
    fakeStream,
    resetSpecCard: vi.fn(),
  };
});

vi.mock('../../api/events', () => ({
  sharedEventStream: () => mocks.fakeStream,
}));

vi.mock('../../api/calm', () => ({
  createClaudeCard: vi.fn(),
  createCodexCard: vi.fn(),
  getTerminalForCard: vi.fn(),
  resetSpecCard: mocks.resetSpecCard,
}));

vi.mock('../overlayRegistry', () => ({
  useCardStatusOverlay: () => ({ state: 'running' }),
}));

import { CodexEntry } from './codex';
import { SpecEntry } from './spec';
import {
  __resetRegistryForTest,
  CardInstanceProvider,
  registerCard,
} from '../registry';

type HarnessItem = components['schemas']['HarnessItem'];

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'card_spec_1',
    wave_id: 'wave_1',
    kind: 'codex',
    sort: 0,
    payload: {
      schemaVersion: 1,
      spec_harness: true,
      prompt: 'Ship the spec UI',
      icon_bg: '#123456',
      icon_fg: '#ffffff',
    },
    deletable: false,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

function harnessRow(
  id: number,
  item_type: string,
  item: Record<string, unknown>,
  over: Partial<HarnessItem> = {},
): HarnessItem {
  return {
    id,
    runtime_id: 'runtime_1',
    card_id: 'card_spec_1',
    wave_id: 'wave_1',
    thread_id: 'thread_1',
    turn_id: 'turn_1',
    item_uuid: typeof item.id === 'string' ? item.id : `item_${id}`,
    item_type,
    method: 'item/completed',
    params: JSON.stringify({ item: { id: `item_${id}`, type: item_type, ...item } }),
    created_at_ms: 1000 + id,
    ...over,
  };
}

function response(rows: HarnessItem[]): Response {
  return new Response(JSON.stringify(rows), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function renderSpecCard(rows: HarnessItem[]) {
  const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(response(rows)));
  vi.stubGlobal('fetch', fetchMock);
  const Component = SpecEntry.Component;
  render(
    <Component
      card={{
        type: 'spec',
        id: 'card_spec_1',
        goal: 'Ship the spec UI',
      }}
    />,
  );
  return fetchMock;
}

function renderSpecCardWithProvider(rows: HarnessItem[] | HarnessItem[][]) {
  const fetchMock = Array.isArray(rows[0])
    ? vi.fn().mockImplementation(() => {
        const nextRows = (rows as HarnessItem[][]).shift() ?? [];
        return Promise.resolve(response(nextRows));
      })
    : vi.fn().mockImplementation(() => Promise.resolve(response(rows as HarnessItem[])));
  vi.stubGlobal('fetch', fetchMock);
  const Component = SpecEntry.Component;
  const card = {
    type: 'spec' as const,
    id: 'card_spec_1',
    goal: 'Ship the spec UI',
  };
  render(
    <CardInstanceProvider cardId="card_spec_1" deletable={false} card={card}>
      <Component card={card} />
    </CardInstanceProvider>,
  );
  return fetchMock;
}

beforeEach(() => {
  __resetRegistryForTest();
  registerCard(SpecEntry);
  mocks.fakeStream.reset();
  mocks.resetSpecCard.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('SpecEntry.fromKernel', () => {
  it('maps codex spec-harness payloads into spec cards', () => {
    const out = SpecEntry.fromKernel!(makeKernelCard());
    expect(out).toMatchObject({
      type: 'spec',
      id: 'card_spec_1',
      goal: 'Ship the spec UI',
      iconBg: '#123456',
      iconFg: '#ffffff',
    });
  });

  it('returns null for non-harness codex cards and non-codex cards', () => {
    expect(
      SpecEntry.fromKernel!(
        makeKernelCard({ payload: { schemaVersion: 1, terminal_id: 'term_1' } }),
      ),
    ).toBeNull();
    expect(
      SpecEntry.fromKernel!(
        makeKernelCard({ kind: 'terminal', payload: { terminal_id: 'term_1' } }),
      ),
    ).toBeNull();
  });

  it('rejects malformed spec harness payloads', () => {
    expect(
      SpecEntry.fromKernel!(
        makeKernelCard({ payload: { spec_harness: 'yes', prompt: 'bad' } }),
      ),
    ).toBeNull();
  });
});

describe('CodexEntry.fromKernel', () => {
  it('does not claim codex spec-harness cards', () => {
    expect(CodexEntry.fromKernel!(makeKernelCard())).toBeNull();
  });

  it('still claims regular codex cards', () => {
    const out = CodexEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 1, terminal_id: 'term_1', cwd: '/tmp' },
      }),
    );
    expect(out).toMatchObject({
      type: 'codex',
      id: 'card_spec_1',
      terminalId: 'term_1',
      cwd: '/tmp',
    });
  });
});

describe('SpecCard chat timeline', () => {
  it('renders supported harness item types without crashing on unknown items', async () => {
    renderSpecCard([
      harnessRow(1, 'userMessage', {
        id: 'user_1',
        content: [{ text: 'please update the PR UI' }],
      }),
      harnessRow(2, 'agent_message', {
        id: 'agent_1',
        text: 'assistant hello',
      }),
      harnessRow(3, 'reasoning', {
        id: 'reason_1',
        text: 'private chain summary',
      }),
      harnessRow(4, 'function_call', {
        id: 'call_1',
        name: 'lookup',
        arguments: { query: 'spec docs' },
      }),
      harnessRow(5, 'function_call_output', {
        id: 'out_1',
        output: 'tool output body',
      }),
      harnessRow(6, 'web_search', {
        id: 'search_1',
        query: 'playwright fixtures',
      }),
      harnessRow(7, 'local_shell', {
        id: 'shell_1',
        command: 'echo hi',
        output: 'hi',
      }),
      harnessRow(8, 'mystery_item', {
        id: 'mystery_1',
      }),
      harnessRow(9, 'mcpToolCall', {
        id: 'mcp_1',
        server: 'neige',
        tool: 'spec_set_phase',
        status: 'completed',
        arguments: { phase: 'expand' },
        result: { content: [{ type: 'text', text: 'ok' }] },
        durationMs: 42,
      }),
      harnessRow(10, 'mcpToolCall', {
        id: 'mcp_2',
        server: 'neige',
        tool: 'spec_set_phase',
        status: 'failed',
        arguments: { phase: 'bad' },
        error: { message: 'invalid phase' },
      }),
    ]);

    expect(await screen.findByText('please update the PR UI')).toBeInTheDocument();
    expect(screen.queryByText('[userMessage]')).not.toBeInTheDocument();
    expect(screen.getByText('user')).toBeInTheDocument();
    expect(await screen.findByText('assistant hello')).toBeInTheDocument();
    expect(screen.getByText('Reasoning')).toBeInTheDocument();
    expect(screen.getByText('private chain summary')).toBeInTheDocument();
    expect(screen.getByText(/Called/)).toBeInTheDocument();
    expect(screen.getByText('lookup')).toBeInTheDocument();
    expect(screen.getByText(/spec docs/)).toBeInTheDocument();
    expect(screen.getByText('tool output body')).toBeInTheDocument();
    expect(screen.getByText('Searched: playwright fixtures')).toBeInTheDocument();
    expect(screen.getByText(/\$ echo hi/)).toBeInTheDocument();
    expect(screen.getByText('[mystery_item]')).toBeInTheDocument();
    expect(screen.queryByText('[mcpToolCall]')).not.toBeInTheDocument();
    expect(screen.queryByText('[mcp_tool_call]')).not.toBeInTheDocument();
    expect(screen.getAllByText('neige/spec_set_phase')).toHaveLength(2);
    expect(screen.getByText('completed')).toBeInTheDocument();
    expect(screen.getByText('failed')).toBeInTheDocument();
    expect(screen.getByText(/invalid phase/)).toBeInTheDocument();
  });

  it('merges a live item that arrives while the initial REST load is in flight', async () => {
    const restRow = harnessRow(1, 'agent_message', {
      id: 'agent_rest',
      text: 'loaded from rest',
    });
    const liveRow = harnessRow(2, 'agent_message', {
      id: 'agent_live',
      text: 'arrived over websocket',
    });
    let resolveInitial!: (value: Response) => void;
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(
        new Promise<Response>((resolve) => {
          resolveInitial = resolve;
        }),
      )
      .mockResolvedValueOnce(response([liveRow]));
    vi.stubGlobal('fetch', fetchMock);
    const Component = SpecEntry.Component;
    render(
      <Component
        card={{
          type: 'spec',
          id: 'card_spec_1',
          goal: 'Ship the spec UI',
        }}
      />,
    );

    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        '/api/cards/card_spec_1/harness/items?limit=100&direction=desc',
        expect.objectContaining({ credentials: 'include' }),
      ),
    );

    await act(async () => {
      mocks.fakeStream.emit({
        ev: 'harness.item.added',
        data: {
          runtime_id: 'runtime_1',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
          item_db_id: 2,
          item_uuid: 'agent_live',
          item_type: 'agent_message',
          turn_id: 'turn_1',
          method: 'item/completed',
        },
      });
    });

    expect(await screen.findByText('arrived over websocket')).toBeInTheDocument();
    await act(async () => {
      resolveInitial(response([restRow]));
    });

    expect(await screen.findByText('loaded from rest')).toBeInTheDocument();
    expect(screen.getByText('arrived over websocket')).toBeInTheDocument();
  });

  it('dedupes by item_uuid when a completed item replaces its started row', async () => {
    const started = harnessRow(
      1,
      'agent_message',
      { id: 'agent_same' },
      { method: 'item/started', item_uuid: 'agent_same' },
    );
    const completed = harnessRow(
      2,
      'agent_message',
      { id: 'agent_same', text: 'final assistant answer' },
      { method: 'item/completed', item_uuid: 'agent_same' },
    );
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response([started]))
      .mockResolvedValueOnce(response([completed]));
    vi.stubGlobal('fetch', fetchMock);
    const Component = SpecEntry.Component;
    render(
      <Component
        card={{
          type: 'spec',
          id: 'card_spec_1',
          goal: 'Ship the spec UI',
        }}
      />,
    );

    expect(await screen.findByText('Thinking...')).toBeInTheDocument();

    await act(async () => {
      mocks.fakeStream.emit({
        ev: 'harness.item.added',
        data: {
          runtime_id: 'runtime_1',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
          item_db_id: 2,
          item_uuid: 'agent_same',
          item_type: 'agent_message',
          turn_id: 'turn_1',
          method: 'item/completed',
        },
      });
    });

    expect(await screen.findByText('final assistant answer')).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.queryByText('Thinking...')).not.toBeInTheDocument();
    });
    expect(fetchMock).toHaveBeenLastCalledWith(
      '/api/cards/card_spec_1/harness/items?after_id=1&limit=1',
      expect.objectContaining({ credentials: 'include' }),
    );
  });

  it('drops a stale started item when its completed item already exists', async () => {
    const completed = harnessRow(
      2,
      'agent_message',
      { id: 'agent_same', text: 'final assistant answer' },
      { method: 'item/completed', item_uuid: 'agent_same' },
    );
    const lateStarted = harnessRow(
      1,
      'agent_message',
      { id: 'agent_same' },
      { method: 'item/started', item_uuid: 'agent_same' },
    );
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response([completed]))
      .mockResolvedValueOnce(response([lateStarted]));
    vi.stubGlobal('fetch', fetchMock);
    const Component = SpecEntry.Component;
    render(
      <Component
        card={{
          type: 'spec',
          id: 'card_spec_1',
          goal: 'Ship the spec UI',
        }}
      />,
    );

    expect(await screen.findByText('final assistant answer')).toBeInTheDocument();

    await act(async () => {
      mocks.fakeStream.emit({
        ev: 'harness.item.added',
        data: {
          runtime_id: 'runtime_1',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
          item_db_id: 1,
          item_uuid: 'agent_same',
          item_type: 'agent_message',
          turn_id: 'turn_1',
          method: 'item/started',
        },
      });
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(screen.queryByText('Thinking...')).not.toBeInTheDocument();
    expect(screen.getByText('final assistant answer')).toBeInTheDocument();
  });

  it('keeps only the completed row when live events arrive completed before started', async () => {
    const completed = harnessRow(
      2,
      'agent_message',
      { id: 'agent_live_same', text: 'completed over websocket' },
      { method: 'item/completed', item_uuid: 'agent_live_same' },
    );
    const lateStarted = harnessRow(
      1,
      'agent_message',
      { id: 'agent_live_same' },
      { method: 'item/started', item_uuid: 'agent_live_same' },
    );
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response([]))
      .mockResolvedValueOnce(response([completed]))
      .mockResolvedValueOnce(response([lateStarted]));
    vi.stubGlobal('fetch', fetchMock);
    const Component = SpecEntry.Component;
    render(
      <Component
        card={{
          type: 'spec',
          id: 'card_spec_1',
          goal: 'Ship the spec UI',
        }}
      />,
    );

    expect(await screen.findByText('No conversation items yet.')).toBeInTheDocument();

    await act(async () => {
      mocks.fakeStream.emit({
        ev: 'harness.item.added',
        data: {
          runtime_id: 'runtime_1',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
          item_db_id: 2,
          item_uuid: 'agent_live_same',
          item_type: 'agent_message',
          turn_id: 'turn_1',
          method: 'item/completed',
        },
      });
    });

    expect(await screen.findByText('completed over websocket')).toBeInTheDocument();

    await act(async () => {
      mocks.fakeStream.emit({
        ev: 'harness.item.added',
        data: {
          runtime_id: 'runtime_1',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
          item_db_id: 1,
          item_uuid: 'agent_live_same',
          item_type: 'agent_message',
          turn_id: 'turn_1',
          method: 'item/started',
        },
      });
    });

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(3));
    expect(screen.queryByText('Thinking...')).not.toBeInTheDocument();
    expect(screen.getAllByText('completed over websocket')).toHaveLength(1);
  });

  it('clears and refetches rows when the transcript is cleared', async () => {
    const before = harnessRow(1, 'agent_message', {
      id: 'agent_before_clear',
      text: 'before clear',
    });
    const after = harnessRow(
      10,
      'agent_message',
      { id: 'agent_after_clear', text: 'after clear' },
      { runtime_id: 'runtime_2' },
    );
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(response([before]))
      .mockResolvedValueOnce(response([after]));
    vi.stubGlobal('fetch', fetchMock);
    const Component = SpecEntry.Component;
    render(
      <Component
        card={{
          type: 'spec',
          id: 'card_spec_1',
          goal: 'Ship the spec UI',
        }}
      />,
    );

    expect(await screen.findByText('before clear')).toBeInTheDocument();

    await act(async () => {
      mocks.fakeStream.emit({
        ev: 'harness.transcript.cleared',
        data: {
          runtime_id: 'runtime_2',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
        },
      });
    });

    expect(await screen.findByText('after clear')).toBeInTheDocument();
    expect(screen.queryByText('before clear')).not.toBeInTheDocument();
    expect(fetchMock).toHaveBeenLastCalledWith(
      '/api/cards/card_spec_1/harness/items?limit=100&direction=desc',
      expect.objectContaining({ credentials: 'include' }),
    );
  });

  it('remounts the chat timeline after a successful reset', async () => {
    const user = userEvent.setup();
    mocks.resetSpecCard.mockResolvedValueOnce({
      card_id: 'card_spec_1',
      terminal_id: '',
      new_thread_id: 'thread_reset',
    });
    const fetchMock = renderSpecCardWithProvider([
      [
        harnessRow(1, 'agent_message', {
          id: 'agent_before_reset',
          text: 'before reset',
        }),
      ],
      [],
    ]);

    expect(await screen.findByText('before reset')).toBeInTheDocument();
    const timelineBefore = screen.getByTestId('spec-chat-timeline');
    act(() => {
      mocks.fakeStream.emit({
        ev: 'harness.phase.changed',
        data: {
          runtime_id: 'runtime_1',
          card_id: 'card_spec_1',
          wave_id: 'wave_1',
          old_phase: 'idle',
          new_phase: 'turn_running',
        },
      });
    });
    expect(screen.getByText('Turn Running')).toBeInTheDocument();

    await user.click(
      await screen.findByRole('button', { name: 'Reset spec session' }),
    );
    await user.click(screen.getByRole('button', { name: 'Reset session' }));

    await waitFor(() =>
      expect(mocks.resetSpecCard).toHaveBeenCalledWith('card_spec_1'),
    );
    await waitFor(() =>
      expect(screen.getByTestId('spec-chat-timeline')).not.toBe(timelineBefore),
    );
    await waitFor(() =>
      expect(screen.queryByText('before reset')).not.toBeInTheDocument(),
    );
    expect(screen.getByText('No conversation items yet.')).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByText('Turn Running')).not.toBeInTheDocument(),
    );
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
