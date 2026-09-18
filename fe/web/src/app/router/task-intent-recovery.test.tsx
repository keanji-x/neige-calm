import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import type { IndependentTaskIntent, IndependentTaskRequest } from '../../../../core/domain/independent-task.ts';
import type { TaskRecoveryIntent } from '../../../../core/domain/task-execution.ts';
import type { CardWire } from '../../../../core/domain/track.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { useIndependentTaskLaunch } from './independent-task.tsx';
import { TaskRecovery } from './task-recovery.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
async function setup(kind: 'independent' | 'recovery') {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const requests: ApiRequest[] = []; const pending: ((response: ApiTransportResponse) => void)[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const card: CardWire = { id: 'report', track_id: 'w1', kind: 'track-report', title: null, sort: 1, deletable: false,
    created_at: 1, updated_at: 1, payload: { schemaVersion: 3, docRev: 12, summary: '', body: '', blocks: [] } };
  const old = { attempt_id: 'old', generation: 1, status: 'failed', blocking_reason: null, status_detail: null,
    worker_card_id: null, created_at_ms: 1, finished_at_ms: 2 };
  const transport = createRecoveryTransports({ send: request => {
    requests.push(request);
    if (request.method === 'POST') return new Promise(resolve => pending.push(resolve));
    if (request.path.endsWith('/report')) return Promise.resolve(ok({ attemptId: 'old', report: null }));
    if (request.path.endsWith('/attempts')) return Promise.resolve(ok({ key: 'b', current: old, attempts: [old],
      recovery: { allowed: true, code: 'available', reason: 'Retry this task.' } }));
    return Promise.resolve(ok({ track: { id: 'w1', area_id: 'a', title: 'Track', sort: 1, lifecycle: 'working', cwd: '/tmp',
      archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 }, can_resume: false, cards: [card], overlays: [] }));
  } }, access).business;
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const identity = vi.fn(() => Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner' as const, sessionId: 'old-session' }));
  const values = new Map<string, string>(); const online = vi.fn(() => true);
  const session = new RecoverySession({ access, storage: { getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); } },
    origin: 'https://server.test', compatibleVersion: 28, identity,
    version: () => Promise.resolve({ webCompatVersion: 28, minWebCompatVersion: 28, syncEventVersion: 3, dbInstanceId: 'db' }),
    logout: async () => {}, clear: () => client.clear(), adoptScope: () => {}, online, visible: () => true });
  session.start(); await waitFor(() => expect(access.read().phase).toBe('syncing')); session.events('connected');
  function Independent() {
    const launch = useIndependentTaskLaunch({ trackId: 'w1', cards: [card], lifecycle: 'working', transport, unauthorized, onCreated: vi.fn() });
    return <><button onClick={launch.open}>Open task</button>{launch.form}</>;
  }
  const mount = () => render(<QueryClientProvider client={client}><ThemeProvider>
    {kind === 'independent' ? <Independent /> : <TaskRecovery trackId="w1" taskKey="b" expanded
      transport={transport} unauthorized={unauthorized} openWorker={vi.fn()} openableWorkerIds={new Set()} />}
  </ThemeProvider></QueryClientProvider>);
  const key = kind === 'independent' ? ['independent-task-intent', 'w1'] : ['task-recovery-intent', 'w1', 'b'];
  const mounted = mount();
  const submit = async (goal = 'Private original goal') => {
    const before = pending.length;
    if (kind === 'independent') {
      fireEvent.click(screen.getByText('Open task'));
      fireEvent.change(screen.getByRole('textbox', { name: 'Goal' }), { target: { value: goal } });
      fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
    } else fireEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
    await waitFor(() => expect(pending).toHaveLength(before + 1));
    expect(client.getQueryData<{ phase: string }>(key)?.phase).toBe('sending');
  };
  let completed = 0;
  const finish = async () => {
    const post = requests.filter(request => request.method === 'POST')[completed++];
    await act(async () => {
      pending.shift()!(ok(kind === 'independent'
        ? { taskKey: (post.body as IndependentTaskRequest).key, blockId: 'created', docRev: 13 }
        : { key: 'b', previous_attempt_id: 'old', attempt_id: 'new', generation: 2 }));
      await new Promise(resolve => setTimeout(resolve, 0));
    });
  };
  return { access, client, requests, session, mounted, mount, submit, finish, key, identity, online };
}

it.each(['independent', 'recovery'] as const)('%s task completion cannot repopulate cleared logout state or a subsequent login', async kind => {
  const h = await setup(kind);
  try {
    await h.submit(); h.mounted.unmount(); h.online.mockReturnValue(false); await h.session.signOut();
    expect(h.client.getQueryData(h.key)).toBeUndefined(); await h.finish();
    expect(h.client.getQueryData(h.key)).toBeUndefined();
    h.identity.mockResolvedValue({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'new-session' });
    h.online.mockReturnValue(true); await h.session.verifyNewSession(); h.session.events('connected'); h.mount();
    expect(h.client.getQueryData<{ phase: string }>(h.key)?.phase).toBe(kind === 'independent' ? 'editing' : 'idle');
    expect(h.requests.filter(request => request.method === 'POST')).toHaveLength(1);
  } finally { h.session.stop(); }
});

it.each(['independent', 'recovery'] as const)('%s task interruption releases only its retained busy intent without another write', async kind => {
  const h = await setup(kind);
  try {
    await h.submit(); const intent = h.client.getQueryData<IndependentTaskIntent | TaskRecoveryIntent>(h.key)!;
    act(() => { h.session.pause(); }); await h.finish();
    expect(h.client.getQueryData(h.key)).toMatchObject({ phase: 'uncertain', request: 'request' in intent ? intent.request : null });
    act(() => { h.session.resume(); }); await waitFor(() => expect(h.access.read().phase).toBe('syncing'));
    act(() => { h.session.events('connected'); });
    expect(h.requests.filter(request => request.method === 'POST')).toHaveLength(1);
    const retry = await screen.findByRole('button', { name: kind === 'independent' ? 'Retry same request' : 'Retry recovery request' });
    expect((retry as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(retry); await waitFor(() => expect(h.requests.filter(request => request.method === 'POST')).toHaveLength(2));
    const writes = h.requests.filter(request => request.method === 'POST');
    expect(writes[1].body).toEqual(writes[0].body); await h.finish();
    expect(h.client.getQueryData<{ phase: string }>(h.key)?.phase).toBe('accepted');
  } finally { h.session.stop(); }
});


it.each(['independent', 'recovery'] as const)('%s old task cleanup cannot release a new login submission at the same key', async kind => {
  const h = await setup(kind);
  try {
    await h.submit(); h.mounted.unmount(); h.online.mockReturnValue(false); await h.session.signOut();
    h.identity.mockResolvedValue({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'new-session' });
    h.online.mockReturnValue(true); await h.session.verifyNewSession(); h.session.events('connected'); h.mount();
    await h.submit('Fresh login goal'); const fresh = h.client.getQueryData(h.key);
    await h.finish(); expect(h.client.getQueryData(h.key)).toBe(fresh);
    expect(h.client.getQueryData<{ phase: string }>(h.key)?.phase).toBe('sending');
    await h.finish(); expect(h.client.getQueryData<{ phase: string }>(h.key)?.phase).toBe('accepted');
    expect(h.requests.filter(request => request.method === 'POST')).toHaveLength(2);
  } finally { h.session.stop(); }
});

it.each(['independent', 'recovery'] as const)('%s task click while recovering neither acquires busy state nor queues a write', async kind => {
  const h = await setup(kind);
  try {
    if (kind === 'independent') {
      fireEvent.click(screen.getByText('Open task'));
      fireEvent.change(screen.getByRole('textbox', { name: 'Goal' }), { target: { value: 'Keep this draft' } });
    } else await screen.findByRole('button', { name: 'Recover task' });
    act(() => h.session.pause());
    if (kind === 'independent') fireEvent.submit(screen.getByRole('textbox', { name: 'Goal' }).closest('form')!);
    else fireEvent.click(screen.getByRole('button', { name: 'Recover task' }));
    expect(h.client.getQueryData<{ phase: string }>(h.key)?.phase).toBe(kind === 'independent' ? 'editing' : 'idle');
    act(() => h.session.resume()); await waitFor(() => expect(h.access.read().phase).toBe('syncing'));
    act(() => h.session.events('connected'));
    expect(h.requests.filter(request => request.method === 'POST')).toHaveLength(0);
  } finally { h.session.stop(); }
});
