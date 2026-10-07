# Contributing to Neige Calm

Thank you for improving Neige Calm. The project moves quickly, but changes must
remain reviewable, reproducible, and safe to merge.

## Before you start

- Search existing issues and pull requests before opening a new one.
- Open an issue before a large change. Describe the user-visible outcome, the
  affected authority or persistence boundaries, and how the behavior will be
  verified.
- Keep one pull request focused on one outcome. Split unrelated refactors,
  formatting, generated-file churn, and dependency updates into separate pull
  requests.
- Do not rewrite existing migrations or persisted contracts without an explicit
  migration and compatibility plan.

Development prerequisites and common commands are documented in the
[README](README.md#development).

## Pull requests

### Title

Pull request titles must be written in English. Use this form:

```text
<type>(<optional-scope>): <imperative summary>
```

Use one of these types:

- `feat` — user-visible capability
- `fix` — defect or security correction
- `refactor` — behavior-preserving restructuring
- `perf` — performance improvement
- `test` — test-only change
- `docs` — documentation-only change
- `build` — build system or dependency change
- `ci` — continuous-integration change
- `chore` — repository maintenance
- `revert` — reversal of an earlier change

Keep the summary concise and specific. Do not prefix the title with an issue
number or a priority label.

Good examples:

```text
fix(mcp-http): refuse credential-bearing redirects
feat(tracks): add report revision history
docs: document the pull request workflow
```

### Description

Pull request descriptions should be written in English. Use the repository
template and include:

1. **Summary** — the outcome in one to three bullets.
2. **Why** — the problem, constraint, or user need.
3. **Changes** — the important implementation decisions.
4. **Verification** — exact commands and manual checks that were actually run.
5. **Risk and rollback** — likely failure modes and how to undo the change.
6. **Related issues** — use `Closes #123` when the merge should close an issue.

Preserve exact identifiers, error messages, and user-facing copy in their
original language when translating them would make the report less accurate.
Never claim a check was run when it was not.

### Verification

Run only the smallest relevant checks while iterating and before requesting
review. The commands, and when each one applies, are listed under
[Verification in AGENTS.md](AGENTS.md#verification); run
`scripts/local-ratchet-gates.sh` for every change, including docs-only ones.
Workspace-wide Rust tests run in CI and are not a routine local step. When an
integrated flow changes, run `./e2e/run.sh`. Without flags it selects tier 1
only, which needs no Codex credentials and spends no tokens, so it may run on
the shared host. Tier 2 (`--tier 2` or `--all`) is real Codex E2E and runs only
on a dedicated host.

When an API schema or generated binding changes, run the relevant generation
command and commit every generated artifact it updates. Tests for a defect or
security fix should fail without the fix and pass with it. Prefer a regression
at the boundary where the defect was observable.

Documentation-only changes do not require unrelated code suites, but links,
commands, and examples must still be checked.

### Ready for review

Before marking a pull request ready:

- Rebase or update it onto the current `main` when the base has moved in a way
  that affects the change.
- Remove debug code, temporary files, unrelated edits, and accidental secrets.
- Confirm the diff contains only the intended files.
- Resolve every review conversation or explain the remaining decision.
- Wait for all required checks to pass.

## Merge policy

Neige Calm uses **Squash and merge only**.

- Do not use merge commits or rebase merges for pull requests.
- The approved pull request title becomes the squash commit subject, so review
  the title before merging.
- Copy every `OWNERSHIP-CHANGE` trailer from the branch commits verbatim into
  the pull request description's **Ownership changes** section. The repository
  uses the pull request body as the default squash commit body, and the
  `ownership trailers preserved in squash body` check reruns when that body is
  edited. Keep each branch-commit and pull-request trailer on one physical line
  with the canonical single spaces shown by the template.
- Keep useful rationale, issue references, co-author attribution, and required
  trailers in the final squash commit message.
- Do not override the default squash body with a custom message that drops
  required trailers. If merge tooling supplies a custom body, it must preserve
  every canonical `OWNERSHIP-CHANGE` trailer. The final squash message audit
  accepts either the canonical single line or GitHub's deterministic wrapping:
  greedily append whole space-delimited tokens while the physical line stays at
  or below 72 Unicode code points, then start the next token on a new line. A
  token over 72 code points occupies its own line. Original commits and the pull
  request body accept only the canonical single-line form.
- Merge only when required checks are green, review feedback is resolved, and
  GitHub reports that the pull request is mergeable.
- Delete the source branch after the squash merge when it is no longer needed.

Individual commits on a pull request may be amended or reorganized during
review. The squash merge keeps `main` to one coherent commit per pull request.
