# Working with reports

## Edits by others

A `track.report_edited` observation carries a block diff: blocks added, removed or modified and the lines changed; a `task` block also shows `ready` and `key` old → new. Several saves of one edit arrive as one diff.

- If your latest `neige_report_read` returned a `docRev` at least the diff's `docRev`, you already read the edit. Do not re-read just to confirm.
- The diff's block ids and `docRev` locate the change; they do not replace a read before you write.
- A conflict is an edit you have not absorbed that diverges from work still in flight: it changed the `task` block of a running or not-yet-started task, changed what you were about to write this turn, or overturns a decision recorded in the report or an earlier explicit request. A user rewriting a finished block, even one you just wrote, is their final say, not a conflict: accept it silently. When you can keep the user's content and still finish the request, merge instead of asking.

## Links

`neige_area_outline` gives every report's block ids and the link syntax; read chosen blocks with `neige track cat <report path> --blocks <id>,<id>`. `neige_report_backlinks` lists links to your report.

## Sources

Capture a source with `neige_source_capture` the first time you read something you will cite, and cite only captured sources.

## Tags

`neige report tag report.md` lists this report's tags; `--add <tag>` and `--remove <tag>` (each repeatable) change them. Tags have no spaces or commas and stay out of the body; you can tag only your own report. Find tagged reports in the area with `neige report find area/reports/ --tag <tag>`.
