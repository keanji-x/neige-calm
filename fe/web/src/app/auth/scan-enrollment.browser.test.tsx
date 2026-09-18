import { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, act, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import '../../styles/entry.css';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createScanPairingPort } from '../../systems/recovery/scan.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { createRecoveryUnauthorizedChannel } from '../../systems/recovery/unauthorized.ts';
import { createIsolatedRetryFixture } from '../router/isolated-task-retry-fixture.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { ProductionApp } from './production-app.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
it('the production bundled gate pairs in its original document and opens the saved page without a verification click', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(390, 844);
  window.history.replaceState({}, '', '/next/track/w1');
  const fixture = createIsolatedRetryFixture(); const access = new RecoveryAccess();
  const transport = createRecoveryTransports(fixture.transport, access).business;
  const unauthorized = createRecoveryUnauthorizedChannel(access, { enqueue: task => queueMicrotask(task) });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
  const order: string[] = []; let redeem!: (reply: ApiTransportResponse) => void;
  const send = vi.fn((request: ApiRequest): Promise<ApiTransportResponse> => {
    order.push(request.path);
    return request.path.endsWith('/claim') ? Promise.resolve({ status: 200, statusText: 'OK', body: { enrollmentId: 'invite', attemptId: 'attempt', claimId: 'claim' } })
      : new Promise(resolve => { redeem = resolve; });
  });
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 3, dbInstanceId: 'scan-test' };
  const fetchVersion = vi.fn(() => { order.push('version'); return Promise.resolve(version); });
  const identity = vi.fn(() => { order.push('whoami'); return Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner' as const, sessionId: 'fresh-session' }); });
  const recovery = new RecoverySession({ access, storage, origin: 'https://neige.tail.example', compatibleVersion: WEB_COMPAT_VERSION,
    adoptScope: vi.fn(), identity, version: fetchVersion, logout: () => Promise.resolve(), clear: () => client.clear(), online: () => true, visible: () => true },
  { input: { kind: 'scan', context: { generation: 9, origin: 'https://neige.tail.example', enrollmentId: 'invite', attemptId: 'attempt',
    attemptSecret: 'a'.repeat(64), pairTicket: 'b'.repeat(64), deadline: Date.now() + 180_000 } }, pairing: createScanPairingPort({ send }) });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => { void recovery.signOut(); } });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  const document = window.document;
  render(<ProductionApp transport={transport} unauthorized={unauthorized} client={client} router={router} recovery={recovery}
    runtime={{ fetchVersion, reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: 'scan-test', storage }} cursorStore={{ clear: vi.fn() }}
    renderLogin={() => <p>需要重新扫码</p>} renderError={() => <p>暂时未连接</p>} />);
  await waitFor(() => expect(send).toHaveBeenCalledTimes(2));
  expect(identity).not.toHaveBeenCalled(); expect(fixture.requests).toHaveLength(0);
  expect(screen.getByRole('heading', { name: 'Track' })).toBeTruthy();
  act(() => { redeem({ status: 200, statusText: 'OK', body: { enrollmentId: 'invite', attemptId: 'attempt', sessionFingerprint: '6542995698f2176ac198ef55125a6924b40e7cd07653a76c1157da771b2c9bf9' } }); });
  await screen.findByText(fixture.goal);
  expect(window.document).toBe(document); expect(window.location.pathname).toBe('/next/track/w1');
  expect(order).toEqual(['/api/mobile/enrollments/claim', '/api/mobile/enrollments/redeem', 'whoami', 'version']);
  expect(send).toHaveBeenCalledTimes(2); expect(identity).toHaveBeenCalledOnce();
  await page.screenshot({ path: '../../../../test-results/1712-scan-paired-390.png' });
});
