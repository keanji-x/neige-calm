# PR #594 design review — subagent channel

Scope: judgment/ordering/assumption check on the design + 5-PR plan in
`docs/_issue-draft-spec-report-merge.md`. Codex channel handles fact-checks.

## 1. 设计意图与现状的张力

设计稿把"进行中"压成右下一颗 pill, "成稿"占满中央 (chat1.md:107, Report.html:167)。
issue 自己也承认 spec card 同时承担 transcript + FSM owner + 长任务用户
"实时看着 agent 在做什么"的窗口 (`_issue-draft-spec-report-merge.md:68-71`,
`spec.tsx:736`, `spec.tsx:818`, `spec.tsx:840`)。问题不是"过程 vs 成品"分不开,
而是: **当前 product 的主要使用场景是不是看成稿?**

证据偏向"不是": `spec.tsx:736` `ChatTimeline` 是 `height: 100%` 整列, 默认行为是
滚到底部追新, `requestFreshFetch` + reset 路径 (`spec.tsx:625`, `:863`) 都是为
长跑运行设计的; `wave-report.tsx:1-27` 注释明确 wave-report 是"agent 维护的
markdown body", 内容来自 spec 写入。如果 spec 不在主屏, 用户怎么知道"agent 还
在干活 / 现在干到哪"? pill 里那一行 phase + last tool name (issue PR5,
`_issue-draft-spec-report-merge.md:142-146`) 信息密度比当前 transcript 低 1-2 数量级。

这是 product 决策, 不是视觉决策。issue 把它当视觉问题处理 (`_issue-draft-spec-report-merge.md:58-64`
"chrome 视觉同源"), 但根因可能是"两张卡同时在主屏的频率比预期低"。
chat1.md:9 的最初提问也来自一个 research-agent 隐喻 — research 跑完一次, 用户
读结论; 而 calm 的 spec 是连续多 turn 的长运行 agent, 两个产品 mental model 不一样。

## 2. 5-PR 顺序的真实可行性

**PR1 → PR2** — PR1 引入 `--surface-bubble-*` `--font-prose-*` token
(`_research-spec-report-redesign-codex.md:13`) 给 *spec 内部*用; PR2 的 Report
布局 token 是 `--canvas` / `--ink` / `--accent` / `--hair` (Report.html:11-33),
两套 token 几乎无重合。PR1 的产出**不为 PR2 铺路**, 它只是给"grid 模式 spec
长得不那么糟"做 prep。issue 说 PR1 是"后续每个 PR 的前置"
(`_issue-draft-spec-report-merge.md:106`) 言过其实。

**PR2 → PR3** — 致命。PR2 ship 后 `WaveReportPage` 在生产 build 里挂出"Sources/
Generated"假文件 (Report.html:241-298 全是 hard-coded sample), 用户切到 report
模式看到的就是 demo 数据。issue 说"先用 sandbox 占位"
(`_issue-draft-spec-report-merge.md:118`), 但**没有 feature flag**。最低限度要么
PR2 把 Files/Event line 渲染成 "no source connected yet" 空态 (这本来就是
acceptance criteria, `_issue-draft-spec-report-merge.md:187`), 要么 PR2 加 dev-only
gate, 否则 PR3 没 ship 前 report 模式不能开放给用户。

**PR3 ↔ PR4** — 不独立。选项 (b) (从 file-viewer card 合成 Files,
`_issue-draft-spec-report-merge.md:128`) 同时决定了 Event line 里 `data.feed`
有没有"file added/refreshed"这类信号 (Report.html:336-352 设计稿样本里
"+1,204 rows added to market-data-q2.csv" 是 file ↔ data 强耦合的事件)。
如果 Files 是合成的, Event line 的 data 行就没有 first-class source。

**PR5 应该领先 PR2** — PR5 是产品语义改造 (transcript 在 report 模式消失,
`_issue-draft-spec-report-merge.md:140-146`)。它 implicit 改 acceptance: chat pill
能否替代用户对"agent 正在做什么"的可见性。这是 §1 的核心问题, **必须在 PR2
开建 `WaveReportPage` skeleton 之前**就有用户拍板, 否则 PR2 的中央 prose+pill
布局本身就是错的押注。issue 把 PR5 排最后, 是按"代码 deps 最少"排序, 不是
"产品决策最阻塞"排序。

## 3. 哪些 acceptance criteria 是空头支票

`_issue-draft-spec-report-merge.md:179-189` 逐条:

- "spec FSM phase 真正驱动 chat pill 状态" — 测什么? `spec.tsx:822` phase 是 React
  state, 测试要构造一条 `harness.phase.changed` 事件 + 断言 pill DOM 文字。可写,
  但 issue 没说要写 e2e 还是单测。
- "同 wave 在 grid/list 模式视觉零变化" — **没有视觉回归网兜住 spec/wave-report 卡面**。
  `__color_anchor__/ColorContactSheet.tsx:14-81` 只覆盖 input/textarea/select,
  `wave-report-textarea` 是唯一沾边的, 还只测 textarea 的 form color。PR1 抽 class
  改 CSS 是无网捕鱼。"视觉零变化"靠人眼 review, 不可验证。
- "Files 侧栏与 Event line 渲染真实内容 (或显式空态)" — "或显式空态"是兜底逃生
  口, 让 PR2 永远满足这条。如果 PR3 选合成方案 (b), `file-viewer` 卡可能本来就没
  挂 → "空态"成为 default 用户体验 → acceptance 名存实亡。
- "ChatTimeline transcript 在 grid/list 模式仍可达" — 隐含: report 模式下
  *不可达*。这是 §1 的产品改动, 在 acceptance 里被轻描淡写带过。
- 缺一条: report 模式下 spec card 的 reset session 路径 (`spec.tsx:863-879`,
  `:923`) 如何到达? 用户怎么 reset?

## 4. open decisions 之间的依赖

`_issue-draft-spec-report-merge.md:148-156` 三项决策:

- (1) Files source-of-truth 选 (a) kernel `wave.files` vs (b) file-viewer 合成。
- (2) `data.feed` / `alert.anomaly` 事件所有权。
- (3) wave 默认视图模式。

(1) 严格 gate (2): 若 (1)=b (无 kernel files table), `data.feed` 设计就要重定义
— "rows added to market-data-q2.csv" 这种事件没有 stable source-of-truth。
设计稿 Event line 是 file-centric 叙事 (Report.html:336, :345, :369 都 mention
具体文件), 这套叙事和 (1)=b 不自洽。

(3) 隐藏依赖 §1 / PR5。如果 transcript 在 report 模式消失, "默认是否进 report"
就不仅是首选项, 而是"系统是否默默隐藏 agent 行为流"的开关。issue 给的"安全
起见保留 grid" (`_issue-draft-spec-report-merge.md:156`) 是对的, 但承认了一件事:
**report 模式不被默认信任**。如果它不被默认信任, 单写 5 PR ship 它图什么?

## 5. 我会先问用户什么

issue 列了 3 个 open decisions, 但绕开了最大的产品问题:

**"今天 cove 里既有 spec 又有 wave-report 卡的 wave, 占多大比例? 用户在这种
wave 上, 默认看的是 spec 跑步, 还是 wave-report 成稿? 频率是多少?"**

这决定了 report 模式是"另一种 view 让人挑"还是"主屏改朝换代"。如果答案
是"现在用户大多数时间盯着 spec card 看 agent 跑", 则 PR2 的 pill 化方向
等于把现役主屏砍掉 — 应该先做 §1 提到的"长跑 agent 的当前状态可见性"
而不是 Report 布局。

---

**3-line summary**

1. PR5 的产品语义 (transcript 在 report 模式消失) 是整个设计的根, 应该比 PR2 早决策, 不是排最后。
2. PR2 没有 feature flag, ship 后用户看到 hard-coded demo 文件 (Report.html:241-298), acceptance 里"显式空态"逃生口架空 "渲染真实内容" 这条标准。
3. acceptance "grid/list 视觉零变化"无网可兜 — color-anchor 套件 (`__color_anchor__/ColorContactSheet.tsx:14-81`) 只覆盖 form 输入, 不覆盖卡面 token 改动。
