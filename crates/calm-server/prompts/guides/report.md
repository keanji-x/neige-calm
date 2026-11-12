# Reports

Honor explicit output formats. Short replies go in chat. Save long analysis or multi-section content as workspace Markdown without asking; reply with a brief summary and link.

Before reply/delivery, Planner/Assistant add workspace-relative Report links for all new/modified Markdown/TXT (task outputs too): `[Notes](docs/notes.md)`. Verify targets exist inside the workspace and preview. Planner links worker outputs; workers pass paths/paste-ready links via `result`/`artifacts`, never edit Report. Update/deduplicate existing sections per Report contract. Workspace files aren't `neige://source` citations; virtual `report.md` isn't on disk.

## Edits by others

`track.report_edited`: block revisions vs session knowledge; no read recorded. Small known edits give diffs; others block refs. Saves may share suggestions.

- `doc_rev` anchors documents, not text coverage. Check known block revisions and section membership; tasks need id reads. Index reads add no knowledge. Re-read changed targets before writes; whole replacements need whole-report reads.
- Unabsorbed edits conflict if they change unfinished tasks, pending writes or recorded decisions/explicit requests. A user's finished-block rewrite is final: accept silently. Merge without asking when preserving their content and finishing.

## Links

`neige cat <report-path> --detail index`: all block ids, no paging. Read with `--blocks <id>,<id>`. `neige_link_ls`: backlinks. Directory/backlink pages use next_cursor until null; pages may end early at 32 KiB.

Save diagrams/screenshots as workspace images; use `![alt](path)` inline.

## Sources

Capture sources on first reading with `neige_source_capture`; cite only captures.

## Tags

List: `neige report tag report.md`; edit: repeat `--add <tag>` / `--remove <tag>`. No spaces/commas/body tags. Find: `neige report find area/reports/ --tag <tag>`.
