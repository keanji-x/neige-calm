# DirectoryField

## Visual contract

The field is one `@astryxdesign/core` `Button` in two states, and pointer and keyboard enter the same selection flow through it. Unset, it is the folder mark alone: the control has only a path to say, and a full-width box saying it has none is a placeholder taking a form row to say nothing. Set, the mark is joined by the path's last segment in mono, and the control is as wide as that. A long name truncates rather than stretching the row it sits in — astryx's own label span carries the ellipsis.

There is no `Browse…` affordance. It labelled what the control already is: a button marked with a folder, carrying `aria-haspopup="dialog"`.

## Accessibility contract

Inside Dialog it pushes one owned child view and changes the outer accessible name; the returned disposer removes exactly that view so concurrent owners cannot pop each other. Outside Dialog it falls back inline so the field remains reusable. It does not nest a dialog because one modal must own focus and Escape; cancel leaves the value unchanged because only explicit selection commits.

The control names itself: the accessible name is the placeholder, plus the **whole** path once there is one ("Choose a folder: /srv/app"), set through `aria-label` and repeated in the native `title` for the pointer. Neither half can come from the contents — unset there are none, and set they are a basename, which answers neither "which folder" nor "what is this control for". A call site therefore must not wrap this in a labelled field: a second name for one control is the defect, not the fix. `title` is passed through as a rest prop because astryx's prop surface drops it in favour of a `tooltip` that would mount a floating layer on a control whose job is to open one.

## Test contract

The type-only consumer supplies only `ListDirectory`, value and onChange. Separate contract and integration tests assert the browse button surface — the basename as its name and its only text, its `aria-haspopup`, the `title`, and the description carrying the full path — that an outer `<label htmlFor>` still wins the name, that the purpose phrase follows `mode` and survives a blank placeholder, plus both Dialog child-view and inline paths, keeping the browse → select → field chain on public entries.

## Deliberately not done

No inline editing of the path in the field itself: the browser owns path entry, and a second editable surface for the same value would need its own validation and its own error row.
