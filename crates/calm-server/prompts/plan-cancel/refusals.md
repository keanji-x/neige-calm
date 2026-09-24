# Refusal sentences for `calm.plan.cancel` on a task that is past `pending` (#1785). One line per key: `<key>` TAB `<sentence>`.
# The kernel prefixes each with `task <key> is <current status>: `; Rust only maps a refusal to a key.
dispatched	its worker start is not confirmed and its worker card may be unbound; cancel it once calm.plan.list shows it running.
verifying	its gate is running; wait for task.gate_result, then decide on the result.
ended	its execution already ended; only pending and running tasks can be canceled.
changed	it changed state concurrently; re-check with calm.plan.list and retry.
route	only a codex or claude worker running inside this Track can be canceled while running; this task finishes on its own.
isolated	an isolated worker is stopped by its own controller, not by calm.plan.cancel; wait for its settlement briefing.
unbound	no worker card is bound to it yet; cancel it once calm.plan.list shows its worker.
