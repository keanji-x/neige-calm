/*
 * `window.matchMedia` for the jsdom tier — #1245.
 *
 * jsdom does not implement `matchMedia` and never has: it is a layout question,
 * and jsdom computes no layout. Reading it throws `TypeError: not a function`
 * rather than returning nothing, so a component that merely *asks* about the
 * viewport takes the whole render down.
 *
 * Nothing in this app asked until the transcript started rendering replies
 * through Astryx's `Markdown`, which calls `useTheme` → `useMediaQuery` →
 * `matchMedia` on mount (`@astryxdesign/core/dist/hooks/useMediaQuery.js`) to
 * find out whether the page is dark. That is one line inside a vendor
 * component we do not control, and it is not optional: the hook runs before any
 * prop of ours is read.
 *
 * ── What this returns, and what that decides ──────────────────────────────
 *
 * **Every query does not match.** That is a *choice*, not an absence of one,
 * and it is the same answer a browser gives for the queries that reach here
 * today: `prefers-color-scheme: dark` is false on a default light page, and so
 * is `prefers-reduced-motion: reduce`. So the jsdom tier renders the light
 * theme, and a test that wants the dark one must say so through the app's own
 * `data-theme` protocol rather than expecting this to answer for it.
 *
 * **It is not a stub of the behaviour under test.** It answers a question about
 * the *environment* — is this page dark, does this reader want less motion —
 * and no assertion in the jsdom tier is about the answer. The tier that can
 * decide those questions honestly is the browser one, which has a real
 * `matchMedia` and needs none of this; `web/src/app/theme/theme.browser.test.tsx`
 * is where the theme's own behaviour is held down.
 *
 * **The list is inert, deliberately.** `addEventListener` accepts and drops.
 * Nothing here can ever fire a change, because nothing here can change: there
 * is no viewport to resize and no system setting to flip. A shim that kept
 * listeners it would never call would be a queue that only leaks.
 *
 * **Only if absent.** The browser projects load the same setup files and have
 * the real thing; overwriting it there would replace a working implementation
 * with this one, which is the exact failure mode a shim is supposed to avoid.
 *
 * `matchMedia` is available in insecure contexts, so this does not paper over
 * anything production hits: the app is served over plain http on the LAN, and
 * that is a real constraint for `crypto.randomUUID` and friends, but not for
 * this one.
 */

// `platform-independent` runs in node, where there is no window and nothing to
// shim; the same setup list feeds both projects.
if (typeof window !== 'undefined' && typeof window.matchMedia !== 'function') {
  window.matchMedia = (query: string): MediaQueryList => ({
    media: query,
    matches: false,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    /* Deprecated in the spec, still called by older libraries. */
    addListener: () => {},
    removeListener: () => {},
    /* A list nothing dispatches to: no listener was kept, so nothing is
       cancelled and the spec's return value for that is `true`. */
    dispatchEvent: () => true,
  });
}

export {};
