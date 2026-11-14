import { StrictMode } from 'react';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest } from '../../../../core/api/types.ts';
import { ShareUnlockGate } from './share-unlock.tsx';

afterEach(() => { cleanup(); window.history.replaceState(null, '', '/'); vi.restoreAllMocks(); });
it('probes access, handles wrong tokens and rate limits, then retries session after unlock', async () => {
  const requests: ApiRequest[] = [];
  let attempt = 0;
  const retry = vi.fn();
  const transport = { send: (request: ApiRequest) => {
    requests.push(request);
    const status = request.method === 'GET' ? 200 : [401, 429, 200][attempt++];
    return Promise.resolve({ status, statusText: '', body: request.method === 'GET' ? { status: 'locked', expiresAt: 100 }
      : status === 200 ? { scope: { kind: 'track', id: 't' }, expiresAt: 100 } : {} });
  } };
  render(<ShareUnlockGate shareLinkId="a/b" transport={transport} retry={retry} />);
  const token = await screen.findByLabelText('口令');
  fireEvent.change(token, { target: { value: 'password' } });
  fireEvent.click(screen.getByRole('button', { name: '解锁' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', '口令不正确，请重试。');
  fireEvent.click(screen.getByRole('button', { name: '解锁' }));
  await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('尝试次数过多，请稍后再试。'));
  fireEvent.click(screen.getByRole('button', { name: '解锁' }));
  await waitFor(() => expect(retry).toHaveBeenCalledOnce());
  expect(requests[0].path).toBe('/api/share-links/a%2Fb/access');
  expect(requests.slice(1).map(r => [r.path, r.body])).toEqual(Array.from({ length: 3 }, () => ['/api/share-links/a%2Fb/unlock', { token: 'password' }]));
  expect((token as HTMLInputElement).value).toBe('');
});
it.each(['probe', 'unlock'])('shows expired or missing links on %s 404', async stage => {
  const transport = { send: (request: ApiRequest) => Promise.resolve({ status: stage === 'probe' || request.method === 'POST' ? 404 : 200,
    statusText: '', body: { status: 'locked', expiresAt: 100 } }) };
  render(<ShareUnlockGate shareLinkId="id" transport={transport} retry={() => {}} />);
  if (stage === 'unlock') {
    fireEvent.change(await screen.findByLabelText('口令'), { target: { value: 'secret' } });
    fireEvent.click(screen.getByRole('button', { name: '解锁' }));
  }
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', '分享链接不存在或已过期。');
});

it.each([200, 401, 429, 500, 404])('consumes a hash once in StrictMode and handles automatic unlock %s', async status => {
  window.history.replaceState({ preserved: true }, '', '/next/share/id?view=report#secret%2Fonce%2B%23%3F');
  let finish = () => {};
  const pending = new Promise<void>(resolve => { finish = resolve; });
  const retry = vi.fn();
  const requests: ApiRequest[] = [];
  const transport = { send: async (request: ApiRequest) => {
    expect(window.location.hash).toBe('');
    requests.push(request);
    if (request.method === 'GET') return { status: 200, statusText: '', body: { status: 'locked', expiresAt: 100 } };
    await pending;
    return { status, statusText: '', body: status === 200 ? { scope: { kind: 'track', id: 't' }, expiresAt: 100 } : {} };
  } };
  render(<StrictMode><ShareUnlockGate shareLinkId="id" transport={transport} retry={retry} /></StrictMode>);
  expect(window.location.hash).toBe('');
  expect(window.location.pathname + window.location.search).toBe('/next/share/id?view=report');
  expect(window.history.state).toEqual({ preserved: true });
  await waitFor(() => expect(requests.filter(r => r.method === 'POST')).toHaveLength(1));
  expect(requests.find(r => r.method === 'POST')?.body).toEqual({ token: 'secret/once+#?' });
  finish();
  if (status === 200) await waitFor(() => expect(retry).toHaveBeenCalledOnce());
  else {
    expect(await screen.findByRole('alert')).toHaveProperty('textContent', status === 404 ? '分享链接不存在或已过期。'
      : status === 401 ? '口令不正确，请重试。' : status === 429 ? '尝试次数过多，请稍后再试。' : '无法解锁，请重试。');
    expect(retry).not.toHaveBeenCalled();
    if (status === 404) expect(screen.queryByLabelText('口令')).toBeNull();
    else {
      const input = await screen.findByLabelText('口令');
      expect((input as HTMLInputElement).value).toBe('');
      fireEvent.change(input, { target: { value: 'manual' } });
      fireEvent.click(screen.getByRole('button', { name: '解锁' }));
      await waitFor(() => expect(requests.filter(r => r.method === 'POST')).toHaveLength(2));
      expect(requests.at(-1)?.body).toEqual({ token: 'manual' });
    }
  }
});
it('clears a residual hash without unlocking an already unlocked session', async () => {
  window.history.replaceState(null, '', '/next/share/id#secret');
  const retry = vi.fn();
  const transport = { send: vi.fn(() => Promise.resolve({ status: 200, statusText: '', body: {
    status: 'unlocked', scope: { kind: 'track', id: 't' }, expiresAt: 100,
  } })) };
  render(<StrictMode><ShareUnlockGate shareLinkId="id" transport={transport} retry={retry} /></StrictMode>);
  expect(window.location.hash).toBe('');
  await waitFor(() => expect(retry).toHaveBeenCalledOnce());
  expect(transport.send.mock.calls).toHaveLength(1);
});

it('clears a malformed hash and falls back to manual entry without posting it', async () => {
  window.history.replaceState(null, '', '/next/share/id#%ZZ');
  const transport = { send: vi.fn(() => Promise.resolve({ status: 200, statusText: '', body: { status: 'locked', expiresAt: 100 } })) };
  render(<StrictMode><ShareUnlockGate shareLinkId="id" transport={transport} retry={() => {}} /></StrictMode>);
  expect(window.location.hash).toBe('');
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', '无法解锁，请重试。');
  expect(screen.getByLabelText('口令')).toBeTruthy();
  expect(transport.send).toHaveBeenCalledOnce();
});
