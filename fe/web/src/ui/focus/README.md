# Focus

## Visual contract

This hook renders no visual surface.

## Accessibility contract

Exactly one item has tabIndex 0. Vertical navigation optionally loops; typeahead trims, lowercases and prefix-matches with a 500 ms reset. Modified and named keys do not enter typeahead, and horizontal arrows are deliberately untouched.

## Test contract

Pure matching tests lock cycling and wraparound. Source-contract assertions rewrite each key literal independently so trivial key-name mutations fail separately.
