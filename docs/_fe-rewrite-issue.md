在 `fe/` 下重建前端。目标不是复制现有 `web/`，是**拿到更干净的架构**。

架构设计与契约提取已完成，当前进入**阶段 1：接口冻结**。本 issue 是实现的唯一输入。

- 架构全文 `docs/_fe-architecture.md` · 执行计划 `docs/_fe-rewrite-plan.md`
- 契约库 `docs/oracle/`（**1127 条**，schema 见 `docs/oracle/SCHEMA.md`）

---

## 1. 目标与非目标

**目标**——解决具体的、已定位的结构问题，每条可验收：

1. **依赖方向单向且可 lint** — 现架构靠 context 与模块级单例穿透层级，import 图看不见真实耦合
2. **消灭隐性契约** — `docs/a11y-contract.md` 的 110 条契约中 **74 条（67%）无任何测试锁定**
3. **副作用集中且显式** — 单例、持久化 key、DOM 查询、全局样式现在散落各处

**非目标**：

- ❌ **不是像素对齐现有 web**。视觉只需"设计系统内部一致"
- ❌ **不换技术栈**。React 19 + Vite + TanStack Router/Query + zod 全部沿用
- ❌ oracle **不是**截图基线，是能力清单 + 不变量集 + 契约测试
- ❌ **mobile 端不在本次范围**（已 defer）

## 2. 现状事实

| | |
|---|---|
| 实现代码（排除 test/generated） | **26,596** 行 |
| 测试代码 | **37,363** 行 |
| `web/src/calm.css` | **6,392** 行，599 个全局类 |
| 带 `className` 的实现文件 | 58 / 72，共 695 处 |
| 路由 | **4 条**：`/`、`/cove/$coveId`、`/wave/$waveId`、`/settings`（+ Login） |
| 卡片类型 | 6：terminal / codex / spec / iframe / file-viewer / wave-report |
| report block 类型 | 5：prose / chart.candles / table / app / task |

CSS 按域分布（前 10 占 60%）：`report 859 · wave 663 · side 541 · file 359 · today 301 · cal 298 · rb 251 · card 245 · new 189 · dirpicker 153`

## 3. Oracle：1127 条契约

三部分，**都不是截图**：

- **能力清单**（capability 290）— 用户可达的能力。丢了功能就少了
- **不变量集**（invariant 697）— 踩坑教训、排序依赖、竞态防护、"故意不做"。丢了会重新踩坑
- **机器闸门**（gate 140）— 类型级穷尽、lint 规则、token 形状契约。丢了防线就没了

### 条目 schema

```yaml
id / kind / family / statement / why / source / authoritative_test
owner_slice          规范化模块路径（见 docs/oracle/owner-aliases.yaml）
runtime_layer        core | ui | systems | features | app | styles | none
verification_owner   e2e | unit | lint | css | build | architecture | review-waiver | null
test_tier            browser | jsdom | static | none
intentional_omission true = "故意不做"型契约
migration            pending | skipped（skipped 必带 skip_reason）
```

### 关键统计

```
runtime_layer   features 407 · systems 271 · app 138 · ui 132 · none 96 · styles 57 · core 26
test_tier       browser 585 · jsdom 424 · static 80 · none 38
migration       pending 1122 · skipped 5
归层率          100%（1127/1127），114 个规范 owner_slice
```

### ⚠️ 两个必须先知道的数字

**① `core` 只有 26 / 1127 条。** 平台无关的纯逻辑面很小——这决定了 `core` 的边界必须收紧（见 §9）。

**② 200 条恒真断言风险。** `test_tier: browser` 共 585 条，其中 **200 条当前 `verification_owner: unit`**（systems 100 · features 54 · ui 38 · app 6 · core 1 · styles 1）。它们涉及布局／几何／滚动／真实焦点／canvas／PTY——**jsdom 没有布局，"断言没有发生重排"永远通过**。这批必须搬到真实浏览器，单 slice 的验证成本按此上调，不能按单测估。

### 如何消费

- 有测试的 → 翻译（提取其断言的不变量，在新结构下重述）
- **无测试的 325 条 → oracle 条目本身就是规格，必须新写 contract test**
- **`migration: skipped` 的 5 条不要实现**：`INV-APP-019`、`INV-APP-105`（被 events typestate 取代）、`INV-DEAD-001..003`（死代码）
- `NORMALIZATION-REPORT.md` 里 **13 条 / 6 项**标「需人工裁决」——有临时归属，但层的选择存在实质分歧，**必须在阶段 1 内裁决**，否则会把分歧固化进文件划分

## 4. 架构：五层

```
┌──────────────────────────────────────────────────────────────┐
│ app/       router · providers · theme · shell · di            │
│            events = EventBridge · invalidation-adapter · trace│
│            ★ createContext 白名单主体（primitive 自有目录例外） │
├──────────────────────────────────────────────────────────────┤
│ features/  wave · cove · today · report · spec                │
│            settings · auth       ★ 域之间禁止横向依赖          │
├──────────────────────────────────────────────────────────────┤
│ systems/   cards · terminal · wheel · fs-viewers · editor     │
│            events = transport · cursor-store · probe          │
├──────────────────────────────────────────────────────────────┤
│ ui/        dialog · menu · focus · roving                     │
│            directory-browser · schema-form/fields · state     │
│            ★ 不得依赖业务 domain（仅 core/types 白名单）        │
├──────────────────────────────────────────────────────────────┤
│ core/      api · domain · schemas · keys · state              │
│            markdown · events = protocol · reducer · plan       │
│            ★ 禁 JSX、禁 React、禁浏览器 API                    │
└──────────────────────────────────────────────────────────────┘
              依赖只能向下，不能向上或横向
```

**`system` vs `feature` 判据**：只有具备**独立资源生命周期、协议、或宿主能力**的模块才是 system。纯页面行为归 feature。（推论：cards 的 registry/host/lifecycle 是 system；`WaveGrid`/`WaveList` 的页面布局归 `features/wave`。）

**features 之间禁止横向 import**。跨域只有两条路：下沉 `core/domain`（数据模型/纯逻辑）或上提 `app`（组装关系）。**业务层不得 import `app`**——"经 app 中转"不等于 `wave → app → cove`。

**没有 `shared/`。** "shared" 是没想清楚归属的代名词；现有 `shared/components/` 混装 primitive、业务组件、外壳三种东西，oracle 里 104 条挂在 `shared/*`，全部重新分配。

## 5. 目录

```
fe/
├── core/        平台无关逻辑。api · domain · schemas · keys · state · markdown · events
├── mock/        从 openapi.json 生成（禁止手写 handler）
└── web/src/     app/ features/ systems/ ui/ styles/
```

**`core` 与 `web` 分离的理由是可测试性，不是跨端。** `core` 里没有 `WebSocket`/`localStorage`/`location`/`fetch` 的直接调用，平台能力一律经注入的 port——因此 `core` 能在 node 里直接跑测试，不需要 jsdom、不需要假 WebSocket。

判据是 **platform-independent**，不只是 **DOM-free**。这两个不是一回事：`web/src/api/events.ts` 一行 DOM 也没有，但有 7 处浏览器平台依赖：

```
WebSocket                        events.ts:172-175, 306-360
requestIdleCallback / setTimeout events.ts:160-170
localStorage                     events.ts:511-529, 585-596
location.protocol / host         events.ts:599-602
fetch(credentials:'include')     events.ts:551-578
模块级共享实例                     events.ts:604-634
loadCursor() 构造函数内同步返回      events.ts:204   ← 连"形状"都不是平台无关的
```

> mobile 端已 defer。`core` 的平台无关约束同时也是未来加端时的预留，但当前的正当性来自可测试性。

## 6. 十一条病灶 × 机器检查

**没有机器检查的条目不算解决——它会在半年内长回来。**

| # | 病灶 | 机器检查 |
|---|---|---|
| 1 | 卡片创建在 `router.tsx:415/481/531` | dependency-cruiser：任何层只能经 `systems/cards/public.ts`，禁一切深导入（只查 `app/router.*` 会被挪到 `app/cardNavigation.ts` 绕过） |
| 2 | 6 处模块级单例 | `architecture/no-module-runtime-state`，见下方精确定义 |
| 3 | context 跨域穿透 | 基于 **import binding** 检测 `createContext`/`React.createContext`/alias（不能文本匹配）；白名单精确到文件 |
| 4 | `calm:sync:cursor` 硬编码 3 处 | ①字面量 `/^calm[:.]/` 只许在 `core/keys`；②`no-direct-persistence` 禁直连 localStorage/sessionStorage/IndexedDB |
| 5 | markdown pipeline 5 份 / SchemaForm 2 份 / calm-select 2 份 / TOC 2 套 | **硬编码清单，不用 AST 相似度**（重命名／抽 helper／JSX 展开会让阈值漂移，相似表单会误报）。唯一 public entry + 禁止直接 import `react-markdown`/remark/rehype/**`mdast-util-*`** + 禁手写 markdown 正则解析器（漏掉后两者，两个现存重复实现都能逃过）。清单 = `INV-DUP-001..010` |
| 6 | 6392 行全局 CSS | ①组件只可 import `*.module.css`；②普通 `.css` 只许 `styles/entry.css` import；③第三方 CSS 只从 `styles/vendor.css` 汇入 |
| 7 | 高扇出共享类（`.go` 跨 8 文件 6 域等） | CSS AST 生成 manifest 并**双向 set 相等**比对；禁 `:global(...)`（除 allowlist）；禁 manifest 外的全局类字符串；禁运行时拼类名 |
| 8 | 运行时按 class 查 DOM | 解析静态 selector，任何 class selector 都报错；覆盖 `querySelector(All)`/`closest`/`matches`/`getElementsByClassName`；**第三方 DOM 开 allowlist 口子**（`file-viewer.tsx:135` 查 `.cm-scroller`，CM 内部结构加不了 `data-*`）且必须带容器前缀 |
| 9 | 根目录散件 | `fe/web/src/` 顶层只允许 `main.tsx`，**同时覆盖 `.ts/.tsx/.js/.jsx`** |
| 10 | `WaveContext` 死代码 | oracle `migration: skipped` + `skip_reason` |
| 11 | lint 历史包袱 | `reportUnusedDisableDirectives:'error'` + 禁子目录 eslint 配置 + 禁本地 shim rule + `eslint-disable` 必带理由 |

### `architecture/no-module-runtime-state` 精确定义

判据不是"哪些 AST 节点"，是**"模块求值后可达的对象图是否可变、是否承载运行态"**：

| | |
|---|---|
| 禁 | 顶层 `let`/`var`；顶层构造 `Map`/`Set`/`WeakMap`/`WeakSet`/`EventTarget`/`WebSocket` |
| 禁 | **顶层可变对象字面量与数组**（`const cache = {}`） |
| 禁 | **class static 可变成员**（`class C { static current = new Map() }`） |
| 禁 | **IIFE / 闭包 lazy singleton**（`const get = (() => { let c; return () => c ??= make() })()`） |
| 允许 | primitive、函数声明、类型、schema、`Object.freeze` 的静态数据 |
| **必须豁免** | **type-only 的 `declare module` 声明合并** — `GATE-CARD-083/084` 证明它是类型穷尽的唯一机制，无法改注入 |

**无法零误报**，需要小型 allowlist。别指望规则全自动。

### 隐性契约的机器化

无测试锁定的 a11y 契约按 `verification_owner` 指派责任，**不能机械套模板**：

| 契约类 | 检查方式 |
|---|---|
| 逐页 Tab 顺序 | 独立契约文件 `contracts/a11y/tab-order.yaml`，用 **role + accessible name + 条件**描述（不用 CSS selector）。Playwright 从 `body` 真实按 Tab 遍历。**e2e 不得 import 生产代码旁的 TS 常量**——否则实现和预期会被一次修改"自洽地改错"。必须分别覆盖条件状态、Shift+Tab、Dialog 打开后的 trap |
| focus-visible | 禁 `outline: none\|0`，除非带注释且在 allowlist；允许移除 outline 的组件要求同一 stylesheet 有对应 `:focus-visible` 且提供非透明 outline/box-shadow；关键控件由 Playwright computed-style 兜底。**不要用"相邻规则"判定**——CSS Modules／嵌套／layer／媒体查询会让"相邻"失去意义 |
| motion | **只对出现 `animation`/`transition`/`scroll-behavior` 的模块**要求 reduced-motion override；关键页面用 `reducedMotion:'reduce'` 跑 e2e。（没动画的文件不该被迫加空块；`!important` 不是正确性的必要条件；全禁 `animationend` 会误杀合法清理） |
| 215 条"故意不做" | **逐条评估**：反向键盘测试 / 正向精确断言 / node identity 保留断言 / 行为回归锁定（effect 声明顺序不能用"不发生"测）/ oracle migration gate（纯产品决策不写运行测试） |

> ⚠️ **恒真断言是最大陷阱。** jsdom 没有布局，"断言没有发生重排"**永远通过**。凡布局／几何／滚动／真实焦点／canvas／PTY 相关，`test_tier` 必须是 `browser`。

## 7. CSS 分层

```css
@layer reset, vendor, tokens, base, astryx, ui, features, overrides;
```

**六个坑**：

1. **自定义属性不免疫 layer** — 它同样参加 cascade 与 layer 排序；`var()` 在使用点解析是另一回事
2. **声明 `@layer astryx` 控制不了 Astryx** — 它用的是 `astryx-base`，必须 `@import url(...) layer(astryx)` 或把真实层名写进总层序
3. **CodeMirror 6 完全绕过 layer** — CM6 用 style-mod **运行时注入未分层 `<style>`**，全仓无 CM 的 CSS import。一旦执行"所有 CSS 必须在 layer 内"，`calm.css` 里 **14 处 `.cm-*` 覆盖立即失效**
4. **`!important` 的 layer 优先级是反转的** — 普通声明后层赢，`!important` 早层赢
5. **未分层 CSS 仍压过一切** — 需要**两种**检查：build audit（静态 CSS）+ runtime audit（Playwright 遍历 `document.styleSheets` + `<style>` + **`[style]` 元素**）
6. **CSS Modules 不自动进 layer** — 每个 `.module.css` 仍需自己包 `@layer features`/`ui`

**第三方 CSS 去处**：`.cm-*` → 具名 **unlayered 例外文件**（加 stylelint「最右复合选择器必须含 `.cm-`」钉死爆炸半径）；`.xterm*`/`.react-grid*` → `overrides` 层（顺手删 `!important`），且它们现在的**三行 JS `import` 必须改成 `@import ... layer(vendor)`**，否则整套层序静默失效。

> ⚠️ **一个上线即失效的遗漏**：14 处 `.cm-*` 规则的祖先 hook（`.file-viewer-code-wrap` 等）是**应用全局类**，转 CSS Modules 后会哈希化，unlayered 例外文件将无法引用。这些祖先 hook 必须先进全局类 manifest 或改 `data-*`。

## 8. 事件流三分法

```
core/events/         protocol · reducer · invalidation-plan（纯，不 import QueryClient）
web/systems/events/  websocket-transport · cursor-store · unauthorized-probe
web/app/events/      EventBridge.tsx · query-invalidation-adapter
```

reducer 输出 effect 由端侧执行：`{persist-cursor} | {invalidate} | {clear-cache} | {reconnect}`。
拆分的收益是**协议正确性变成纯函数测试**——cursor 推进、control frame、version gate 不再需要在 React + TanStack + 假 WebSocket 里验。

**排序不变量用类型消除，不用运行时守卫。** `INV-APP-019` 要求 `setSyncEventVersion → subscribe → start`。运行时守卫只能保住 `set < start`，`set → start → subscribe` 照样通过。改用：

```ts
interface UnconfiguredEventStream {
  configure(opts: { syncEventVersion: number | null; topics: Topic[] }): ConfiguredEventStream
}
interface ConfiguredEventStream { start(): void; stop(): void }
```

subscribe 折进 configure，三步坍缩成一步——**排序不变量消失而非被守卫**。

**类型管不了的两条，仍需 contract test**：`INV-APP-001`（EventBridge 必须挂在 `ServerCompatGate` 内）、`INV-APP-020`（唯一 `start()` 调用方）。

> ⚠️ 必须保留测试逃生口：`events.ts:624-626` 注释明写"测试可直接 `new EventStream(url)` 后 `start()`"。类型分裂会封死它——用 `EventStream.forTest(url).configure({...}).start()` 保留"不经 bridge 也能连"，但不保留"跳过配置"。

## 9. `core` 不含 JSX

**裁决：不允许 `core/render/`。** 黑名单 lint（禁 `react-dom`/`useState`/`useEffect`/`on(Key|Pointer|Touch)*`/`createContext`）有 8 类绕过：`onClick`/`onChange`/`onWheel` 不在正则内、`useRef`/`useLayoutEffect`/`useSyncExternalStore` 未禁、`document`/`window`/`matchMedia` 未禁、hook 可 alias、可渲染内部有 state 的导入组件、class component、`dangerouslySetInnerHTML`、间接 `createPortal`。封死这些等于写一个不完整的 React effect system。

```
core/markdown/                     parse · normalize · sanitize-ast-policy · outline · block schema
web/src/features/report/render/    React renderer
```

`sanitize` 在 core 里是 **sanitize-ast-policy**（输出平台无关的安全中间 AST）。一旦它依赖 React 元素/属性白名单、`ReportLink`、URL transform 或 `dangerouslySetInnerHTML`，就属于 renderer adapter。

**outline 必须参数化**（两套实现的差异是配置不是分歧）：

```ts
parse(md): NormalizedMarkdownAst
extractOutline(ast, { maxDepth, headingId, textPolicy }): HeadingOutline[]
```

| | report | file-viewer |
|---|---|---|
| 解析 | mdast，`fromMarkdown()` 在 `report-outline.ts:72` | 手写解析，支持 setext/fence/inline（`file-viewer-markdown-toc.tsx:9`） |
| 层级 | H1–H2 | H1–H4 |
| ID | `<blockId>-h<n>`，**block 内局部 ordinal** | `md-h-<n>`，**全文件全局 ordinal** |

> report 的 **block-to-heading 归组、fallback 顶层项（`number:null`）、连续编号、`children` 组装**不是 AST heading policy，属 `features/report` 的纯组合函数，**不进 core**。
>
> `headingId` 必须能拿到 heading 节点、全局/局部 ordinal 与调用上下文；`textPolicy` 必须是**冻结的策略类型**而非无约束 callback。

## 10. 技术栈

| | 决定 | 依据 |
|---|---|---|
| React 19 + Vite + TanStack + zod | **沿用** | 栈不变则老代码可作参考，知识可迁移 |
| CSS Modules + `@layer` | 采用 | 病灶 6/7 |
| **Astryx**（`@astryxdesign/core`） | **采用，锁死精确版本** | 实测：**零构建配置**（自带预编译 dist，peer dep 只为运行时 `stylex.props`）；14/14 组件齐；tree-shaking 有效。但 **5.5 周 12 版、67% 带 breaking 且无 codemod** → 升级必须当独立任务排期 |
| Astryx `<Theme>` 组件 | **不用** | 它往 `<html>` 写 `data-theme`，与自有主题机制撞车。只用组件，主题归 `app/theme` |
| Astryx Tooltip | **禁用** | 触屏 tap 无反应（源码 `if (!target.matches(':focus-visible')) return`），只有键盘可达 |
| `astryx.css` 148.6 kB / 25.8 kB gzip | 接受 | 单体，与用量无关，`exports` 无组件级子路径 |

## 11. 八项已裁决

| # | 裁决 |
|---|---|
| 1 | `core` **不允许** JSX → `core/markdown` + 端侧 renderer |
| 2 | `DirectoryPicker` **两层，全在 `ui/`**：`ui/directory-browser`（注入 `listDir` port）+ `ui/schema-form/fields/DirectoryField`。**不设 `features/wave/create` 那层**——它由卡片 create schema 的 `type:'directory'/'file'` 驱动（`SchemaForm.tsx:119-126`），与 wave create 无专属关系；放进 features 会让 `ui/schema-form` 向上 import features |
| 3 | `systems/editor` 沿用 **folder-level split，不建 npm workspace**。升级触发条件：出现第二个 JS 消费端 / 需独立版本发布 / 依赖树或构建耗时需隔离 |
| 4 | `events` 三分（§8） |
| 5 | 重复检测用**硬编码清单**（`INV-DUP-001..010` 全部），展开成具体 import/path 约束。AST 相似度只作非阻断报告 |
| 6 | **保留 `declare module` 类型注册**，但 `systems/cards/public.ts` 必须重新导出合并后的完整接口类型；`no-module-runtime-state` 豁免 type-only |
| 7 | `ui` 可依赖的 `core/types` 白名单**只允许两类**：branded ID 类型（`WaveId`/`CoveId`/`CardId`，无字段无方法）、无障碍原语类型（role 枚举、focus 目标描述）。**任何带业务字段的 domain 类型一律禁止** |
| 8 | **不要宣称"消除 boot 顺序依赖"**：`INV-CARD-225` 中 codex 与 spec 共用 kernel kind `'codex'`，注册顺序决定兜底全扫的命中结果。顺序依赖是**语义**的不是**机制**的，改注入只是搬家。正确做法：顺序从"隐式 import 副作用"变成 `app` 里显式的一行，并由 contract test 锁定 |

## 12. 并行策略：接口优先

按页面切片会继承现有耦合（10 组硬耦合、5–6 层依赖链），并行不起来。**先冻结跨模块接口，实现只依赖接口，才能扁平并行。**

| 阶段 | 可并发 | 状态 |
|---|---|---|
| 架构设计 | 1 | ✅ |
| oracle 提取 | 30–50 | ✅ 1127 条 |
| **接口冻结** | 3–5 | ← 当前 |
| 实现 | 20–30 | 见下 |
| 集成 | 1–3 | 不可压缩 |

实现阶段的三个真约束：

1. **写冲突** — 可设计掉：worktree 隔离 + 文件所有权表，每个文件恰好一个 owner
2. **接口漂移** — agent 不得自行改接口，必须回接口层裁决并广播。**宁可一个 agent 阻塞，不可两个 agent 各改一版**
3. **review 带宽（唯一硬约束）** — 缓解：三关自动判据（contract test 全绿 → 能力条目全勾 → 不变量迁移清单全覆盖），全绿的只抽样，人只 review 架构决策与接口

## 13. 阶段 1：接口冻结

### 冻结顺序

| 序 | 接口 |
|---|---|
| **1a** | `core/state`：`Persistent<T>` **条件返回类型** · codec · storage port（**无 React**）。硬闸是类型不是 lint |
| **1b** | `ui/state`：React hook wrapper。**放 `ui` 不放 `app`**——否则下层要向上 import，破坏单向规则 |
| **2** | `core/keys`：持久化 key 工厂 + storage adapter port |
| 3 | `core/api`：契约 / schema / 错误规范化（transport 由端注入） |
| 4 | `styles/tokens`：token 定义 + **十类**形状契约（单模标量内部还有 7 个形状各异的子族） |
| 5a | `core/events`：protocol / reducer / invalidation-plan |
| 5b | `systems/events`：typestate。**冻结面必须含四项**：handler 注册属哪个 typestate 且会不会漏第一帧、`configure()` 不得连接、重复 configure 语义、唯一 start ownership |
| 5c | `core/markdown`：`NormalizedMarkdownAst` · `parse()` error 通道 · `sanitize-ast-policy` · `TextPolicy` · `HeadingIdPolicy` · `HeadingOutline` + 下列七项语义 |
| 6 | `systems/cards/public.ts` |
| 7 | `ui/directory-browser` · `ui/schema-form/fields/DirectoryField` · `ui/dialog` · `ui/menu` · `ui/focus` |
| 8 | `styles/`：层序 + 全局类 manifest + unlayered 例外 manifest + `data-*` 约定 |

**前三个先做**：`core/state` 是所有层的最上游硬约束；`ui/state` 必须紧随，否则各域会自行包装出不同语义；`core/keys` 尽早统一，避免重新长出硬编码。

### `core/markdown` 的七项语义（决定类型，必须在阶段 1 内定）

CommonMark/GFM 方言 · raw HTML 是否保留 · setext/fence/图片 alt/inline code 的文本规则 · heading depth 范围 · ID 策略与跨版本稳定性 · 重复标题处理 · malformed markdown 行为（抛错还是降级）

### 出口验收清单

- [ ] 全部接口形成**可编译的类型 + 公开入口 + 错误/生命周期语义**，不再只有目录名或概念描述
- [ ] `core/markdown` 七项语义决定全部落定
- [ ] `systems/events` 四项冻结面 + 两条 typestate 外 contract test 已确定
- [ ] `styles` 的 token 形状、layer 顺序、全局类 manifest、unlayered 例外 manifest、`data-*` 约定固化
- [ ] 每个冻结接口至少有编译型/契约型测试覆盖主要消费者；**不得靠生产端与测试端共享常量形成自洽断言**
- [ ] 根据最终接口落出完整模块/文件清单
- [ ] **ownership manifest** 覆盖未来全部实现文件，每文件恰好一个 owner；冻结接口与 `styles/` 标只读
- [ ] **解决 `NORMALIZATION-REPORT.md` 中 13 条 / 6 项人工裁决**
- [ ] change-request 流程可执行，结束时无悬而未决的公开面变更请求
- [ ] schema / 枚举 / ID 唯一性 / owner 与 runtime_layer 一致性校验全绿

> **次序**：ownership manifest 是阶段 1 的**出口**，不是入口——它要列未来的文件路径，而路径由接口冻结决定。

## 14. AI 友好规范

核心：**agent 只靠文件路径就能定位全部相关信息，不需要全局理解。**

1. **局部性 > DRY（最重要）** — 实现/样式/类型/测试同目录。改 `ReportTaskBlock` 时不该需要在 6392 行 CSS 里搜 `rb-task`。**这是 CSS Modules 的真正价值——不是作用域，是可定位性。** 推论：宁可重复，抽象门槛是"第三次出现且语义相同"
2. **命名可推导，禁 barrel** — 路径 = 组件名 = module 名；`index.ts` 纯转出会让 agent 多跳一层且掩盖循环依赖（有实际逻辑的分发器不算）
3. **契约文档贴代码旁** — `web/src/ui/README.md` 是最佳范例，升格为全域模板：三段式（visual / a11y / test contract）、每条写"为什么"、**明确列出"故意不做"什么**
4. **测试即规格** — `*.test.tsx`（行为）与 `*.contract.test.tsx`（不变量）分离。流程强制：**先写 contract test 再写实现**
5. **每层一个 `AGENTS.md`** — 固定四节：放什么 / 不放什么 / 依赖方向 / 契约模板
6. **显式优于隐式** — props 显式类型、跨边界过 zod、必填字段类型为必填（不用 `Option<T>` + 默认值）、无隐式全局

## 15. ⚠️ 已知陷阱速查

**215 条 `intentional_omission`，其中 100 条（47%）无测试锁定**——看到"没实现"就补上，不会有任何测试报错。最容易被"顺手修好"的：

| 契约 | 会被误当成 | 真实后果 |
|---|---|---|
| ResizeObserver **首次通知必须丢弃**（`INV-CARD-018`） | bug | PTY 被收窄回去，重制 remount 数据丢失 |
| theme effect **不得加 prev 守卫**（`INV-CARD-030`） | 性能问题 | remount 重置组件内记账 → 跳过派发，正是 #177 要关掉的 bug |
| effect deps **故意排除 theme/status**（`INV-CARD-009`） | exhaustive-deps 违规 | theme 进依赖 → 重建 WS；status 进依赖 → `term.dispose()` 抹掉缓冲区 |
| **删掉**客户端活性定时器（`INV-CARD-007`） | 缺心跳 | 浏览器自动 pong 不过 JS，空闲 40s 误判假死 |
| 退出后**不渲染**覆盖层（`INV-CARD-055`） | 缺状态提示 | 覆盖层盖掉用户要看的输出（#306 warp 式保留缓冲区） |
| 卡片离开视口**不卸载**（`INV-CARD-106`） | 性能优化机会 | PTY 连接与 iframe 会话全丢 |
| codex **不跑本地 FSM**（`INV-CARD-174`） | 状态该在前端 | 与 kernel 服务端并集不一致 |
| `registry.ts` **保持纯 `.ts`**（`INV-CARD-082`） | 无所谓 | 改 `.tsx` 牵连构建与文档引用 |
| `cove.updated` 命中不存在的 cove 时 **no-op**（`INV-APP-031`） | 该补上数据 | 凭事件造出 phantom cove（#288） |
| 服务端重置检测**只在新 socket**（`INV-APP-043`） | bug | 注释原文："故意不做，重写时容易被误当成 bug 去补全" |
| 直接读 `dataset.theme` 而非 `useTheme()`（`INV-APP-071`） | 坏味道 | 复活 #177：订阅 → 重渲 wave 子树 → Suspense → remount XtermView |
| DEV 短路**必须内联**（`GATE-APP-079`） | 可抽成函数 | Vite 无法 dead-code，生产 bundle 带上 trace buffer。注释要求重构前 `grep __neigeEvents__ web/dist/assets/*.js` |
| 内置注册**顺序承重**（`INV-CARD-225`） | 数组顺序无所谓 | codex/spec 共用 kernel kind，兜底全扫命中结果会变 |

**其他易踩**：

- `Persistent<T>` 的硬闸是 `shared/state.ts` 的**条件返回类型**（塌成 `never`），eslint 只是人类可读层——**只搬 lint 不搬类型 = 闸门失效**
- `invalidationPolicies.ts:21` 的类型级穷尽是全仓最强的"新增事件"闸门，不需失效的要写 `noop('reason')`
- `--font-mono` 与 `font-stack.ts` 的 `MONO_STACK` 必须**逐字节相同**
- `--tracking-normal` 必须是 `0` 不是 CSS `normal`（为了让 linter 能禁裸关键字）
- `themeRgb.ts` ↔ `XtermView` ↔ Rust `RequestTheme::default_dark()` **三处必须同步**（跨语言不变量）
- **产品级"故意不做"**：wave archive UI、cove-folder 管理 UI、`report.md` 的 file-viewer 面板——kernel 支持但前端刻意没有，别热心补上
- 测试钉法：断言最终状态会漏掉"多发了一个请求"这类 bug（`#288` 用**计数 PATCH 请求**来钉）

---

## 下一步

进入**阶段 1 接口冻结**，从 `core/state` → `ui/state` → `core/keys` 开始。出口条件见 §13。
