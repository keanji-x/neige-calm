# Central theme typography (#2354)

The user-approved preview becomes an explicit production styles contract: 12/14/16/20px, UI sans, reading serif with Songti fallbacks, mono code, 400/500/600 and chapter-only 700, normal/label tracking. Keep the original palettes and accepted 240px sidebar, 32px rows, 40px header, 8px project breaks and 16px section breaks. No demo groups or preview content are shipped.

## Owner decision

The coordinating owner approves changing the frozen styles contract and the package verification entry points for #2354. The styles owner owns `theme.config.json`, its deterministic generated `tokens.css`, public token types, inventory and Astryx bridge. Font roles are composite custom properties; components explicitly select them. No runtime role detection or blanket `!important` rules are introduced. UI state and status colors remain component-owned semantic token consumers. Styles remains a non-runtime leaf.

## Verification

Pin config/generated equality and unchanged palette contracts. Test actual sidebar/report/table/cards under both themes and a larger user root font, plus disabled/destructive/status states. Preserve architecture and ownership checks. Use L2 with two independent fresh review channels after each fix, since the global style change spans components. Scope excludes persistence, backend/API changes, radius/icon/motion changes and experimental palettes.

Native report views and plugin chips also consume central tokens. Prominent numerical chart metrics retain their 36px presentation through a dedicated metric role. Terminal ANSI colors and daemon RGB defaults move to generated static values without changing wire values, checked against Rust.

Review fixes: reject nonzero unitless tracking and missing roles; retain reading serif/600 subsections. Mobile route titles share 14/20/600. The responsive new-track preference overflow measures untransformed offsetWidth, so popover opening animation cannot silently dismiss the menu when fonts grow. Sidebar header remains the approved40px while the page band remains56px; this is intentional, and the browser contract pins both.
