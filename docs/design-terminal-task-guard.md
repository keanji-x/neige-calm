# Terminal execution guards

Both terminal-create and terminal-worker reserve the canonical execution directory
in their prepare transaction. A persisted launch record lets the initial driver
or UI issue EnsureProc once. Requested and handed-off executions attach on retry;
historical repository-backed rows without a recorded launch owner only attach.
Create a new terminal operation to start a replacement. Standalone instrumentation
without a repository retains its independent runtime contract.

Supervisor control version 2 adds StopAndConfirm. It serializes with process
creation, permanently seals that execution identity, and confirms the managed
execution has stopped before the database releases its writer. A delayed EnsureProc
is rejected even when the earlier launch acknowledgement was lost. ExitPersisted,
a missing PID, a negative liveness probe, and a signal acknowledgement do not release
the guard. Preparation that never issued a request can release during compensation.
Natural exits and restarted servers reconcile persisted references through the same
stop protocol; failures retain the reference for a later retry.

The supervisor supplies the inherited NEIGE_EXECUTION_OP marker. Stop checks the
owned group and marker-preserving descendants that moved into other groups. This
uses the existing trusted-provider contract, not containment against a process
intentionally clearing its marker and escaping the original group. An unreadable
live process in the original group prevents confirmation. An old supervisor is
explicitly refused before starting a new managed terminal; restart it to use the
new stop contract. Browser terminal protocol version 4 is unchanged.

Authenticated terminal.open captures the caller's live native write origin separately
from its stable request hash. Preparation verifies that origin atomically; unrelated
physical scopes reserve their own root. Each terminal keeps its own durable reference,
so ending the caller's turn cannot permit readers while its terminal can still write.
Claude CLI creators and task workers explicitly declare the same terminal writer
reference. Codex viewers declare launch ownership without acquiring a writer reference.
A terminal task reaching a terminal status requests strict stop during reconciliation;
card, Track, and area deletion stop all scoped terminal writers before releasing them.
