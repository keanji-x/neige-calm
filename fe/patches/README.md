# Dependency patches

`npm ci` applies these patches with `patch-package --error-on-fail`. Install
failures must be resolved before building; do not skip lifecycle scripts.

## Astryx 0.6.3: DropdownMenu opening focus

An opening animation frame can arrive after keyboard navigation or pointer
hover has already focused a menu item. The patch preserves that focus, cancels
the pending frame on close/unmount, and captures the opening modality so a
cancelled pointer open cannot affect a later programmatic open.

The repair belongs to Astryx's shared popover menu, covering both compound and
data-driven menus without a Sidebar-specific wrapper. The descendant check
also covers inline submenu flyouts. Bottom-sheet focus is unchanged.

[Astryx #10590](https://github.com/meta/astryx/pull/10590) fixes the pointer
descendant case upstream. Version 0.7.13 still does not cancel this frame or
protect keyboard-open navigation, and includes unrelated interaction changes.
This patch keeps the existing release while covering all three cases.

The patch includes the package's TypeScript source and its generated runtime
JavaScript. The JavaScript was regenerated with the Babel configuration and
extension plugin from Astryx's `v0.6.3` tag; the same generator reproduced the
unmodified published file byte for byte before the edit. No styles or API
declarations change.

When upgrading Astryx, re-evaluate or remove this patch and keep the observable
focus tests in `web/src/ui/menu/astryx-focus.browser.test.tsx` and the Sidebar
integration test. Adding ordinary menus or tests requires no patch-list edits.
Patches apply once per dependency installation, including each CI job that
runs `npm ci`; no additional CI job or runtime listener is added.
