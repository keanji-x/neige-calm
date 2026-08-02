# Menu

## Visual contract

The caller owns every class and trigger rendering; the primitive only appends `is-active`.

## Accessibility contract

The structure is menu → presentation list item → menuitem. Vertical roving handles ArrowUp/Down, Home/End, activation, Escape and prefix typeahead. ArrowLeft/Right intentionally pass through. Escape and activation restore trigger focus; outside mousedown intentionally does not.

## Test contract

Consumers locate the menu and items by role and label. Tests independently lock role/ARIA/key literals, horizontal-key non-handling, and synchronous restore-before-select ordering.
