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

Run the smallest relevant checks while iterating, then the appropriate project
gates before requesting review:

```bash
# Rust: fast feedback, then the broad local gate when appropriate
scripts/local-rust-gates.sh --quick
scripts/local-rust-gates.sh

# Next-generation frontend
(cd fe && npm ci && npm run lint && npm run build && npm test)

# Browser tests when browser behavior changes
(cd fe && npx playwright install --with-deps chromium && npm run test:browser)

# Default stack end-to-end tier when an integrated flow changes
./e2e/run.sh
```

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
- Keep useful rationale, issue references, co-author attribution, and required
  trailers in the final squash commit message.
- Merge only when required checks are green, review feedback is resolved, and
  GitHub reports that the pull request is mergeable.
- Delete the source branch after the squash merge when it is no longer needed.

Individual commits on a pull request may be amended or reorganized during
review. The squash merge keeps `main` to one coherent commit per pull request.
