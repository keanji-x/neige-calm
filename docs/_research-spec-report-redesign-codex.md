# Spec + Wave Report 视觉重设计 audit

## 1. Token inventory
- 底层 chrome token 够用: report/spec 现在共用 `border + radius + paper + shadow + flex shell`, 已落在 `.wave-report-card` 上。`web/src/calm.css:2578`
- 文本原子够用但不够语义化: `--text-*`、`--leading-*`、`--tracking-*` 已覆盖字号/行高/字距阶梯, 但 prose 仍手拼 `--text-sm + --text-2 + --leading-loose`。`web/src/calm.css:58` `web/src/calm.css:75` `web/src/calm.css:89` `web/src/calm.css:2645`
- 缺 `--font-prose-*` bundle: 现有 bundle 是 `--font-nav-*`, 明确服务 nav/sidebar 语义, 不描述 report 正文、section title、dense timeline。`web/src/calm.css:97`
- surface 原子够用但 chat 语义缺口明显: `--surface-paper/bg/rail/card/chip/panel-head` 和 code/terminal surfaces 已有, 但 bubble 仍在 TSX 里按 tone 选 `paper/paper-2`。`web/src/calm.css:132` `web/src/calm.css:159` `web/src/cards/builtins/spec.tsx:437`
- 缺 `--surface-bubble-*` 家族: `TimelineBubble` 同时决定 padding、border、background、text color、font size、line-height, 设计师无法只改 token。`web/src/cards/builtins/spec.tsx:438`
- report 缺 prose rhythm token: section body 的 p/ul/ol/code/pre CSS 是硬绑定在 `.wave-report-section-body`, 没有 `--prose-body-font`、`--prose-block-gap`、`--prose-code-surface`。`web/src/calm.css:2645` `web/src/calm.css:2657` `web/src/calm.css:2664`
- spec 缺 chat rhythm token: timeline scroller用 inline `gap` 和 `paddingBottom`, bubble/pre/details 又各自定义 padding。`web/src/cards/builtins/spec.tsx:742` `web/src/cards/builtins/spec.tsx:392` `web/src/cards/builtins/spec.tsx:438` `web/src/cards/builtins/spec.tsx:500`
- status token 基本够 current states, 但 danger 仍是欠债: `Errored/failed` 都复用 warn, 代码注释也标出未来 `--danger`。`web/src/shared/components/CardStatusDot.tsx:24` `web/src/shared/components/WaveLifecycleBadge.tsx:20`
- logo/header 原语够用: `<CardHead>` 已有 icon/title/actions/status slots, `.agent-card-logo` 支持 CSS vars 注入颜色。`web/src/cards/CardHead.tsx:97` `web/src/calm.css:2485` `web/src/cards/builtins/spec.tsx:68`
- 必补 token 建议: `--font-prose-body/title/meta`, `--report-body-pad`, `--report-section-gap`, `--surface-bubble-agent/user/tool/muted`, `--bubble-pad-*`, `--chat-gap`, `--status-danger`。证据是上述值目前分散在 report CSS 与 spec inline style。`web/src/calm.css:2588` `web/src/cards/builtins/spec.tsx:425`

## 2. Visual debt 清单
- 最大 inline-style 岛 1: `HarnessStateChip` 三层 span 自管 flex、gap、pill bg/border/padding/type, 约 39 LOC。`web/src/cards/builtins/spec.tsx:132` `web/src/cards/builtins/spec.tsx:141` `web/src/cards/builtins/spec.tsx:161`
- inline-style 岛 2: `TimelinePre` 自管 code block chrome, 约 20 LOC, 应抽 `.spec-timeline-pre`。`web/src/cards/builtins/spec.tsx:389`
- inline-style 岛 3: `SpecMarkdown` 复用 `.wave-report-section-body` 后再 inline 改 color/overflow, 约 12 LOC。`web/src/cards/builtins/spec.tsx:411`
- inline-style 岛 4: `TimelineBubble` 是核心 chat UI, 约 40 LOC, tone 分支和视觉属性混在 TSX。`web/src/cards/builtins/spec.tsx:425`
- inline-style 岛 5: reasoning `<details>/<summary>` 自管 border/background/padding/type, 约 25 LOC。`web/src/cards/builtins/spec.tsx:499`
- inline-style 岛 6: MCP status/code snippets 自管 margin/color/font/wrap, 约 21 LOC。`web/src/cards/builtins/spec.tsx:562` `web/src/cards/builtins/spec.tsx:574`
- inline-style 岛 7: `ChatTimeline` scroller和 empty padding overrides 约 28 LOC, 应是 `.spec-chat-timeline` 和 `.spec-chat-empty`。`web/src/cards/builtins/spec.tsx:736` `web/src/cards/builtins/spec.tsx:752`
- inline-style 岛 8: reset error 用 raw `marginTop: 8`, 已脱离 `--space-*`。`web/src/cards/builtins/spec.tsx:907` `web/src/calm.css:228`
- 重复 hardcode: `var(--paper-2)` 在 spec chip/bubble/details 里出现, 但现有公开 surface vocabulary 是 `--surface-*`。`web/src/cards/builtins/spec.tsx:146` `web/src/cards/builtins/spec.tsx:443` `web/src/cards/builtins/spec.tsx:503` `web/src/calm.css:136`
- 重复 hardcode: `--space-3/4` padding/gap 在 pre、bubble、details、timeline 中重复, 没有 chat-level rhythm token。`web/src/cards/builtins/spec.tsx:394` `web/src/cards/builtins/spec.tsx:440` `web/src/cards/builtins/spec.tsx:504` `web/src/cards/builtins/spec.tsx:747`
- 命名债: spec root/goal/body/empty 都套 `wave-report-*`, 所以 spec redesign 会误改 report。`web/src/cards/builtins/spec.tsx:178` `web/src/cards/builtins/spec.tsx:783` `web/src/cards/builtins/spec.tsx:882` `web/src/cards/builtins/spec.tsx:893`
- report 本体较健康: `ReadOnlyView`、`ReportSection`、`EditView` 大多已经给 CSS class, 不需要先拆大量 TSX。`web/src/cards/builtins/wave-report.tsx:196` `web/src/cards/builtins/wave-report.tsx:334` `web/src/cards/builtins/wave-report.tsx:411`
- spec 只要先抽类即可一行改样式的段落: `HarnessStateChip`, `GoalBanner`, `TimelinePre`, `SpecMarkdown`, `TimelineBubble`, reasoning details, `ChatTimeline`。`web/src/cards/builtins/spec.tsx:122` `web/src/cards/builtins/spec.tsx:175` `web/src/cards/builtins/spec.tsx:389` `web/src/cards/builtins/spec.tsx:411` `web/src/cards/builtins/spec.tsx:425` `web/src/cards/builtins/spec.tsx:499` `web/src/cards/builtins/spec.tsx:605`

## 3. 共享 vs 分化建议
- 共享 card shell: border、radius、background、shadow、height、flex、overflow 可以沉为 `.agent-card-shell`, 因为两张卡现在都靠 `.wave-report-card`。`web/src/calm.css:2578` `web/src/cards/builtins/spec.tsx:882` `web/src/cards/builtins/wave-report.tsx:488`
- 共享 header skeleton: `<CardHead>` 已固定 DOM order 和 icon/title/actions/status slots, CSS 已定义 padding/gap/status alignment。`web/src/cards/CardHead.tsx:97` `web/src/calm.css:2250` `web/src/calm.css:2284`
- 共享 status primitive: lifecycle badge 和 FSM dot 都明确复用 accent/warn/text vocabulary, 不应为这两张卡各造颜色。`web/src/shared/components/WaveLifecycleBadge.tsx:58` `web/src/shared/components/CardStatusDot.tsx:14`
- 分化 body padding rhythm: report 是 collapsible prose sections, spec 是 scroll chat stream; 现在都叫 `.wave-report-body`, spec 再用 inline scroller补节奏。`web/src/calm.css:2594` `web/src/cards/builtins/spec.tsx:893` `web/src/cards/builtins/spec.tsx:742`
- 分化 summary/goal: report summary 是单行摘要, spec goal 带 label 且复用 `.wave-report-summary`; 应拆 `.report-summary` 与 `.spec-goal-strip`。`web/src/cards/builtins/wave-report.tsx:330` `web/src/cards/builtins/spec.tsx:175` `web/src/calm.css:2588`
- 分化 content primitives: report 需要 `.report-section`, `.report-prose`, `.report-attention`; spec 需要 `.spec-bubble`, `.spec-bubble--user/tool/muted`, `.spec-tool-output`, `.spec-reasoning`。`web/src/calm.css:2605` `web/src/cards/builtins/spec.tsx:425` `web/src/cards/builtins/spec.tsx:526`
- Token 化方案: shell tokens 保留 `--r/--shadow/--hairline`, content tokens 新增 `--report-*` 和 `--chat-*`, status 增补 `--status-danger` 后替换 warn fallback。`web/src/calm.css:317` `web/src/calm.css:2578` `web/src/shared/components/CardStatusDot.tsx:24`

## 4. 改造可行性
- wave-report 重设计主要是 CSS rewrite: TSX 已暴露 card、summary、body、section、toggle、title、markdown body、edit states。`web/src/cards/builtins/wave-report.tsx:200` `web/src/cards/builtins/wave-report.tsx:334` `web/src/cards/builtins/wave-report.tsx:338` `web/src/cards/builtins/wave-report.tsx:411`
- wave-report 只有新视觉需要改变 section 结构时才动 TSX, 因为 section model 已在 `ReportSection` 和 `parseSections`。`web/src/cards/builtins/wave-report.tsx:99` `web/src/cards/builtins/wave-report.tsx:196`
- spec 重设计要先 TSX 小重构: bubble/pre/details/status chip 都是 inline style, 设计师给 CSS 后没有稳定类可挂。`web/src/cards/builtins/spec.tsx:122` `web/src/cards/builtins/spec.tsx:389` `web/src/cards/builtins/spec.tsx:425` `web/src/cards/builtins/spec.tsx:499`
- 建议工作量分摊: `spec.tsx` 约 50% 抽组件/类名, `calm.css` 约 35% 加 token 与样式, `wave-report.tsx` 约 10% 类名语义化, shared status 约 5% 补 danger。`web/src/cards/builtins/spec.tsx:605` `web/src/calm.css:2573` `web/src/cards/builtins/wave-report.tsx:549` `web/src/shared/components/CardStatusDot.tsx:27`
- 最小 PR 顺序: 先把 spec 的 inline visual props 移到 CSS class, 再拆 `.wave-report-card/body/summary` 为 shared shell + report/spec aliases, 最后交给设计师改 token值。`web/src/cards/builtins/spec.tsx:438` `web/src/calm.css:2578` `web/src/cards/builtins/spec.tsx:882`

## 5. 不要做的
- 不要只换字体: 字体栈已经统一, 问题是缺 prose/chat 语义 token, 不是 font family。`web/src/calm.css:10` `web/src/calm.css:28`
- 不要只加阴影/圆角: shell 已有 `border/radius/shadow`, 丑主要在内容层节奏和 inline bubble。`web/src/calm.css:2578` `web/src/cards/builtins/spec.tsx:425`
- 不要给 spec/report 继续共用 `wave-report-*` 作为长期 API: 现在的命名已让 chat-stream 继承 report prose 语义。`web/src/cards/builtins/spec.tsx:414` `web/src/calm.css:2645`
- 不要靠 emoji 或更多状态颜色解决层级: status 组件刻意限制在 accent/warn/text, 且 danger 尚未 token 化。`web/src/shared/components/WaveLifecycleBadge.tsx:16` `web/src/shared/components/CardStatusDot.tsx:24`
- 不要重写 markdown/parser 来解决视觉: report 的 section split 和 Markdown renderer 已稳定, 视觉债在 CSS/token 层。`web/src/cards/builtins/wave-report.tsx:99` `web/src/cards/builtins/wave-report.tsx:214`
