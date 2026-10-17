# An offline lesson using the native workspace

Outcome: first-use training runs against a browser-memory scenario, while the
normal application keeps its real transport. A persistent Demo banner names the
simulation and provides reset/exit. Native Area, Track, conversation and report
components remain the UI; only the composition and data port change.

The bootstrap selects Demo before importing/mounting production services. Demo
uses hash history so navigation and refresh retain its explicit URL selector,
private in-memory preferences, an in-memory ApiTransportPort, no session/event
bridge and an injected raw-file URL factory for sandboxed data-URL HTML previews.
Unsupported actions fail locally. No fallback delegates to fetch. A document CSP
additionally prohibits connections, remote frames/images, objects and native form
navigation for the lifetime of the Demo document. Exit is a full page navigation.

The lesson has two named, prerecorded outcomes: an initial portfolio and a title
revision. Messages, reports and verification claims explicitly identify these as
demonstrations, not work performed by a model. Reset discards all Demo state.
Normal onboarding/help opens Demo; normal empty workspaces offer a Demo entry
alongside creating a real project. Existing projects keep their normal layout.

Callouts use the existing design tokens and Floating UI arrow. Move step context
to a quiet header, establish a readable title/body hierarchy, use one primary
action and subdued back/skip actions, and reduce arrow and example decoration.
Do not add another layout or introduce global theme tokens.

Review tier: L2 because this adds a composition/isolation boundary and a sizable
integration. Two independent review worktrees must inspect ownership layers,
duplicate logic and application assumptions. Verify native create/send/revise,
reset and refresh; assert zero backend HTTP/WebSocket requests, no production
storage writes, explicit refusal of unsupported routes, and normal bootstrap
selection. Exercise desktop/mobile screenshots and browser tests, frontend
lint/build and text gates. Mutation-verify the bootstrap isolation check in an
exclusive worktree. Preserve all current real preview data.
