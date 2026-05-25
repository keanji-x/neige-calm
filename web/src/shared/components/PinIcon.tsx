// Up/down arrow glyph for pin/unpin — direction indicates the action available
// (↑ = "raise to top / pin", ↓ = "lower / unpin").
export function PinIcon({ down = false }: { down?: boolean }) {
  return (
    <svg
      width="1em"
      height="1em"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M12 5 L12 19" />
      {down ? <path d="M6 13 L12 19 L18 13" /> : <path d="M6 11 L12 5 L18 11" />}
    </svg>
  );
}
