// Shared mono font-family stack for any JS consumer that needs a font-family
// string (xterm.js can't read CSS custom properties, fallback cards inline
// styles, etc.). The CSS counterpart is `--font-mono` in `calm.css:12`. The
// two must stay byte-equal — `calm-tokens.test.ts` enforces this with a
// drift assertion (#150 slice 3). Change both together or CI fails.
//
// Why not just read `getComputedStyle(document.documentElement)` at runtime?
// xterm.js needs the font string before its container is in the DOM, and
// SSR / test environments don't have a live cascade. A pinned constant is
// the cheapest source of truth, with the drift test catching divergence at
// build time instead of "looks slightly different in the terminal panel".

/**
 * Mono font stack used by xterm.js and any other JS consumer that needs a
 * font-family string. Must match `--font-mono` in calm.css byte-for-byte.
 * A drift assertion in `calm-tokens.test.ts` enforces this — change both
 * together.
 */
export const MONO_STACK = '"SF Mono", ui-monospace, "Menlo", monospace';
