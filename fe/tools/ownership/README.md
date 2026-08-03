# Ownership validator

This validator prevents implementation work from falling between slices. Entries are explicit files or directory prefixes, overlapping entries are rejected even when they describe future files, and every current file below `fe/core` and `fe/web/src` must match exactly once.

`OWNERSHIP_YAML_FIELDS` has no external schema document: the validator is the authoritative source for the manifest field set, and fixtures are its executable coverage evidence.

Readonly entries freeze interfaces and `styles/`. Every commit that changes a frozen path must carry an exact-path trailer in its full message: `OWNERSHIP-CHANGE: <path> — <reason> (#issue)`. A trailer approves only that commit and path; approvals remain auditable in the pull-request history and require no post-merge cleanup.

`npm run lint:js` drives `check-readonly-change-requests.mjs`. CI injects the pull-request base or push predecessor through `OWNERSHIP_BASE_SHA` and the event through `OWNERSHIP_EVENT_NAME`. Trailer range validation runs for pull requests, and also by default when local runs have no event information. On a push to `main`, coverage, overlap, readonly-entry, and style checks still run, but trailer range validation does not: this repository squash-merges pull requests, and the squash commit message cannot reliably preserve the exact-path trailers already reviewed in the PR history. A zero or unavailable injected SHA falls back to `HEAD~1`. Only local runs without an injected base fall back to `origin/main`, with an actionable fail-closed diagnostic. Git IO stays in the checker and isolated-repository checks; decision logic must live in a module that Vitest can drive, with the `.mjs` checker limited to wiring and IO.

## Known escapes

- Ownership describes write authority, not whether an owner implemented the right behavior.
- Renames appear as changed paths and therefore require a request when either affected path is readonly; rename intent is not inferred.
- Merge commits are excluded from trailer range inspection. The repository currently uses a rebase workflow before its final squash, so conflict-resolution merges are not expected; changing that workflow requires revisiting this limitation.
- Trailer whitespace currently uses `\s+`, so a newline after `OWNERSHIP-CHANGE:` can be accepted when the exact path, em dash, and reason follow on the next line.
- The tool validates manifest mechanics only. P8b owns the actual future-file manifest and approval trailers.

## Stage 2 connection

The inventory is a closed reviewed set, not permission to create arbitrary files under a layer: add a new module to the inventory first, then regenerate the ownership view and pass review. Coverage includes `fe/core`, `fe/mock`, all of `fe/web`, and `fe/tools`, so newly added files fail closed.

`fe/mock` and `fe/tools` are deliberately whole-directory owners: new files below those two tooling boundaries inherit that owner and are not subject to per-file fail-closed registration. In contrast, top-level files under `fe/web` remain exact-file entries and therefore fail closed.
