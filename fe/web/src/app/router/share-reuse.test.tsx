import { afterEach, expect, it, vi } from 'vitest';
import { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory } from '@tanstack/react-router';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { createShareStorage } from '../auth/share-storage.ts';
import { createFetchTransport } from '../providers/transport.ts';
import { createCardFilesPort, createTrackWorkspaceFilesPort } from '../providers/directory.ts';
import { createMemoryStorage } from './share-reuse-fixture.ts';
import { createTrackTaskFixture } from './track-task-fixture.ts';

afterEach(() => { vi.unstubAllGlobals(); });
it.each(['track', 'area', 'empty'] as const)('redirects the %s scope root within its basepath preserving search and fragment', async kind => {
  const fixture = createTrackTaskFixture();
  const transport = kind !== 'empty' ? fixture.transport : { send: () => Promise.resolve({ status: 200, statusText: '', body: [] }) };
  const router = createAppRouter({ transport, unauthorized: createUnauthorizedChannel({ enqueue: task => task() }),
    client: new QueryClient(), cards: bootTestCardRuntime(), onSignOut: () => {}, prepareReport: prepareReportDocument,
    basepath: '/next/share/link', shareLinkId: 'link', shareScope: kind === 'track' ? { kind: 'track', id: 'w1' } : { kind: 'area', id: 'c1' } });
  router.update({ history: createMemoryHistory({ initialEntries: ['/next/share/link/?card=tool#intro'] }) });
  await router.load();
  await vi.waitFor(() => expect(router.history.location.pathname).toBe(`/next/share/link/${kind === 'empty' ? 'area/c1/new' : 'track/w1'}`));
  expect(router.state.location.searchStr).toBe('?card=tool'); expect(router.state.location.hash).toBe('intro');
});
it('isolates enumeration, clear, UI preferences and cursor keys between owner and guests', () => {
  const storage = createMemoryStorage();
  storage.setItem('owner', 'keep');
  const first = createShareStorage(storage, 'a'); const second = createShareStorage(storage, 'b');
  first.setItem('cursor', '12'); second.setItem('cursor', '34');
  expect(first.getItem('cursor')).toBe('12'); expect(first.length).toBe(1); expect(first.key(0)).toBe('cursor');
  first.clear(); expect(first.length).toBe(0); expect(second.getItem('cursor')).toBe('34'); expect(storage.getItem('owner')).toBe('keep');
});
it('sends share identity on ordinary HTTP and both raw-file ports', async () => {
  const fetch = vi.fn(() => Promise.resolve(new Response('{}', { status: 200 }))); vi.stubGlobal('fetch', fetch);
  const transport = createFetchTransport({ shareLinkId: 'a/b' });
  await transport.send({ method: 'GET', path: '/api/auth/whoami', credentials: 'include' });
  expect(fetch.mock.calls[0]).toEqual(['/api/auth/whoami', expect.objectContaining({ headers: { 'X-Calm-Share-Link': 'a/b' }, credentials: 'include' })]);
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  for (const files of [createCardFilesPort(transport, unauthorized, 'a/b'), createTrackWorkspaceFilesPort(transport, unauthorized, 'w1', 'a/b')]) {
    const url = new URL(files.rawUrl('image.png'), 'https://example.test');
    expect(url.searchParams.get('share_link')).toBe('a/b');
  }
});
