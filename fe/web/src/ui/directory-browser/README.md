# Directory browser

## Visual contract

The browser fills its host surface so Dialog can present it as a child view or a field can use it inline; it never creates a portal or second modal because nested modal ownership breaks Escape and focus restoration.

## Accessibility contract

The editable absolute path is a combobox controlling a listbox; options retain focus on the input through `aria-activedescendant` so typing and navigation share one focus target. Directory mode skips files, while file mode may select them. Slash descends, Escape cancels, and clean-path Enter confirms the current directory. Loading is announced and a double animation frame restores input focus after async layout so a parent transition cannot steal it. This primitive declares no dialog role because its host owns modality.

## Test contract

The injected `ListDirectory` port keeps listing behavior independent of business APIs. Pure path tests lock canonical trailing-slash and join semantics; DOM tests cover loading, validation, filtering reset, keyboard navigation, cancellation, selection, and post-load focus.

## Deliberately not done

This slice freezes the props surface and behavioral contract, not visual styling. The rendered class names are placeholders; their CSS Modules and global-class ownership belong to §13 sequence 8 (the styles layer, global-class manifest, and unlayered-exception manifest) and the implementation phase. The current rendering is intentionally not visually finished, which is not a defect in this slice.
