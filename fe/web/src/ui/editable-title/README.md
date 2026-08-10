# `ui/editable-title`

INV-DUP-008 — the single click-or-F2-to-rename title, used by the cove header
and the wave header.

## Accessibility contract

Read mode is a `<button>` whose accessible name is the caller-supplied
`editLabel` ("Rename cove"), so a screen reader hears the action, not just the
current name. Edit mode is an `<input>` labelled by `inputLabel`. F2 enters
edit mode; Enter commits, Escape cancels, blur commits.

## The suppressor is load-bearing

Committing with Enter returns focus to the restored title button, and the
browser turns the trailing `keyup` into a `click` — which reopened the editor
and, on the next commit, wrote the *stale* name back (#288). Enter commits
therefore arm a short click-suppression window. Deleting it reintroduces the
bug silently: the UI looks right and the PATCH carries the wrong value.

## Deliberately not done

No auto-save on every keystroke, and no commit of an empty or unchanged value —
a rename that clears the field is treated as a cancel, not as a request for an
empty title.
