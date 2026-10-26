import { useEffect, useRef, type ReactNode } from 'react';
import { ConfirmDialog, type DialogOrigin } from '../dialog/public.tsx';
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

export type DeleteConfirmState = Readonly<{
  target: string | null; origin: DialogOrigin; open: boolean; pending: boolean;
  feedback: OperationFeedbackState;
  request: (id: string, origin: DialogOrigin) => void;
  cancel: () => void;
  confirm: () => void;
}>;

export function DeleteConfirmDialog({ confirm, copy, busyLabel = 'Deleting…', body, blocked = false }: Readonly<{
  confirm: DeleteConfirmState;
  copy: Readonly<{ title: string; description?: ReactNode; confirmLabel: string }>;
  busyLabel?: string;
  body?: ReactNode;
  blocked?: boolean;
}>) {
  return <ConfirmDialog origin={confirm.origin} open={confirm.open} title={copy.title}
    description={<>{body ?? copy.description}<OperationFeedback feedback={confirm.feedback} /></>}
    confirmLabel={copy.confirmLabel} confirmBusyLabel={busyLabel}
    confirmState={confirm.pending ? 'busy' : blocked ? 'blocked' : 'ready'}
    onConfirm={confirm.confirm} onCancel={confirm.cancel} />;
}

export function useDeleteConfirm(
  perform: (id: string, signal: AbortSignal) => void | Promise<void>,
  read: FailureReading,
  onDone?: () => void,
): DeleteConfirmState {
  const center: DialogOrigin = { kind: 'surface-center' };
  const [intent, setIntent] = useState<Readonly<{ id: string; origin: DialogOrigin }> | null>(null);
  const [pending, setPending] = useState(false);
  const active = useRef<AbortController | null>(null);
  const feedback = useOperationFeedback();
  useEffect(() => () => { active.current?.abort(); }, []);
  return {
    target: intent?.id ?? null,
    origin: intent?.origin ?? center,
    open: intent !== null,
    pending,
    feedback,
    request: (id: string, origin: DialogOrigin) => { feedback.clear(); setIntent({ id, origin }); },
    // Closing aborts the request and releases this target; no delete may outlive the dialog that owns its consequences.
    cancel: () => { active.current?.abort(); active.current = null; setPending(false); setIntent(null); },
    confirm: () => {
      if (pending || intent === null) return;
      const controller = new AbortController();
      active.current = controller;
      setPending(true);
      void feedback.run(Promise.resolve().then(() => perform(intent.id, controller.signal)), read, () => controller.signal.aborted)
        .then((deleted) => {
          if (active.current !== controller) return;
          if (deleted) { setIntent(null); onDone?.(); }
        })
        .finally(() => {
          if (active.current !== controller) return;
          active.current = null; setPending(false);
        });
    },
  };
}
