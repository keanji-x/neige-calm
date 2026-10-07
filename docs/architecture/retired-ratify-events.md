# Retire the ratify event contract

Outcome: remove `ratify.requested`, `ratify.resolved`, and `RatifyDecision`
from the current Rust and frontend event contracts after the ask replacement.
Historical replay of these events is no longer required.

Review tier: L2, because this changes the interpretation of persisted events.

The SQLite event readers explicitly skip these retired kinds, like
`review.round`, before decoding payloads. Raw event rows and their ids remain
unchanged; the raw replay-window probe continues to advance the WebSocket
cursor across retired rows. No database migration or conversion to asks is
needed. Historical scorecard statistics continue reading the raw rows.

Remove obsolete role gates, dispatcher branches, goldens, schemas, and generated
wire types. Keep the ask authorization and wake behavior unchanged. Reject
retired events at both current contract decoders.

Acceptance checks:

- Rust and frontend decoders reject both retired event kinds.
- SQLite typed readers skip old requests and both grant/deny replies while
  preserving their original payloads and returning current asks.
- WebSocket replay advances across retired-only windows and streams current
  events after retired rows without missing ids.
- Real wire generation removes obsolete types; focused tests, contract gates,
  text gates, and frontend lint/build/tests pass.
- Two independent reviews check persistence and synchronization behavior,
  abstraction boundaries, duplicate logic, and hardcoded assumptions.

Frontend contract change: the user explicitly authorized deleting these retired
contracts. The core/api and core/events owners remove their obsolete definitions;
consumers use the remaining authoritative ask contract. Ownership trailers name
these two frozen directories. The sync envelope and REST version stay unchanged:
no new shape or endpoint is introduced, and old clients accept the ask events
already emitted by this kernel.
