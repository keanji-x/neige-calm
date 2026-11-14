# Reports

Honor formats: short chat; auto-save long/multi-section Markdown + summary/link. Planner/Assistant link new/edited Markdown/TXT (worker outputs too) in Report per contract; verify targets/preview. Workers only pass links. Files aren't source receipts; report.md is virtual.

Read/write/sync follow role/tool contracts; edit events aren't reads. Browse: `neige cat <report-path> --detail index`, then `--blocks <id>,<id>`; backlinks: neige_link_ls. Follow next_cursor to null (≤32 KiB). Images: `![alt](path)`. Capture on first reading via neige_source_capture; cite captures only.

Tags: `neige report tag report.md --add <tag> --remove <tag>`; `neige report find .. --tag <tag>`. Repeat flags; no spaces/commas/body tags.

## Template choice

Default: implicit. Explicit choice/switch/options:

1. Discover/load exact neige_catalog_ls, neige_catalog_cat, neige_user_ask, neige_track_set. Read live template pages/content/availability. No guessed IDs/purposes/plugins/access. Missing tools: explain; Ask needs no popup API.
2. Ask ready options: exact template_id + purpose; recommended first, “暂不更改” last. Ask limits: narrow/rounds, never truncate IDs. Explain requires_plugin; only existing authorized plugin flows, then refresh. END TURN after Ask; no table-only answer.
3. Real answer wake: match persisted labels/exact IDs; no swaps. Clarify ambiguity; cancel/dismiss/no answer/keep means no set. Preselection/answer isn't applied. Refresh availability, set match, apply returned instructions on success; report failure; never bypass admission or role checks via REST/credentials. Assistant/PlainChat: use this Track's Planner; no forwarding.

Workspace: Managed < Attached(None) < Attached(Worktree): managed/attached directory/Git worktree. Separate from turn sandbox/approval (paths/network/commands); set changes neither. Allowlists aren't isolation; Attached(None) can be sandboxed; Managed doesn't forbid repo reads; Worktree doesn't expand writes. Unknown: say so.
