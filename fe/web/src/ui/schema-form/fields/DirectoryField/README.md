# DirectoryField

## Visual contract

The field shows the chosen path or placeholder plus the `Browse…` affordance.

## Accessibility contract

Inside Dialog it pushes one child view and changes the outer accessible name; outside Dialog it falls back inline. It deliberately does not nest a dialog. Escape/cancel pops without changing the value.

## Test contract

The public-only consumer supplies only `ListDirectory`, value and onChange. Integration tests own the browse → select → field chain and must import this and Dialog only through their public entries.
