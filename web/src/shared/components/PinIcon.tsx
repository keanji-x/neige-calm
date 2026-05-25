// Inline SVG pushpin glyph — stroke-only to match CloseIcon/PlusIcon line style.
export function PinIcon() {
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
      <path d="M7 6 L17 6" />
      <path d="M9 9 L15 9" />
      <path d="M12 9 L12 18" />
    </svg>
  );
}
