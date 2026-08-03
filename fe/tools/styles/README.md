# Style boundary tools

These checks make cascade boundaries observable: `entry.css` supplies the only layer order, PostCSS AST traversal rejects rules outside known layers, CodeMirror exceptions stay scoped by their rightmost compound selector, and extracted global classes must equal the manifest in both directions.

The runtime page is a harness, not a full-site scan. It deliberately injects one unlayered `<style>` rule so the test proves the audit fails when runtime CSS escapes the layer system. The audit function also checks readable `document.styleSheets` and `[style]` elements.

## Known escapes

- Cross-origin stylesheets can deny `cssRules`; this is reported but the harness cannot inspect their contents.
- Dynamic selector construction outside CSS and semantic selector/property exception ownership need separate architecture rules and the P8b manifests.
- The lightweight selector lexer supports ordinary CSS class identifiers but does not decode escaped identifiers.

## Stage 2 connection

After a runnable application exists, call `auditRuntimeStyles` from Playwright on every routed page and relevant theme/state combination. Feed real `entry.css`, the named unlayered exception files, and the P8b global-class manifest into the static/build audit in CI.
