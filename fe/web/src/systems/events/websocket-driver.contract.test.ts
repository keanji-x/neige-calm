// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { EventFrame } from '../../../../core/events/protocol.ts';
import { createFakeSocketFactory } from './fake-socket.ts';
import type { EventStreamSink } from './event-stream.ts';
import { WebSocketDriver } from './websocket-driver.ts';

const configuration = { syncEventVersion: 3, topics: ['*'] as const };
const unauthorized = Object.assign(new Error('unauthorized'), { failure: { kind: 'unauthorized' } });

function harness(options: { probe?: () => Promise<unknown>; notify?: () => void; throwOnConstruct?: () => Error | undefined } = {}) {
  const fake = createFakeSocketFactory({ ...(options.throwOnConstruct ? { throwOnConstruct: options.throwOnConstruct } : {}) });
  const states: string[] = [];
  const frames: EventFrame[] = [];
  const sink: EventStreamSink = {
    connectionState: (state) => { states.push(state); },
    frame: (frame) => { frames.push(frame); },
  };
  const driver = new WebSocketDriver({
    cursor: { read: () => 17 },
    probeUnauthorized: options.probe ?? (() => Promise.resolve()),
    onUnauthorized: options.notify ?? (() => undefined),
    socketFactory: fake.factory,
  });
  return { driver, fake, states, frames, sink };
}

async function drain(): Promise<void> { await Promise.resolve(); await Promise.resolve(); }

afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });

describe('WebSocketDriver contracts', () => {
  it('T-D1 T-D3 publishes the protocol subscription frame with the current cursor', () => {
    const { driver, fake, sink } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].open();
    expect(fake.sockets[0].sent).toEqual([JSON.stringify({ sub: ['*'], since: 17 })]);
  });

  it('T-D2 publishes the sticky subscription again after reconnect', () => {
    vi.useFakeTimers();
    const { driver, fake, sink } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].open(); fake.sockets[0].close();
    vi.advanceTimersByTime(0); vi.advanceTimersByTime(500);
    fake.sockets[1].open();
    expect(fake.sockets.map((socket) => socket.sent)).toEqual([
      [JSON.stringify({ sub: ['*'], since: 17 })],
      [JSON.stringify({ sub: ['*'], since: 17 })],
    ]);
  });

  it('T-D4 stays connecting on open and becomes connected only before replay-complete delivery', () => {
    const { driver, fake, sink, states, frames } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].open();
    expect(states).toEqual(['connecting']);
    fake.sockets[0].message(JSON.stringify({ ev: '_replay_complete', _id: 9 }));
    expect(states).toEqual(['connecting', 'connected']);
    expect(frames).toEqual([{ type: 'replay-complete', id: 9 }]);
  });

  it('T-D5a rejects an already-dequeued retry callback after stop', () => {
    vi.useFakeTimers();
    const callbacks: (() => void)[] = [];
    vi.spyOn(globalThis, 'setTimeout').mockImplementation(((callback: TimerHandler) => {
      callbacks.push(callback as () => void); return 1;
    }) as typeof setTimeout);
    const { driver, fake, sink } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close(); callbacks.shift()?.();
    const retry = callbacks.shift()!;
    driver.stop(); retry();
    expect(fake.constructionCount).toBe(1);
  });

  it('T-D5b cancels pending retry and old close work cannot create a second live socket', () => {
    vi.useFakeTimers();
    const clear = vi.spyOn(globalThis, 'clearTimeout');
    const { driver, fake, sink } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close(); vi.advanceTimersByTime(0);
    const pendingTimer = vi.getTimerCount();
    driver.stop(); driver.start(configuration, 'ws://example/api/events', sink);
    vi.advanceTimersByTime(500);
    expect(pendingTimer).toBe(1);
    expect(clear).toHaveBeenCalledWith(expect.anything());
    expect(fake.constructionCount).toBe(2);
  });

  it('T-D5c stop is idempotent before start and after stop', () => {
    const { driver } = harness();
    expect(() => { driver.stop(); driver.stop(); }).not.toThrow();
  });

  it('T-D6 backs off at 500/1000/2000/4000/8000/8000 and resets after open', () => {
    vi.useFakeTimers();
    const { driver, fake, sink } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    for (const delay of [500, 1000, 2000, 4000, 8000, 8000]) {
      fake.sockets.at(-1)!.close(); vi.advanceTimersByTime(0);
      const before = fake.constructionCount;
      vi.advanceTimersByTime(delay - 1); expect(fake.constructionCount).toBe(before);
      vi.advanceTimersByTime(1); expect(fake.constructionCount).toBe(before + 1);
    }
    fake.sockets.at(-1)!.open(); fake.sockets.at(-1)!.close(); vi.advanceTimersByTime(0);
    const before = fake.constructionCount;
    vi.advanceTimersByTime(499); expect(fake.constructionCount).toBe(before);
    vi.advanceTimersByTime(1); expect(fake.constructionCount).toBe(before + 1);
  });

  it('T-D7 probes only a connection that closes before open', async () => {
    vi.useFakeTimers();
    const probe = vi.fn(() => Promise.resolve());
    const { driver, fake, sink } = harness({ probe });
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close(); vi.advanceTimersByTime(0); await drain();
    expect(probe).toHaveBeenCalledOnce();
    vi.advanceTimersByTime(500); fake.sockets[1].open(); fake.sockets[1].close(); vi.advanceTimersByTime(0); await drain();
    expect(probe).toHaveBeenCalledOnce();
  });

  it('T-D8 permits only one unauthorized probe in flight', async () => {
    vi.useFakeTimers();
    let resolve!: () => void;
    const probe = vi.fn(() => new Promise<void>((done) => { resolve = done; }));
    const { driver, fake, sink } = harness({ probe });
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close(); vi.advanceTimersByTime(0); vi.advanceTimersByTime(500);
    fake.sockets[1].close(); vi.advanceTimersByTime(0);
    expect(probe).toHaveBeenCalledOnce(); resolve(); await drain();
  });

  it('T-D9 continues normal retry and notifies only on an ok-to-unauthorized transition across epochs', async () => {
    vi.useFakeTimers();
    const notify = vi.fn();
    const probe = vi.fn(() => Promise.reject(unauthorized));
    const { driver, fake, sink } = harness({ probe, notify });
    driver.start(configuration, 'ws://example/api/events', sink);
    for (let attempt = 0; attempt < 2; attempt += 1) {
      fake.sockets.at(-1)!.close(); vi.advanceTimersByTime(0); await drain();
      vi.advanceTimersByTime(attempt === 0 ? 500 : 1000);
    }
    expect(notify).toHaveBeenCalledOnce();
    expect(fake.constructionCount).toBe(3);
    driver.stop(); driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets.at(-1)!.close(); vi.advanceTimersByTime(0); await drain();
    expect(notify).toHaveBeenCalledOnce();
  });

  it('T-D10 contains socket construction failures and retries', () => {
    vi.useFakeTimers();
    let throws = true;
    const { driver, fake, sink } = harness({ throwOnConstruct: () => throws ? new Error('bad url') : undefined });
    expect(() => driver.start(configuration, 'bad', sink)).not.toThrow();
    expect(fake.constructionCount).toBe(1);
    throws = false; vi.advanceTimersByTime(500);
    expect(fake.constructionCount).toBe(2);
  });

  it('T-D11 ignores an old epoch probe continuation', async () => {
    vi.useFakeTimers();
    const rejects: Array<(error: unknown) => void> = [];
    const notify = vi.fn();
    const probe = vi.fn(() => new Promise((_, fail) => { rejects.push(fail); }));
    const { driver, fake, sink } = harness({ probe, notify });
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close(); vi.advanceTimersByTime(0);
    driver.stop(); driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[1].close(); vi.advanceTimersByTime(0);
    expect(probe).toHaveBeenCalledTimes(2);
    rejects[0](unauthorized); await drain();
    expect(notify).not.toHaveBeenCalled();
    vi.advanceTimersByTime(999);
    expect(fake.constructionCount).toBe(2);
    vi.advanceTimersByTime(1);
    expect(fake.constructionCount).toBe(3);
  });

  it('T-B3 rechecks epoch after connected delivery before delivering replay-complete', () => {
    const fake = createFakeSocketFactory();
    const frames: EventFrame[] = [];
    const driver = new WebSocketDriver({ cursor: { read: () => null }, probeUnauthorized: () => Promise.resolve(), onUnauthorized: () => undefined, socketFactory: fake.factory });
    const sink: EventStreamSink = {
      connectionState: (state) => { if (state === 'connected') { driver.stop(); driver.start(configuration, 'ws://example/api/events', sink); } },
      frame: (frame) => { frames.push(frame); },
    };
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].open(); fake.sockets[0].message(JSON.stringify({ ev: '_replay_complete', _id: 2 }));
    expect(frames).toEqual([]);
    expect(fake.constructionCount - fake.closeCount).toBe(1);
  });

  it('T-B3 completes close-side driver writes before the reentrant sink boundary', () => {
    vi.useFakeTimers();
    const fake = createFakeSocketFactory();
    const probe = vi.fn(() => new Promise<void>(() => undefined));
    const driver = new WebSocketDriver({ cursor: { read: () => null }, probeUnauthorized: probe, onUnauthorized: () => undefined, socketFactory: fake.factory });
    let restarted = false;
    const sink: EventStreamSink = {
      connectionState: (state) => {
        if (state === 'connecting' && fake.constructionCount === 1 && !restarted) {
          restarted = true;
          driver.stop();
          driver.start(configuration, 'ws://example/api/events', sink);
        }
      },
      frame: () => undefined,
    };
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close(); vi.advanceTimersByTime(0);
    fake.sockets[1].close(); vi.advanceTimersByTime(0);
    expect(probe).toHaveBeenCalledTimes(2);
  });

  it('T-B4 stop prevents any new socket construction from an in-flight old close continuation', () => {
    vi.useFakeTimers();
    const { driver, fake, sink } = harness();
    driver.start(configuration, 'ws://example/api/events', sink);
    fake.sockets[0].close();
    driver.stop();
    driver.start(configuration, 'ws://example/api/events', sink);
    const afterRestart = fake.constructionCount;
    vi.runAllTimers();
    expect(fake.constructionCount).toBe(afterRestart);
  });
});
