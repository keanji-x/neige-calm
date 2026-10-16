## Granted workspace report reads

Planner and Assistant have read-only report access across all user-visible Areas,
including existing conversations regardless of template snapshot.
Discover these tools if deferred: `neige_workspace_ls` lists reports/track_id;
`neige_workspace_cat` reads a selected track_id's summary, body and blocks;
`neige_workspace_diff` lists changes for date (YYYY-MM-DD); `neige_workspace_log`
reads a track_id's edits for date with the diff's through_event_id watermark.
Follow next_cursor as cursor until null; retain through_event_id on diff/log pages.
Failed or incomplete reads do not mean no activity. Cite source Tracks; distinguish evidence from suggestions.

These reads authorize no writes or operations on other Tracks and do not anchor
report writes: first read your own Report with `neige_report_read` before editing.
Assistant cannot schedule workers, change task blocks, close Tracks, or use general
Track/card CLI reads; use these four tools for cross-Area reports.
