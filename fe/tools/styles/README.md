# Style boundary tools

These checks make cascade boundaries observable: `entry.css` supplies the only layer order, PostCSS AST traversal rejects rules outside known layers, CodeMirror exceptions stay scoped by their rightmost compound selector, and extracted global classes must equal the manifest in both directions.

The `neige-calm/unlayered-cm-scope` stylelint rule runs under `npm run lint:css`. Its
`unlayeredExceptions` option is the complete, explicit repository-relative file allowlist; it is currently empty.

The runtime page is a harness, not a full-site scan. It deliberately injects one unlayered `<style>` rule so the test proves the audit fails when runtime CSS escapes the layer system. The audit function also checks readable `document.styleSheets` and `[style]` elements.

## Known escapes

- Cross-origin stylesheets can deny `cssRules`; this is reported but the harness cannot inspect their contents.
- Dynamic selector construction outside CSS and semantic selector/property exception ownership need separate architecture rules and the P8b manifests.
- The lightweight selector lexer supports ordinary CSS class identifiers but does not decode escaped identifiers.
- Runtime text/CSSOM de-duplication canonicalizes comments, whitespace, and trailing semicolons only; other formatting differences can duplicate a report, but cannot hide one.
- Nested named layers inherit their top-level parent (for example `@layer ui { @layer alien {} }` belongs to `ui`); accepting the parent name is intentional because the entry order governs top-level layers.
- Unlayered exception files waive only the requirement that rules belong to a layer; layer statements and layered imports must still name a layer from the entry order.

## Stage 2 connection

After a runnable application exists, call `auditRuntimeStyles` from Playwright on every routed page and relevant theme/state combination. Feed real `entry.css`, the named unlayered exception files, and the P8b global-class manifest into the static/build audit in CI.

Stage 2 must also bind `unlayeredExceptions` to the repository's actual unlayered-file set with a bidirectional test when the first exception is introduced.
