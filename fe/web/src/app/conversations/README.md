# Conversation assembly

`store.ts` is the shared read/operation assembly used by the Today and Track
conversation panes, including main and side panes. `contracts.ts` declares its
Scope, RouteIntent and Store interfaces. The router chooses a valid scope and
layout, then calls this one assembly; it contains no copy or re-export of it.

| Input or capability | Owner |
| --- | --- |
| Transport and recovery/unauthorized handling | app/providers and core/api |
| Scoped query facts and invalidation | QueryClient via app/providers/queries |
| Read numbering and live/history handoff | conversations/read-order and live-replies |
| Cross-mount input, drafts, send records and confirmed memory | ConversationProvider |
| Outbox delivery and view projection | conversations/outbox and core/domain/conversation-outbox |
| Stop feedback and request lease | conversations/stop |
| Summary/name/state projection | core/domain/conversation-summary |
| Queue revision visibility | core/domain/conversation-outbox |

Initialization and cleanup retain the existing React hook order: tab registry,
transcript/read tracking, history and run observations, model catalog and
view-local state, live handoff, stop lease, then outbox reconciliation. One
mounted view observes one card. Changing scope disables/rekeys its queries and
resets view-local state under the existing hooks. Closing/unmounting does not
clear the provider's drafts, inputs or dispatched sends; late results remain
owned by the original card. Query collection, stop cleanup and live retirement
stay with their declared owners.

Production contracts are exercised through `app/router/track-conversation`,
`today-conversation`, `planner-conversation`, `live-replies`, the side-pane browser
paths and `chat-performance` browser probe. Direct store consumers in tests
import the assembly here; query/route integration remains covered by the router.
The extraction preserves function bodies and contract types, with AST parity
checked against the source snapshot before moving them.

This stage moves the shared store, not Pane's first-create/recovery workflow or
layout state. Those remaining responsibilities are tracked in #2083; loaded
DOM and streaming display work remain under #2235.
