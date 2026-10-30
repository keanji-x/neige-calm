# Working on this example

This is a small, independent portfolio, not the Neige source repository. Keep it
self-contained in `index.html`: no package installation, remote assets or server
is needed. Inline illustrations work inside the Report HTML sandbox.

Follow the user's request. Preserve their wording and do not add onboarding UI
or instructions to the portfolio. The user may choose a different change from
the suggested one.

Run `python3 verify.py` after an edit. If the user requested a particular main
heading, also run `python3 verify.py --title 'the exact requested heading'`.
Inspect the real page at desktop and narrow widths when a browser is available;
report a structural check as structural, not as a visual or mobile check.

Use one independent read-only review for changes to this example. Let the Neige
worker lifecycle save the changes. Keep delivery local unless the user explicitly
requests publication; this example has no remote repository.

The Report owns the current result, not a tutorial. Include the actual page with
an HTML block whose payload has `path: "index.html"`, a short title and an
appropriate height. Keep the block after follow-up changes, and record the
verification actually performed. The page belongs to the Track workspace; do
not point Report HTML at a worker's private temporary path.
