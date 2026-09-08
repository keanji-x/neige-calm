# Explicit cross-area directory reuse (#1585)

Users may create a track in the selected Area while reusing a directory claimed
by another Area. The first request still reports a conflict; the confirmation
names the exact folder claim and owner returned by that conflict. Successful
reuse preserves the original claim and does not attach a second folder.

## Contract change request and decision

The core/api owner boundary is `fe/core/api/generated/openapi.json`, a frozen
generated contract. The requested change adds optional
`allow_cross_area_cwd: { folder_id, area_id }` to track creation. The orchestrator
approved this scope during PR review on 2026-09-08, subject to preserving existing
idempotency fingerprints and transactionally validating the exact foreign claim.
The domain request type and both frontend creation flows consume that contract.
No migration or persisted claim representation changes are required.

The ownership trailer for the schema-changing commit is:

```
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — expose explicit cross-area directory reuse authorization (#1585)
```

## Acceptance checks

- Without authorization, foreign directories remain conflicts.
- With matching authorization, the track belongs to the selected Area and the
  sole original folder claim remains unchanged.
- Deleted, replaced or mismatched claims reject stale authorization, including
  requests carrying `attach_folder: true`; rejection creates no track or claim.
- A vanished claim returns an invalid-input response for either attachment flag,
  so a definitive refusal unlocks the frontend draft instead of trapping it in
  retries with obsolete consent. An earlier unconfirmed request remains locked.
- Authorization cannot widen an ancestor claim or authorize an unclaimed path.
- Requests without authorization retain their pre-upgrade fingerprint. Both
  message-less and first-message requests replay the same track after upgrade.
- Both frontends require a distinct confirmation, preserve the chosen target
  Area and send the exact conflicting claim identity.
- Editing the legacy form's Area selection or directory invalidates pending
  consent; a delayed conflict response cannot restore an action for old input.

The regression tests exercise production HTTP entry points, including an old
persisted fingerprint, rather than reproducing the admission logic in fixtures.
Focused verification and independent reviews are recorded in the PR.
