/** One dedicated channel per task. Native Worker structurally satisfies this port. */
export type WorkerTaskPort = Readonly<{
  postMessage: (message: unknown) => void;
  addEventListener: (type: 'message' | 'error' | 'messageerror', listener: EventListener) => void;
  removeEventListener: (type: 'message' | 'error' | 'messageerror', listener: EventListener) => void;
  terminate: () => void;
}>;

export type WorkerTaskScope = Readonly<{
  postMessage: (message: unknown) => void;
  addEventListener: (type: 'message', listener: EventListener) => void;
  removeEventListener: (type: 'message', listener: EventListener) => void;
}>;

type Request<Input> = Readonly<{ type: 'run'; input: Input }>;
type Reply<Output> = Readonly<{ type: 'success'; output: Output } | { type: 'failure'; error: string }>;

function errorOf(value: unknown): Error {
  return value instanceof Error || value instanceof DOMException ? value : new Error(String(value));
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** A task owns its Worker and all listeners until exactly one terminal outcome. */
export function runWorkerTask<Input, Output>(
  createWorker: () => WorkerTaskPort, input: Input, signal: AbortSignal,
): Promise<Output> {
  return new Promise<Output>((resolve, reject) => {
    if (signal.aborted) { reject(errorOf(signal.reason)); return; }
    let worker: WorkerTaskPort;
    try { worker = createWorker(); } catch (error) { reject(errorOf(error)); return; }
    let settled = false;
    const finish = (result: Readonly<{ ok: true; value: Output } | { ok: false; error: unknown }>) => {
      if (settled) return;
      settled = true;
      worker.removeEventListener('message', message);
      worker.removeEventListener('error', error);
      worker.removeEventListener('messageerror', messageError);
      signal.removeEventListener('abort', abort);
      worker.terminate();
      if (result.ok) resolve(result.value); else reject(errorOf(result.error));
    };
    const message: EventListener = event => {
      const reply: unknown = (event as MessageEvent<unknown>).data;
      if (record(reply) && reply.type === 'success' && Object.hasOwn(reply, 'output')) {
        finish({ ok: true, value: reply.output as Output });
      } else if (record(reply) && reply.type === 'failure' && typeof reply.error === 'string') {
        finish({ ok: false, error: new Error(reply.error) });
      } else {
        finish({ ok: false, error: new Error('Invalid worker task response') });
      }
    };
    const error: EventListener = event => {
      event.preventDefault();
      const detail = event as ErrorEvent;
      finish({ ok: false, error: new Error(detail.message || 'Worker task failed') });
    };
    const messageError: EventListener = () => { finish({ ok: false, error: new Error('Worker task response could not be decoded') }); };
    const abort = () => { finish({ ok: false, error: signal.reason }); };
    try {
      worker.addEventListener('message', message);
      worker.addEventListener('error', error);
      worker.addEventListener('messageerror', messageError);
      signal.addEventListener('abort', abort, { once: true });
      if (signal.aborted) { abort(); return; }
      const request: Request<Input> = { type: 'run', input };
      worker.postMessage(request);
    } catch (cause) { finish({ ok: false, error: errorOf(cause) }); }
  });
}

/** One-shot server for a feature-owned algorithm. Disposing suppresses any pending reply. */
export function serveWorkerTask<Input, Output>(
  scope: WorkerTaskScope, execute: (input: Input) => Output | Promise<Output>,
): () => void {
  let open = true;
  let listening = true;
  const removeListener = () => {
    if (!listening) return;
    listening = false;
    scope.removeEventListener('message', receive);
  };
  const post = (reply: Reply<Output>) => { if (open) scope.postMessage(reply); };
  const run = async (request: unknown) => {
    try {
      if (!record(request) || request.type !== 'run' || !Object.hasOwn(request, 'input')) {
        throw new Error('Invalid worker task request');
      }
      const output = await execute(request.input as Input);
      post({ type: 'success', output });
    } catch (error) {
      post({ type: 'failure', error: errorOf(error).message });
    } finally { open = false; }
  };
  const receive: EventListener = event => {
    if (!open || !listening) return;
    removeListener();
    void run((event as MessageEvent<unknown>).data);
  };
  scope.addEventListener('message', receive);
  return () => { open = false; removeListener(); };
}
