/**
 * The resolved theme, read off the document.
 *
 * `app/theme` mirrors the resolved mode into `<html data-theme>` synchronously,
 * and that attribute is the one carrier a system may read: `systems/**` sits
 * below `app/**` and cannot import the context, and subscribing to it would
 * re-render the card subtree on every toggle — which is what remounts a live
 * editor or terminal (#177).
 *
 * Anything that is not the string `light` is dark, including a document that
 * has not been stamped yet: dark is this app's default, so an unstamped
 * document renders as what it is about to become rather than flashing white.
 */
export function readHostTheme(): 'light' | 'dark' {
  if (typeof document === 'undefined') return 'dark';
  return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}
