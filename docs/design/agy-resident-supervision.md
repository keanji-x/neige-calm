# Antigravity ACP resident supervision

Use the existing ACP resident execution owner for Antigravity. The same
Neige execution supervisor launches, stops and accounts for workspace
attachments for both resident ACP providers. Launches require the configured Neige
execution supervisor. This is L2 because destructive
workspace operations rely on the supervisor's descendant-exit proof.
Operator environment values, including multiline values, travel through the
bounded private stdin release protocol. The encoded startup payload has the
same 16 KiB limit as other supervised provider launches.

Antigravity documents ACP session close. Its 1.5.1 changelog specifies that
closing a session ends an in-flight prompt as cancelled. The documented
contract does not establish that every
background process using the workspace has exited. A successful native
close retires the session attachment, revokes its grant and lets sibling
sessions remain on the same server. Its durable workspace obligation lasts
until the process generation drains. Track and Area teardown, and explicit
abandonment that requires generation proof, stop the generation only after
work outside the operation has finished. After confirmed exit, existing
native session IDs can be reopened and attached credentials are renewed.
[Service contract](https://antigravity.google/docs/ide/extensions/acp) ·
[Close behavior](https://antigravity.google/docs/changelog?ide=vscode&tab=extensions)

Preserve Antigravity's receipt recovery after an exact verified stop. Old
resident receipts retain their persisted process identity and use the
pre-supervisor sweep evidence. New receipts use the exact execution ID.
OpenCode continues to hold an unconfirmed input for an explicit user
decision. AGY's ACP approval replies, compact command, and background-tool
notifications use their existing profile.

Acceptance covers close and reopen on the same PID, sibling work surviving a
session close, confirmed drain before workspace release, old receipt recovery,
and resume without replaying an already issued input.
