import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import type { HarnessItem } from '../../../../core/api/generated/wire.ts';
import { buildTranscript, harnessItemToTurns } from '../../../../core/domain/conversation.ts';
import { groupTranscriptActivities } from '../../../../core/domain/conversation-groups.ts';
import { currentResponseMessage } from '../../../../core/domain/conversation-actions.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

// Count the canonical production derivations, without replacing their behavior.
vi.mock(import('../../../../core/domain/conversation.ts'), async original => {
  const actual = await original();
  return { ...actual, buildTranscript: vi.fn(actual.buildTranscript), harnessItemToTurns: vi.fn(actual.harnessItemToTurns) };
});
vi.mock(import('../../../../core/domain/conversation-groups.ts'), async original => {
  const actual = await original();
  return { ...actual, groupTranscriptActivities: vi.fn(actual.groupTranscriptActivities) };
});
vi.mock(import('../../../../core/domain/conversation-actions.ts'), async original => {
  const actual = await original();
  return { ...actual, currentResponseMessage: vi.fn(actual.currentResponseMessage) };
});

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.clearAllMocks(); document.getElementById('root')?.remove(); });

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const field = () => screen.getByRole('combobox', { name: 'Message' });
async function type(text: string) {
  await act(async () => {
    field().textContent = text;
    fireEvent.input(field());
    await Promise.resolve();
  });
}
function heldResponse() {
  let release!: (response: ApiTransportResponse) => void;
  const promise = new Promise<ApiTransportResponse>(resolve => { release = resolve; });
  return { promise, release: async (body: unknown) => { await act(async () => { release(ok(body)); await promise; }); } };
}
function mount({ phase = 'idle', input }: { phase?: string; input?: Promise<ApiTransportResponse> } = {}) {
  return renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
    if (request.path === '/api/version') return ok({ assistantProviders: true, conversationCreateModel: true,
      webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 29, dbInstanceId: 'input-response' });
    if (request.path.startsWith('/api/agent-providers/managed')) return ok([{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
    }]);
    if (request.path === '/api/cards/second-planner/planner/run') return ok({ card_id: 'second-planner', worker_session_id: 'runtime',
      phase, model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, manual_compact_supported: true,
      compact_outcome: null, context_usage_invalidated: false, running_turn: null });
    if (request.path.includes('/second-planner/harness/items')) return ok(Array.from({ length: 120 }, (_, index) => ({
      id: index + 1, worker_session_id: 'runtime', card_id: 'second-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: `Stored reply ${index}. **Evidence** remains readable.` } }), created_at_ms: index + 1,
    })));
    if (input !== undefined && request.path.endsWith('/planner/input')) return input;
    if (input !== undefined && request.method === 'POST' && request.path.endsWith('/conversations')) return input;
    return undefined;
  } });
}
async function openExisting() {
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
  await screen.findByText('Stored reply 119.', { exact: false }, { timeout: 5000 });
}

it('keeps unchanged stored history derivation and grouping out of composer updates', async () => {
  const view = mount();
  try {
    await openExisting();
    // The first nonempty character legitimately withdraws Edit's empty-composer affordance.
    await type('A');
    vi.mocked(buildTranscript).mockClear(); vi.mocked(harnessItemToTurns).mockClear(); vi.mocked(groupTranscriptActivities).mockClear();
    vi.mocked(currentResponseMessage).mockClear();
    const stored = document.querySelector('[data-nc-turn="agent"]');
    await type('A draft'); await type('A draft with more words');
    expect(field().textContent).toBe('A draft with more words');
    expect(document.querySelector('[data-nc-turn="agent"]')).toBe(stored);
    expect(buildTranscript).not.toHaveBeenCalled();
    expect(harnessItemToTurns).not.toHaveBeenCalled();
    expect(groupTranscriptActivities).not.toHaveBeenCalled();
    expect(currentResponseMessage).not.toHaveBeenCalled();
    // Real new server content still invalidates the projection without touching the draft.
    await act(async () => {
      view.client.setQueryData<{ pages: HarnessItem[][] }>(['harness-items', 'second-planner'], previous => previous === undefined ? previous : ({
        ...previous, pages: previous.pages.map(page => page.map(row => row.id === 120
          ? { ...row, params: JSON.stringify({ item: { text: 'The server updated the last reply.' } }) } : row)),
      }));
      await Promise.resolve();
    });
    await screen.findByText('The server updated the last reply.');
    expect(buildTranscript).toHaveBeenCalled();
    expect(groupTranscriptActivities).toHaveBeenCalled();
    expect(field().textContent).toBe('A draft with more words');
  } finally { cleanup(); view.client.clear(); }
});

it.each(['idle', 'turn_running'])('shows the local sending state before a held %s request answers', async phase => {
  const response = heldResponse();
  const view = mount({ phase, input: response.promise });
  try {
    await openExisting(); await type('Send without waiting for the server');
    vi.mocked(buildTranscript).mockClear(); vi.mocked(harnessItemToTurns).mockClear();
    fireEvent.keyDown(field(), { key: 'Enter' });
    await waitFor(() => expect(view.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(1));
    expect(document.querySelector('[data-nc-turn="you"]')?.textContent).toBe('Send without waiting for the server');
    expect(screen.getByText('Sending…')).toBeDefined();
    expect(buildTranscript).not.toHaveBeenCalled();
    expect(harnessItemToTurns).not.toHaveBeenCalled();
    // Local dispatch is not a confirmed queued entry; Stop remains available for an existing run.
    expect(screen.queryByText('Queued · sends when this turn ends')).toBeNull();
    if (phase === 'turn_running') expect(screen.getByRole('button', { name: 'Stop' })).toBeDefined();
    else {
      expect(screen.getByRole('button', { name: 'Sending…' }).hasAttribute('disabled')).toBe(true);
      expect(screen.queryByText('Running', { exact: true })).toBeNull();
    }
    fireEvent.keyDown(field(), { key: 'Enter' });
    expect(view.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(1);
  } finally { await response.release({ card_id: 'second-planner', worker_session_id: 'runtime' }); cleanup(); view.client.clear(); }
});

it('shows first-message sending feedback while conversation creation is held', async () => {
  const response = heldResponse();
  const view = mount({ input: response.promise });
  try {
    fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
    await waitFor(() => expect(view.client.getQueryData(['server-version'])).toMatchObject({ assistantProviders: true }));
    await waitFor(() => expect(view.client.getQueryData(['managed-providers'])).toHaveLength(1));
    await type('The first local message');
    fireEvent.keyDown(field(), { key: 'Enter' });
    await waitFor(() => expect(view.requests.some(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toBe(true));
    expect(document.querySelector('[data-nc-turn="you"]')?.textContent).toBe('The first local message');
    expect(screen.getByText('Sending…')).toBeDefined();
    expect(screen.getByRole('button', { name: 'Sending…' }).hasAttribute('disabled')).toBe(true);
  } finally { await response.release({ id: 'created', trackId: 'daily', title: null, kind: 'track-assistant', provider: 'codex', state: null, updatedAt: 1, lastTurnCompletedAt: null }); cleanup(); view.client.clear(); }
});
