# Today 文档化 + AI 今日进度

> 状态：设计 **r11 —— 已收敛，含 PR1 实现期修订**（§5.1 的 `is_unique_constraint` 两处不对称、D7 的 O(1) 上限、INV-002 三态、§6 的 PR2 失效链）。原 r10：七轮双通道 review；最后一轮两个通道均判零 BLOCKER（各自指出的唯一一条已在 r9 修掉，两边都确认）。Issue：#1253。
> review 存档：`docs/_1253-design-review-{codex,subagent}[-r2|…|-r7].md`。
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

5. **`readWaveReport` 对 canonical 初始 report 返回非空**，因为 `initial()` 的 body 含契约注释和四个 H1，字符串非空；而 `ReportDocument` 只在 `report === null` 时渲染空态。所以 r4 的 D7「还没有今日进度 + 触发按钮」**永远不会出现**。→ 空态判据改为服务端计算的 `report_has_noninitial_content`，复用 `report_startup_read_required()` 的 canonical 判据，**不在 FE 镜像初始 body 文本**。

6. **`report_card_id` 补进 DTO 是不必要的改动**：wave detail 已返回 cards，`readWaveReport` 自己按 kind 定位。→ 只读 resolve 用窄 DTO `{ wave_id, report_has_noninitial_content }`，ensure 的 DTO 不动。

7. **自反排除当前不是 load-bearing**：launchpad 在 system cove，可见性 join 已经先滤掉它，删掉自反谓词也不会红。→ 降级为 defense-in-depth，并说明它在什么条件下才承重。

8. 两处小修正：`is_unique_constraint` 被按索引名调用的是**两处**（`idx_coves_one_system`、`idx_waves_one_launchpad`），不是一处；「全仓无按 `at` 的读者」不准确——`events_prune` 已经按 `at` DELETE，准确说法是「无按 `at` 的**读**路径」。

**r5 的净效果：`calm.day.activity`、`day_activity_allowed`、截断纪律、INV-004、INV-005 全部删除，§7 里自认的最大风险（全 MCP 面第一个跨 cove 读）随之消失，PR2 缩小约一半。**

## 0c. r5 → r6：第五轮改掉了什么

两通道均判 fix-then-ship，载体、D4 单层 projection、D7 判据、两处 `is_unique_constraint` 全部经代码验证成立。改的都是 D5 与不变量的**证明层**。

1. **r5 的 D5 自相矛盾，第一次点击拿不到任何活动数据**（两通道独立同结论）。r5 说「摘要注入 prompt」，又说「创建那条消息不得含摘要，摘要走重跑分支」——而 create 的 `text` 是唯一送达 agent 的东西（成功后立即 `send_spec_input`，此后 `user_message_already_enqueued` 保证不再补发），且 r5 已经删掉了 agent 侧的查询能力。**结果是首次使用必然产出无素材的汇总，而那是用户唯一会看的第一印象。** → 改成「按需先建（静态 bootstrap），然后**一律** spec input」，一条路径不是两条。

2. **「点三次跑三轮」被 harness 设计证伪。** `maybe_issue_turn` 一次 `drain` 把整个 pending 队列拼成一条只发**一个** `turn_start`，还有 250ms/5s 去抖。连点三次的典型结果是 **2 个 turn**。→ INV-010 换证明层：改证每次触发留下一条永久的 `harness.user_message.enqueued`，**明确接受合并 turn**。

3. **INV-007 又一次写宽过载体（同一条老病的第五次发作）。** 闸只在新端点里，用户仍可直接打 conversations 或 spec_input。→ 收窄成「**这个端点**在空窗口下既不建会话也不发消息」，并写明另两条路故意不在射程内。§8 同步改。

4. **「全生命周期只有一条会话」证不了**（内核对每 wave 的 assistant 会话数无上界，Today 的 Conversations 模块本来就有 `+`）。→ 换成 INV-011：固定 key 下派生 card_id 恒定，golden 钉住。

5. **确定性 key 会复活一个已记录在案的事故。** payload hash 含 `actor` 与 `cwd`，`insert_operation` 对「同 key + 不同 hash」**永久 409**（`operations` 无 pruner）——`today.rs` 逐字记过 *"409, on every request, forever"*，当时的解法就是把 workspace digest 掺进 key。→ actor 固定、key 掺 `workspace_key_digest`、加 409 兜底。

6. **dormant 会让按钮永久死掉。** 在「只有一条会话」下，一次 `spec_harness_dormant` 之后只能靠人手 reset。→ D5 写明恢复规则并挂进 INV-002。

7. **`report_startup_read_required()` 答的问题和文档说的不是同一个**（它是「被任何人写过没有」，不是「今日汇总跑过没有」；只比 summary+body）。好消息是 blocks-only 写**不会**漏判（每次落库都从 CRDT 重投影 body）。→ 写明这层近似 + 补反方向断言。

8. 两处论证降级：「中间态 404」的理由不成立（同事务建卡，那个状态不可达），404 保留但理由改成「便宜且 fail-closed」；`is_unique_constraint` 的修复**是行为变更**（死代码 → 重试成功），必须配真并发/故障注入用例。

9. 删掉截断纪律后 prompt 没有长度界 → D4 裁决 projection 为 **O(1) DTO**（32,768 字符硬上限就在那条路上）。另记一个更坏的模式：pending 队列满且折叠超限时观测被**直接丢弃**，只留 warn 日志。

## 1. 问题

Today 只有「当前状态」，没有「发生了什么」。

`fe/web/src/features/today/public.tsx`（435 行）的主列是三段 `WaveRow` 列表——Waiting on you / Running / Recent——全部由同一份 `waves` 数组 filter 得到；中间夹着一个写死 `Terminal is not wired up yet.` 的占位 section。它现在的信息量约等于侧边栏加一个时钟。

## 2. 前提

**一、新 FE 尚未上生产**（沿用 `docs/1147-workspace-design.md`）。破坏性 DTO 变更可一步到位。
**二、老数据不迁移、不兼容**（用户 2026-08-31 定调）。
**三、不碰 Today terminal。** `features/today/README.md` 的 INV-TODAYTERM-* 原样保留。

> ⚠️ 前提二只豁免老数据。并发、崩溃后重跑、幂等、失败回滚仍是硬要求。

## 3. 已确认的既有事实

**每行标注载体依赖**——五轮里同一个错犯了四次：把某个上下文下才为真的结论搬到新上下文（详见 §10）。载体是 launchpad 现有的 `wave-report` 卡。

| 事实 | 位置 | 在本载体下 |
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
| `report_startup_read_required()` 是 canonical 的「写过没有」判据（与 `initial()` 逐字比较） | `calm-types/src/wave_report.rs` | ✔ D7 的 `report_has_noninitial_content` 用它 |
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

**投影是 O(1) 的，因此 prompt 有可证明的长度界。** 删掉 MCP 层时把截断纪律一起删了，但注入 prompt 这条路仍有硬上限：`validate_first_message` 与 `spec/input` 都拒绝超过 **32,768 Unicode 字符**（`MAX_SPEC_INPUT_CHARS`）。裁决：**projection 只返回计数，不返回任何可变长字符串**——每个 allowlist kind 一个整数，外加受影响 wave 的数量。没有 wave 标题、没有 cove 名、没有明细列表。这样 prompt 长度是模板文本 + 几个整数，可以静态算出上界，无需字符预算也无需边界用例。

> r6 写的是「最多 N 条 wave 级明细，N 写死」——那只限行数不限长度，固定条数的可变长字符串仍然证不出 ≤32,768。若将来确实要明细，必须同时给出覆盖模板本身的确定性字符预算 + 32,768 / 32,769 / CJK 边界用例；在那之前不做。

**时区**：日界仍需定义，但 descope 后它**不再是主键**，只是窗口边界。取服务端本地日；跨时区错位的代价写进 doc comment（单机 LAN 单用户下无害，「何时不再无害」要写清）。不引入 workspace timezone 设置——那是独立的产品决策，不该被本 issue 顺手绑架。

### D5 —— 触发：一个服务端合成的专用动作；按需先建，然后一律 spec input

r4 把「复用当天那条会话」压在 `Idempotency-Key` 上，两个通道独立证明那会让第二次点击变成静默 no-op（§0b.1）。r5 重写。

**端点**：`POST /api/today/summary`（服务端合成，不接受客户端 prompt）。它按顺序做四件事：

1. 算 `workspace_activity_window(今天)`；
2. **活动为空 → 拒绝**（`409` 或 `204`，不发起任何会话）。这是 INV-007 的**执行点**，落在服务端而不是「FE 藏按钮」上——绕过按钮直接打**这个端点**也拒（另两条通用写入口故意不在射程内，见 §5.2）；
3. 把活动摘要**注入 prompt**（不是让 agent 去查，见 D4）；
4. 走下面那条**单一**路径。

**「按需先建，然后一律 spec input」——不是两条并列分支**（r5 的写法两个通道都判 BLOCKER，见 §0c.1）：

```
card_id = derive_wave_conversation_keys(launchpad_wave, "today-summary").card_id
          ← 裸 key。见下：digest 绝不能掺进这里，它同时决定 card id
if card_get(card_id) 不存在:
    POST /api/waves/{launchpad}/conversations   ← 只发静态 bootstrap 文本
POST /api/cards/{card_id}/spec/input            ← 唯一的 prompt 通道，带活动摘要
```

- **创建那次只发静态 bootstrap**（常驻指令，不含日期/摘要/时间戳），因为 arm (e) 把 `text` 以 SHA-256 绑进 operation payload，动态文本会让重试 409。
- **然后无条件走一次 spec input**。r5 把摘要只交给「重跑」分支，而创建路径的 `text` 是唯一送达 agent 的东西（create 成功后立即 `send_spec_input`，此后 `user_message_already_enqueued` 保证不再补发）——所以 r5 的**第一次使用必然产出无素材的汇总**，而那正是用户唯一会看的第一印象。
- **分支判据必须是 `card_get(derived.card_id)`，不能靠列表或启发式**：`Stuck` 补偿会留下卡却没有 runtime，而且那张卡 `deletable: false` 用户删不掉；选错分支就直接撞下面的 dormant 死路。

**key 必须是裸 `today-summary`，_不_ 掺 workspace digest——r6 在这里判反了，r8 改回来。** 创建路径把整个 `SpecHarnessStartOperationPayload` 送进 `stable_payload_hash`，其中含 `actor` 与 `cwd: wave.workspace.path`；`insert_operation` 对「同 key + 不同 payload_hash」**硬 409，而 `operations` 全仓无 pruner ⇒ 永久**。这正是 `today.rs` 里逐字记过的事故（*"409, on every request, forever"*），当时的解法是把 workspace digest 掺进 key。r6 照搬了那个解法——**但它搬不过来**：`derive_wave_conversation_keys` 用同一个 digest 同时喂 **card_id 和 operation_key**（`conversation_keys.rs` 的 doc comment 写死：card 是 `conv-{digest[..32]}`，operation key 是 `wave-conversation-{digest}`）。所以 key 掺了 cwd，**card id 也跟着变**：一次 re-point 就派生出第二张会话卡，直接推翻「全生命周期一条汇总 conversation」；而伪码若按裸 key 查卡、用 digest key 创建，创建出来的又不是查的那张。`today.rs` 的 key 不承担 conversation identity，这里的承担——所以那个解法在这条路径上是错的。

**正确的解法是把 payload 里的变量消掉，而不是把变量塞进 key。** `SpecHarnessStartOperationPayload` 有 **12 个字段**（穷举，别只数前几个）：`wave_id` 固定；`spec_card_id` 由裸 key 派生因而固定；`report_card_id` / `sort` / `goal` 是 `None`；`reset_harness_items: false`、`force_new_thread: true`、`profile: Assistant`、`create_card`（内含同一个裸 key）都是常量。所以只剩**三个**变量——

1. **`actor` 固定为单一值**，但理由要说准：`Actor::to_actor_id()` 把 `"user"` **和一切非 `ai:codex` 的值**都映射成 `ActorId::User`，而中间件只放行 `user` / `ai:<id>`。所以「owner/dev 两个人类账号各点一次」根本到不了——两者同为 `ActorId::User`。**真正的变量通道是客户端自带的 `X-Calm-Actor: ai:<id>`**，服务端合成路径不透传它即可消掉。
   **实现约束**：若要按 kernel 归属，**不能**经 `Actor::to_actor_id()`（`Actor("kernel").to_actor_id()` 会降为 `User`），必须直接构造 `ActorId::Kernel`。这条汇总记在谁头上，按 `identity_migration_attribution_scope` 裁决。
2. **`cwd` 只在 workspace re-point 时变**，而 re-point 之后我们**不会再调 create**——分支判据是 `card_get(derived.card_id)`，卡已存在就直接走 spec input，且那张卡 `deletable: false` 不会消失（`plan_compensation` 的注释逐字写明：补偿第一次出错就 `Stuck`、不再重驱，遗留的卡带 `deletable: false`）。所以「succeeded 之后 payload 变了」这个永久 409 的触发条件在本路径上到不了。
3. **`first_message_sha256`** —— 它由 D5 上一段的「bootstrap 文本必须逐字节静态」管住。这一条要和上面两条一起读：**三个变量各有各的约束，缺一条这个论证就不成立**。

**残余窗口**（写在明处，且比初稿更窄）：create 尝试过但**没有成功**（`Stuck`，arm (c) 持续 500）、**且补偿没有留下派生卡**，期间又发生了 re-point——此时同 key 不同 hash 会 409。`retryable_operation_key` 的 `#N` 逃逸只在 `phase == Failed` 时给，`Stuck` 不给。

> 「且没有留下卡」这个限定是必要的：若 `Stuck` 已经留下那张 `deletable:false` 的卡，`card_get` 分支就直接绕过 create 转入 dormant 恢复，根本不会再去比较旧 payload hash。所以真正的窗口是「Stuck ∧ 无卡 ∧ 期间 re-point」，窄且是 fail-closed 的已知态，接受；实现时写进错误文案，别让人误以为是 bug。

兜底仍然保留：**创建返回 409 conflict ⇒ resolve 派生卡 ⇒ 转 spec input**（若卡存在）。

**dormant 恢复规则（不写就等于按钮会永久死掉）：** `send_spec_input` → `ensure_live_spec_harness` 有四种失败：无 active runtime / 无 thread / snapshot 损坏 → **409 `spec_harness_dormant`**；`Starting` → 503；共享 app-server 未运行 → 503；`observe` 的有界 `try_send`（`OBSERVATION_BUFFER = 256`）→ 503。在「全生命周期只有一条会话」的裁决下，**一次 dormant 就让这个按钮永久失效**，只能靠人手 reset。**裁决：重新提交一次 `spec-harness-start`，不要走 `/spec/reset`。** 两者不等价，而差别是用户可见的——`reset_spec_harness_card` 写死 `reset_harness_items: true`，会**清空这条会话的 transcript**，而这条会话正是用户要看的那个 conversation。直接提交 start（`reset_harness_items: false`、`force_new_thread: true`、`create_card: None`、新 operation_key）能恢复而不擦历史；它也不会被幂等短路，因为 reset 路径用的是 `operation_key: new_id()` + `idempotency_key: None`。503 类按可重试错误浮出。这条挂进 INV-002。

**会话生命周期裁决**：**launchpad 全生命周期只有一条汇总 conversation**（不按日分键）。按天分键的稳态与 r3 判死过的论证逐字相同（每条 = 一个可恢复 harness session + 50ms run loop + 一条永久 operation），只是没有 wave 和工作区。单条会话的代价是 transcript 无界增长，用既有的 `reset_harness_items` / force-new-thread 控制。

> 注意这条**不是**「内核保证只有一条」——内核对每 wave 的 assistant 会话数没有任何上界，任意 key 都派生新卡，而 Today 的 Conversations 模块本来就有 `+`。可证的是纯函数性质，不是数据库计数，见 INV-010。

**触发不保证一轮一 turn。** `run_loop::maybe_issue_turn` 一次 `drain` 把整个 pending 队列拼成一条 `joined_observation_text` 只发**一个** `turn_start`。所以连点多次的典型结果是 turn 被合并。设计**接受合并**，INV-010 因此改证「每次触发都留下一条永久的 `harness.user_message.enqueued`」，而不是数 turn（见 §5.2）。

> **`UserMessage` 是 hard-fire，绕过 250ms/5s 去抖**（r6 把去抖写成了合并的原因，那是错的）。合并的真正条件是**两条消息在同一次「可发起的 drain」时仍共同排队**——不限于同一次 50ms tick：harness 被状态阻塞时，跨多个 tick 的消息照样会被合并。反过来，若第一条赶在下一次可发起 drain 之前独自入队，**首次触发就会先跑一个 bootstrap-only 的 turn**。因此 bootstrap 文本必须写成「待命，收到指令前不要动 report」——一个无害的空转，而不是让 agent 在没有素材时先写一版。这条和 INV-007 是同一个目的。

> 还有一个更坏的模式要防：pending 队列满 256 且折叠后超过 `MAX_FOLDED_USER_MESSAGE_CHARS`（4×32768）时，观测会被**直接丢弃**，只留一条 warn 日志——「点了，什么都没发生，且没有错误返回」。这与 INV-010 要防的是同一类，实现时要让它至少可观测。

**会话对用户可读**，出现在 Today 现有的 Conversations 模块里——它就是用户要的那个 conversation。

定时汇总属于 #120，不做。

### D6 ~~保留窗口与 GC~~ —— 删除

descope 的直接后果。文档只有一份且每次 REWRITE，不增长；1000 字预算由卡自带的契约管。

### D7 —— FE 形态：状态条在前，文档紧随

`fe-design.md` §8.1：用户打开这一页是要回答「有什么在等我？」，答案归位置和 `--warn` 像素，不归字号。状态条在前（三轮两通道一致推荐）——它高度 O(1)，不随内容增长把文档推出首屏；「文档是主角」由面积和视觉权重表达。

> **r11：「高度 O(1)」不是描述，是必须被实现强制的约束。** PR1 的第一版把全部 waiting wave 无上限地渲染在文档之前——100 条 blocked wave 就把报告推出首屏，而 O(1) 恰恰是本节把状态条放在文档**前面**的**承重理由**。理由被抽掉，结论就不成立。**裁决：waiting 行数写死上限（PR1 取 5），溢出收进一个 `+M more waiting` 展开**；溢出的 wave 不得被丢弃——RUNNING / RECENT 都排除了已计入 waiting 的项，丢了就无处可达。上限本身要有断言，单行用例证不了这条性质。

> **承重的是「加载时有界」，不是「永远有界」（r11 二次确认轮裁决）。** 初稿写成「高度不增长的展开」，而实现里展开后是 N 行内联、无 `max-height`——评审把这条差异挂起来交给作者裁。**裁决：保持现状，改的是这里的措辞。** D7 用来论证「状态条放在文档之前」的性质是**打开页面那一刻**的高度有界；展开是用户主动发起且可收回的，那一刻文档被推下去是用户自己要的。把「永远有界」写进设计会逼出一个滚动容器，买不到对应的价值。边界行为两通道均已实测：0 条 → 整个 section 不渲染；5 条 → 5 行无控件；6 条 → 5 行 + `+1 more waiting`。

- 状态条：`N waiting · N running` + 等待中的 compact 行（**有上限**）。
- 主体：`ReportDocument`。空态「还没有今日进度」+ 触发按钮。

  **空态判据是服务端的 `report_has_noninitial_content`，不是「report 为空」。** `readWaveReport` 对 canonical 初始 report 返回**非空**——`initial()` 的 body 含契约注释和四个 H1，字符串非空——而 `ReportDocument` 只在 `report === null` 时渲染空态。所以照 r4 的写法，空态永远不会出现，用户第一次打开看到的是四个空标题。判据用 §5.1 resolve 返回的 `report_has_noninitial_content`，它在服务端复用 `report_startup_read_required()` 的 canonical 判据；**不在 FE 镜像初始 body 文本**（那是 mirror code）。

  **它是一个近似，要写明。** `report_startup_read_required()` 答的是「这份 report 被**任何人**写过没有」，不是「今日汇总跑过没有」——它只比较 `summary + body`，刻意忽略 `doc_rev` 与 `blocks`。所以汇总跑过一次之后空态永不回来，哪怕内容陈旧、或最后的写者是用户手改（甚至 D1 记的第二写者复活）。descope 之后这可以接受，**所以字段就叫 `report_has_noninitial_content`**——名字说的是它实际答的那个问题，不是「曾经汇总过」。若哪天真需要后者，得用持久 marker/event，不能复用这个 helper。

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
  → { wave_id, report_has_noninitial_content }  （窄 DTO；ensure 的 TodayLaunchpad 不动）
GET wave detail                 → readWaveReport 自己按 kind 定位那张卡
[仅「写今日进度」动作] POST /api/today/summary  → 内部按需 ensure
```

- 页面加载**只 resolve**，404 是正常空态。
- **不给 `TodayLaunchpad` 加 `report_card_id`**：wave detail 已返回 cards，`readWaveReport` 自己按 `kind === 'wave-report'` 定位，那个字段没有消费者。新增的是**窄只读 DTO**，它带的 `report_has_noninitial_content` 才是 FE 真正缺的东西（D7）。所以 D1 的「不新建端点」精确表述为：**不新增写/CRDT 端点，只新增一个只读 resolve 和一个触发动作**。
- 「wave 存在但无 report 卡」**裁决为 404**（走空态）。但要说清理由：这不是在防一个真实可达的中间态——`today_launchpad_ensure_tx` 在**同一个事务**里建 wave 和 report 卡，adopt-legacy 那支在提交前也还没有 `purpose='launchpad'`，所以按 purpose 查的 resolve 根本看不见它。**404 是因为它便宜且 fail-closed，不是因为那个状态会发生**；别把不可达状态写成裁决理由，那又会变成一条删掉也不会红的假不变量。
- `ensure` 只挂在显式动作上；它的任何失败必须浮出为错误，不得静默重试或降级成空态（与 INV-TODAYTERM-001 同理：静默会把「读失败」变成「悄悄铸了第二份」）。
- 顺手修 `is_unique_constraint`：它在 `today.rs` 里被按**索引名**调用了**两处**——`idx_coves_one_system` 与 `idx_waves_one_launchpad`——而 SQLite 报的是 `coves.kind` / `waves.purpose`（两个索引都是 partial，实跑确认错误串不含索引名）。**所以那两个 `Err(...)` 分支现在都是死代码。**

  **改它是行为变更，不是纯修辞**：从「500 直抛」变成「重试并成功」。因此必须配**真并发或故障注入**用例——`waves.rs` 已导出 `is_unique_constraint_for_test`，用手工构造的 `CalmError` 很容易只测到 helper 那一层而测不到路径。前者恰好在 system cove 首次并发 mint 的那个 race 上，正是 §7 要求验的路径。

  > **r11（PR1 实现期修订）——两处不对称，设计原先假定它们同构，那是错的。**
  >
  > PR1 的评审用**逐点**变异证死了这一点：两处一起还原成索引名 → 并发用例 5/5 红；**只**还原 `waves.purpose` → 18 个用例 3/3 全绿。根因不是测试写少了，是**那个状态在 `ensure` 上不可达**：`write_in_tx` 是 `BEGIN IMMEDIATE`，写锁在事务**开始**时就拿到，所以 `SELECT … WHERE purpose='launchpad'` 与随后的 INSERT/UPDATE 共享同一次持锁，别的写者无法在中间提交。`coves.kind` 之所以**可达**，正因为 `cove_get_system()` 跑在事务**外面**。320 次并发探针从未进入该分支；把 in-tx SELECT 打掉（`AND 1=0`）后分支立即进入——所以那个「从未进入」不是空绿。
  >
  > **裁决：不加 fixtures 钩子。** 为了让不可达分支可达而在生产路径上开一道缝，买来的是一条钉住「生产产生不出来的状态」的假不变量（`feedback_vacuous_invariant_audit`），代价是代码更差。
  >
  > **但不可达 ≠ 不可测。** 这处修复的实质内容是一条关于外部世界的事实断言——*SQLite 对这个 partial unique index 报的是 `waves.purpose` 而不是 `idx_waves_one_launchpad`*——它与路由可达性无关，可以非空洞地钉住：用**真 sqlx 对真 migration** 造一次真实约束冲突，断言 `is_unique_constraint(err, "waves.purpose")` 为真（`coves.kind` 同样办）。不得手工构造 `CalmError`，不得走 `is_unique_constraint_for_test`。
  >
  > 所以两处的证明层**不同**，这一点必须写在明处：`coves.kind` 的**可达性**由并发用例钉住；`waves.purpose` 只有**字符串**被钉住，**可达性没有**。这是一个具名的已知缺口——它的存续价值是「有人把那条 SELECT 挪出事务、或出现第二个 `purpose='launchpad'` 写者时能兜住」，而「今天 `routes/today.rs` 是唯一写者」正是那种改动会悄悄破坏的前提。**别把它说成被并发用例覆盖了。**

### 5.2 不变量表

| ID | 陈述（可证死的形式） | 反例（必须红） | 正例（必须绿） |
|---|---|---|---|
| INV-TODAYDOC-001 | 页面加载只走只读 resolve；`ensure` 只在显式动作上调用 | 加载路径触发 ensure | 加载只 resolve；404 → 空态 |
| INV-TODAYDOC-002 | 动作路径上的失败浮出为错误，不静默降级；**读路径上 detail 的「在飞 / 5xx / 解码失败」三态各自成支**；`spec_harness_dormant` 走一次 start 重提交后重试，仍失败则浮出 | 5xx 被吞成空态**或吞成任何一句不描述该态的文案**；**或**「在飞」那一帧被当成解码失败；**或** dormant 被静默吞掉／无限重试／走了会清空 transcript 的 `/spec/reset` | 5xx → 带服务端原文的错误框 + 重试；在飞 → 不出文案；解码失败 → 只在这一格出「读不出」文案；dormant → 重提交 start → 单次重试 → 成功或浮出 |
| INV-TODAYDOC-003 | 空态判据是服务端 `report_has_noninitial_content`；FE 不解析 report body 文本 | canonical 初始 report 下渲染出四个空标题而非空态；或 FE 出现按 body 文本判断的分支 | canonical initial payload（含 `doc_rev`/`blocks` 已被 CRDT 物化的那一格）→ 空态 + 按钮 |
| ~~INV-TODAYDOC-004~~ | ~~`day_activity_allowed`~~ | 随 D4 第二层一并删除（§0b.4） | — |
| ~~INV-TODAYDOC-005~~ | ~~返回体字段 allowlist~~ | 随 D4 第二层一并删除 | — |
| INV-TODAYDOC-006 | 活动窗口是半开区间；相邻两天不重复计数 | 午夜边界事件被两天各计一次 | 边界事件恰好计一次 |
| INV-TODAYDOC-007 | **`POST /api/today/summary` 自身**在活动窗口为空时既不建会话也不发消息 | 空窗口下打该端点，仍产生了会话或 `harness.user_message.enqueued` | 空窗口 → 409/204，两者皆无 |
| INV-TODAYDOC-010 | **首次**成功触发新增 2 行 `harness.user_message.enqueued`（bootstrap + 摘要），**其后每次**新增 1 行（摘要）；允许 harness 合并 turn | 第二次触发后没有新增 enqueued 行（r4 的 no-op 病） | 三次触发 → 2 + 1 + 1 = 4 行 |
| INV-TODAYDOC-011 | 本端点用的是**裸常量 key**，`card_id` 与 actor / cwd / 请求次数无关 | key 里混入 actor、cwd digest 或任何随请求变化的量 | 换登录身份、re-point 后重复请求，派生同一 `card_id`（golden 钉住） |

方法论备注：

- **INV-007 是收窄后的陈述，不是全称否定。** r5 写成「服务端在空窗口时拒绝发起」，但闸只在这一个端点里：同一个已认证用户可以直接打 `POST /api/waves/{launchpad}/conversations`（wave 级守卫只拒 cove-chat，launchpad 不在其列）或 `POST /api/cards/{id}/spec/input`。**那两条路故意不在射程内**——用户自己手打不是要防的事，要防的是「按钮在没素材时也发起」。写成全称就是一条按字面读自己就红的假不变量。§8 的「没有素材时不写」同步收窄。
- **INV-010 换了证明层。** r5 要求「点三次跑三轮」，而 harness 的 drain + debounce 语义使它**必红或只能靠时序凑绿**（见 D5）。改证 `harness.user_message.enqueued` —— 它是永久 kind、每次入队一行，既抓得住 r4 的 no-op 病（那才是真正要防的回归），又不与合并 turn 冲突。
- **INV-011 取代 r5 的「全生命周期只有一条会话」。** 内核对每 wave 的 assistant 会话数没有上界，端到端数数量证不了它。也不能照抄 `conversation_keys.rs` 现成的 golden——`derive_wave_conversation_keys` 的确定性已经被那条钉死了，照它写等于删掉本设计全部新代码也照样绿。INV-011 钉的是**本端点选 key 的那段代码**，而 r8 之后它要证的恰恰是「key 里什么都不许掺」（见 D5：掺 digest 会连带改掉 card id）。
- **INV-010 证的是「入队」，不是「送达」。** enqueued 事件写在 `observe` 成功之后，而 D5 记的折叠溢出丢弃发生在更后面的 run_loop 里——观测被丢时这条仍然绿。与 INV-003 一样是可接受的近似，写在这里免得下一轮有人拿它当「送达」的证据。
- **INV-010 的正例不能写成绝对计数 3。** 首次触发按新流程会留下**两行**（create 内部的 bootstrap 一行 + 紧随的摘要一行），所以断言按「每次触发的增量 ≥1 且含摘要那条」写。
- **INV-003 防的是 mirror code**：`initial()` 的 body 文本是内核事实，FE 复述必然产错。判据来自服务端。**已验证 blocks-only 写不会漏判**——`calm.report.blocks.*` 每次落库都从 CRDT 重投影出 `body` 再写回 payload，所以只比 summary+body 的判据会翻真。但要补**反方向**断言：canonical 初始 payload 且 `doc_rev`/`blocks` 已被 CRDT 物化时（该函数**刻意**忽略这两者），`report_has_noninitial_content` 仍须为 false——这正是 r4 判据翻车的那一格。
- **自反排除不进不变量表**：它当前不承重（可见性 join 已先滤掉 system cove），写成「必须」会是删掉也不红的假不变量。见 D4。

## 6. 切片计划

descope 后从三个 PR 缩到两个，且 PR1 完全不碰内核。

```
PR1（纯 FE + 一个只读端点） ──> PR2
```

| PR | 内容 | 交付 / 证死的风险 |
|---|---|---|
| **PR1**（~0.7k） | §5.1 窄只读 resolve 端点（`{wave_id, report_has_noninitial_content}`；中间态算 404；修**两处** `is_unique_constraint`，但**两处证明层不同**，见 §5.1 的 r11 注）+ D2/D7 FE：主体渲染 launchpad 的 report、状态条（**waiting 行必须有上限**，见下）、空态、日历保持现状 | Today 变成文档形态。**读路径风险**用 INV-001/002 证死（加载不得触发 ensure；detail 的 in-flight / 5xx / 解码失败**三态各自成支**，不得合并成一句文案）；**空态风险**用 INV-003 证死（canonical 初始 report 必须渲染空态而不是四个空标题）。手写 REST 块即可验，不依赖 AI |
| **PR2**（~0.6k） | D4 `workspace_activity_window`（event-first + allowlist + 半开区间 + 自反排除）+ D5 `POST /api/today/summary`（服务端空活动闸 + prompt 注入 + **按需先建、一律 spec input 的单一路径** + **裸常量 key**（digest 绝不掺，见 D5）+ 静态 bootstrap + dormant 走重提交 start） | **闸的风险**用 INV-007 证死（打这个端点、空窗口必须既不建会话也不发消息）；**边界风险**：INV-006 午夜用例；**有效性风险**用 INV-010 证死（首次 2 行、其后每次 1 行 `harness.user_message.enqueued`，三次共 4 行，**允许合并 turn**）|

PR2 比 r4 小了约三分之一：删掉 MCP 工具那一层，连带 `day_activity_allowed`、截断纪律和两条不变量一起消失。

> **PR2 必须一并解决的失效链（PR1 实现期发现，PR1 内不可达故不修）：** `['today-launchpad']` 不在任何失效路径上，而 `fe/core/events/invalidation-plan.ts` 的 `'wave.report_edited'` 失效的是 `['wave-files']/['wave-report']/['wave-backlinks']`，**不含 `['wave', id]`**——所以 Today 的文档本身也不随 report 编辑刷新。PR1 里没有任何动作能改这两个值（`ensure` 无生产调用方），所以是死状态；**PR2 落 `POST /api/today/summary` 的同时必须把两个 key 都挂上去**，否则「点了按钮页面不动」会是第一个 bug 报告。注：`PolicyMap` 是对 **event kind** 穷举，不是对 query key，所以不加新 Event kind 就没有 golden 会替你把这条报红——它只能靠这行字。

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
- 汇总内容的质量评价。本设计只保证「**这个按钮**在没有素材时不发起」（INV-007，收窄后的陈述），不保证「有素材时写得好」，也**不防**用户自己手打对话去让 agent 写——那不是要防的事。

## 9. 开放问题

**无。**

前四轮的 Q1–Q5 随 descope 全部消解（日期身份、保留窗口、时区主键都不存在了）。**Q6 已定**：汇总写者 = assistant，谁复活 concierge 谁负责裁决分段；这条约束**已同时写进 #951**（issue 注记，含「REWRITE 语义下冲突静默丢内容不报错」这个理由），而不是只留在本 issue——否则复活它的人读不到。

## 10. 七轮 review 的账

留给下一个读这份文档的人，因为过程本身是结论的一部分。

| 轮次 | 载体 | 被什么推翻 |
|---|---|---|
| r1 | 写进 launchpad 的 `wave-report` | 文体契约：那张卡明写「当下快照，每次 REWRITE」 |
| r2 | 新 card kind `daily-log` | 写通道硬绑 `wave-report`；role_gate ∧ 唯一索引的钳形 |
| r3 | 每天一份 wave | system cove 不可删 → GC 无执行器；每天 +1 条常驻 harness |
| r4 | 回到 launchpad 的卡（descope 掉历史） | `Idempotency-Key` 撑不起重跑；MCP 工具模型看不见 |
| r5 | 同 r4，删掉 MCP 层 | `Idempotency-Key` 撑不起重跑（同 key 第二次静默 no-op）；「点三次跑三轮」被 harness 的 drain 语义证伪 |
| r6 | 同上 | 切片表还停在 r5，会把已关闭的 BLOCKER 写回实现 brief；workspace digest 掺进 conversation key 会连带改 card id |
| r7–r8 | 同上 | 切片表**第二次**停在旧版（同一处、同一形状）|

**同一个错犯了四次**：把某个上下文下为真的结论搬到新上下文里，不重验。r2 搬的是 r1 的「AI 写块无需内核改动」；r3 反向过度纠正成「每一条都是不做改动」；r4 把 #951 关于**插件**工具的可见性裁决搬到了**内置**工具上。教训已写进 `feedback_facts_dont_survive_carrier_swap`：换载体时事实表**逐行**重验，并标注每条绑定在哪个载体上——§3 的最后一列就是这么来的。

两条更普适的：

- **「不做改动」和「已确认」是同一个病的两个方向。** 过度推销零成本和搬运旧结论一样危险。
- **切片表是拆 brief 的载体，它比不变量表更容易变成事故。** §5.2 改对了、§6 忘了同步，两轮里发生了两次——按切片表派活的人不会去读不变量表。**任何一处裁决翻案，先改切片表。**
- **一条只约束「不多」不约束「有效」的不变量会绿着骗人。** INV-010 因此补了有效性反例。但补的第一版（「点三次必须真的跑三轮」）又被 harness 的 drain + debounce 证伪——**换证明层比换措辞更难，也更该做**：最终改证每次触发留下一条永久的 `harness.user_message.enqueued`，并明确接受合并 turn。
