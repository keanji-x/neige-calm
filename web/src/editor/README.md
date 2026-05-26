# web/src/editor — AI-first editor (TS/React)

## Scope

The web-side surface of the AI-first editor described in issue #330:
Plate setup, the op dispatcher that applies validated mutations, the MCP
tool client surface, the stable block-id middleware, and the
`WaveReportEditor` component.

## Boundary

`web/src/editor/` (TS/React) owns: Plate, dispatcher, MCP client glue,
block-id middleware, editor component.

`crates/calm-editor` (Rust) owns: schema, validation, MCP tool server
surface, and the `ts-rs`-emitted type bindings under `types/`. The wire
contract between the two sides is those generated TS types.

This is a *folder-level* split inside the existing `web` package, not a
separate npm workspace — promotion to a workspace is deferred until the
editor's surface area justifies the build/tooling overhead.

## Status

Scaffold only — see issue #330 Spike Day 1-3 for the build-out plan.
