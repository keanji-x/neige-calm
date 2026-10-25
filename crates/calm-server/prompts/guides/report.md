# Working with reports

## Edits by others

A `track.report_edited` diff lists block/line changes, including `task` blocks' old/new `ready` and `key`. Multiple saves may share a diff.

- A `doc_rev` is a document anchor, not proof of text coverage. Check known block revisions and section membership; tasks require reads by id. Index reads add no knowledge. Re-read changed targets before writes; full replacement needs a whole-report read.
- An unabsorbed edit conflicts if it changes an unfinished task, your pending write, or a recorded decision/explicit request. A user's rewrite of a finished block is final: accept silently. Merge without asking when you can preserve their content and finish.

## Links

`neige_area_ls`: block ids/link syntax. Read: `neige track cat <report path> --blocks <id>,<id>`. `neige_link_ls`: your report backlinks. Both: follow `next_cursor` to null; pages may end early at 32 KiB.

Link all created/modified Markdown files in the Report, e.g. `[Notes](docs/notes.md)`; verify workspace-root targets exist and preview. Update/deduplicate existing sections' links per the maintenance contract. Planner maintains links after delivery; workers supply paths/paste-ready links via `result`/`artifacts`, never write the Report. Assistants use existing Report tools. Workspace files aren't `neige://source` citations; virtual `report.md` isn't on disk.

Workspace `![alt](path)` images render inline. Save diagrams/screenshots as workspace images and use this syntax.

## Sources

Capture sources with `neige_source_capture` on first reading; cite only captured sources.

## Tags

`neige report tag report.md` lists tags; repeatable `--add <tag>` / `--remove <tag>` edit them. No spaces/commas or body tags. Find area reports: `neige report find area/reports/ --tag <tag>`.
