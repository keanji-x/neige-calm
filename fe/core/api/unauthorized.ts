export interface MicrotaskSchedulerPort {
  enqueue(task: () => void): void;
}

export interface UnauthorizedListenerErrorPort {
  report(error: unknown): void;
}

export interface UnauthorizedChannel {
  subscribe(listener: () => void): () => void;
  notify(): void;
}

/** Creates an instance-owned listener lifecycle; scheduling and reporting are injected ports. */
export function createUnauthorizedChannel(
  scheduler: MicrotaskSchedulerPort,
  errors?: UnauthorizedListenerErrorPort,
): UnauthorizedChannel {
  const listeners = new Set<() => void>();
  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    notify() {
      scheduler.enqueue(() => {
        for (const listener of listeners) {
          try {
            listener();
          } catch (error) {
            errors?.report(error);
          }
        }
      });
    },
  };
}
