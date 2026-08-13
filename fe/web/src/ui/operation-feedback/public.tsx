import { useEffect, useRef, type ReactNode } from 'react';

import { useState } from '../state/public.ts';

export type OperationFeedbackState = Readonly<{
  error: string | null;
  clear: () => void;
  run: (operation: Promise<unknown>, fallback: string, ignore?: () => boolean) => Promise<boolean>;
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
    run: async (operation, fallback, ignore) => {
      setError(null);
      try {
        await operation;
        return true;
      } catch (reason) {
        if (ignore?.()) return false;
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
  perform: (id: string, signal: AbortSignal) => void | Promise<void>,
  onDone?: () => void,
) {
  const [target, setTarget] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const active = useRef<AbortController | null>(null);
  const feedback = useOperationFeedback();
  useEffect(() => () => { active.current?.abort(); }, []);
  return {
    target,
    open: target !== null,
    pending,
    feedback,
    request: (id: string) => { feedback.clear(); setTarget(id); },
    // INV-CONFIRM-001 — closing aborts the request and releases this target;
    // no delete is allowed to outlive the dialog that owns its consequences.
    cancel: () => { active.current?.abort(); active.current = null; setPending(false); setTarget(null); },
    confirm: () => {
      if (pending || target === null) return;
      const controller = new AbortController();
      active.current = controller;
      setPending(true);
      void feedback.run(Promise.resolve().then(() => perform(target, controller.signal)), 'Could not delete this item.', () => controller.signal.aborted)
        .then((deleted) => {
          if (active.current !== controller) return;
          if (deleted) onDone?.();
        })
        .finally(() => {
          if (active.current !== controller) return;
          active.current = null; setPending(false); setTarget(null);
        });
    },
  };
}
