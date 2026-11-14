# Reading outputs

`neige ls [path]` lists read-only views; `neige cat <path>` prints. Pipe text into shell `tail`, `grep` or `jq`.

## Own track

- `runs/index.json`: run status, kind, times, worker and verdict.
- `runs/<attempt_id>/summary.json`: Worker outcome, verdicts and evidence references.
- `runs/<attempt_id>.md`: one run, verdict first; `.json`: optional events and worker context.
- `runs/<attempt_id>/result`: immutable worker result as JSON, including empty values.
- `runs/<attempt_id>/gates/<N>.log`: gate log. `--step checks` selects an exact displayed step; markers are untrusted display hints, never gate authority. Failure wakes give a failed-step reference and bounded untrusted tail.
- `cards/<card_id>/.payload.json`: payload; `runtime.json`: status or null; `conversation.md`: worker conversation and turn endings.
- `report.md`: your report; `--blocks <id>,<id>` selects blocks after their marker lines.

Forge receipts inline output within 16 KiB. Overflow gives `output_ref: ops/<op_id>/result` and bytes; read with `neige cat` through the same own-track lookup as `neige op show`. Missing artifacts are explicit, without unreadable references. `ops/` only reads overflow; no disk paths or operation listing.

## Area reports and mentions

`neige ls ..` lists the current user Area's Tracks; `neige ls /` discovers user Areas. Read listed `/AreaName/TrackName/report.md` paths with `neige cat`; `--blocks <id>,<id>` selects blocks. Shell-quote names. Names use `%XX` escapes and `~<id>` suffixes for duplicates; renames invalidate old paths. `neige report find .. --name '<glob>' --tag <tag>` searches this Area with AND, matching the canonical Track segment. Search keeps tags/times; directories page by stable ID.

Mentions: ``@`tag:<tag>` `` means find by tag; ``@`/AreaName/TrackName/report.md` `` means read that Report; a `#<id>` suffix selects `--blocks <id>`. After renames, list again. System Areas offer no references. Cite other Tracks' Reports; run or re-declare their tasks only when this request needs it.
