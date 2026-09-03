# Ownership validator

This validator prevents implementation work from falling between slices. Entries are explicit files or directory prefixes, overlapping entries are rejected even when they describe future files, and every current file below `fe/core` and `fe/web/src` must match exactly once.

`OWNERSHIP_YAML_FIELDS` has no external schema document: the validator is the authoritative source for the manifest field set, and fixtures are its executable coverage evidence.

Readonly entries freeze interfaces and `styles/`. Every commit that changes a frozen path must carry an exact-path trailer in its full message: `OWNERSHIP-CHANGE: <path> — <reason> (#issue)`. A trailer approves only that commit and path; approvals remain auditable in the pull-request history and require no post-merge cleanup.

`npm run lint:js` drives `check-readonly-change-requests.mjs`. CI injects the pull-request base or push predecessor through `OWNERSHIP_BASE_SHA` and the event through `OWNERSHIP_EVENT_NAME`. Trailer range validation runs for pull requests, pushes, and by default when local runs have no event information. The independent `ownership trailers preserved in squash body` check runs for pull requests and every push to `main`, without consulting the code-versus-documentation classifier. For pull requests it reads the current head, base, and body from the GitHub API; every exact valid trailer found in the branch commits must appear in that body. The repository uses the PR body as its default squash commit body, and edits retrigger the check. Each push has a non-cancellable audit range, providing the final backstop for a custom merge body that diverges from the reviewed pull request description.

For local runs without event information, a zero or unavailable injected SHA falls back to `HEAD~1`. Pull-request and push ranges instead fail closed when their base, head, or required history is unavailable; pushes also reject non-linear and forced updates. Only local runs without an injected base fall back to `origin/main`, with an actionable fail-closed diagnostic. Git IO stays in the checkers and isolated-repository checks; decision logic must live in a module that Vitest can drive, with the `.mjs` checkers limited to wiring and IO.

Repository administration must keep the `ownership trailers preserved in squash body` status check required on `main`; without branch protection, the workflow is advisory rather than a merge gate.

## Known escapes

- Ownership describes write authority, not whether an owner implemented the right behavior.
- Renames appear as changed paths and therefore require a request when either affected path is readonly; rename intent is not inferred.
- A range containing a merge commit fails closed. Rebase the branch before review so each changed path has one auditable parent and commit message.
- A custom squash body can still differ after the pull-request check runs. Merge policy forbids dropping trailers, and the push audit reports any final squash commit that does so.
- Trailer whitespace currently uses `\s+`, so a newline after `OWNERSHIP-CHANGE:` can be accepted when the exact path, em dash, and reason follow on the next line.
- The tool validates manifest mechanics only. P8b owns the actual future-file manifest and approval trailers.

## Stage 2 connection

The inventory is a closed reviewed set, not permission to create arbitrary files under a layer: add a new module to the inventory first, then regenerate the ownership view and pass review. Coverage includes `fe/core`, all of `fe/web`, and `fe/tools`, so newly added files fail closed.

`fe/tools` is deliberately a whole-directory owner: new files below that tooling boundary inherit its owner and are not subject to per-file fail-closed registration. In contrast, top-level files under `fe/web` remain exact-file entries and therefore fail closed.
