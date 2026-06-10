// Inline SVG `+` glyph for create affordances. Same shape contract as
// CloseIcon: geometrically centered inside a 24x24 viewBox and sized via
// CSS `1em`, so flex `align-items: center` lands the ink on the row
// midline without optical corrections. Its paths run longer than
// CloseIcon and use a slightly heavier stroke to offset the +/× geometry
// ink-mass gap without overshooting the existing 1.7px icon strokes.
export function PlusIcon() {
  return (
    <svg
      width="1em"
      height="1em"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M12 5 L12 19" />
      <path d="M5 12 L19 12" />
    </svg>
  );
}
