# DirectoryField

## Visual contract

The field shows the chosen path or placeholder plus one `Browse…` affordance so pointer and keyboard users enter the same selection flow.

## Accessibility contract

Inside Dialog it pushes one owned child view and changes the outer accessible name; the returned disposer removes exactly that view so concurrent owners cannot pop each other. Outside Dialog it falls back inline so the field remains reusable. It does not nest a dialog because one modal must own focus and Escape; cancel leaves the value unchanged because only explicit selection commits.

## Test contract

The type-only consumer supplies only `ListDirectory`, value and onChange. Separate contract and integration tests assert the browse button surface plus both Dialog child-view and inline paths, keeping the browse → select → field chain on public entries.

## Deliberately not done

This slice freezes the props surface and behavioral contract, not visual styling. The rendered class names are placeholders; their CSS Modules and global-class ownership belong to §13 sequence 8 (the styles layer, global-class manifest, and unlayered-exception manifest) and the implementation phase. The current rendering is intentionally not visually finished, which is not a defect in this slice.
