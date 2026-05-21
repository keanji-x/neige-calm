// ConfirmDialog — a small opinionated wrapper around `<Dialog>` for
// destructive-action confirmations (delete wave, remove cove, etc.).
//
// What this primitive adds on top of Dialog
// -----------------------------------------
// 1. **Cancel-safe default focus.** Initial focus lands on the *Cancel*
//    button, not Confirm. A user who reflexively mashes Enter when the
//    dialog appears will dismiss it, not nuke their data. Implemented by
//    passing Dialog's `initialFocusRef` to our Cancel button.
//
// 2. **Esc + outside-click route to onCancel.** Dialog already handles
//    Esc and outside-click via its `onClose` prop; we wire `onClose` to
//    `onCancel` so call sites don't have to think about two distinct
//    dismissal callbacks. The contract: every non-confirm exit (Esc,
//    overlay click, Cancel button, header X) calls `onCancel`.
//
// 3. **Destructive visual.** The Confirm button gets the `go warn` class
//    (existing red-ish variant in `web/src/calm.css`) when `destructive`
//    is true (the default for this primitive). Pass `destructive={false}`
//    for a non-destructive confirmation (e.g. "Save changes?").
//
// What this primitive deliberately does NOT do
// --------------------------------------------
// - No focus trap of its own — inherited from Dialog.
// - No Esc handler of its own — inherited from Dialog.
// - No new CSS — uses existing `go` / `go warn` / `go outline` classes.
// - No async / loading state — call sites that need a spinner can layer
//   that into their `onConfirm` or render their own inline indicator.
//
// See `web/src/ui/README.md` for the Visual / A11y / Test contracts and
// `ConfirmDialog.contract.test.tsx` for the executable behavior spec.

import { useRef } from 'react';
import type { ReactNode } from 'react';
import { Dialog } from '../Dialog/Dialog';

export interface ConfirmDialogProps {
  /** Whether the dialog is mounted. */
  open: boolean;
  /** Dialog title — also becomes the dialog's accessible name. */
  title: string;
  /** Body content explaining what the destructive action will do. */
  description?: ReactNode;
  /** Label on the Confirm (destructive) button. Defaults to `"Confirm"`. */
  confirmLabel?: string;
  /** Label on the Cancel (safe) button. Defaults to `"Cancel"`. */
  cancelLabel?: string;
  /** Called when the user activates Confirm. */
  onConfirm: () => void;
  /** Called when the user dismisses via Cancel button, Esc, overlay
   *  click, or the header close button. */
  onCancel: () => void;
  /** When true (the default), Confirm gets the warn-variant button
   *  styling (`go warn`). Set to false for non-destructive confirmations
   *  where the primary action should not look dangerous. */
  destructive?: boolean;
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
        >
          {confirmLabel}
        </button>
      </div>
    </Dialog>
  );
}
