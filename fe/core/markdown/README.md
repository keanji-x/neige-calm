# Markdown core

`parse()` is the single platform-independent Markdown/GFM parse and normalization boundary. It uses `mdast-util-from-markdown` with `micromark-extension-gfm` and `mdast-util-gfm`; these dependencies replace handwritten Markdown recognition and preserve CommonMark/GFM structure and visible text.

The normalized vocabulary includes headings, paragraphs, blockquotes, ordered/unordered lists and list items, links, images, code and inline code, raw HTML, GFM tables, strikethrough, emphasis/strong text, breaks, and thematic breaks. Malformed input does not throw: parsing returns a `ready` AST plus diagnostics where the boundary recognizes an actionable condition; an internal parser/normalizer failure uses the `failed` value channel.

`extractOutline()` accepts an ordered block sequence. Global ordinals continue across blocks and local ordinals restart for every block. The frozen report policy exposes H1–H2 IDs as `<blockId>-h<n>` with legacy one-based `n`; the file-viewer policy exposes H1–H4 IDs as `md-h-<n>` with legacy zero-based global `n`. The file-viewer text policy omits headings whose normalized label is empty before assigning an ordinal, preserving its legacy TOC and anchor sequence.

Heading labels intentionally omit raw HTML and collapse whitespace (for example, `# Safe <i>label</i>` becomes `Safe label`), as a consequence of the core sanitization semantics; legacy labels retained the HTML source.

`sanitizeAstPolicy()` implements the raw-HTML `drop` policy recursively. Its output is a distinct narrowed `SafeMarkdownAst` whose vocabulary cannot contain `html` nodes. This remains an AST boundary, not a promise that rendered HTML is safe for DOM insertion; renderer element, URL, and link policies remain endpoint responsibilities.
