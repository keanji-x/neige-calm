# 用户通知：点只属于「指名给你、还没处理的事」（#1829）— 设计 v3.3（第五轮确认评审处置后，2026-09-28）

**实现说明（Implementation notes，合并前补记，正文未改）**：REST 为 "13"→"14"、WEB_COMPAT 为 32→33（#1822 先占了文中的 12→13 与 31→32）；迁移号为 `0121_activity_dismissals.sql`；planner down 的 `text` 经内核 `readable_error_text` 化简，不再逐字（owner 要求），转录 wire 另带同源的 `turn_error_text`；侧条行的 UI 重新设计在 preview 上由 owner 确认。

基线：`origin/main` = `dc37cb24a`（工作树 `1829-design`）。所有 `path:line` 都在该基线上读取；4140 的数字由 §2 的只读命令取得（2026-09-28 12:30 取数，Q1/Q2/Q3/Q10 于 13:05 重取；数字会漂，验收一律当场重取）。v1 → v2、v2 → v3、v3 → v3.1、v3.1 → v3.2、v3.2 → v3.3 的逐项处置在 §14。前作：`docs/architecture/1722-track-activity-indicators.md`（下称 1722）、`docs/architecture/1743-activity-v2.md`（下称 1743）。标记：**[v]** = 已按行号或查询核实；**[a]** = 假设，未核实。

**owner 的两条规则（每一节都受其约束）**：

1. **不要无限扩张**：抓已观测的痛点；简单优先，只用覆盖**已观测**症状的最少机制。假设性情形写成 §11 的一行「已知缺口」，不变成机制。
2. **兼容只看 4140**：只以 4140 库（`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`，只读打开，永不写）为准；不做一般兼容叙事。库里有的行要么继续工作，要么被清理。数字与命令都写在文中（§2、§9）。

**已批准的模型（不重议，本文只做设计）**：三层——状态（lifecycle badge、working 转圈，只描述，永不出点）/ 未读（不变）/ 通知（琥珀或红点 + 数量 = 一件指名给用户、还没处理的事）。lifecycle 本身不产生点。通知只有两个来源：**ask**（琥珀）与 **planner down**（红）。task 失败、worker 会话失败、`reviewing`、lifecycle `failed`/`done`、交互卡退出都不再是通知来源；1743 的失败老化规则删除。

## 0. 决定一览

| # | 问题 | 决定 | 依据 |
|---|---|---|---|
| D1 | blocked 的问题在哪 | `events` 里 `kind='track.lifecycle_changed'`、`payload.to='blocked'` 的最新一行，文本是 `payload.agent_message` | §1 K9–K10；4140 blocked 边 7 条，缺 `agent_message` 0 条（Q9） |
| D2 | notify 在哪 | 转录表 T 的 `item/completed` + `mcpToolCall` + `tool='calm.user.notify'` 行（即今天的 E2 谓词），文本 `$.item.arguments.text`；Claude Planner 经翻译层写同形行 | §1 K8、K12 |
| D3 | ask 何时关 | U = Planner 卡上 `harness.user_message.enqueued`、`actor.kind='User'` 的 `MAX(at)`；L = 本 track `track.lifecycle_changed` 且 `payload.from='blocked'`（任一 actor）的 `MAX(at)`。**ask（两种生产者同一规则）开着 ⇔ `ask.at_ms > MAX(U, L)` 且未 Dismiss**。两个后端、两个入口都走同一路由、同一事件 | §1 K13–K15；Q10；§3.1 |
| D4 | notify 的 ask 与 lifecycle | 不看当前 lifecycle，但 Planner 离开 blocked（L）会关掉在那之前的 notify ask；对 blocked 的 ask，`at_ms > L` 恰等价于「仍在 blocked」（更新的 blocked 边就是更新的 ask） | §3.1 |
| D11 | done / 归档 track | **items 不再做终态过滤**：done / 归档 track 上开着的 ask 与 planner down 照样显示；终态过滤只留给 `cards[]`（丢 `input`/`failed` 卡级结论，只留 `working`） | §3.4、§4.2 |
| D5 | planner down 的证据 | Planner 卡上最新一条非 `interrupted` 的 `turn/completed` 行的 `$.status='failed'`；文本 `$.error.message`；两个后端同形 | §1 K16–K18；Q3、Q7 |
| D6 | 「之后成功完成一轮」 | 同一查询：最新的那条变成 `completed` 即关。**不用** `worker_sessions.last_turn_completed_ms`：Claude Planner 从不写它（2/2 为 NULL，Q11） | §1 K19 |
| D7 | 「重启 Planner」 | **owner 定：没有 Restart**。两种项共用 `Reply`（打开 Planner 输入框）；planner down 行显示原因 + 固定一句 `Fix the cause, then send the Planner a message to continue.`。发消息本身就让 Planner 继续（codex 的 system_error 楔住经 `/planner/input` 恢复；Claude 起进程后的失败轮之后会话仍 idle，下一条消息起新一轮）。不新增路由；无消息的重启只有 `/planner/reset`，它硬删转录，FE 有测试钉住「无路径到它」 | §4.6 |
| D8 | Dismiss 存储 | 新表 `activity_dismissals(track_id, item_key, dismissed_at_ms)`，主键 `(track_id, item_key)`；`item_key` 含证据行 id ⇒ 同来源再次发生是新 key、重新亮 | §4.5 |
| D9 | payload | `schemaVersion` 1 → **2**；`items[] = {source: ask\|planner_down, key, text, at_ms}`；`attention` 与 `cards[]` 形状不变 | §4.3 |
| D10 | 版本 | `WEB_COMPAT_VERSION` 31 → 32（S1）；`REST_API_VERSION` "12" → "13"（S3，新路由） | §4.3、§4.5 |

## 1. 现状代码地图（`dc37cb24a`）

### 1.1 内核

| # | 事实 | 位置 |
|---|---|---|
| K1 | 投影器每次唤醒 / 30 s tick 从持久行整算一个 track；bus 事件只是唤醒 | `track_activity.rs:437-498`（`recompute_track`）、`:559-572`（`reconcile_all`）、`:622-667`（`run`） |
| K2 | payload：`schemaVersion, working, attention(none\|input\|failed), activity_at_ms, items[{kind, source(card\|task\|session\|lifecycle), id, card_id, at_ms}], cards[{card_id, state}]` | `track_activity.rs:41-102` |
| K3 | 四个通知来源：task `failed` → `{failed, task}`（`:234-251`）；会话 `state='failed'` → `{failed, session}`（`:282-298`，含 `failed_session_is_finished_work` 例外 `:168-183`）；lifecycle `blocked\|reviewing` → `{input, lifecycle}`、`failed` → `{failed, lifecycle}`（`:301-313`）；卡级 `input` 自 1743 S3 起无生产者 | `track_activity.rs` 同上 |
| K4 | 失败老化：`task/session` 项只在 `at_ms > P` 时计入，P = `PLANNER_LAST_TURN_SQL`（Planner 卡全部会话 `MAX(last_turn_completed_ms)`） | `track_activity.rs:211-213`；`track_activity/sql.rs:113-121`；`read_rows` `:409` |
| K5 | 终态过滤：`done` 或归档 ⇒ `items=[]`，`cards[]` 只留 `working` | `track_activity.rs:315-323` |
| K6 | `attention` = items 的折叠（`failed > input > none`） | `track_activity.rs:145-156` |
| K7 | 唤醒表：`harness.phase.changed`、`item/completed`+`mcpToolCall`、会话事件、任务事件、`track.lifecycle_changed`、`track.report_edited`、`track.updated`；**没有** `harness.user_message.enqueued` | `track_activity.rs:576-601` |
| K8 | unread 证据：E1（任一卡非 interrupted `turn/completed`）、E2（notify 行）、E4（非 User 的 lifecycle 边，故 `failed`/`done` 照亮 unread）、E7；E3、E8 在 fold 里 | `track_activity/sql.rs:12-25,224-241` |
| K9 | `working → blocked` 只有 Planner 能写；`blocked → working` User 与 Planner 都能 | `calm-types/src/track_lifecycle.rs:172,180` |
| K10 | Planner 的 lifecycle 写必带 `agent_message: String`，事件 `TrackLifecycleChanged.agent_message = Some(..)`；`calm.ratify.request` 也在同一写里做 `working → blocked`（`reason` 即 message） | `track_lifecycle.rs:32-66`；`prompts/tools/calm.ratify.request.md:1`；4140 事件 28475 的 `agent_message` = 该次 ratify 的 `reason` |
| K11 | `track.*` 事件不在剪枝清单；`harness.user_message.enqueued` 也不在 | `calm-truth/src/events_prune.rs:34-41` |
| K12 | `calm.user.notify` 只对 Planner 可见，内核只校验 `text`、什么都不写；codex 与 Claude 都把调用存成 `item/completed` 的 `mcpToolCall` 行（Claude 经 `translate.rs` 把 `mcp__calm__…` 还原成点名、`status` 取 `completed/failed`） | `mcp_server/tools/user_notify.rs:1-2,57-58,83-91`；`claude_planner/translate.rs:395-432,479-483` |
| K13 | 用户给已有 Planner 发消息只有一条路由 `POST /api/cards/{id}/planner/input`：先 `ensure_live_planner_harness`，再 `observe_user_message_durable`，再写审计事件 `harness.user_message.enqueued`（scope = Card，`scope_card` / `scope_track` 都填；审计写失败只记日志）。该事件的**第二个写者**是 harness 启动操作的首条消息：`planner_harness_start_adapter.rs:871-900`，由 track 创建（`routes/tracks/create.rs:702`，新 track 的 Planner 卡）与会话创建（`routes/track_conversations.rs:210`，`HarnessProfile::Assistant` 的新卡，含 Today 摘要的合成 User）触发 | `routes/cards.rs:170,833-944`（`:862`、`:900`、`:921`、`:930`）；`calm-types/src/event.rs:385-386` |
| K14 | Claude Planner 卡同走该路由：`card_runs_headless_harness` = `PlannerBinding::from_card`，按 `planner_provider` 键接纳两种后端 | `routes/cards.rs:88-90`；`harness/profile.rs:62-80` |
| K15 | Planner 的 system prompt 两个后端同源：Claude 用 `planner_instructions(..)` 再追加 `long-lived-processes.md` | `claude_planner/wiring.rs:14,45-54`；`planner_card.rs:5` |
| K16 | codex：`thread/status systemError` ⇒ `Wedged{system_error}`，本次通知处理末尾的 `persist_snapshot` 就发出 `harness.phase.changed`（会话行 `failed`）；**之后**那条 failed `turn/completed` 才落转录（`:1945`），`persist_failed_system_error_snapshot` 不发事件（`:1947`，`:3851-3872`），唯一更晚的事件是 `harness.item.added{item_type: None, method: "turn/completed"}`（`:1950-1957`）。若 loop 在那条完成通知到达前已被 quiesce，failed 行改由下次人发消息时的恢复从 provider 历史补写（`shared_codex_appserver/preserving_recovery.rs:103-106,125-165`），随后同样发 `harness.item.added{method: turn/completed}`（`routes/planner_recovery.rs:81-105`） | `harness/run_loop.rs:1839-1847,2221,1939-1958`；`harness/state.rs:47` |
| K17 | Claude：**进程起来之后**本轮的任何失败（init 检查失败、协议错、CLI 退出、错误结果）都是 `TurnCompleted{status:'failed', error:{message}}`；harness 走普通 `TurnCompleted` 臂，**不进 Wedged**，会话仍 `idle`。**起进程之前**的失败（未配置、`--version` 校验、`stop`、`spawn` 本身）从 `turn_start` 返回 `Err`，只进内存 `issuance_block`、批次回队，**不写任何行**（K24） | `claude_planner/driver.rs:72-112,528-560`；`translate.rs:165-175`；`run_loop.rs:1962-1980`；起进程前：`claude_planner/session.rs:424,433,449,450,498-505`、`run_loop.rs:3342-3390` |
| K18 | 普通完成路径：转录的 `turn/completed` 行由 `turn_outcome::record` 写，之后才持久化 phase（发 `harness.phase.changed`）。**systemError 路径相反**（K16）：phase 先、行后 | `harness/run_loop.rs:1975-1980,3814-3848`；`harness/turn_outcome.rs:6-30` |
| K19 | `worker_sessions.last_turn_completed_ms` 只由 codex daemon 流的 feeder 写 | `liveness_feeder.rs:1-3,66-74` |
| K20 | 人发消息可恢复一个 `failed` 的 codex Planner：`candidate(.., human_send)` 接纳 `recoverable_snapshot`（`failed`、`completed_at_ms IS NULL`、phase `wedged`、reason `system_error`、线程吻合），`recover` 调 `resume_system_error_conversation` | `routes/cards.rs:1232-1322`；`routes/planner_recovery.rs:14-33,36-110,115-146` |
| K21 | `/planner/reset` 以「清空转录」标志（`:1417`）与 `force_new_thread: true`（`:1418`）起新会话，事务内硬删整张卡的转录 | `routes/cards.rs:1336-1420`；`operation/planner_harness_start_adapter.rs:1281-1283` |
| K22 | overlay 注册表：`activity` 校验器对 payload 闭合；读侧只丢 `schemaVersion > max` 的行；写侧 `schemaVersion` 缺省或等于 max | `calm-truth/src/validation.rs:97-98,212-296,326-330,446-510`；单元 `:859-960` |
| K23 | 每个 track 恰一张 Planner 卡（唯一索引 `idx_cards_one_planner_per_track`）；4140 27/27 | Q14 |
| K24 | `issuance_block`（如「Pick a model to start it again」，以及 K17 的 Claude 起进程前失败）只在内存，不落库 | `harness/run_loop.rs:206,773-775,3342-3390`；`routes/cards.rs:1218` |

### 1.2 前端

| # | 事实 | 位置 |
|---|---|---|
| F1 | 解码：`activityOverlayWireSchema.schemaVersion: z.literal(1)`；item 形状 `kind/source/id/card_id/at_ms`，坏 item 单独跳过 | `fe/core/domain/track.ts:144-191` |
| F2 | 谓词只读 overlay：`needsUserAttention = attention==='input'`、`hasFailed = attention==='failed'`、`trackActivityState` | `track.ts:578-592` |
| F3 | `ActivityItem{origin, id, cardId, atMs, kind}`、`attentionKindOf`（只有测试用）、`foldAttentionByCard`（1743 S4） | `fe/core/domain/activity.ts:13-25,60-90` |
| F4 | 侧条 = `attentionNotifications`：按卡折叠，标题取 `cardGoalTitle` / `notificationCardLabel`，正文是 8 句固定文案；唯一按钮 `Review` | `app/router/public.tsx:1867-1905,2087-2090`；`features/track/page/public.tsx:43-57,517-583` |
| F5 | `Review` 的去向：无卡 → 打开 Planner 抽屉；会话卡 → 打开该会话；网格卡 → 跳到卡 | `router/public.tsx:2103-2109,2409-2425` |
| F6 | rail「Waiting on you」与 strip 计数、Today 第一个数 = `needsUserAttention ∨ hasFailed` | `app/shell/sidebar.tsx:83-84,180,206`；`features/today/public.tsx:135,254` |
| F7 | 唯一的发送操作 `sendPlannerInputOperation` → `POST /api/cards/{id}/planner/input`，抽屉与 track 输入框共用 | `fe/core/domain/conversation.ts:437-445`；`app/providers/queries.ts:249` |
| F8 | FE 不得有路径到 `/planner/reset`（测试钉住） | `router/planner-conversation.test.tsx:247-268` |
| F9 | 相对时间 `relativeTime(atMs, nowMs)` 在同一 feature 域 | `features/track/row/public.tsx:47` |

### 1.3 本文推翻 1722 / 1743 的哪些决定

| 前作决定 | 位置 | 处置 | 为什么 |
|---|---|---|---|
| attention = permission / `waitingOn*` / lifecycle `blocked`、`reviewing`；failed = harness 楔住、当前 attempt failed、`systemError`、lifecycle `failed` | 1722 §3、§4.1 | **推翻**：input 只剩 ask，failed 只剩 planner down | 108/125 次 `reviewing` 由内核推入（issue；Q12 当场为 110/127）；task/worker 失败是 Planner 的事 |
| 失败的「已处理」= Planner 之后完成过一轮（老化） | 1743 §3、§4.1 规则 2 | **删除**（K4 全删，含 P 查询） | Planner 坏了时永不成立（40b02ce4），跑了没处理时又误消 |
| 侧条按卡折叠、标题取 worker 卡 `goal` | 1743 §5 S4 | **删除**：item 不再指向卡 | 两个来源都属于 Planner（K23 一卡） |
| items 的 `source/id/card_id` 词表 | 1722 §4.1（M6） | **替换**为 `source/key/text` | 需要原话与 Dismiss 身份 |
| 终态过滤（done/归档 ⇒ 无项） | 1743 §4.1 规则 1 | **items 上删除，`cards[]` 上保留**（D11） | 那条规则删的是失败项；ask 与 planner down 是指名给人的事，track 结局不替用户处理它们 |
| W/S 的 working 规则、E1–E8 unread、`cards[]` 逐卡结论、INV-APP-118「只从 overlay 推导」 | 1722 §4.2、1743 §4.3 | **保留**（`cards[]` 只去掉老化） | 状态层不变 |

## 2. 4140 事实（只读）

命令前缀（表名拆开写：#1316 术语闸门在 `docs/` 与 `crates/` 按字面计数该表名，两处都已在基线上，见 §8）：

```sh
DB=~/.local/share/neige-next/data/calm.db
T="harness""_items"          # 转录表（migrations/0031 建），下称 T
sqlite3 -readonly "$DB" <<SQL
…
SQL
```

| # | 事实（12:30） | SQL（`<<SQL` 体） |
|---|---|---|
| Q1 | 当前亮点（13:05）1 条：`40b02ce4` draft，`failed`，item `session`（12:30 时还有 `ded027ca` reviewing / `input` / `lifecycle`，其后它被推到 done） | `SELECT substr(o.entity_id,1,8), t.lifecycle, json_extract(o.payload,'$.attention'), json_extract(o.payload,'$.items[0].source'), json_extract(o.payload,'$.schemaVersion') FROM overlays o JOIN tracks t ON t.id=o.entity_id WHERE o.plugin_id='kernel' AND o.kind='activity' AND json_extract(o.payload,'$.attention')<>'none';` |
| Q2 | activity overlay 27 行，全是 `schemaVersion 1`，归档 track 上 0 行 | `SELECT json_extract(payload,'$.schemaVersion'), COUNT(*), SUM((SELECT archived_at FROM tracks t WHERE t.id=entity_id) IS NOT NULL) FROM overlays WHERE plugin_id='kernel' AND kind='activity' GROUP BY 1;` |
| Q3 | 按新规则会亮 planner down 的 track：只有 `40b02ce4`（T.id 22825，`created_at_ms` 1790348641211，message `unexpected status 403 Forbidden: …sub2api…/v1/responses`，216 字） | §4.1 N3 的同一判定：`WITH last_turn AS (SELECT h.track_id, h.id, h.created_at_ms, json_extract(h.params,'$.status') AS st, ROW_NUMBER() OVER (PARTITION BY h.track_id ORDER BY h.created_at_ms DESC, h.id DESC) AS rn FROM $T h JOIN cards c ON c.id=h.card_id WHERE c.role='planner' AND h.method='turn/completed' AND COALESCE(json_extract(h.params,'$.status'),'')<>'interrupted') SELECT substr(t.id,1,8), t.lifecycle, t.archived_at IS NOT NULL, lt.id, lt.created_at_ms FROM tracks t JOIN last_turn lt ON lt.track_id=t.id AND lt.rn=1 WHERE lt.st='failed';` |
| Q4 | 未归档 `blocked` track 0；**真实的 `calm.user.notify` 调用 0**；issue 的「6 次 / 4 track」来自 `LIKE` 文本匹配 | `SELECT (SELECT COUNT(*) FROM tracks WHERE lifecycle='blocked' AND archived_at IS NULL), (SELECT COUNT(*) FROM $T WHERE method='item/completed' AND item_type='mcpToolCall' AND json_extract(params,'$.item.tool')='calm.user.notify'), (SELECT COUNT(*) FROM $T WHERE method='item/completed' AND params LIKE '%calm.user.notify%');` → `0\|0\|6` |
| Q5 | 那 6 行 = `commandExecution` 5（输出里含该词）+ `userMessage` 1 | `SELECT item_type, COUNT(*) FROM $T WHERE method='item/completed' AND params LIKE '%calm.user.notify%' GROUP BY 1;` |
| Q6 | 历史 7 次 blocked：6 次是**用户回复**先到（分钟：1.7 / 117.2 / 2.0 / 421.0 / 27.9 / 420.4），Planner 随后离开 blocked（16.4 / 117.9 / 5.0 / 未离开 / 29.4 / 421.3）；1 次（28475，ratify）Planner 1.6 分钟后自行离开、用户 41.8 分钟后才回 | `SELECT e.id, substr(e.scope_track,1,8), round(((SELECT MIN(u.at) FROM events u WHERE u.kind='harness.user_message.enqueued' AND u.scope_track=e.scope_track AND json_extract(u.actor,'$.kind')='User' AND u.at>e.at)-e.at)/60000.0,1), round(((SELECT MIN(l.at) FROM events l WHERE l.kind='track.lifecycle_changed' AND l.scope_track=e.scope_track AND l.id>e.id)-e.at)/60000.0,1) FROM events e WHERE e.kind='track.lifecycle_changed' AND json_extract(e.payload,'$.to')='blocked' ORDER BY e.id;` |
| Q7 | 历史 Planner 失败轮 2 条：`32acbdf9`（usageLimitExceeded）7.0 分钟后有成功轮；`40b02ce4` 至今没有 | `SELECT h.id, substr(h.track_id,1,8), round(((SELECT MIN(n.created_at_ms) FROM $T n JOIN cards c2 ON c2.id=n.card_id WHERE c2.role='planner' AND n.track_id=h.track_id AND n.method='turn/completed' AND json_extract(n.params,'$.status')='completed' AND n.created_at_ms>h.created_at_ms)-h.created_at_ms)/60000.0,1) FROM $T h JOIN cards c ON c.id=h.card_id WHERE c.role='planner' AND h.method='turn/completed' AND json_extract(h.params,'$.status')='failed';` |
| Q8 | `failed` 的 Planner 会话 1 条（40b02ce4：phase `wedged`、reason `system_error`、`completed_at_ms` NULL），其中**没有** failed 转录行的 0 条 | `SELECT COUNT(*), SUM(NOT EXISTS (SELECT 1 FROM $T h WHERE h.worker_session_id=ws.id AND h.method='turn/completed' AND json_extract(h.params,'$.status')='failed')) FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id WHERE c.role='planner' AND ws.state='failed';` → `1\|0` |
| Q9 | 进 blocked 的边 7 条全是 `AiPlannerSession`，缺 `agent_message` 0 | `SELECT json_extract(actor,'$.kind'), COUNT(*), SUM(json_extract(payload,'$.agent_message') IS NULL) FROM events WHERE kind='track.lifecycle_changed' AND json_extract(payload,'$.to')='blocked' GROUP BY 1;` |
| Q10 | `harness.user_message.enqueued`（User，13:05）：codex Planner 103 / **Claude Planner 11** / codex assistant 10 | `SELECT json_extract(e.actor,'$.kind'), ws.provider, c.role, COUNT(*) FROM events e JOIN cards c ON c.id=e.scope_card JOIN worker_sessions ws ON ws.id=json_extract(e.payload,'$.worker_session_id') WHERE e.kind='harness.user_message.enqueued' GROUP BY 1,2,3;` |
| Q11 | Planner 会话：Claude 2 条，`last_turn_completed_ms` 非空 0、有成功转录行 2；codex 42 / 8 / 9 | `SELECT ws.provider, COUNT(*), SUM(ws.last_turn_completed_ms IS NOT NULL), SUM(EXISTS (SELECT 1 FROM $T h WHERE h.worker_session_id=ws.id AND h.method='turn/completed' AND json_extract(h.params,'$.status')='completed')) FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id WHERE c.role='planner' GROUP BY 1;` |
| Q12 | 进 `reviewing`：Kernel 55 + KernelDispatcher 55 + Planner 17 | `SELECT json_extract(actor,'$.kind'), COUNT(*) FROM events WHERE kind='track.lifecycle_changed' AND json_extract(payload,'$.to')='reviewing' GROUP BY 1;` |
| Q13 | failed 当前 attempt 20 条、failed worker 当前会话 8 条全在 `done` track；非 done 上只有 40b02ce4 的 Planner 会话 | `SELECT t.lifecycle,'task',COUNT(*) FROM current_tasks ct JOIN tracks t ON t.id=ct.track_id WHERE ct.status='failed' GROUP BY 1 UNION ALL SELECT t.lifecycle, c.role, COUNT(*) FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id JOIN tracks t ON t.id=ws.track_id WHERE ws.state='failed' GROUP BY 1,2;` |
| Q14 | 每个 track 的 Planner 卡数：1 张 × 27 | `SELECT n, COUNT(*) FROM (SELECT (SELECT COUNT(*) FROM cards c WHERE c.track_id=t.id AND c.role='planner') n FROM tracks t) GROUP BY n;` |

## 3. 两个来源的精确定义

### 3.1 ask（琥珀，`attention='input'`）

| 生产者 | 证据行（持久） | `key` | `text` | `at_ms` | 关闭（任一） |
|---|---|---|---|---|---|
| **A-blk**：Planner 推到 `blocked`（含 `calm.ratify.request`） | 本 track 最新的 `events(kind='track.lifecycle_changed', to='blocked')` 行 | `ask:lifecycle:<events.id>` | `payload.agent_message`（K10：必有） | `events.at` | `at_ms ≤ MAX(U, L)`；Dismiss |
| **A-ntf**：Planner 调 `calm.user.notify` 成功 | T 中 Planner 卡的 `item/completed`+`mcpToolCall`+`tool='calm.user.notify'`、`item.error IS NULL`、`item.status<>'failed'` 行（= 今天的 E2 谓词，`sql.rs:20-25`） | `ask:notify:<T.id>` | `$.item.arguments.text`，去首尾空白（同 `user_notify.rs:63-80`） | `T.created_at_ms` | `at_ms ≤ MAX(U, L)`；Dismiss（D4） |

**关闭规则（D-a，两种 ask 同一条）**：ask 开着 ⇔ `at_ms > MAX(U, L)` 且 key 未 Dismiss。U = 用户在 ask 之后给 Planner 发了消息；L = Planner（或用户）让 track 离开了 blocked：

```sql
-- L
SELECT MAX(at) FROM events
 WHERE scope_track = ?1 AND kind = 'track.lifecycle_changed'
   AND json_extract(payload, '$.from') = 'blocked'
```

对 A-blk，`at_ms > L` 恰等价于「track 仍在 blocked」：`blocked → blocked` 是空操作、不产生事件（`track_lifecycle.rs:42-44`），所以每一段 blocked 由一条 `to='blocked'` 边开始、由一条 `from='blocked'` 边结束，更新的 blocked 边就是更新的 ask。于是不再单独读 `tracks.lifecycle`。对 A-ntf，L 让「Planner 离开 blocked」也关掉在那之前发出的 notify。

**U**：

```sql
SELECT MAX(at) FROM events
 WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?planner_card
   AND json_extract(actor, '$.kind') = 'User'
```

- 两个入口（track 输入框、Planner 抽屉）都是 F7 的同一操作 → K13 的同一路由 → 同一事件 **[v]**；两个后端都写它（Q10：Claude 11 行）**[v]**。
- K13 的第二个写者不会误关 ask：track 创建时的首条消息落在**新** track 的 Planner 卡上，此前没有任何 ask 证据；会话创建写在新的 assistant 卡上，被 `scope_card = Planner 卡` 排除。
- `scope_card = Planner 卡`：发给 `/new` 出来的 assistant 会话（Q10：10 行）不关 ask；`actor.kind='User'`：AI 头路径（`AiPlannerSession` 审计 actor，`cards.rs:888-895`）不关 ask。
- 事件在入队**之后**写（`cards.rs:900,921`）⇒ 回复后点**立即**消，不等 Planner 那一轮。它通常早于该消息触发的那一轮，但**不保证**：`observe_user_message_durable` 回 ack 后 run loop 的下一个 tick 就可能发出 turn（`run_loop.rs:1184,1213`），先于路由写审计（`cards.rs:900-930`）；§11 G13。
- U、L 都为空 ⇒ 所有 ask 开着；同毫秒（`at_ms = MAX(U, L)`）按已关处理（§11 G5）。
- `ratify` 的 Grant 与 FE 的 Resume 都是 User 的 `blocked → working`（`cards.rs:961-1040`；`router/public.tsx:2427`）⇒ 经 L 关闭。`ratify` 的 Deny 不改 lifecycle（只发 `ratify.resolved`），ask 仍开着（§11 G14）。

### 3.2 planner down（红，`attention='failed'`）

| 生产者 | 证据行 | `key` | `text` | `at_ms` | 关闭（任一） |
|---|---|---|---|---|---|
| **P-turn**：Planner 本轮失败（codex failed 轮 / Claude 任何失败轮，K16–K17） | T 中 Planner 卡上**最新**一条 `method='turn/completed'` 且 `$.status<>'interrupted'` 的行，其 `$.status='failed'` | `planner_down:<T.id>` | `$.error.message` | `T.created_at_ms` | 之后出现一条 `completed` 的 `turn/completed`（它变成最新）；Dismiss；`/planner/reset` 删转录（§11 G9） |

- `interrupted` 既不亮也不关（用户按 Stop 不是 Planner 坏了）。
- **只用转录行，不用 `worker_sessions.state='failed'`**：Claude 失败轮从不让会话 `failed`（K17）；codex 的 `failed` 会话在 4140 都带 failed 转录行（Q8：1/0）。`failed` 会话而无 failed 轮的情形（`interrupt_timeout` 楔住、reaper 判死、启动操作失败）登记 §11 G1；没有任何行的发起拒绝（含 Claude 起进程前失败，K17）登记 §11 G2。
- codex systemError 的行在 phase 事件**之后**才写（K16），所以亮红靠 `harness.item.added{method: turn/completed}` 唤醒（§4.4）。
- `40b02ce4`：Planner 从未完成过一轮，最新行就是 22825（failed）⇒ 亮；1743 的 P 为 NULL 那条死路不再存在。

### 3.3 身份与重现

`key` 把证据行的主键带进去：T 与 `events` 的 id 都单调递增 ⇒ 同一来源之后再发生（新的 blocked 边、新的 notify、新的失败轮）必是新 key，即使旧 key 已 Dismiss 也会重新亮。一条 A-blk 与一条 A-ntf 可同时开着（两个生产者 = 两项；prompt 要求不要为同一个问题两处都说，§5）。

### 3.4 不再产生通知的东西

task `failed`、worker / 交互卡会话 `failed`、lifecycle `reviewing`、lifecycle `failed`、`done`：`items` 里不再出现。反过来，track 进入 done / 归档也**不**清掉开着的 ask 或 planner down（D11）；终态过滤只剩 `cards[]` 那一半：done / 归档 ⇒ `cards[]` 由 working 证据重建，卡级 `input`/`failed` 结论丢掉（`track_activity.rs:315-323`，第二句）。task 与会话失败仍进 `cards[]` 的 `failed`（Tasks 面板、卡头、CARDS 行照旧显示），lifecycle 仍是 badge，`failed`/`done` 边仍经 E4 亮 unread（`sql.rs:227-229`）。

## 4. 内核设计

### 4.1 新读的行（全部是自动提交单语句，同 `sql.rs:1-3` 的约束）

| 名 | SQL（形状） | 索引 |
|---|---|---|
| N0 | `SELECT id FROM cards WHERE track_id=?1 AND role='planner'` | 唯一索引（K23）；无 Planner 卡 ⇒ 无 A-ntf、无 planner down、U 为空 |
| N1 | `SELECT id, at, json_extract(payload,'$.agent_message') FROM events WHERE scope_track=?1 AND kind='track.lifecycle_changed' AND json_extract(payload,'$.to')='blocked' ORDER BY id DESC LIMIT 1` | `idx_events_scope_track` |
| N1b | L（§3.1） | `idx_events_scope_track` |
| N2 | U（§3.1） | `idx_events_kind_scope_card(kind, scope_card, id)` |
| N3 | **一条**语句，**取代** E2；全文见下 | 两臂都走 `idx_transcript_card_method_created_at(card_id, method, created_at_ms)`（下方计划） |
| N4（S3） | `SELECT item_key FROM activity_dismissals WHERE track_id=?1` | 主键前缀 |

N3（`?1` = Planner 卡）：

```sql
WITH h AS NOT MATERIALIZED (
  SELECT id, method, item_type, params, created_at_ms FROM <T> WHERE card_id = ?1)
SELECT 'notify', id, created_at_ms, json_extract(params,'$.item.arguments.text'), NULL FROM h
 WHERE method = 'item/completed' AND item_type = 'mcpToolCall'
   AND json_extract(params,'$.item.tool') = 'calm.user.notify'
   AND json_extract(params,'$.item.error') IS NULL
   AND COALESCE(json_extract(params,'$.item.status'),'') <> 'failed'
UNION ALL
SELECT * FROM (
  SELECT 'turn', id, created_at_ms, json_extract(params,'$.error.message'), json_extract(params,'$.status') FROM h
   WHERE method = 'turn/completed' AND COALESCE(json_extract(params,'$.status'),'') <> 'interrupted'
   ORDER BY created_at_ms DESC, id DESC LIMIT 1)
```

4140 上 `EXPLAIN QUERY PLAN`（`sqlite3 -readonly "$DB"`，SQLite 3.40.1；把 `<T>` 换成 `$T`、`?1` 原样留着；输出里的表名同样换回 `<T>`；v3 重跑）：

```text
QUERY PLAN
`--COMPOUND QUERY
   |--LEFT-MOST SUBQUERY
   |  `--SEARCH <T> USING INDEX idx_transcript_card_method_created_at (card_id=? AND method=?)
   `--UNION ALL
      |--CO-ROUTINE (subquery-3)
      |  `--SEARCH <T> USING INDEX idx_transcript_card_method_created_at (card_id=? AND method=?)
      `--SCAN (subquery-3)
```

对照：CTE 被引用两次时 SQLite 3.40 默认物化它——v1 的写法得到 `MATERIALIZE h` + 按 `card_id` 的旧索引整段扫 Planner 卡转录 + `USE TEMP B-TREE FOR ORDER BY`。起作用的是 `NOT MATERIALIZED`：方法过滤写在 CTE 里（v1 的 `OR` 形状）还是写在每一臂里，加上它之后计划相同，都是两臂 `SEARCH … idx_transcript_card_method_created_at (card_id=? AND method=?)`（两种都在 4140 上跑过）；不加它，过滤写在哪一边都会物化。改写后的 `e1_e2_query_plans_use_the_transcript_index` 断言两臂都是 `USING INDEX idx_transcript_card_method_created_at`，且**没有对转录表的 SCAN**；`SCAN (subquery-N)` 是 `LIMIT 1` 那一臂的协程扫描，允许——今天的断言「任何细节都不以 `SCAN` 开头」（`track_activity_projection.rs:1953-1956`）会误红，要改。以 sqlx 测试跑在内置 SQLite（`libsqlite3-sys 0.30.1`，`Cargo.lock:1937-1938`）上的计划为准，上面的 3.40.1 CLI 输出只是参考。

E2 = N3 返回的**全部** notify 行（不看 U/L）的 `MAX(at)`，与今天的 E2 一样没有截断（`sql.rs:20-25`）：notify 只对 Planner 可见（K12），按 Planner 卡取与今天按「track 的任意卡」取等价。U/L **只在 Rust 里建 items 时**用（`at_ms > MAX(U, L)`）。v2 曾把 `created_at_ms > MAX(U, L)` 下推进 SQL，那会让「notify 落地后、整算前用户就回复了」（唤醒还排在队里）的那一行永远不进高水位；§7 行 24 钉住。删除：`PLANNER_LAST_TURN_SQL`、`planner_last_turn`、`TrackRows.planner_last_turn`（`sql.rs:113-121`、`track_activity.rs:127-128,409`）。

### 4.2 fold 的改动

| 动作 | 位置 |
|---|---|
| 删老化闭包及两处调用 | `track_activity.rs:211-213,236,288` |
| task `failed` 不再 push item；`cards[worker]=failed` 与 E3 照旧 | `:234-251` |
| 会话 `failed` 不再 push item；`cards[card]=failed` 照旧（含 † 例外） | `:282-298` |
| 删 lifecycle 项 | `:301-313` |
| 新纯函数 `notifications(rows) -> Vec<Item>`，放新文件 `track_activity/notifications.rs`：A-blk、A-ntf（都按 `at_ms > MAX(U, L)`）、P-turn →（S3 起）去掉 `key ∈ N4` → 按 `at_ms` 倒序、`key` 升序。**不做终态过滤**（D11） | 新文件（`track_activity.rs` 保持 < 800 行） |
| 终态过滤只剩 `cards[]` 的重建：`:317-323` 去掉 `items.clear()`，保留「done / 归档 ⇒ `cards` 由 `working_cards` 重建」 | `:315-323` |
| `Fold::attention`：有 `planner_down` ⇒ `failed`；否则有 `ask` ⇒ `input`；否则 `none` | `:145-156` |

`ItemKind`、`ItemSource::{Card,Task,Session,Lifecycle}` 删除；`CardState` 不动（`input` 仍是无生产者的线上词汇，同 1743 §4.3）。

### 4.3 payload v2

```json
{ "schemaVersion": 2, "working": false, "attention": "failed",
  "activity_at_ms": 1790348641211,
  "items": [ { "source": "planner_down", "key": "planner_down:22825",
               "text": "unexpected status 403 Forbidden: …", "at_ms": 1790348641211 } ],
  "cards": [ { "card_id": "8a6cc362a05042d8af0ebae8c4a5bd21", "state": "failed" } ] }
```

- `items[]`：`source ∈ {ask, planner_down}`、`key`、`text`、`at_ms`，四个都必填、`deny_unknown_fields`。不带卡 id：动作目标恒为本 track 的 Planner 卡（K23），FE 已持有。
- `attention`：词表不变（`input` = 琥珀 = 有 ask，`failed` = 红 = planner down）。`cards[]`：形状不变；唯一变化是不再老化（failed attempt / 会话照实显示为状态）。
- **`schemaVersion` 升到 2**：item 的含义变了。读侧 `should_skip_overlay`（K22）不丢 v1 行（v1 ≤ max），`recompute_track` 在版本不同时必写（`:487-490`），高水位从原始 JSON 读（`:446-449`）⇒ 启动扫一次把 27 行改写成 v2，unread 不丢。「启动扫改写全部行」只因 4140 归档 track 为 0（Q2）：启动扫与 tick 只枚举未归档 track（`sql.rs:94-99`，`track_activity.rs:559-572`），归档 track 的行只在有事件唤醒它时才重算（§11 G15）。FE 解码改 `z.literal(2)`：旧 bundle 会把 v2 当「无 overlay」静默变安静，所以 **`WEB_COMPAT_VERSION` 31 → 32**（`routes/version.rs:27` 与 `fe/web/src/app/providers/public.tsx:16`），逼旧页刷新。
- 校验器 `validate_activity_overlay_payload`（`validation.rs:212-296`）按 v2 改写；`activity_overlay_payload_is_closed_to_the_design_shape`（`:882-960`）的夹具与拒绝用例随之改。
- `text` 是必填 `String`：A-blk 的 `agent_message` 由构造保证（K10，Q9：0 缺失）；A-ntf 的 `text` 由工具校验保证（成功调用必有）；P-turn 的 `error.message`：Claude 由构造保证（K17），codex failed 轮带 `error.message` 是 **[a]**（4140 2/2）。某一行解码到 NULL 时**只丢这一项**并记 warn（带 key），其余项与整个 overlay 照常写，不补默认文案（§11 G8）。E2 的 `MAX(at)` 不看 `text`，照算。

### 4.4 唤醒

| 变化 | 现状 | 改动 |
|---|---|---|
| 用户回复关 ask | `harness.user_message.enqueued` 不在唤醒表（K7） | `track_for_event` 加该事件 → payload 的 `track_id` |
| planner down 亮 / 关 | 普通完成：行先于 phase（K18），`harness.phase.changed` 已在表（`:578`）。**codex systemError：phase 先、行后**（K16），之后唯一的事件是 `harness.item.added{item_type: None, method: "turn/completed"}`，今天的臂只收 `mcpToolCall` + `item/completed`（`:580-587`）⇒ 红点要等 ≤30 s 的 tick；恢复路径补写的行同样只带这条事件（K16） | **放宽同一条臂**：`method == "item/completed" && item_type == Some("mcpToolCall")` **或** `method == "turn/completed"`。仍是一条臂（该臂里 `harness.item.added` 的 Rust 名只出现一次，§8 的术语计数不变） |
| notify 亮 | `item/completed`+`mcpToolCall` 已在表（`:580-587`） | 不改 |
| blocked 进出 | `track.lifecycle_changed` 已在表（`:596`） | 不改 |
| Dismiss | — | 进程内唤醒通道（§4.5） |

`wakeup_table_resolves_every_row_of_the_design`（`track_activity_projection.rs:2355`）加两行（用户发送；`turn/completed` 的条目事件）。

### 4.5 Dismiss

**存储**（迁移号放最后：当前 main 最大 `0118_worktree_reclaim_indexes.sql`，合并时重查；`tests/cases/head_schema_fixture.rs:58` 的文件名清单同步加一行）：

```sql
-- 0119_activity_dismissals.sql
CREATE TABLE activity_dismissals (
    track_id        TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    item_key        TEXT    NOT NULL,
    dismissed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (track_id, item_key)
) WITHOUT ROWID;
```

track 删除随 FK 级联（同 `0116_task_replacements.sql:12` 的写法）。不存 `source`/`at_ms`：身份全在 key 里（§3.3）。

**路由**：`POST /api/tracks/{id}/activity/dismissals`，body `{"key": "<item key>"}`，`204`。

| 检查 | 结果 |
|---|---|
| `actor.as_str() != "user"` | `403`（同 `ratify_card`，`cards.rs:967`） |
| track 不存在 | `404` |
| `key` 不匹配 `^(ask:(lifecycle\|notify)\|planner_down):[0-9]+$` | `400` |
| 其余 | `write_in_tx_typed` 一个 IMMEDIATE：`INSERT … ON CONFLICT(track_id, item_key) DO NOTHING`（幂等，保留首次时间），不发事件；提交后 `activity_wake.send(track_id)` |

不校验 key 此刻是否开着：一个已关的 key 写进去无害（投影器只拿它过滤本 track 的项），省掉路由对投影 payload 的依赖。新文件 `routes/activity_dismissals.rs`（`routes/tracks.rs` 已过 4000 行）。

**唤醒**：`track_activity::spawn`（`track_activity.rs:673-685`）改为返回 `ActivityWake(mpsc::UnboundedSender<String>)`，装进 `RouteState`（`state.rs:1206` 的 spawn 点）；`run` 的 `select!` 加第四臂，与 PTY 边沿臂同形（`:653-664`）。投影器没起（非 sqlite）时发送失败被忽略。不新增事件 kind：`fe/core/events` 是冻结面，且 sync-event 版本不必动。

**投影器读**：N4，§4.2 的过滤一步（都在 S3）。

**OpenAPI / 生成客户端**：`openapi.rs` 注册路径与 `DismissActivityItemRequest{key}`。该请求体定义在 calm-server（路由文件旁），只派生 `ToSchema`、**不派生 `TS`**：`gen:api` 的 TS 导出只跑 calm-types 的 `export_bindings_`（`fe/package.json:14`），所以 `fe/core/api/generated/wire.ts` 不变，只有 `openapi.json` 变。`(cd fe && npm run gen:api)` 重生成它——`fe/core/api` 在 `fe/module-file-inventory.yaml` 里是 readonly ⇒ 该提交只需一条 trailer：`OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — dismiss route (#1829)`。`REST_API_VERSION` "12" → "13"（`calm-types/src/compatibility.rs:5`；先例 #1817：新路由、旧内核上 404）。FE 的操作定义放 `fe/core/domain/activity.ts`（同 `conversation.ts:437` 的形状），`fe/tools/architecture/openapi-contract.test.ts` 随之覆盖。

### 4.6 planner down 的动作：Reply（没有 Restart）

owner 已定（2026-09-28）：**没有 Restart 动作**。两种项共用一个主动作 `Reply`，打开 Planner 输入框；发消息就是让 Planner 继续。下表是「发消息之后会怎样」的代码证据。

| 后端 / 状态 | 发一条消息会怎样 | 证据 |
|---|---|---|
| codex，`failed` + `wedged/system_error` + `completed_at_ms` NULL（= 40b02ce4，Q8） | `/planner/input` 的人类发送经 `candidate` → `recover` → `resume_system_error_conversation`，然后入队；抽屉已显示 `RECOVERY_NOTICE`「…send a message to resume」 | K20；`routes/cards.rs:862,1199,1248-1263` |
| Claude，最新轮 failed（起进程之后的失败） | 会话仍 `idle`，`can_issue_turn` 为真，消息起新一轮（新进程） | K17；`harness/state.rs:52-54` |
| Claude，起进程之前失败 | 无行、无红点（G2）；消息进队，等下次发起再试 | K17、K24 |
| codex，`failed` 但不可恢复（`interrupt_timeout`、reaper 判死、`completed_at_ms` 已填） | `409 planner_harness_dormant`；唯一出路 `/planner/reset`（硬删转录，FE 无路径，F8） | §11 G1；4140 0 条（Q8） |

**决定**：不新增任何重启路由。planner down 行 = 失败原因（`text`）+ 一句固定文案 `Fix the cause, then send the Planner a message to continue.` + `Reply`。发送失败则没有入队事件、红点不变；发出后 Planner 再失败 = 新的 failed 轮 = 新 key 重新亮。

### 4.7 事务形状与文件体量

新 SELECT 全部自动提交单语句（`deferred_write_tx_invariant`）；投影器写不变（`write_with_events_typed`）；Dismiss 一个 IMMEDIATE、无事件。`track_activity.rs` 685 行，净变化约 −60（删）+ 20（唤醒臂）；`notifications.rs` 与路由文件都是新文件、< 200 行。

## 5. Planner prompt

| 文件 | 改动（最小） |
|---|---|
| `crates/calm-server/prompts/planner.md:12` | `* working → blocked         when you need the user; your \`message\` is shown to them verbatim as the question, so write the question itself. If you still need them after they reply, ask again with \`calm.user.notify\` (blocked → blocked writes nothing)` |
| `crates/calm-server/prompts/tools/calm.user.notify.md` | ① 把 `Do not use it in turns opened by a user message or a task event; reply normally there.` 改为 `Do not use it in turns opened by a user message or a task event; reply normally there — except to ask the user again when the track is still \`blocked\` after their reply.`（与 `planner.md:12` 的再问规则一致；原句与之矛盾）② 第一句后加：`The call is an ask: it stays on the user's notifications until they reply to you, so \`text\` must be the question or decision itself. If you are also moving the track to \`blocked\` for the same question, do not call this tool; the blocked message is the ask.`（不提 Dismiss：S2 可先于 S3 合并，prompt 不承诺还不存在的东西；S3 也不回头改这句——Dismiss 是用户的事，Planner 不必知道） |
| `crates/calm-server/prompts/planner.md:129` | 同一处矛盾的中文版：`其它 turn（用户消息、任务事件）不受影响，照常回复，不要用这个工具。` 改为 `其它 turn（用户消息、任务事件）不受影响，照常回复，不要用这个工具——唯一例外：用户回复后 track 仍在 \`blocked\`、你还需要他时，用它再问一次。` |
| `crates/calm-server/prompts/planner.md:133` | 渠道表第一行「用户消息 / 任务事件 → 普通回复」两格末尾各加 `（blocked 中再问：见上）`，让表与 `:129` 的例外同处可见 |
| `crates/calm-server/prompts/tools/calm.ratify.request.md` | 加一句：`\`reason\` is shown to the user verbatim as the question; write it for a person.`（4140 的 ratify reason 形如 `merge_hold: pr #1811 converged at …`，K10） |

- 被钉住的 golden（S2 两个都要重生成）：`tests/goldens/issue_development_planner_prompt.txt`（整份 `planner.md` 渲染；`REGEN_PLANNER_PROMPT_GOLDEN=1`，`plugin_host/manifest.rs:1558,1913`）；`tests/goldens/mcp_tool_registry.json`——它只存每个工具描述的 `description_sha256`（`calm.user.notify` 在 `:2319-2320`，`calm.ratify.request` 同形），两个工具描述一改哈希就变（`REGEN_MCP_TOOL_REGISTRY_GOLDEN=1`，`mcp_server/tools/mod.rs:66,124`）。planner prompt golden 逐行人读 diff；registry golden 只能核对「只有这两行哈希变了」。
- 不改：`planner.md:20` 的 lifecycle 段落（「Lifecycle transitions are available…」，`neige state` / `next` 在该段末）、`:129-134` quiet-sync 段中 `:129` 那一句以外的部分、`run_loop.rs:2806-2810` 的通道句（它只追加在「全是报告编辑」的后台批次末尾，`:2803-2805`，说的是后台轮，与用户消息轮里的再问不冲突）。**#1828 正在改的正是 `:20` 这一段**（把合法目标移进拒绝信息）：本文只动 `:12` 这一行分支表，与 `:20` 隔着分支表其余 6 行与一个空行，文本不重叠；若 #1828 也改分支表则按 #1828 合并后的文本 rebase。

## 6. 前端

层：`core/domain`（`activity.ts`、`track.ts`）、`app/router`（`public.tsx`）、`app/providers`（`queries.ts`）、`features/track/page`（`public.tsx` + 同目录 CSS module）。不动：`app/shell/sidebar.tsx`、`features/today`、`ui/activity-indicator`、`styles/`（readonly）、`core/events`（readonly）、`core/api`（只有生成物，见 §4.5）。

| 面 | 之前 | 之后 |
|---|---|---|
| 解码（`track.ts:144-191`） | v1、item `kind/source/id/card_id` | `z.literal(2)`；item `{source, key, text, at_ms}` → `ActivityItem{source, key, text, atMs}` |
| 侧条行 | 标签（卡 goal / Planner / Task key）+ 8 句固定文案之一 + `Review` | ask：标签 `Planner asks`（琥珀）+ **Planner 原话**；planner down：标签 `Planner stopped`（红）+ **失败原因** + 固定一句 `Fix the cause, then send the Planner a message to continue.`。两者都有 `relativeTime(atMs)`（F9）、`Reply`、`Dismiss`（S3）。正文 CSS 截 3 行，全文在 Planner 对话里 |
| 主动作 | `Review`：按卡路由（F5） | 两种项同一个 `Reply` = `registry.requestOpen(plannerCard.id, {focusComposer: true})`（§4.6） |
| Dismiss | 无 | `POST /api/tracks/{id}/activity/dismissals {key}`；成功后等 overlay.set 重取（不做乐观删除）；`404`（track 已删）按完成处理 |
| 侧条头 | `N items need attention` | `N waiting on you`（与 rail strip 同词） |
| 行状态属性 | `data-nc-notification-state = awaiting-input \| errored` | `= ask \| planner-down` |
| 可访问名 | `Review <source> notification: <message>` | `Reply to the Planner: <text 前 80 字>`（两种项同形）/ `Dismiss: <标签>: <text 前 80 字>`（带正文，理由同今天 `page/public.tsx:560` 的注释：同名按钮对读屏者是一个按钮） |
| rail / strip / Today | `needsUserAttention ∨ hasFailed` | **代码不改**；含义随 overlay：琥珀 = 有 ask，红 = Planner 停了，strip 与 Today 第一个数 = 有通知的 track 数；侧条启动器的数 = 该 track 的项数（`page/public.tsx:569-581`） |

**删除**：`foldAttentionByCard`、`attentionKindOf`、`ActivityOrigin`（`activity.ts:13-14,60-90`）；`cardGoalTitle`、`CARD_GOAL_TITLE_MAX` 及其测试（`track.ts:255-272`）；`notificationCardLabel`、`attentionNotifications` 的 8 句文案与 `Task …`/`Track`/`Card` 标签（`router/public.tsx:1867-1905`）；`conversationNotificationCardIds` 与按卡路由（`:2103-2109,2409-2425`）；`TrackInputNotification.origin/id/cardId/state`（`page/public.tsx:43-57`）；`Review` 按钮（`:556-565`）。#1741 B「aside button names」随之失效。

**oracle / 变异**：`INV-APP-118`（`docs/oracle/app-dataflow.yaml:635-661`）的 statement：S1 把「侧条按卡折叠」一句换成「侧条每项一行，显示内核给的原话；唯一的动作 Reply 打开 Planner 输入框」，`source`/`authoritative_test` 行号随改；S3 再加「与 Dismiss（`POST …/activity/dismissals`）」一句及其测试。其余指向被改文件的锚点也在 S1 重指：已知 `capabilities-e2e.yaml:116`（`router/public.tsx:405-418,…`）、`:332`、`:354`（`features/track/page/public.tsx:238-240`）、`:394`（`router/public.tsx:1397,1988-1994`）、`pages-shared.yaml:61`（`router/public.tsx:2264,2373,2469`）、`app-dataflow.yaml:652-653`；**全集以跑 oracle 校验为准**，不靠 grep：`(cd fe && npm exec -- vitest run tools/oracle/oracle.test.ts --project platform-independent)`（`fe/tools/oracle/README.md:16`），红的每一行按该 README 重指。两本账：`anchor-pending.json` 只能删行（`README.md:7`；待定 ID 集由 `validator.ts:433` 冻结）。`anchor-baseline.json` 今天是 `[]`；README 规定不得新增或改动 baseline 行（`README.md:5`），**本设计遵守这条 README 规则**：锚点移位一律改 YAML 重指，从不增改 baseline 行。validator 本身只保证 baseline 行数不超过上限 171、且与实际债务逐条相等（`validator.ts:385,487-489,565-581`），并不单独拦「改一行」。

`fe/tools/mutation/manifest.json`（全部在 S1，除 F3）：

| 条目 | 处置 |
|---|---|
| `s4-aside-not-folded-per-card`（`:1733`） | 删（`foldAttentionByCard` 删除） |
| `s4-aside-title-falls-back-to-key`（`:1772`） | 删（标题逻辑删除） |
| `s2b-notifications-from-card-status`（`:1655`，目标 `router/public.tsx`，patch 让侧条改读 `kernel/card/status` 行） | **重写**：patch 改到新的行映射（读退役的 `status` 行而不读 `activity.items`），继续守 INV-APP-118 的「不读退役卡级 overlay」；`expected_red` 重算为新的全集——预言：`track conversations ignores a retired kernel/card/status row and a plugin-authored activity row`、`degraded workspace reads stay usable prefers track-detail overlays to the neutral workspace fallback`、F2（ask / planner down 行显示原话且 Reply 打开输入框）、一个 ask + 一个 planner down 同时在时的计数用例；旧名单里的折叠、goal 标题、按卡路由、assistant 路由四类用例随功能删除。以 `npm run test:mutation:plan` 与 `test:mutation:run` 的实际红集为准，`why_more_than_one` 同步改写 |
| `s2a-activity-overlay-plugin-gate-dropped`（`:1263`，目标 `track.ts`） | 上下文行随解码改动移动 ⇒ `git apply --check` 不过就重新生成（1743 §5 的同一处置） |
| 新增 `n1829-decoder-accepts-v1`、`n1829-row-fixed-copy`（S1）；`n1829-dismiss-not-posted`（S3） | 见 §7 F 行 |

## 7. 生产者 × 状态矩阵（每行一个必红测试）

内核测试在新文件 `tests/cases/track_notifications.rs`；它复用的 `Fx`（含转录 helper `transcript_item` / `pin_transcript_row` / `item_added`，`track_activity_projection.rs:643-690`）先在 S0 原样挪进共享模块——挪动不增减 §8 的术语计数。

夹具来源固定（变异的红集按此预言）：行 1、2、3、4a、4b、19、20 只有 A-blk；行 5–8 只有 A-ntf，其中**只有行 5** 带一段已结束的 blocked（一条 `to='blocked'` 与一条 `from='blocked'` 边），行 6–8 没有任何 `to='blocked'` 边，且各自是独立夹具（行 7、8 **不**继承行 5 的夹具：各是一条 `working` 下成功的 notify、无 blocked 历史）——所以「去掉 L」的红集恰是 {3, 3b, 5}；行 3b 是 blocked 期间的 notify（A-blk + A-ntf 同在）；行 9–13、23 是 P-turn（23 另带一条 A-ntf）；行 17b、18 见行内。**带 `from='blocked'`（离开 blocked）边的夹具只有 3、3b、4a、4b、5**；行 2、19、18 的 track 进入 blocked 后**从不离开**——2、19 只由 U 关闭，18 只由 Dismiss 关闭；实现者不得给它们补离开边，否则「去掉 U → {2, 19, 24}」会被 L 掩盖而失效。

| # | 生产者 / 事件 | 之后的项 | 关闭者 | 必红测试 | 单因素变异 |
|---|---|---|---|---|---|
| 1 | Planner `working→blocked` + message | 1 × ask（A-blk） | U / L / Dismiss | `blocked_with_message_is_an_ask_with_its_words` | 不产 A-blk |
| 2 | 1 之后 User 发给 Planner | 0 | — | `user_send_after_block_closes_the_ask` | 比较里去掉 U |
| 3 | 1 之后 Planner `blocked→working` | 0 | — | `leaving_blocked_closes_the_ask` | 比较里去掉 L |
| 3b | notify 之后 Planner 离开 blocked | 0 | — | `leaving_blocked_closes_an_earlier_notify_ask` | A-ntf 只比 U |
| 4a | 1 之后 Planner `blocked→working→blocked`（第二条 blocked 边）（S1） | 1 × ask，key ≠ 第一次 | 同 1 | `second_block_edge_gives_a_new_key` | key 不含证据 id |
| 4b | 1 被 Dismiss 后再 block 一次（S3） | 1 × ask（新 key，重新亮） | 同 1 | `second_block_after_dismiss_relights` | key 不含证据 id |
| 5 | `calm.user.notify` 成功，track 在 `working`，此前有一段已结束的 blocked | 1 × ask（A-ntf） | U / L / Dismiss | `notify_after_a_closed_block_is_an_ask` | A-ntf 要求当前 lifecycle 为 blocked |
| 6 | notify 调用失败（`item.error` 或 `status='failed'`） | 0 | — | `failed_notify_call_is_no_ask` | 去掉错误谓词 |
| 7 | 一条 A-ntf（无 blocked 段）之后，User 发给 assistant 卡（含会话创建的首条消息，K13 第二写者） | 1（不变） | — | `assistant_send_does_not_close_the_ask` | U 不按 Planner 卡 |
| 8 | 一条 A-ntf（无 blocked 段）之后，AI 头路径发送（`AiPlannerSession`） | 1（不变） | — | `ai_actor_send_does_not_close_the_ask` | U 不看 actor |
| 9 | Planner 最新轮 `failed`（codex 夹具 + Claude 形状夹具各一） | 1 × planner_down | 之后 `completed` 轮 / Dismiss | `failed_planner_turn_is_planner_down` | 不产 P-turn |
| 9b | codex systemError 的真实顺序：先 `harness.phase.changed`（wedged），再写 failed 行并发 `harness.item.added{item_type: None, method: turn/completed}`；`run()` 循环、先吃掉启动 tick、之后不调 `reconcile_all` | 5 s 内出现 planner_down | — | `system_error_row_after_the_wedged_phase_wakes_the_projector` | 唤醒臂不收 `turn/completed` |
| 10 | 9 之后一条 `completed` 轮 | 0 | — | `later_completed_turn_closes_planner_down` | 取「任一 failed」而非「最新」 |
| 11 | 9 之后一条 `interrupted` 轮 | 1（不变） | — | `interrupted_turn_neither_raises_nor_closes` | 不排除 `interrupted` |
| 12 | 从未完成一轮、唯一一轮 failed（40b02ce4 形状） | 1 × planner_down | 同 9 | `planner_that_never_completed_is_down` | —（由 9 的变异同时钉红） |
| 13 | assistant 卡的失败轮 | 0 | — | `assistant_failed_turn_is_not_planner_down` | 不按 Planner 卡 |
| 14 | 当前 attempt `failed` | 0；`cards[worker]=failed` | 状态 | `failed_task_is_status_not_notification` | 恢复 task 项 |
| 15 | worker / 交互卡会话 `failed` | 0；`cards[card]=failed` | 状态 | `failed_session_is_status_not_notification` | 恢复 session 项 |
| 16 | lifecycle `reviewing`（任一 actor） | 0 | 状态 | `reviewing_is_status_only` | 恢复 lifecycle 项 |
| 17 | lifecycle `failed` / `done` | 0；E4 亮 unread | 结局 | `failed_lifecycle_is_outcome_only` | 恢复 lifecycle 项 |
| 17b | track 经 `track_update_tx` 推到 done（不经 blocked）后，落一条 notify 与一条 failed 轮；同样的夹具再做一份归档的。测试**直接调 `recompute_track`**——它钉的是 fold 不再清 items，不是启动扫 / tick 会不会枚举归档 track（那是 G15） | A-ntf + planner_down 仍在；`cards[]` 只剩 working | U / L / 之后 completed 轮 | `done_track_keeps_open_notifications`、`archived_track_keeps_open_notifications` | 终态过滤里恢复 `items.clear()` |
| 18 | 一条 A-blk + 一条 planner_down，Dismiss A-blk 的 key（S3） | 只剩 planner_down | — | `dismiss_hides_only_that_key` | 去掉 N4 过滤 |
| 19 | 用户发送经 bus 唤醒投影器（`run()` 循环，先吃掉启动 tick） | ask 秒级消失，不等 tick | — | `user_send_wakes_the_projector` | 删唤醒表新行 |
| 20 | Dismiss 经唤醒通道 | 项秒级消失 | — | `dismissal_wakes_the_projector` | 路由不 `send` |
| 21 | 路由：AI actor / 坏 key | `403` / `400`，无行 | — | `dismiss_route_is_user_only`、`dismiss_route_rejects_a_bad_key` | 去掉对应检查 |
| 22 | v1 payload 的高水位 | 改写成 v2，`activity_at_ms` 不降 | — | 既有 `high_water_mark_survives_an_unparseable_stored_payload` 改为 v1→v2 | — |
| 23 | 一条 failed 轮缺 `error.message`，另有一条开着的 ask | 只丢 planner_down，ask 与 overlay 照写 | — | `missing_text_drops_only_that_item` | 解码错误向上传播 |
| 24 | 唤醒排队时的回复：落一条 notify 行，**在任何整算之前**再落一条 User 的 `harness.user_message.enqueued`，然后整算一次 | `items=[]`，但 `activity_at_ms ≥` 该 notify 的 `at_ms`（E2 未截断） | — | `closed_notify_still_advances_activity` | E2 也按 `MAX(U, L)` 截断 |
| F1 | FE：v1 overlay | 视为无 overlay | — | `track.test.ts`：v1 activity overlay is ignored | `n1829-decoder-accepts-v1`（`z.literal(2)` → `z.union([z.literal(1), z.literal(2)])`，单因素：v2 照常解码，只有 v1 用例变红） |
| F2 | FE：一个 ask + 一个 planner down | 两行：ask 显示原话；planner down 显示原因 + 固定一句；**两行的 `Reply` 都打开 Planner 输入框**（一个用例覆盖两种项） | — | `track-conversation.test.tsx`：both notification kinds show the kernel's words and Reply opens the Planner composer | `n1829-row-fixed-copy`（行正文换回固定文案） |
| F3 | FE：Dismiss（S3） | POST 该 key | — | `track-conversation.test.tsx`：Dismiss posts the item key | `n1829-dismiss-not-posted` |

变异验证（AGENTS.md「Mutation verification」）：改生产代码一处，在选定套件（内核：`track_notifications` + `track_activity_projection` + `terminal_signals`；FE：`manifest.json` 的 `selection_paths`）里比对**完整**红集与下表预言，多一个少一个都作废；复原后确认绿与无残留。

| 变异 | 预言的完整红集 | 为什么是这些 |
|---|---|---|
| 比较里去掉 U | 2、19、24 | 三者都断言「User 发送之后 ask 没了」；7、8 断言「不变」，照绿 |
| 比较里去掉 L | 3、3b、5 | 带 `from='blocked'` 边的只有 3、3b、4a、4b、5：3、3b 只靠 L 关闭 → 红；5 的旧 blocked 段没有 U 覆盖，去掉 L 后旧 A-blk 复活 → 红；4a、4b 的 A-blk 只取最新一条 blocked 边，它晚于 L、本就开着 → 绿。2、19 仍在 blocked、由 U 关闭 → 绿；18 仍在 blocked、项本就开着 → 绿；7、8、17b、23、24 无 blocked 历史 → 绿 |
| A-ntf 只比 U | 3b | 5 的 notify 晚于 L，只比 U 也开着 |
| key 不含证据 id | S1：4a；S3 起：4a、4b | 18 的两项 source 不同，key 仍不同 |
| U 不按 Planner 卡 | 7 | 2、19 的发送本来就在 Planner 卡上 |
| U 不看 actor | 8 | 同上 |
| 唤醒臂不收 `turn/completed` | 9b、`wakeup_table_resolves_every_row_of_the_design` | 唤醒表测试逐行断言 |
| 取「任一 failed」而非「最新」 | 10 | 11（failed 后 interrupted）、12 最新行本就是 failed |
| 终态过滤恢复 `items.clear()` | 17b 两条 | 既有 done / 归档测试只断言任务失败不成项，本来就无项 |
| 去掉 N4 过滤（S3） | 18、4b、20 | 三者都断言 Dismiss 之后项没了 |
| 删用户发送的唤醒行 | 19、`wakeup_table_resolves_every_row_of_the_design` | 2 直接调 `recompute_track`，不经唤醒 |
| E2 按 `MAX(U, L)` 截断 | 24 | 其余 E2 用例没有用户发送 |
| `n1829-decoder-accepts-v1` | F1 | v2 夹具照常解码 |
| `n1829-row-fixed-copy` | F2 | — |
| `n1829-dismiss-not-posted`（S3） | F3 | — |

**既有测试与调用点的清扫**（S1）：

| 文件 | 处置 |
|---|---|
| `track_activity_projection.rs` | 删老化六条（`:1564-1782`）。只留 `cards[]` / `working` / 高水位断言、去掉 item 断言：`sub_track_child_failed_marks_parent_failed`（`:853`）、`interactive_card_failed_session_is_failed`（`:948`）、`wedged_harness_is_failed`（`:1074`）、`done_task_replacement_session_failure_is_failed`（`:1119`）、`superseded_failed_attempt_session_is_not_actionable`（`:1173`）、`done_track_failed_attempt_is_quiet`（`:1340`）、`archived_track_failed_attempt_is_quiet`（`:1371`）、`reopened_track_failed_attempt_is_red_again`（`:1399`，改断言「重开后 `cards[worker]=failed` 回来」）、`done_track_running_task_is_still_working`（`:1432`）、`done_track_failed_session_on_a_running_worker_is_still_working`（`:1517`）、`kernel_lifecycle_edge_advances_activity`（`:2008`，删 reviewing 项三行，留 E4）。`e2_user_notify_completed_row_is_the_activity_instant`（`:1871`）夹具补合法的 `arguments.text`，unread 断言不动。`e1_e2_query_plans_use_the_transcript_index`（`:1934`）改测 N3 两臂。`activity_payload_passes_the_overlay_registry`（`:2271`）、`high_water_mark_…`（`:2144`）、`wakeup_table_…`（`:2355`）改到 v2 |
| `tests/cases/terminal_signals.rs` | `:9` 的 `use` 去掉 `ItemKind, ItemSource`；`signal_killed_ephemeral_card_is_failed`（`:3366-3382`）的 `attention` 期望由 `Failed` 改为 `None`、删 `:3377-3381` 五行 item 断言，留 `cards[card]=failed` |
| `calm-truth/src/validation.rs` | 夹具与拒绝用例（`:859-960`）改到 v2 |
| `tests/domain_api_suite.rs` | S0 注册共享 `Fx` 模块、S1 注册 `cases/track_notifications.rs`（同 `:69-70` 的 `#[path] mod` 形状）；不注册则 `nextest -p calm-server track_notifications` 选中 0 个测试 |
| `tests/cases/mobile_pairing/browser_contract.rs:107-108` | 该用例 `#[ignore]`，但断言写死版本号：S1 把 `webCompatVersion` 31 → 32，S3 把 `apiVersion` "12" → "13"（先例 #1810、#1817） |
| `tests/cases/openapi.rs`（S3） | `document_contains_every_annotated_path` 的路径清单（`:10-40`）加新路由；`document_contains_every_wire_model`（`:88`）加 `DismissActivityItemRequest` |
| FE | `activity.test.ts`、`track.test.ts`（含 `cardGoalTitle` 用例）、`track-conversation.test.tsx`、`features/track/page/public.test.tsx`、`header-lifecycle.browser.test.tsx`；带 v1 activity 夹具的 `today-activity`、`today-conversation`、`planner-conversation`、`track-cards-panel`、`read-fallbacks.contract`（router）、`queries.contract`（providers）、`row/public.test.tsx`、`activity-indicator.browser.test.tsx`、`terminal-head-activity.test.tsx` 改 `schemaVersion: 2` |

收尾扫描（期望只剩本设计有意保留的名字）：`grep -rn 'ItemSource\|ItemKind\|attentionItems\|foldAttentionByCard\|cardGoalTitle\|notificationCardLabel\|ActivityOrigin\|attentionKindOf\|inputNotifications\|onOpenInputNotification' crates fe/core fe/web/src docs/oracle fe/tools/mutation`；`origin:` 另按 `grep -rn "origin: '\(card\|task\|session\|lifecycle\)'" fe` 扫（v1 时 5 个测试文件命中）。

## 8. 门禁

| 门禁 | 位置 | 本设计 |
|---|---|---|
| #1316 术语闸门 | `scripts/gate-1316-terminology-ratchet.sh`；闸门第 5 项（转录表名那一词）的基线：crates **262**、docs 2，都已在上限 | **约束**：`crates/` 净变 0——N3 取代 E2（各含一次表名），唤醒只放宽既有一条臂，测试只经挪动后的 `Fx` helper 触达转录表；本文不含该词（§2 用拆开的 `$T`）。**两个方向都红**（脚本头 `:3-4`）：任何一项计数下降，同一 PR 里跑 `--update-baseline` 并提交 tsv |
| prose ratchet | `scripts/gate-prose-ratchet.sh` | 新 Rust 文件无 ≥4 字 CJK、无 ≥120 字节无引号无反斜杠字面量（SQL 用 `\` 续行）。同样**两个方向都红**（脚本头）：删掉旧代码使 `cjk` / `long_literal` 下降时同 PR `--update-baseline` |
| 本地入口 | `scripts/local-ratchet-gates.sh`（本工作树 `AGENTS.md`：每个改动都跑，含纯文档；新文件先 `git add -N`） | 每片都跑 |
| deferred-tx | `tests/cases/deferred_write_tx_invariant.rs` | 约束：新读全是自动提交单语句；Dismiss 走 IMMEDIATE |
| web-compat lockstep | `scripts/gate-web-compat-version-lockstep.sh` | **bump** 31 → 32，两处同改（S1） |
| sync-event lockstep | `scripts/gate-sync-event-version-lockstep.sh` | 不动：无新事件 kind，迁移不打 `event_version` |
| OpenAPI 漂移 | `scripts/local-rust-gates.sh --quick` 第 5 步 | S3 重生成 `openapi.json`；readonly ⇒ `OWNERSHIP-CHANGE` trailer（`fe/tools/ownership/README.md`） |
| 迁移清单 | `tests/cases/head_schema_fixture.rs:58` | S3 加 `0119_…` 一行；号码合并时重查 |
| overlay 注册表测试 | `validation.rs:859-960`、`track_activity_projection.rs:2271` | 改到 v2 形状（S1） |
| prompt golden | `tests/goldens/issue_development_planner_prompt.txt`、`tests/goldens/mcp_tool_registry.json` | S2 重生成并人读 |
| oracle | `docs/oracle/app-dataflow.yaml:635-661`（INV-APP-118）及 §6 列出的全部行号锚点 | **S1** 改 statement 与锚点，以 oracle 校验为准 |
| FE 变异清单 | `fe/tools/mutation/manifest.json:1263,1655,1733,1772` | **S1**：删二、重写 `s2b-notifications-from-card-status`、可能重生成一、加二；S3 加一 |
| FE 架构 | `fe/eslint.config.js:60`（`no-module-runtime-state`）、dependency-cruiser | 新函数无模块态；`features/track/page` 只从同域 `row` 取 `relativeTime` |
| 不涉及 | `boot_invariants.rs`、`harness_turn_start_invariant.rs`、`fork_guard_exemption_invariant.rs`、`scripts/ci/ratchets/*`（append-seam、report-write、runtimes-retirement） | 不加启动步骤、不发 turn、不 fork、不碰报告写边界 **[a]**：ratchets 目录只按脚本名判断 |

## 9. 4140 前后（13:05 重算；D11 之后仍不变）

升级后第一次启动扫（`reconcile_all`，≤30 s）：

| track | lifecycle | 之前（v1，Q1） | 之后（v2） |
|---|---|---|---|
| `40b02ce4`（未命名草稿） | draft | `attention=failed`；item `{failed, session, 4ed16faa…}`；`cards` Planner = failed | `attention=failed`；item `{planner_down, key planner_down:22825, text "unexpected status 403 Forbidden: …", at_ms 1790348641211}`；`cards` 不变。用户 Reply（发消息 → K20 的恢复；sub2api 仍 403 时再得一条 failed 轮 = 新 key 重新亮）或 Dismiss |
| `ded027ca`（侧栏头像固定与配色） | done（12:30 时 reviewing） | `none` | `none` |
| 其余 25 条 | done 19 / draft 4 / planning 2 | `none` | `none`；行因版本不同被改写一次（一个 `overlay.set`） |
| 合计 | — | 亮点 1（红 1）；v1 行 27 | 亮点 1（红 1）；v1 行 0（归档 0） |

D11（done / 归档不再过滤 items）不改变结果：planner down 的判定本身不看 lifecycle，Q3 已覆盖全部 track，只命中 `40b02ce4`；A-blk 按 D-a 逐 track 重算如下，仍在库里的 track 全部已关（`524f8151` 已删，不再整算）：

```sh
sqlite3 -readonly "$DB" <<'SQL'
WITH b AS (SELECT scope_track tid, MAX(id) eid FROM events
            WHERE kind='track.lifecycle_changed' AND json_extract(payload,'$.to')='blocked' GROUP BY 1)
SELECT substr(b.tid,1,8), (SELECT lifecycle FROM tracks t WHERE t.id=b.tid), e.at >
  MAX(COALESCE((SELECT MAX(at) FROM events l WHERE l.scope_track=b.tid AND l.kind='track.lifecycle_changed'
                  AND json_extract(l.payload,'$.from')='blocked'),0),
      COALESCE((SELECT MAX(u.at) FROM events u JOIN cards c ON c.id=u.scope_card AND c.role='planner'
                 WHERE u.kind='harness.user_message.enqueued' AND u.scope_track=b.tid
                   AND json_extract(u.actor,'$.kind')='User'),0)) AS open
  FROM b JOIN events e ON e.id=b.eid;
SQL
```

→ `08f2ce95|(已删)|0`、`524f8151|(已删)|1`、`5fe197ee|done|0`、`affb2b97|done|0`、`df5b037a|(已删)|0`；A-ntf 无行（Q4）。复核命令：Q1、Q2（期望 v2 27 行、`attention<>'none'` 1 行）；rail `document.querySelectorAll('[data-nc-activity="failed"],[data-nc-activity="attention"]').length` 期望 1。

## 10. 切片

| 片 | 内容 | 估行（改动行） | 为什么这样切 |
|---|---|---|---|
| **S0** 测试夹具挪动 | `Fx` 与转录 helper 从 `track_activity_projection.rs` 原样挪进共享测试模块，零行为变化 | ~250（纯移动） | 让 S1 的评审只看行为；术语计数不变 |
| **S1** 内核条目 + FE 解码与清理 | §4.1 的 N0–N3（**不含 N4**）、§4.2–4.4、§4.2 删除、校验器 v2、`WEB_COMPAT` bump；FE：解码 v2、`ActivityItem` 新形状、侧条行（标签、原话 / 原因 + 固定句、时间、`Reply`）、§6 删除清单全部、INV-APP-118（只含 Reply 一句）与全部移动的 oracle 锚点、变异清单除 F3 外的全部改动；§7 除 4b、18、20、21、F3 外各行；§7 清扫表 | ~1,350（内核实现 ~330、内核测试 ~570、FE 实现 ~250 其中删除过半、FE 测试与夹具 ~200） | payload v2、FE 解码、oracle 锚点与变异清单必须同 PR，否则 main 在两片之间要么全安静、要么门禁红。超出 ~1k 的部分主要是删除与夹具改版本号 |
| **S2** prompt | §5 三个文件 + 两个 golden | ~90 | 不提 Dismiss，可在 S1 前后任意时刻合并；在 #1828 合并后 rebase |
| **S3** Dismiss | 迁移 0119、路由、唤醒通道、N4 与 fold 里的过滤一步、OpenAPI + `REST_API_VERSION`、`openapi.rs` 清单、FE `Dismiss` 按钮与操作、INV-APP-118 加 Dismiss 一句；§7 行 4b、18、20、21、F3 | ~700 | UI 片；**preview 点**：S3 实现完成、进评审之前，在真浏览器给 owner 看 `40b02ce4`（红、原因 + 固定句、Reply、Dismiss）与一个人工造的 blocked track（琥珀、Reply、Dismiss） |

顺序 S0 → S1 → S3，S2 独立。每片跑本工作树 `AGENTS.md` 的 `scripts/local-ratchet-gates.sh`、定向 nextest（`-p calm-server track_notifications` / `track_activity_projection` / `terminal_signals`）与 `scripts/local-rust-gates.sh --quick`；FE 片跑 `(cd fe && npm ci && npm run lint && npm run build && npm test)`、browser 测试与 `npm run test:mutation:plan`。

## 11. 已知缺口（登记，不加固）

- **G1** Planner 会话 `failed` 而无 failed 轮（`interrupt_timeout` 楔住、reaper 判死、启动失败）不亮红，且发消息得 `409 planner_harness_dormant`；4140 为 0（Q8）。
- **G2** 一切发起拒绝都只在内存 `issuance_block`（K24），不亮红：codex 的 `turn/start` 失败（如配置无默认模型）与 Claude 起进程前的失败（未配置、`--version` 校验、`stop`、`spawn`，K17）。库里没有这类行可数；抽屉照旧显示原因。
- **G3** `harness.user_message.enqueued` 审计写失败（`cards.rs:930` 只记日志）⇒ 真回复了 ask 仍开着；Dismiss 兜底。
- **G4** 用户在 ask 之前发出、还排在队里的消息不关 ask（「之后」按时间算）。
- **G5** `ask.at_ms = MAX(U, L)`（同毫秒）按已关处理。
- **G6** （v1 的「done / 归档藏起项」已由 D11 删除。）
- **G7** （v1 的「`canceled` 不在终态过滤里」随 D11 不再成立：items 对任何 lifecycle 都不过滤。）
- **G8** 某项的必填 `text` 解码到 NULL（codex failed 轮缺 `error.message` 是唯一可设想的来源，§4.3 [a]）⇒ 只丢该项并记 warn。
- **G9** `/planner/reset` 硬删转录 ⇒ A-ntf 与 P-turn 的证据消失，项随之关闭（FE 无路径到 reset）。
- **G10** Dismiss 过的 key 行只增不删（每次点一行，track 删除时级联）。
- **G11** 同一问题既 block 又 notify ⇒ 两项；靠 prompt。
- **G12** 交互卡的 permission / elicitation、跨设备已读（#1741 B）、系统推送：issue 已划出范围。
- **G13** 审计事件不保证早于那条消息触发的一轮（§3.1）：若 Planner 在那一轮里、审计行落地前就发出新 ask，该 ask `at_ms < U`，生来即关。4140：Planner 卡上的 User 发送 114 条，能在 ±60 s 内配上转录 userMessage 行的 46 条，其中 1 条的行早于审计事件 67 ms；而从那一轮开始到第一次工具调用最少 5.4 s（Claude，11 条）、8.6 s（codex，35 条），比 67 ms 大两个数量级。仍用审计事件作 U；两个替代都被否：① 转录的 userMessage 行（drain 时写）——排在运行中的一轮后面的消息要等那一轮结束才落行，会把这段时间里新发出的 ask 当成「已回复」而误关（46 条里 2 条晚 20.6 s、34.4 s）；另外纯系统批次、AI 头发送也写同形行，要再按 `input_segments` 的 `presentation` 区分；② 队列条目的 `queued_at_ms`（`harness/queue.rs:80`，`snapshot.rs:35`）只活在快照的待发队列里，drain 后随条目离开快照，不是持久证据。命令（`$DB`、`$T` 同 §2；G13a → `114`；G13b → `matched|46|1|-67|20609,34366`、`claude|11|5416`、`codex|35|8632`；2026-09-28 第四轮在 4140 只读跑过）：

```sh
# G13a — Planner 卡上的 User 发送总数
sqlite3 -readonly "$DB" <<SQL
SELECT COUNT(*) FROM events e JOIN cards c ON c.id = e.scope_card AND c.role = 'planner'
 WHERE e.kind = 'harness.user_message.enqueued' AND json_extract(e.actor,'$.kind') = 'User';
SQL
# G13b — 与转录 userMessage 行配对（±60 s）、早于审计的条数、排队后落行的延迟、首个工具调用
sqlite3 -readonly "$DB" <<SQL
WITH s AS (SELECT e.id AS eid, e.at AS eat, e.scope_card AS card FROM events e
             JOIN cards c ON c.id = e.scope_card AND c.role = 'planner'
            WHERE e.kind = 'harness.user_message.enqueued' AND json_extract(e.actor,'$.kind') = 'User'),
p AS (SELECT h.card_id, h.worker_session_id AS ws, h.created_at_ms AS pat FROM $T h
       WHERE h.item_type = 'userMessage' AND h.method = 'item/completed'
         AND h.input_segments LIKE '%"presentation":"user"%'),
j AS (SELECT s.eid, s.eat, s.card, p.ws, p.pat,
             ROW_NUMBER() OVER (PARTITION BY s.eid ORDER BY abs(p.pat - s.eat)) AS rn
        FROM s JOIN p ON p.card_id = s.card AND p.pat BETWEEN s.eat - 60000 AND s.eat + 60000),
m AS (SELECT j.*, (SELECT MIN(t.created_at_ms) FROM $T t WHERE t.card_id = j.card AND t.method = 'item/started'
                     AND t.item_type = 'mcpToolCall' AND t.created_at_ms > j.pat) - j.pat AS first_tool_ms
        FROM j WHERE j.rn = 1)
SELECT 'matched', COUNT(*), SUM(pat < eat), MIN(pat - eat), group_concat(CASE WHEN pat - eat > 5000 THEN pat - eat END) FROM m
UNION ALL
SELECT w.provider, COUNT(*), MIN(m.first_tool_ms), NULL, NULL FROM m JOIN worker_sessions w ON w.id = m.ws GROUP BY w.provider;
SQL
```

§14 R2-10 行里的旧命令不带 Planner 卡过滤（配出 54 条），R3-5 行建议的补 join 在那条命令上会报 `ambiguous column name: id`；两者都以上面两条为准。
- **G14** `ratify` 的 Deny 不改 lifecycle，ask 仍开着，要 Reply 或 Dismiss。
- **G15** 归档 track 不在启动扫与 30 s tick 的枚举里（`track_activity/sql.rs:94-99`，`track_activity.rs:559-572`）：D11 之后它上面的项只在有事件唤醒它时才重算（例如 v1 行要等一次唤醒才改写成 v2，期间 FE 读作无 overlay）。4140 归档 track 0。

## 12. 风险

- **R1 原话不像问题**：4140 的 blocked message 多是机器串（`merge_hold: pr #1811 converged at …`、`baseline_mismatch: required=…`）；S2 的 prompt 是唯一缓解，S3 preview 时看真话。
- **R2 notify 来源零观测**：issue 说 6 次，实为文本匹配（Q4、Q5），真实调用 0。owner 已定保留 A-ntf（§0）；它复用现有 E2 行，但 4140 上无法验收。
- **R3 术语闸门**：`crates/` 里转录表名那一词的计数正好在基线，且两个方向都红；实现若多写或少写一次都要处理（§8）。
- **R4 Claude 失败轮可能偏多**（任何 init 检查失败、无原因的停止都算 failed，K17）⇒ 红点闪一轮；4140 Claude failed 轮 0。
- **R5 版本号**：`WEB_COMPAT` bump 让所有打开的旧页刷新一次；打包的移动端在 APK 重建前会看到 `app-update` 幕布（`fe/web/src/app/providers/public.tsx:88`：`__NC_BUNDLED__ && minWebCompatVersion > WEB_COMPAT_VERSION`）。`REST_API_VERSION` 与 `WEB_COMPAT` 的 bump 都让 neige-app 的 preflight 判 `Breaking`（`crates/neige-app/src/preflight.rs:286-295` 的 `api_version` 与 `min_web_compat_version` 两条）。

## 13. owner 决定

owner 于 2026-09-28 的决定已记入 §0 与正文：没有 Restart，两种项共用 `Reply`（§4.6、§6、§7 F2）；保留 A-ntf（R2）；done / 归档 track 上照样显示项（D11）。

## 14. 评审处置

### 第一轮（v1 → v2）

A = subagent 通道，B = codex 通道，O = 编排方裁决，Own = owner 决定。所有发现都先对代码或 4140 只读库核实过；本轮无驳回。

| id | 通道 | 处置 | 改在哪 |
|---|---|---|---|
| D-a 统一 ask 关闭规则 `at_ms > MAX(U, L)` | O（含 B MAJOR 2） | 采纳。核实：`blocked → blocked` 是空操作不发事件（`track_lifecycle.rs:42-44`），故 A-blk 的 `at_ms > L` ⇔ 仍 blocked | §0 D3/D4、§3.1、§4.1 N1b、§7 行 1–5 |
| D-b items 不做终态过滤 | O（B MAJOR 3） | 采纳；终态过滤只剩 `cards[]` 重建；4140 结果不变（§9 查询） | §0 D11、§1.3、§3.4、§4.2、§7 17b、§9、§11 G6/G7 |
| D-c Q1/Q2 暂按推荐 | O | 被 owner 附加决定取代（见下两行） | §13 |
| D-d 保持 `schemaVersion` 2 | O | 采纳，无改动 | §4.3 |
| 1 S1 须带 FE 删除、oracle 锚点、变异清单（含漏掉的 `s2b-notifications-from-card-status`） | A+B | 采纳。核实：`manifest.json:1655` 的该条 patch 让侧条改读 `kernel/card/status`，`expected_red` 9 条里 6 条随功能删除；锚点另有 `capabilities-e2e.yaml:116,332,354`、`pages-shared.yaml:61`。偏离一处：因 owner 取消 Restart，`Reply` 就是原按钮改目标改名，放在 S1；S3 只剩 Dismiss | §6、§8、§10 |
| 2 codex systemError 的行晚于 phase 事件 | A+B | 采纳。核实：`run_loop.rs:1839-1847` 置 Wedged → `:2221` `persist_snapshot` 发 phase；行在 `:1945`，`:1950-1957` 发 `harness.item.added{None, turn/completed}`；恢复路径 `preserving_recovery.rs:103-106,125-165` + `planner_recovery.rs:81-105` 同形 | K16、K18、§3.2、§4.4、§7 9b |
| 3 N3 物化整段转录 | A | 采纳。核实：v1 写法在 4140 上 `MATERIALIZE h` + 按 `card_id` 旧索引 + 临时 B 树；新写法两臂都走 `idx_transcript_card_method_created_at`（计划已贴）；notify 臂加 `created_at_ms > MAX(U, L)` 仍走索引（范围谓词进了索引） | §4.1 |
| 4 K17 只在起进程之后成立 | A | 采纳。核实：`session.rs:424,433,449,450,498-505` 都在 `spawn` 前返回 `Err`，经 `run_loop.rs:3342-3390` 只进 `issuance_block` 并回队 | K17、K24、§3.2、§4.6、§11 G2 |
| 5 enqueue 事件的第二个写者 | A | 采纳。核实：`planner_harness_start_adapter.rs:871-900`，由 `routes/tracks/create.rs:702` 与 `routes/track_conversations.rs:210`（Assistant）触发；两者都不会误关 ask（新 track / 非 Planner 卡） | K13、§3.1、§7 行 7 |
| 6 「审计先于 turn」不保证 | B | 采纳。核实：ack 在 `run_loop.rs:1184` 发出，`:1213` 的 tick 可先 `maybe_issue_turn`，路由审计在 `cards.rs:900-930` | §3.1、§11 G13 |
| 7 blocked 中再问 | A | 采纳：S2 加一句「还需要就用 notify」 | §5 |
| 8 ratify Deny 不关 ask | A | 采纳为缺口。核实：Deny 分支不做 `apply_requested_transition_in_tx`，只发 `ratify.resolved`（`cards.rs:1016-1040`） | §3.1、§11 G14 |
| 9 S2 顺序 | A | 采纳：选「S2 不提 Dismiss」，S2 与 S3 解耦 | §5、§10 |
| 10 G8 波及面 | A | 采纳：只丢该项 + warn，不补文案 | §4.3、§7 行 23、§11 G8 |
| 11 测试与调用点清扫 | A+B | 采纳。核实：`terminal_signals.rs:9,3375-3381`；`track_activity_projection.rs` 另有 6 条带 item 断言；`openapi.rs:10-40,88` 清单；E2 夹具无 `arguments.text`（`:1874-1882`） | §7 清扫表 |
| 12 棘轮下降也红 | A | 采纳。核实：1316 脚本头 `:3-4`、prose 脚本头同句 | §8 |
| 13 `planner.md` 锚点 | A | 采纳：lifecycle 段在 `:20`，重写与 #1828 的关系 | §5 |
| 14 版本 bump 对移动端 / neige-app 的影响 | A | 采纳。核实：`providers/public.tsx:88`；`neige-app/src/preflight.rs:286-295` | §12 R5 |
| 15 4140 漂移 | A | 采纳。13:05 重取：`ded027ca` 已 done、Claude Planner 11 行；Q2/Q3 不变 | §2 Q1/Q10、§9 |
| Own-Q1 没有 Restart，只有 Reply；planner down 行 = 原因 + 固定一句 | Own | 采纳 | §4.6、§6、§7 F2、§10、§13 |
| Own-Q2 保留 A-ntf | Own | 采纳 | §12 R2、§13 |
| Own-Q3 done / 归档照样显示 | Own | 采纳（即 D-b） | §0 D11、§13 |

### 第二轮（v2 → v3）

本轮无驳回；第 8 条有一处更正（见行内）。

| id | 通道 | 处置 | 改在哪 |
|---|---|---|---|
| R2-1 notify 工具描述与再问规则矛盾 | A | 采纳。核实：`calm.user.notify.md` 原句「Do not use it in turns opened by a user message…」；同一矛盾还在 `planner.md:129`（中文版），一并改。`run_loop.rs:2806-2810` 只挂在全是报告编辑的后台批次上（`:2803-2805`），不冲突、不改。registry golden 只存 `description_sha256`（`:2319-2320`），列入 S2 | §5 |
| R2-2 N3 丢 E2 证据 | B | 采纳。E2 恢复为全部成功 notify 行的 `MAX`，U/L 只在建 items 时用；4140 重跑 `EXPLAIN`，两臂仍 `SEARCH … idx_transcript_card_method_created_at (card_id=? AND method=?)` | §4.1、§7 行 24 |
| R2-3 S1/S3 边界 | A+B | 采纳：N4 与过滤移入 S3；行 4 拆成 4a（S1）/ 4b（S3）；INV-APP-118 在 S1 只写 Reply、S3 加 Dismiss | §4.1、§4.2、§6、§7、§10 |
| R2-4 归档 track 不被启动扫 / tick 枚举 | B | 采纳。核实：`sql.rs:94-99` 的 `WHERE archived_at IS NULL`；§4.3 注明「改写全部 27 行」只因归档为 0；17b 注明测的是 fold；新缺口 G15 | §4.3、§7 17b、§11 G15 |
| R2-5 变异红集要完整 | B | 采纳：固定各行夹具来源，逐个变异列完整预言红集（如去掉 U 另红 19、24） | §7 |
| R2-6 「方法过滤外移是必要的」不成立 | B | 采纳。核实：4140 上 `NOT MATERIALIZED` + v1 的 CTE 内 `OR` 过滤同样两臂走新索引；默认 CTE（不加 `NOT MATERIALIZED`）+ 过滤外移仍物化并用临时 B 树 | §4.1 |
| R2-7 解码变异不是单因素 | A | 采纳：改为 `z.union([z.literal(1), z.literal(2)])`，只红 F1 | §7 |
| R2-8 oracle 命令与两本账 | A | 命令采纳（`README.md:16`）；`anchor-pending.json` 只能删行，采纳。**更正**：简报说 `anchor-baseline.json`「可以编辑」，与代码不符——它也只能收缩：`README.md:5`「New, changed, duplicated, expired, or already-fixed rows fail validation」，`validator.ts:487-489`（行数上限）、`:570-575`（subtype 变了 = unbaselined、修好了 = stale）。文中按代码写 | §6 |
| R2-9 `gen:api` 的影响面 | A | 采纳。核实：`fe/package.json:14` 的 TS 导出只跑 calm-types 的 `export_bindings_`；请求体放 calm-server、只派生 `ToSchema` ⇒ `wire.ts` 不变，trailer 只给 `openapi.json` | §4.5 |
| R2-10 G13 的数字与替代方案 | A | 采纳。我的重算：Planner 卡的 User 发送 232 条里能在 ±60 s 内配上转录 userMessage 行的 54 条（评审为 52，配对窗口不同），1 条早 67 ms；首个工具调用最少 5.4 s（Claude，与评审同）/ 7.0 s（codex，评审 8.1 s）；排队后才落行的 2 条（20.6 s、34.4 s，与评审「2/52」一致）。命令：`WITH e AS (SELECT id eid, at eat, scope_card card FROM events WHERE kind='harness.user_message.enqueued' AND json_extract(actor,'$.kind')='User'), p AS (SELECT card_id, worker_session_id ws, created_at_ms pat FROM $T WHERE item_type='userMessage' AND method='item/completed' AND input_segments LIKE '%"presentation":"user"%'), j AS (SELECT eid, eat, pat, card, ws, ROW_NUMBER() OVER (PARTITION BY eid ORDER BY abs(pat-eat)) rn FROM e JOIN p ON p.card_id=e.card AND pat BETWEEN eat-60000 AND eat+60000) SELECT COUNT(*), SUM(pat<eat), MIN(pat-eat), SUM(pat-eat>5000) FROM j WHERE rn=1;` → `54\|1\|-67\|2`；首个工具调用：在 `j` 上对每行取同卡 `item/started` + `mcpToolCall` 且晚于 `pat` 的最小 `created_at_ms` 减 `pat`，按 `worker_sessions.provider` 取 `MIN` → claude 5416 / codex 7017 | §11 G13 |

### 第三轮（v3 → v3.1，收敛：A APPROVE，B 唯一 MAJOR 经编排方读 `validator.ts` 降为 MINOR）

本轮无驳回，全部先对代码或 4140 核实。

| id | 通道 | 处置 | 改在哪 |
|---|---|---|---|
| R3-1 baseline 措辞 | A+B | 采纳。核实：`anchor-baseline.json` 为 `[]`；`validator.ts:385` 上限 171、`:487-489` 行数、`:565-581` 逐条相等；只有 pending ID 冻结（`:433`）。改为「README 规则（`README.md:5`），本设计遵守」；删「S1 修好旧债就删行」一句。R2-8 行的「更正」据此收窄：validator 不单独拦改行，拦的是 README | §6 |
| R3-2 计划测试的 SCAN 断言 | A | 采纳。核实：`track_activity_projection.rs:1953-1956` 断言无细节以 `SCAN` 开头；N3 的 `SCAN (subquery-3)` 是 LIMIT 协程。改为「转录表无 SCAN」；以内置 SQLite（`libsqlite3-sys 0.30.1`）下 sqlx 的计划为准 | §4.1 |
| R3-3 夹具的 blocked 边 | A | 采纳：只有行 5 带已结束的 blocked 段 | §7 |
| R3-4 `browser_contract.rs:107-108` | A | 采纳。核实：断言 `webCompatVersion == 31`、`apiVersion == "12"` | §7 清扫表 |
| R3-5 G13 / R2-10 的数字 | A | 采纳。加上 Planner 卡的 role join 重算：User 发送 114 条（非 232，232 含 assistant 卡）、配上 46 条（54 里 8 条是 assistant）、1 条早 67 ms、排队后落行 2 条；首个工具调用 Claude 5.4 s（11 条）/ codex 8.6 s（35 条，R2-10 的 7.0 s 来自 assistant 卡）。结论不变。R2-10 的命令在 `e` 里加 `JOIN cards c ON c.id=e.scope_card AND c.role='planner'` 即得 `114`（总数）与 `46\|1\|-67\|2` | §11 G13 |
| R3-6 套件注册 | A | 采纳。核实：`tests/domain_api_suite.rs:69-70` 以 `#[path] mod` 注册 `track_activity_projection` | §7 清扫表 |
| R3-7 渠道表加再问指引（可选） | A | 采纳：`planner.md:133` 两格加一句 | §5 |

### 第四轮（v3.1 → v3.2，确认评审：codex 1 MAJOR + 1 MINOR，编排方已核实 MAJOR）

| id | 通道 | 处置 | 改在哪 |
|---|---|---|---|
| R4-1 行 7、8 继承行 5 的已结束 blocked 段，与「去掉 L」红集 {3, 3b, 5} 矛盾 | B | 采纳。核实：v3.1 的行 7、8 写作「5 之后」，去掉 L 时行 5 的旧 A-blk 复活，二者读到 2 项而变红。改为各自独立的 notify-only 夹具（`working` 下成功 notify、无 blocked 历史）；逐行复查变异表：带 `from='blocked'` 边的只有 3、3b、4a、4b、5（2、19、18 进入 blocked 后从不离开，见 R5-1），红集 {3, 3b, 5} 与「U 不按 Planner 卡 → {7}」「U 不看 actor → {8}」都成立 | §7 夹具说明、行 7、8、变异表 |
| R4-2 G13 的复现命令过时 | B | 采纳。换成两条完整命令（`FROM events e` 起别名、列全限定、按 Planner 卡过滤）并在 4140 只读跑过：`114`；`matched\|46\|1\|-67\|20609,34366`、`claude\|11\|5416`、`codex\|35\|8632`，与评审核对的数一致 | §11 G13 |

### 第五轮（v3.2 → v3.3，确认评审：1 处措辞缺陷，编排方已核实）

| id | 通道 | 处置 | 改在哪 |
|---|---|---|---|
| R5-1 行 2、19 被写成带 `from='blocked'` 边，行 18 被写成「无 blocked 历史」 | B | 采纳。行 2、19 是「1 之后 User 发给 Planner」，track 一直在 blocked、无离开边，只由 U 关闭；行 18 有 A-blk（`to='blocked'` 边）但从不离开，只由 Dismiss 关闭。带离开边的夹具只有 3、3b、4a、4b、5。按此重推一遍：去掉 L → {3, 3b, 5}（4a、4b 最新 blocked 边晚于 L 仍开；2、19 有 U；18 本就开）；去掉 U → {2, 19, 24}（2、19 唯一的关闭者是 U，24 同理；7、8 断言不变）；A-ntf 只比 U → {3b}。红集不变；§7 夹具说明写明 2、19、18 不得补离开边 | §7 夹具说明与变异表、§14 R4-1 |
