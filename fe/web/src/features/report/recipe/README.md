# Recipe editing

The page and `RecipeEditor` own the title, Markdown, revision and write outcome.
Opening the list or reading a saved recipe does not load CodeMirror. Entering Edit
loads the existing enhanced body editor through `load-body-editor.ts`; language,
theme and Markdown behavior stay in `body-editor.tsx`.

While that module loads, the current body remains visible and the reader can
choose **Use plain text**. The same option remains available if loading fails.
That deliberate choice focuses a controlled TextArea and lasts for the current
edit session: a late module answer never replaces the field, selection or input
composition. Module completion itself does not request focus. Cancel, leaving the
recipe, or a successful save ends the session. A later Edit may use the enhanced
editor again. No global loader cache or page reload is involved.

Both editors consume the same parent draft and unconfirmed-create read-only
fact. Module failure is distinct from recipe read/save failure; the existing
conflict, retry and revision behavior remains in its original owner. Tests use
the real page/editor with only the module load delayed or rejected, then resolve
the actual CodeMirror module; the route tests still exercise write contracts.
