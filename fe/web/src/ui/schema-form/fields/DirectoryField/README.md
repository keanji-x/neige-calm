# DirectoryField

## Visual contract

The field shows the chosen path or placeholder plus one `Browse…` affordance so pointer and keyboard users enter the same selection flow.

The row is an `@astryxdesign/core` `Button` sized to the width of a form control, with a folder mark, the path in mono, and the affordance pushed to the far edge. A deep path truncates rather than widening the row — astryx's own label span carries the ellipsis — and the full value stays readable through the native `title`, which is passed through as a rest prop because the library's prop surface drops `title` in favour of a `tooltip` that would mount a floating layer on a control whose job is to open one. An empty value renders as a dimmed placeholder, mark included, so a field left alone reads as unset rather than as a value that failed to load.

## Accessibility contract

Inside Dialog it pushes one owned child view and changes the outer accessible name; the returned disposer removes exactly that view so concurrent owners cannot pop each other. Outside Dialog it falls back inline so the field remains reusable. It does not nest a dialog because one modal must own focus and Escape; cancel leaves the value unchanged because only explicit selection commits. The visible label and the affordance are the button's contents, never an `aria-label`: the accessible name belongs to the call site's `<label for>`, and an `aria-label` here would silently outrank it and rename the control to whatever path it holds.

## Test contract

The type-only consumer supplies only `ListDirectory`, value and onChange. Separate contract and integration tests assert the browse button surface — its name, its `aria-haspopup`, and the `title` that carries the untruncated path — plus both Dialog child-view and inline paths, keeping the browse → select → field chain on public entries.

## Deliberately not done

No inline editing of the path in the field itself: the browser owns path entry, and a second editable surface for the same value would need its own validation and its own error row.
