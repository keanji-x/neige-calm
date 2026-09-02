# r3 窄审 — subagent 通道

> 总判：**fix-then-ship**。载体（每天一份 wave）在写通道和 role_gate 上实测成立；挡住 ship 的是 GC 无执行器与 D2/D8 自相矛盾。

## 实测确认成立的（不必再复核）

- **B1 写通道真的通。** `mcp_server/tools/wave_report.rs::resolve_report_for_caller` 从 caller 卡的 `wave_id` 解析 report 卡，`wave_report_blocks.rs` 全部 `require_role_any([Spec, Assistant])`。
- **B2 role_gate 真的放行。** 读的是 `role_gate.rs::enforce_assistant_scope` + `enforce_card_scope` 的**实现**而非注释：target 允许 `self` 或 `cache.get(target) == ReportCard && cache.wave_of(target) == home_wave`，随后 `scope.wave == home_wave`、`scope.cove == cove_of(home_wave)` 两道 #232/#234 反欺骗天然满足（assistant 卡就生在这条 wave 上）。

## BLOCKER

**B-1 —— D6「删一天 = 删一条 wave（既有删除路径）」不成立：system cove 的 wave 在两层上都不可删。**

1. `routes/waves.rs::delete_wave` 在任何 teardown 之前 `if owning_cove.kind == CoveKind::System { return Forbidden }`。其上方长注释是 **2026-09-01 裁决**，故意覆盖整个 system cove，并逐字反对本设计要做的事：*"the alternative — carving out `purpose = launchpad` — puts an exception into 'the system cove is kernel-owned'. An invariant with an exception is the shape this design line keeps getting hurt by."*
2. `workspace_recycle.rs::decide_and_move` guard 4：`cove_kind != User → Refused(SystemCove)`。即便绕过 route 直调 `Repo::wave_delete`，**磁盘目录也永远回收不了**——`wave_delete_tx` 只删 DB 行不碰文件系统。

后果：42 天保留窗口没有执行器。稳态不是「42 个 git 仓库」，是**每天 +1、永不回收**。

建议三选一（都要进 PR1 成本表）：(a) 不放 system cove（但撞 D8）；(b) 新增 kernel-only 删除通道 + 按 purpose 的窄豁免，并正面推翻 09-01 裁决；(c) **不给 daily-log wave 工作区**，把「回收」从设计里消掉。倾向 (c)+(b)——daily-log wave 里没有任何 worker 会跑，一个 git 仓库买不到任何东西。

**B-2 —— INV-009 与 D2/§5.1 直接互斥。**

D8/INV-009 要求「任一读端点」都不返回 daily-log；而 §5.1 的日历索引走 `list_waves_window`（返回前无条件跑 `retain_user_visible_waves`），文档正文走 `get_wave_detail`（**完全没有**这个过滤）。第一行要求加进去，D8 要求剔出来；第三行依赖不过滤，而 INV-009 的句式把它判红。这是 r3 新引入的——r2 的载体是卡，不产生 wave 可见面义务。

建议：INV-009 重写成**枚举面 vs 定址面**的二分（枚举面默认不含；定址面允许，调用方已知 id）。日历索引**不要**给 `list_waves_window` 加 `purpose_prefix`——那等于给公共列表端点开一个按 purpose 取回隐藏行的口子，判据变成可被 query 参数绕过。改用专用只读端点。
另：D8 说 `list_waves_window`「当前只排除 `COVE_CHAT_PURPOSE`」属实，但它**还**排除 template waves，判据是两处不是一处，扫地测试的 oracle 要按真实形状写。

## MAJOR

**M-1 —— harness carve-out 不是两处是五处，且其中两处是手写 SQL、一处把「跳过」变成「失败」。**
实际强制点：`harness/mod.rs`（recovery skip）✔文档有、`replay.rs`（Forbidden）✔文档有、`routes/cards.rs`（`/spec/reset` 403）✘、`operation/spec_harness_start_adapter.rs`（validate → **Forbidden**，是拒绝不是跳过）✘、`session_repo_impl.rs`（两处手写 SQL，注释「Keep in sync with calm_server::COVE_CHAT_PURPOSE」）✘。
第 5 处**不能碳拷贝**：等值改前缀后 SQL 变成 `(w.purpose IS NULL OR w.purpose NOT LIKE 'daily-log:%')`——三值逻辑 + LIKE 元字符两个坑叠加。这条 SQL 扫 `lifecycle IN ('draft','planning')` 且 spec-harness-start 失败的 wave 去**重启 planner**；daily-log 恒为 draft，只要留下一条 failed op 就会被反复拉起 spec agent。
建议：一处判据 + 全仓 grep 门禁（`purpose` 的每个 `==`/`<>`/`LIKE` 必须过同一 helper 或在 allowlist 注册），PR1 加单违例 fixture：造一条 failed spec-harness-start op，断言 reaper **不**拉起它。

**M-2 —— 「日志文体契约前缀」在卡出生时写不进去。**
`card_create_with_id_tx` 对 `kind == "wave-report"` 有出生硬校验：`payload != WaveReportPayload::initial()` → BadRequest。既有先例 `seed_workflow_template_wave` 的真实形状是 create（拿 `initial()`）**再** `persist_report` 覆盖——**两个事务**。后果：(1) 两事务之间崩溃 → 这一天停在默认四段 H1 + REWRITE 契约上，与日志文体正相反；仓库为此专门写了 `restamp_template_report_if_placeholder`，daily-log 需要等价物而文档没有。(2) 播种后 `report_startup_read_required()` 恒为 true。

**M-3 —— `list_waves_window` 做索引也不合格。** daily-log 恒 `terminal_at IS NULL`（设计里没有任何一步推进生命周期，且 `WaveUpdated` 在 role_gate 里是 User/Kernel/AiSpec-only，assistant 推不动），`since` 分支恒真。查任何窗口都返回全部 42 条，真正的按日筛选落在客户端解析 `purpose` 字符串上——正是 §0 反对的那种搬运。建议专用端点直接 `purpose BETWEEN 'daily-log:<from>' AND 'daily-log:<to>'`（`YYYY-MM-DD` 字典序 = 时间序，这是选这个 key 格式唯一的实用理由，值得写进 D6）。

**M-4 —— `create_wave_structure(..., purpose: Option<&'static str>)`。** day key 是运行时算的，不是 `'static`。本身是小改，列出是因为它证伪了「每一条都是不做改动」：加上 M-1/M-2，D1 的实际改动面是 **5 处 purpose 判据 + 1 处签名 + 1 条 seed/restamp 流程 + 1 条新删除通道**。量级仍远小于 r2 的新 kind 方案（认同改判方向），但那张表是**过度推销**，应改成「改动更小且都在已有形状内」。

**M-5 —— 可见性清单缺一条真实跨面泄漏：report backlinks。** `WaveBacklink { src_wave_id, src_wave_title, ... }` 跨 wave 解析「谁链接到本 wave」，并把源 wave 标题放进用户 wave 的详情响应。daily-log 正文若引用某条用户 wave（这几乎是「今日进度」的定义），那条 wave 页面上就会冒出 `src_wave_title = "Daily log 2026-09-02"`。这证明 D8「是义务不是补丁」是对的，但也证明「一条覆盖全部读端点的扫地测试」写不出来——backlink 不是「返回一个 wave」，是「返回一个 wave 的标题」。

## MINOR

- **D4 少一条自反项。** daily-log 自己的 report 写入也是 `wave.report_edited`，于是活动计数会把「写今日进度」这个动作本身算进去。`EVENTS_PRUNE_KINDS` 里确实没有它（「永久」判断正确），所以不会自愈。建议 projection 显式排除 daily-log wave 自身的事件。
- **INV-007 只是「首次触发闸」。** 与上一条合起来，第一次写完后窗口永远非空。可接受，但 D5 要写明它不是持续闸。
- **§0 那条既有 bug 的修法有现成写法可抄。** `ensure_cove_chat_wave_inner` 用的是 `is_unique_constraint(&error, "waves.cove_id")` 列名形式。
- **资源节奏的完整账**（替换 D1 那句一行的「目录 + git init」）：每天 1 wave 行 + Spec 卡 + wave-report 卡 + 2 条 CardRoleCache + 1 个 managed 工作区（目录 + `git init` + init commit）+ 永久结构性事件；触发写入再加 1 张 Assistant 卡 + 1 行 `worker_sessions` + 1 行 `operations` + N 行 `harness_items`。`wave_delete_tx` 清 `wave_vcs_refs/commits`、`tasks`、`worker_sessions`，但**不清 `operations`**（全仓无 operations pruner）。所以 D6「删除不减少占用」是对的但偏乐观——加上 B-1，稳态是「什么都不减少」。

## 结论

改完 B-1、B-2、D1 表格降级 + M-1 的五处 carve-out 清单之后，PR1 可以开工，不需要第四轮全审，只需对改动段落做一次窄确认。
