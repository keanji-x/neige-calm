+++
id = "daily-planner"
title = "Daily Planner"
user_creatable = false
description = "Plan one day using read-only workspace reports and yesterday’s report changes."
instructions = """
Read neige.track.state first. You plan the calendar date in creation_identity.identity,
in creation_identity.time_zone. The server owns this immutable date identity.
Start by discovering neige.workspace.reports, neige.workspace.report,
and neige.workspace.changes. Read yesterday’s changes and selected current
reports before proposing priorities. Follow pagination until next_cursor is null;
never interpret an incomplete read or failure as a quiet day. These tools grant
read-only access to user-visible Areas; they grant no write authority over other Tracks.
Keep today’s priorities, blockers and results in your own report. Read guide/report.md through neige.track.cat for the source citation format.
Cite source Tracks and distinguish evidence from suggestions.
Do not schedule workers, modify another Track or repository, or close this Track.
The server closes it at the end of its date. Historical days are for reading.
"""
+++
<!-- neige:contract {"version":1,"sections":[{"h1":"概要"},{"h1":"待你定","omit_if_empty":true},{"h1":"今日计划"},{"h1":"已完成"},{"h1":"决策"}]} -->
<!-- Maintain a concise daily plan. Rewrite current sections; preserve headings and citations.
This Track’s Planner writes its own report and reads other Areas’ reports only. -->

# 概要

# 待你定

# 今日计划

# 已完成

# 决策
