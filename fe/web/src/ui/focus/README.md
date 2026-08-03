# Focus

## Visual contract

This hook renders no visual surface because composites retain ownership of their markup and styling.

## Accessibility contract

Exactly one item has tabIndex 0 so a composite contributes one tab stop. Vertical navigation optionally loops; typeahead trims, lowercases and prefix-matches with a 500 ms reset so repeated letters cycle predictably. Modified and named keys do not enter typeahead, and horizontal arrows are untouched because a vertical composite must not steal parent navigation.

## Test contract

Pure matching tests lock cycling and wraparound. A real rendered roving list gives every handled key its own behavior test, verifies horizontal keys leave both state and `defaultPrevented` unchanged, and verifies buffered Space differs from activation Space; this locks semantics while allowing refactors.
