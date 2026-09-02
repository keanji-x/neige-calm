   - 文档：[D1「在同样两处对 daily-log: 前缀碳拷贝」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:99)。
   - 代码反证：recovery 与 replay 都是 `== Some(COVE_CHAT_PURPOSE)`，[harness/mod.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/mod.rs:117)、[replay.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/replay.rs:497)。此外 start adapter、reset endpoint 和 dead-root SQL 也各有独立 exact-purpose 判断。[adapter](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/operation/spec_harness_start_adapter.rs:560)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:1341)、[session_repo_impl.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/session_repo_impl.rs:210)
   - 建议：定义共享 `spec_harness_disabled_for_wave(purpose)`：exact cove-chat 或 canonical daily-log 前缀；所有 start、reset、replay、recovery、reaper 判据调用/对齐它，并做调用面扫地测试。

3. **daily-log 的动态 purpose 和日志契约都不能直接通过现有 wave 创建函数。**

   - 文档：[D1「WaveReportPayload::new + 日志契约前缀播种」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:97)。
   - 代码反证：`create_wave_structure` 的 purpose 是 `Option<&'static str>`，无法传 `daily-log:<day>`；它还固定铸 `WaveReportPayload::initial()`。[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:1387) 更底层的 report-card 创建明确拒绝任何非 `initial()` payload。[card.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/card.rs:61)
   - 建议：purpose 改为受控 owned value；report 仍先合法出生为 `initial()`，再仿 template 路径经 persist seam restamp，并定义崩溃后 ensure 如何识别和修复半播种状态。

4. **D8 的“完整用户可见面”清单不完整，一条 Wave predicate 覆盖不了。**

   - 文档：[D8 已知面清单](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:170)。
   - 代码反证，实际面包括：
     - discovery：`GET /api/coves`、`GET /api/coves/:id/waves`、`GET /api/waves`；
     - 辅助全局读：`GET /api/overlays?entity_kind=wave` 当前返回所有 wave overlay，daily-log 的 layout/spec/report IDs 会进入浏览器。[overlays.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/overlays.rs:71)
     - direct-ID 面：wave detail、cards、report、conversations、files、backlinks，以及 card harness-items/spec-run/thread lookup；这些又必须有一部分供 Today 使用；
     - FE：`useWorkspace` 依靠“先过滤 system cove、再 fan-out”隐藏 daily-log，[queries.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/providers/queries.ts:572)；sidebar/mobile 用 `userVisibleWaves`，但 Cove/Today 用的 `visibleWaves` 只过滤 archived，`activeWavesOn` 完全不看 purpose。[wave.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/wave.ts:372)
   - 建议：将义务拆成“普通 discovery 不出现”“Today 专用投影可读”“direct-ID 哪些允许”“overlay/event 不泄漏”四类契约；分别扫端点和 FE cache 输入，不能用一个 bool 宣称全覆盖。

5. **隐藏 daily wave 后，它的 Assistant conversation 没有一致的用户归宿。**

   - 文档：[D7「现有 Conversations 模块不动」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:165)。
   - 代码反证：Today 的 Conversations 只读本 tab 已打开过的 registry，不从服务端枚举所有 wave conversations。[router/public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/router/public.tsx:106) 新 daily Assistant 不会自然出现；若强行记入，点击行为会导航到普通 `/wave/{daily-id}`，反过来违反 D8。[router/public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/router/public.tsx:1422)
   - 建议：明确汇总会话是否用户可读。可读则做 Today 专用 run-history/drawer；不可读则成功后关闭会话，别留下 42 个隐藏但可恢复的 conversations。

## MINOR

1. **N=42 只约束 live rows，不是实际稳态磁盘。**

   基础 live 成本为 42 waves、84 内建 cards、42 layout overlays、168 条创建事件、42 个 wave-VCS heads/至少 42 commits、42 个 Git 仓库；若每天汇总，再加约 42 Assistant cards/sessions/operations。即使修好回收，trash 还有约 7 天，即约 49 个磁盘 Git 仓库。operations、events 永久增长；wave-VCS objects 仅在后续 hourly sweep 回收；workspace path-lock map 也明确永不驱逐。[workspace_materialize.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/workspace_materialize.rs:289)

2. **当前代码下，r2 所说“一年 365 个 Git 仓库”仍会发生。**

   不是因为没有 42 天策略，而是该策略目前无法回收 system-cove workspace。r3 只有在新增受控 internal recycler 后，才有资格把这一成本改写成 N=42。

总判：**fix-then-ship**。载体方向可以保留；先关闭 system-cove GC、Assistant session 生命周期和专用可见性/索引端点这三个 BLOCKER。
tokens used
313,376
结论：每天一份 wave 确实通过了 r2 的四条载体 BLOCKER，但 r3 仍有 3 个新 BLOCKER。主要问题不在 report 写通道，而在资源生命周期、system-cove 回收和可见性模型。

### D1 四条复核

| 项目 | 结论 | 代码依据 |
|---|---|---|
| B1 report 写通道 | 成立 | 工具从 Assistant 卡解析 home wave，再找该 wave 的 `kind == "wave-report"`；daily-log 正好符合。[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report.rs:460) |
| B2 role gate | 成立 | 实现允许 `target == self`，或目标为同一 home wave 的 `ReportCard`。[role_gate.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/role_gate.rs:689) |
| B3 purpose 单例 | 成立，但不是照抄现有任一索引 | `UNIQUE(purpose) WHERE purpose LIKE 'daily-log:%'` 能按完整字符串全局去重；不能照抄 chat 的 `UNIQUE(cove_id)`，否则 system cove 总共只能有一天。[0064](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/migrations/0064_waves_launchpad_purpose.sql:3)、[0074](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/migrations/0074_one_chat_wave_per_cove.sql:1) |
| B4 新 kind 的死 trait | 成立 | 不新增 kind，确实与 `create_mode` 等无关。 |
| spec carve-out | 不成立为“碳拷贝” | 现有判断全是精确相等，不能识别动态前缀。并且强制点不止文档所说的 recovery/replay。 |

## BLOCKER

1. **system cove 的 daily-log 无法走“既有删除路径”回收工作区。**

   - 文档：[D6「删一天 = 删一条 wave（既有删除路径）」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:151)，并要求后台 GC 清理目录与 role cache。
   - 代码反证：公开 wave 删除对整个 system cove 返回 403，[delete_wave](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:3125)；即使内部绕过 REST，唯一 workspace recycler 也要求 `CoveKind::User`，明确拒绝 system cove。[workspace_recycle.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/workspace_recycle.rs:207)
   - 额外残留：wave cascade 不调用 `CardRoleCache::remove`；`wave_delete_tx` 只清 wave→cove cache，[wave.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/wave.rs:467)，而 role cache 只有单卡删除会清。[card.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/card.rs:554)
   - 建议：设计并复用一个 kernel-only `delete_daily_log_wave`，完整执行 runtime teardown、role-cache 扫除、VCS/row 删除；workspace recycler 增加以 `purpose + ownership marker` 为依据的窄授权，不能把 system-cove 总禁令直接拆掉。

2. **“残余成本只有目录 + git init”漏掉每天一个长期存活的 Assistant harness。**

   - 文档：[D1「残余成本」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:101)只列 workspace；D5 又要求每天在该 wave 上创建 Assistant 会话。
   - 代码反证：conversation POST 会铸 Assistant 卡、提交永久 `spec-harness-start` operation，并等待 session 启动。[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:176) `idle` 仍被视为 active；boot selector明确恢复所有 Assistant session，[session_projection.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/session_projection.rs:495)，每个恢复后的 harness 都有一个 50ms tick run loop。[run_loop.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/run_loop.rs:516)
   - N=42 时，除共享 codex daemon 外，最多还有 42 个 active/idle session projection、42 个 harness registry entry/run-loop、42 个 Assistant 卡和至少 42 条永久 operation。
   - 建议：先裁决写者是“一次性汇总 job”还是“可继续对话”。若是前者，成功后必须 shutdown 并 terminalize session；若是后者，r3 必须承认并预算 42 个可恢复会话，不能把成本写成只有 Git 仓库。

3. **D2 与 D8 对同一个通用端点提出互斥要求。**

   - 文档：D2/§5.1 要 `GET /api/waves?...purpose_prefix=daily-log:` 返回 daily-log，[§5.1](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:180)；D8/INV-009 又要求一个 `user_visible_wave` 判据令所有读端点都不返回它。[D8](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:168)
   - 代码反证：`GET /api/waves` 无条件调用现有 `retain_user_visible_waves`。[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:664) 把 daily-log 加进 predicate 后，D2 的索引请求也会变空；若按 query 参数绕过，predicate 就不再是 D8 所称的单一全称判据。
   - 建议：新增专用 `GET /api/today/daily-logs?from_day&to_day`，只返回 `{wave_id, day_key, updated_at}`；普通 wave discovery 永远过滤 daily-log。两者使用不同 DTO/cache lane，不能让 Today 索引混进 `workspace.waves`。

## MAJOR

1. **`list_waves_window` 不是 day-key 索引。**

   - 文档：[D1「存在性正是 lifecycle overlap 能正确回答」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:95)。
   - 代码反证：查询只按 `created_at <= until` 和 `terminal_at IS NULL OR terminal_at >= since`。[read.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/read.rs:141) daily-log 保持 Draft、`terminal_at=NULL`，所以 8 月 1 日的日志会命中之后每一个窗口，直到 GC 删除；它代表哪一天只存在于 `purpose`，窗口查询完全没看它。
   - 建议：专用索引按 canonical day key 做半开范围查询，例如完整 purpose 的字典序边界；不要复用生命周期窗口。

2. **spec harness 禁用不是“两处碳拷贝”，且相等比较不能覆盖前缀。**

   - 文档：[D1「在同样两处对 daily-log: 前缀碳拷贝」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:99)。
   - 代码反证：recovery 与 replay 都是 `== Some(COVE_CHAT_PURPOSE)`，[harness/mod.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/mod.rs:117)、[replay.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/replay.rs:497)。此外 start adapter、reset endpoint 和 dead-root SQL 也各有独立 exact-purpose 判断。[adapter](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/operation/spec_harness_start_adapter.rs:560)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:1341)、[session_repo_impl.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/session_repo_impl.rs:210)
   - 建议：定义共享 `spec_harness_disabled_for_wave(purpose)`：exact cove-chat 或 canonical daily-log 前缀；所有 start、reset、replay、recovery、reaper 判据调用/对齐它，并做调用面扫地测试。

3. **daily-log 的动态 purpose 和日志契约都不能直接通过现有 wave 创建函数。**

   - 文档：[D1「WaveReportPayload::new + 日志契约前缀播种」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:97)。
   - 代码反证：`create_wave_structure` 的 purpose 是 `Option<&'static str>`，无法传 `daily-log:<day>`；它还固定铸 `WaveReportPayload::initial()`。[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:1387) 更底层的 report-card 创建明确拒绝任何非 `initial()` payload。[card.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/card.rs:61)
   - 建议：purpose 改为受控 owned value；report 仍先合法出生为 `initial()`，再仿 template 路径经 persist seam restamp，并定义崩溃后 ensure 如何识别和修复半播种状态。

4. **D8 的“完整用户可见面”清单不完整，一条 Wave predicate 覆盖不了。**

   - 文档：[D8 已知面清单](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:170)。
   - 代码反证，实际面包括：
     - discovery：`GET /api/coves`、`GET /api/coves/:id/waves`、`GET /api/waves`；
     - 辅助全局读：`GET /api/overlays?entity_kind=wave` 当前返回所有 wave overlay，daily-log 的 layout/spec/report IDs 会进入浏览器。[overlays.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/overlays.rs:71)
     - direct-ID 面：wave detail、cards、report、conversations、files、backlinks，以及 card harness-items/spec-run/thread lookup；这些又必须有一部分供 Today 使用；
     - FE：`useWorkspace` 依靠“先过滤 system cove、再 fan-out”隐藏 daily-log，[queries.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/providers/queries.ts:572)；sidebar/mobile 用 `userVisibleWaves`，但 Cove/Today 用的 `visibleWaves` 只过滤 archived，`activeWavesOn` 完全不看 purpose。[wave.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/wave.ts:372)
   - 建议：将义务拆成“普通 discovery 不出现”“Today 专用投影可读”“direct-ID 哪些允许”“overlay/event 不泄漏”四类契约；分别扫端点和 FE cache 输入，不能用一个 bool 宣称全覆盖。

5. **隐藏 daily wave 后，它的 Assistant conversation 没有一致的用户归宿。**

   - 文档：[D7「现有 Conversations 模块不动」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:165)。
   - 代码反证：Today 的 Conversations 只读本 tab 已打开过的 registry，不从服务端枚举所有 wave conversations。[router/public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/router/public.tsx:106) 新 daily Assistant 不会自然出现；若强行记入，点击行为会导航到普通 `/wave/{daily-id}`，反过来违反 D8。[router/public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/router/public.tsx:1422)
   - 建议：明确汇总会话是否用户可读。可读则做 Today 专用 run-history/drawer；不可读则成功后关闭会话，别留下 42 个隐藏但可恢复的 conversations。

## MINOR

1. **N=42 只约束 live rows，不是实际稳态磁盘。**

   基础 live 成本为 42 waves、84 内建 cards、42 layout overlays、168 条创建事件、42 个 wave-VCS heads/至少 42 commits、42 个 Git 仓库；若每天汇总，再加约 42 Assistant cards/sessions/operations。即使修好回收，trash 还有约 7 天，即约 49 个磁盘 Git 仓库。operations、events 永久增长；wave-VCS objects 仅在后续 hourly sweep 回收；workspace path-lock map 也明确永不驱逐。[workspace_materialize.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/workspace_materialize.rs:289)

2. **当前代码下，r2 所说“一年 365 个 Git 仓库”仍会发生。**

   不是因为没有 42 天策略，而是该策略目前无法回收 system-cove workspace。r3 只有在新增受控 internal recycler 后，才有资格把这一成本改写成 N=42。

总判：**fix-then-ship**。载体方向可以保留；先关闭 system-cove GC、Assistant session 生命周期和专用可见性/索引端点这三个 BLOCKER。
