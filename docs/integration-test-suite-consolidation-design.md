# Calm-server integration test suite consolidation

## Problem

`calm-server` currently exposes every `tests/*.rs` file as an independent
Cargo integration-test target. There are 153 such targets. Each target links
the same large server dependency graph, so a clean test build produces tens
of gigabytes of mostly duplicated executable code. Repeated feature or source
changes leave multiple fingerprinted copies under `target/debug/deps`.

The goal is to reduce the number of linked test executables without reducing
test coverage or making process-global tests flaky.

## Constraints

- Tests that mutate environment variables, the current directory, or other
  process-global state currently rely on separate test processes. Their local
  locks do not coordinate with locks in other integration targets.
- `tests/support` exports macros and must be loaded only once per consolidated
  test crate.
- Per-file `#![cfg(...)]` attributes must continue to apply only to that case.
- `CARGO_BIN_EXE_*` fixture paths must remain available.
- CI and documented `cargo test --test ...` commands must be updated to the
  new suite target and module filter.

## Design

Move ordinary test cases to `tests/cases/` and expose a small set of top-level
domain suite targets. Each suite loads shared support once and includes its
cases as named modules:

```rust
mod support;

#[path = "cases/actor.rs"]
mod actor;
```

Within a case, `mod support;` and `mod common;` become imports from the suite
root. Keeping each case as a module preserves duplicate helper/test names and
keeps its inner `cfg` attribute scoped to that case.

The first migration deliberately keeps process-sensitive targets independent.
This preserves their existing process isolation and makes the change safe to
land without first redesigning all environment guards. A follow-up may add a
single shared process-state lock and consolidate those targets too.

The implemented first migration reduces the package from 153 integration-test
targets to 35: 18 domain suites and 17 process-sensitive standalone targets.

Initial domain suites:

- API/domain behavior
- Codex runtime and Codex E2E
- MCP core and MCP integration
- plugin host/routes
- spec-card and spec-harness behavior
- worker-flow Codex, Claude, and driver behavior
- wave, terminal/WebSocket, migration, replay/event, kernel/process,
  runtime/dispatch, and forge behavior

## Profile policy

Development builds retain line-level debug information. Test binaries use
`debug = 0` because panic locations and Rust backtraces remain useful without
embedding DWARF in every integration executable. External dependencies also
compile without debug information in dev and test profiles. Incremental
compilation stays disabled for both profiles to prevent a second large cache.

## Command compatibility

A former target command such as:

```console
cargo test -p calm-server --test wave_fsm_golden
```

becomes:

```console
cargo test -p calm-server --test wave_suite wave_fsm_golden::
```

Cargo still provides `CARGO_BIN_EXE_*` to the consolidated integration crate.

## Validation

1. Verify every original integration target maps exactly once to either a
   suite module or an explicitly retained standalone target.
2. Compare the pre/post test inventories after normalizing suite prefixes.
3. Compile default and `codex-e2e` test targets with `--no-run`.
4. Run the complete `calm-server` test set and the migration replay CI command.
5. Run workspace formatting and Clippy checks.
6. From a clean target directory, record integration-target count and disk
   usage to quantify the reduction.

## Risks and rollback

The main risk is hidden process-global coupling. The conservative standalone
set limits that risk. If a suite exposes a previously hidden collision, that
case can be moved back to the top level without changing its test body or
coverage. The refactor changes test crate boundaries only; production code and
runtime behavior are untouched.
