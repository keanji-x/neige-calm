// Inline SVG right-chevron for disclosure affordances. Sized by CSS via
// `1em` and drawn with currentColor so the parent button controls color
// and dimensions, matching the rest of the sidebar icon components.
export function ChevronIcon() {
  return (
    <svg
      width="1em"
      height="1em"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M9 6 L15 12 L9 18" />
    </svg>
  );
}
