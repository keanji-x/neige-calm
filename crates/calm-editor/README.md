# calm-editor — AI-first editor (Rust crate)

## Scope

This crate owns the Rust-side surface of the AI-first editor described in
issue #330: schema definitions for the Plate/Slate-compatible document AST,
schema validation, the MCP tool surface the agent calls to mutate documents,
and the `ts-rs` bindings emitted into `web/src/editor/types/`.

## Boundary

`calm-editor` (Rust) owns: schema, validation, MCP tools, ts-rs bindings.

`web/src/editor/` (TS/React) owns: Plate setup, op dispatcher, MCP tool
client surface, stable block-id middleware, and the `WaveReportEditor`
component. The wire contract between the two sides is the generated TS
types in `web/src/editor/types/`.

## Status

Scaffold only — see issue #330 Spike Day 1-3 for the build-out plan.
