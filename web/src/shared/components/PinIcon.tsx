// Inline SVG pushpin glyph for pin/unpin affordances. Same shape contract
// as CloseIcon and PlusIcon: geometrically centered inside a 24x24 viewBox,
// sized via CSS `1em`, fill="currentColor" so the `.side-wave-pin.pinned`
// CSS `color: var(--accent)` rule takes effect.
export function PinIcon() {
  return (
    <svg
      width="1em"
      height="1em"
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden="true"
      focusable="false"
    >
      {/* Pushpin: vertical pin body + diagonal shaft */}
      <path d="M9 4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v1.586l2.707 2.707A1 1 0 0 1 18 10v1a1 1 0 0 1-1 1h-4v5l-1 2-1-2v-5H7a1 1 0 0 1-1-1v-1a1 1 0 0 1 .293-.707L9 5.586V4z" />
    </svg>
  );
}
