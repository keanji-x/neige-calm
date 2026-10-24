# Reading outputs

The track views are read-only: `neige track ls [path]` lists, `neige track cat <path>` prints. Both pipe into `grep`, `jq` and `head`.

## Paths

- `runs/index.json`: every run in the track: status, kind, times, worker card and verdict.
- `runs/<attempt_id>.md`: one run, verdict first.
- `runs/<attempt_id>.json`: the structured run. `events.completed.payload.result` is the worker's output, `events.failed` holds failures, `verdict` holds your verdict, `worker_card_payload` holds the task context and a codex worker's `resolved_model`.
- `runs/<attempt_id>/gates/<N>.log`: the full log of gate run N.
- `cards/<card_id>/.payload.json`: a card's payload; `cards/<card_id>/runtime.json`: its runtime status, or `null`; `cards/<card_id>/conversation.md`: a worker's conversation, including how each turn ended.
- `report.md`: your report. `--blocks <id>,<id>` prints only those blocks, each after its marker line.

## Other tracks' reports

`area/reports/` holds every report in this area, yours included.

- `neige track ls area/reports/` prints one `<title>.md` per report, newest first; `-l` adds times and tags.
- `neige report find area/reports/ --name '<glob>' --tag <tag>` narrows by name and tag (AND).
- `neige track cat area/reports/<name>.md` prints one; `--blocks <id>,<id>` narrows it.
- Use the exact path a listing printed: a shared title gets a `~<id>` suffix, special characters are `%XX`-escaped, and a renamed track's old path stops resolving. `--json` adds `title`, `track_id`, `tags` and `updated_at`.

Do not run or re-declare another track's tasks unless this track's request needs it; cite what you use.

## Mentions

A user message may point at reports with `@` and a code span: ``@`tag:<tag>` `` is a tag (`neige report find area/reports/ --tag <tag>`), ``@`area/reports/<name>.md` `` a report (`neige track cat` it), ``@`area/reports/<name>.md#<id>` `` one block (`--blocks <id>`). After a rename, `neige track ls area/reports/` shows the new name.

## Full results and selected logs

`neige track cat runs/<attempt_id>/result` prints the original worker result as JSON, including empty values. Completion receipts give this path with `--rev <event-id>` so a later completion cannot silently replace it. Run JSON remains optional metadata and artifact claims; it is not needed to read the result.

`neige track cat runs/<attempt_id>/gates/<N>.log --step checks --grep 'error.' --tail 30` selects an exact displayed step and a literal substring before taking the tail. `--start-line 100 --end-line 150` selects inclusive original line numbers instead of a tail. Step markers are untrusted display hints, not gate authority. Selected output is bounded and includes original line numbers, byte positions, path, revision and `next_byte_offset`. Repeat the same selectors with `--rev <returned-rev> --byte-offset <next_byte_offset>` until it is null. A very long line is split only at UTF-8 boundaries. No selectors reads the full output.

Forge receipts inline small recorded output. Large output carries `output_ref: ops/<op_id>/result`, its byte count and content revision. `neige track cat ops/<op_id>/result` reads it through the same track-scoped operation lookup as `neige op show`; line and continuation selectors work there too. An unavailable recorded artifact yields an explicit error and no readable reference is promised. These virtual paths accept no disk paths.
