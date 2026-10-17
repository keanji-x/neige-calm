# Per-job Worker tasks

`runWorkerTask(createWorker, input, signal)` creates one dedicated Worker, sends one
typed request and settles on one response, error, decode error, clone error or abort.
A pre-aborted call does not create a Worker. Every created Worker is terminated and
all client/signal listeners are removed at settlement. There is no idle worker,
shared queue, feature registry, module state or synchronous fallback.

`serveWorkerTask(scope, execute)` implements the same one-shot envelope at the
feature-owned Worker entry. It removes its request listener before executing the
algorithm. Its disposer removes that listener and suppresses a pending response.
The channel instance identifies the job; late/duplicate replies cannot settle a
finished client. Unknown or malformed responses reject rather than leave busy state.
The caller supplies cloneable input/output and owns their domain validation.

The feature owns its algorithm and payload types. App composition supplies the
native Worker factory and owns the surrounding scope. Abort terminates active
execution, including synchronous work that cannot otherwise observe a signal.
No request IDs, domain identities, file/navigation capabilities or feature imports
belong in this system.

`public.test.ts` exercises lifecycle, transport and server behavior through the
public entry. The report owner's browser test exercises its actual native Worker
entry, structured clone and rendered prepared-report identity.
