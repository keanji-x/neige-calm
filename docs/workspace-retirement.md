# Workspace retirement and deferred cleanup

Workspace retirement is a durable ownership decision. Physical cleanup is
maintenance, and is not a prerequisite for completing a logical close, deletion,
or workspace repoint.

## Online policy

The current execution contract permits ordinary terminals to access arbitrary
host paths. Different Track identities, working directories, process ids, and
Git worktrees do not establish filesystem isolation. A separate cleanup process
or a rename into trash would not change that contract.

Consequently, online cleanup of these unconfined resources is deferred:

- extra worktrees recorded by Neige provision receipts;
- primary Track worktrees, their branches and candidate references;
- replaced or deleted managed workspace directories;
- ignored-file cleaning in a closed workspace;
- physical removal of workspace trash.

The server does not attempt Git preflight or deletion for a newly deferred
resource. It does not acquire a global launch writer lock, wait for Git metadata,
or spawn a retry task to check the same unavailable isolation condition. This
also avoids an unbounded queue of background tasks. Maintenance discovery may
register additional retirements but must not wait for physical cleanup.

Closed primary workspaces remain available for reopen; closing is not wholesale
retirement. Selected extra worktrees retire independently, except receipts naming the retained
primary root or its ancestors. Those receipts cannot retire the primary on close. Deletion retires
owned selections before removing their live rows. Repoint retires the old owned
selection in the transaction installing its replacement. Borrowed repository
roots remain the user's resources and are never retired by deleting a Track.

## Durable identity and admission

`workspace_resources` remains the owner of logical workspace selection identity.
Its retirement timestamp and cleanup reason survive binding replacement and
Track deletion. Extra worktrees use their Neige provision event as the immutable
retirement key; a close/delete retry cannot select a different resource by path.

A logical selection id and a saved device/inode observation are not physical
cleanup capabilities. Future destructive maintenance still needs independent
ownership, quiescence and isolation proof. Never infer it from a timestamp,
missing directory, failed wait or successful stop request.

A retired recorded root cannot be assigned again or selected as an execution root.
The historical alias limitation below applies when no physical observation exists.
Admission compares the requesting path against immutable lexical and saved
location roots, including the stored observed root when available. It does not
resolve retired directories again: a garbage directory replaced by a dangling
symlink must not block unrelated startup. Location roots are captured when a
selection is bound or a provisioning target is declared, before retirement. Rejection is final and actionable, rather than a Pending
operation indefinitely consuming scheduler capacity. An old resource cannot be
revived by an unchanged-binding shortcut or by creating a new binding at the same
path. Existing arbitrary shell access is unchanged; supported admission is not
being presented as a filesystem sandbox.

Retirement is not a `worktree.removed` receipt. Successful physical removal must
never be inferred from a logical deletion response.

## Historical effects and upgrade

Previously admitted deletion executions may still be running, or their outcome
may be unknown. They retain their original execution identity and removal
protection through recovery and restart. The new policy never bulk-clears those
reservations to improve availability. Such old effects can still require
containment and operator investigation; the strict no-new-cleanup-dependency
contract applies to new deferred work.

Unadmitted historical reclaim operations become deferred reservations without
launching a deletion. Malformed records are not silently treated as unadmitted
or successfully cleaned. An explicit null execution link proves no deletion was
admitted: malformed or mismatched targets are diagnosed locally and the
current authoritative receipt, when readable, supplies the reservation. This
case must not block unrelated execution. Missing or corrupt admitted links
retain the old strict protection.

## Operations and future cleanup

Deferred resources deliberately consume disk. Inventory the retirement timestamp,
root and cleanup reason through the resource store. Do not delete rows or forge
removal receipts to clear a reservation. Disk capacity must be monitored; ordinary
allocation errors remain errors, not permission to delete protected resources.

This delivery does not add a new offline cleanup command or claim that stopping
only the main service proves safety: supervisors and child executions can survive
it. Automatic physical cleanup can be added only for an explicitly enforced
isolation contract. Maintenance of unconfined resources needs a separately
reviewed exclusion/quiescence protocol covering all relevant processes and
shared Git metadata. See issue #3484.

Provisioning now requires the target path in the action's context before the
effect; an output-derived allocation path is refused before release. Relative
declarations are resolved against the action's declared cwd and persisted as an
absolute physical receipt path. The built-in lowerer already declares context paths.
Old Held actions without a declared target are stopped, not reconstructed from
command arguments. Receipts without an independent physical observation, including historical
absolute aliases and relative paths, are recorded as `unknown_location`. An
unobserved selection is classified the same way. Their recorded spelling stays
reserved, but historical alternative spellings (symlinks or `..`) cannot be
reliably reserved without a prior observation. This is an explicit upgrade limit:
no historical physical identity is invented, and none of these records authorizes
physical cleanup. Fresh bindings save a resolved location and fresh Forge targets
are resolved before execution; retirement never probes garbage to reconstruct
lost historical identity.

The admission inventory performs metadata comparisons proportional to retained
resources; it performs no filesystem operation on those resources. Track close
and individual legacy recovery use scoped receipt queries, rather than loading
all provision history once per target. There is no new list of callers, test
names, or application identities to maintain. Future physical eligibility must
be declared in the workspace owner, with its own access/quiescence proof.
