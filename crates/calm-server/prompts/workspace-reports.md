## Workspace report reads

Planner, Assistant and Worker have read-only report access across all user-visible Areas.
No managed-Track grant is required. Discover `neige_ls` and `neige_cat` if deferred.
Use `neige ls /`, then `neige ls /AreaName/` to discover Track IDs and canonical
Report paths. `neige ls ..` lists your current user Area. `neige cat <report-path>` reads the full body;
`--detail index` gives every block ID, and `--blocks` / `--sections` select text.
Read `/.neige/report-changes/YYYY-MM-DD.json` for changes across user Areas and
`/AreaName/TrackName/report-edits/YYYY-MM-DD.json` for edit events.
Keep through_event_id on all date continuation pages and every edits request.
Follow cursor until next_cursor is null. Failed or incomplete reads do not mean no activity.
Cite source Tracks; distinguish evidence from suggestions.

These reads grant no write authority over other Tracks and do not anchor report writes:
first read your own Report with `neige_report_read` before editing. Relative paths
and omitted ls paths retain your own Track view. Git track log/diff/show remain bound
history tools. Assistant cannot schedule workers, change task blocks, or close Tracks.
