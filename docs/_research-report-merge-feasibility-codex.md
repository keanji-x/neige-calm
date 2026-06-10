# Report merge feasibility audit

## 1. Chat history 归宿
- 设计稿的结构是 docked 左栏 + 中央 report + 右下 chat: `.sidebar` 在左, `.center` 从 `left:280px` 开始, `.chat` fixed 到右下。`web/public/_design/Report.html:88` `web/public/_design/Report.html:92` `web/public/_design/Report.html:203`
- 设计意图把 event line 定义成"数据更新 / 系统事件", chat 定义成"追问报告内容"; 后续明确"对话不用显示history...当前状态"和"点击展开...不需要气泡推荐"。`web/public/_design/chat1.md:26` `web/public/_design/chat1.md:27` `web/public/_design/chat1.md:72` `web/public/_design/chat1.md:107`
- 当前 `ChatTimeline` 是 100% 高度的 scroll column, 每条 harness row 都 map 到一个 `HarnessItemView`; 因此它是完整历史, 不是 current-state pill。`web/src/cards/builtins/spec.tsx:736` `web/src/cards/builtins/spec.tsx:766`

| 当前状态 | 视觉权重 | Report 页是否接住 |
|---|---:|---|
| `user_message` / `agent_message` | 高: full bubble + Markdown, 用户气泡有 attribution。 | 不默认保留历史; 只保留最近问题/回答摘要或输入上下文。`web/src/cards/builtins/spec.tsx:480` `web/src/cards/builtins/spec.tsx:488` |
| `reasoning` | 中低: `<details>` 折叠面板, 打开后是 Markdown。 | Report 默认可丢; 需要时留到 debug transcript。`web/src/cards/builtins/spec.tsx:496` `web/src/cards/builtins/spec.tsx:499` |
| `function_call` / `web_search` | 中: compact bubble, 展示函数名/参数或 query。 | 不能进 event line; 可压成"当前正在调用 X"。`web/src/cards/builtins/spec.tsx:526` `web/src/cards/builtins/spec.tsx:537` |
| `function_call_output` / `local_shell` | 很高: `<TimelinePre>` 可长输出, shell 会拼 `$ command + output`。 | Report 默认不显示; 只显示 last action + status, 输出留在 transcript。`web/src/cards/builtins/spec.tsx:534` `web/src/cards/builtins/spec.tsx:540` |
| `mcp_tool_call` | 中到高: MCP bubble + status + args + error/result pre。 | 保留当前 MCP 名称/状态, 不保留历史结果块。`web/src/cards/builtins/spec.tsx:546` `web/src/cards/builtins/spec.tsx:560` `web/src/cards/builtins/spec.tsx:586` |
| harness FSM / phase | 高: CardHead status chip 展示 raw state + phase, phase 来自 `harness.phase.changed`。 | 必须接住; 否则长任务时用户看不到"正在 X"。`web/src/cards/builtins/spec.tsx:818` `web/src/cards/builtins/spec.tsx:840` `web/src/cards/builtins/spec.tsx:890` |

| 选项 | cost | risk | 可复用 |
|---|---|---|---|
| a. 全移到右下 chat box | 中: 扩展 pill header/body 即可。 | 输出和 tool args 会挤爆极简 chat, 违背"不需要气泡推荐"的收敛。`web/public/_design/Report.html:484` `web/public/_design/chat1.md:107` | `HarnessStateChip` + phase stream。`web/src/cards/builtins/spec.tsx:122` `web/src/cards/builtins/spec.tsx:840` |
| b. 移到左侧 event line | 低。 | 语义错: event line 是 wave/data/system, harness turn 和 function call 是 conversation/process。`web/public/_design/chat1.md:26` `web/src/app/invalidationPolicies.ts:126` | 只可复用 WS 订阅, 不推荐。`web/src/cards/builtins/spec.tsx:684` |
| c. 独立 running surface | 中高: 新 surface + z-index + responsive。 | 设计已把两栏收进左侧、chat 收成小框; 再加第三 live 面会碎片化。`web/public/_design/chat1.md:133` `web/public/_design/Report.html:485` | FSM chip, last harness item, lifecycle badge。`web/src/shared/components/WaveLifecycleBadge.tsx:35` |
| d. 改产品语义: Report chat = current agent control/status, 不是 transcript | 中: 抽 `SpecCurrentRun` + 保留 transcript 在 grid/list/debug。 | 需要明确"历史在 Report 不显示"但"当前运行可见"。`web/public/_design/chat1.md:72` | 复用 status chip、phase、最近 harness item; 不复用 `ChatTimeline` DOM。`web/src/cards/builtins/spec.tsx:605` `web/src/cards/builtins/spec.tsx:818` |

推荐 d: Report 页把 chat pill 语义改成"当前 agent 状态 + follow-up input"; running 时显示 `Working / phase / last tool`, idle 时只显示输入。这样尊重 no-history 设计, 又不把 function_call 塞进 event line。`web/public/_design/chat1.md:72` `web/src/cards/builtins/spec.tsx:842`

## 2. Event line 数据源映射
- 今天 WS bridge 的粗事件族是 `cove.updated/deleted`, `wave.updated/deleted`, `card.added/updated/deleted`, `overlay.set/deleted`, `plugin.state`; policy 里还已有 `wave.lifecycle_changed`, runtime, harness, `wave.report_edited`, job/task 事件。`web/src/app/eventBridge.tsx:8` `web/src/app/eventBridge.tsx:14` `web/src/app/invalidationPolicies.ts:89` `web/src/app/invalidationPolicies.ts:164`
- `EventBridge` 现在的职责是 cache invalidation, 不是产品 timeline; dev trace ring 也只在 `?trace=1` 下暴露。`web/src/app/eventBridge.tsx:1` `web/src/app/eventBridge.tsx:49` `web/src/app/eventBridge.tsx:63`

| 设计稿 sample | 最近可用信号 | 状态 |
|---|---|---|
| Report regenerated | `card.updated` on `wave-report` 后 refetch, 手工编辑还有 `wave.report_edited` + companion `card.updated`。 | Need synthesizer: 现有信号是 card/wave 粒度, 不是 product event。`web/public/_design/Report.html:355` `web/src/app/invalidationPolicies.ts:106` `web/src/app/invalidationPolicies.ts:135` |
| Anomaly detected | report prose 可有 attention section, 但没有 alert event。 | Need backend feed: 不能从 Markdown section 稳定反推异常事件。`web/src/cards/builtins/wave-report.tsx:14` `web/public/_design/Report.html:362` |
| New data synced | 无 data-source/card-level rows-added 事件; 只有 generic `card.updated`/`overlay.set`。 | Need backend feed。`web/public/_design/Report.html:369` `web/src/app/eventBridge.tsx:12` |
| Source connected | 当前没有 source model/event; design 中是 feed/indexed 语义。 | Need backend feed。`web/public/_design/Report.html:376` `web/src/app/invalidationPolicies.ts:149` |
| Dataset refreshed | 无 dataset refresh event; `task.completed/failed` 是 dispatcher/spec waiters 消费, 不等于 data refresh。 | Need backend feed。`web/public/_design/Report.html:383` `web/src/app/invalidationPolicies.ts:160` |
| Report created | `card.added` + `wave-report` kind 可 synthesize; wave-report 是 kernel-minted one-per-wave card。 | Need synthesizer。`web/public/_design/Report.html:390` `web/src/app/invalidationPolicies.ts:103` `web/src/cards/builtins/wave-report.tsx:3` |

- Already published 可直接进 MVP: `wave.lifecycle_changed` 作为 `wave.lifecycle` event, 因为 lifecycle 是 kernel contract, badge 已统一 labels。`web/src/app/invalidationPolicies.ts:100` `web/src/shared/components/WaveLifecycleBadge.tsx:9`
- Need synthesizer: `report.created` from first `wave-report` card, `report.updated` from wave-report `card.updated`/`wave.report_edited`; 需要读取 `detail.cards` 才知道 kind。`web/src/app/router.tsx:339` `web/src/cards/builtins/wave-report.tsx:558`
- Need backend feed: `data.synced`, `source.connected`, `alert.anomaly`; 不要把 `harness.item.added`/`harness.phase.changed` 塞进 event line, policy 已说明它们由 SpecCard/ChatTimeline 自己消费。`web/src/app/invalidationPolicies.ts:126` `web/src/app/invalidationPolicies.ts:132`
- MVP taxonomy: `report.created`, `report.updated`, `wave.lifecycle`, `data.feed` (`synced/refreshed/source_connected`), `alert.anomaly`。前三个可前端 synth/live, 后两个要 backend durable feed。`web/src/app/eventBridge.tsx:200` `web/src/app/invalidationPolicies.ts:138`

## 3. Page topology 改造成本
- `ViewMode` 目前只有 `'grid' | 'list'`, overlay 持久化在 `view-mode`, `isViewMode` 只认两值, toggle 是二态 switch。`web/src/pages/Wave.tsx:33` `web/src/pages/Wave.tsx:37` `web/src/pages/Wave.tsx:46` `web/src/pages/Wave.tsx:271`
- 加 `'report'` 要把二态 toggle 改成三态 segmented/menu, 否则 `role="switch"` 语义不成立。`web/src/pages/Wave.tsx:174` `web/src/pages/Wave.tsx:273`
- 新 `WaveReportPage` 建议同级 lazy import, 在 `workbench-main` 内按 `viewMode === 'report'` 分支渲染; 路由数据不用变, 因为 router 已 `useWaveDetailQuery` 后把 `detail.cards` adapt 成 `uiWave.cards`。`web/src/pages/Wave.tsx:22` `web/src/pages/Wave.tsx:314` `web/src/app/router.tsx:304` `web/src/app/router.tsx:338`
- 粗骨架: `WaveReportPage.tsx` 约 180-260 LOC TSX, CSS 约 250-350 LOC; 可直接用 `WaveContext`, `WaveLifecycleBadge`, `waveDisplayTitle`, `sharedEventStream`, `CardEntry` adapted card data。`web/src/pages/Wave.tsx:10` `web/src/pages/Wave.tsx:15` `web/src/shared/components/WaveContext.ts:22` `web/src/app/eventBridge.tsx:200` `web/src/cards/registry.ts:365`

数据流:
```text
waveRoute/useWaveDetailQuery
  -> detail.cards.map(adaptCard)
  -> WavePage uiWave.cards
  -> WaveReportPage
       center prose: first card.type == 'wave-report' -> summary/body
       chat pill: first card.type == 'spec' -> card.id -> status/phase/current item
       files: new report-files feed, fallback file-viewer card paths
       event line: synthesized wave/report events + future backend feed
```
`web/src/app/router.tsx:339` `web/src/cards/builtins/wave-report.tsx:50` `web/src/cards/builtins/spec.tsx:41`

- Center prose: `wave-report` payload is `summary/body`, rendered by `ReadOnlyView`; card is `claim exact kind='wave-report'` and kernel-minted, no AddPanel duplicate。`web/src/cards/builtins/wave-report.tsx:50` `web/src/cards/builtins/wave-report.tsx:314` `web/src/cards/builtins/wave-report.tsx:549`
- Chat pill: `spec` adapts `kind='codex'` only when payload has `spec_harness`, and is kernel-minted-only; registry maps every matching kernel row, so Report mode should select by sort/first and surface duplicate warning if more than one。`web/src/cards/builtins/spec.tsx:81` `web/src/cards/builtins/spec.tsx:962` `web/src/cards/builtins/spec.tsx:965` `web/src/cards/registry.ts:365`
- Files sidebar: no wave-level file list is present in `WavePage` props (`wave.cards` only); nearest existing surface is `file-viewer` with a single `path` payload and file/folder picker schema, so design's Sources/Generated tree is a new feed unless you intentionally synthesize from file-viewer cards。`web/src/pages/Wave.tsx:84` `web/src/cards/builtins/file-viewer.tsx:669` `web/src/cards/builtins/file-viewer.tsx:690` `web/src/cards/builtins/file-viewer.tsx:700`
- Layout overlay: report mode should ignore grid layout entirely and leave it as grid/list fallback; `WaveGrid` only reads/writes layout overlay when mounted, so not mounting it avoids clobbering seeded positions。`web/src/WaveGrid.tsx:151` `web/src/WaveGrid.tsx:217` `web/src/cards/builtins/wave-report.tsx:552`

| Surface | % | Notes |
|---|---:|---|
| 新 `WaveReportPage` + CSS shell | 30% | Docked sidebar / center doc / fixed chat。`web/public/_design/Report.html:252` `web/public/_design/Report.html:404` |
| `Wave.tsx` view-mode plumbing | 12% | type/schema guard/toggle/branch/fallback strings。`web/src/pages/Wave.tsx:160` `web/src/pages/Wave.tsx:322` |
| Sidebar 折叠组件 | 10% | 设计稿已有 two collapsible sections JS 行为。`web/public/_design/Report.html:508` |
| Files tree feed + UI | 16% | 当前缺 wave files feed, file-viewer 只能给 path fallback。`web/src/cards/builtins/file-viewer.tsx:672` |
| Event line synthesizer/feed | 17% | 前端 synth live + backend durable feed 边界要定。`web/src/app/eventBridge.tsx:200` |
| Chat pill/current-run 抽取 | 15% | 从 spec 拆 status/phase/current action, 不搬 `ChatTimeline`。`web/src/cards/builtins/spec.tsx:605` `web/src/cards/builtins/spec.tsx:818` |

## 4. 风险 / 必踩坑
- Spec card 同时是 transcript UI 和 harness state owner; 只删 `ChatTimeline` 会丢 reset/session 状态和 `phase`。`web/src/cards/builtins/spec.tsx:833` `web/src/cards/builtins/spec.tsx:928`
- Event line 不能靠 dev trace ring 上生产; trace 明确 DEV + `?trace=1`, durable timeline 需要 feed。`web/src/app/eventBridge.tsx:57` `web/src/app/eventBridge.tsx:66`
- 三态 view mode 不能继续用二态 switch 文案和 `aria-checked`; 需要 segmented/menu。`web/src/pages/Wave.tsx:264` `web/src/pages/Wave.tsx:274`
- 不要重写 report Markdown/parser 来做视觉; 上轮 audit 已确认 report TSX 本体健康, 视觉债主要在 spec inline styles/token。`docs/_research-spec-report-redesign-codex.md:40` `docs/_research-spec-report-redesign-codex.md:42`
- 不要打破全局 `CardHead` 假设; 上轮 subagent 把拆 `.card-head` 列为高连锁风险。`docs/_research-spec-report-redesign-subagent.md:271` `docs/_research-spec-report-redesign-subagent.md:276`
