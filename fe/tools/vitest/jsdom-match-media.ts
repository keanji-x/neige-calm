/*
 * `window.matchMedia` for the jsdom tier: jsdom does not implement it and reading it throws. Every
 * query answers "does not match" (light theme, no reduced motion) and the listener list is inert.
 * Installed only if absent, so the browser projects keep the real one.
 */

// `platform-independent` runs in node with no window; the same setup list feeds both projects.
if (typeof window !== 'undefined' && typeof window.matchMedia !== 'function') {
  window.matchMedia = (query: string): MediaQueryList => ({
    media: query,
    matches: false,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    /* Deprecated in the specification, still called by older libraries. */
    addListener: () => {},
    removeListener: () => {},
    /* No listener was kept, so nothing is cancelled and the spec's return value is `true`. */
    dispatchEvent: () => true,
  });
}

export {};
