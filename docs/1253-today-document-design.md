# Today 文档化 + AI 今日进度

> 状态：设计 **r2**（经一轮双通道 review；两个通道均判 *fix-then-ship*，但 review 证据推翻了 r1 的载体决策，见 §0）。Issue：#1253。
> r1 review 存档：`docs/_1253-design-review-{codex,subagent}.md`。
> 关联：#951（launchpad wave 与它的 report card）、#120（定时汇总）、#1045（文档运行时）、#1234（手机端）。

## 0. r1 → r2：review 改掉了什么（先读这段）

两个通道都认可方向（Today 改文档形态、AI 写进度），但 r1 有一条**载体错误**和一条**可达性错误**，各自足以让 r1 的实现计划整片作废。

1. **r1 把 Today 的进度写进 launchpad wave 现有的 `wave-report` card——而那张卡自带的契约明令禁止这种写法。** `crates/calm-types/src/wave_report_contract_rules.md` 写死了「报告反映当下的状态，不是历史。每次更新 **REWRITE** 相关章节，让陈旧条目消失；历史由内核的 event timeline 承载」，`wave_report_section_rules.md` 又写死了四个 H1（概要/待你定/已完成/决策）「不要新增、不要重命名、不要调整顺序」，外加 1000 字散文预算。而 `today.rs` 正是用 `WaveReportPayload::initial()` 铸的这张卡，契约文本就在 body 里。**一份按日累积的进度日志和一份「当下快照、每次重写」的简报是相反的文体**；把两者塞进同一张卡，agent 会同时收到两套冲突指令。→ **D1/D2 重做**（§4）。

2. **r1 §5.1 的 launchpad 解析链根本不可达。** 三重反证：`TodayLaunchpad` 响应里**没有 `report_card_id`**（它只活在内部 `EnsureTxResult`）；`GET /api/coves` 默认过滤掉 system cove（#175，`include_system=true` 才是逃生口），而新 FE 的 waves 是按 cove 扇出的，所以 launchpad wave 在 `workspace.waves` 里**由构造不可见**；`fe/core/domain/wave.ts` 的域模型又把 `purpose` 丢了。→ **§5.1 重写**。

3. **D3（给 `ReportBlock` 加时间戳）的成本被 r1 低估了一个数量级。** 它不是「加两个字段」：`WaveReportPayload::SCHEMA_VERSION` 是 **Tier A**，doc comment 明写「同一个 PR 必须同时扩 `WaveReportCardHandler` 和前端 zod」，还要穿过 CRDT block entry、JSON 投影、MCP read、REST、wire、两个前端的 domain。而 r1 用来否掉「每天一份」的理由（「D3 用一个字段就买到了」）因此不成立。→ **D3 在 r2 里被删除**，日期身份改由载体自带。

4. **备选 C（从 `wave.report_edited` 事件流派生块时间）不可实现**，不是「成本高」：该事件只有扁平的 `body_before/body_after`，没有 block id，重排/拆块后无法恢复「某块首次出现」。→ 明确否决，不再列为待评审。

5. **素材源有一条现成的、purpose-built 却从没被用过的端点**：`GET /api/waves?since&until&cove_id`（#250 PR2，doc comment 直接写着 "calendar window query parameters"），全 `fe/` grep 不到任何调用方，而现在的日历在客户端重算同一件事。→ **D4/D5 必须先回答「为什么不是扩它」**（§4 D4）。

6. **`calm.day.activity` 会是整个 MCP 面上第一个跨 cove 读**，不是「一个更宽的工具」，是一个新类别（现存最宽的 `calm.cove.outline` 是单 cove，且有 50 wave / 40 block / 32KB 三重截断）。→ 授权与截断纪律按 §951 裁决和 `report_links.rs` 的既有先例收紧（§4 D4）。

7. **增长边界（r1 D6）是假的**：256KB 只约束非 prose 块的 canonical JSON，**prose 块没有尺寸上限**；一天可以创建任意多块；而且删掉旧内容并不减少占用——`wave.report_edited` 永久保存完整 `body_before/body_after`，删除只是把增长搬进事件表。→ **D6 重做**。

8. 杂项：`turns` 的事实源是 `harness.item.added`，**它正在 30 天 prune allowlist 里**，r1「只依赖结构性事件」的说法把它引申错了；`POST /api/waves/{id}/conversations` **必须带 `Idempotency-Key`**，r1 的 `{text}` 调用漏了必需契约；`events` 表虽有 `idx_events_at`，但**全仓没有任何按 `at` 查询的读者**，且 `0004_events.sql` 明确警告「`at` 是墙钟，`id` 才是游标，永不混用」。

r1 里活下来的：Today 该变成文档（§0 的问题陈述）、AI 写入通道无需内核改动、状态条守 §8.1、手动触发优先、切片按「FE 可独立交付」切。

## 1. 问题

Today 只有「当前状态」，没有「发生了什么」。

`fe/web/src/features/today/public.tsx`（435 行）的主列是三段 `WaveRow` 列表——Waiting on you / Running / Recent——全部由同一份 `waves` 数组 filter 得到；右侧周历的日格数字是「那天有几条 wave 活动」，点开换成同一种列表的另一份切片；中间夹着一个写死 `Terminal is not wired up yet.` 的占位 section。

它现在的全部信息量约等于侧边栏加一个时钟。**一天结束时，这一页答不出「今天做了什么」。**

## 2. 前提

**一、新 FE 尚未上生产**（沿用 `docs/1147-workspace-design.md` 的同名前提）。破坏性 DTO 变更可以一步到位。

**二、老数据不迁移、不兼容**（用户 2026-08-31 定调，含生产库 `:4040`）。

> ⚠️ 只豁免老数据。同一次运行内的并发、崩溃后重跑、幂等、失败回滚仍是硬要求。

**三、不碰 Today terminal。** `features/today/README.md` 的 INV-TODAYTERM-* 原样保留。

## 3. 已确认的既有事实

每条都读了代码。**不写行号**——r1 的行号在一轮 review 内就腐化了两处；引 symbol 与文件。

| 事实 | 位置 |
|---|---|
| launchpad wave 的 ensure 端点存在且幂等，`waves.purpose='launchpad'` 由 partial unique index `idx_waves_one_launchpad` 保证单例 | `routes/today.rs`；migration `0064` |
| **但 `ensure` 没有任何生产调用方**——只有路由声明、五处集成测试、旧 `web/` 的 generated types。新旧两个前端都不调它 | 全仓 grep |
| `TodayLaunchpad` DTO **不含** `report_card_id` | `routes/today.rs::TodayLaunchpad` |
| system cove 默认不在 `GET /api/coves` 里（#175），因此 launchpad wave 不在 FE 的 workspace 扇出中 | `routes/coves.rs::list_coves`；`app/providers/queries.ts::useWorkspace` |
| 块渲染器完整且已在生产路由使用 | `features/report/*`；`app/router` 的 `WaveRoute → ReportDocument` |
| **AI 写块无需内核改动**：`CardRole::Assistant` 已被允许调 `calm.report.blocks.*` / `write_markdown` | `mcp_server/tools/wave_report_blocks.rs`；`calm-types/src/model.rs::CardRole::Assistant` |
| 人写块与 agent 写块是两条通道，互不冒充 | REST 强制 `X-Calm-Actor: user`；MCP 走 `ToolCallIdentity` |
| `POST /api/waves/{id}/conversations` **必须带 `Idempotency-Key`**，语义是「同 key = 同一条可重试草稿」，五个 arm | `routes/wave_conversations.rs` |
| 新增 card kind 是一个小 trait impl + 注册 | `calm-truth/src/card_kind.rs::CardKindHandler`（`kind_id` / `matcher` / `create_mode` / `schema_version` / `validate_payload`）+ `builtins.rs` |
| `GET /api/waves?since&until&cove_id` 是为日历窗口专门做的，**且完全没有前端调用方** | `routes/waves.rs::list_waves_window`（#250 PR2） |
| `events` 有 `idx_events_at`，但全仓无按 `at` 的读者；`at` 是墙钟，`id` 才是游标 | `0004_events.sql`、`0007_events_scope.sql` |
| 结构性事件永久；**`harness.item.added` 不是结构性事件，它在 prune allowlist 里** | `docs/events-retention.md`；`calm-truth/src/events_prune.rs` |
| `wave.report_edited` 永久保存完整 `body_before/body_after`（无 block id） | `calm-types/src/event.rs::WaveReportEdited` |
| wave VCS 默认只留最近 50 个 commit——**它不是永久历史** | `calm-truth/src/wave_vcs/gc.rs` |
| prose 块**没有尺寸上限**；256KB 只约束非 prose 块的 canonical JSON | `report_blocks/kinds.rs::MAX_CANONICAL_BYTES`、`report_blocks/mod.rs` |

## 4. 决策

### D1 —— 新 card kind `daily-log`，一天一张卡，挂在 launchpad wave 上

这是 r2 最大的改动，直接来自 §0.1。

```
launchpad wave (purpose='launchpad', system cove)
├── spec card        （既有，不动）
├── terminal card    （既有，不动）
├── wave-report card （既有，不动 —— 它继续做「当下快照」）
└── daily-log card × N   ← 新增，一天一张
```

- **载体**：新 card kind，payload 复用 report 的**块模型**（`ReportBlock[]` + `doc_rev`），因此 `ReportDocument` 渲染器直接可用，**不新做渲染路径**。
- **日期身份**：卡本身就是日期——确定性 card id（`daily-log:<launchpad_wave_id>:<YYYY-MM-DD>`）。日期身份由载体携带，**不需要块时间戳**（r1 D3 因此删除，连同 Tier-A schema bump）。
- **契约**：这张卡自带**日志文体**的契约注释（当日追加、写产出不写过程、当日字数预算），与 `wave-report` 的「当下快照 / 每次重写 / 四个固定 H1」互不干扰。两种文体各有各的卡，这正是 §0.1 要解决的冲突。
- **幂等**：同一天重复汇总 = upsert 同一张卡（INV-006 因此从「靠 agent 遵守 prompt」变成**载体层的确定性 id**）。
- **日历索引**：列 launchpad wave 的 cards，一次请求拿到「哪些天有进度」——不需要每天一次请求。
- **保留**：删一整天 = 删一张卡，边界清晰（见 D6）。

**被否的备选 A：给 launchpad 的 `wave-report` 换一份专用契约**（codex 的建议）。不新增 kind，但要让 launchpad 的 report body 偏离 `WaveReportPayload::initial()`——而 `report_startup_read_required()` 正是按与 `initial()` **逐字相等**来判定的，偏离会让每条 launchpad spec 首轮都被强制 `calm.report.read`。为省一个小 trait impl（§3 末行：新 kind 很便宜）去动一个 Tier-A 判定式，划不来。

**被否的备选 B：每天一份 wave/report**（r1 D2 的备选 A，本轮重新评估）。它能天然拿到日期身份和契约文体，但**每个 wave 都要物化一个 managed 工作区并 `git init`**——一年 365 个 git 仓库。这条成本 r1 没算，现在算清楚了：否。

**被否的备选 C：塞进现有 `wave-report`**（r1 的 D1/D2）。见 §0.1。

### D2 —— Today 主体渲染选中日的 `daily-log`，日历是它的索引

日历从「换一份 wave 列表」变成「翻文档的日期」——这是它第一次有真正的用途。选中日无卡 = 空态。

### D3 ~~块时间戳~~ —— 删除

见 §0.3。日期身份由 D1 的载体携带。

### D4 —— 素材源：先扩 `list_waves_window`，MCP 工具只包一层

§0.5 的发现改变了这条的形状。分两层：

**第一层（REST，扩既有端点）**：`GET /api/waves?since&until` 已经是为日历窗口做的，且无调用方。给它加**计数投影**（lifecycle 变迁数、report 编辑数、task completed/failed），成为工作区活动窗口的**唯一 server-side projection**。FE 和 MCP 共用它——这同时解决了 §0 的 D5 问题（见下）。

**第二层（MCP）**：`calm.day.activity { since, until }` 是第一层的薄封装 + 授权闸，**不重新实现查询**（`feedback_mirror_code_must_call_the_original`：复述一遍必然产错）。

**event-kind allowlist 必须显式列出**，且带两条注释：`harness.item.added` **在 prune allowlist 里**（所以 `turns` 要么删掉，要么改成一个可实现且不依赖可 prune 事件的定义——r2 倾向**删掉 `turns`**）；`at` 是墙钟而 `id` 是游标，窗口查询按 `at`，**不得与 id 游标混用**。

**授权**（本设计唯一的风险面，形状取自 #951 裁决，不重新发明）：

- descriptor 保持 `visible_to_roles: &[]`，只在**已验证身份**的 list 分支做 contextual augmentation；`tools/call` 不看 list，所以 handler **独立重查**。两处都闸。
- 收敛成**唯一一个 gate helper** `day_activity_allowed(identity)`：role **仅 Assistant**（D5 只需要它）+ active session + wave 行存在 + `purpose='launchpad'` + cove/card 归属一致。unresolved / cross-session / dormant / missing row / DB error **一律拒**。
- 判据只从解析出的 `ToolCallIdentity` 推导，**绝不从 args 取**。
- 参数受限：`until <= now`，窗口至多一日。
- 返回值最小化 + 脱敏 + **三重截断**（wave 数 / 每 wave 条目数 / 总字节），纪律照抄 `report_links.rs` 的既有先例，而不是 r1 那个光秃秃的 `truncated?`。

### D5 —— 手动触发；「没有活动就不发起会话」判据来自 D4 第一层

Today 一个动作「写今日进度」→ `POST /api/waves/{launchpad}/conversations`，**带 `Idempotency-Key`**（§3；key 的语义要在实现里明确是「同请求重试」而非「同日汇总」——后者由 D1 的确定性 card id 负责）。prompt 指示 agent：`calm.day.activity` → 写 `daily-log`。内核写通道零改动。

**空活动不发起**：判据用 **D4 第一层的同一份 projection**，不是 r1 那个「FE 已加载的 waves 快照」——后者只有当前 lifecycle，看不到 report 编辑、task 结果、历史 lifecycle 变迁，能证明的只是「快照为空时没调用」，不是「活动窗口为空时没调用」。

定时汇总属于 #120，不做。

### D6 —— 边界写在服务端，不写在 prompt 里

§0.7 说明 r1 的 D6 是假边界。r2：

- **卡级硬上限**：单张 `daily-log` 的块数与总字节数由 `CardKindHandler::validate_payload` 拒绝越界——这是内核约束，不是 agent 自觉。**顺带补上 prose 块的尺寸上限**（现在没有，见 §3）。
- **保留窗口**：超过 N 天的 `daily-log` 卡由**服务端 GC** 删除（`calm.admin.wave_gc` 是既有先例），不由「用户手动汇总且 agent 恰好遵守 prompt」这条脆链触发——连续无活动、agent 失败、CAS 冲突都会让 r1 的清理漏跑。
- **诚实标注**：删除内容并不必然减少占用——`wave.report_edited` 类事件永久保存完整前后文。所以 GC 的收益是「读取路径不再变长」，不是「磁盘变小」。这一条要写进实现的 doc comment，免得下一个人以为 GC 是空间手段。
- N 的取值仍是开放问题（§9 Q2）。

### D7 —— FE 形态：文档是主体，状态条守住 §8.1

`fe-design.md` §8.1 的约束不因改版而丢：**用户打开这一页是要回答「有什么在等我？」**，答案归位置和 `--warn` 像素，不归字号。

- 顶部一行状态条：`N waiting · N running` + 等待中的 compact 行，排在文档之前。
- 主体：`ReportDocument` 渲染选中日的 `daily-log`。空态「今天还没有进度」+ 触发按钮（无活动时按钮不出现，D5）。
- 右面板：日历（选日切主体）+ Running / Recent 降级到这里 + 现有 Conversations 模块不动。
- terminal 占位 section 原样保留。

## 5. 契约与不变量

### 5.1 launchpad 解析（r1 版本不可达，见 §0.2）

```
POST /api/today/launchpad/ensure   （幂等，无条件调用）
  → wave_id
  → GET wave detail，从 cards 里取 daily-log 卡
```

三条随之而来的要求：

- **不从 `workspace.waves` 找 launchpad**。system cove 被 `GET /api/coves` 过滤掉是 #175 的既定行为，把 system wave 暴露进普通 workspace 扇出是错误的修法。
- `ensure` 幂等，所以**无条件调用**比「先查后建」简单且更少一次往返。它的 `is_unique_constraint` 分支已经在处理并发撞 `idx_waves_one_launchpad` 的情况——PR1 要确认它覆盖的正是这个索引名。
- `ensure` **从未在真实客户端上跑过**（§3 第二行），所以它是新路径，要按新路径验，包括并发。

r1 的 INV-TODAYDOC-002（read-first + 非缺失错误必须报错）随此重写：**任何 `ensure` 失败都必须浮出来，不得静默重试或降级成空态**——理由与 terminal 链的 INV-TODAYTERM-001 相同：静默会把「读失败」变成「悄悄铸了第二份」。

### 5.2 不变量表

r1 的 review 指出原表里有五条**证不死**、两条**是全称否定却只打算列举验证**。r2 逐条改成有载体的形式。

| ID | 陈述（可证死的形式） | 反例（必须红） | 正例（必须绿） |
|---|---|---|---|
| INV-TODAYDOC-001 | 只有一条写 `daily-log` 的 API，且 card id 由确定性函数唯一决定 | 绕开该函数构造 card id 的调用点 | 全部写入过该函数 |
| INV-TODAYDOC-002 | `ensure` 的任何失败都浮出为错误，不静默重试/降级 | 5xx 被吞成空态 | 5xx 浮出错误框 |
| INV-TODAYDOC-003 | 分日只依赖 card id，前端不存在按文本解析日期的路径 | 改写卡内 heading 文本后分日结果改变 | heading 任意变化，分日不变（metamorphic） |
| INV-TODAYDOC-004 | `day_activity_allowed` 是唯一判据，且当且仅当 predicate 成立时放行 | transport 的任一分支（unresolved / cross-session / dormant / missing row / DB error）绕过 | 全分支 iff 断言 |
| INV-TODAYDOC-005 | 返回体字段是 **allowlist**（schema 层无 path / 无 body 字段） | schema 里出现 path 或正文字段 | 字段集合等于 allowlist |
| INV-TODAYDOC-006 | 同日重复汇总落到同一张卡 | 出现两张同日 `daily-log` | 第二次 upsert 覆盖 |
| INV-TODAYDOC-007 | 活动窗口（D4 第一层 projection）为空时不发起会话 | 空窗口起了会话 | 空窗口直接空态 |
| INV-TODAYDOC-008 | 卡级块数/字节上限由 `validate_payload` 拒绝；保留窗口由服务端 GC 执行 | 越界 payload 被接受；第 N+1 天的卡在无人触发时仍存活 | 越界 400；GC 后只剩 N 天 |

三条方法论备注，来自 r1 review：

- **INV-004 是全称否定**，身份空间不止四个 `CardRole`——还有 unresolved daemon、card-bound/no-thread、cross-session、dormant、missing card/wave、`purpose` 为 null/其它、DB error。有限枚举证不了开放世界，所以形式必须是「gate helper 的 iff 测试」+「transport 每个分支的集成测试」，不是列举几个身份。
- **INV-005 的值级「无绝对路径」不可证明**——合法的 `title` / `cove_name` 本身就可以是 `/home/x`。能证的是 **schema 层的字段 allowlist**，所以陈述改成那个。
- **INV-003 用变异测试证死**：固定时间戳，把卡内 heading 换成任意合法/非法文本，分日结果必须**不变**。r1 那句「乱码 heading 变异」抓不到「只对合法日期 heading 生效」的隐藏分支，所以改成 metamorphic 形式。

## 6. 切片计划

r1 声称三个 PR「各自可独立验证」，review 指出这是**自相矛盾**的（PR3 同时依赖 PR1 的日期身份和 PR2 的 projection，而 §9 的开放问题恰好落在 PR1 里）。r2 显式给出 DAG。

```
PR1 ──┐
      ├──> PR3
PR2 ──┘
```

| PR | 内容 | 交付 / 证死的风险 |
|---|---|---|
| **PR1**（~1.0k） | D1 `daily-log` card kind（handler + 确定性 id + 卡级上限 + prose 上限）+ D2/D7 FE：无条件 `ensure` 解析、主体渲染选中日、状态条、日历索引、空态 | Today 变成文档且能按日翻。**分日风险**用 INV-003 的 metamorphic 测试证死。手写 REST 卡即可完整验证，**不依赖 AI**，也不依赖 PR2 |
| **PR2**（~0.9k） | D4 第一层（扩 `list_waves_window` 的计数投影）+ 第二层（`calm.day.activity` 薄封装）+ `day_activity_allowed` 单一 gate + 截断纪律 | **授权风险**证死：gate 的 iff 测试 + transport 全分支矩阵；返回体 schema allowlist 断言 |
| **PR3**（~0.6k） | D5 触发闭环（动作 + `Idempotency-Key` + prompt + 空活动不发起）+ D6 服务端 GC | 端到端：点一次，今天的卡出现；点第二次不长第二张；无活动时按钮不出现；GC 后只剩 N 天 |

**Q1/Q3（§9）落在 PR1 内，必须在 PR1 开工前关闭。** r1 说「开放问题翻案只影响 PR2/PR3」是错的。

## 7. 风险

- **`ensure` 是冷路径**（§3）。首次上线必然走它，且从未在真实客户端上跑过。按新路径验，含并发撞唯一索引。
- **`calm.day.activity` 是全 MCP 面第一个跨 cove 读**，是一个新类别而非一个更宽的工具。#951 在同一个位置踩过一次（当时「no role_gate change」的表述是半真且危险的）。PR2 的 review 按那份裁决逐条核对，不重新推导。
- **新 card kind 触及 Tier-A 边界**：`schema_version` / `validate_payload` / 前端 zod / goldens。按 `feedback_run_ci_exact_command_locally`，PR1 门禁跑 CI 完整命令（整个 workspace + features + web build），不是子集。
- **0065/0066 显示 #951 的 proposals 通道已被回退**，launchpad 这条线上有过一次撤退。PR1 开工前确认 launchpad wave 在当前生产库里的实际状态。
- **删除不等于省空间**（D6）。不要把 GC 当容量手段宣传。

## 8. 明确不做

- 定时/自动汇总（→ #120）。
- Today terminal 接线（→ 现有 INV-TODAYTERM-*）。
- 保留窗口之外的历史 UI（D6）。
- 手机端形态（→ #1234）。
- 汇总内容的质量评价。本设计只保证「没有素材时不写」（INV-007），不保证「有素材时写得好」。

## 9. 待评审的开放问题

- **Q1**：D1 的 `daily-log` 是**一天一张卡**，还是**一张卡 + 日期键块**？前者日期身份更硬、GC 更简单；后者卡数少、单次读更省。倾向一天一张卡。**落在 PR1 内，开工前须关闭。**
- **Q2**：D6 的保留窗口 N 由什么决定——天数、总字节，还是卡数？r1 的「30 天」是拍的。
- **Q3**：D7 的状态条与文档的排序。等待中的 wave 排在文档之前是 §8.1 的直接推论，但也可能应该反过来——文档才是改版后的主角。**落在 PR1 内。**
- **Q4**：D4 的窗口参数由 agent 传（`since/until`），还是由服务端从 launchpad 身份固定为「今天」？后者少一个可被滥用的参数，且与「窗口至多一日」的限制天然一致。
- **Q5**：时区。全仓没有 workspace timezone 概念，现在的 Today 全部用浏览器本地 `Date` 分日。`daily-log` 的 `YYYY-MM-DD` 由谁定义——浏览器本地、服务端本地，还是新引入一个工作区时区设置？**这条 r1 完全没写，且它决定 card id，所以也落在 PR1 内。**
