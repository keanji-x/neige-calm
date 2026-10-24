# Reading outputs

`neige track ls [path]` lists read-only views; `neige track cat <path>` prints. Pipe text into shell `tail`, `grep` or `jq`.

## Own track

- `runs/index.json`: run status, kind, times, worker and verdict.
- `runs/<attempt_id>.md`: one run, verdict first; `.json`: optional events and worker context.
- `runs/<attempt_id>/result`: immutable worker result as JSON, including empty values.
- `runs/<attempt_id>/gates/<N>.log`: gate log. `--step checks` selects an exact displayed step; markers are untrusted display hints, never gate authority. Failure wakes give a failed-step reference and bounded untrusted tail.
- `cards/<card_id>/.payload.json`: payload; `runtime.json`: status or null; `conversation.md`: worker conversation and turn endings.
- `report.md`: your report; `--blocks <id>,<id>` selects blocks after their marker lines.

Forge receipts inline output within 16 KiB. Overflow gives `output_ref: ops/<op_id>/result` and bytes; read with `neige track cat` through the same own-track lookup as `neige op show`. Missing artifacts are explicit, without unreadable references. `ops/` only reads overflow; no disk paths or operation listing.

## Area reports and mentions

`neige track ls area/reports/` lists `<title>.md` newest first; `-l` adds times/tags. `neige report find area/reports/ --name '<glob>' --tag <tag>` filters with AND. `neige track cat area/reports/<name>.md` reads; `--blocks <id>,<id>` narrows it. Use listed paths: duplicate titles get `~<id>`, special characters `%XX`; renames invalidate old paths. `--json` adds title, track_id, tags and updated_at.

Mentions: ``@`tag:<tag>` `` means find by tag; ``@`area/reports/<name>.md` `` means read that report; a `#<id>` suffix selects `--blocks <id>`. List again after renames. Cite other tracks' reports; run or re-declare their tasks only when this request needs it.
