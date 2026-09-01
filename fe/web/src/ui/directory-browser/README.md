# Directory browser

## Visual contract

The browser fills its host surface so Dialog can present it as a child view or a field can use it inline; it never creates a portal or second modal because nested modal ownership breaks Escape and focus restoration.

Its controls are `@astryxdesign/core`'s — the path field, the parent button, the two actions, and the error banner — while the option list stays local markup with a CSS Module, because a listbox driven by `aria-activedescendant` from an input that keeps DOM focus is the one shape the library has no component for. The path renders mono — it is read by its separators, and a proportional font puts every `/` at a different offset down a list of them — and the module wins over astryx's body font because the `ui` layer sorts after `astryx` in `entry.css`. The list is a bounded scrolling window rather than a growing column, so the actions cannot be pushed off the panel by a large directory. A listing that is still loading keeps the entries it already has under the pointer and says so beside them; an empty directory and a filtered-out one are two different rows, because the reader's next move differs.

## Accessibility contract

The editable absolute path is a combobox controlling a listbox; options retain focus on the input through `aria-activedescendant` so typing and navigation share one focus target. Directory mode skips files, while file mode may select them — they stay listed and `aria-disabled`, not hidden, so both readings of the directory hold the same entries. Slash descends, Escape cancels, and clean-path Enter confirms the current directory. The parent button issues the same load that typing the parent path and pressing Enter issues; it is disabled exactly when the listing has no parent. Loading is announced and a double animation frame restores input focus after async layout so a parent transition cannot steal it. This primitive declares no dialog role because its host owns modality.

## Test contract

The injected `ListDirectory` port keeps listing behavior independent of business APIs. Pure path tests lock canonical trailing-slash and join semantics; DOM tests cover loading, validation, filtering reset, keyboard navigation, parent navigation and its unavailable case, the two empty rows, cancellation, selection, and post-load focus. The loading row is located by its text and its role asserted there, because every astryx `Button` ships an always-present `role="status"` live region of its own and the bare role query is therefore ambiguous on this surface.

## Deliberately not done

No fuzzy or scored matching (#433): the filter is the prefix the typed basename spells. No breadcrumb trail — the editable path is the trail, and a second control naming the same value would need its own truncation rule for deep paths.
