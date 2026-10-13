import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { useReadReceipt, useReadReceipts } from './read-receipts.tsx';
import { createReadReceiptsKernel, readReceiptsFixture, type ReadReceiptsKernel } from './read-receipts-fixture.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function mount(kernel: ReadReceiptsKernel, children: ReactNode) {
  const { wrap } = readReceiptsFixture(kernel);
  const view = render(wrap(children));
  return { rerender: (node: ReactNode) => view.rerender(wrap(node)) };
}

function Verdict({ id, at }: { id: string; at: number | null }) {
  const receipts = useReadReceipts();
  return <p>{id}: {receipts.ready ? (receipts.isUnread('track', id, at) ? 'unread' : 'read') : 'pending'}</p>;
}

it('reads nothing as unread until the kernel answers, then applies its baseline and receipts', async () => {
  let answer!: (value: unknown) => void;
  const pending = new Promise((resolve) => { answer = resolve; });
  const kernel = createReadReceiptsKernel();
  const slow = { ...kernel, transport: { send: async () => ok(await pending) } };
  function Probe() {
    const receipts = useReadReceipts();
    return <p>{['old', 'seen', 'fresh'].map((id) => `${id}=${receipts.isUnread('track', id, 200)}`).join(' ')}</p>;
  }
  mount(slow, <Probe />);
  expect(screen.getByText('old=false seen=false fresh=false')).toBeTruthy();
  await act(async () => {
    answer({ baselineMs: 150, receipts: [{ kind: 'track', targetId: 'old', readThroughMs: 0, manualUnread: false },
      { kind: 'track', targetId: 'seen', readThroughMs: 200, manualUnread: false }] });
    await pending;
  });
  await screen.findByText('old=true seen=false fresh=true');
});

it('marks unread optimistically, posts it, and refetches', async () => {
  const fake = createReadReceiptsKernel({ writeDelayMs: 20 });
  let markUnread!: (id: string) => void;
  function Actions() { const receipts = useReadReceipts(); markUnread = (id) => receipts.markUnread('track', id); return <Verdict id="t" at={null} />; }
  mount(fake, <Actions />);
  await screen.findByText('t: read');
  act(() => markUnread('t'));
  await screen.findByText('t: unread');
  await waitFor(() => expect(fake.requests.map((request) => `${request.method} ${request.path}`)).toEqual([
    'GET /api/read-receipts', 'POST /api/read-receipts/track/t/unread', 'GET /api/read-receipts',
  ]));
  expect(screen.getByText('t: unread')).toBeTruthy();
});

it('puts the kernel state back when a write fails', async () => {
  const fake = createReadReceiptsKernel({ fail: true, writeDelayMs: 20 });
  let markUnread!: () => void;
  function Actions() { const receipts = useReadReceipts(); markUnread = () => receipts.markUnread('track', 't'); return <Verdict id="t" at={null} />; }
  mount(fake, <Actions />);
  await screen.findByText('t: read');
  act(() => markUnread());
  await screen.findByText('t: unread');
  await screen.findByText('t: read');
  expect(fake.posts()).toHaveLength(1);
});

it('acknowledges the open target only while the tab is visible, and only when it is unread', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
  function Open({ at }: { at: number }) { useReadReceipt('track', 't', at); return <Verdict id="t" at={at} />; }
  const view = mount(fake, <Open at={150} />);
  await screen.findByText('t: unread');
  expect(fake.posts()).toHaveLength(0);
  visibility.mockReturnValue('visible');
  act(() => { document.dispatchEvent(new Event('visibilitychange')); });
  await screen.findByText('t: read');
  expect(fake.posts().map((request) => [request.path, request.body])).toEqual([
    ['/api/read-receipts/track/t/read', { readThroughMs: 150 }],
  ]);
  // Activity at or before the baseline is already read: nothing to write.
  view.rerender(<Open at={90} />);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(fake.posts()).toHaveLength(1);
});

it('keeps a manual unread of the open target until it is opened again', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  let markUnread!: () => void;
  function Open({ opened }: { opened: number }) {
    const receipts = useReadReceipts();
    markUnread = () => receipts.markUnread('track', 't');
    useReadReceipt('track', 't', 50, true, opened);
    return <Verdict id="t" at={50} />;
  }
  const view = mount(fake, <Open opened={1} />);
  await screen.findByText('t: read');
  act(() => markUnread());
  await screen.findByText('t: unread');
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(screen.getByText('t: unread')).toBeTruthy();
  view.rerender(<Open opened={2} />);
  await screen.findByText('t: read');
  expect(fake.posts().map((request) => request.path)).toEqual([
    '/api/read-receipts/track/t/unread', '/api/read-receipts/track/t/read',
  ]);
});
