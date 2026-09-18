# S5 Host Enrollment

Implementation base: `3757477219263c3e9b887077473c384762545336`.
Worktree: `.claude/worktrees/tailnet-enrollment`, sole writer. No rebase onto
the subsequently fetched main, account/device operation, deployment or push.
Independent A and CLI B implementation reviews are owned by the parent task.

## Contracts

- Existing S3 control v1 remains separate. Enrollment control is v2, with
  strict bounded messages, request deadlines and generation IDs. An old helper
  fails explicitly; replacing its binary requires a full neige-app restart.
- Long-lived credentials are read only inside Go from a typed private file.
  Every directory component is opened without following symlinks; the final
  parent must be owner-only 0700 and each regular file owner-only 0600 with one
  link. Neither Rust nor Settings receives the OAuth secret or access token.
- Official HTTPS OAuth/key endpoints are fixed. No credential redirects,
  environment proxies, ACL changes, or device APIs. The current embedded
  node must match expected tailnet/origin, have HTTPS/ingress ready, and report
  Tailnet Lock disabled. System Tailscale state is not used.
- Key creation requests 300 seconds and verifies actual created/expires,
  explicit returned capabilities and exact phone tags. Invitation lifetime is
  at most 180 seconds and cannot outlive the real key expiry. Missing boolean
  capability fields are rejected, not assumed false.
- A bounded durable unknown-result marker precedes the key POST. Once an ID
  arrives it is persisted with the returned expiry before grant validation.
  Failed/uncertain creation is never automatically repeated. Unknown IDs and
  configuration-mismatched records remain for administrator reconciliation.
- Cleanup records bind to the canonical issuer configuration, including the
  expected tailnet, client ID, secret path, tags and origin. A new binding's
  404 cannot erase an old binding's record. Cleanup-only runs without starting
  tsnet, a listener or node state. Disabled hosts periodically run this mode.
- Rust v2 invitations are separate from v1. Claim hashes the ticket/attempt
  secret, binds the first attempt, and allows only identical attempt retries.
  Redeem creates one PairedDevice session under the existing grant lock;
  retries return only that still-live session. Revoke/disable/cancel invalidate
  retries. Creation completion is fenced against late cancellation/disable.
- Owner management is PasswordLogin-only, including direct main-port access;
  dev_autologin and PairedDevice sessions cannot manage enrollment. Public
  ingress exposes only claim/redeem, never management. Responses are no-store.
- Settings holds QR only in component memory, drops it at its absolute deadline,
  cancels a late response after unmount, and never auto-retries creation.
  Cleanup state distinguishes cloud-key cleanup from removing a joined device.
- The fixed claim/redeem DTOs and seven-field `neige-enroll:v2:` envelope match
  the approved handoff. No receipt/callback API. REST revision is 9 and web compatibility is 30;
  final S4 integration must rebuild its bundled frontend against this contract.

## Local Configuration

This is deployment configuration, not a command to modify an existing account.
Do not paste credentials into chat, argv, environment variables or logs.

```toml
[tailnet]
provider = "private-tailnet"
enrollment_config = "/absolute/private-directory/tailnet-enrollment.json"
```

The directory must be owned by the Neige user with mode 0700. Configuration and
secret files must be regular, non-symlink files owned by that user with mode
0600. Use canonical absolute paths without symlinked ancestors.

```json
{
  "schemaVersion": 1,
  "clientId": "ADMIN_PROVIDED_CLIENT_ID",
  "secretFile": "/absolute/private-directory/tailnet-oauth.secret",
  "phoneTags": ["tag:neige-phone-test"],
  "expectedTailnet": "ACTUAL_CURRENT_TAILNET_NAME",
  "expectedOrigin": "https://ACTUAL_EMBEDDED_NODE.ts.net"
}
```

Use the embedded node's actual CurrentTailnet.Name, never `-`, and its actual
canonical HTTPS origin. The administrator provisions only auth_keys Write and
the fixed phone tags, and separately authorizes their minimum access policy.
Neige does not modify tag ownership, grants, device approval or Tailnet Lock.

`enrollment-ledger.json` is secret-free but security-relevant. An unknown result
requires administrator reconciliation against the original account. Do not
delete or rewrite it simply to enable another attempt. Lost key IDs cannot be
recovered by blindly repeating the create request.

## Verification Record

Logs for this work use `/tmp/neige-1712-s5-*`. Final commands, counts and mutation
results are appended after their actual completion. Early green runs are not
claimed as final evidence after a relevant edit.

The missing-capability regression was observed red in
`/tmp/neige-1712-s5-capability-red.log` before the required-field fix.

## Required External Acceptance

No existing credential files were read and no account or device calls were
performed. OAuth API acceptance of a real 300-second lifetime is unverified.
Tests use synthetic credentials and controlled HTTP responses; they are not
evidence of cloud expiry, tag scope enforcement, MagicDNS/HTTPS provisioning,
device approval behavior, or a real phone's one-scan flow. Those remain release
blockers for the parent integration task. The console's 1-90 day UI is not
treated as proof that the API accepts or clamps 300 seconds.

Go VCS metadata from this nested worktree is not source provenance. Final helper
artifact validation must use a clean standalone checkout of the committed S5
source and independently record that SHA, alongside the ts_omit_logtail build
verification. No production release is authorized by these local checks.
