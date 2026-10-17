# Qualified directory observations without execution registration

This protocol is a one-shot, authenticated read-only observation. It does not
create execution authority, a persistent source proof or a physical-backing
independence certificate. The kernel owns authorization, dependency closure,
owner exclusion and the stability conditions at its actual commit boundary.

`workspace::execution::objects::qualify_directory_pair(&File, &File)` returns a
nonserializable `QualifiedDirectoryPair<'files>`. It borrows the original ancestor
and subject descriptions, retains the actual current namespace, and uses the
shared actual-FD ext4-driver, mount-table and object qualification. Both objects
must be directories on the same qualified mount. Different or unsupported mounts,
namespace changes and detached objects remain typed Unsupported/Unknown. No
pathname or serialized identity creates this witness.

The negotiated kernel connection consumes that qualification through
`observe_transferred_directories(QualifiedDirectoryPair<'files>)`. Its wire
request contains the actual executor boot, a fresh exchange nonce and
`DirectoryPairFacts`. Exactly two SCM_RIGHTS descriptors carry the ancestor first
and subject second; both have the closed Root directory role. The executor checks
the actual received descriptions and facts, including order and role. The
request contains no execution registration, source incarnation, grant, workspace
ID, generation, path, PID, command, namespace FD or configuration override.

The existing read-only helper handles both registered and transferred origins.
Its context distinguishes these origins explicitly and binds the actual boot,
nonce, roles and directory facts. The same helper image, inherited-FD allowlist,
concrete Child owner, collection deadline and exact wait apply. There is no new
execution/persistent registration, helper daemon or negative-relation algorithm.
Transferred origins must never be represented with dummy execution identifiers
or authority. Losing an outer RPC deadline does not assert that its retained
blocking child owner has completed collection.

A response echoes the full request and reports the directed current VFS relation:
Contains, Disjoint, Unknown or Unsupported. The client checks provenance, rejects
returned descriptors and poisons an interrupted or mismatched exchange. Only the
checked client constructs `ObservedTransferredDirectoryRelation<'files>`, which
retains the original borrowed qualification and cannot be deserialized from a
wire report. Complete typed qualification/refusal replies carry no witness.

Object/profile revalidation does **not** renew an ancestry observation. A retained
directory can be renamed without changing its object identity. The kernel must
hold and consume this evidence under its actual owner exclusion and establish
its qualified same-mount and stability conditions. A selection/intent exclusion
is not automatically a guarantee against outside rename of an entire managed
root or content parent. Directed Disjoint alone does not establish symmetric
non-overlap, physical backing independence or future path confinement. Unproved
artifact/7d obligations remain Recovery/blocked.

Acceptance covers actual same/descendant/sibling FDs, rename/replacement/deletion,
filesystem/mount/namespace rejection, malformed descriptor roles/count/kinds,
boot/nonce/fact provenance, client stream poisoning and actual timeout/failure
reap. Observation must leave execution/registration history unchanged and confer
no Prepare/Launch capability. New API and actual kernel consumers require fresh
independent full L2 reviews and their own gates; earlier executor-repair results
do not qualify this interface.
