# Output projections

Issue: #3182. Review tier: L2, because these read-only projections expose
untrusted output across existing track isolation boundaries.

The outcome is directly consumable output with bounded previews and readable references.
The implementation retains existing events, authorization and operation storage;
it adds no migrations, arbitrary filesystem reads or generic ToolResult changes.

- Truth owns `runs/<attempt>/result`, selected from its authoritative worker
  completion projection, never the Planner verdict. Receipt enrichment uses the
  typed event projection rather than serializing and reparsing an entire run.
  Completed attempts are immutable; result references need no revision selector.
- The existing authorized gate-log reader owns log loading. Only exact step
  selection is server-side; shell `tail` and `grep` narrow the returned text.
  Step markers convey display boundaries only, never gate authority. Failure
  wakes include the failed step reference and a bounded untrusted tail preview.
- Server owns `ops/<op>/result`. Its existing track-scoped forge lookup runs
  before decoding or reading recorded artifact provenance. Common receipts
  inline output within a 16 KiB escaped display budget; overflow uses this same
  virtual reader. The `ops/` path is only an overflow reader. Missing
  artifacts are explicit, and no unreadable reference is advertised.
- Shared JSON quoting escapes markup, bidi and line separators. Preview budgets
  remain separate from full reads; a full read never silently drops UTF-8 bytes
  or an oversized line.

Local verification covers production MCP reads, receipt behavior and isolation.
Load-bearing isolation/escaping assertions receive a single-factor production
mutation with the complete expected red set declared before the run. The kernel
owns text, contract, quick and focused replay gates; CI owns broad suites.
Existing test targets remain extensible without a permanent test-name registry.

Base: `7460f354fc32cc108bcc5614b13cc05c67522e3d`. Preserve the generic result wire
contract. Formal independent L2 reviews are arranged by the Planner.
