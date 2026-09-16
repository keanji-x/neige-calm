# Menu

## Visual contract

The caller owns every class and trigger rendering so menus can fit local chrome without expanding this primitive's API; the primitive only appends `is-active` to expose keyboard state consistently.

## Accessibility contract

The structure is menu → presentation list item → menuitem so list semantics do not obscure menu-item semantics. Vertical roving handles ArrowUp/Down, Home/End, activation, Escape and prefix typeahead. ArrowLeft/Right pass through because this is a vertical composite and horizontal keys may belong to its parent. Escape and activation restore trigger focus to preserve keyboard position; outside mousedown does not because pointer users already moved interaction context.

## Test contract

Consumers locate the menu and items by role and label. Rendered DOM and keyboard tests lock the role/ARIA structure, horizontal-key pass-through, initial-item focus, and synchronous restore-before-select ordering without depending on source spelling.

## Deliberately not done

This slice freezes the props surface and behavioral contract, not visual styling. The rendered class names, including the `is-active` state hook, are placeholders; their CSS Modules and global-class ownership belong to §13 sequence 8 (the styles layer, global-class manifest, and unlayered-exception manifest) and the implementation phase. The current rendering is intentionally not visually finished, which is not a defect in this slice.

## Mobile Area selection seam

The orchestrator approved optional `MenuItem.current`, `separatorBefore` and
`MenuProps.separatorClassName` for the mobile Area chooser. `current` emits
`aria-current="page"`; the caller supplies selected colours through that semantic
attribute. `separatorBefore` inserts a nonfocusable semantic separator before
an action group without changing item indexes or keyboard traversal. Existing
callers that omit these properties keep their prior structure and behavior.
The focused public test verifies current/noncurrent items and traversal across
the separator. This does not encode Area or Track data inside the primitive.
