# Proven helper startup failure

Related: #3494 and #3466. Deferred Planner follow-up: #3497.

Main #3480 owns bounded task admission/retry and the Linux descriptor fallback.
This change reuses those implementations and adds only the missing executor
conclusion when preparation or std's exec handshake proves the helper did not
start. It adds no scheduler mechanism, migration or decision tool.

The Linux process owner mints a private typed NoChild proof around preparation
and Command::spawn. The executor persists its existing NoChild state through the
existing latch and settlement owner. Errors after successful spawn, including
native pin/readiness errors, remain conservative Unknown outcomes. Existing old
Unknown journals retain their original protection.

L2: executor settlement and resource-release authority. Regressions exercise real
failed helper exec, replay/restart and post-spawn failure classification. Reverse
mutation must make the failed-exec assertion red when typed classification is
removed, then restore the complete focused selection to green. Existing main
child_fds tests retain descriptor-isolation coverage.

Planner policy keeps a concise instruction to inspect task failures and record
the next action. Persistent stall observation and explicit follow-up decisions
remain in #3497 and must consider existing progress obligations.
