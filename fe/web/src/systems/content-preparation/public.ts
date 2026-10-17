import { useEffect } from 'react';
import { useState } from '../../ui/state/public.ts';

export type PreparationStore<Input, Value> = Readonly<{
  peek: (input: Input) => Value | undefined;
  prepare: (input: Input, signal: AbortSignal) => Value | Promise<Value>;
}>;
export type PreparedContentCandidate<Input, Value> = Readonly<{ input: Input; value: Value }>;
type State<Input, Value> =
  | Readonly<{ status: 'waiting' | 'preparing'; candidate: PreparedContentCandidate<Input, Value> | null; error: null }>
  | Readonly<{ status: 'ready'; candidate: PreparedContentCandidate<Input, Value>; error: null }>
  | Readonly<{ status: 'failed'; candidate: PreparedContentCandidate<Input, Value> | null; error: Error }>;
export type PreparedContentResource<Input, Value> = State<Input, Value> & Readonly<{ retry: () => void }>;
type Snapshot<Input, Value> = Readonly<{
  selectionKey: string;
  input: Input;
  store: PreparationStore<Input, Value>;
  enabled: boolean;
  state: State<Input, Value>;
}>;

function stateFor<Input, Value>(
  input: Input, store: PreparationStore<Input, Value>, enabled: boolean,
  previous: PreparedContentCandidate<Input, Value> | null,
): State<Input, Value> {
  const value = store.peek(input);
  return value === undefined
    ? { status: enabled ? 'preparing' : 'waiting', candidate: previous, error: null }
    : { status: 'ready', candidate: { input, value }, error: null };
}

/** Preparation data only: a retained candidate is never evidence that content committed. */
export function usePreparedContent<Input, Value>(
  selectionKey: string, input: Input, store: PreparationStore<Input, Value>, enabled: boolean,
): PreparedContentResource<Input, Value> {
  const [snapshot, setSnapshot] = useState<Snapshot<Input, Value>>(() => ({
    selectionKey, input, store, enabled, state: stateFor(input, store, enabled, null),
  }));
  let current = snapshot;
  const cached = store.peek(input);
  const sameSelection = snapshot.selectionKey === selectionKey && snapshot.store === store;
  if (!sameSelection || !Object.is(snapshot.input, input) || snapshot.enabled !== enabled
    || (cached !== undefined && !Object.is(snapshot.state.candidate?.value, cached))) {
    current = { selectionKey, input, store, enabled,
      state: stateFor(input, store, enabled, sameSelection ? snapshot.state.candidate : null) };
    setSnapshot(current);
  }

  useEffect(() => {
    if (!current.enabled || current.state.status !== 'preparing') return;
    const controller = new AbortController();
    const lease = current;
    const failed = (cause: unknown) => {
      if (controller.signal.aborted) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      setSnapshot(previous => previous !== lease ? previous : { ...previous,
        state: { status: 'failed', candidate: previous.state.candidate, error } });
    };
    try {
      void Promise.resolve(store.prepare(current.input, controller.signal)).then(value => {
        if (controller.signal.aborted) return;
        setSnapshot(previous => previous !== lease ? previous : { ...previous,
          state: { status: 'ready', candidate: { input: lease.input, value }, error: null } });
      }, failed);
    } catch (cause) { failed(cause); }
    return () => { controller.abort(); };
  }, [current, store]);

  const retry = () => {
    setSnapshot(previous => previous !== current ? previous : { ...previous,
      state: stateFor(input, store, enabled, previous.state.candidate) });
  };
  return { ...current.state, retry };
}
