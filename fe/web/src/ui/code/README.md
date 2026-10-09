# Code presentation (#2474)

`public.tsx` owns domain-free presentation. It reads no files and owns no navigation,
search adapter, or editing/history/completion lifecycle. Reads remain in
`systems/fs-viewers`; Markdown hosts inject already parsed source.

- `resolveCodeLanguage`: filenames use upstream patterns/extensions; fence labels
  use strict case-insensitive names/aliases. Unknown values return `null`.
- `useCodeLanguage`: cached upstream asynchronous loading, bounded by source size;
  cancelled requests cannot install a grammar for a previous source.
- `ReadOnlyCode`: required original text and typed filename/language source; optional
  theme. Without one, observes `<html data-theme>` and cleans up the observer.
  One editor view survives grammar, theme and text updates, and dies on unmount.
  New sources reset the reading position; growing sources preserve it. Keyboard
  arrows/selection work; Tab leaves the pane; no `/` or search handler is installed.
  Line numbers are excluded from accessible text.
- `MarkdownCode`: adapts Astryx's declared fence props; missing labels are plain text.

The editor and selected upstream grammar load on demand. The immediate fallback
supports copying before the editor loads. Unknown languages and load failures show
plain-text status. Copy writes the original string, including CRLF/trailing newlines,
rather than reconstructing rendered text. Clipboard failures are visible.

Highlighting stops above 262,144 UTF-16 code units, 5,000 lines, or a 16,384-unit
line. This component never truncates text. CodeMirror virtualizes visible lines;
this bounds parsing, not arbitrary input size. Read owners retain their existing
2 MiB cap and truncation metadata. The same bound applies to full file viewers
and the combined source of both diff sides.

The only metadata supplement is `.zsh -> Shell`, preserving prior file support
that upstream omits. It references a declared name and has an actual loader test.

See [design and measurements](../../../../../docs/design/code-presentation.md).

Readonly is enforced by `EditorState.readOnly`, not by disabling the browser's
contenteditable selection surface. Keeping that native surface is necessary for
stable keyboard caret/selection when async highlighting replaces DOM text nodes.
The display still installs no history, completion, search or editing consumer.
Typing, deletion and paste regressions assert that the document stays unchanged;
ARIA declares the textbox readonly. Delayed grammar tests cover all three callers.
