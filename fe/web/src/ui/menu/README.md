# Menu

## Visual contract

The caller owns every class and trigger rendering so menus can fit local chrome without expanding this primitive's API; the primitive only appends `is-active` to expose keyboard state consistently.

## Accessibility contract

The structure is menu → presentation list item → menuitem so list semantics do not obscure menu-item semantics. Vertical roving handles ArrowUp/Down, Home/End, activation, Escape and prefix typeahead. ArrowLeft/Right pass through because this is a vertical composite and horizontal keys may belong to its parent. Escape and activation restore trigger focus to preserve keyboard position; outside mousedown does not because pointer users already moved interaction context.

## Test contract

Consumers locate the menu and items by role and label. Rendered DOM and keyboard tests lock the role/ARIA structure, horizontal-key pass-through, initial-item focus, and synchronous restore-before-select ordering without depending on source spelling.

## Source lift and retention

The trigger lives in a stable nonpressed shell; caller styling still owns centered
placement on the unanimated positioner and visual styling on the inner menu list.
Shared motion reads the shell’s real visual DOM source and projects it using the
independent final layout frame. Menu has no descendant inverse endpoint, placement
strategy or visual-notification subscription. Open is immediate;
retained positioner and list are inert/aria-hidden immediately, including outer
hit testing, and activation is guarded, so selection,
focus restoration and onSelect never wait for settlement. Reopening reverses the
same native playback with its sampled velocity. No props or business policy are
added to the frozen caller API. The [primitive browser target](public.browser.test.tsx)
covers immediate selection,
focus, keyboard input and inert exit paint without application dependencies.
The real AreaSelector source, centered placement and press isolation are covered
by the [app composition target](../../app/shell/area-selector-motion.browser.test.tsx).

## Mobile Area selection seam

The orchestrator approved optional `MenuItem.current`, `separatorBefore` and
`MenuProps.separatorClassName` for the mobile Area chooser. `current` emits
`aria-current="page"`; the caller supplies selected colours through that semantic
attribute. `separatorBefore` inserts a nonfocusable semantic separator before
an action group without changing item indexes or keyboard traversal. Existing
callers that omit these properties keep their prior structure and behavior.
The focused public test verifies current/noncurrent items and traversal across
the separator. This does not encode Area or Track data inside the primitive.
