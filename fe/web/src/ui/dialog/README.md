# Dialog

## Visual contract

`Dialog` owns overlay, panel, header and body chrome so every modal uses one stacking and dismissal policy; `wide` and a pushed child view select the wide variant. `ConfirmDialog` composes the same panel and exposes only a danger/primary action variant so confirmation call sites cannot fork the modal mechanics.

## Accessibility contract

The panel alone owns `role="dialog"`, `aria-modal` and its string-title name so assistive technology sees one modal surface. Initial focus resolves explicit ref → first focusable → panel; Tab is trapped, background siblings are inert, and close restores the latest override or prior focus. Child views form a LIFO stack; `pushView` returns an idempotent ownership disposer so one field cannot pop another field's view. Escape pops the top child before closing. The primitive avoids native `<dialog>` because browser top-layer/backdrop behavior would bypass the app-owned portal and inert policy; it avoids cached focusables because controls can appear after async rendering, and disables overlay dismissal for a child view because a nested workflow must cancel explicitly.

## Test contract

Consumers locate it by dialog role and accessible name. DOM contract tests assert the rendered ARIA surface, retained child content, dynamic focus trapping, exact inert restoration, detached-trigger handling, nested Escape ownership, stack disposal, and the load-bearing inert-before-restore cleanup order so implementation-preserving refactors remain free.
