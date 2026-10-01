# Native workspace issuance recovery

A native write reference persists its exact `clientUserMessageId` before issuing
`turn/start`. The caller's message identifier is kept for transcript attribution;
without one, the lease identifier supplies a unique nonce. Previously issued
identifiers cannot be reused on the same provider thread. A definite refusal or
failure before the RPC releases the unissued reference; missing turn identity,
transport failure, and caller cancellation retain it.

Both read and write task capabilities move into the native request lifecycle.
A read request reserves its own native read reference, without a write root, so
other readers can share the checkout. That independent reference still fences
writers when a Task lease's lightweight stop observation races an unknown RPC.
The same durable nonce, physical stop, and cancellation rules apply to both modes.

Recovery reads the provider's full thread history. Only a user message carrying
that exact nonce identifies an unknown request's turn. Thread identity and actual
cwd must match the reference. The matching turn and every returned turn must be
terminal, the provider must report idle or system error, and the background
terminal roster must be empty before access is released. Missing or malformed
facts retain the reference. Cache absence and interrupt acknowledgments prove
nothing about physical stop.

An explicit cancellation persists `stopping` and seals new turns. Once history
identifies the request, cancellation interrupts that turn, asks the provider to
clean its background terminals, then reads positive facts again. A request absent
from history stays unknown and cancellation returns a retryable conflict. The
current protocol cannot prove that such a request was never accepted: upstream
[thread teardown](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs)
can finish after a shutdown timeout. An archive acknowledgment or `NotLoaded`
therefore cannot replace stop evidence. This boundary is explicit; cleanup never
kills the shared daemon or frees access on an unsupported assumption.

Cold resume restores old conversations' missing workspace binding from the
provider's required `thread.cwd`, including standalone threads whose directory
differs from the Track. An absent cwd or mismatching thread identity invalidates
the binding; no Track path is used as a substitute. Existing active references
remain held until their own stop evidence is available.
