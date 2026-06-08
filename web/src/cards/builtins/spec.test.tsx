import { act, render, screen, waitFor } from '@testing-library/react';
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
  const fetchMock = vi.fn().mockResolvedValue(response(rows));
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

beforeEach(() => {
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
      harnessRow(1, 'agent_message', {
        id: 'agent_1',
        text: 'assistant hello',
      }),
      harnessRow(2, 'reasoning', {
        id: 'reason_1',
        text: 'private chain summary',
      }),
      harnessRow(3, 'function_call', {
        id: 'call_1',
        name: 'lookup',
        arguments: { query: 'spec docs' },
      }),
      harnessRow(4, 'function_call_output', {
        id: 'out_1',
        output: 'tool output body',
      }),
      harnessRow(5, 'web_search', {
        id: 'search_1',
        query: 'playwright fixtures',
      }),
      harnessRow(6, 'local_shell', {
        id: 'shell_1',
        command: 'echo hi',
        output: 'hi',
      }),
      harnessRow(7, 'mystery_item', {
        id: 'mystery_1',
      }),
    ]);

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
});
