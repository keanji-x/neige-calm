# Today 文档化 + AI 今日进度

> 状态：设计 **r4**（descope 后。用户 2026-09-02 裁决：砍掉跨天历史）。Issue：#1253。
> review 存档：`docs/_1253-design-review-{codex,subagent}[-r2|-r3].md`（三轮共六份）。
> 关联：#951（launchpad wave 与它的 report 卡）、#120（定时汇总与日程队列）、#1045、#1234。

## 0. r3 → r4：为什么砍掉跨天历史

三轮双通道 review，**三个载体，三次撞架构，而每次撞的都是同一个维度**：按日持久化 + 保留窗口 + GC。

- r1：写进 launchpad 的 `wave-report` → 撞**文体契约**（那张卡明写「当下快照，每次 REWRITE」，与按日累积相反）。
- r2：新 card kind `daily-log` → 撞**写通道**（`calm.report.blocks.*` 硬绑 `wave-report`，无 target-card 参数）与 **role_gate ∧ 唯一索引的钳形**（Assistant 只能写自己 home wave 的 ReportCard；一个 wave 只能有一张）。
- r3：每天一份 wave → 载体本身两通道实测成立，但撞**回收**：`delete_wave` 对整个 system cove 403，`workspace_recycle` guard 4 同样拒绝，而那条 403 的注释是 **2026-09-01 的裁决**，逐字反对本设计需要的豁免：

  > the alternative — carving out `purpose = launchpad` — puts an exception into "the system cove is kernel-owned". An invariant with an exception is the shape this design line keeps getting hurt by.

  再加上每天一条 assistant 会话 = 每天一个**可恢复的 harness session + 50ms tick run loop + 一条永久 operation**，N=42 就是 42 份常驻运行时。codex 通道的结论说得最直白：当前代码下「一年 365 个 git 仓库」**仍会发生**，因为 42 天策略根本没有执行器。

**而跨天历史是 r1 里我自己加进去的，不在原始需求里**（原话是「有一个 conversation 总结今天做了些什么」「是有个 AI 写今日进度」）。它是三次失败的唯一来源。用户 2026-09-02 裁决：**砍掉**。

r4 因此塌缩成架构本来就支持的形状：**Today 的文档就是 launchpad wave 自己那张 `wave-report` 卡，按它自带的契约用**——当下快照、四段、每次 REWRITE。它不是日志，所以 r1 的文体冲突不复存在。

### 随之消失的东西（不是「以后再做」，是本设计不再需要）

新 card kind、每天一条 wave、保留窗口与 GC、内核删除通道、时区派生的主键、五处 purpose 判据（含两处手写 SQL）、report 卡的 create-then-restamp 两事务、D8 的可见性义务（launchpad 在 system cove，`GET /api/coves` 已按 #175 过滤）、日历作为文档索引。

### 活下来的东西

Today 变文档形态（D2/D7）、AI 写今日进度（D5）、活动源与它的授权闸（D4，**仍是本设计唯一的风险面**）、读路径不得依赖 harness（§5.1）。

### 明确的损失

**没有跨天历史。**「我上周三干了什么」这一页答不出。日历回到它现在的用途（wave 活动 agenda），不再是文档索引。想要历史时另立项——那时的正确起点是 #120 的 schedule 线，而不是把历史塞进 report。

## 1. 问题

Today 只有「当前状态」，没有「发生了什么」。

`fe/web/src/features/today/public.tsx`（435 行）的主列是三段 `WaveRow` 列表——Waiting on you / Running / Recent——全部由同一份 `waves` 数组 filter 得到；中间夹着一个写死 `Terminal is not wired up yet.` 的占位 section。它现在的信息量约等于侧边栏加一个时钟。

## 2. 前提

**一、新 FE 尚未上生产**（沿用 `docs/1147-workspace-design.md`）。破坏性 DTO 变更可一步到位。
**二、老数据不迁移、不兼容**（用户 2026-08-31 定调）。
**三、不碰 Today terminal。** `features/today/README.md` 的 INV-TODAYTERM-* 原样保留。

> ⚠️ 前提二只豁免老数据。并发、崩溃后重跑、幂等、失败回滚仍是硬要求。

## 3. 已确认的既有事实

**每行标注载体依赖**——三轮里两次事故都是把某个载体下才为真的事实搬到了新载体上（r3 review 称之为「过度推销」，它在 r3 自己的对照表里又犯了一次，只是方向相反）。r4 的载体是 launchpad 现有的 `wave-report` 卡。

| 事实 | 位置 | 在 r4 载体下 |
|---|---|---|
| launchpad wave 的 ensure 端点存在且幂等；它已建了 `wave-report` 卡 | `routes/today.rs` | ✔ 直接可用 |
| **但 `ensure` 没有任何生产调用方**；且它会 materialize workspace 再 `submit("spec-harness-start")` 并 **`.wait()`** | 全仓 grep；`routes/today.rs` | ⚠ 见 §5.1：读路径不得走它 |
| `TodayLaunchpad` DTO **不含** `report_card_id` | `routes/today.rs::TodayLaunchpad` | ⚠ 见 §5.1 |
| system cove 默认不在 `GET /api/coves` 里（#175），FE workspace 扇出看不到它 | `routes/coves.rs::list_coves` | ✔ 可见性义务自动满足 |
| **写通道实测通**：`calm.report.blocks.*` 从 caller 卡的 `wave_id` 解析本 wave 的 `wave-report` 卡；全部 `require_role_any([Spec, Assistant])` | `mcp_server/tools/wave_report.rs::resolve_report_for_caller`、`wave_report_blocks.rs` | ✔ 零改动 |
| **role_gate 实测放行**：Assistant 可写 `self` 或 `cache.get(target) == ReportCard && wave_of(target) == home_wave`；两道 #232/#234 反欺骗天然满足（assistant 卡就生在这条 wave 上） | `calm-truth/src/role_gate.rs::enforce_assistant_scope` + `enforce_card_scope`（读的是实现不是注释） | ✔ 零改动 |
| `POST /api/waves/{id}/conversations` **必须带 `Idempotency-Key`**，语义「同 key = 同一条可重试草稿」，五个 arm | `routes/wave_conversations.rs` | ✔ |
| 该端点会铸 Assistant 卡 + 提交永久 operation + 等 session 起来；`idle` 仍算 active，boot 时会恢复所有 Assistant session，每个带 50ms tick run loop | `routes/wave_conversations.rs`、`session_projection.rs`、`harness/run_loop.rs` | ⚠ 见 D5：会话生命周期要裁决 |
| report 卡的文体契约是**当下快照 / 每次 REWRITE / 四个固定 H1 / 1000 字预算**，以注释形式种在 body 里，agent 读得到 | `calm-types/src/wave_report_contract_rules.md`、`wave_report_section_rules.md` | ✔ **r4 正是按它用** |
| 块渲染器是纯值注入（`report: WaveReport \| null`），`readWaveReport` 找 `kind === 'wave-report'` | `features/report/document/public.tsx`、`fe/core/domain/report.ts` | ✔ 直接可用 |
| `events` 有 `idx_events_at`，但全仓无按 `at` 的读者；`at` 是墙钟，`id` 才是游标 | `0004_events.sql` | ⚠ D4 是第一个 |
| 结构性事件永久；**`harness.item.added` 在 30 天 prune allowlist 里** | `docs/events-retention.md`、`events_prune.rs` | ⚠ D4 的 allowlist |
| `list_waves_window` 查的是**生命周期重叠**不是活动，且双端闭区间 | `routes/waves.rs::list_waves_window` | ✘ D4 不用它 |
| 现存最宽的 MCP 读是单 cove 的 `calm.cove.outline`，带 50 wave / 40 block / 32KB 三重截断 | `mcp_server/tools/report_links.rs` | ⚠ D4 的截断先例 |
| `today.rs::is_unique_constraint` 用**索引名**匹配错误消息，而 SQLite 报 `waves.purpose`；正确写法在 `ensure_cove_chat_wave_inner`（列名形式） | `routes/today.rs` vs `routes/waves.rs` | 既有 bug，顺手修 |

## 4. 决策

### D1 —— 载体：launchpad wave 现有的 `wave-report` 卡，按它自带的契约用

不新建 card kind，不新建 wave，不新建端点，不改 CRDT 缝，不动 role_gate，不加 purpose 判据。

**关键在于文体不是被迁就，而是本来就对**：这一页要答的是「现在什么状态、什么等我拍板、今天做成了什么、定了什么」——概要 / 待你定 / 已完成 / 决策，正是那四段。REWRITE 语义也对：每次汇总覆盖上一次，页面永远显示当下，不堆积。

**唯一要写下来的风险：两个写者。** launchpad wave 上已经有一个 spec agent（`ensure` 会起它）。#951 的 concierge 提案通道**已被回退**（migration 0065 建、0066 删），所以那个 spec agent 目前没有生产任务，两写者冲突是**潜在的而不是现行的**。但 #951 若复活，它和汇总 assistant 会写同一份 report。r4 的立场：**汇总的写者是 assistant 会话**（那正是用户要的「一个 conversation」），并把这条约束写进 #951 —— 谁复活 concierge，谁负责裁决两者如何分段。

### D2 —— Today 主体渲染这份 report；日历回到现在的用途

主体 = `ReportDocument` 渲染 launchpad 的 report。没有日期切换——文档只有一份，就是当下。

日历保持它现在的行为（按天显示 wave 活动 agenda），**不再承担文档索引**。r1–r3 里「日历第一次有了真正的用途」那句话随 descope 一起收回。

### D3 ~~块时间戳~~ / ~~按日分段~~ —— 删除（保留编号以免漂移）

### D4 —— 活动源：event-first 的服务端 projection + 一条只读 MCP 工具

这是 r4 里**唯一真正新增的内核面，也是唯一的风险面**。

**第一层（服务端 projection）**：`workspace_activity_window(start, end)`，按 `events.at` 聚合显式 kind allowlist，再 join 用户可见 cove/wave。

**不复用 `list_waves_window`**（它查的是生命周期重叠，不是活动：一条在窗口前就 terminal 的 wave，今天仍可能被编辑 report，而它不在那个候选集里）。

**allowlist 就地裁决**：

| kind | 计什么 | retention |
|---|---|---|
| `wave.lifecycle_changed` | 生命周期变迁数 | 永久（结构性） |
| `wave.report_edited` | 报告编辑数 | 永久（结构性） |
| `task.completed` / `task.failed` | 任务成败数 | 不在 prune allowlist 内 |

**`turns` 删除**：事实源 `harness.item.added` 在 30 天 prune allowlist 里，且正确计数要解析全部 `harness_items.params`（既有 conversation DTO 正因此拒绝提供它）。没有替代的永久事实源。

**自反排除**：汇总写入本身也是 `wave.report_edited`，所以 projection 必须**排除 launchpad wave 自身的事件**，否则「今天做了什么」会把「写今日进度」这个动作算进素材。

**窗口是半开区间 `[start, next_start)`**，按 `at` 查询，**不与 id 游标混用**（`0004_events.sql` 的警告抄进 doc comment）。

**第二层（MCP）**：`calm.day.activity` 是第一层的薄封装 + 授权闸，**复用同一个 repo/service helper，不重新实现查询**。边界写死：**复用查询函数，不复用 REST 的人向可见性与 session 门**——授权只走 `day_activity_allowed`。

**授权**（形状取自 #951 裁决，三轮 review 判定文字层已收敛）：descriptor `visible_to_roles: &[]`；`tools/list` 仅对已验证身份做 contextual augmentation；`tools/call` 独立重查；**同一个 async gate helper 调两次**。判据：role **仅 Assistant** + active session + wave 行存在 + `purpose == 'launchpad'` + cove/card 归属一致；unresolved / cross-session / dormant / missing row / DB error 一律拒。判据只从 `ToolCallIdentity` 推导，**绝不从 args 取**。

**参数**：MCP **不接受** `since/until`，服务端固定为「今天」（攻击面清零，且与「窗口至多一日」天然一致）。

**截断**：wave 数 / 每 wave 条目数 / 总字节三重，按字节回卷，纪律照抄 `report_links.rs`。

**时区**：日界仍需定义，但 descope 后它**不再是主键**，只是窗口边界。取服务端本地日；跨时区错位的代价写进 doc comment（单机 LAN 单用户下无害，「何时不再无害」要写清）。不引入 workspace timezone 设置——那是独立的产品决策，不该被本 issue 顺手绑架。

### D5 —— 手动触发；空活动不发起；会话生命周期就地裁决

「写今日进度」→ 在 launchpad wave 上起 assistant 会话（带 `Idempotency-Key`）→ prompt：`calm.day.activity` → 按四段契约 REWRITE 本 wave 的 report。

**空活动不发起**：判据用 D4 第一层的同一份 projection（不是 FE 的 waves 快照——后者只有当前 lifecycle，看不到 report 编辑、task 结果、历史 lifecycle 变迁）。这是「agent 不得编造进度」唯一可证死的形式：「必须诚实」证不了，「没有素材时根本没有调用」证得了。

**这是首次触发闸，不是持续闸**：第一次写完之后，`wave.report_edited` 会让窗口非空——但 D4 的自反排除正好抵消它，所以对 launchpad 自身的写入不会解除这个闸。两条规则要一起读。

**会话生命周期（r3 review 的 BLOCKER-2 在 r4 下缩小但没消失）**：descope 后不是每天一条会话，但反复点击仍会累积 assistant 会话，每条都是一个可恢复的 harness session + run loop + 永久 operation。**裁决：复用当天已有的那条汇总会话**（按 `Idempotency-Key` 的「同 key = 同一条可重试草稿」语义），不每次新建。会话对用户可读，出现在 Today 现有的 Conversations 模块里——它就是用户要的那个 conversation。

定时汇总属于 #120，不做。

### D6 ~~保留窗口与 GC~~ —— 删除

descope 的直接后果。文档只有一份且每次 REWRITE，不增长；1000 字预算由卡自带的契约管。

### D7 —— FE 形态：状态条在前，文档紧随

`fe-design.md` §8.1：用户打开这一页是要回答「有什么在等我？」，答案归位置和 `--warn` 像素，不归字号。状态条在前（三轮两通道一致推荐）——它高度 O(1)，不随内容增长把文档推出首屏；「文档是主角」由面积和视觉权重表达。

- 状态条：`N waiting · N running` + 等待中的 compact 行。
- 主体：`ReportDocument`。空态「还没有今日进度」+ 触发按钮（无活动时按钮不出现，D5）。
- 右面板：日历（现有行为）+ Running / Recent + 现有 Conversations 模块。
- terminal 占位 section 原样保留。

### D8 ~~可见性义务~~ —— 删除

不新增 wave，launchpad 已在 system cove 且被 #175 过滤。

## 5. 契约与不变量

### 5.1 解析链：读路径不得依赖 harness

三轮 review 里两个通道各自独立提出的同一条：`ensure_today_launchpad` 在事务之后还要 `materialize_workspace`，再 `submit("spec-harness-start")` 并 **`.wait()`**。把它放在页面加载路径上，等于 codex 不可用时 Today 整页硬失败——而现在的 Today 不需要它就能渲染。

```
GET /api/today/launchpad        （新增只读 resolve；404 = 还没有 → 空态，不是错误）
  → wave_id + report_card_id     （现有 DTO 不含后者，一并补上）
GET wave detail                 → 它的 wave-report 卡
[仅「写今日进度」动作] POST /api/today/launchpad/ensure
```

- 页面加载**只 resolve**，404 是正常空态。
- `ensure` 只挂在显式动作上；它的任何失败必须浮出为错误，不得静默重试或降级成空态（与 INV-TODAYTERM-001 同理：静默会把「读失败」变成「悄悄铸了第二份」）。
- 顺手修 `is_unique_constraint`：匹配 `waves.purpose` 而不是索引名，抄 `ensure_cove_chat_wave_inner` 的现成写法。

### 5.2 不变量表

| ID | 陈述（可证死的形式） | 反例（必须红） | 正例（必须绿） |
|---|---|---|---|
| INV-TODAYDOC-001 | 页面加载只走只读 resolve；`ensure` 只在显式动作上调用 | 加载路径触发 ensure | 加载只 resolve；404 → 空态 |
| INV-TODAYDOC-002 | 动作路径上 `ensure` 的任何失败浮出为错误，不静默降级 | 5xx 被吞成空态 | 5xx 浮出错误框 |
| INV-TODAYDOC-004 | `day_activity_allowed` 是唯一判据，当且仅当 predicate 成立时放行 | transport 任一分支（unresolved / cross-session / dormant / missing row / DB error）绕过 | 全分支 iff 断言 |
| INV-TODAYDOC-005 | 返回体字段是 allowlist（schema 层无 path / 无正文字段） | schema 出现 path 或正文字段 | 字段集合等于 allowlist |
| INV-TODAYDOC-006 | 活动窗口是半开区间；相邻两天不重复计数 | 午夜边界事件被两天各计一次 | 边界事件恰好计一次 |
| INV-TODAYDOC-007 | 活动窗口为空时不发起会话；且 projection 排除 launchpad 自身事件 | 空窗口起了会话；或自己的写入让窗口非空 | 空窗口直接空态 |
| INV-TODAYDOC-010 | 重复触发复用当天那条汇总会话，不累积 | 点三次留下三条 assistant 会话 | 三次都落到同一条 |

方法论备注（沿用前三轮裁决）：

- **INV-004 是全称否定**。身份空间不止四个 `CardRole`（还有 unresolved daemon、card-bound/no-thread、cross-session、dormant、missing row、DB error）。形式必须是「单一 predicate 的 iff 测试」+「transport 全分支集成测试」，不是列举几个身份。
- **INV-005 的值级「无绝对路径」不可证明**（合法 `title` 本身就可以是 `/home/x`），能证的是 schema 层字段 allowlist。

## 6. 切片计划

descope 后从三个 PR 缩到两个，且 PR1 完全不碰内核。

```
PR1（纯 FE + 一个只读端点） ──> PR2
```

| PR | 内容 | 交付 / 证死的风险 |
|---|---|---|
| **PR1**（~0.7k） | §5.1 只读 resolve 端点（+ DTO 补 `report_card_id`，+ 修 `is_unique_constraint`）+ D2/D7 FE：主体渲染 launchpad 的 report、状态条、空态、日历保持现状 | Today 变成文档形态。**读路径风险**用 INV-001/002 证死（加载不得触发 ensure）。手写 REST 块即可验，不依赖 AI |
| **PR2**（~0.9k） | D4 第一层 `workspace_activity_window`（event-first + allowlist + 半开区间 + 自反排除）+ 第二层 `calm.day.activity`（薄封装 + `day_activity_allowed` 单一 gate + 截断）+ D5 触发闭环（动作 + `Idempotency-Key` 复用 + prompt + 空活动不发起） | **授权风险**证死：gate 的 iff 测试 + transport 全分支矩阵；**边界风险**：INV-006 午夜用例；返回体 schema allowlist 断言；端到端：无活动时按钮不出现，点三次只有一条会话 |

## 7. 风险

- **`calm.day.activity` 是全 MCP 面第一个跨 cove 读**——是新类别不是更宽的工具（现存最宽是单 cove 的 `calm.cove.outline`）。#951 在同一位置踩过一次（当时「no role_gate change」的表述是半真且危险的）。PR2 按那份裁决逐条核对，不重新推导。
- **`workspace_activity_window` 是仓库里第一个按 `at` 的事件查询**。`0004_events.sql` 明确警告「`at` 是墙钟，`id` 才是游标，永不混用」——抄进 doc comment。
- **两个写者是潜在而非现行冲突**（D1）。#951 复活 concierge 时必须一并裁决。
- **`ensure` 是冷路径**（无生产调用方，且会 materialize + 等 harness）。PR1 只在动作路径上碰它，但那条路径要按新路径验，含并发撞唯一索引。
- **Tier-A 边界**：DTO 补 `report_card_id`、新增只读端点都会牵动 wire 与 goldens。按 `feedback_run_ci_exact_command_locally`，门禁跑 CI 完整命令（整个 workspace + features + web build），不是子集。

## 8. 明确不做

- **跨天历史**（本轮 descope 的核心；想要时另立项，起点是 #120 而不是 report）。
- 定时/自动汇总（→ #120）。
- Today terminal 接线（→ 现有 INV-TODAYTERM-*）。
- 手机端形态（→ #1234）。
- `turns` 计数（D4，无永久事实源）。
- workspace timezone 设置（D4，独立产品决策）。
- 汇总内容的质量评价。本设计只保证「没有素材时不写」（INV-007），不保证「有素材时写得好」。

## 9. 开放问题

前三轮的 Q1–Q5 随 descope 全部消解（日期身份、保留窗口、时区主键都不存在了）。剩下一条：

- **Q6**：D1 的两个写者。目前是潜在冲突（#951 的 concierge 通道已被 0066 回退），r4 的立场是「汇总写者 = assistant，谁复活 concierge 谁负责裁决分段」。这个立场应该只写在本 issue，还是该同时写进 #951？倾向后者——否则复活它的人读不到这条约束。
