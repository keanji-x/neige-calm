# PR594 Design Review

## 1. 事实核查
- `Wave.tsx:37` 仍是 `type ViewMode = 'grid' | 'list'`，`isViewMode` 也只收这两值；没有 `'report'` 早期实现苗头；修法是在 PR2 明确加第三态和迁移策略。`web/src/pages/Wave.tsx:37` `web/src/pages/Wave.tsx:46`
- `Wave.tsx:264/:274` 的 toggle 核对准确：当前是 `button role="switch"`，用 `aria-checked={viewMode === 'list'}` 表示两态。`web/src/pages/Wave.tsx:264` `web/src/pages/Wave.tsx:273` `web/src/pages/Wave.tsx:274`
- `wave-report.tsx:50/:314` 不准确：`:50` 是 `WaveReportCardData`，`:314` 是未导出的 `function ReadOnlyView`；PR2 不能原样 import，需 export 或抽 `WaveReportReadOnly`。`web/src/cards/builtins/wave-report.tsx:50` `web/src/cards/builtins/wave-report.tsx:314` `web/src/cards/builtins/wave-report.tsx:543`
- `spec.tsx:736` 不准确：它指向 `ChatTimeline` 的 return JSX，定义在 `:605`；`:818/:840/:842` 仍对应 card/status/FSM/phase stream 逻辑。`web/src/cards/builtins/spec.tsx:605` `web/src/cards/builtins/spec.tsx:736` `web/src/cards/builtins/spec.tsx:818` `web/src/cards/builtins/spec.tsx:840` `web/src/cards/builtins/spec.tsx:842`
- `WaveGrid.tsx:151` 声明基本准确：layout overlay hook 在 WaveGrid 内，写入也由 grid layout 变化触发；不挂载 WaveGrid 可避免触碰 layout overlay。`web/src/WaveGrid.tsx:151` `web/src/WaveGrid.tsx:217` `web/src/WaveGrid.tsx:254`
- `eventBridge.tsx:57/:66` 核对准确但语义要收紧：trace ring 是 DEV + URL `?trace=1` gate，不是生产 report event line。`web/src/app/eventBridge.tsx:57` `web/src/app/eventBridge.tsx:66` `web/src/app/eventBridge.tsx:197`
- `invalidationPolicies.ts:100` 不应写成前端 bus 上已有 `wave.lifecycle`；实际事件名是 `wave.lifecycle_changed`，`wave.lifecycle` 只是产品 taxonomy 说法。`web/src/app/invalidationPolicies.ts:100` `web/src/api/generated-events.ts:224`
- `_research-spec-report-redesign-codex.md:13/:40` 引用准确，分别是 token 缺口和 wave-report CSS/TSX 结论。`docs/_research-spec-report-redesign-codex.md:13` `docs/_research-spec-report-redesign-codex.md:40`
- `_research-spec-report-redesign-subagent.md:271` 引用准确：该处确实把 Concept 3 标成会改 `CardHead` API 的重方案。`docs/_research-spec-report-redesign-subagent.md:271`

## 2. 实现可行性补漏
- PR2 “first `wave-report` card -> `ReadOnlyView`” 需要 zero/one/many 策略：registry 会按每个 kernel card 的 exact kind claim 适配，不天然保证只出现一张；wave-report entry 的 claim 是 exact `wave-report`。`web/src/cards/registry.ts:364` `web/src/cards/builtins/wave-report.tsx:558`
- 后端设计意图是一波一张 report card，但前端仍应防御：zero 显示空状态并记录 invariant error；one 正常渲染；many 按稳定顺序取第一张并显示 duplicate warning，或直接 blocker。`crates/calm-server/src/wave_report.rs:110` `crates/calm-server/migrations/0014_wave_report_card.sql:31`
- PR2 `'report'` ViewMode 要显式处理 overlay schema：当前常量仍注释为 grid/list，Wave 写入 `OVERLAY_VIEW_MODE_SCHEMA_VERSION`；若只扩 enum 可兼容 v1，若后端/清理器按枚举校验则需 bump。`web/src/cards/builtins/schemaVersions.ts:36` `web/src/pages/Wave.tsx:171` `web/src/pages/Wave.tsx:178`
- 不挂载 WaveGrid 不等于禁止新增 card：`AddPanel` 仍在 Wave header，router 仍会 create card；Report 模式若不能 add，需要 hide/disable AddPanel 并给 UI 状态。`web/src/pages/Wave.tsx:290` `web/src/app/router.tsx:369` `web/src/app/router.tsx:501`
- PR3b “从 file-viewer card 合成” 会失真：card payload 只有单个 `path`，组件运行时只列当前 folder 的一层 entries；从 cards 合成树只能得到已创建/已访问路径，不是完整 Sources/Generated tree。`web/src/cards/builtins/file-viewer.tsx:27` `web/src/cards/builtins/file-viewer.tsx:44` `web/src/cards/builtins/file-viewer.tsx:348` `web/src/cards/builtins/file-viewer.tsx:690`
- PR5 `SpecCurrentRun` 不能只靠 prop：`SpecCardImpl` 同时 owns status/phase/reset/timeline version 并直接渲染 `ChatTimeline`；若 report center 只要 current run，需要先抽 component 或加明确 hide timeline prop。`web/src/cards/builtins/spec.tsx:811` `web/src/cards/builtins/spec.tsx:833` `web/src/cards/builtins/spec.tsx:890` `web/src/cards/builtins/spec.tsx:894`

## 3. 缺失的依赖/影响
- 测试分层应写入 issue：PR1 更新/补 `spec.test.tsx` 行为测试即可，现有用例查 DOM 文本和 reset 行为，不是 snapshot；PR2 需新增 `WaveReportPage`/report overlay 测试。`web/src/cards/builtins/spec.test.tsx:209` `web/src/cards/builtins/spec.test.tsx:654` `web/src/pages/Wave.test.tsx:95`
- PR3a `wave.files` 若加 kernel feed/route，必须走 OpenAPI 重建和契约测试；当前脚本显式生成 `openapi.json`/TS types，Rust test 把缺 schema 视为 wire contract 违约。`web/package.json:22` `crates/calm-server/tests/openapi.rs:1`
- e2e/a11y 要覆盖 report path、zero source、AddPanel policy、三态切换；现 axe matrix 没有 report，键盘测试仍断言两态 switch。`web/e2e/a11y-axe.spec.ts:6` `web/e2e/a11y-axe.spec.ts:323` `web/e2e/a11y-keyboard.spec.ts:612` `web/e2e/a11y-keyboard.spec.ts:681`
- 文案/i18n 需要明确“不做 i18n”或列后续：设计里 `Files`/`Event line`/`Ask the Research Agent` 是硬编码，项目依赖列表未见 i18n 框架。`web/public/_design/Report.html:226` `web/public/_design/Report.html:312` `web/public/_design/Report.html:453` `web/package.json:24`
- 视觉回归缺口：`__color_anchor__` 只覆盖表单/输入控件颜色，未覆盖 report sidebar/center/chat 三块；若 PR1/2 改 token，应新增 anchor 或截图。`web/src/__color_anchor__/ColorContactSheet.tsx:14` `web/src/__color_anchor__/ColorContactSheet.tsx:52` `web/e2e/__snapshots__/color-anchor-baseline.md:5`

## 4. 风险盲点
- `WaveContext` 很小，只提供 `id/lifecycle`；ReportPage 若需要 cove/title/files/progress，不应假设 context 已有，应从 Wave props 或 loader 数据显式传入。`web/src/shared/components/WaveContext.ts:1` `web/src/shared/components/WaveContext.ts:22` `web/src/pages/Wave.tsx:186`
- spec card 既是 transcript 又是 harness owner；PR5 抽 current run 时可能改到 reset/timelineVersion/status chip，进而影响 grid/list 现有 spec 卡行为。`web/src/cards/builtins/spec.tsx:833` `web/src/cards/builtins/spec.tsx:890` `web/src/cards/builtins/spec.tsx:928`
- PR1 token/class 命名要避开现有 `wave-report-*` 语义：spec 目前复用 wave-report shell/body class，贸然改 token 可能把 report/spec 两块绑死。`web/src/cards/builtins/spec.tsx:414` `web/src/cards/builtins/spec.tsx:882` `web/src/calm.css:2578`
- static design 引了 Google Fonts；生产 app 现在用本地 font tokens，e2e 还会 block Google Fonts，移植时不要把远程字体带进 app。`web/public/_design/Report.html:7` `web/src/calm.css:10` `web/e2e/a11y-axe.spec.ts:219`
- EventBridge 当前职责是 cache invalidation/DEV trace，不是 durable event feed；PR4 的 report event line 需要自有合成/回放规则，不能直接消费 trace buffer。`web/src/app/eventBridge.tsx:238` `docs/_research-report-merge-feasibility-codex.md:27` `docs/_research-report-merge-feasibility-codex.md:39`

## 5. 总体判定
- 结论：issue 还不适合直接开 PR2-5；PR1 可以先开，但 issue 需要把“PR1 启动前需拍板”的 open decisions 下放到对应 PR，因为 files/event/default mode 不阻塞 inline-style 抽取。`docs/_issue-draft-spec-report-merge.md:101` `docs/_issue-draft-spec-report-merge.md:148`
- PR2 启动前必须补四个 decision：`ReadOnlyView` export/extract、zero/one/many 行为、AddPanel policy、ViewMode overlay schema。`web/src/cards/builtins/wave-report.tsx:314` `web/src/pages/Wave.tsx:290` `web/src/cards/builtins/schemaVersions.ts:36`
- Open decisions 建议：默认仍 grid，report 做 opt-in；Files 选 PR3a 作为正解，若先 PR3b 必须标注 lossy；Event line 先合成 `report.updated`/`wave.lifecycle_changed`，data feed 留给后端。`docs/_issue-draft-spec-report-merge.md:123` `docs/_issue-draft-spec-report-merge.md:131` `docs/_issue-draft-spec-report-merge.md:155`
- 关键路径估计：最容易 ship 是 PR1；PR2 次之但卡 UI/overlay contracts；PR5 风险在 spec 抽取；最容易卡住的是 PR3a/PR4 后端 feed，因为会带 OpenAPI 和事件契约。`web/package.json:22` `crates/calm-server/tests/openapi.rs:1` `web/src/cards/builtins/spec.tsx:811`
