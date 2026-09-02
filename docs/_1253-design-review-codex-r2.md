
4. **Q5 未关闭，而它决定主键，PR1 还不能开工。**

   - 文档：D1 已写死 `<YYYY-MM-DD>`，§9 Q5 却仍在浏览器、服务端、workspace timezone 三选一。
   - 代码反证：现有 Today 完全使用浏览器本地 `Date`、`setHours` 和本地年月日：[public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/features/today/public.tsx:91)。
   - 建议：使用 app/workspace 级持久化 IANA 时区 `today_timezone`；首次由浏览器提供并由服务端验证、持久化，此后服务端独占 day key 与日窗计算。日窗采用本地日期对应的半开 UTC 区间，允许 DST 的 23/25 小时日。禁止以 server process timezone 或每次请求的浏览器 timezone 生成 ID。

## MAJOR

1. **“无条件 ensure”把文档读取绑在 workspace materialize 和 harness 健康上。**

   - 文档：§5.1「幂等，所以无条件调用更简单且更少一次往返」。
   - 代码反证：ensure 不只是 resolve；它提交数据库事务后还会 materialize workspace、提交并等待 `spec-harness-start`：[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:370)、[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:396)、[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:442)。后两步失败时 launchpad 已存在，但 FE 得不到 wave id，连历史 daily-log 也看不了。
   - 另一个反证：唯一冲突 helper 检查错误消息是否含索引名，却传入 `idx_waves_one_launchpad`：[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:66)。SQLite 实际报告 `UNIQUE constraint failed: waves.purpose`；仓库另一条 ensure 测试正是按列名断言：[chat_wave_ensure.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/tests/cases/chat_wave_ensure.rs:249)。
   - 建议：增加只读 `GET /api/today/launchpad`；页面先 resolve，404 时才 ensure。显式“写今日进度”可以要求 harness 可用，但阅读历史不应要求它。冲突匹配改为 `waves.purpose`，并补完整 endpoint 并发测试。

2. **“列 cards 就是日历索引”会一次下载全部历史正文。**

   - 文档：D1「列 cards，一次请求拿到哪些天有进度」。
   - 代码反证：`cards_by_wave` SELECT 包含每张卡完整 `payload`：[read.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/read.rs:550)；wave detail 原样返回全部 cards：[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:694)。
   - 建议：提供 daily-log metadata index `{card_id, day_key, updated_at, byte_size}`，选日后再取一张卡。否则 N × 单卡上限直接变成 Today 首屏响应上限。

3. **D6 的硬边界尚未落在真实写 seam，GC 先例也引用错层。**

   - 文档：D6「由 `CardKindHandler::validate_payload` 拒绝」「服务端 GC」。
   - 代码反证：report CRDT persist 不调用 CardKindRegistry；validator 目前只在通用 card REST/plugin 写路径调用。`calm.admin.wave_gc` 是 Spec-only 的手动 VCS GC，不是卡保留服务：[admin.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/admin.rs:35)；真正的后台先例是 `spawn_wave_history_pruner`：[gc.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/wave_vcs/gc.rs:107)。
   - 建议：上限校验放进 daily-log CAS 事务；GC 明确 boot wiring、周期、按 oldest day 删除、事件与 role-cache 清理。给 daily-log 单独设 prose/总 payload 上限，不要把“顺带改变所有 wave-report prose 契约”夹带进 PR1。

4. **r1 的 event allowlist/turns MAJOR 仍未关闭。**

   - 文档：D4 仍写「`turns` 要么删掉，要么改定义，r2 倾向删掉」，这不是决策，也没有列出最终 allowlist。
   - 代码反证：事实源仍是会被 prune 的 `harness.item.added`，设计没有出现替代永久事实源。
   - 建议：本轮直接裁决删除 `turns`；固定 allowlist 为 lifecycle、report edit、task completed/failed，并逐项写清 retention 与计数定义。

## MINOR / 已确认成立

1. **确定性 ID 的字符串形状本身兼容。** `CardId` 是完全 opaque 的 string，DB 是 `TEXT PRIMARY KEY`，不做 UUID 解析：[ids.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/ids.rs:24)、[0001_init.sql](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/migrations/0001_init.sql:31)。建议从末尾严格解析日期，wave id 保持 opaque；所有 URL 使用 `encodeURIComponent`。

2. **一张 wave 挂 N 张同 kind 不会破坏现有通用读取。** cards/wave detail 返回数组，没有 kind 唯一假设；`readWaveReport` 精确查找 `wave-report`，会忽略 daily-log：[report.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/report.ts:230)。但 Today 仍需新的 `readDailyLog` 与严格 card-id/day-key parser。

3. **D4 授权在设计文字层已经真正关闭 r1 BLOCKER 3。** `visible_to_roles: &[]`、list contextual augmentation、call 独立重查、Assistant-only、active session、launchpad marker、归属一致及 fail-closed 条件都完整；实现时按同一个 async gate helper 调两次即可。

## §9 推荐答案

- **Q1：一天一卡。** 日期身份、CAS 隔离和整日 GC 都更可靠；但必须配 metadata index，不能用完整 wave detail 当索引。
- **Q2：42 个本地日期且总 payload ≤ 4 MiB，任一超限即从最老卡删除。** 42 天对应六个可导航周；另设单卡 128 KiB、32 blocks、单 prose 32 KiB。
- **Q3：状态条在前。** 只放 counts + waiting compact rows；文档紧随其后，Running/Recent 留右栏。
- **Q4：服务端固定“今天”，MCP 不接受 `since/until`。** 服务端从已验证 launchpad 身份和 Q5 时区计算半开日窗，既减少攻击面也消除 agent 算错边界。
- **Q5：持久化 IANA workspace/app 时区。** 浏览器仅负责首次建议；服务端是 day key、card id 和活动窗口的唯一作者。旧 card id 永不因设置变化而重写。

r1 关闭情况：原 launchpad 可达性、report 文体冲突、D4 授权、D5 空活动事实源方向、D3 事件派生否决和 PR DAG 已关闭；增长边界、turns、真正幂等、可证不变量及 timezone 尚未关闭。
tokens used
248,450
总判：**fix-then-ship**。`daily-log` 方向成立，但 r2 把“新增 kind 很便宜”误当成了“新增可写文档载体很便宜”。当前设计仍有 4 个 BLOCKER。

## BLOCKER

1. **`daily-log` 目前只能成为一种可校验的 JSON 卡，不能复用现有 report 写通道。**

   - 文档：§3「新增 card kind 是一个小 trait impl + 注册」、D1「payload 复用 report 的块模型」、D5「内核写通道零改动」。
   - 代码反证：
     - `calm.report.blocks.*` 最终调用 `resolve_report_for_caller`，始终查找 `kind == "wave-report"`：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report.rs:487)、[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report.rs:499)。
     - REST block API同样通过 wave id 解析唯一 `wave-report`：[wave_report_blocks.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_report_blocks.rs:107)。
     - CRDT 落库 seam 明确拒绝非 `wave-report`：[card.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/card.rs:251)。
     - Assistant 的角色墙只允许写自己或同 wave 的 `ReportCard`：[role_gate.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/role_gate.rs:676)；但数据库又限定一个 wave 只能有一张 `ReportCard`：[0013_cards_deletable.sql](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/migrations/0013_cards_deletable.sql:28)。因此 daily-log 若用 Worker role，Assistant 被拒；若用 ReportCard role，第二张卡就撞索引。
   - 建议：PR1 必须显式包含文档运行时改造：
     - 将唯一索引改为按 `kind='wave-report'` 唯一，使多张 `daily-log` 可以使用 ReportCard 权限语义；或新增独立 Document role，并相应改 role gate。
     - 新增按 day key 定位的 daily-log MCP 写工具。
     - 抽取 kind-aware CRDT/CAS persist seam；daily-log 不应产生 `WaveReportEdited` 或 task projection。
     - D5 的「内核写通道零改动」和 PR1 `~1.0k` 估算必须删除或重估。

2. **确定性 ID 与“upsert/幂等”只有设想，没有现存创建路径。**

   - 文档：D1「确定性 card id」「重复汇总 = upsert」、INV-001/006「全部写入过该函数」「第二次 upsert 覆盖」。
   - 代码反证：
     - 通用 REST 创建无 `id` 参数，固定调用 `new_id()`：[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:370)。
     - 它还固定创建 `Worker + deletable=true`：[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:410)。
     - `create_mode`、`persistence_invariants` 目前没有生产执行者；registry 的公共行为只有 claims/validate：[card_kind.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/card_kind.rs:73)、[card_kind.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/card_kind.rs:138)。
     - 现有 block create 的 `id=None` 会新建块；稳定 card id 只能保证“不多一张卡”，不能阻止重试后重复正文：[wave_report_blocks.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report_blocks.rs:179)。
   - 建议：新增唯一 kernel-minted 边界 `ensure_and_write_daily_log(day_key, if_doc_rev, body)`，在一个 `BEGIN IMMEDIATE` 事务里验证 wave/purpose、计算 ID、insert-or-load、CAS 更新、校验上限并发事件。工具采用“整份当日文档重写”或稳定 block id，不能靠无 id 的追加实现幂等。

3. **D4 扩 `list_waves_window` 不能成为正确的活动事实源。**

   - 文档：D4「成为工作区活动窗口的唯一 server-side projection」、D5「同一份 projection 为空才不发起」。
   - 代码反证：
     - 该端点查询的是 wave 生命周期与日历窗口的重叠：`created_at <= until` 且 `terminal_at IS NULL OR terminal_at >= since`，不是事件活动：[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:613)、[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:664)。
     - 已在窗口前 terminal 的 wave，今天仍可被用户编辑 report；block REST 写路径没有 lifecycle 禁止，但它会被上述候选集排除。
     - 现端点是双端 inclusive；连续两天会把恰好落在午夜的事件重复计数。
   - 建议：抽出 event-first 的 `workspace_activity_window`：先按 `events.at >= start AND at < next_start` 聚合显式 kind allowlist，再 join user cove/wave。REST 和 MCP 调同一个 repo/service helper。不要以 `waves_window` 的生命周期候选集作为活动查询的上游。

4. **Q5 未关闭，而它决定主键，PR1 还不能开工。**

   - 文档：D1 已写死 `<YYYY-MM-DD>`，§9 Q5 却仍在浏览器、服务端、workspace timezone 三选一。
   - 代码反证：现有 Today 完全使用浏览器本地 `Date`、`setHours` 和本地年月日：[public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/features/today/public.tsx:91)。
   - 建议：使用 app/workspace 级持久化 IANA 时区 `today_timezone`；首次由浏览器提供并由服务端验证、持久化，此后服务端独占 day key 与日窗计算。日窗采用本地日期对应的半开 UTC 区间，允许 DST 的 23/25 小时日。禁止以 server process timezone 或每次请求的浏览器 timezone 生成 ID。

## MAJOR

1. **“无条件 ensure”把文档读取绑在 workspace materialize 和 harness 健康上。**

   - 文档：§5.1「幂等，所以无条件调用更简单且更少一次往返」。
   - 代码反证：ensure 不只是 resolve；它提交数据库事务后还会 materialize workspace、提交并等待 `spec-harness-start`：[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:370)、[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:396)、[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:442)。后两步失败时 launchpad 已存在，但 FE 得不到 wave id，连历史 daily-log 也看不了。
   - 另一个反证：唯一冲突 helper 检查错误消息是否含索引名，却传入 `idx_waves_one_launchpad`：[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:66)。SQLite 实际报告 `UNIQUE constraint failed: waves.purpose`；仓库另一条 ensure 测试正是按列名断言：[chat_wave_ensure.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/tests/cases/chat_wave_ensure.rs:249)。
   - 建议：增加只读 `GET /api/today/launchpad`；页面先 resolve，404 时才 ensure。显式“写今日进度”可以要求 harness 可用，但阅读历史不应要求它。冲突匹配改为 `waves.purpose`，并补完整 endpoint 并发测试。

2. **“列 cards 就是日历索引”会一次下载全部历史正文。**

   - 文档：D1「列 cards，一次请求拿到哪些天有进度」。
   - 代码反证：`cards_by_wave` SELECT 包含每张卡完整 `payload`：[read.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/read.rs:550)；wave detail 原样返回全部 cards：[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:694)。
   - 建议：提供 daily-log metadata index `{card_id, day_key, updated_at, byte_size}`，选日后再取一张卡。否则 N × 单卡上限直接变成 Today 首屏响应上限。

3. **D6 的硬边界尚未落在真实写 seam，GC 先例也引用错层。**

   - 文档：D6「由 `CardKindHandler::validate_payload` 拒绝」「服务端 GC」。
   - 代码反证：report CRDT persist 不调用 CardKindRegistry；validator 目前只在通用 card REST/plugin 写路径调用。`calm.admin.wave_gc` 是 Spec-only 的手动 VCS GC，不是卡保留服务：[admin.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/admin.rs:35)；真正的后台先例是 `spawn_wave_history_pruner`：[gc.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/wave_vcs/gc.rs:107)。
   - 建议：上限校验放进 daily-log CAS 事务；GC 明确 boot wiring、周期、按 oldest day 删除、事件与 role-cache 清理。给 daily-log 单独设 prose/总 payload 上限，不要把“顺带改变所有 wave-report prose 契约”夹带进 PR1。

4. **r1 的 event allowlist/turns MAJOR 仍未关闭。**

   - 文档：D4 仍写「`turns` 要么删掉，要么改定义，r2 倾向删掉」，这不是决策，也没有列出最终 allowlist。
   - 代码反证：事实源仍是会被 prune 的 `harness.item.added`，设计没有出现替代永久事实源。
   - 建议：本轮直接裁决删除 `turns`；固定 allowlist 为 lifecycle、report edit、task completed/failed，并逐项写清 retention 与计数定义。

## MINOR / 已确认成立

1. **确定性 ID 的字符串形状本身兼容。** `CardId` 是完全 opaque 的 string，DB 是 `TEXT PRIMARY KEY`，不做 UUID 解析：[ids.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/ids.rs:24)、[0001_init.sql](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/migrations/0001_init.sql:31)。建议从末尾严格解析日期，wave id 保持 opaque；所有 URL 使用 `encodeURIComponent`。

2. **一张 wave 挂 N 张同 kind 不会破坏现有通用读取。** cards/wave detail 返回数组，没有 kind 唯一假设；`readWaveReport` 精确查找 `wave-report`，会忽略 daily-log：[report.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/report.ts:230)。但 Today 仍需新的 `readDailyLog` 与严格 card-id/day-key parser。

3. **D4 授权在设计文字层已经真正关闭 r1 BLOCKER 3。** `visible_to_roles: &[]`、list contextual augmentation、call 独立重查、Assistant-only、active session、launchpad marker、归属一致及 fail-closed 条件都完整；实现时按同一个 async gate helper 调两次即可。

## §9 推荐答案

- **Q1：一天一卡。** 日期身份、CAS 隔离和整日 GC 都更可靠；但必须配 metadata index，不能用完整 wave detail 当索引。
- **Q2：42 个本地日期且总 payload ≤ 4 MiB，任一超限即从最老卡删除。** 42 天对应六个可导航周；另设单卡 128 KiB、32 blocks、单 prose 32 KiB。
- **Q3：状态条在前。** 只放 counts + waiting compact rows；文档紧随其后，Running/Recent 留右栏。
- **Q4：服务端固定“今天”，MCP 不接受 `since/until`。** 服务端从已验证 launchpad 身份和 Q5 时区计算半开日窗，既减少攻击面也消除 agent 算错边界。
- **Q5：持久化 IANA workspace/app 时区。** 浏览器仅负责首次建议；服务端是 day key、card id 和活动窗口的唯一作者。旧 card id 永不因设置变化而重写。

r1 关闭情况：原 launchpad 可达性、report 文体冲突、D4 授权、D5 空活动事实源方向、D3 事件派生否决和 PR DAG 已关闭；增长边界、turns、真正幂等、可证不变量及 timezone 尚未关闭。
