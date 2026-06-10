// Inline SVG `+` glyph for create affordances. Same shape contract as
// CloseIcon: geometrically centered inside a 24x24 viewBox and sized via
// CSS `1em`, so flex `align-items: center` lands the ink on the row
// midline without optical corrections. Its paths share CloseIcon's
// 6-18 bounds; stroke is 1.7 vs 1.5 to compensate for the orthogonal
// vs diagonal mass differential without overshooting the bounding box.
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
      <path d="M12 6 L12 18" />
      <path d="M6 12 L18 12" />
    </svg>
  );
}
