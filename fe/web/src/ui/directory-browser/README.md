# Directory browser

## Visual contract

The browser fills its host surface; it never creates a portal or a second modal.

## Accessibility contract

The editable path is a combobox controlling a listbox; options retain focus on the input through `aria-activedescendant`. Directory mode skips files, while file mode may select them. This primitive deliberately declares no dialog role.

## Test contract

The injected `ListDirectory` port makes listing behavior independent of business APIs. Pure path tests lock canonical trailing-slash and rough-join semantics; the public-only consumer verifies the port and component shape compile together.
