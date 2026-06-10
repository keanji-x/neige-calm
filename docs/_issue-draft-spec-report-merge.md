# Draft — Issue: spec + wave-report 合并为统一 Report 视图 (`ViewMode='report'`)

## Title
`design: spec + wave-report 合并为统一 Report 视图 (ViewMode='report')`

## Labels
- `enhancement`
- `p2` — 前瞻性视觉重设计, 非稳定性 gate

## Body

### 目标

把 wave 工作台从"一张大网格塞所有 card"改成**两套互补视图**:

- **Report view (`ViewMode='report'`)** — 全屏报告. spec + wave-report 两张
  kernel-side card 作为**数据来源**喂进这个 view (中央正文从 wave-report
  payload, chat pill 反映 spec 的当前 harness 状态), **不再作为 card 渲染**.
  布局: 中央正文 + 右侧 docked 侧栏 (Files + Event line) + 左下角悬浮 chat
  pill, 完全按设计稿 `Report.html`.
- **Worker view (`'grid'` / `'list'`)** — 现有 grid/list 行为基本不变, 但
  **过滤掉 spec + wave-report** — 只渲染 worker cards (terminal / codex /
  file-viewer / iframe / plugin cards). 这两张特殊 card 永远在 report view
  里被 "吸收", 不在 worker view 里以 card 形式出现.

两个视图互不重叠: report view 只看报告, worker view 只看 workers.

**已敲定的产品决策**: `spec.tsx` 的 `ChatTimeline` UI **从产品里整体移除**
— 没有任何模式继续挂载完整 transcript. 唯一暴露 spec 状态的地方是 report
view 的 chat pill (当前 phase + 最近 tool + 追问输入). spec 在 kernel 数据
模型里仍存在 (harness state, message log 不丢), 只是 UI 层不再渲染历史滚动列.

### 设计稿

**位置**: `web/public/_design/Report.html` (单文件, 自带 vanilla JS, 无 React
依赖)。本地预览: `http://localhost:5175/calm/_design/Report.html`。配套:
`chat1.md` (设计意图), `screenshots/states.png` (渲染状态), `README.md`
(handoff)。

**三块区域**

1. **中央正文 (`.center`)** — `inset: 0 280px 0 0`, `max-width: 640px` 居中。
   编辑感排版: `Newsreader` serif H1 (40px), `JetBrains Mono` byline meta,
   numbered section (`<h2><span class="n">01</span>`), Key findings 卡 (88px
   stat 列 + prose 列), 条纹背景的 placeholder chart, 表格, mono 上标引用
   (`<a class="ref">`)。
2. **右侧 docked 侧栏 (`.sidebar`)** — `position: fixed; right: 0; width: 280px`,
   `border-left: 1px solid var(--hair)`。两个 `<aside class="section">` 上下
   叠放, 都 `open` 时各占 `flex: 1 1 0`, 折叠一个另一个撑满。
   - **Files** — 文件夹树, 两层顶层 (`Sources` / `Generated`), 一层嵌套
     (`raw-datasets`)。每行 mono 文件后缀徽章, 选中态用 `--accent` 染色。
   - **Event line** — 时间轴 + 节点 dot, 三种 tone (default / `.accent` / `.amber`),
     标签 (`agent` / `alert` / `data` / `source` / `init`) 用 mono uppercase。
     右上角 LIVE 指示灯。
3. **左下角悬浮 chat (`.chat`)** — `position: fixed; right: 308px; bottom: 26px`
   (避开右侧栏)。默认是 `Ask the Research Agent` 药丸; 点击展开成 360px
   glass box (`backdrop-filter: blur(16px)`), 只有标题 + textarea + 送出按钮,
   **无消息历史**。

**Token 系统**: 纯 CSS 自定义属性, OKLCH 色彩 (`--canvas/--ink/--accent/--amber/
--hair*`), 三套字族 (`Newsreader` serif / `Schibsted Grotesk` sans /
`JetBrains Mono`), 一根 `--shadow` 堆叠。

**交互** (~30 行 vanilla JS): section 折叠/展开, folder 展开/折叠, file 选中
高亮, chat 药丸/box 切换, textarea 自动 grow (max 96px)。

**响应式**: `@media (max-width: 1240px)` 把 sidebar 缩到 248px, chat 跟着改成
`right: 276px`。

### 动机

当前 wave 工作台两张卡上下叠放 — chat-stream 的 `spec` (进行中的过程
telemetry) + `wave-report` (沉淀的成稿)。已有 audit
(`docs/_research-spec-report-redesign-codex.md`,
`docs/_research-spec-report-redesign-subagent.md`) 已识别: 视觉债真实, 但
根因是**页面拓扑** — "过程"和"成品"共用同一根 `.card-head` chrome, 视觉
系统把两者画成了亲生兄弟, 眼睛失去锚点。单页 Report 布局把对话收成
current-state pill, 把侧栏让给 files + 系统事件 — 时态分离, 两侧都能呼吸。

### 当前状态 (一句一项)

- **`web/src/cards/builtins/spec.tsx`** — 完整对话 transcript:
  user/agent message, function_call / function_call_output, reasoning,
  mcp_tool_call, 同时承担 harness FSM (status chip + phase) 的拥有方。
  `ChatTimeline` (`spec.tsx:605`) 是历史滚动列。
- **`web/src/cards/builtins/wave-report.tsx`** — 沉淀成稿: H1 + `parseSections`
  解析的章节、Markdown body、attention section、编辑态。TSX 本身健康,
  视觉债在 `calm.css` token 层。`ReadOnlyView` 存在但未 export
  (`wave-report.tsx:314`; export list 在 `wave-report.tsx:543`)。
- **`web/src/pages/Wave.tsx`** — 拥有 2 态 `ViewMode = 'grid' | 'list'`,
  overlay 持久化 (`Wave.tsx:37`, `:271`), 用 `aria-checked` 的 switch toggle
  (`Wave.tsx:264`, `:274`)。
- Wave 级**没有**文件清单数据源 (`Wave.tsx:84` 只传 `wave.cards`)。
- Wave 级**没有**系统事件 timeline 数据源; `eventBridge` 的 ring 是 dev-only
  (`?trace=1`, `eventBridge.tsx:57`, `:66`), 不能直接当生产 Event line 用。

### 数据流

```
useWaveDetailQuery(waveId)
  └── detail.cards.map(adaptCard) → uiWave.cards (full list, kernel-side)
       │
       ├── ViewMode='report' →  WaveReportPage (新)
       │     ├── 中央正文      ← first card.type === 'wave-report' → ReadOnlyView
       │     ├── chat pill    ← first card.type === 'spec' → SpecCurrentRun (新抽出)
       │     ├── Files 侧栏    ← 新 feed (见 PR3 决策)
       │     └── Event line   ← 合成 + 新 feed (见 PR4 分类)
       │
       └── ViewMode='grid'|'list' → WaveGrid / WaveList
             └── uiWave.cards.filter(c => c.type !== 'spec' && c.type !== 'wave-report')
                   → 只渲染 worker cards
```

`waveRoute` (`router.tsx:118`) 数据通路不变; `WaveReportPage` 与
`WaveGrid` / `WaveList` 同级, 由 `pages/Wave.tsx` 按 ViewMode 分支选择渲染.
两个特殊 kind (`spec`, `wave-report`) 在 worker view 里被显式过滤掉; 这条
过滤规则应当在 `pages/Wave.tsx` 进入 `WaveGrid` / `WaveList` 前应用, 而不是
在 `WaveGrid` 内部, 这样 list 视图也享受同样语义.

### 分阶段提案

切成可独立 review 的 PR, 每个 PR 在自己的 worktree 里实现 + 双路 review。

**PR 1 — spec inline-style 抽取 (无 UX 变化)**
把 `spec.tsx` 7 个 inline-style 岛 (HarnessStateChip / TimelineBubble /
TimelinePre / reasoning details / ChatTimeline scroller / MCP status·code /
attribution) 收编到 CSS class; 按上轮 audit
(`_research-spec-report-redesign-codex.md:13`) 引入 `--surface-bubble-*` 与
`--font-prose-*` token 家族。PR1 是 **spec 本身的 visual-debt 清理, 不直接为
PR2 铺路**; 设计稿 token (`--canvas/--ink/--accent/--hair`) 来自
`Report.html:11-33`, 与 PR1 token 几乎无重合。工作量分摊: spec.tsx 50% /
calm.css 35% / wave-report.tsx 10% / status 5%。

**PR 2 — `ViewMode='report'` 骨架 + spec/wave-report 在 worker view 隐藏**
- PR2 启动前必须先回答 4 件事:
  - `wave-report` zero/one/many 策略: `registry.ts:364` 按 exact kind claim
    配对, 不保证一张. zero → 显式 empty state + invariant warning; one →
    正常 `ReadOnlyView` 渲染; many → 取 sort 最小者 + duplicate banner, 不
    silent first-only.
  - `OVERLAY_VIEW_MODE_SCHEMA_VERSION` (`schemaVersions.ts:36`,
    `Wave.tsx:171`, `:178`): 扩 enum 不 bump version.
  - AddPanel 在 report 模式 hide (`Wave.tsx:290`, `router.tsx:369`).
  - PR2 不把 `Report.html:241-298` 的 hard-coded `Sources` / `Generated`
    假文件带进生产; Files / Event line 默认渲染为 "no source connected yet"
    空态, report 模式开放给用户前必须等 PR3 + PR4 都 ship.
- `Wave.tsx:37` → `ViewMode = 'grid' | 'list' | 'report'`; `isViewMode` 放宽.
- Toggle (`Wave.tsx:264`, `:274`) 从 `role="switch"` (二态) 改成
  `role="radiogroup"` segmented control (a11y-real 改动).
- **Worker view 过滤**: 在 `pages/Wave.tsx:84` 把
  `uiWave.cards` 传给 `WaveGrid` / `WaveList` 前过滤掉
  `card.type === 'spec'` 和 `card.type === 'wave-report'`. 这条过滤规则配 1
  条 unit test, 防止后续 kind 命名 drift.
- 新增 `web/src/pages/WaveReportPage.tsx` (~180–260 LOC TSX + ~250–350 LOC
  CSS), 渲染设计稿三块区域; 复用 `WaveContext`, `WaveLifecycleBadge`,
  `waveDisplayTitle`, `sharedEventStream`.
- 中央正文: 首个 `wave-report` card → 直接复用 `ReadOnlyView`
  (`wave-report.tsx:314`). 需先 export `ReadOnlyView` 或抽
  `WaveReportReadOnly` 组件.
- Files 侧栏 + Event line: 先用 sandbox 的占位内容; PR3/PR4 替换.
- Chat pill: 占位 current-state surface; PR5 填充.
- Report 模式 **不挂载** `WaveGrid` (避免动 seeded layout overlay;
  `WaveGrid.tsx:151`).

**PR 3 — Files tree 数据源 + UI**
今天 wave 级没有"文件清单"概念。PR3 设计 doc 要先定:
  a. 新建 `wave.files` kernel feed (source-of-truth, 持久, 需要
     `crates/calm-server/` 后端工作)
  b. 从 `file-viewer` card 的 path 合成 (`file-viewer.tsx:669`, 信息不完整
     但不动后端就能 ship)
前端: 按设计稿结构 (`Report.html:233`) 实现可折叠文件夹树。

注意: 选项 (b) 严重 lossy — `file-viewer.tsx` payload 只有单个 path
(`file-viewer.tsx:27`, `:44`), 运行时只列一层 entries (`file-viewer.tsx:348`,
`:690`), 不能反推完整 Sources / Generated 树。生产采用 (b) 必须在 UI 上
明确标注 "partial / recently opened only"。

**PR 4 — Event line 合成器 + (可选) 后端 feed**
MVP 事件分类 (per `_research-report-merge-feasibility-codex.md`):
- `report.created` — 从 `card.added` + `wave-report` kind 合成
- `report.updated` — 从 wave-report `card.updated` + `wave.report_edited` 合成
- `wave.lifecycle_changed` — 前端已发布 (`invalidationPolicies.ts:100`,
  `generated-events.ts:224`)
- `data.feed` (`synced/refreshed/source_connected`) — **需要新后端 feed**
- `alert.anomaly` — **需要新后端 feed**
前三类纯前端合成 ship; 后两类要数据真正存在后另起一个后端 PR。

注意: (b) 若被选, Event line 的 `data.feed` 样本事件
(`Report.html:336-352` 的 "rows added to market-data-q2.csv") 失去 stable file
source-of-truth, 需重写事件 schema, **PR3 和 PR4 不可并行**。

**PR 5 — Chat pill = 当前 agent 状态 + 追问输入 + spec 卡退役**
**排在 PR2 之后实现** (要等 `WaveReportPage` 落地有地方接 `SpecCurrentRun`).
两件事一起做:

1. 从 `spec.tsx` 抽出 `SpecCurrentRun` 组件, 暴露 report view chat pill 需要
   的字段:
   - Harness FSM + phase (`spec.tsx:818`, `:840`, `:842`)
   - 最近一个活跃 tool / function_call 名 (不显示 output body)
   - 单行追问 textarea
   - **Reset session 入口** — 当前在 `SpecCardImpl` (`spec.tsx:863-879`,
     `:923`), 抽到 chat pill 的展开态. 这是新决策的输入 (见 Open
     decision #3).
2. **退役 `SpecCardImpl` 的 card 渲染**: `ChatTimeline` 不再有挂载点,
   `SpecCardImpl` (`spec.tsx:811`, `:833`, `:890`, `:894`) 简化为一个
   "kernel-side 占位" 或直接从 builtin registry 退出 (但 kind claim 仍占
   `wave-report` 同样的 exact 名, 否则会破坏 kernel 数据). `spec.tsx` 大量
   代码会变成死码 — 应在 PR5 内一并删除.

实现细节: `SpecCardImpl` 同时 own status / phase / reset / timeline 并直接
渲染 `ChatTimeline`; 抽 `SpecCurrentRun` **必须组件化**, 不能靠 `hideTimeline`
prop. 删除 transcript 是不可逆改动, 任何对历史的诊断需求需要先在 PR5 设计
doc 里明确(e.g. `/spec/{cardId}/transcript` 的 dev-only 路由保留).

### 已解决的产品决策 (本 issue 写入主线)

- **报告作为 view, 不作为 card**: spec + wave-report 在 worker view 隐藏,
  只能从 report view 进入. 见"目标".
- **transcript 整体退役**: `ChatTimeline` UI 从产品里彻底移除, 唯一 spec
  状态可视面是 report view 的 chat pill. 见"目标"和 PR5.

### Open decisions (PR1 启动前需用户拍板)

1. **Files 数据源 source-of-truth** — kernel `wave.files` (PR3a 后端) vs
   从 `file-viewer` card 合成 (纯前端). 前者是长期答案, 后者 ship 得快.
2. **`data.feed` / `alert.anomaly` 事件所有权** — 哪个 kernel actor 发?
   dispatcher? 新的 "research-pipeline" 组件? 还是这些本质上是 spec 下游的
   信号, 需要另一套框架?
3. **spec reset session 新归宿** — `SpecCardImpl` 上的 reset/session 路径
   (`spec.tsx:863-879`, `:923`) 是否搬到 chat pill 展开态? 备选: report
   header 上一个 ⋯ 菜单 / 完全移到 keyboard shortcut + command palette.
   决定 PR5 chat pill 的高度和交互密度.
4. **wave 默认视图模式** — 同时有 `wave-report` + `spec` 卡的 wave 是否
   默认进 `'report'`? 安全起见多半保留 `'grid'` 直到产品数据建议反转,
   用户主动 opt-in. 同时, 没有 `wave-report` + `spec` 的 wave (e.g. 只有
   terminal 的 wave) 在 toggle 上要不要 hide `'report'` 选项?

### Out of scope / non-goals

- 删除或重写 `spec.tsx` / `wave-report.tsx` 的 parser。TSX 大体健康, 视觉债
  在 token 层 (`_research-spec-report-redesign-codex.md:40`)。
- 动 `CardHead` 适配新设计。对其它 card kind 连锁风险高
  (`_research-spec-report-redesign-subagent.md:271`)。
- 把 dev 的 `eventBridge` trace ring 改造成持久 Event line timeline
  (`eventBridge.tsx:57`, `:66`)。
- 新开 git 仓 / monorepo。本仓已是多 crate workspace + `web/` + plugins;
  新页面落 `web/src/pages/` 即可。

### 已识别风险

- `WaveContext.ts:1-22` 只暴露 `id/lifecycle`; `WaveReportPage` 需要的
  cove / title / files / progress **必须由 props/loader 显式传**, 不能假设
  Context 已有 (`Wave.tsx:186`)。
- PR1 token 命名要避开 `wave-report-*` 前缀 — spec 现在借用
  `wave-report-card/body` 壳 (`spec.tsx:414`, `:882`, `calm.css:2578`);
  贸然改 token 可能把 report / spec 绑死。
- 设计稿引 Google Fonts (`Report.html:7`); 生产用本地字体 + e2e 显式 block
  Google Fonts (`a11y-axe.spec.ts:219`)。移植时 **必须用 project 字族 token**,
  不能把远程字体带进 app。
- `eventBridge.tsx:238` 当前职责仅 cache invalidation + DEV trace ring
  (`eventBridge.tsx:57`, `:66`); PR4 必须有自有合成/回放, 不能消费 trace buffer。

### 依赖

- 已有研究: `docs/_research-spec-report-redesign-codex.md`,
  `docs/_research-spec-report-redesign-subagent.md`,
  `docs/_research-report-merge-feasibility-codex.md`。
- Sandbox: `web/public/_design/Report.html` (已落地)。
- 提交时无 blocking PR。

### 测试与契约影响

- **测试**:
  - PR1 → 更新/补 `spec.test.tsx` (`:209`, `:654`) 行为测试; 不是 snapshot,
    但若有 baseline 文本断言要核对。
  - PR2 → 新增 `WaveReportPage.test.tsx` (zero / one / many cases + overlay
    schema + AddPanel policy + 三态 toggle a11y); 扩展 `Wave.test.tsx:95`
    路径覆盖。
  - PR5b → 新增 `SpecCurrentRun.test.tsx`。
- **契约**:
  - PR3a 若新建 kernel `wave.files` feed → 必须走 OpenAPI 重建
    (`web/package.json:22`) + `crates/calm-server/tests/openapi.rs:1`
    契约测试, per `feedback_rust_pr_gates`。
  - PR4 若新增 `data.feed` / `alert.anomaly` 事件 → 走 generated-events
    重新生成 + `invalidationPolicies.ts` 加 case。
- **e2e / a11y**:
  - 现 axe 矩阵 (`web/e2e/a11y-axe.spec.ts:6`, `:323`) **不覆盖 report path**;
    PR2 必须加。
  - 现键盘断言 (`a11y-keyboard.spec.ts:612`, `:681`) **仍假设两态 switch**;
    PR2 必须改成三态 radiogroup。
- **视觉回归**:
  - `__color_anchor__/ColorContactSheet.tsx:14-81` 只覆盖 form 输入, 不覆盖
    spec / wave-report / sidebar / center / chat。
  - PR1 token 改动 + PR2 新页面 = 需新增 anchor 或在 PR1 / PR2 显式补
    Playwright 截图基线 (`web/e2e/__snapshots__/color-anchor-baseline.md:5`)。
  - grid / list 视觉验收必须通过新增 color anchor + 截图基线验证, 不能只靠肉眼。
- **i18n**: 项目 `web/package.json:24` 无 i18n 框架; 设计稿 `Files`,
  `Event line`, `Ask the Research Agent`, 文件类型徽章等是硬编码。本次不引入
  i18n; 所有新文案沿用项目现有英文硬编码惯例。

### Acceptance criteria

- `ViewMode='report'` 被 `isViewMode` 接受, overlay 持久化与
  `'grid'`/`'list'` 行为一致。
- Toggle 键盘可达 (radiogroup, 方向键) + 屏幕阅读器可读。
- 同时挂载 `wave-report` 和 `spec` 卡的 wave 进入 report 模式:
  侧栏折叠正常、chat pill 展开正常、`wave-report` payload 实际渲染为正文、
  spec FSM phase 真正驱动 chat pill 状态。
- 同 wave 在 `'grid'` / `'list'` 模式视觉无变化, 并通过新增 color anchor +
  截图基线验证。
- Report 模式下 `WaveGrid` 不被挂载, layout overlay 不被触碰.
- Files 侧栏与 Event line 渲染真实内容; report 模式开放给用户的前提是
  PR3 + PR4 都已 ship.
- Worker view (`'grid'` / `'list'`) 渲染的 card 列表里 **不出现** `spec`
  或 `wave-report` (配对应的 unit test).
- Reset spec session 仍可从 report view 的 chat pill 内触达, 触达 ≤ 2 次点击.
- `SpecCardImpl` 的 chat-bubble / tool-output / reasoning details 等历史渲染
  代码在 PR5 落地后从 `spec.tsx` 中删除 (净代码减少, 不留 dead code).
