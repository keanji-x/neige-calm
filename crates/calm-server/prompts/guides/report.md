# Working with reports

## Edits by others

A `track.report_edited` diff lists block/line changes, including `task` blocks' old/new `ready` and `key`. Multiple saves may share a diff.

- If your latest read's `doc_rev` is at least the diff's, you already read it. Otherwise read before writing; diff ids/revisions only locate changes.
- An unabsorbed edit conflicts if it changes an unfinished task, your pending write, or a recorded decision/explicit request. A user's rewrite of a finished block is final: accept silently. Merge without asking when you can preserve their content and finish.

## Links

`neige_area_ls` gives report block ids and link syntax; read blocks with `neige track cat <report path> --blocks <id>,<id>`. `neige_link_ls` lists your report's backlinks. Follow `next_cursor` until null for both: pages may end early at 32 KiB.

Link every created/modified Markdown file in the Report, e.g. `[Notes](docs/notes.md)`, previewable from the track workspace root. Verify targets exist inside it; update/deduplicate links in existing sections under the maintenance contract. Planner maintains links after delivery; workers supply paths and paste-ready links in `result`/`artifacts`, never write the Report. Assistants use existing Report tools. Workspace files are not `neige://source` citations; virtual `report.md` is not a disk file.

## Sources

On first reading material you will cite, capture it with `neige_source_capture`; cite only captured sources.

## Tags

`neige report tag report.md` lists tags; repeatable `--add <tag>` / `--remove <tag>` edit them. No spaces/commas or body tags. Find area reports: `neige report find area/reports/ --tag <tag>`.
