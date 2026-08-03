# `_fe-architecture.md` 独立 review（subagent 通道）

日期：2026-08-02 ｜ 只读审阅。读过：架构文档、rewrite plan、SCHEMA.md + 7 份 yaml（脚本统计全量
owner_slice）、`eventBridge.tsx` 与 `api/events.ts` 全文、`editor/README.md`、`calm.css`、
`package.json`、`DirectoryPicker.tsx`

## 总体判断

**不能直接驱动 20–30 agent 并行。** 架构方向我认为是对的（五层 + 依赖单向 + 接口冻结优先），
病灶表也确实对准了真问题。但文档缺三样东西，缺了就会变成"N 个 agent 各自发挥"：

1. **五层容不下全部 oracle。** 我按 148 个 distinct owner_slice 归层统计（脚本，非目测）：
   能无歧义落进 app/features/systems/ui/core 的是 **795/1126 = 70.6%**。剩下 **331 条（29.4%）**
   落在文档没定义的桶里：`shared/*` 104、infra/lint/build/e2e 84、styles/design-system 68、
   `hooks/*` + `ui/hooks` 31、`shell/*` 21、`web/components/*` 9、`NONE` 2。
   这 331 条是 agent 打架的确定来源。
2. **`shared/` 只被"废除"，没被"重分配"。** §2.1 说"无 `shared/` 这个筐"，但 104 条 oracle
   顶着 `shared/*`，其中 `shared/new-task-form` 29 条是**跨 feature 复用的有状态表单**
   （`Cove.tsx` + wave 创建两处），按 §2.2 的两条合法路径既不能下沉 `core/domain`（有 DOM/状态）
   也不能上提 `app`（业务层不得 import app）。**§2.2 的规则对这类组件是不闭合的。**
3. **§7 文件所有权表是空的**（"完整所有权表在模块清单定稿后生成"）。并行的唯一前提被推迟了，
   而 §6 的接口清单里 8 项没有一项标明产出文件路径。

补齐这三样（层的第 6 类 + shared 分配规则 + 所有权表），我认为可以并行。

## §1 五层是否正交 / 归属争议清单

**统计口径**：剥掉 `web/` 前缀后按首段归并（这本身证实同物异名：`ui/dialog` 33 + `web/ui/dialog` 11 +
`web/ui` 2 + `ui/modal` 4 = 同一 Dialog 家族被 4 个名字切开；`terminal/xterm-view` 16 + `web/xterm-view` 2；
`shell/sidebar` 10 + `web/shell/sidebar` 10 + `shared/sidebar` 19）。

归层结果：features 330（29.3%）· systems 252（22.4%）· app 95+shell 21 · ui 81 · core 37
＝ **可无歧义归层 795**。
**无归属 331**：`shared/*` 104 · infra/lint/build/a11y-axe/e2e 84 · styles/design-system/tokens/layer-rules 68 ·
`hooks/*`+`ui/hooks` 31 · `web/components/*` 9 · 跨 slice/NONE 10 · 其他 25。

**具体争议类别（按条数排序）**

1. **测试/工具基础设施（84 条）** — `infra/e2e` 40、`lint/custom-rules` 15、`design-system/css-lint` 10、
   `lint/config` 6、`a11y/axe` 5、`web/e2e*` 8、`infra/ci` 2、`build/ts-rs` 2。五层是**运行时**分层，
   这些是**构建期**产物，却恰恰是 §3「每条病灶都要有机器检查」的载体。
   **建议加第六类 `tooling/`**（不参与依赖方向 lint），并在 §7 给它独立 owner——它是天然共享可写区，最易冲突。
2. **样式层（68 条）** — `design-system/tokens` 28、`ui/layer-rules` 12、`web/styles` 9、
   `design/color-system` 5、`report/typography` 3。`styles/` 只在 §4.5 目录图里，**不在 §2 五层内**，
   "依赖只能向下"对它无定义。且 `ui/layer-rules` 12 条 source 全部指向 `web/src/ui/README.md:17-124`
   （z-index 六级、portal 规则）——**文档即契约**，不指定承接者就会随 README 蒸发。
3. **跨 feature 的有状态业务组件**：`shared/new-task-form` 29 + `shared/add-panel` 10 +
   `shared/wave-row` 6 + `shared/atoms` 8 —— §2.2 规则的反例集合，见「总体判断 2」。
4. **spec 家族 78 条**（`spec-chat` 25 + `pages/spec-conversation` 21 + `pages/spec-chat-history` 12 +
   `pages/spec-chat-items` 8 + `pages/spec-run` 9 + `cards/spec` 3）——§2 的 features 列表里**没有 spec**，
   而它既是页面又是卡片，跨 features/systems。必须显式定名。
5. **report 家族跨层**：`pages/report-*` 61 + `report/*` 18 + `cards/wave-report` 4 + `wave/report` 1 ——
   同时是 feature（页面）、system（卡片承载），§8 还想让 renderer 进 core。三层争一物。
6. **hooks（31 条）** — `ui/hooks` 17、`hooks/today-terminal` 6、`hooks/overlay-state` 5、
   `web/hooks/roving-tabindex` 3。hook 不是层；`useModalView`/`useRovingTabindex` 属 ui，
   `useTodayTerminal` 属 feature。**规则应是"hook 跟随消费层"，文档要写死这句。**
7. **单条跨两 slice**：`app/router → cards/create`（7 条）、`api/events, api/onUnauthorized`（1）、
   `NONE`（2）。这 10 条直接违反 §7"每文件恰好一个 owner"，需在所有权表生成前人工裁决。

## §2 「只有 core 跨端」+ `systems/events` 下沉 —— 切法对，论证有一处**事实错误**

**切法我认为对。** ui/systems/features/app 每端一份的四条理由站得住，不用改。

**但"事件流没有一行 DOM"这句，读 `api/events.ts` 全文后站不住** —— 它依赖 5 个宿主 API：
`new WebSocket(url)`（`:312`）、**`localStorage`**（`:514,528,589`，cursor 持久化）、
**`location.protocol`/`location.host`**（`:600-601`）、
**`fetch(...,{credentials:'include'})`**（`:565`，cookie ride-along 是这个 401 探测的全部意义）、
`requestIdleCallback`（`:165-170`，已有 setTimeout 兜底，OK）。
加粗的三个在 React Native 里都不存在（storage 还是异步的）。

结论：**没有 DOM 是真的，"纯逻辑可跨端"是假的。** `core/events` 要跨端必须把
**storage / origin / fetch-with-credentials 三个 port 显式注入**（构造函数参数）——
与 §3 病灶 2 的"构造函数注入"同向，顺手可做，但文档没写，20 个 agent 不会自己想到。
另外 `localStorage` 同步而 AsyncStorage 异步，**`loadCursor()` 在构造函数里同步返回（`events.ts:204`）
这个形状本身就不跨端**，接口冻结时就得改成 `async init()` 或注入初值。

**`eventBridge.tsx` 那侧文档说得对**：真正的 React 胶水只有 `:172-233` 那 19 行。但两块不能进 core：
- `:66-148` trace ring buffer —— 写 `window.__neigeEvents__`、靠 Vite 专有 `import.meta.env.DEV` 折叠死代码，
  `:196` 明确要求"重构前必须 `grep __neigeEvents__ web/dist/assets/*.js` 复验"。归 `tooling/` 或 web 端 app。
- `:238-269` `dispatch`/`findWaveOwningCard` —— 依赖 `QueryClient`。不是 DOM 但是 **TanStack 依赖**。
  `core/events` 若含 `invalidationPolicies`（§6 第 5 项要它类型级穷尽）就把 TanStack 绑进跨端层——
  现状策略里 `apply`/`remove` 直接操作 qc（`:247-253`）。**要么写明接受这个绑定，要么策略表只描述 key 不描述动作。**

另有一条**下沉后最易丢**的排序不变量文档只字未提：`setSyncEventVersion → subscribe → start` 严格按此序
（`eventBridge.tsx:178-189`，引 issue #198 concern 2）。拆成 core+app 后它跨了模块边界，
**必须在 §6 第 5 项接口里用类型强制**（如 `start()` 只能从 `subscribe()` 返回值上调），否则退化成注释。

## §3 `core` 允许无状态 JSX —— 判据不够，且 markdown 跨端共享的前提**当前不成立**

**判据的漏洞**（黑名单枚举 + 对 `gates-types.yaml` 已有 lint 缺口的类比）：

- 漏掉 `useRef` / `useLayoutEffect` / `useSyncExternalStore` / `useImperativeHandle` —— 全是宿主耦合
- 只禁了 `on(Key|Pointer|Touch)*`，**`onClick`/`onWheel`/`onScroll`/`onFocus`/`onDrag`/`onChange` 全放行**。
  一个 `onClick` 就足以把交互模型漏进 core
- 漏掉 `ref={...}` 拿 DOM node、`dangerouslySetInnerHTML`、`document`/`window` 全局引用
- **绕过路径有先例**：`gates-types.yaml:710-718` 记录 `no-react-state-hook-members` 对
  `React['useState'](...)` computed member **完全绕过**；`:914` 记录 `no-raw-primitive-role` 对
  `role={expr}` 动态形式**刻意不检查**。同样的绕过对新规则一字不改地成立。

**建议换判据**：黑名单必漏，改**白名单 import**——`core/render/**` 只允许 import `react`
（且只允许具名 `createElement`/`Fragment`/`memo`）+ `core/**`。100% 静态可判，无 computed-member 绕过。

**markdown 跨端共享：我的结论是"现在不成立，收敛后才成立"。**
`pages-shared.yaml:3789-3805`(INV-DUP-004) 明确：四处管线里 `SpecConversation.tsx:106-110`
**只有 `remarkGfm`，没有 `urlTransform`、没有 `ReportLink`** —— 我核对了源码，agent 气泡确实是裸
`<ReactMarkdown remarkPlugins={MARKDOWN_PLUGINS}>`，无 `components`。**行为已发散**，不是"五份一样的拷贝"，
oracle 要求"合并时必须显式决策而非默认对齐"。INV-DUP-005 更硬：TOC 两套 id 前缀方案不同
（`<blockId>-h<n>` vs `md-h-`）**且各自的锚点稳定性都要保留**。

推论：`core/render/markdown` 不能是"一个 renderer"，必须是**一个内核 + 一组显式配置点
（urlTransform / link component / heading-id 策略）**，而其中 `ReportLink`（要路由跳转）**天然端相关，只能注入**。
§8 的"重合近 100%"偏乐观 —— `neige://` 深链语义（`pages-shared.yaml:716`：非 `neige://` 必须交回
`defaultUrlTransform`）在 mobile 上要不要成立是**未决的产品问题**，不是技术问题。

## §4 十一条病灶的机器检查 —— 逐条评估

- **1 / 6 / 9 / 10 ✅ 高**：import 图约束用 `eslint-plugin-import/no-restricted-paths` + `no-restricted-imports`
  patterns + 目录 glob 断言，全部平凡可做。
- **3 ✅ 高**：`createContext` 是具名调用，白名单可靠。注意 `React.createContext` member 形式要一并覆盖
  （`gates-types.yaml:695` 的教训）。
- **4 ⚠️ 中**：`/^calm[:.]/` 会误伤测试 fixture 与 e2e；`'calm-prose'`/`'calm-select'` 靠 `[:.]` 躲开纯属巧合。
  **建议改成"调用 localStorage/sessionStorage 时实参必须是 `keys.*()` 调用"**——卡出口比卡字面量准。
- **8 ⚠️ 中**：绕过路径有模板字符串、变量、`[class*=]`、`getElementsByClassName`。
  **建议反向白名单：选择器字面量必须匹配 `/^\[data-/`**，无绕过。
- **11 ✅ 配置即检查**：但"`eslint-disable` 必须带理由"要定义机器可判形式（建议：同行 `-- ` 后 ≥10 字符），
  否则退化成人工 review。
- **2 ⚠️ 与 oracle 正面冲突、5 ❌ 不现实、7 ⚠️ 有洞** —— 见下。

**#2「禁止模块级可变导出」—— AST 能识别，但会与三组 oracle 正面冲突（读了 `cards-terminal.yaml` 得出）：**

- **`declare module` 类型注册是必须的模块级全局，无法注入**：`GATE-CARD-083`
  （`lifecycle.ts:116-133` 用 `declare module './registry'` 合并注入 `createController`/`wheelTarget`/
  `refreshBacking`）与 `GATE-CARD-084`（`builtins/terminal.tsx:20-24` 往 `WaveCardDataMap` 注册 data 类型，
  **这是让 `card.type` 在类型层穷尽的唯一机制**）。实例化 registry 解决不了它们。
  **规则必须显式豁免 type-only 声明**，否则 agent 会为过 lint 把类型穷尽拆掉，净损失。
- **注册顺序承重**：`INV-CARD-225`（`builtins/index.ts:31-45`）——
  terminal→codex→spec→claude→wave-report→file-viewer→iframe→plugin-iframe 的顺序
  **决定 `adaptKernelCard` 兜底全扫的命中结果**（`INV-CARD-073` 第三段按 Map 插入序全扫）。
  改成注入后顺序由 `app` 组装代码决定——**这不是消除顺序依赖，是把它搬家且不再有 boot-once 守卫锁定**。
  建议直接消灭"兜底全扫"（要求所有条目声明 exact/prefix claim），顺序依赖才真的没了；
  否则 §2.1 表里"无 boot 顺序依赖"是**空头承诺**。
- **`INV-CARD-101` 的"值相等才删"守卫必须保留**（`resolver.ts:22-34`，防 StrictMode 双挂载时旧实例
  cleanup 删掉新实例）。它与单例无关，实例化后同样成立，**必须写进 §6 第 6 项 registry 契约**。
- **误报率（推测）**：顶层 `const X = new Map()` 作为不可变查表（icon map / mime map）会被误杀。
  建议收窄为"顶层 `let` / 被 export 的 Map|Set|数组 / 被 export 的顶层 `new`"，
  可变顶层常量须 `Object.freeze` 或 `as const` 才放行。

**#5「AST 相似度重复检测」—— 不现实，应直接退化为硬编码清单。** (a) 阈值校准要样本，新仓没有样本；
(b) oracle 已给出**精确 5 组清单**（INV-DUP-001..006，每条带 source 行号），硬编码归零断言成本≈0、误报=0；
(c) jscpd 类工具对 React 的 JSX 骨架天然高相似，噪音大到最后会被 `--ignore` 掉。
**建议**：jscpd 只做**非阻塞报告**，阻塞门是 INV-DUP-001..006 各自的"单一模块存在性"断言。

**#7「全局类名清单钉死在测试里」防绕过** —— 现表述有洞：只能断言"清单里的类都存在"，
挡不住"新写全局类但不加进清单"。**必须反向断言**：postcss 解析 `styles/**/*.css` 抽出全部类选择器，
与清单做**双向 set 相等**（多一个少一个都红）；再加"CSS Modules 内禁 `:global(...)`"。两条合起来才闭合。

## §5 a11y 契约机器化 —— 三种手段逐条

数据对齐：`a11y-contract.yaml` 110 条中 NONE **74**、`intentional_omission: true` **18**，与 §3.1 一致
（全仓 NONE 327、故意不做 217；文档写 325/215，2 条误差无关紧要）。

1. **Tab 顺序表作为数据 + e2e 读它 —— ✅ 可行，三条里最有价值。** 形态建议写死：每个路由旁
   `tab-order.ts` 导出 `readonly string[]`（`data-testid` 值），e2e 一个泛化 spec 遍历全部路由。
   风险：`capabilities-e2e.yaml` 头部把 "focus traps across real Tab" 列为**结构上只能 e2e**，
   即这条只能进 e2e 项目、不能进 vitest，而 e2e 最慢最脆。
   **建议加一条便宜的 jsdom 前置检查**：断言表里每个 id 在渲染树中存在且 `tabIndex >= 0`，
   把"表写错了"与"真实 Tab 顺序不对"两类失败分开。
2. **stylelint：`outline:none` 必须配 `:focus-visible` —— ⚠️ 部分可行。** "同选择器"可判（平凡）；
   **"相邻规则"不可靠**——stylelint 按文件 AST 遍历，"相邻"在 CSS Modules 拆分后跨文件即失效。
   **建议改为纯语法判定：全仓禁止裸 `outline: none|0`，除非该 rule 的选择器含 `:focus-visible`**，
   去轮廓必须写成 `&:focus-visible{outline:...}` 配对。无绕过。`禁 *:focus{outline:none}` 平凡可行。
3. **每条"故意不做"落反向测试 —— ✅ 技术可行，但有一部分只能 e2e。** 文档举的例子
   （grid 视图方向键不触发重排）恰好落在 e2e-only 清单（"wheel routing, CSS geometry/overflow"
   结构上无 jsdom 等价）。可 jsdom 化的例子是好的：`INV-DIRPICK-001`（DirectoryBrowser 绝不声明自己的
   `role="dialog"`，`DirectoryPicker.tsx:452-461`，已有正反两条测试）——`queryByRole('dialog')` 断空即可。
   **建议 oracle schema 加 `test_tier: unit|e2e|none`**，派发前把 217 条分好档。
   否则 agent 会把该进 e2e 的写成假 jsdom 断言（用 `fireEvent.keyDown` 断言"没重排"——jsdom 无布局，
   该断言**恒真**，比没测试更坏）。**这是反向测试的最大风险，文档没提。**

## §6 CSS `@layer` 层序 —— 顺序基本对，但 `vendor` 层对 CodeMirror **无效**，且 §4 的 stylelint 规则会**制造事故**

**层序 `reset, vendor, tokens, base, astryx, ui, features, overrides` 我认为基本对**：
`reset` 在 `vendor` 前 ✅（reset 不该赢过第三方组件自有样式，正是 Astryx spike 的教训）；
`tokens` 位置无所谓（自定义属性免疫 layer）；`astryx` 在 `ui` 前 ✅。
唯一要改的是 `vendor` 必须拆成「静态 CSS」与「运行时注入不可控」两类，理由见下。

**关键事实（读代码 + 读依赖得出，不是推测）：**

| 第三方 | 引入方式 | 能否进 `@layer vendor` |
|---|---|---|
| xterm | `XtermView.tsx:5` `import '@xterm/xterm/css/xterm.css'` | ✅ 可以（静态 CSS，Vite 可包 layer） |
| react-grid-layout / react-resizable | `WaveGrid.tsx:13-14` 两个静态 import | ✅ 可以 |
| **CodeMirror 6** | **全仓无任何 `.css` import**（`@uiw/react-codemirror` + `@codemirror/view`） | ❌ **不能** |

CodeMirror 6 用 `style-mod` 在运行时把 `<style>` 注入 `document.head`。**运行时注入的是 unlayered 样式，
而 unlayered 普通声明优先级高于任何 `@layer` 内的普通声明**（与 Astryx spike 同一条规则，方向相反）。
所以 **§4 的检查"`styles/` 之外的 CSS 必须整体在 `@layer` 内"一旦上线，现有全部 `.cm-*` 覆盖立即失效** ——
`calm.css` 里 `.cm-*` 规则 **14 处**（`:2140-2145` report-code、`:5089-5096` wave-report-files、
`:5318-5347` file-viewer 含 `.cm-panels.fv-code-search-panels-empty`/`.cm-searchMatch(-selected)`、
`:5546-5547` merge view）。`.xterm*` 20 处、`.react-grid*`/`.react-resizable*` 12 处
（我按选择器行计数 46 行，与 ~168 行的差异是规则块行 vs 选择器行）。

**另有 `!important` 反转坑**：`calm.css:5658`
`.xterm-container .xterm/.xterm-viewport/.xterm-screen { background: transparent !important }`。
**`!important` 声明的层序是反的——低层（vendor）的 `!important` 赢过高层（overrides）的 `!important`。**
`node_modules` 未装，**未能核实 xterm.css 自身是否含 `!important`**：规则已验证，具体风险待实测。

**第三方覆盖规则该放哪层（我的建议）：**
- `.xterm*` / `.react-grid*` / `.react-resizable*` 的覆盖 → **`overrides` 层**。原始 CSS 在 `vendor`，
  层序天然赢，**可以顺手删掉现有的 `!important`**。
- **`.cm-*` 的覆盖 → `styles/vendor-escape.css`，显式 unlayered**，并在 §4 的 stylelint 规则里
  开一个**具名例外**（只允许这一个文件）。CodeMirror 运行时注入不可控，唯一稳的办法是同为 unlayered 再比特异性。
- xterm 若真带 `!important`，覆盖侧也只能 `!important` **且必须 unlayered**。

**并且**：`vendor` 层的 xterm/rgl 静态 CSS 要靠 `styles/index.css` 里 `@import url(...) layer(vendor)` 引入，
**把 JS 侧的 `XtermView.tsx:5` / `WaveGrid.tsx:13-14` 三行 import 全删掉**。文档没说这步，
agent 大概率保留原 import → vendor CSS 仍是 unlayered → **整套层序当场失效且症状隐蔽**。
这条要写进 §6 接口冻结清单第 8 项。

## §7 三处未定归属 —— 推荐

**(a) `DirectoryPicker` → 拆两半：`ui/directory-browser`（原语，收注入的 port）+ `features/wave/create` 薄接线。**
读代码：它 `import * as api from '../../api/calm'`（`:29-31`）直接打 `GET /api/fs/listdir`；
`INV-DIRPICK-002` 记录它**刻意**用 `useModalView` 接管 modal body（浮层与 modal flex sizing 打架）；
`INV-DIRPICK-004` 是标准 combobox + listbox + `aria-activedescendant`。
17 条 oracle（14 `shared/directory-picker` + 3 `ui/directory-browser`）**全是形态约束，没有一条提 wave**，
唯一不通用的是"从哪拿目录列表"。**推荐**：`ui/directory-browser` 接 `listDir:(path)=>Promise<Entry[]>` port，
不 import `core/api`；`features/wave/create` 传 `api.listdir`。这同时满足 §2「ui 不得 import 业务」
与 §2.1「primitive 不被反向依赖」。child-view stack 不是问题——`useModalView` 本就住 `ui/dialog`，同层。

**(b) `core` 允许无状态 JSX → 有条件同意**：判据换白名单（见 §3），目录固定为 `core/render/` 而非散落；
**并必须加一条**：`core/render/**` 每个组件都要能接受注入的 link component 与 urlTransform（INV-DUP-004 的
发散是既成事实）。做不到"注入而非硬编码"，我的建议翻转为**不允许**——那时它已不是纯渲染。

**(c) `systems/editor` → 沿用 folder-level split，不建 workspace。**
(1) `editor/README.md:20-21` 原话是 "promotion to a workspace is deferred until the editor's surface area
justifies the build/tooling overhead"，`:24` 明说 **"Scaffold only"**；
(2) oracle 里 `owner_slice: editor` 只有 **3 条**，按 §7 粒度连一个 agent 的活都不够；
(3) 它真正的契约边界是 `crates/calm-editor` 的 ts-rs 生成类型（README `:14-17`），
该边界在 `core/api` 的生成类型里已有位置——workspace 解决的是打包问题，这里没有打包问题。
**建议**：保留 folder-level，把 README 那段 boundary 声明原样搬过去（唯一记录该决定的地方），
oracle 里 3 条标 `migration: deferred`。

## 附：建议在进入阶段 1 前补的最小增量

1. §2 加第六类 `tooling/`（84 条），§4.5 把 `styles/` 提为正式层（68 条）。
2. §2.2 加第三条合法路径：**跨 feature 的有状态 UI 下沉 `ui/`，且必须把数据来源改为注入 port**
   （DirectoryPicker 与 NewTaskForm 是同一类问题，一条规则解决 133 条 oracle）。
3. §6 的 8 项接口每项标明**产出文件路径**，并把 §7 的所有权表提前到接口冻结的**同一批**产出。
4. oracle schema 加 `test_tier` 字段，把 217 条"故意不做"分 unit/e2e/none 三档（防恒真测试）。
5. §4 的 stylelint「必须在 layer 内」规则**必须带 CodeMirror 例外**，否则上线即事故。
