# Output projections

Issue: #3182. Review tier: L2, because these read-only projections expose
untrusted output across existing track isolation boundaries.

The outcome is directly consumable, bounded output with readable continuations.
The implementation retains existing events, authorization and operation storage;
it adds no migrations, arbitrary filesystem reads or generic ToolResult changes.

- Truth owns `runs/<attempt>/result`, selected from its authoritative worker
  completion projection, never the Planner verdict. Receipt enrichment uses the
  typed event projection rather than serializing and reparsing an entire run.
  References carry the completion event revision and reject stale reads.
- The existing authorized gate-log reader owns log loading. A shared text
  projection applies exact step selection, literal grep, then range or tail.
  Report selectors and output selectors cannot mix. Original line numbers,
  revision and continuation addresses survive omissions. Markers convey display
  boundaries only, never gate authority.
- Server owns `ops/<op>/result`. Its existing track-scoped forge lookup runs
  before decoding or reading recorded artifact provenance. Common receipts
  inline small output; oversized output uses this same virtual reader. Missing
  artifacts are explicit, and no unreadable reference is advertised.
- Shared JSON quoting escapes markup, bidi and line separators. Preview budgets
  remain separate from full reads; a full read never silently drops UTF-8 bytes
  or an oversized line. Selector reads have an explicit budget and continuation.

Local verification covers production MCP reads, receipt behavior and isolation.
Load-bearing isolation/revision assertions receive a single-factor production
mutation with the complete expected red set declared before the run. The kernel
owns text, contract, quick and focused replay gates; CI owns broad suites.
Existing test targets remain extensible without a permanent test-name registry.

Base: `7460f354fc32cc108bcc5614b13cc05c67522e3d`. The s2-context revision is
unknown; inspect adjacent upstream changes and preserve the generic result wire
contract. Formal independent L2 reviews are arranged by the Planner.
