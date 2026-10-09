# Shared code presentation (#2474)

## Outcome and ownership

File hover cards and Markdown fences in reports/chat share one read-only display.
Complete file viewers and diffs retain their existing interactions but consume the
same language resolver. No file reads, link policy, or report semantics move into
UI. GitHub summaries remain plain text until #2473 introduces Markdown rendering;
that host can inject `MarkdownCode` through Astryx's declared contract.

Review tier: **L2** because the cross-caller diff includes editor/diff lifecycle,
report/chat/hover rendering and new measurement tooling. It changes no authority,
isolation, persistence or wire contract; two independent channels review the tree. The existing #2474 is the
implementation scope; #2477 is not incorporated.

`ui/code` owns filename versus fence-name recognition, original-source copying,
line numbers, keyboard navigation, scroll bounds, theme observation, and disposal.
`systems/fs-viewers` keeps file/resource ownership, search adapters and merge
views. Features pass text and a typed source (`filename` or `language`) down to UI.

## Reproduction and implementation

A Chromium regression exercised real `CodePane` instances before implementation.
Rust, JS/TS, JSX/TSX, shell and Dockerfile had no target grammar (7 failures);
Python loaded correctly. The regression reads CodeMirror's actual installed
language, and JSX/TSX additionally assert a JSX syntax node. Tests now also cover
both real diff sides, report/chat fences, file hover placement/closing, original
copying, keyboard read-only behavior, light/dark changes, streaming and load races.

The handwritten extension switch and cast are removed. Upstream
`LanguageDescription.matchFilename` handles file patterns/extensions;
`matchLanguageName(..., false)` handles case-insensitive names/aliases. Filenames
are reduced to basenames, and extensions are normalized for case. `.zsh` alone
supplements upstream Shell metadata to preserve prior supported files. Actual
loads cover every previously supported extension and this exception. Unknown
labels stay plain text; extensions are not guessed as fence aliases.

The selected grammar loads asynchronously using the description's own cache.
Requests cancel on source change/unmount and cannot install an older grammar.
The lightweight view has no search, editing history, completion, fold or slash
shortcut. A compartment updates grammar/theme without destroying selection,
focus or scroll. Growing text preserves reading position. Native source copying
preserves CRLF and trailing newlines; failures are visible.

The editor chunk loads on demand. Its immediate source fallback supports copying.
An effect import avoids the observed ~300 ms Suspense reveal delay on first open.
Display/grammar failures retain source with explicit plain-text status.

## Bounds and library choice

Above 262,144 UTF-16 units, 5,000 lines, or a 16,384-unit line, grammar loading is
skipped and the display announces `Plain text · large file`. The complete original
source remains available, and CodeMirror virtualizes the visible viewport. This
bounds parser work; it is not an unlimited-input performance guarantee. File
owners retain the existing 2 MiB read cap and truncation metadata. Full editors
use the same bound; diffs apply it to the combined source of both sides.

Keep the minimal CodeMirror view for this change. It uses the same lazy grammars
as full viewers, provides viewport virtualization and keyboard selection, and
meets the measured budget without a second highlighting engine. Shiki offers
[TextMate-based highlighting and lazy grammars/themes](https://shiki.style/guide/),
but would require an additional rendering/metadata bridge and separate large-file
DOM bounds. Shiki was evaluated as an architectural alternative, not benchmarked;
these measurements do not claim that CodeMirror is faster than Shiki.

## Reproducible first-open measurements

Run `cd fe && npm run benchmark:code -- 4e96394e4`. The tool obtains the exact old
production CodePane from Git (Python, whose baseline grammar works), builds a
standalone production bundle and opens a fresh Chromium context for each of five
samples per variant/size. Timings run from the first dynamic display import to a
highlighted rendered pane. React and base CSS load before the timer. HTTP cache
is cold; OS/server caches are warm. There is no backend or Codex activity.

Measured on the shared host, without throttling. Reported medians are diagnostic,
not CI thresholds; repeat runs varied. Bytes include the standalone page's
resources and headers over uncompressed localhost HTTP, not gzip bundle sizes.

| Python source | Old CodePane median | Read-only median | Updated full CodePane median |
| --- | ---: | ---: | ---: |
| 200 lines | 234.2 ms | 108.7 ms | 119.8 ms |
| 4,000 lines | 237.5 ms | 124.4 ms | 129.0 ms |

Old CodePane: 749,319 transferred bytes / 115 resources. Read-only: 269,015 bytes /
16 resources. Updated full CodePane: 283,261 bytes / 16 resources. The shared
metadata registry loads only the selected parser rather than the old loader's
full static language roster. Raw per-sample results are written to ignored
`fe/test-results/code-benchmark/results.json`.

Production application build measurements are recorded in the pull request;
standalone first-open bytes are not the application's total build size.

## Critical assertion verification

In the exclusive implementation worktree, a single production mutation enabled
fuzzy fence-name matching. All 78 tests in the complete six-file focused target
ran (DOM, desktop Chromium and coarse-pointer Chromium; no test-name filter).
Predicted and actual red sets were exactly
`keeps filename extensions and strict language labels in separate namespaces`.
Restoring the production bytes made all 78 pass again. No mutation residue remains.

## Approved tooling change request

For the user-authorized #2474 implementation, the task orchestrator approves the
architecture-owner changes to `fe/package.json`, `fe/package-lock.json`, and
`fe/module-file-inventory.yaml`: declare the three CodeMirror dependencies,
register the new UI module and benchmark owner, and expose the reproducible
benchmark command. No frozen API, stylesheet token, or gate rule is loosened.
Every affected commit carries the exact ownership trailers, preserved in the PR.

The L2 review fixes additionally pin delayed diff grammar arrival without losing
either view, selection, focus or expanded context, and use actual EditorState
normalization as the oracle for CR/LF/CRLF line and individual-line budgets.

Async-highlight selection preservation uses CodeMirror's native read-only mode:
`EditorState.readOnly` rejects changes, while the native contenteditable surface
retains browser caret/selection. Disabling that surface reset focused selections
after highlighted text nodes changed, even without destroying the view. Deferred
grammar regressions cover full files, diffs and fences; typing/deletion/paste are
blocked and ARIA remains readonly. No delayed restoration or duplicate selection
synchronizer is added.
