# Reading outputs

The track views are read-only: `neige track ls [path]` lists, `neige track cat <path>` prints. Both compose with `grep`, `jq` and `head`.

## Paths

- `runs/index.json`: every run in the track, with status, kind, times, worker card and verdict.
- `runs/<attempt_id>.md`: a readable summary of one run.
- `runs/<attempt_id>.json`: the structured run. `events.completed.payload.result` is the worker's output, `events.failed` holds failures, `verdict` holds your verdict, `worker_card_payload` holds the task context.
- `runs/<attempt_id>/gates/<N>.log`: the full log of gate run N.
- `cards/<card_id>/.payload.json`: a card's payload; `cards/<card_id>/runtime.json`: its runtime status, or `null`; `cards/<card_id>/conversation.md`: a worker's conversation, including how each turn ended.
- `report.md`: your report. `--blocks <id>,<id>` prints only those blocks, each after its marker line.

## Other tracks' reports

`area/reports/` holds every report in this area, yours included.

- `neige track ls area/reports/` prints one `<title>.md` per report, newest first; `-l` adds update times and tags.
- `neige report find area/reports/ --name '<glob>' --tag <tag>` narrows by name and tag (both means AND).
- `neige track cat area/reports/<name>.md` prints one; `--blocks <id>,<id>` narrows it.
- Use the exact path a listing printed: a shared title gets a `~<id>` suffix, special characters are `%XX`-escaped, and a renamed track's old path stops resolving. `--json` adds `title`, `trackId`, `tags` and `updatedAt`.

Do not run or re-declare another track's tasks unless this track's request calls for it, and cite what you use.

## Mentions

A user message may point at reports with `@` and a code span: ``@`tag:<tag>` `` is a tag (`neige report find area/reports/ --tag <tag>`), ``@`area/reports/<name>.md` `` a report (`neige track cat` it), ``@`area/reports/<name>.md#<id>` `` one block (`--blocks <id>`). After a rename, `neige track ls area/reports/` shows the current name.
