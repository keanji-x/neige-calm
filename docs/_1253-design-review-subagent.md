# r1 review — subagent 通道

> 归档说明：这里是该通道**最终返回的正交基建扫描附录**。它与 codex 通道在 B1（launchpad 不可达）上独立同结论；下面两条「被忽略的既有基建」是本通道独有的发现，直接改写了 r2 的 D4/D5。总判：**fix-then-ship**。

## B1 加固（与 codex 独立同结论）

按 cove 扇出的是 `GET /api/coves/{cove_id}/waves`（`routes/waves.rs`），由 `app/providers/queries.ts::useWorkspace` 驱动。system cove 在上游就被排除 ⇒ launchpad wave 在 `workspace.waves` 里**由构造不可达**，不是偶然。

## 被忽略的既有基建（本通道独有，MAJOR）

1. **`GET /api/waves?since&until&cove_id` 是为这个界面专门做的，而且完全没被前端用过。** #250 PR2，`routes/waves.rs` 的 doc comment 直接写着 "calendar window query parameters"；grep 整个 `fe/` 只找得到 `POST /api/waves`。与此同时现在的日历在客户端重算同一件事（`fe/core/domain/wave.ts`）。

   所以设计是在**已有一个 purpose-built 且闲置的跨 wave 时间窗读**的情况下，再提第三个。§6 必须显式回答：是扩 `waves_window` 加计数，还是说明为什么独立 MCP 工具才是对的形状。这也是 D5「空活动判断」最便宜的正确来源。

2. **`calm.day.activity` 会是整个 MCP 面上第一个跨 cove 读。** 现存最宽的是单 cove 的 `calm.cove.outline`（`tools/report_links.rs`），带 50 wave / 40 block / 32KB 三重截断。设计说它是「唯一的授权风险面」——对，但说轻了：它不是「一个更宽的工具」，是一个**新类别**。截断纪律应照抄 `report_links.rs` 的先例，设计里那个光秃秃的 `truncated?: <n>` 比既有先例弱。

## 数据层：比担心的便宜，但比暗示的更绿地

`events` 已有 `idx_events_at`（`0004_events.sql`）和 `idx_events_scope_wave` / `idx_events_scope_cove`（`0007_events_scope.sql`），D4 需要的每个 kind 都持久化（`wave.lifecycle_changed`、`harness.item.added`、`wave.report_edited`，均在 `calm-types/src/event.rs`）。

但**全仓没有任何读者按 `at` 查 `events`**——现存读者一律是 id 序 + wave 作用域。D4 会写下仓库里第一个时间窗事件查询，而 `0004_events.sql` 明确警告「`at` 是墙钟；`id` 才是游标——永不混用」。这条警告应与设计已计划写的 prune 注意事项并列，放进 D4 的 doc comment。

## 否定性结论（已确认）

- 除 `POST /api/today/launchpad/ensure` 外没有别的 `GET /api/today/*`。
- scheduler 是 policy-free 的，没有任何 rollup。
- `plugins/` 只有 `git-forge`，且只做 forge-actions。
- FE 已订阅 `'*'` WS firehose（`app/events/event-bridge.tsx`），但那是 id 游标推送通道，**不可按时间查询**。

⇒ 设计 §3.1「没有跨 cove、按时间窗的活动查询」这个核心结论**成立**。

## 总判

**fix-then-ship**，且 §6 的 PR2 范围另欠一个回答：为什么它不是 `list_waves_window` 的扩展。
