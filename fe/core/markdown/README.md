# Markdown core

`parse()` is the single platform-independent Markdown/GFM parse and normalization boundary. It uses `mdast-util-from-markdown` with `micromark-extension-gfm` and `mdast-util-gfm`; these dependencies replace handwritten Markdown recognition and preserve CommonMark/GFM structure and visible text.

The normalized vocabulary includes headings, paragraphs, blockquotes, ordered/unordered lists and list items, links, images, code and inline code, raw HTML, GFM tables, strikethrough, emphasis/strong text, breaks, and thematic breaks. The root AST contract also exposes readonly normalized `sourceLines`, which line-level policies retain across AST transformations. Malformed input does not throw: parsing returns a `ready` AST plus diagnostics where the boundary recognizes an actionable condition; an internal parser/normalizer failure uses the `failed` value channel.

`extractOutline()` accepts an ordered block sequence. Global ordinals continue across blocks and local ordinals restart for every block. The frozen report policy exposes H1–H2 IDs as `<blockId>-h<n>` with legacy one-based `n` and recursively visits container blocks; the file-viewer policy exposes H1–H4 IDs as `md-h-<n>` with legacy zero-based global `n`, enters lists/list items, and skips blockquotes. This preserves the legacy line recognizer's 0–3-space list headings without admitting `>`-prefixed headings. Its text policy also omits headings whose normalized label is empty before assigning an ordinal.

Reference spelling is an independent outline dimension: `referenceText: 'source'` preserves file-viewer reference syntax, while `'visible'` produces report labels. It is not inferred from the empty-label text policy.

Heading labels intentionally omit raw HTML and collapse whitespace (for example, `# Safe <i>label</i>` becomes `Safe label`), as a consequence of the core sanitization semantics; legacy labels retained the HTML source.

GFM strikethrough contributes its visible child text to a label (`~~gone~~ kept` becomes `gone kept`), which differs from the legacy non-GFM recognizer.

## Known limitations

A line inside a four-space-indented code block that begins, after at least 130 spaces, with a list marker can be rejected as `limit-exceeded`. Pre-parse nesting protection cannot reliably classify full block structure without invoking the parser it protects. A scan of 834 real repository `.md` files produced zero instances, so this conservative false positive is documented rather than guessed around.

`sanitizeAstPolicy()` implements the raw-HTML `drop` policy recursively. Its output is a distinct narrowed `SafeMarkdownAst` whose vocabulary cannot contain `html` nodes. This remains an AST boundary, not a promise that rendered HTML is safe for DOM insertion; renderer element, URL, and link policies remain endpoint responsibilities.
