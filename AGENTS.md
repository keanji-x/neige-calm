# Neige Calm Development Guidelines

These rules apply across the repository. Read any more specific `AGENTS.md` in
the target directory before editing. See `CONTRIBUTING.md` for pull request and
merge rules. Code under `external/` follows its own guidance; do not modify it
unless the task requires it.

## Workflow

1. Inspect the real implementation, full call path, existing tests, and current
   worktree state. Preserve unrelated changes.
2. Define one clear outcome and its acceptance checks. Write an issue or short
   design first only for large, risky, authority-boundary, or persistence-boundary
   changes.
3. For a bug, add the smallest stable reproduction first and confirm it is red.
   If it does not reproduce, report that rather than inventing a cause.
4. Implement the smallest root-cause fix. Exercise production entry points; do
   not copy the behavior under test into fixtures or helper scripts.
5. Run focused tests, mutation-verify critical assertions, then run the gates
   appropriate to the changed surface.
6. For non-mechanical changes, review the complete diff through two independent
   channels. Classify findings with evidence, fix all blocking findings and
   in-scope actionable findings, then run both reviews fresh against the updated
   diff.
7. Re-run invalidated checks after every fix, rebase, conflict resolution, or
   generated-artifact change. Deliver only when review is converged, required
   gates are green, and the diff contains only intended files.

Keep implementation and review isolated. Allow only one writer per worktree;
use separate worktrees for parallel agents or independent review. Never clean a
worktree with destructive commands such as `git checkout` or `git reset --hard`.

## Review and fix loop

- Review behavior, security, data integrity, architecture boundaries, callers,
  and tests—not only style or the happy path.
- Treat review claims as hypotheses. Resolve disagreements with a decisive test
  or source-level check, not by weighing reviewer confidence.
- Fix the defect class, not one visible instance. After a fix, sweep sibling
  branches and callers for the same failure mode.
- Every fix requires a fresh review. A user-approved round limit applies only to
  recorded non-blocking findings; blockers always continue the loop. Escalate a
  diverging loop or an architectural conflict instead of stopping silently.
- Convergence means no unresolved blocking finding, no unexplained test failure,
  all required checks actually green, and no unrelated or generated-file drift.

## Contracts and code

- Model required fields as required types. Do not hide missing values with
  `Option`, defaults, or test-helper backfills. When changing an API, event,
  database field, or configuration, sweep every caller, fixture, script, and
  generated artifact.
- Treat every released database migration as byte-frozen, including comments and
  whitespace. Correct it with a new migration.
- At untrusted, credential-sensitive, or isolation boundaries, build child-process
  environments from explicit allowlists. Add configuration through typed
  configuration or CLI arguments, not implicit environment variables. Ordinary
  developer tooling may inherit the environment when that is part of its contract.
- Avoid duplicate implementations, broad fallbacks, and silent compatibility.
  Preserve architecture boundaries. Changes under the next-generation frontend
  `fe/` must follow `fe/AGENTS.md` and the guidance in each layer directory;
  do not apply those rules or commands to the legacy `web/` tree.
- Keep hand-written source files at or below 800 lines where practical. Split by
  responsibility and explain any necessary exception for core code.

## Mutation verification

Use this for the small set of tests that uniquely pin a load-bearing invariant,
such as security fences, replay equality, or no-silent-miss sweeps—not ordinary
unit tests:

1. Predict the complete set of tests that must fail by name, then apply one
   explicit, single-factor mutation to production code only. Do not mutate the
   test or a hand-written copy.
2. Confirm the mutation was actually applied and compare the complete actual red
   set with the prediction. A missing or unexplained additional failure invalidates
   the result.
3. Restore the mutation safely, confirm the test is green, and inspect the final
   diff for residue.

Run mutation work only in an exclusive, recoverable worktree with no concurrent
writer or reviewer that could observe transient code. Do not use destructive
checkout/reset for restoration, and do not interrupt a mutation run midway;
repository mutation tooling rewrites files in place.

## Verification

Run only the smallest relevant tests while iterating and before delivery. Do
not run workspace-wide `nextest` by default; the broad suite belongs to CI.

```bash
# Rust: select the affected package and test-name filter
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked \
  -p <package> <test-name-filter> --test-threads 8

# When the Rust change also needs the compile, lint, and OpenAPI preflight
scripts/local-rust-gates.sh --quick

# Next-generation frontend only
(cd fe && npm ci && npm run lint && npm run build && npm test)

# When browser behavior changes
(cd fe && npx playwright install --with-deps chromium && npm run test:browser)
```

- Add `--features calm-server/codex-e2e` to a targeted Rust command only when
  the affected test requires that feature. Narrow further with `--lib` or
  `--test <test-target>` when useful. Keep `NEIGE_CODEX_BIN` unset and cap local
  concurrency on the shared production host.
- Do not run the full `scripts/local-rust-gates.sh` as routine local
  verification. Run it only when explicitly requested or when changing the
  gate/nextest configuration itself. It uses `scripts/run-rust-nextest.sh` for
  the broad workspace suite; remote CI remains authoritative for that suite and
  runner-specific setup.
- Never re-enable real Codex E2E on the shared production host.
- Run the real generator after schema or generated-code changes and include every
  updated artifact.
- Preview visible UI changes in a real browser and run the relevant E2E for
  integrated paths. Never run real Codex E2E on the shared production host; use
  a dedicated host for Tier 2 stack E2E. `make e2e-codex-isolated` safely runs the
  separate `codex_forge_e2e` suite but does not replace Tier 2 stack coverage.
- Finally inspect `git diff`, `git status`, and actual test output. Report only
  commands and results that were truly run.
