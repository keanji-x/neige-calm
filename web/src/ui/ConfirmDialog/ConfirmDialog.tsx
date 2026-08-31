// Cancel owns initial focus and stays enabled while confirmation is pending.

import { useRef } from 'react';
import type { ReactNode } from 'react';
import { Dialog } from '../Dialog/Dialog';

export interface ConfirmDialogProps {
  open: boolean;
  title: string;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
  destructive?: boolean;
  confirmDisabled?: boolean;
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = 'Confirm',
  cancelLabel = 'Cancel',
  onConfirm,
  onCancel,
  destructive = true,
  confirmDisabled = false,
}: ConfirmDialogProps) {
  // Cancel-safe default: initial focus lands here, so Enter on open
  // dismisses the dialog rather than firing the destructive action.
  const cancelRef = useRef<HTMLButtonElement | null>(null);

  return (
    <Dialog
      open={open}
      onClose={onCancel}
      title={title}
      initialFocusRef={cancelRef}
    >
      {description !== undefined && (
        <div className="confirm-dialog-body">{description}</div>
      )}
      {/* Button order: Cancel on the left, Confirm on the right. The
          right-hand position is the conventional primary-action slot;
          paired with the warn coloring on Confirm, this keeps the
          destructive control unambiguously the "loud" one. */}
      <div
        className="confirm-dialog-actions"
        style={{
          display: 'flex',
          justifyContent: 'flex-end',
          gap: 8,
          marginTop: 16,
        }}
      >
        <button
          ref={cancelRef}
          type="button"
          className="go outline"
          onClick={onCancel}
        >
          {cancelLabel}
        </button>
        <button
          type="button"
          className={destructive ? 'go warn' : 'go'}
          onClick={onConfirm}
          disabled={confirmDisabled}
        >
          {confirmLabel}
        </button>
      </div>
    </Dialog>
  );
}
