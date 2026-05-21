import { useState } from '../shared/state';
import { ConfirmDialog } from '../ui/ConfirmDialog/ConfirmDialog';

// ---------------- Header actions: edit + delete ----------------
//
// Two reusable single-purpose buttons that sit next to a page title.
// Single-action affordances beat a kebab menu here — the icon itself
// says what'll happen, and there's no menu to open and close.

export function IconButton({
  glyph,
  label,
  tone = 'neutral',
  onClick,
  fontSize = 14,
}: {
  glyph: React.ReactNode;
  label: string;
  /** `neutral` greys-up on hover; `danger` shifts to warn-red. */
  tone?: 'neutral' | 'danger';
  onClick: () => void;
  fontSize?: number;
}) {
  const [hover, setHover] = useState(false);
  const dangerStyle = {
    background: hover ? 'var(--warn-soft)' : 'transparent',
    color: hover ? 'var(--warn)' : 'var(--text-3)',
  };
  const neutralStyle = {
    background: hover ? 'oklch(0% 0 0 / 0.04)' : 'transparent',
    color: hover ? 'var(--text-2)' : 'var(--text-3)',
  };
  const tonal = tone === 'danger' ? dangerStyle : neutralStyle;
  return (
    <button
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={label}
      aria-label={label}
      style={{
        width: 26, height: 26,
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        border: 'none', borderRadius: 6,
        font: 'inherit', fontSize, lineHeight: 1, cursor: 'pointer',
        transition: 'color 0.1s, background 0.1s',
        ...tonal,
      }}
    >
      {glyph}
    </button>
  );
}

// DeleteButton — a × icon-button paired with a ConfirmDialog.
//
// Migrated from `window.confirm()` to the `<ConfirmDialog>` primitive
// (issue #60). The old `window.confirm()` made "OK" the keyboard
// default, so an Enter-mash on the prompt would fire the destructive
// action immediately; ConfirmDialog default-focuses Cancel and locks
// that contract in `ConfirmDialog.contract.test.tsx`.
//
// Pattern: stay-open-while-pending. When the user activates Confirm
// the dialog stays mounted; the Confirm button is disabled (via
// `confirmDisabled`) while `onDelete` awaits, so a second Enter or
// click can't re-trigger the delete. Cancel stays enabled — bailing
// out mid-await is intentional and the parent decides what to do if
// the underlying request still resolves. The external prop API
// (`label`, `confirmMessage`, `onDelete`) is unchanged so existing
// call sites compile without edits.
export function DeleteButton({
  label,
  confirmMessage,
  confirmTitle = 'Delete?',
  confirmLabel = 'Delete',
  onDelete,
}: {
  label: string;
  confirmMessage: string;
  /** Dialog title — becomes the dialog's accessible name. Defaults to
   *  `"Delete?"` so existing call sites that only pass `confirmMessage`
   *  still get a sensible (if generic) heading. */
  confirmTitle?: string;
  /** Label on the destructive button. Defaults to `"Delete"`. */
  confirmLabel?: string;
  onDelete: () => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);

  const onConfirm = async () => {
    setPending(true);
    try {
      await onDelete();
    } finally {
      // Always reset both flags so an erroring onDelete doesn't strand
      // the dialog open with a disabled Confirm. The parent can re-open
      // it; we don't try to be smart about replaying the click.
      setPending(false);
      setOpen(false);
    }
  };

  const onCancel = () => {
    // Cancel-during-pending is allowed (ConfirmDialog leaves Cancel
    // enabled even when `confirmDisabled` is true). We simply close
    // the dialog; the in-flight onDelete promise — if any — continues
    // to resolve on the parent's terms.
    setOpen(false);
  };

  return (
    <>
      <IconButton
        glyph="×"
        label={label}
        tone="danger"
        fontSize={18}
        onClick={() => setOpen(true)}
      />
      <ConfirmDialog
        open={open}
        title={confirmTitle}
        description={confirmMessage}
        confirmLabel={confirmLabel}
        cancelLabel="Cancel"
        onConfirm={onConfirm}
        onCancel={onCancel}
        confirmDisabled={pending}
      />
    </>
  );
}
