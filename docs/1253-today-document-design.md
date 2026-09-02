# Today 文档化 + AI 今日进度

> 状态：设计 **r1**，待双通道 review。Issue：#1253。
> 关联：#951（launchpad wave 与它的 report card 是本设计的载体）、#120（定时汇总属于那条线）、#1045（块时间戳对文档运行时同样有用）、#1234（手机端）。

## 0. 问题

Today 只有「当前状态」，没有「发生了什么」。

`fe/web/src/features/today/public.tsx`（435 行）的主列是三段 `WaveRow` 列表——Waiting on you / Running / Recent——全部由同一份 `waves` 数组 filter 得到；右侧面板是周历，日格上的数字是「那天有几条 wave 活动」，点开换成同一种列表的另一份切片。中间还夹着一个写死 `Terminal is not wired up yet.` 的占位 section。

所以这一页粗糙的根源不是样式，是**它答不出「今天做了什么」**。它现在的全部信息量约等于侧边栏加一个时钟。

本设计做两件事：主列改成文档形态，以及让一个 agent 把当天的活动写成进度段。

## 1. 前提

**一、新 FE 尚未上生产**（沿用 `docs/1147-workspace-design.md` 的同名前提）。破坏性的 DTO 变更可以一步到位，不加过渡层。

**二、老数据不迁移、不兼容**（用户 2026-08-31 定调，范围含生产库 `:4040`）。因此 §4 的块时间戳不做存量回填，也不写兼容守卫。

> ⚠️ 这条**只**豁免老数据。同一次运行内的并发、崩溃后重跑、幂等、失败回滚仍是硬要求。

**三、本设计不碰 Today terminal。** `features/today/README.md` 里的 INV-TODAYTERM-001/003/005/006 描述的是尚未接线的 terminal 解析链，那条链的契约原样保留；本设计只是**共用它的前半段**（解析 launchpad wave），见 §5.1。

## 2. 已确认的既有事实

以下每条都是读代码实测的结论，不是推测。它们决定了这件事的成本远低于「新做一个 Today」。

| 事实 | 位置 |
|---|---|
| launchpad wave 在 ensure 时**已经建了 `wave-report` card** | `crates/calm-server/src/routes/today.rs:229-242`，`report_card_id` 出现在 `EnsureTxResult` 与响应里 |
| `waves.purpose = 'launchpad'` 已存在，且 `purpose` 已在 wire 上 | migration `0064_waves_launchpad_purpose.sql`；`fe/core/api/generated/wire.ts:379` |
| 文档渲染器完整且已在生产路由使用 | `features/report/{document,outline,backlinks,task,candles,table,app}`；`app/router/public.tsx:1534` |
| **AI 写 report 无需任何内核改动**：`CardRole::Assistant` 已被允许写块 | `mcp_server/tools/wave_report_blocks.rs:101,114,245,295,326`；角色定义 `calm-types/src/model.rs:28-32` |
| 起一条 assistant 会话就是一个 REST 调用 | `POST /api/waves/{wave_id}/conversations {text}`，`routes/wave_conversations.rs:57` |
| 人写块与 agent 写块是两条通道，互不冒充 | REST 要求 `X-Calm-Actor: user`（`wave_report_blocks.rs:74-81`），agent 走 MCP |
| 结构性事件永久保留，不受 30 天 prune 影响 | `docs/events-retention.md`：allowlist 只含 `claude.hook` / `codex.hook` / `harness.*` / `overlay.set`，`wave.*` / `card.*` 由构造永久 |

**结论：文档的载体、渲染器、写入通道、事件源全都已经在了。**缺的是四样东西——FE 没读过这份 report、没有分日的事实、agent 看不到跨 cove 的活动、以及这份文档没有增长边界。

## 3. 缺口

### 3.1 素材源（最大的一块）

MCP 工具全表实测：`calm.admin.*`、`calm.cove.outline`、`calm.plan.*`、`calm.ratify.request`、`calm.report.*`、`calm.review.round`、`calm.task.*`、`calm.wave.{cat,cat_at,diff,log,ls,state}`。

**没有一条能回答「过去 24 小时全工作区发生了什么」。**

- `calm.wave.*` 全部按 MCP 身份解析到**调用者自己的 wave**（`tools/wave_file.rs::resolve_wave_for_identity`）。
- `calm.cove.outline` 按 `identity.cove_id` 取报告卡（`tools/report_links.rs:69-82`），且 `visible_to_roles: &[CardRole::Spec]`。而 launchpad wave 在 **system cove** ——从它调这条工具，看到的是它自己，等于什么都没有。

所以「AI 写今日进度」的素材源必须新做，而且它天然是一次**跨 cove 读**，是本设计里唯一的授权风险面。

### 3.2 分日的事实来源

`ReportBlock` 只有 `{ id, kind, rev, payload }`（`calm-types/src/wave_report.rs:16-21`）；文档级只有 `doc_rev`。**块没有时间戳。**

于是「哪些块属于 9 月 2 日」目前只有一条路：解析 agent 写进 heading 的日期文本。那是 mirror-code——前端复述一遍 agent 应该遵守的格式约定，agent 写错一次就丢一整段，而没有任何东西会因此变红。见 §4 的三个备选与取舍。

### 3.3 FE 没有 launchpad 解析链

`grep launchpad fe/web/src` 只命中测试。`ensure` 端点没有生产调用方。

### 3.4 增长边界

块级上限 256KB（`report_blocks/kinds.rs:46`），**文档级没有块数上限**。Today 这份文档按天只增不减，而每次打开 Today 都要把整份 payload 读回来。这是设计必须给出答案的约束，不是可以留给以后的实现细节。

## 4. 决策

### D1 —— 文档的载体是 launchpad wave 现有的 `wave-report` card

不新建表，不新建 card kind，不新建端点。

理由：这张卡已经存在（§2），而 report 是内核的 Tier-A 持久载荷，自带块模型、`doc_rev` CAS、稳定块 id/锚点、outline、反链、`wave.report_edited` 事件、以及一整套 FE 渲染器。任何新载体都要把这些重做一遍。

**被否的备选**：新建 `daily-digest` card kind。否的理由是它会造出第二种「文档」，而 #1045 正在收敛文档运行时的形态；多一种载体等于多一份要对齐的渲染路径。

### D2 —— 单份累积文档，按天分段；Today 默认渲染今天那段

一份 report，按日切段。Today 主体渲染「今天」的段；日历选别的日子，主体切到那天的段。

**被否的备选 A：每天一份 wave/report。** 干净，但要新做每日 bootstrap、每日单例索引、GC，且日历要跨天预览时变成 N 次请求。成本高一档，收益只有「天然分段」——而 D3 用一个字段就买到了。

**被否的备选 B：report 只存今天，每天覆盖。** 否——「我上周三干了什么」正是这一页要答的问题，覆盖等于把它销毁。

**这条决策让日历第一次有了真正的用途**：它从「换一份 wave 列表」变成「翻文档的日期」。

### D3 —— 分段依据是内核盖章的块时间戳，不是 agent 写的 heading 文本

`ReportBlock` 增加 `created_at_ms`（与 `updated_at_ms`），由内核在 upsert 时盖章，**agent 不可写、不可改**；schema 版本随之推进。FE 分段 = 按工作区本地时区把块按 `created_at_ms` 分桶。

**被否的备选 A：解析 heading 里的日期。** 这是 §3.2 的 mirror-code：前端复述格式约定，agent 写歪一次静默丢段。

**被否的备选 B：块 id 编码日期（`day-2026-09-02`），服务端用正则约束。** 比 A 强——服务端校验能把约定变成内核事实——但它把「一天一块」写死进了 id 命名空间，agent 想在一天里既写叙述又写 task 列表就得发明 `day-2026-09-02.2` 之类的后缀，复杂度回流。

**备选 C（保留在 §9 待评审）：从 `wave.report_edited` 事件流派生块的首次出现时间。** 不改 schema，且结构性事件永久保留（§2）。代价是 FE 要拉全量事件才能渲染一页，或者后端要新做一个派生端点——后者的成本已经不比加两个字段低了。

> 语义上的诚实：`created_at_ms` 是「写入日」，不是「这段进度描述的哪一天」。二者在正常路径下重合（汇总当天跑）。补写昨天的进度会落到今天的段里——**这是已知且接受的局限**，不要在实现里用 heading 文本去「修正」它，那会把被否的备选 A 从后门放回来。

### D4 —— 素材源是一条新的只读 MCP 工具 `calm.day.activity`

```
calm.day.activity { since: <ms>, until: <ms> } -> {
  waves: [{ id, title, cove_name, lifecycle, transitions: [...], turns: <n>,
            report_edits: <n>, tasks: { completed, failed } }],
  truncated?: <n>
}
```

数据来自 `events` 表按 `at` 窗口查询 + `waves` 表联查。**只依赖结构性事件**（`wave.*` / `card.*`），因此不受 30 天 prune 横线影响（§2 末行）——这一点要写进工具的 doc comment，否则以后有人往里加 `harness.item.added` 就会静默拿到一个有洞的窗口。

**授权（本设计唯一的风险面）**，形状直接复用 #951 review 的裁决，不重新发明：

- `visible_to_roles` **不足以**作为闸。它只按 role 分，而 launchpad 的 Spec/Assistant 与普通 wave 的 Spec/Assistant 是同一个 role。
- `tools/call` 不看 `tools/list`。**因此 `tools/list` 与 `tools/call` 两处都要闸，且 handler 级 fail-closed。**
- 判据是 DB 持久的 `waves.purpose = 'launchpad'`（§2），从解析出的 `ToolCallIdentity` 推导，**绝不从 args 取**。
- 返回值**最小化 + 脱敏**：不含报告正文、不含会话正文、不含任何绝对路径（attached 仓库路径尤其不能出现——它是用户机器上的真实路径）、不含 cove/wave id 以外的机器标识。

### D5 —— 触发是手动的，且「没有活动就不发起会话」

Today 上一个动作「写今日进度」→ `POST /api/waves/{launchpad}/conversations { text }` 起一条 assistant 会话，prompt 指示它 `calm.day.activity` → `calm.report.blocks.upsert`。**内核零改动**（§2）。

定时汇总属于 #120 的 schedule 线，本设计不做。

**空活动不发起**：FE 在触发前先判活动窗口是否为空（复用已加载的 waves/overlays，不新增请求），空则直接显示空态，不起会话。这条既省一次 agent 调用，也是「agent 不得编造进度」这个要求**唯一可证死的形式**——「必须诚实」证不了，「没有素材时根本没有调用」证得了。

### D6 —— 保留最近 30 天，更早的段由汇总时删除

回答 §3.4。汇总 agent 在写新段之前，用已有的 `calm.report.blocks.delete` 删掉超过 30 天的段。更早的历史由 wave 的 git 快照与事件流兜底，**不做 UI**。

**被否的备选：不设边界。** 否——一份只增不减、每次打开 Today 都要整份读回的 JSON 是确定会出事的，而 #36/#854 的 events 膨胀事故就是这个形状。

### D7 —— FE 形态：文档是主体，状态条守住 §8.1

`fe-design.md` §8.1 的那条约束不能因为改版就丢：**用户打开这一页是要回答「有什么在等我？」**，而那个答案归位置和 `--warn` 像素，不归字号。

- **顶部一行状态条**：`N waiting · N running`，以及等待中的那几条 wave（compact 行）。它排在文档之前。
- **主体**：`ReportDocument` 渲染选中日的段。空态：「今天还没有进度」+ 触发按钮（无活动时按钮不出现，见 D5）。
- **右面板**：日历（选日切主体）+ Running / Recent 降级到这里 + 现有 Conversations 模块不动。
- terminal 占位 section 原样保留（§1 前提三）。

## 5. 契约与不变量

### 5.1 launchpad 解析：read-first，只在缺失时 ensure

```
已加载的 waves 里找 purpose === 'launchpad'
  ├─ 命中 → 用它的 wave-report card
  └─ 缺失 → POST /api/today/launchpad/ensure，用返回的 report_card_id
```

任何**其它**错误（网络、5xx、鉴权）必须作为错误浮出来，**不得静默 ensure**。这与 terminal 链的 INV-TODAYTERM-001 是同一条规矩的同一个理由：静默重建会把「读失败」变成「悄悄铸了第二份」。

注意与 terminal 链的差别：terminal 判据是 404，这里是「已加载列表里没有这一条」，因为 waves 列表已经在手，不该为了拿一个 id 多打一次请求。

### 5.2 不变量表

| ID | 陈述 | 反例（必须红） | 正例（必须绿） |
|---|---|---|---|
| INV-TODAYDOC-001 | Today 的进度只写进 launchpad wave 的 `wave-report` card，没有第二真源 | 任何往别的 card / 表写进度的路径 | 全部进度块都挂在该 card 上 |
| INV-TODAYDOC-002 | launchpad 解析 read-first；非缺失类错误必须报错，不得静默 ensure | 5xx 时静默调 ensure | 缺失时调 ensure；5xx 时浮出错误 |
| INV-TODAYDOC-003 | 分段依据是内核盖章的时间戳，不是 agent 文本 | 前端出现任何按 heading 文本解析日期的分支 | 分段只读 `created_at_ms` |
| INV-TODAYDOC-004 | `calm.day.activity` 对非 launchpad 身份 fail-closed，且 `tools/list` 与 `tools/call` 两处都闸 | 普通 wave 的 Spec 直接 `tools/call` 该工具拿到数据 | 普通身份两处都被拒 |
| INV-TODAYDOC-005 | 活动摘要最小化：无正文、无绝对路径 | 返回体里出现 attached 仓库路径 | 只有 id / 标题 / cove 名 / 计数 |
| INV-TODAYDOC-006 | 重复触发幂等：同一天重复汇总覆盖同一段，不产生第二段 | 点两次出现两段今天 | 第二次 upsert 覆盖 |
| INV-TODAYDOC-007 | 活动窗口为空时不发起会话 | 空窗口仍起了 assistant 会话 | 空窗口直接渲染空态 |
| INV-TODAYDOC-008 | 保留窗口 30 天，超出的段在汇总时删除 | 第 31 天的段仍在文档里 | 汇总后只剩 30 天 |

INV-TODAYDOC-004 是**全称否定**，所以它要的是 fail-closed 的扫地测试（对每一个非 launchpad 身份 × 两个入口的矩阵），不是列举几个身份。INV-TODAYDOC-003 同理：它是「前端不存在这样的分支」，得靠一条针对 heading 文本的变异测试证死——把 agent 写的 heading 改成乱码，分段必须**不变**。

## 6. 切片计划

三个 PR，每个约 1k 行以内，各自可独立验证。

| PR | 内容 | 交付 / 证死的风险 |
|---|---|---|
| **PR1**（~1.1k） | D3 块时间戳（内核盖章 + schema 推进 + FE zod）+ D1/D2/D7 FE 文档化：launchpad read-first 解析、主体渲染选中日的段、状态条、日历选日、空态 | Today 变成文档且能按日翻。**分段风险**用 heading 变异测试证死。用手写 REST 块即可完整验证，不依赖 AI |
| **PR2**（~0.9k） | D4 `calm.day.activity` + launchpad 双入口 fail-closed 闸 + 最小化/脱敏 | **授权风险**证死：非 launchpad 身份 × `tools/list`/`tools/call` 矩阵全拒；返回体断言无路径、无正文 |
| **PR3**（~0.6k） | D5 触发闭环（动作 + prompt + 空活动不发起 + `doc_rev` CAS 幂等）+ D6 保留窗口 | 端到端：点一次按钮，今天的段出现；点第二次不长第二段；第 31 天的段消失 |

PR1 不依赖 PR2/PR3——文档形态本身就是 user-visible 的交付，AI 只是它的一个写入者。这也是有意的切法：如果 §9 的开放问题在 review 里翻案，翻的是 PR2/PR3，PR1 不受影响。

## 7. 风险

- **块时间戳是 schema 变更**，牵动 `calm-types` → `wire.ts` → FE zod → goldens。按 `feedback_run_ci_exact_command_locally` 的教训，PR1 的门禁必须跑 CI 的完整命令（整个 workspace + features + web build），不是子集。
- **`calm.day.activity` 是跨 cove 读**。它是本设计里唯一能泄露用户机器信息的面，而 #951 的 review 已经在同一个位置踩过一次（「no role_gate change」当时是半真且危险的表述）。PR2 的 review 要按那份裁决逐条核对，不要重新推导。
- **`ensure` 目前没有任何生产调用方**。`grep` 全仓：只有 `today.rs` 自己的路由声明、五处集成测试，以及旧 `web/` 客户端的 generated types——新旧两个前端都不调它。所以 §5.1 的 read-first 在**首次上线时必然走 ensure 分支**，且那条分支从未在真实客户端上跑过。PR1 必须把它当作新路径验（包括 §7 第三条的并发），不能假设它是热路径。
- **0065/0066 显示 #951 的 proposals 通道已被回退**，说明 launchpad 这条线上有过一次撤退。结合上一条，PR1 开工前要确认 launchpad wave 在当前生产库里的实际状态（存在 / 不存在 / 存在但从未被使用）。
- **单例由 DB 保证，但并发 ensure 的行为要验**。0064 带 partial unique index `idx_waves_one_launchpad ON waves(purpose) WHERE purpose = 'launchpad'`，所以「两条 launchpad wave」在数据层不可能——read-first 不需要定义取哪一条。剩下的风险在写侧：两个并发 ensure 会撞唯一约束，`today.rs` 已有 `is_unique_constraint` 分支，PR1 要确认它覆盖的正是这个索引名。

## 8. 明确不做

- 定时/自动汇总（→ #120）。
- Today terminal 接线（→ 现有 INV-TODAYTERM-*）。
- 30 天以前的历史 UI（D6）。
- 手机端形态（→ #1234）。
- 汇总内容的质量评价机制。本设计只保证「没有素材时不写」（INV-TODAYDOC-007），不保证「有素材时写得好」。

## 9. 待评审的开放问题

- **Q1**：D3 采纳 `created_at_ms` 盖章，还是备选 C（从 `wave.report_edited` 事件流派生）？取舍点是「两个字段 + schema 推进」对「不改 schema 但要新做派生路径」。倾向盖章。
- **Q2**：D6 的 30 天是拍的。保留窗口该由什么决定——文档字节数、块数，还是天数？
- **Q3**：状态条（D7）与文档的排序。等待中的 wave 排在文档之前是 §8.1 的直接推论，但也可能应该反过来——文档才是这一页改版后的主角。
- **Q4**：`calm.day.activity` 的窗口该由 agent 传（`since/until`），还是由服务端从 launchpad 身份固定为「今天」？前者灵活，后者少一个可被滥用的参数。
