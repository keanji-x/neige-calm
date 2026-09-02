# Today 文档化 + AI 今日进度

> 状态：设计 **r5**（四轮双通道 review 后。r4 的 descope 由用户 2026-09-02 裁决；r5 砍掉 MCP 工具那一层）。Issue：#1253。
> review 存档：`docs/_1253-design-review-{codex,subagent}[-r2|-r3|-r4].md`（四轮共八份）。
> 关联：#951（launchpad wave 与它的 report 卡；两写者约束已留注记）、#120（定时汇总与日程队列）、#1045、#1234。

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

## 0b. r4 → r5：第四轮改掉了什么

两个通道都判 fix-then-ship（codex 从 r3 的 4 个 BLOCKER 降到 1 个），载体不动，但 D4/D5 两块要段落级重写。

1. **`Idempotency-Key` 撑不起「复用当天那条汇总会话」，而且失败方式最坏——测试绿、功能坏。** `create_wave_conversation` 在 operation 成功后走 `user_message_already_enqueued`，**为真就跳过 `send_spec_input`**；arm (a) 的 API 文档写死 *"does not re-send the first message"*。所以同 key 第二次点击 = 不发消息、agent 不跑、report 不更新，但 HTTP 201、UI 正常，r4 的 INV-010 会**绿**。两通道独立同结论。→ **D5 重写**（§4 D5）。

2. **key 由谁生成 r4 根本没写**，而现有唯一生产者 `mintIdempotencyKey` 每次返回随机 v4，且落在 #1225 的丢失窗口里（路由级 `useReducer`，切走再回来必丢）。另外 arm (e) 是同 key + 不同 `text` → 409，所以 prompt 文本必须逐字节稳定。→ key 改为**确定性派生**，prompt 稳定性配断言。

3. **INV-007「空活动不发起」没有执行点。** D4 第一层是内部 helper，第二层是 MCP 面（只有 agent 能调），FE 没有任何 REST 读得到它。于是这条只能靠「FE 藏按钮」——绕过按钮直接 POST 就发起了。这正是本仓库记过的 `statement_widened_past_carrier`。→ **闸下沉到服务端**（§4 D5）。

4. **`visible_to_roles: &[]` + tools/list contextual augmentation 这个形状，在内置工具上不存在。** `registry.rs::descriptors_for_role` 就是 `filter(|d| d.visible_to_roles.contains(&role))`，`&[]` 对任何角色永不出现，没有 per-identity 加回的钩子；唯一的 augmentation 缝只处理**插件**工具。现有 `&[]` 内置工具（`tools/wave_history.rs`）的头注释也说明了用途：那是给 CLI 调的，不是给模型调的。**这是同一个病的第四次发作**——把 #951 关于插件/concierge 工具的裁决搬到了内置工具上。

   > 两通道在这里表面分歧、实则不矛盾：codex 说「descriptor 隐藏 + gate 双重检查下 fail-closed」，讲的是**安全**，成立；subagent 说模型看不见，讲的是**可发现性**，也成立。一个安全但不可发现的工具，prompt 里让它调是碰运气。

   → **D4 第二层整块删除**（§4 D4）。

5. **`readWaveReport` 对 canonical 初始 report 返回非空**，因为 `initial()` 的 body 含契约注释和四个 H1，字符串非空；而 `ReportDocument` 只在 `report === null` 时渲染空态。所以 r4 的 D7「还没有今日进度 + 触发按钮」**永远不会出现**。→ 空态判据改为服务端计算的 `report_started`，复用 `report_startup_read_required()` 的 canonical 判据，**不在 FE 镜像初始 body 文本**。

6. **`report_card_id` 补进 DTO 是不必要的改动**：wave detail 已返回 cards，`readWaveReport` 自己按 kind 定位。→ 只读 resolve 用窄 DTO `{ wave_id, report_started }`，ensure 的 DTO 不动。

7. **自反排除当前不是 load-bearing**：launchpad 在 system cove，可见性 join 已经先滤掉它，删掉自反谓词也不会红。→ 降级为 defense-in-depth，并说明它在什么条件下才承重。

8. 两处小修正：`is_unique_constraint` 被按索引名调用的是**两处**（`idx_coves_one_system`、`idx_waves_one_launchpad`），不是一处；「全仓无按 `at` 的读者」不准确——`events_prune` 已经按 `at` DELETE，准确说法是「无按 `at` 的**读**路径」。

**r5 的净效果：`calm.day.activity`、`day_activity_allowed`、截断纪律、INV-004、INV-005 全部删除，§7 里自认的最大风险（全 MCP 面第一个跨 cove 读）随之消失，PR2 缩小约一半。**

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
| `events` 有 `idx_events_at`；全仓无按 `at` 的**读**路径（唯一既有用户是 retention 的 DELETE）；`at` 是墙钟，`id` 才是游标 | `0004_events.sql`、`events_prune.rs` | ⚠ D4 是第一个读路径 |
| `events` 的 **scope 列**（`scope_kind/cove/wave/card` + 两个 partial index）是 D4 的地基：`WaveReportEdited` 明写 `scope_wave = wave_id`；`task.completed/failed` 经 `emit.rs` 走 card scope | `0007_events_scope.sql`、`calm-types/src/event.rs` | ✔ D4 可行 |
| 但 `0007` 注释写明：老行一律 `scope_kind='system'` 且**不做 payload 回填** | `0007_events_scope.sql` | ⚠ 跨升级点的窗口会静默漏事件 |
| `readWaveReport` 对 canonical 初始 report 返回**非空**（`initial()` 的 body 含契约注释 + 四个 H1，非空字符串）；`ReportDocument` 只在 `report === null` 时渲染空态 | `fe/core/domain/report.ts`、`features/report/document/public.tsx`、`calm-types/src/wave_report.rs::initial_body` | ⚠ 空态判据不能靠它，见 D7 |
| `report_startup_read_required()` 是 canonical 的「写过没有」判据（与 `initial()` 逐字比较） | `calm-types/src/wave_report.rs` | ✔ D7 的 `report_started` 用它 |
| conversation-create 成功后走 `user_message_already_enqueued`，**为真就跳过 `send_spec_input`**；arm (a) 明写「不重新发送首条消息」 | `routes/conversations_shared.rs`、`routes/wave_conversations.rs` | ⚠ **D5 的重跑不能走它** |
| `mintIdempotencyKey` 每次返回随机 v4；#1225 确认 key 存在路由级 `useReducer` 里，切走再回来必丢 | `app/router/idempotency-key.ts`；#1225 | ⚠ D5 的 key 必须确定性派生 |
| 内置工具的 `tools/list` 可见性是 `filter(|d| d.visible_to_roles.contains(&role))`，**`&[]` 对任何角色都不可见**；唯一的 per-identity augmentation 缝只处理**插件**工具 | `mcp_server/registry.rs::descriptors_for_role`、`transport.rs::extend_plugin_tool_descriptors_for_role` | ✘ **r4 的 MCP 层因此删除** |
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

### D4 —— 活动源：一个服务端 projection，**没有 MCP 工具**

r5 删掉了 r4 的第二层（见 §0b.4）。活动**不由 agent 去查**，而是由服务端算好、注入 prompt。

理由：参数已写死「今天」、无 args、只读、单调用方——做成 MCP 工具买不到任何东西，却买来「全 MCP 面第一个跨 cove 读」这个新类别，以及一个模型根本看不见的 descriptor。删掉它，`day_activity_allowed`、截断纪律、INV-004、INV-005 一并消失。

**唯一一层（服务端 projection）**：`workspace_activity_window(start, end)`，按 `events.at` 聚合显式 kind allowlist，再 join 用户可见 cove/wave。它有且只有一个调用方：D5 的触发端点。

**不复用 `list_waves_window`**（它查的是生命周期重叠，不是活动：一条在窗口前就 terminal 的 wave，今天仍可能被编辑 report，而它不在那个候选集里）。

**allowlist 就地裁决**：

| kind | 计什么 | retention |
|---|---|---|
| `wave.lifecycle_changed` | 生命周期变迁数 | 永久（结构性） |
| `wave.report_edited` | 报告编辑数 | 永久（结构性） |
| `task.completed` / `task.failed` | 任务成败数 | 不在 prune allowlist 内 |

**`turns` 删除**：事实源 `harness.item.added` 在 30 天 prune allowlist 里，且正确计数要解析全部 `harness_items.params`（既有 conversation DTO 正因此拒绝提供它）。没有替代的永久事实源。

**自反排除（defense-in-depth，不是承重墙）**：汇总写入本身也是 `wave.report_edited`，所以 projection 排除 launchpad wave 自身的事件。但要写明它**当前不承重**：launchpad 在 system cove，可见性 join 已经先滤掉它，删掉这条谓词也不会有测试变红。它承重的条件是「哪天可见性 join 放宽了，或 launchpad 搬出 system cove」。要么按 defense-in-depth 写，要么把测试打在**可见性 join 之前的 raw allowlisted rows** 上——两者选一，别写成「必须」却证不死。

顺带：自反排除按 **wave** 粒度，所以用户**手改** Today report 也不算活动。无害，但 INV-007 的正例要按这个写。

**窗口是半开区间 `[start, next_start)`**，按 `at` 查询，**不与 id 游标混用**（`0004_events.sql` 的警告抄进 doc comment）。另一条要抄的：`0007_events_scope.sql` 明写老行一律 `scope_kind='system'` 且**不做 payload 回填**，所以跨过升级点的窗口会静默漏掉老事件。

**时区**：日界仍需定义，但 descope 后它**不再是主键**，只是窗口边界。取服务端本地日；跨时区错位的代价写进 doc comment（单机 LAN 单用户下无害，「何时不再无害」要写清）。不引入 workspace timezone 设置——那是独立的产品决策，不该被本 issue 顺手绑架。

### D5 —— 触发：一个服务端合成的专用动作；创建与重跑是两条路径

r4 把「复用当天那条会话」压在 `Idempotency-Key` 上，两个通道独立证明那会让第二次点击变成静默 no-op（§0b.1）。r5 重写。

**端点**：`POST /api/today/summary`（服务端合成，不接受客户端 prompt）。它按顺序做四件事：

1. 算 `workspace_activity_window(今天)`；
2. **活动为空 → 拒绝**（`409` 或 `204`，不发起任何会话）。这是 INV-007 的**执行点**，落在服务端而不是「FE 藏按钮」上——绕过按钮直接 POST 也拒；
3. 把活动摘要**注入 prompt**（不是让 agent 去查，见 D4）；
4. 分派到下面两条路径之一。

**创建与重跑是两条路径，不能合成一条**：

| 情形 | 走什么 | 为什么 |
|---|---|---|
| 汇总会话**尚不存在** | `POST /api/waves/{launchpad}/conversations`，`Idempotency-Key` **确定性派生**（如 `today-summary`） | key 必须是纯函数：现有 `mintIdempotencyKey` 每次返回随机 v4，且落在 #1225 的丢失窗口里（路由级 `useReducer`，切走再回来必丢）。确定性 key 自动绕开 #1225 |
| 会话**已存在**（第二次及以后） | resolve `derive_wave_conversation_keys(wave_id, key).card_id`，然后 `POST /api/cards/{id}/spec/input` | conversation-create 在成功后走 `user_message_already_enqueued`，为真就**跳过** `send_spec_input`；arm (a) 明写「不重新发送首条消息」。想让 agent 真的再跑一轮，只能走 spec input |

**会话生命周期裁决**：**launchpad 全生命周期只有一条汇总 conversation**（不按日分键）。r4 写「不是每天一条会话」却又按天分键，是自相矛盾；按天分键的稳态与 r3 判死过的那条论证逐字相同（每条 = 一个可恢复 harness session + 50ms run loop + 一条永久 operation，而 `operations` 全仓无 pruner），只是没有 wave 和工作区。单条会话的代价是 transcript 无界增长，用既有的 `reset_harness_items` / force-new-thread 控制。

**prompt 文本必须逐字节稳定**（仅在创建那一次）：arm (e) 是同 key + 不同 `text` → 409 `conflict`（text 以 SHA-256 绑进 operation payload）。所以**创建用的首条消息不得内嵌日期、活动摘要或时间戳**——活动摘要走重跑路径的 spec input，不走创建。这条要配断言。

**会话对用户可读**，出现在 Today 现有的 Conversations 模块里——它就是用户要的那个 conversation。

定时汇总属于 #120，不做。

### D6 ~~保留窗口与 GC~~ —— 删除

descope 的直接后果。文档只有一份且每次 REWRITE，不增长；1000 字预算由卡自带的契约管。

### D7 —— FE 形态：状态条在前，文档紧随

`fe-design.md` §8.1：用户打开这一页是要回答「有什么在等我？」，答案归位置和 `--warn` 像素，不归字号。状态条在前（三轮两通道一致推荐）——它高度 O(1)，不随内容增长把文档推出首屏；「文档是主角」由面积和视觉权重表达。

- 状态条：`N waiting · N running` + 等待中的 compact 行。
- 主体：`ReportDocument`。空态「还没有今日进度」+ 触发按钮。

  **空态判据是服务端的 `report_started`，不是「report 为空」。** `readWaveReport` 对 canonical 初始 report 返回**非空**——`initial()` 的 body 含契约注释和四个 H1，字符串非空——而 `ReportDocument` 只在 `report === null` 时渲染空态。所以照 r4 的写法，空态永远不会出现，用户第一次打开看到的是四个空标题。判据用 §5.1 resolve 返回的 `report_started`，它在服务端复用 `report_startup_read_required()` 的 canonical 判据；**不在 FE 镜像初始 body 文本**（那是 mirror code）。

  按钮在无活动时不出现，但那只是 UI；真正的闸在服务端（D5）。
- 右面板：日历（现有行为）+ Running / Recent + 现有 Conversations 模块。
- terminal 占位 section 原样保留。

### D8 ~~可见性义务~~ —— 删除

不新增 wave，launchpad 已在 system cove 且被 #175 过滤。

## 5. 契约与不变量

### 5.1 解析链：读路径不得依赖 harness

三轮 review 里两个通道各自独立提出的同一条：`ensure_today_launchpad` 在事务之后还要 `materialize_workspace`，再 `submit("spec-harness-start")` 并 **`.wait()`**。把它放在页面加载路径上，等于 codex 不可用时 Today 整页硬失败——而现在的 Today 不需要它就能渲染。

```
GET /api/today/launchpad        （新增只读 resolve；404 = 还没有 → 空态，不是错误）
  → { wave_id, report_started }  （窄 DTO；ensure 的 TodayLaunchpad 不动）
GET wave detail                 → readWaveReport 自己按 kind 定位那张卡
[仅「写今日进度」动作] POST /api/today/summary  → 内部按需 ensure
```

- 页面加载**只 resolve**，404 是正常空态。
- **不给 `TodayLaunchpad` 加 `report_card_id`**：wave detail 已返回 cards，`readWaveReport` 自己按 `kind === 'wave-report'` 定位，那个字段没有消费者。新增的是**窄只读 DTO**，它带的 `report_started` 才是 FE 真正缺的东西（D7）。所以 D1 的「不新建端点」精确表述为：**不新增写/CRDT 端点，只新增一个只读 resolve 和一个触发动作**。
- resolve 会看到 `ensure` 未走完的中间态（wave 行在、report 卡还没建，或 adopt-legacy 的旧 `Today` wave）。**裁决：那算 404**（「wave 存在但无 report 卡」= 还没有，走空态），不引入 `Option<report_card_id>` 给解析链加分支。
- `ensure` 只挂在显式动作上；它的任何失败必须浮出为错误，不得静默重试或降级成空态（与 INV-TODAYTERM-001 同理：静默会把「读失败」变成「悄悄铸了第二份」）。
- 顺手修 `is_unique_constraint`：它在 `today.rs` 里被按**索引名**调用了**两处**——`idx_coves_one_system` 与 `idx_waves_one_launchpad`——而 SQLite 报的是 `coves.kind` / `waves.purpose`。两处一起改成列名形式，抄 `routes/waves.rs::ensure_cove_chat_wave_inner` 的现成写法。前者恰好在 system cove 首次并发 mint 的那个 race 上，正是 §7 要求验的路径。

### 5.2 不变量表

| ID | 陈述（可证死的形式） | 反例（必须红） | 正例（必须绿） |
|---|---|---|---|
| INV-TODAYDOC-001 | 页面加载只走只读 resolve；`ensure` 只在显式动作上调用 | 加载路径触发 ensure | 加载只 resolve；404 → 空态 |
| INV-TODAYDOC-002 | 动作路径上 `ensure` 的任何失败浮出为错误，不静默降级 | 5xx 被吞成空态 | 5xx 浮出错误框 |
| INV-TODAYDOC-003 | 空态判据是服务端 `report_started`；FE 不解析 report body 文本 | canonical 初始 report 下渲染出四个空标题而非空态；或 FE 出现按 body 文本判断的分支 | 从未汇总 → 空态 + 按钮 |
| ~~INV-TODAYDOC-004~~ | ~~`day_activity_allowed`~~ | 随 D4 第二层一并删除（§0b.4） | — |
| ~~INV-TODAYDOC-005~~ | ~~返回体字段 allowlist~~ | 随 D4 第二层一并删除 | — |
| INV-TODAYDOC-006 | 活动窗口是半开区间；相邻两天不重复计数 | 午夜边界事件被两天各计一次 | 边界事件恰好计一次 |
| INV-TODAYDOC-007 | **服务端**在活动窗口为空时拒绝发起（不是 FE 藏按钮） | 绕过按钮直接 POST 触发端点、空窗口仍起了会话 | 空窗口 → 409/204，无会话、无 turn |
| INV-TODAYDOC-010 | 汇总会话全生命周期只有一条；且**每次触发都真的产生一次新的 agent turn** | 点三次留下三条会话；**或**点三次只有一条会话但只跑了一轮 | 一条会话，三次 turn |

方法论备注：

- **INV-010 的第二个反例是 r4 的教训**：r4 只写了「三次落到同一条」，而那条在 `Idempotency-Key` 实现下会**绿着骗人**——第二次 HTTP 201、会话不变、agent 根本没跑。一条不变量如果只约束「不多」不约束「有效」，它证死的是错的东西。
- **INV-003 防的是 mirror code**：`initial()` 的 body 文本是内核事实，FE 复述它必然产错（`feedback_mirror_code_must_call_the_original`）。判据必须来自服务端调用 `report_startup_read_required()`。
- **自反排除不进不变量表**：它当前不承重（可见性 join 已先滤掉 system cove），写成「必须」会是一条删掉也不红的假不变量。见 D4。

## 6. 切片计划

descope 后从三个 PR 缩到两个，且 PR1 完全不碰内核。

```
PR1（纯 FE + 一个只读端点） ──> PR2
```

| PR | 内容 | 交付 / 证死的风险 |
|---|---|---|
| **PR1**（~0.7k） | §5.1 窄只读 resolve 端点（`{wave_id, report_started}`；中间态算 404；修**两处** `is_unique_constraint`）+ D2/D7 FE：主体渲染 launchpad 的 report、状态条、空态、日历保持现状 | Today 变成文档形态。**读路径风险**用 INV-001/002 证死（加载不得触发 ensure）；**空态风险**用 INV-003 证死（canonical 初始 report 必须渲染空态而不是四个空标题）。手写 REST 块即可验，不依赖 AI |
| **PR2**（~0.6k） | D4 `workspace_activity_window`（event-first + allowlist + 半开区间 + 自反排除）+ D5 `POST /api/today/summary`（服务端空活动闸 + prompt 注入 + 创建/重跑两条路径 + 确定性 key） | **闸的风险**用 INV-007 证死（绕过按钮直接 POST，空窗口必须拒）；**边界风险**：INV-006 午夜用例；**有效性风险**用 INV-010 的第二个反例证死（点三次 → 一条会话、三次 turn）|

PR2 比 r4 小了约三分之一：删掉 MCP 工具那一层，连带 `day_activity_allowed`、截断纪律和两条不变量一起消失。

## 7. 风险

> **r4 的头号风险已经不存在了。** 那条是「`calm.day.activity` 是全 MCP 面第一个跨 cove 读，是一个新类别」——r5 删掉了那一层（§0b.4），风险随之消失，而不是被缓解。这是删掉它最大的收益。

- **`workspace_activity_window` 是仓库里第一个按 `at` 的事件**读**查询**（`events_prune` 已经按 `at` DELETE，所以准确说法是第一个读路径）。`0004_events.sql` 明确警告「`at` 是墙钟，`id` 才是游标，永不混用」；`0007_events_scope.sql` 又写明老行一律 `scope_kind='system'` 且不回填。两条都抄进 doc comment。
- **单条常驻汇总会话的 transcript 无界增长**（D5）。用既有的 `reset_harness_items` / force-new-thread 控制，但要在实现里明确触发条件，否则它会长到把每轮 prompt 撑爆。
- **两个写者是潜在而非现行冲突**（D1）。约束注记已留在 #951；REWRITE 语义下冲突表现为**静默丢内容、不报错**，所以复活 concierge 的人必须先裁决分段。
- **`ensure` 是冷路径**（无生产调用方，且会 materialize + 等 harness）。PR1 不在读路径上碰它；PR2 的触发端点会碰，那条路径要按新路径验，含并发撞唯一索引——`is_unique_constraint` 的两处按索引名匹配正好在这条路上。
- **Tier-A 边界**：新增只读端点与触发端点都会牵动 wire 与 goldens。按 `feedback_run_ci_exact_command_locally`，门禁跑 CI 完整命令（整个 workspace + features + web build），不是子集。

## 8. 明确不做

- **跨天历史**（本轮 descope 的核心；想要时另立项，起点是 #120 而不是 report）。
- 定时/自动汇总（→ #120）。
- Today terminal 接线（→ 现有 INV-TODAYTERM-*）。
- 手机端形态（→ #1234）。
- `turns` 计数（D4，无永久事实源）。
- workspace timezone 设置（D4，独立产品决策）。
- **任何 MCP 工具**（§0b.4：删掉的那一层不要在实现时又长回来）。
- 汇总内容的质量评价。本设计只保证「没有素材时不写」（INV-007），不保证「有素材时写得好」。

## 9. 开放问题

**无。**

前四轮的 Q1–Q5 随 descope 全部消解（日期身份、保留窗口、时区主键都不存在了）。**Q6 已定**：汇总写者 = assistant，谁复活 concierge 谁负责裁决分段；这条约束**已同时写进 #951**（issue 注记，含「REWRITE 语义下冲突静默丢内容不报错」这个理由），而不是只留在本 issue——否则复活它的人读不到。

## 10. 四轮 review 的账

留给下一个读这份文档的人，因为过程本身是结论的一部分。

| 轮次 | 载体 | 被什么推翻 |
|---|---|---|
| r1 | 写进 launchpad 的 `wave-report` | 文体契约：那张卡明写「当下快照，每次 REWRITE」 |
| r2 | 新 card kind `daily-log` | 写通道硬绑 `wave-report`；role_gate ∧ 唯一索引的钳形 |
| r3 | 每天一份 wave | system cove 不可删 → GC 无执行器；每天 +1 条常驻 harness |
| r4 | 回到 launchpad 的卡（descope 掉历史） | `Idempotency-Key` 撑不起重跑；MCP 工具模型看不见 |
| r5 | 同 r4，删掉 MCP 层 | — |

**同一个错犯了四次**：把某个上下文下为真的结论搬到新上下文里，不重验。r2 搬的是 r1 的「AI 写块无需内核改动」；r3 反向过度纠正成「每一条都是不做改动」；r4 把 #951 关于**插件**工具的可见性裁决搬到了**内置**工具上。教训已写进 `feedback_facts_dont_survive_carrier_swap`：换载体时事实表**逐行**重验，并标注每条绑定在哪个载体上——§3 的最后一列就是这么来的。

两条更普适的：

- **「不做改动」和「已确认」是同一个病的两个方向。** 过度推销零成本和搬运旧结论一样危险。
- **一条只约束「不多」不约束「有效」的不变量会绿着骗人。** INV-010 的第二个反例（点三次必须真的跑三轮）就是这么补上的。
