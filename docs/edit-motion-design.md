# Edit and drawer motion

Edit currently binds a message and its composer field to a document View Transition. Different text geometry and the root crossfade make unrelated content participate.

Use Motion in a small `ui/motion` size primitive. Keep existing duration tokens and add styles-owned enter, exit and layout easing tokens. Edit state and focus update synchronously. The original message stays in place; composer height changes without scaling text. Cancellation and re-entry interrupt the current animation. Drawer and seam use the same easing tokens; reduced motion skips travel. Ordinary CSS interactions and decorative SVGs retain their mechanisms.

Review tier: L1, frontend presentation and a dependency addition with no authority or persistence changes.

Acceptance: production-component browser tests cover immediate content and focus, no document transition or text transforms, intermediate height, rapid reversal, and reduced motion. Existing composer/drawer tests and frontend/text gates pass. Preview production components interactively.

## Owner change request

The user authorized this implementation on 2026-10-06. The root orchestrator accepts these narrowly scoped changes: register `ui/motion` in `fe/module-file-inventory.yaml`; add MIT Motion to `fe/package.json` and its lockfile; extend styles tokens, public types and inventory tests with easing names. Frozen ownership controls and unrelated interfaces retain their contracts. Issue publication was subsequently authorized: https://github.com/keanji-x/neige-calm/issues/2191.
