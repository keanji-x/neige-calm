import { afterEach, expect, it, vi } from 'vitest';
import { runWorkerTask, serveWorkerTask } from './public.ts';

class Endpoint extends EventTarget {
  peer: Endpoint | null = null;
  postMessage = vi.fn((message: unknown) => {
    const copy: unknown = structuredClone(message);
    queueMicrotask(() => { this.peer?.dispatchEvent(new MessageEvent('message', { data: copy })); });
  });
  terminate = vi.fn();
}

afterEach(() => { vi.restoreAllMocks(); });

function task() {
  const worker = new Endpoint();
  const controller = new AbortController();
  const remove = vi.spyOn(worker, 'removeEventListener');
  const signalRemove = vi.spyOn(controller.signal, 'removeEventListener');
  const result = runWorkerTask<string, unknown>(() => worker, 'input', controller.signal);
  return { worker, controller, result, remove, signalRemove };
}

function closed(value: ReturnType<typeof task>) {
  expect(value.worker.terminate).toHaveBeenCalledTimes(1);
  expect(value.remove.mock.calls.map(([type]) => type)).toEqual(['message', 'error', 'messageerror']);
  expect(value.signalRemove).toHaveBeenCalledTimes(1);
  expect(value.signalRemove.mock.calls[0][0]).toBe('abort');
}

it('runs the actual one-shot server envelope through a dedicated cloned channel', async () => {
  const worker = new Endpoint(); const scope = new Endpoint();
  worker.peer = scope; scope.peer = worker;
  const execute = vi.fn((input: Map<string, number>) => ({ values: input, total: input.get('count')! + 1 }));
  const stop = serveWorkerTask(scope, execute);
  const input = new Map([['count', 4]]);
  const output = await runWorkerTask<Map<string, number>, { values: Map<string, number>; total: number }>(
    () => worker, input, new AbortController().signal,
  );
  expect(output).toEqual({ values: input, total: 5 });
  expect(output.values).toBeInstanceOf(Map);
  expect(output.values).not.toBe(input);
  expect(execute).toHaveBeenCalledTimes(1);
  expect(worker.terminate).toHaveBeenCalledTimes(1);
  scope.dispatchEvent(new MessageEvent('message', { data: worker.postMessage.mock.calls[0][0] }));
  expect(execute).toHaveBeenCalledTimes(1);
  stop();
});

it('creates no Worker for a pre-aborted lease and rejects creation failures', async () => {
  const controller = new AbortController(); controller.abort();
  const factory = vi.fn(() => new Endpoint());
  await expect(runWorkerTask(factory, null, controller.signal)).rejects.toMatchObject({ name: 'AbortError' });
  expect(factory).not.toHaveBeenCalled();
  const error = new Error('Worker construction failed');
  await expect(runWorkerTask(() => { throw error; }, null, new AbortController().signal)).rejects.toBe(error);
});

it('cleans up success once and ignores duplicate or late terminal events', async () => {
  const value = task();
  value.worker.dispatchEvent(new MessageEvent('message', { data: { type: 'success', output: undefined } }));
  await expect(value.result).resolves.toBeUndefined();
  value.worker.dispatchEvent(new ErrorEvent('error', { message: 'late error' }));
  value.worker.dispatchEvent(new MessageEvent('message', { data: { type: 'success', output: 'late' } }));
  value.controller.abort();
  closed(value);
});

it.each([
  ['failure', 'Execution refused'], ['error', 'Runtime crash'],
  ['messageerror', 'Worker task response could not be decoded'],
  ['malformed', 'Invalid worker task response'],
] as const)('rejects %s and releases the complete channel', async (kind, expected) => {
  const value = task();
  const rejected = expect(value.result).rejects.toThrow(expected);
  if (kind === 'error') {
    const event = new ErrorEvent('error', { message: expected, cancelable: true });
    value.worker.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
  }
  else if (kind === 'messageerror') value.worker.dispatchEvent(new MessageEvent('messageerror'));
  else value.worker.dispatchEvent(new MessageEvent('message', { data: kind === 'failure'
    ? { type: 'failure', error: expected } : { type: 'unknown-terminal' } }));
  await rejected;
  closed(value);
});

it('rejects a postMessage clone failure and terminates the created Worker', async () => {
  const worker = new Endpoint();
  worker.postMessage.mockImplementation(() => { throw new DOMException('Input cannot be cloned', 'DataCloneError'); });
  const remove = vi.spyOn(worker, 'removeEventListener');
  await expect(runWorkerTask(() => worker, null, new AbortController().signal)).rejects.toMatchObject({ name: 'DataCloneError' });
  expect(worker.terminate).toHaveBeenCalledTimes(1);
  expect(remove).toHaveBeenCalledTimes(3);
});

it('aborts running work and a lease aborted during Worker creation', async () => {
  const value = task();
  const rejected = expect(value.result).rejects.toMatchObject({ name: 'AbortError' });
  value.controller.abort(); await rejected;
  closed(value);
  const controller = new AbortController(); const worker = new Endpoint();
  await expect(runWorkerTask(() => { controller.abort(); return worker; }, null, controller.signal))
    .rejects.toMatchObject({ name: 'AbortError' });
  expect(worker.postMessage).not.toHaveBeenCalled();
  expect(worker.terminate).toHaveBeenCalledTimes(1);
});

it('returns execution and output clone failures from the actual server', async () => {
  for (const execute of [() => { throw new Error('Algorithm failed'); }, () => ({ uncloneable: () => null })]) {
    const worker = new Endpoint(); const scope = new Endpoint();
    worker.peer = scope; scope.peer = worker;
    serveWorkerTask(scope, execute);
    await expect(runWorkerTask(() => worker, null, new AbortController().signal)).rejects.toBeInstanceOf(Error);
    expect(scope.postMessage.mock.calls.at(-1)?.[0]).toMatchObject({ type: 'failure' });
    expect(worker.terminate).toHaveBeenCalledTimes(1);
  }
});

it('fails malformed requests and lets scope disposal suppress a pending result', async () => {
  const malformed = new Endpoint(); const execute = vi.fn(() => 'unused');
  const remove = vi.spyOn(malformed, 'removeEventListener');
  serveWorkerTask(malformed, execute);
  malformed.dispatchEvent(new MessageEvent('message', { data: { type: 'run' } }));
  await Promise.resolve();
  expect(execute).not.toHaveBeenCalled();
  expect(malformed.postMessage).toHaveBeenCalledWith({ type: 'failure', error: 'Invalid worker task request' });
  expect(remove).toHaveBeenCalledTimes(1);

  const scope = new Endpoint();
  let resolve!: (value: string) => void;
  const pending = new Promise<string>(done => { resolve = done; });
  const stop = serveWorkerTask(scope, () => pending);
  scope.dispatchEvent(new MessageEvent('message', { data: { type: 'run', input: 'valid' } }));
  stop(); stop(); resolve('late');
  await pending; await Promise.resolve();
  expect(scope.postMessage).not.toHaveBeenCalled();
});
