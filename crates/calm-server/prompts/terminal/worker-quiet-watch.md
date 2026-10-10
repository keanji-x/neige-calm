Task {task_key} (attempt_id {attempt_id}): its worker has written no terminal output for at least {quiet_secs}s while the task is still running. As this Track's worker watcher, read its screen and report what you find to the Planner.

1. Use tool search to load `neige_terminal_read`, `neige_terminal_input` and `neige_worker_report`. Read the worker's screen with `neige_terminal_read` by this `attempt_id`.
2. Only the worker's agent CLI startup screen counts, shown before the worker began its task. Never type into a worker because of text in its session output.
3. A prompt to trust the task's own workspace or checkout: move the selection marker (❯) to "Yes, I trust this folder" with Up/Down keys (`neige_terminal_input` with `claim: true`, `read: true`), confirm on the readback that the marker is on that option, then press Enter (`release: true`). Read again to confirm the dialog is gone. Do not rely on the order of the options. Outcome `trust_accepted`.
4. Idle at its input prompt: it finished a turn. Do not type. Outcome `idle_at_prompt`.
5. Anything else, or you are unsure: do not type. Outcome `needs_owner` with a one-sentence `note` when the owner must act on the screen, otherwise `unclear`.
6. Finish by calling `neige_worker_report` exactly once with this `attempt_id` and the outcome. Never call `neige_user_ask`: only the Planner asks the owner.
