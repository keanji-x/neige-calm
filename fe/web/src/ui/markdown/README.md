# Markdown presentation

`MarkdownBlocks` renders an already sanitized Markdown AST. The caller owns parsing,
raw HTML policy, destination admission, image loading and heading identity. Callback
outputs are rendered as React content; the component does not infer application
routes or workspace access. Existing callers keep their own heading and image
presentation while sharing block, inline, code and formula rendering.

Math uses KaTeX with untrusted commands disabled and expansion and size bounds.
Invalid expressions remain visible as source. The resulting HTML is produced by
KaTeX rather than interpolating Markdown source into the DOM directly. Display
math scrolls horizontally when it is wider than the available reading column.

Callers provide heading IDs when their surface needs navigation anchors. The
report and conversation each retain their own existing heading identity policy.
