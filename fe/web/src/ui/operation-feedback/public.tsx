import { useEffect, useRef, type ReactNode } from 'react';
import { Banner } from '@astryxdesign/core/Banner';

import { useState } from '../state/public.ts';

/**
 * How the caller's write reads a rejection: the sentence to show at the object, or `null` when the answer proves the
 * intent already holds, which counts as success. The caller builds it from the write's failure table; this primitive
 * never reads an error itself.
 */
export type FailureReading = (reason: unknown) => string | null;

export type OperationFeedbackState = Readonly<{
  error: string | null;
  clear: () => void;
  run: (operation: Promise<unknown>, read: FailureReading, ignore?: () => boolean) => Promise<boolean>;
}>;

/** The one non-chat write runner: every rename, pin, close, dismiss and delete settles here. */
export function useOperationFeedback(): OperationFeedbackState {
  const [error, setError] = useState<string | null>(null);
  return {
    error,
    clear: () => setError(null),
    run: async (operation, read, ignore) => {
      setError(null);
      try {
        await operation;
        return true;
      } catch (reason) {
        if (ignore?.()) return false;
        const text = read(reason);
        if (text === null) return true;
        setError(text);
        return false;
      }
    },
  };
}

/**
 * The write's failure where the write was made, as the Astryx error alert every other surface uses: its sentence, the
 * caller's way out (`action`, such as a Try again) when it has one, and Dismiss, which clears it (the banner returns
 * focus to where it came from). A host that closes on blur must treat this as inside itself (see `ui/editable-title`).
 */
export function OperationFeedback({ feedback, action }: {
  feedback: OperationFeedbackState;
  action?: ReactNode;
}) {
  if (feedback.error === null) return null;
  return <Banner status="error" title={feedback.error} endContent={action} isDismissable onDismiss={feedback.clear}
    data-nc-operation-feedback="" />;
}

export function useDeleteConfirm(
  perform: (id: string, signal: AbortSignal) => void | Promise<void>,
  read: FailureReading,
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
    // Closing aborts the request and releases this target; no delete may outlive the dialog that owns its consequences.
    cancel: () => { active.current?.abort(); active.current = null; setPending(false); setTarget(null); },
    confirm: () => {
      if (pending || target === null) return;
      const controller = new AbortController();
      active.current = controller;
      setPending(true);
      void feedback.run(Promise.resolve().then(() => perform(target, controller.signal)), read, () => controller.signal.aborted)
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
