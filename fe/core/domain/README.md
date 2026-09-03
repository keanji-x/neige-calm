# `core/domain`

Platform-independent track and area models: wire schemas, the decoders that turn
them into required-field domain types, the shared predicates, and the
`ApiOperation` descriptors that name their endpoints. No transport, no React, no
browser globals — the transport is injected at the call site.

## Why the decoders carry defaults

`lifecycle`, `cwd`, `kind` and the `*_at` columns are absent from the OpenAPI
`required` set because the kernel emits them with `#[serde(default)]` for
event-log replay. The default belongs to the decoder: the decoded `Track` / `Area`
keep every field required so no reader has to re-derive it. Unknown server fields
are dropped, not rejected, so a kernel that adds a column does not break the UI.

## Shared predicates

`isWaitingForUser` / `isRunning` / `lifecycleLabel` / `trackDisplayTitle` /
`activeTracksOn` live here precisely so the sidebar, the Today counters, and the
agenda cannot drift into parallel tables.
