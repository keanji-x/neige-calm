import { useState } from '../shared/state';

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

export function DeleteButton({
  label,
  confirmMessage,
  onDelete,
}: {
  label: string;
  confirmMessage: string;
  onDelete: () => void | Promise<void>;
}) {
  return (
    <IconButton
      glyph="×"
      label={label}
      tone="danger"
      fontSize={18}
      onClick={async () => {
        if (!window.confirm(confirmMessage)) return;
        await onDelete();
      }}
    />
  );
}
