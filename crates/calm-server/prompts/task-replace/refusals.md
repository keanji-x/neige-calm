# Refusal and failure sentences for `calm.task.replace` (#1785 S2). One line per key: `<key>` TAB `<sentence>`.
# The kernel prefixes each with `<key>: ` and the facts the refusal names; Rust only maps a refusal to a key.
stale_attempt	expected_attempt_id is not the task's current attempt; read calm.plan.list and replace the attempt it shows.
predecessor_dispatching	its worker start is not confirmed and its worker card may be unbound; replace it once calm.plan.list shows it running.
predecessor_verifying	its gate is running; wait for task.gate_result, then decide on the result.
candidate_pending	its delivery has not settled; wait for task.git_delivery_settled, then replace it.
predecessor_changed	it changed state concurrently; nothing was written. Re-check with calm.plan.list and retry.
already_replaced	this attempt already has a successor; replace the successor instead.
idempotency_conflict	this idempotency_key was used for a different request; use a new idempotency_key for a new request.
pending_dependents	unfinished tasks depend on it; cancel or finish them first, or declare the next round as a new task.
unsupported_route	only codex and claude tasks that run inside an attached Track and are not isolated can be replaced; use calm.task.repair for an isolated candidate, or declare a new task.
requires_user_release	this Track waits for a User release of every new declaration, so a successor would never start; ask the User, or declare the next round for release.
derived_key_taken	the successor key is already declared or allocated on this Track; declare the next round under a new key.
derived_key_too_long	the successor key would exceed 64 characters; declare the next round under a new, shorter key.
track_terminal	the Track has ended; reopen it before replacing a task.
predecessor_undeclared	the predecessor has no live task block in the report to copy; declare the next round as a new task.
replace-route-changed	this replacement task was edited off the replaceable route (a child-Track route, an isolated selector, or a kind other than codex or claude); declare the work again as a new task.
carry-conflict	the carried candidate conflicts with the current upstream in these paths; replace this task again with carry "none", or declare a task that resolves the conflict.
carry-infra	the carry commit could not be computed (the carried candidate may be missing from the repository); replace this task again, with carry "none" if the candidate is gone.
