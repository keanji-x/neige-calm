# `ui/editable-title`

INV-DUP-008 — the single click-or-F2-to-rename title, used by the cove header
and the wave header.

## Accessibility contract

Read mode is a `<button>` whose accessible name is the caller-supplied
`editLabel` ("Rename cove"), so a screen reader hears the action, not just the
current name. Edit mode is an `<input>` labelled by `inputLabel`. F2 enters
edit mode; Enter commits, Escape cancels, blur commits.

## Two carriers: `value` edits, `placeholder` displays

`value` is the stored name, verbatim, and it is what the editor works on: the
draft is seeded from it and the no-op check compares against it. `placeholder`
is what read mode shows *instead* while `value` is blank, and it stops there —
it never seeds the draft and there is no path that commits it.

They used to be one prop, and the wave page fed it `waveDisplayTitle(...)`. So
opening the editor on an unnamed wave put `Untitled wave` in the box, and the
reader deleted it before typing. It was never *stored* — resubmitting it hit the
`next === value` arm and wrote nothing — the defect was the text in the box.

## What an empty commit means is the caller's to say

`emptyCommit` is `'cancel'` by default: clearing the field and pressing Enter
leaves edit mode and writes nothing. That is right for the cove header, where
the owner is the only namer there will ever be.

The wave header passes `'clear'`, because a wave has a second namer: the spec
agent's `calm.wave.rename` succeeds only while the title is empty (#1211), so
clearing the name is how a reader hands naming back to it. Under `'clear'` the
only empty commit that still writes nothing is the one on an already-blank
title, which is the arithmetic no-op, not the policy one.

## The suppressor is load-bearing

Committing with Enter returns focus to the restored title button, and the
browser turns the trailing `keyup` into a `click` — which reopened the editor
and, on the next commit, wrote the *stale* name back (#288). Enter commits
therefore arm a short click-suppression window. Deleting it reintroduces the
bug silently: the UI looks right and the PATCH carries the wrong value.

## Deliberately not done

No auto-save on every keystroke, and no commit of an unchanged value. Whether an
*empty* value is a cancel or a request is `emptyCommit`'s to decide, above — not
a property of the primitive.
