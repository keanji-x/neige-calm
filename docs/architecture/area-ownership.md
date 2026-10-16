# Area ownership and creation identities

C2a establishes ownership at the persistent Area writer. It does not enable
additional-user credentials or claim that the remaining application surfaces
are ready for multiple users. Area membership, credential-authenticated user
identity, event attribution, and deployment-owner authority remain separate.

`calm-types::organization` owns the closed owner/member vocabulary and the rule
that both roles collaborate while only the owner deletes. `calm-truth` owns
registered application users, current membership queries, and persistence.
Area routes consume that policy using the verified session Principal. They do
not derive authority from Actor headers, profile names, Linux users, or a role
string supplied by a client. System Areas have no human membership; their HTTP
access requires the authenticated deployment owner, and their creation remains
a separate kernel path. Today is not made personal by this change.

User Area creation requires an explicit registered owner in both
`area_create_tx` and the raw repository interface. The Area and its single owner
membership commit with defaults, optional creation identity, and creation event.
The unique partial index proves at most one owner; the canonical writer supplies
the required owner. There is no owner-transfer, member-mutation, or user-deletion
API in this slice. Internal global repository readers retain their explicit
kernel purpose; the user-facing Area list reads current memberships.

Migration 0167 adopts the released single-user database exactly once: it
registers the stable `local-owner` application identity, assigns it ownership of
existing user Areas, and copies every permanent creation key, including deleted
Area tombstones, into that creator namespace. Existing key bytes, request hashes,
Area IDs, and historical events are preserved. This migration value is never a
runtime creator fallback. Reopening does not repair missing membership.
Startup explicitly reconciles the configured owner's display name before tasks
or listeners start; that updates profile text and never assigns membership or
issues credentials. A later provisioning owner may explicitly register other
stable IDs; session creation itself cannot register or resurrect an identity.

Creation bindings are immutable and permanent, keyed by `(creator_user_id, key)`.
Two users can independently use the same key. A retry checks current membership
before returning the original Area, without repairing membership or repeating
mutable folder/template validation. Deletion exhausts that creator's key
permanently. No old-table lookup or hash-only namespace fallback exists.

Updates check access before filesystem validation and again in the write
transaction. Deletion checks ownership while holding the existing Area deletion
fence, before quiescing or recycling, and again in the final transaction. The
existing lock order, execution checks, workspace recycle compensation, and event
transaction remain authoritative. Workspace grants and lifecycle are not owned
by this membership module.

An older binary refuses a database carrying unknown migrations. Rollback after
migration requires a complete pre-upgrade backup or a forward fix, never deleting
membership rows to make an older binary accept the database.
