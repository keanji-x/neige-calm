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
// - No built-in spinner or pending visual — call sites that want one can
//   layer it into `description` or compose it next to the dialog.
//
// What this primitive opts INTO for the "stay open while pending" pattern
// -----------------------------------------------------------------------
// The `confirmDisabled` prop lets a call site keep the dialog mounted
// during an in-flight async confirm without exposing a window where the
// user can fire Confirm again. Cancel stays enabled — the Cancel-safe
// default contract continues to hold even mid-await. The call site is
// still responsible for actually awaiting its handler and flipping
// `confirmDisabled` back to false (or closing) when the work resolves.
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
  /** When true, the Confirm button is disabled. Intended for the
   *  "stay open while pending" pattern: a call site can set this to
   *  `true` after the user clicks Confirm, keep the dialog mounted, and
   *  flip it back to `false` (or close the dialog) once the async work
   *  resolves. Cancel remains enabled — the Cancel-safe default contract
   *  is preserved during the pending window so the user can still back
   *  out. Defaults to `false`. */
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
