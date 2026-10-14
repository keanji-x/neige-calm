import { onlineManager } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { useReadReceipt, useReadReceipts } from './read-receipts.tsx';
import { createReadReceiptsKernel, readReceiptsFixture, type ReadReceiptsKernel } from './read-receipts-fixture.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); onlineManager.setOnline(true); });

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function mount(kernel: ReadReceiptsKernel, children: ReactNode) {
  const { wrap, client } = readReceiptsFixture(kernel);
  const view = render(wrap(children));
  return { client, rerender: (node: ReactNode) => view.rerender(wrap(node)) };
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
  function Actions() { const receipts = useReadReceipts(); markUnread = (id) => { void receipts.markUnread('track', id); }; return <Verdict id="t" at={null} />; }
  mount(fake, <Actions />);
  await screen.findByText('t: read');
  act(() => { void markUnread('t'); });
  await screen.findByText('t: unread');
  await waitFor(() => expect(fake.requests.map((request) => `${request.method} ${request.path}`)).toEqual([
    'GET /api/read-receipts', 'POST /api/read-receipts/track/t/unread', 'GET /api/read-receipts',
  ]));
  expect(screen.getByText('t: unread')).toBeTruthy();
});

it('puts the kernel state back when a write fails', async () => {
  const fake = createReadReceiptsKernel({ fail: true, writeDelayMs: 20 });
  let markUnread!: () => Promise<void>;
  let failure: unknown = null;
  function Actions() {
    const receipts = useReadReceipts();
    markUnread = () => receipts.markUnread('track', 't').catch((error: unknown) => { failure = error; });
    return <Verdict id="t" at={null} />;
  }
  mount(fake, <Actions />);
  await screen.findByText('t: read');
  act(() => { void markUnread(); });
  await screen.findByText('t: unread');
  await screen.findByText('t: read');
  expect(fake.posts()).toHaveLength(1);
  // The reader's own write rejects, for the caller's failure feedback.
  expect(failure).not.toBeNull();
});

it('acknowledges the open target only while the tab is visible, and again when its activity moves', async () => {
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
  // A move is acknowledged whatever the cache says; the kernel keeps the larger instant.
  view.rerender(<Open at={160} />);
  await waitFor(() => expect(fake.posts().map((request) => request.body)).toEqual([{ readThroughMs: 150 }, { readThroughMs: 160 }]));
});

it('keeps a manual unread of the open target until it is opened again', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  let markUnread!: () => void;
  function Open({ opened }: { opened: number }) {
    const receipts = useReadReceipts();
    markUnread = () => { void receipts.markUnread('track', 't'); };
    useReadReceipt('track', 't', 50, true, opened);
    return <Verdict id="t" at={50} />;
  }
  const view = mount(fake, <Open opened={1} />);
  await screen.findByText('t: read');
  act(() => { void markUnread(); });
  await screen.findByText('t: unread');
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(screen.getByText('t: unread')).toBeTruthy();
  view.rerender(<Open opened={2} />);
  await screen.findByText('t: read');
  expect(fake.posts().map((request) => request.path)).toEqual([
    '/api/read-receipts/track/t/read', '/api/read-receipts/track/t/unread', '/api/read-receipts/track/t/read',
  ]);
});

/* #2596 fix round 1: no read that started before a write settled may be the final cached state. */

it('keeps a manual unread posted while the first read is still loading', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const release = fake.holdReads();
  let markUnread!: () => void;
  function Actions() { const receipts = useReadReceipts(); markUnread = () => { void receipts.markUnread('track', 't'); }; return <Verdict id="t" at={50} />; }
  mount(fake, <Actions />);
  await waitFor(() => expect(fake.requests).toHaveLength(1));
  act(() => { void markUnread(); });
  await waitFor(() => expect(fake.stored('track', 't')?.manualUnread).toBe(true));
  act(() => release());
  await screen.findByText('t: unread');
  expect(fake.requests.map((request) => `${request.method} ${request.path}`)).toEqual([
    'GET /api/read-receipts', 'POST /api/read-receipts/track/t/unread', 'GET /api/read-receipts',
  ]);
});

it('acknowledges a target opened while a refetch reveals another device’s manual unread', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100,
    receipts: [{ kind: 'track', targetId: 't', readThroughMs: 200, manualUnread: false }] });
  const view = mount(fake, <Verdict id="t" at={150} />);
  await screen.findByText('t: read');
  // Another device marks it unread; this tab's refetch on focus is still in flight when the target opens.
  await fake.reply({ method: 'POST', path: '/api/read-receipts/track/t/unread', credentials: 'include' });
  const release = fake.holdReads();
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(fake.requests.filter((request) => request.method === 'GET')).toHaveLength(2));
  function Open() { useReadReceipt('track', 't', 150); return <Verdict id="t" at={150} />; }
  view.rerender(<Open />);
  act(() => release());
  await waitFor(() => expect(fake.posts().map((request) => request.path)).toEqual([
    '/api/read-receipts/track/t/unread', '/api/read-receipts/track/t/read',
  ]));
  await waitFor(() => expect(fake.stored('track', 't')?.manualUnread).toBe(false));
  await screen.findByText('t: read');
});

it('keeps a second write optimistic until it settles, whatever the first write reads back', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  let markUnread!: (id: string) => void;
  function Actions() {
    const receipts = useReadReceipts();
    markUnread = (id) => { void receipts.markUnread('track', id); };
    return <><Verdict id="a" at={50} /><Verdict id="b" at={50} /></>;
  }
  mount(fake, <Actions />);
  await screen.findByText('b: read');
  const writes = fake.holdWrites();
  act(() => { markUnread('a'); markUnread('b'); });
  await screen.findByText('b: unread');
  await waitFor(() => expect(writes.held()).toBe(2));
  act(() => writes.release());
  await waitFor(() => expect(fake.stored('track', 'a')?.manualUnread).toBe(true));
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(screen.getByText('a: unread')).toBeTruthy();
  expect(screen.getByText('b: unread')).toBeTruthy();
  act(() => writes.release());
  await waitFor(() => expect(fake.requests.map((request) => request.method)).toEqual(['GET', 'POST', 'POST', 'GET']));
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(screen.getByText('a: unread')).toBeTruthy();
  expect(screen.getByText('b: unread')).toBeTruthy();
});

/* #2596 fix round 2: a target's writes reach the kernel in the order they were made. */
it('sends one target’s writes in order, even over a network that answers out of order', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  let receipts!: ReturnType<typeof useReadReceipts>;
  function Actions() { receipts = useReadReceipts(); return <Verdict id="t" at={50} />; }
  mount(fake, <Actions />);
  await screen.findByText('t: read');
  const writes = fake.holdWrites();
  act(() => { void receipts.markUnread('track', 't'); receipts.markRead('track', 't', 50); });
  await waitFor(() => expect(writes.held()).toBeGreaterThan(0));
  await new Promise((resolve) => setTimeout(resolve, 50));
  act(() => writes.releaseLatest());
  await waitFor(() => expect(writes.held()).toBeGreaterThan(0));
  act(() => writes.releaseLatest());
  // Both writes have committed once the last of them reads the receipts back.
  await waitFor(() => expect(writes.held()).toBe(0));
  await waitFor(() => expect(fake.requests.at(-1)?.method).toBe('GET'));
  expect(fake.posts().map((request) => request.path)).toEqual([
    '/api/read-receipts/track/t/unread', '/api/read-receipts/track/t/read',
  ]);
  expect(fake.stored('track', 't')?.manualUnread).toBe(false);
  await screen.findByText('t: read');
});

/* #2596 fix round 3: an acknowledgement is owed until the kernel stored it. */
it('acknowledges a target opened offline once the browser is back online', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const view = mount(fake, <Verdict id="t" at={150} />);
  await screen.findByText('t: unread');
  act(() => onlineManager.setOnline(false));
  function Open() { useReadReceipt('track', 't', 150); return <Verdict id="t" at={150} />; }
  view.rerender(<Open />);
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(fake.posts()).toEqual([]);
  // Back online, with no navigation and no visibility change: the acknowledgement goes out before any read answers.
  const release = fake.holdReads();
  act(() => onlineManager.setOnline(true));
  await waitFor(() => expect(fake.posts().map((request) => [request.path, request.body])).toEqual([
    ['/api/read-receipts/track/t/read', { readThroughMs: 150 }],
  ]));
  act(() => release());
  await screen.findByText('t: read');
});

it('keeps a failed acknowledgement owed until the next focus', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100, fail: true });
  function Open() { useReadReceipt('track', 't', 150); return <Verdict id="t" at={150} />; }
  mount(fake, <Open />);
  await waitFor(() => expect(fake.posts()).toHaveLength(1));
  await screen.findByText('t: unread');
  // The read-back after the failed write is no trigger: nothing loops.
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(fake.posts()).toHaveLength(1);
  fake.failWrites(false);
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(fake.posts()).toHaveLength(2));
  await screen.findByText('t: read');
  expect(fake.stored('track', 't')).toMatchObject({ readThroughMs: 150, manualUnread: false });
});

/* #2596 fix round 4: an owed acknowledgement is one record, and the provider's own read-back is one fetch. */
it('keeps an acknowledgement recorded after a manual unread, behind an earlier one still in flight', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const writes = fake.holdWrites();
  let receipts!: ReturnType<typeof useReadReceipts>;
  function Open({ opened }: { opened: number }) {
    receipts = useReadReceipts();
    useReadReceipt('track', 't', 150, true, opened);
    return <Verdict id="t" at={150} />;
  }
  const view = mount(fake, <Open opened={1} />);
  await waitFor(() => expect(writes.held()).toBe(1));
  act(() => { void receipts.markUnread('track', 't'); });
  view.rerender(<Open opened={2} />);
  for (let write = 0; write < 3; write += 1) {
    await waitFor(() => expect(writes.held()).toBeGreaterThan(0));
    act(() => writes.release());
  }
  await waitFor(() => expect(fake.requests.at(-1)?.method).toBe('GET'));
  expect(fake.posts().map((request) => request.path)).toEqual([
    '/api/read-receipts/track/t/read', '/api/read-receipts/track/t/unread', '/api/read-receipts/track/t/read',
  ]);
  expect(fake.stored('track', 't')).toMatchObject({ readThroughMs: 150, manualUnread: false });
  await screen.findByText('t: read');
});

it('retries a failed acknowledgement on the next focus, even after its read-back failed', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const view = mount(fake, <Verdict id="t" at={150} />);
  await screen.findByText('t: unread');
  fake.failWrites(true);
  fake.failReads(true);
  function Open() { useReadReceipt('track', 't', 150); return <Verdict id="t" at={150} />; }
  view.rerender(<Open />);
  await waitFor(() => expect(fake.posts()).toHaveLength(1));
  await waitFor(() => expect(fake.requests.at(-1)?.method).toBe('GET'));
  await new Promise((resolve) => setTimeout(resolve, 50));
  fake.failWrites(false);
  fake.failReads(false);
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(fake.posts()).toHaveLength(2));
  await screen.findByText('t: read');
});

/* #2596 fix round 5: every reader-visible event owes its own acknowledgement, and no fired trigger is lost. */
it('acknowledges a reopen at the same instant after another device marked it unread while the first answer was held', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const answers = fake.holdAnswers();
  function Open({ opened }: { opened: number }) { useReadReceipt('track', 't', 150, true, opened); return <Verdict id="t" at={150} />; }
  const view = mount(fake, <Open opened={1} />);
  await waitFor(() => expect(answers.held()).toBe(1));
  expect(fake.stored('track', 't')).toMatchObject({ readThroughMs: 150, manualUnread: false });
  fake.store({ kind: 'track', targetId: 't', readThroughMs: 150, manualUnread: true });
  view.rerender(<Open opened={2} />);
  act(() => answers.release());
  await waitFor(() => expect(fake.posts()).toHaveLength(2));
  await waitFor(() => expect(answers.held()).toBe(1));
  act(() => answers.release());
  await waitFor(() => expect(fake.requests.at(-1)?.method).toBe('GET'));
  expect(fake.stored('track', 't')).toMatchObject({ readThroughMs: 150, manualUnread: false });
  await screen.findByText('t: read');
});

it('sends once more after a failed attempt when a trigger fired while it was in flight', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100, fail: true });
  const writes = fake.holdWrites();
  function Open() { useReadReceipt('track', 't', 150); return <Verdict id="t" at={150} />; }
  mount(fake, <Open />);
  await waitFor(() => expect(writes.held()).toBe(1));
  // A receipts read lands while the acknowledgement is in flight: a trigger its flush has to skip.
  const reads = fake.requests.filter((request) => request.method === 'GET').length;
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(fake.requests.filter((request) => request.method === 'GET').length).toBeGreaterThan(reads));
  await new Promise((resolve) => setTimeout(resolve, 50));
  act(() => writes.release());
  await waitFor(() => expect(writes.held()).toBe(1));
  fake.failWrites(false);
  act(() => writes.release());
  await waitFor(() => expect(fake.stored('track', 't')).toMatchObject({ readThroughMs: 150, manualUnread: false }));
  expect(fake.posts()).toHaveLength(2);
  await screen.findByText('t: read');
});

it('keeps a newer acknowledgement owed when an older one commits and the newer one fails', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const writes = fake.holdWrites();
  function Open({ at }: { at: number }) { useReadReceipt('track', 't', at); return <Verdict id="t" at={at} />; }
  const view = mount(fake, <Open at={150} />);
  await waitFor(() => expect(writes.held()).toBe(1));
  view.rerender(<Open at={160} />);
  act(() => writes.release());
  await waitFor(() => expect(fake.stored('track', 't')?.readThroughMs).toBe(150));
  await waitFor(() => expect(writes.held()).toBe(1));
  fake.failWrites(true);
  act(() => writes.release());
  await waitFor(() => expect(fake.requests.at(-1)?.method).toBe('GET'));
  await new Promise((resolve) => setTimeout(resolve, 50));
  fake.failWrites(false);
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(writes.held()).toBe(1));
  act(() => writes.release());
  await waitFor(() => expect(fake.stored('track', 't')?.readThroughMs).toBe(160));
  await screen.findByText('t: read');
});

it('replays a missed trigger for its own target only, so two failing targets do not keep replaying each other', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100, fail: true, writeDelayMs: 20 });
  function Open() {
    useReadReceipt('track', 't', 150);
    useReadReceipt('conversation', 'c', 150);
    return <Verdict id="t" at={150} />;
  }
  mount(fake, <Open />);
  await waitFor(() => expect(fake.posts().length).toBeGreaterThanOrEqual(2));
  await new Promise((resolve) => setTimeout(resolve, 300));
  const settled = fake.posts().length;
  expect(settled).toBeLessThanOrEqual(3);
  await new Promise((resolve) => setTimeout(resolve, 200));
  expect(fake.posts()).toHaveLength(settled);
});

/* #2596 fix round 6: triggers are external signals only; a receipt write never causes one. */
it('sends failing acknowledgements once per trigger, however long the reader waits', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100, fail: true });
  fake.capWrites(40);
  function Open() {
    useReadReceipt('track', 'a', 150);
    useReadReceipt('track', 'b', 150);
    useReadReceipt('conversation', 'c', 150);
    return <Verdict id="a" at={150} />;
  }
  mount(fake, <Open />);
  // Three records, each a trigger: a and b were in flight when the later records flushed, so each is sent once more.
  await waitFor(() => expect(fake.posts().length).toBeGreaterThanOrEqual(5));
  await new Promise((resolve) => setTimeout(resolve, 300));
  expect(fake.posts()).toHaveLength(5);
  // One focus: one more attempt per owed target, then quiet again.
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(fake.posts().length).toBeGreaterThanOrEqual(8));
  await new Promise((resolve) => setTimeout(resolve, 300));
  expect(fake.posts()).toHaveLength(8);
});

it('retries an acknowledgement whose read-back an outside refresh replaced, on the next focus', async () => {
  const fake = createReadReceiptsKernel({ baselineMs: 100 });
  const view = mount(fake, <Verdict id="t" at={150} />);
  await screen.findByText('t: unread');
  fake.failWrites(true);
  const release = fake.holdReads();
  function Open() { useReadReceipt('track', 't', 150); return <Verdict id="t" at={150} />; }
  view.rerender(<Open />);
  await waitFor(() => expect(fake.posts()).toHaveLength(1));
  const reads = () => fake.requests.filter((request) => request.method === 'GET').length;
  // The provider's read-back is out and held; a global refresh replaces it, then every read answers.
  await new Promise((resolve) => setTimeout(resolve, 50));
  const readBack = reads();
  await act(async () => { void view.client.invalidateQueries(); await Promise.resolve(); });
  await waitFor(() => expect(reads()).toBeGreaterThan(readBack));
  act(() => release());
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(fake.posts()).toHaveLength(1);
  fake.failWrites(false);
  act(() => { window.dispatchEvent(new Event('visibilitychange')); });
  await waitFor(() => expect(fake.posts()).toHaveLength(2));
  await screen.findByText('t: read');
});
