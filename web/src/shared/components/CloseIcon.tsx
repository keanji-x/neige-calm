// ---------------- CloseIcon ----------------
//
// Inline SVG `×` glyph for close/delete affordances. Replaces the
// Unicode U+00D7 `×` character which, in IBM Plex Sans, sits on the
// font's math-axis — ~30-40% above em-box center. Flex-centering a
// Unicode `×` inside a button visually places the ink high; the
// previous workaround was a `transform: translateY(2px)` hack that
// was font+size coupled (change either, the magic number's wrong).
//
// The SVG is geometrically centered inside a 24x24 viewBox so flex
// `align-items: center` lands the visible ink exactly on the row
// midline — no optical corrections needed.
//
// Sizes via CSS: by default the SVG inherits `1em` from its parent's
// `font-size`, so existing button styling that controls glyph size
// via `font-size` (e.g. IconButton's `fontSize: 18`) keeps working
// without changes.
export function CloseIcon() {
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
      <path d="M6 6 L18 18" />
      <path d="M18 6 L6 18" />
    </svg>
  );
}
