# Today 文档化 + AI 今日进度

> 状态：设计 **r3**（经两轮双通道 review。r2 confirm 轮两个通道独立给出同一组 BLOCKER，载体第二次被推翻）。Issue：#1253。
> review 存档：`docs/_1253-design-review-{codex,subagent}.md`（r1）、`docs/_1253-design-review-{codex,subagent}-r2.md`（r2 confirm）。
> 关联：#951、#120、#1045、#1234。

## 0. r2 → r3：confirm 轮改掉了什么

两轮下来载体错了两次，**而且是同一个错法**：把「已确认的事实」跨载体搬运——那些事实只在被否掉的那个载体下为真。r1 说「AI 写块无需内核改动」，r2 换了载体却把这行原样留在事实表里。这是 r3 首先要修的方法论问题，所以 §3 的事实表现在**逐行标注它绑定在哪个载体上**。

confirm 轮的四条 BLOCKER（两通道独立同结论，我逐条读代码复核）：

1. **`daily-log` 卡拿不到 report 的写通道。** `calm.report.blocks.*` 经 `resolve_report_for_caller` → `load_report_for_wave`，永远查 `kind == "wave-report"`，**没有 target-card 参数**；REST 块 API 同样按 wave 解析唯一 report 卡；CRDT 落库缝 `card_update_with_crdt_tx` 开头就拒非 wave-report（还有测试逐字钉死错误串）。所以 r2 的「内核写通道零改动」是假的。

2. **role_gate 与唯一索引形成一记钳形夹击，这条最致命。** `enforce_assistant_scope` 的注释写得毫不含糊：Assistant 只能写**自己的卡**或**自己 home wave 的 ReportCard**——「the report card of *another* wave is refused」。而 `0013` 的 `idx_cards_one_report_per_wave` 限定**一个 wave 只能有一张 ReportCard**。于是 daily-log 若用 Worker role，Assistant 写不了；若用 ReportCard role，第二张卡撞索引。**新 kind 这条路要么放宽索引、要么新增 Document role、要么新开一族 MCP 工具，三条都是文档运行时的手术。**

3. **确定性 card id 没有写口。** `create_card` 一律 `new_id()`，wire 上没有 id 字段；`card_create_with_id_tx` 是裸 INSERT，**没有 ON CONFLICT**，第二次同 id 是 UNIQUE 违例而不是覆盖。INV-006 的「upsert 覆盖」在 r2 里只是设想。

4. **`create_mode` / `persistence_invariants` 是死代码**（全仓零消费者，trait 上挂 `#[allow(dead_code)]`）。wave-report 的「内核铸造 / 不可删 / 每 wave 唯一」实际由手写分支 + `deletable` 列 + migration 索引三处硬编码保证。所以 §3 的「新 kind 是小 trait impl」低估了 PR1：它买到的只是 `validate_payload`。

另有两条改变了 D4/D5 形状：

5. **`list_waves_window` 不是活动源。** 它查的是**生命周期与窗口的重叠**（`created_at <= until AND (terminal_at IS NULL OR terminal_at >= since)`），不是「发生了什么」。r2（承 r1 subagent 通道）说它「purpose-built for this」说过头了。而且它是**双端闭区间**，连续两天会把恰好落在午夜的边界重复计数。

6. **「无条件调 ensure」把读文档绑在 harness 健康上。** `ensure_today_launchpad` 在事务之后还要 `materialize_workspace`，再 `submit("spec-harness-start")` 并 **`.wait()`**。所以 r2 的写法会让每次打开 Today 都同步引导 codex spec harness 并阻塞；codex 不可用时整页硬失败，连历史都读不了——而现在的 Today 不需要这些就能渲染。

> 顺带发现一个**既有 bug**（不属于本设计，记录在案）：`today.rs::is_unique_constraint` 用索引名 `idx_waves_one_launchpad` 去匹配错误消息，而 SQLite 实际报 `UNIQUE constraint failed: waves.purpose`；仓库另一条 ensure 测试（`chat_wave_ensure.rs`）正是按列名断言的。

## 1. 问题

Today 只有「当前状态」，没有「发生了什么」。

`fe/web/src/features/today/public.tsx`（435 行）的主列是三段 `WaveRow` 列表，全部由同一份 `waves` 数组 filter 得到；右侧周历的日格数字是「那天有几条 wave 活动」，点开换成同一种列表的另一份切片；中间夹着一个写死 `Terminal is not wired up yet.` 的占位 section。

它现在的信息量约等于侧边栏加一个时钟。**一天结束时，这一页答不出「今天做了什么」。**

## 2. 前提

**一、新 FE 尚未上生产**（沿用 `docs/1147-workspace-design.md`）。破坏性 DTO 变更可一步到位。

**二、老数据不迁移、不兼容**（用户 2026-08-31 定调，含生产库 `:4040`）。

> ⚠️ 只豁免老数据。同一次运行内的并发、崩溃后重跑、幂等、失败回滚仍是硬要求。

**三、不碰 Today terminal。** `features/today/README.md` 的 INV-TODAYTERM-* 原样保留。

## 3. 已确认的既有事实

**每行都标注它绑定在哪个载体上**——这是 §0 那个方法论错误的解药。不写行号（r1 的行号一轮内腐化两处），引 symbol。

| 事实 | 位置 | 载体依赖 |
|---|---|---|
| `waves.purpose` + partial unique index 是「一个作用域一条特殊 wave」的房内模式，**已用过两次** | `0064_waves_launchpad_purpose.sql`（launchpad）、`0074_one_chat_wave_per_cove.sql`（cove-chat） | 载体无关 |
| **`purpose` 标记的 wave 可以关掉 spec harness**，有既有先例且两处强制 | `harness/mod.rs`（recovery skip）、`replay.rs`（Forbidden），判据 `COVE_CHAT_PURPOSE` | 载体无关 |
| `create_wave` 铸 wave + `CardRole::Spec` 卡 + `wave-report` 卡，然后 materialize workspace、submit `spec-harness-start`（start 失败非致命） | `routes/waves.rs::create_wave` | 载体无关 |
| Assistant 只能写自己的卡或**自己 home wave 的** ReportCard；别的 wave 的 report 卡一律拒 | `calm-truth/src/role_gate.rs::enforce_assistant_scope` | 载体无关（**是 §0.2 钳形的一半**） |
| 一个 wave 只能有一张 ReportCard | `0013_cards_deletable.sql::idx_cards_one_report_per_wave` | 载体无关（**钳形的另一半**） |
| `calm.report.blocks.*` / REST 块 API / CRDT 落库缝**全部硬绑 `wave-report`**，无 target-card 参数 | `mcp_server/tools/wave_report.rs::load_report_for_wave`、`routes/wave_report_blocks.rs`、`db/sqlite/card.rs::card_update_with_crdt_tx` | 载体无关 |
| 模板路径已有「换一份 report 契约前缀」的先例 | `calm-types/src/wave_report.rs::report_contract_prefix_for_workflow_template` | 载体无关 |
| `POST /api/waves/{id}/conversations` **必须带 `Idempotency-Key`**，语义是「同 key = 同一条可重试草稿」，五个 arm | `routes/wave_conversations.rs` | 载体无关 |
| `ensure` 端点存在但**没有任何生产调用方**；它还会 materialize + 等 harness 起来 | `routes/today.rs`；全仓 grep | 载体无关 |
| system cove 默认不在 `GET /api/coves` 里（#175），FE workspace 扇出因此看不到它 | `routes/coves.rs::list_coves`；`app/providers/queries.ts::useWorkspace` | 载体无关 |
| `list_waves_window` 查的是**生命周期重叠**，不是活动；且双端闭区间 | `routes/waves.rs::list_waves_window` | 载体无关 |
| `events` 有 `idx_events_at`，但全仓无按 `at` 的读者；`at` 是墙钟，`id` 才是游标 | `0004_events.sql`、`0007_events_scope.sql` | 载体无关 |
| 结构性事件永久；**`harness.item.added` 在 prune allowlist 里**（30 天） | `docs/events-retention.md`、`calm-truth/src/events_prune.rs` | 载体无关 |
| 后台保留循环的先例是 `spawn_wave_history_pruner` / `events_prune`；**`calm.admin.wave_gc` 不是**——它是 Spec-only 的手动 VCS GC | `wave_vcs/gc.rs`、`events_prune.rs`、`mcp_server/tools/admin.rs` | 载体无关 |
| 块渲染器是纯值注入（`report: WaveReport \| null`），与 kind 无关 | `features/report/document/public.tsx` | 载体无关 |
| `readWaveReport` 硬绑 `kind === 'wave-report'` | `fe/core/domain/report.ts` | 载体无关 |
| prose 块**没有尺寸上限**；256KB 只约束非 prose 块的 canonical JSON | `report_blocks/kinds.rs`、`report_blocks/mod.rs` | 载体无关 |
| ~~AI 写块无需内核改动~~ | — | **仅在「写进 wave-report」时为真。r2 误搬，已删。** |
| ~~新增 card kind 是小 trait impl~~ | — | **它只买到 `validate_payload`；见 §0.4。已删。** |

## 4. 决策

### D1 —— 每天一份 wave；当天的文档就是它自带的 `wave-report` 卡

```
system cove
├── launchpad wave   （purpose='launchpad'，既有，不动）
└── daily-log wave × N
    ├── purpose = 'daily-log:<day_key>'   ← 日期身份在这里
    ├── wave-report card                  ← 当天的文档（既有 kind，既有一切）
    └── assistant 会话                    ← 当天的写者
```

**为什么这条能过 §0 的四条 BLOCKER，而新 kind 过不了**——每一条都是「不做改动」而不是「做对改动」：

| confirm 轮的 BLOCKER | 每天一份 wave 下的状态 |
|---|---|
| B1 写通道硬绑 `wave-report` | 当天的文档**就是**一张 `wave-report` 卡。`calm.report.blocks.*`、REST 块 API、CRDT 缝原样可用 |
| B2 role_gate ∧ 唯一索引的钳形 | 写者是**当天 wave 内**的 assistant 会话 → 写的正是「自己 home wave 的 ReportCard」，gate 原样放行；每 wave 一张 ReportCard，索引原样满足 |
| B3 确定性 card id 没有写口 | 身份移到 wave 的 `purpose`，用 §3 第一行那个**用过两次**的 partial-unique-index 模式；幂等用既有 ensure 事务形状 |
| B4 `create_mode` 等是死代码 | 不新增 card kind，与它无关 |

附带解决的：`readWaveReport` 不用拆（它找的就是 `wave-report`）；`headless-filter` / CARDS 面板不会被 N 张陌生卡污染（没有新 kind）；日历索引是「哪些 daily-log wave 存在于这个窗口」——**存在性正是 `list_waves_window` 的生命周期重叠语义能正确回答的问题**（注意这与 §0.5 不矛盾：它做**索引**合格，做**活动源**不合格，D4 只用它做前者）。

**文体契约**：daily-log wave 的 report 用 `WaveReportPayload::new` + 一份**日志文体**的契约前缀播种，而不是 `initial()` 的「当下快照 / 每次重写 / 四个固定 H1」。这条路模板已经走过（§3 的 `report_contract_prefix_for_workflow_template`）。

**spec harness 关掉**：daily-log wave 不需要常驻 spec agent。按 `COVE_CHAT_PURPOSE` 的既有先例，在同样的两处（recovery、replay）对 `daily-log:` 前缀做同样的碳拷贝carve-out。

**残余成本，诚实列出**：每天一个 managed 工作区（目录 + `git init`），受 D6 的保留窗口约束在 N 个而非 365 个。r2 用「一年 365 个 git 仓库」否掉这条方案时**没算上自己设计的保留窗口**，那是个错误的否决。

**被否的备选 A：新 card kind `daily-log`**（r2 的 D1）。见 §0.1–0.4：它要么放宽 `idx_cards_one_report_per_wave`，要么新增 Document role 并改 role gate，要么新开一族按 day key 定位的 MCP 写工具 + 第二条 kind-aware CRDT/CAS persist seam。三条都是文档运行时的手术，而本 issue 是一个页面改版。

**被否的备选 B：写进 launchpad 现有的 `wave-report`**（r1 的 D1）。文体冲突，见 r2 §0.1。

### D2 —— Today 主体渲染选中日的 daily-log report，日历是它的索引

日历从「换一份 wave 列表」变成「翻文档的日期」。索引 = `list_waves_window` 筛 `purpose LIKE 'daily-log:%'`；选中某日 → 取那条 wave 的 detail（一次请求，只有那一天的正文）。这同时避开了 confirm 轮的 MAJOR「列 cards 会一次下载全部历史正文」——每天独立成 wave，不存在这个问题。

### D3 ~~块时间戳~~ —— 删除（r2 已删，保留条目以免编号漂移）

### D4 —— 活动源必须 event-first；`list_waves_window` 只做索引

confirm 轮 B3 的直接后果。两层：

**第一层（服务端 projection）**：新的 `workspace_activity_window(start, end)`，**按 `events.at` 聚合显式 kind allowlist**，再 join 用户可见 cove/wave。**不以 `list_waves_window` 的生命周期候选集为上游**——一条在窗口前就 terminal 的 wave，今天仍可能被编辑 report，而它不在那个候选集里。

**allowlist 就地裁决**（confirm 轮指出「倾向删掉」不是决策）：

| kind | 计什么 | retention |
|---|---|---|
| `wave.lifecycle_changed` | 生命周期变迁数 | 永久（结构性） |
| `wave.report_edited` | 报告编辑数 | 永久（结构性） |
| `task.completed` / `task.failed` | 任务成败数 | 不在 prune allowlist 内 |

**`turns` 删除**。它的事实源 `harness.item.added` 在 30 天 prune allowlist 里，且正确计数要解析全部 `harness_items.params`（既有 conversation DTO 正是因此拒绝提供它）。没有替代的永久事实源，所以不做。

**窗口是半开区间 `[start, next_start)`**，避免 §0.5 的午夜重复计数；按 `at` 查询，**不与 id 游标混用**（`0004_events.sql` 的警告要抄进 doc comment）。

**第二层（MCP）**：`calm.day.activity` 是第一层的薄封装 + 授权闸，**复用同一个 repo/service helper，不重新实现查询**。「薄封装」的边界要写死：**复用查询函数，不复用 REST 的人向可见性与 session 门**——授权只走 `day_activity_allowed`（这正是 #951 踩错的位置）。

**授权**（confirm 轮判定 r2 的文字层已关闭 r1 BLOCKER 3，此处只调整判据）：descriptor `visible_to_roles: &[]`；`tools/list` 仅对已验证身份做 contextual augmentation；`tools/call` 独立重查；同一个 async gate helper 调两次。判据：role **仅 Assistant** + active session + wave 行存在 + **`purpose` 以 `daily-log:` 开头**（不再是 launchpad——写者搬进当天的 wave 了）+ cove/card 归属一致；unresolved / cross-session / dormant / missing row / DB error 一律拒。判据只从 `ToolCallIdentity` 推导，**绝不从 args 取**。截断纪律照抄 `report_links.rs`（wave 数 / 每 wave 条目数 / 总字节三重，按字节回卷）。

### D5 —— 手动触发；服务端固定「今天」；没有活动就不发起

「写今日进度」→ ensure 当天的 daily-log wave → 在**它**上面起 assistant 会话（带 `Idempotency-Key`；key 的语义是「同请求重试」，「同日只有一份」由 D1 的 wave 单例索引负责，两者不要混）。prompt：`calm.day.activity` → 写本 wave 的 report。

**MCP 不接受 `since/until`**（Q4 裁决）：服务端从已验证身份 + D6 的时区算半开日窗。攻击面清零，且与「窗口至多一日」天然一致。

**空活动不发起**：判据用 D4 第一层的同一份 projection，不是 FE 的 waves 快照。

定时汇总属于 #120，不做。

### D6 —— 时区由服务端独占；边界落在写 seam 与后台 GC

**时区（Q5 裁决，r1/r2 都漏了，而它决定主键）**：持久化一个 app/workspace 级 IANA 时区。浏览器只在首次提供建议值，服务端校验并持久化；**此后服务端是 day key、wave id 与活动窗口的唯一作者**。日窗 = 本地日期对应的**半开 UTC 区间**，允许 DST 的 23/25 小时日。禁止用 server process timezone，也禁止用每次请求的浏览器时区——否则两个不同时区的客户端会为同一天铸两条 wave，直接打穿单例索引。已铸的 day key **永不因设置变化而重写**。

代价要写进 doc comment：FE 现有的浏览器本地日历与服务端日界在跨时区时会错位；单机 LAN 单用户场景下无害，**但「何时不再无害」要写清楚**，别让下一个人以为它普适正确。

**保留窗口**：42 个本地日（六个可导航周）**且**总字节 ≤ 4 MiB，任一超限即删最老的一天。删一天 = 删一条 wave（既有删除路径）。

**上限校验落在写 seam**，不落在 `CardKindHandler::validate_payload`——confirm 轮指出 report CRDT persist 根本不调 CardKindRegistry。daily-log 的单份上限（建议单卡 128 KiB / 32 blocks / 单 prose 32 KiB）在 CAS 事务里校验。**不要顺带改所有 wave-report 的 prose 契约**——那是夹带。

**GC 用后台 pruner 先例**（`spawn_wave_history_pruner` / `events_prune`），不是 `calm.admin.wave_gc`（那是 Spec-only 手动 VCS GC，恰恰是 D6 要反对的「agent 触发的脆链」）。要明确 boot wiring、周期、按最老日删除、事件与 role-cache 清理。

**诚实标注**：删除**不减少占用**——`wave.report_edited` 永久保存完整 `body_before/body_after`，删卡还会新增永不清理的 `card.*` 结构性事件。GC 的收益是「读取路径不再变长」，不是「磁盘变小」。`events_prune` 的模块注释已经就同一件事说过一次（"The pruner never VACUUMs"），换引它。

### D7 —— FE 形态：状态条在前，文档紧随

`fe-design.md` §8.1 的约束不因改版而丢：用户打开这一页是要回答「有什么在等我？」，答案归位置和 `--warn` 像素，不归字号。**状态条在前**（Q3 裁决）：它高度 O(1)、不随内容增长把文档推出首屏；而「文档是主角」由面积和视觉权重表达即可，不必用位置表达。

- 状态条：`N waiting · N running` + 等待中的 compact 行。
- 主体：`ReportDocument` 渲染选中日的 report。空态「今天还没有进度」+ 触发按钮（无活动时按钮不出现）。
- 右面板：日历（选日切主体）+ Running / Recent + 现有 Conversations 模块不动。
- terminal 占位 section 原样保留。

### D8 —— 可见性：daily-log wave 不得漏进用户面

新增的一类 wave 必须在每个用户可见面上被过滤掉。已知面：`GET /api/coves`（system cove 已被 #175 过滤 ✓）、`list_waves_window`（**当前只排除 `COVE_CHAT_PURPOSE`，`purpose='launchpad'` 是漏过去的**，daily-log 会同样漏 ✗）、侧边栏、cove 页列表、`activeWavesOn` 的客户端计算。

这条在 r2 里只是 confirm 轮的一个 MAJOR，r3 把它提成独立决策：**它是「新增一类 wave」的必然义务，不是某个端点的补丁**。实现要求是一处 `user_visible_wave` 判据 + 一条覆盖全部读端点的扫地测试，而不是逐个端点打补丁。

## 5. 契约与不变量

### 5.1 解析链：读路径不得依赖 harness

confirm 轮的 MAJOR 1 直接改写了 r2 的 §5.1。

```
GET /api/today/launchpad        （新增只读 resolve，404 = 还没有）
  └─ 404 → 只有显式动作才 ensure
GET /api/waves?since&until&purpose_prefix=daily-log:   （日历索引）
GET wave detail（选中那一天）    → 它的 wave-report 卡
```

- **读历史不得要求 harness 可用。** `ensure` 会 materialize workspace 并 `.wait()` 等 spec harness 起来；把它放在页面加载路径上，等于让 codex 不可用时整页硬失败。ensure 只挂在「写今日进度」这个显式动作上。
- INV-002 因此收窄到**动作**上：ensure 的任何失败必须浮出为错误，不得静默重试或降级成空态。页面加载走只读 resolve，404 是正常空态而不是错误。
- `is_unique_constraint` 的既有 bug（§0 末）要在碰到它的那个 PR 里顺手修：匹配 `waves.purpose` 而不是索引名。

### 5.2 不变量表

| ID | 陈述（可证死的形式） | 反例（必须红） | 正例（必须绿） |
|---|---|---|---|
| INV-TODAYDOC-001 | 一天至多一条 daily-log wave，由 partial unique index 保证 | 同日铸出第二条 | 并发 ensure 撞索引后收敛到同一条 |
| INV-TODAYDOC-002 | ensure 只在显式动作上调用；其失败浮出为错误。页面加载走只读 resolve，404 是空态 | 页面加载触发 ensure；或 5xx 被吞成空态 | 加载只 resolve；动作失败浮出错误框 |
| INV-TODAYDOC-003 | day key 由服务端按持久化 IANA 时区计算；客户端时区不参与 | 改浏览器时区导致铸出第二条同日 wave | 任意客户端时区，day key 不变 |
| INV-TODAYDOC-004 | `day_activity_allowed` 是唯一判据，当且仅当 predicate 成立时放行 | transport 任一分支（unresolved / cross-session / dormant / missing row / DB error）绕过 | 全分支 iff 断言 |
| INV-TODAYDOC-005 | 返回体字段是 allowlist（schema 层无 path / 无正文字段） | schema 出现 path 或正文字段 | 字段集合等于 allowlist |
| INV-TODAYDOC-006 | 活动窗口是半开区间；相邻两天不重复计数 | 午夜边界事件被两天各计一次 | 边界事件恰好计一次 |
| INV-TODAYDOC-007 | 活动窗口为空时不发起会话 | 空窗口起了会话 | 空窗口直接空态 |
| INV-TODAYDOC-008 | 单份上限在 CAS 事务内拒绝；保留窗口由后台 GC 执行 | 越界 payload 被接受；第 43 天在无人触发时仍存活 | 越界拒绝；GC 后只剩 42 天 |
| INV-TODAYDOC-009 | daily-log wave 不出现在任何用户可见面 | 任一读端点返回 `purpose` 以 `daily-log:` 开头的 wave | 全端点扫地测试通过 |

三条方法论备注（沿用 r1 review 的裁决）：

- **INV-004 与 INV-009 都是全称否定。** 身份空间不止四个 `CardRole`（还有 unresolved daemon、card-bound/no-thread、cross-session、dormant、missing row、DB error）；可见面也不是一个固定清单。所以形式必须是「单一 predicate 的 iff 测试」+「对全部读端点/全部 transport 分支的扫地测试」，不是列举。
- **INV-005 的值级「无绝对路径」不可证明**（合法 `title` 本身就可以是 `/home/x`），能证的是 schema 层字段 allowlist。
- **INV-003 的变异对象是时钟与时区，不是文本。** 日期身份落在服务端 day key 上之后，「改 heading 文本」不再是危险面。

## 6. 切片计划

```
PR1 ──┐
      ├──> PR3
PR2 ──┘
```

| PR | 内容 | 交付 / 证死的风险 |
|---|---|---|
| **PR1**（~1.1k） | D1 daily-log wave（`purpose` + 单例索引 + ensure 事务 + 日志文体契约前缀 + harness carve-out）+ D6 时区 + D8 可见性过滤 + D2/D7 FE（只读 resolve、日历索引、主体渲染、状态条、空态） | Today 变成文档且能按日翻。**时区风险**用 INV-003 的时钟/时区变异证死；**可见性风险**用 INV-009 扫地测试证死。手写 REST 块即可验，**不依赖 AI，不依赖 PR2** |
| **PR2**（~0.9k） | D4 第一层 `workspace_activity_window`（event-first + allowlist + 半开区间）+ 第二层 `calm.day.activity`（薄封装 + `day_activity_allowed` 单一 gate + 截断） | **授权风险**证死：gate 的 iff 测试 + transport 全分支矩阵；**边界风险**：INV-006 午夜用例；返回体 schema allowlist 断言 |
| **PR3**（~0.7k） | D5 触发闭环（动作 + ensure + `Idempotency-Key` + prompt + 空活动不发起）+ D6 后台 GC | 端到端：点一次今天的文档出现；点第二次不长第二条 wave；无活动时按钮不出现；GC 后只剩 42 天 |

§9 的开放问题**全部已裁决**，PR1 可以开工。

## 7. 风险

- **`ensure` 是冷路径**且会 materialize + 等 harness。PR1 要按新路径验，含并发撞唯一索引，并修 `is_unique_constraint` 的既有 bug。
- **`calm.day.activity` 是全 MCP 面第一个跨 cove 读**，是新类别不是更宽的工具。#951 在同一位置踩过一次。PR2 按那份裁决逐条核对，不重新推导。
- **`workspace_activity_window` 是仓库里第一个按 `at` 的事件查询**。`0004_events.sql` 明确警告「`at` 是墙钟，`id` 才是游标，永不混用」——抄进 doc comment。
- **每天一条 wave 是新的资源节奏**。工作区目录 + `git init` × N。保留窗口是唯一的边界，所以 PR3 的 GC 不是可选项，是 PR1 的债主。
- **新增一类 wave 就是新增一份可见性义务**（D8）。`list_waves_window` 今天连 launchpad 都漏，说明这类义务在本仓库确实会被漏。
- **Tier-A 边界**：日志文体契约前缀、可见性判据、时区设置都会牵动 wire 与 goldens。按 `feedback_run_ci_exact_command_locally`，PR1 门禁跑 CI 完整命令（整个 workspace + features + web build），不是子集。

## 8. 明确不做

- 定时/自动汇总（→ #120）。
- Today terminal 接线（→ 现有 INV-TODAYTERM-*）。
- 保留窗口之外的历史 UI。
- 手机端形态（→ #1234）。
- `turns` 计数（D4，无永久事实源）。
- 顺带修改所有 wave-report 的 prose 契约（D6，那是夹带）。
- 汇总内容的质量评价。本设计只保证「没有素材时不写」（INV-007），不保证「有素材时写得好」。

## 9. 开放问题：全部已裁决

两轮 review 两个通道对五问的推荐**完全一致**（Q2 的具体数值取 codex 更细的一版）。裁决记录在此，不再留作开放项：

- **Q1 一天一份**（原「一天一卡」）→ 采纳，但载体从卡改为 wave（§0.1–0.4）。日期身份、隔离、整日 GC 都更可靠，且不需要任何文档运行时手术。日历索引用 `list_waves_window`，**不用**「列 cards」（那会一次下载全部历史正文）。
- **Q2 保留窗口** → 42 个本地日 **且** 总 payload ≤ 4 MiB，任一超限删最老；单份 128 KiB / 32 blocks / 单 prose 32 KiB。42 天 = 六个可导航周。
- **Q3 排序** → 状态条在前（D7）。
- **Q4 窗口参数** → 服务端固定「今天」，MCP 不接受 `since/until`（D5）。
- **Q5 时区** → 持久化 IANA 工作区时区，服务端独占 day key（D6）。这是 r1/r2 都漏掉、而它决定主键的一条。
