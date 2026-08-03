# Ownership validator

This validator prevents implementation work from falling between slices. Entries are explicit files or directory prefixes, overlapping entries are rejected even when they describe future files, and every current file below `fe/core` and `fe/web/src` must match exactly once.

`OWNERSHIP_YAML_FIELDS` has no external schema document: the validator is the authoritative source for that field set, and fixtures are its executable coverage evidence.

Readonly entries freeze interfaces and `styles/`. The repository entry point computes `git merge-base origin/main HEAD`, audits the resulting changed paths, and requires a corresponding non-empty change-request record for every readonly change.

`npm run lint:js` drives `check-readonly-change-requests.mjs`. This Git-dependent check must not run inside Vitest: its subprocess requirement and its base-ref requirement are two independent environmental constraints. The checker exercises both the readonly alarm and the actionable, fail-closed missing-ref diagnostic in isolated Git repositories, while Vitest tests the pure validator with injected changed paths.

## Known escapes

- Ownership describes write authority, not whether an owner implemented the right behavior.
- Renames appear as changed paths and therefore require a request when either affected path is readonly; rename intent is not inferred.
- The tool validates manifest mechanics only. P8b owns the actual future-file manifest and change-request records.

## Stage 2 connection

Load P8b's manifest and change-request file in CI, then call `auditRepositoryOwnership` before parallel implementation starts and on every subsequent change. Keep coverage rooted at both current trees so newly added files fail closed.
