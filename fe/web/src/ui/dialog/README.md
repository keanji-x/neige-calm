# Dialog

## Visual contract

`Dialog` owns overlay, panel, header and body chrome; `wide` and a pushed child view select the wide variant. `ConfirmDialog` composes the same panel and exposes only a danger/primary action variant.

## Accessibility contract

The panel alone owns `role="dialog"`, `aria-modal` and its string-title name. Initial focus resolves explicit ref → first focusable → panel; Tab is trapped, background siblings are inert, and close restores the latest override or prior focus. Escape pops the child view before closing. The primitive deliberately avoids native `<dialog>`, cached focusable lists, visibility filtering, and overlay dismissal while a child view is active.

## Test contract

Consumers locate it by dialog role and accessible name. Contract tests lock literal ARIA names, child-view public shape, and the load-bearing inert-before-restore declaration order; real focus layout remains a browser-test responsibility.
