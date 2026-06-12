# E2E Framework

This directory holds opt-in end-to-end cases for the docker dev stack. The old
`scripts/e2e-multitask.sh` flow is now case `110-multitask-golden-path.sh`; run
it with `e2e/run.sh --tier 2 --case multitask`.

## Run

```bash
e2e/run.sh --list
e2e/run.sh
e2e/run.sh --tier 2 --case multitask
e2e/run.sh --all
```

The default run selects tier 1 only. Tier 2 cases burn real Codex tokens, so
they require an explicit `--tier 2` or `--all`. `--case <substring>` filters by
file name or case name.

## Case Anatomy

Each case is a bash file under `e2e/cases/`. The runner sources `lib/*.sh`, then
the case file. A case declares:

```bash
CASE_NAME="short human name"
CASE_TIER=1
CASE_TIMEOUT_SECS=300

case_run() {
  # use lib primitives here
}
```

The runner owns `RUN_ID` and `DEV_ID`, stack startup, the cleanup trap, artifact
dumping on failure, per-case `PASS`/`FAIL` lines, the final summary, and the
nonzero exit if any case fails. Cases should use library functions for API calls,
auth, docker exec file probes, polling, and stack access.

## Tiers And Order

Case file names start with a number. The prefix controls run order:

- `0xx`: tier 1, no Codex credentials or token spend.
- `1xx`: tier 2, requires Codex auth and may spend tokens.

## Stack Conventions

The runner keeps artifacts at repo-root `e2e-artifacts/` and tears stacks down
with `docker compose down -v --remove-orphans`. Stack startup preserves the
battle-tested dev-state neutralizer sweep: `CALM_CONTAINER_STATE_DIR=`,
`CALM_DB_URL=`, `CALM_DATA_DIR=`, `CALM_PLUGINS_DATA_DIR=`, `RESET_DB=`, and
`FRESH=`.

API helpers keep environment-first `.env` lookup, the autologin probe, and the
compose-resolved `SERVER_CID` for `docker exec`. The multitask case keeps the
done-only lifecycle gate, trailing-newline-safe JSON summaries, and fail-fast
server log signatures.

## Add A Case

1. Pick the next numeric prefix for the tier.
2. Add `e2e/cases/NNN-name.sh`.
3. Declare `CASE_NAME`, `CASE_TIER`, and `CASE_TIMEOUT_SECS`.
4. Implement `case_run()` using `api.sh`, `stack.sh`, and `assert.sh`.
5. Run `bash -n e2e/run.sh e2e/lib/*.sh e2e/cases/*.sh` and `shellcheck` if it
   is installed.
