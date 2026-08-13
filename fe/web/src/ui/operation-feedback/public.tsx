import type { ReactNode } from 'react';

import { useState } from '../state/public.ts';

export type OperationFeedbackState = Readonly<{
  error: string | null;
  clear: () => void;
  run: (operation: Promise<unknown>, fallback: string) => Promise<boolean>;
}>;

function messageOf(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() !== '' ? error.message : fallback;
}

/** One handled rejection path for rename/delete writes across every surface. */
export function useOperationFeedback(): OperationFeedbackState {
  const [error, setError] = useState<string | null>(null);
  return {
    error,
    clear: () => setError(null),
    run: async (operation, fallback) => {
      setError(null);
      try {
        await operation;
        return true;
      } catch (reason) {
        setError(messageOf(reason, fallback));
        return false;
      }
    },
  };
}

export function OperationFeedback({ feedback, children }: {
  feedback: OperationFeedbackState;
  children?: ReactNode;
}) {
  if (feedback.error === null) return null;
  return <div role="alert" data-nc-error-box="">{children ?? feedback.error}</div>;
}

export function useDeleteConfirm(
  perform: (id: string) => void | Promise<void>,
  onDone?: () => void,
) {
  const [target, setTarget] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const feedback = useOperationFeedback();
  return {
    target,
    open: target !== null,
    pending,
    feedback,
    request: (id: string) => { feedback.clear(); setTarget(id); },
    cancel: () => { if (!pending) setTarget(null); },
    confirm: () => {
      if (pending || target === null) return;
      setPending(true);
      void feedback.run(Promise.resolve().then(() => perform(target)), 'Could not delete this item.')
        .then((deleted) => { if (deleted) onDone?.(); })
        .finally(() => { setPending(false); setTarget(null); });
    },
  };
}
