# `fe/` 架构设计

状态：定稿 v6 — 阶段 0 完成，进入阶段 1 接口冻结
日期：2026-08-02
上游：`_fe-rewrite-plan.md`（阶段 0 的产出）

---

## 1. 架构目标

不是"重新实现现有功能"，是解决**具体的、已定位的**结构问题。目标有三条，每条都可验收：

1. **依赖方向单向且可 lint** — 现有架构靠 context 和模块级单例穿透层级，import 图看不见真实耦合
2. **消灭隐性契约** — 首个提取 agent 的数字：`a11y-contract.md` 110 条契约中 **74 条（67%）无任何测试锁定**。新架构必须把文档里的契约变成机器检查
3. **副作用集中且显式** — 单例、持久化 key、DOM 查询、全局样式，现在散落各处

---

## 2. 层定义

```
┌──────────────────────────────────────────────────────────────┐
│ app/       组装层                                              │
│            router · providers · theme · shell · di             │
│            events  = EventBridge · invalidation-adapter · trace│
│            ★ createContext 白名单的主体（另见 §2.1 primitive 例外）│
├──────────────────────────────────────────────────────────────┤
│ features/  业务域                                              │
│            wave · cove · today · report · spec                 │
│            settings · auth                                     │
│            ★ 域之间禁止横向依赖                                 │
├──────────────────────────────────────────────────────────────┤
│ systems/   子系统（有独立资源生命周期 / 协议 / 宿主能力）          │
│            cards · terminal · wheel · fs-viewers · editor      │
│            events  = transport · cursor-store · probe          │
├──────────────────────────────────────────────────────────────┤
│ ui/        交互原语                                            │
│            dialog · menu · focus · roving                      │
│            directory-browser · schema-form/fields · state      │
│            ★ 不得依赖业务 domain（仅 core 类型白名单，§8-7）   │
├──────────────────────────────────────────────────────────────┤
│ core/      平台无关逻辑                                         │
│            api · domain · schemas · keys · state               │
│            types = branded ID · 无障碍原语类型                  │
│            markdown · events = protocol · reducer · plan       │
│            ★ 禁 JSX、禁 React、禁浏览器 API                     │
└──────────────────────────────────────────────────────────────┘
              依赖只能向下，不能向上或横向

  同名不同层：`events` 在三层各有一段，职责严格分工，见 §4.7
  `state` 在 core（类型/codec/port，无 React）与 ui（React hook wrapper）各一段，见 §6

  styles/     全局样式层 —— 非运行时层，但有 owner（见 §4）
  （非运行时域）infra/e2e · lint · build —— 用 verification_owner 标记，不入五层
```

### 2.0 system 与 feature 的判据

两轮 review 都指出这条边界会产生归属争议。**判据固定为**：

> 只有具备**独立资源生命周期、协议、或宿主能力**的模块才是 `system`。纯页面行为归 `feature`。

推论：`cards` 的 registry / host / lifecycle 是 system；`WaveGrid` / `WaveList` 的页面布局归 `features/wave`。

### 2.1 与现有结构的关键差异

| 现有 | 新 | 为什么 |
|---|---|---|
| 卡片创建逻辑在 `app/router.tsx:415/481/531` | `systems/cards` 自治，只经 `public.ts` 暴露 | 路由不该知道卡片怎么造 |
| `cards/registry.ts` 三张模块级 Map + `resolver.ts` + boot-once 守卫 | registry 实例由 `app` 创建并注入 | 可测试、可多实例、**注册顺序显式化**（见下方警告） |
| `ThemeContext` 跨 5 个域、`ModalViewContext` 定义在 `ui/Dialog` 被业务消费 | context 只在 `app/**` 与 primitive 自有目录 | primitive 被业务反向依赖是层级破损 |
| `shared/components/` 混装三种东西 | 拆到 `ui/` · `features/*` · `app/shell`，**无 `shared/`** | "shared" 是没想清楚归属的代名词。oracle 里 104 条挂在 `shared/*`，全部要重新分配 |
| `src/` 根目录散件 | 全部归层 | |

> ⚠️ **不要宣称"消除 boot 顺序依赖"。**
> `INV-CARD-225`：codex 与 spec 共用 kernel kind `'codex'`，**注册顺序决定兜底全扫的命中结果**。
> 顺序依赖是**语义**的，不是**机制**的——改成注入只是把它搬家。
> 正确表述：顺序从"隐式 import 副作用"变成"`app` 里显式的一行"，并由 contract test 锁定该顺序。

### 2.2 features 之间如何通信

**禁止横向 import。** 跨域需求只有两条合法路径：

1. **下沉到 `core/domain`** — 共享的数据模型或纯逻辑
2. **上提到 `app`** — 组装关系（A 域的事件要触发 B 域的刷新，由 `app` 的 invalidation adapter 连接）

**明确否掉**："经 `app` 中转"不等于 `features/wave → app → features/cove`。业务层**不得** import `app`。`app` 单向依赖 features。

### 2.3 oracle 的层标注 schema

五层是**运行时**分层，装不下全部 1127 条 oracle（实测只有 70.6% 能无歧义归层）。oracle 条目额外携带：

```yaml
runtime_layer:      core | ui | systems | features | app | styles | none
owner_slice:        规范化后的模块路径（见 docs/oracle/owner-aliases.yaml）
verification_owner: e2e | unit | lint | css | build | architecture | review-waiver | null
test_tier:          browser | jsdom | static | none
```

`runtime_layer: none` 用于 lint 规则、CI 闸门、构建配置、e2e 基础设施——它们不属于五层，但必须有 `verification_owner`。

**`test_tier` 是防"恒真断言"的**：jsdom 没有布局，"断言没有发生重排"在 jsdom 里**永远通过**。凡涉及布局 / 几何 / 滚动 / 真实焦点 / canvas / PTY 的条目必须标 `browser`。

---

## 3. 十一条病灶的解法与机器检查

每条必须有检查。**没有检查的条目不算解决——它会在半年内长回来。**

| # | 病灶 | 解法 | 机器检查 |
|---|---|---|---|
| 1 | 卡片创建在路由里 | `systems/cards` 自治，只经 `public.ts` 暴露 | dependency-cruiser：**任何层只能经 `systems/cards/public.ts`**（app 与 features 均是合法消费方），禁止任何 `systems/cards/**` 深导入。<br>（只检查 `app/router.*` 会被"挪到 `app/cardNavigation.ts`"绕过） |
| 2 | 6 处模块级单例 | 构造函数注入，`app` 组装 | 自定义规则 `architecture/no-module-runtime-state`，见 §3.2 |
| 3 | context 跨域穿透 | context 只在 `app/**` 与 primitive 自有目录 | lint：基于 **import binding** 检测 `createContext` / `React.createContext` / alias（**不能文本匹配**）；白名单精确到文件，不是目录 |
| 4 | `calm:sync:cursor` 硬编码 3 处 | `core/keys` 单一工厂 | 两条规则叠加：①字面量 `/^calm[:.]/` 只许出现在 `core/keys`；②`architecture/no-direct-persistence` — 除 storage adapter 外禁止直接调 `localStorage`/`sessionStorage`/IndexedDB<br>（只查字面量会被拼接、模板串、常量转引绕过） |
| 5 | markdown pipeline 5 份、SchemaForm 2 份、calm-select 2 份、TOC 2 套 | 各收敛为单一模块 | **硬编码清单，不用 AST 相似度**（重命名／抽 helper／JSX 展开都会让阈值漂移，且相似表单会误报）。<br>规则：唯一 public entry + 禁止其他模块直接 import `react-markdown` / remark / rehype / **`mdast-util-*`**，并禁止手写 markdown 正则解析器。<br>（漏掉后两者的话，`report-outline.ts:1` 和 `file-viewer-markdown-toc.tsx` 这**两个现存重复实现都能逃过规则**）<br>AST 相似度只作非阻断报告。oracle 已给精确清单 `INV-DUP-001..010` |
| 6 | 6392 行全局 CSS | CSS Modules + `@layer` | ①组件只可 import `*.module.css`；②普通 `.css` 只许 `styles/entry.css` import；③第三方 CSS 只从 `styles/vendor.css` 汇入。<br>（原表述"styles 外禁 import `.css`"会把 CSS Modules 自己封死） |
| 7 | 高扇出共享类无归属（`.go` 8 文件 6 域等） | 提为组件**或**进显式全局层，二选一 | 从 CSS AST 生成 manifest 并**双向 set 相等**比对（单向存在断言有洞）；禁 CSS Module 内 `:global(...)`（除 allowlist）；禁 JSX 用 manifest 外的全局类字符串；禁运行时拼全局类名 |
| 8 | 运行时按 class 查 DOM | 一律 `data-*` | **解析静态 selector，任何 class selector 都报错**；覆盖 `querySelector(All)` / `closest` / `matches` / `getElementsByClassName`；**第三方 DOM 必须开显式口子**（`file-viewer.tsx:135` 查 `.cm-scroller` —— CM 的内部结构我们加不了 `data-*`），走 allowlist 且必须带应用容器前缀；动态 selector 一律报错，或只许经封装 locator 且**该 locator 参数类型必须是结构化 data 描述**（否则只是把字符串搬家）。测试代码排除在外。<br>（"以 `.` 开头"漏掉 `div .x`、`[role=x].y`、模板串、变量） |
| 9 | 根目录散件 | 全部归层 | 架构测试：`fe/web/src/` 顶层只允许 `main.tsx`，**同时覆盖 `.ts/.tsx/.js/.jsx`**（否则换扩展名即绕过） |
| 10 | `WaveContext` 死代码 | 不迁移 | oracle `migration: skipped` + 必填 `skip_reason` |
| 11 | lint 历史包袱 | 新仓全开，无 shim | `reportUnusedDisableDirectives:'error'` + CI 禁止子目录 eslint 配置 + 禁止本地 shim rule + `eslint-disable` 必带理由 |

### 3.2 `architecture/no-module-runtime-state` 的精确定义

原表述"禁止顶层 `let`/`Map`/`Set`/`new X()`"**既误报又漏报**：

- 误报：顶层 frozen lookup、schema、URL 常量
- 漏报：`const cache = {}`、`const store = createStore()`、以及**未导出的私有模块状态**（`events.ts:628-633` 的 `_shared` 就不是可变导出）

**判据不是"哪些 AST 节点"，是"模块求值后可达的对象图是否可变、是否承载运行态"。** 按这个判据展开：

| | |
|---|---|
| 禁 | 顶层 `let` / `var` |
| 禁 | 顶层构造 `Map`/`Set`/`WeakMap`/`WeakSet`/`EventTarget`/`WebSocket` |
| 禁 | **顶层可变对象字面量与数组**（`const cache = {}`、`const entries = []`） |
| 禁 | **class 的 static 可变成员**（`class C { static current = new Map() }`） |
| 禁 | **IIFE / 闭包 lazy singleton**（`const get = (() => { let c; return () => c ??= make() })()`） |
| 禁 | 顶层调用非白名单的工厂/构造器 |
| 允许 | primitive、函数声明、类型、schema、`Object.freeze` 的静态数据 |
| **必须豁免** | **type-only 的 `declare module` 声明合并** — `GATE-CARD-083/084` 证明它是类型穷尽的唯一机制，无法改注入（见 §8 裁决 6） |

**无法做到零误报**，需要小型 allowlist。写在这里免得实现 agent 以为规则能全自动。

### 3.3 把隐性契约变成机器检查

针对无锁定的 a11y 契约。**每条 oracle 条目由 `verification_owner` 指定由谁保证**，不能机械套模板：

```yaml
verification_owner: e2e | unit | lint | css | build | architecture | review-waiver | null
```

> 注：前一版此处引入过一个 `enforcement` 字段——那是重复概念，`verification_owner` 已承担该语义
> 且枚举更全（多 `css` / `build`）。**oracle schema 里没有 `enforcement`，不要去找它。**

| 契约类 | 检查方式 |
|---|---|
| 逐页 Tab 顺序 | 独立契约文件 `contracts/a11y/tab-order.yaml`，用 **role + accessible name + 条件**描述（不用 CSS selector）。Playwright 从 `body` 真实按 Tab 遍历。<br>★ **e2e 不得 import 生产代码旁的 TS 常量** —— 否则实现和预期会被一次修改"自洽地改错"。<br>★ 必须分别覆盖条件状态、Shift+Tab、Dialog 打开后的 trap（单一静态序列不够） |
| focus-visible | 禁 `outline: none\|0`，除非带注释且在 allowlist；允许移除 outline 的组件，要求同一 stylesheet 存在对应 `:focus-visible` 且提供非透明 outline/box-shadow；关键控件最终由 Playwright computed-style 兜底。<br>★ **不要用"相邻规则"做判定** —— CSS Modules／嵌套／layer／媒体查询会让"相邻"失去意义 |
| motion | **只对出现 `animation`/`transition`/`scroll-behavior` 的模块**要求声明对应 reduced-motion override；关键页面用 `reducedMotion:'reduce'` 跑 e2e。<br>★ 原表述三处都错：没动画的文件不该被迫加空块；`!important` 不是正确性的必要条件；全禁 `animationend` 会误杀合法的生命周期清理 |
| 215 条"故意不做" | **逐条评估，不套模板**：<br>· "左右键不处理" → 反向键盘测试<br>· "不污染 accessible name" → 正向精确断言<br>· "不卸载节点" → 断言 node identity/state 保留<br>· **effect 声明顺序** → 不能用"不发生"测，只能由行为回归锁定<br>· 纯产品范围决策 → 由 oracle migration gate 锁定，不写运行测试 |

> ⚠️ **恒真断言是最大陷阱。** jsdom 没有布局，"断言没有发生重排"**永远通过**。凡布局／几何／滚动／真实焦点／canvas／PTY 相关，`test_tier` 必须是 `browser`。

---

## 4. CSS 分层策略

Astryx spike 实测出的关键事实：**未分层的 CSS 永远赢过分层的，与特异性无关**。Astryx 全部样式在 `@layer astryx-base`，而 `calm.css` 零 `@layer`，导致 calm 的 `button{padding:0;...}` reset 摧毁 Astryx 组件（实测 padding 8px→0、背景消失）。

新架构从第一天显式声明层序：

```css
@layer reset, vendor, tokens, base, astryx, ui, features, overrides;
```

| 层 | 内容 |
|---|---|
| `reset` | 元素 reset |
| `vendor` | 第三方原始 CSS，**必须经 `@import ... layer(vendor)` 汇入**（xterm / react-grid-layout / react-resizable） |
| `tokens` | `:root` / `[data-theme]` 变量定义 |
| `base` | 排版基线、`.calm-prose` 一类文档流样式 |
| `astryx` | Astryx 预编译 CSS（若采用） |
| `ui` | primitive 样式 |
| `features` | 业务组件的 CSS Modules |
| `overrides` | 第三方覆盖、逃生舱（**manifest 到 package + selector + reason + expiry**，不是只列类名） |

好处：覆盖关系从"特异性军备竞赛"变成**显式声明**。

### 4.1 六个坑（两轮 review 逐条验证，其中两条是本文档原先的事实错误）

**① ~~自定义属性免疫 layer~~ —— 错的。**
自定义属性同样参加 cascade 和 layer 排序。`var()` 是在**使用点**解析，这与"声明免疫 layer"是两回事。token 桥接仍然可行，但不能靠"免疫"这个理由。

**② ~~声明 `@layer astryx` 就能控制 Astryx~~ —— 错的。**
Astryx 用的是它自己的 `@layer astryx-base`。外层声明一个叫 `astryx` 的层不会自动接管它。必须二选一：
```css
@import url('@astryxdesign/core/astryx.css') layer(astryx);   /* 包进目标层 */
/* 或把第三方真实层名写进总层序： */
@layer reset, vendor, tokens, base, astryx-base, ui, features, overrides;
```

**③ CodeMirror 6 完全绕过 layer 体系。**
CM6 用 style-mod 在**运行时注入未分层的 `<style>`**，全仓没有任何 CM 的 CSS import。一旦执行"所有 CSS 必须在 layer 内"，`calm.css` 里 **14 处 `.cm-*` 覆盖立即失效**（它们进了 layer，而 CM 注入的是未分层的，未分层永远赢）。

→ `.cm-*` 覆盖走**具名的 unlayered 例外文件**。但这个方案有两个必须处理的副作用：

1. **反向压制**：unlayered 的 `.cm-*` 会压过应用在 `ui`/`features`/`overrides` 层对同一属性的任何调整
2. **不自动获胜**：unlayered 覆盖和 CM 自己注入的 unlayered 样式**处于同一 cascade 层级**，最终仍由 specificity / source order / `!important` 决定；而 CM 的注入时机可能晚于应用样式

所以例外必须带四条约束：

| | |
|---|---|
| 粒度 | 精确到 **selector + property**，不是"整个文件豁免" |
| 前缀 | 必须有应用容器前缀，**禁止裸 `.cm-*`** |
| 排他 | 禁止其他 layered 文件再覆盖这些 property |
| 验证 | 真实浏览器 computed-style contract test，覆盖 light/dark、挂载、动态 theme reconfigure |

**优先级**：能用 CodeMirror 的 theme/extension API 表达的，一律走 API；CSS 例外只留外部容器无法表达的部分。

**④ `!important` 的 layer 优先级是反转的。**
普通声明后层赢；带 `!important` 时**早层赢**。`overrides` 层里现存的 `!important` 需要重新评估——进了 `overrides` 之后很多可以直接删掉。

**⑤ 未分层 CSS 仍压过一切分层声明——需要两种检查，不是一种。**

前一版说"CI 扫最终 bundle"**不够**：静态扫 bundle 发现不了运行后由 style-mod 动态生成的 CSS，也判断不了页面里的 `<style>` 节点和 inline style。这是两种检查：

| 检查 | 手段 | 抓什么 |
|---|---|---|
| **build audit** | 解析构建产物的静态 CSS | 源码和第三方 `@import` 里未分层的规则 |
| **runtime audit** | Playwright 遍历 `document.styleSheets` 与 `<style>` 节点，**外加 `[style]` 元素扫描** | style-mod 这类**运行时注入**的未分层样式。<br>★ `document.styleSheets` **不覆盖**元素的 `style=""` / React `style` 属性——那要单独遍历 `[style]`，否则这条检查会漏掉整类 inline 覆盖 |

只做前者会漏掉 CodeMirror 整类问题——而那恰恰是坑 ③ 的来源。

**⑥ CSS Modules 不会自动进 layer。**
每个 `.module.css` 仍需自己包 `@layer features` / `@layer ui`。

### 4.2 现有 168 行第三方覆盖的去处

| 现状 | 去处 |
|---|---|
| `.cm-*`（14 处） | **具名 unlayered 例外文件** —— 因为 CM6 注入的就是未分层的 |
| `.xterm*` | `overrides` 层，顺手删掉 `!important` |
| `.react-grid*` / `.react-resizable*` | `overrides` 层；且它们现在的**三行 JS `import`** 必须改成 `@import ... layer(vendor)`，否则整套层序静默失效 |

**检查**（三层，缺一不可）：
①**stylelint** —— `styles/` 与 unlayered 例外清单之外的 CSS 必须整体在 `@layer` 内；例外文件另加一条「最右复合选择器必须含 `.cm-`」把爆炸半径钉死；
②**build audit** —— 扫构建产物的静态 CSS，抓源码与第三方 `@import` 里的未分层规则；
③**runtime audit** —— Playwright 遍历 `document.styleSheets` + `<style>` + `[style]`，抓 style-mod 这类运行时注入与 inline 覆盖。
④unlayered 例外清单双向 set 相等。

> ⚠️ **一个上线即失效的遗漏**：现有 14 处 `.cm-*` 规则的祖先 hook（`.file-viewer-code-wrap` 等）是**应用全局类**，转 CSS Modules 后会哈希化，unlayered 例外文件将无法引用它们。这些祖先 hook 必须进全局类 manifest（`:global`）或改 `data-*`，且要在 CM 例外落地**之前**处理。

---

## 4.5 五层落到目录：`core` 跨端，其余每端一份

五层是**分层模型**，跨端是**打包边界**，两者正交。落到磁盘：

```
fe/
├── core/                    ★ 唯一跨端共享层。**平台无关**（不只是无 DOM）
│   ├── api/                 契约与错误规范化（transport 由端注入）
│   ├── domain/              wave · cove · report · block 模型与纯函数
│   ├── schemas/             zod 边界校验
│   ├── keys/                持久化 key 工厂 + storage port（治 calm:sync:cursor 三处硬编码）
│   ├── state/               Persistent<T> 条件类型 · codec · storage port（**无 React**）
│   ├── types/               跨层基础原语类型；token 统一归 `web/src/styles/tokens`
│   ├── markdown/            parse · normalize · sanitize-ast-policy · outline（**不含 JSX**）
│   └── events/              protocol · reducer · invalidation-plan（纯，见 §4.6）
├── mock/                    从 openapi.json 生成
├── web/                     桌面端
│   └── src/
│       ├── app/             router · providers · theme · DI · shell
│       ├── features/        wave · cove · today · report · settings · auth
│       ├── systems/         cards · terminal · wheel · fs-viewers · editor
│       ├── ui/              dialog · menu · focus · roving
│       └── styles/          @layer 全局层
```

> **mobile 端已 defer，不在本次范围。** 下方「（未来加端时）为什么只有 `core` 跨端」保留为将来的判据。

### `core` 与 `web` 分离的理由：可测试性

`core` 里没有 `WebSocket`/`localStorage`/`location`/`fetch` 的直接调用，平台能力一律经注入的 port——
因此 **`core` 能在 node 里直接跑测试**，不需要 jsdom、不需要假 WebSocket。这是当前分离的正当性来源。

### （未来加端时）为什么只有 `core` 跨端

- **`ui`** — 桌面 hover/右键/多栏 vs 移动 tap/手势/单栏，交互模型不同。共享必然长出 `isMobile` 分支
- **`systems`** — `cards`/`terminal`/`wheel` 是桌面特有；移动端"只读阅读"的定位下这层近乎为空
- **`features`** — 同一业务在两端的信息密度和操作面完全不同
- **`app`** — 组装关系天然各端一套

**`core` 的边界是本架构最关键的单点决策**：它是唯一的共享面，划宽了会把平台依赖漏进跨端层，划窄了 report 逻辑要写两遍。

### 4.6 `core` 的判据是 platform-independent，不是 DOM-free

> **跨端边界是"平台无关"，不是"没有 DOM"。** 这两个不是一回事——本文档前一版把它们混为一谈了。

反例（两轮 review 各自实读 `web/src/api/events.ts` 得到）：

```
WebSocket                        events.ts:172-175, 306-360
requestIdleCallback / setTimeout events.ts:160-170
localStorage                     events.ts:511-529, 585-596
location.protocol / host         events.ts:599-602
fetch(credentials:'include')     events.ts:551-578
模块级共享实例                     events.ts:604-634
loadCursor() 构造函数内同步返回      events.ts:204   ← 连"形状"都不跨端（RN 存储是异步的）
```

这些一行 DOM 也没有，但全是浏览器平台依赖。

### 4.7 事件流三分法

```
core/events/                    ★ 纯，可在 node 直接测，跨端
  protocol.ts                   WireEvent · control frame · version gate
  reducer.ts                    (frame, state) → EventEffect[]
  invalidation-plan.ts          纯 query-key 计划，**不 import QueryClient**

web/src/systems/events/         平台实现
  websocket-transport.ts
  browser-cursor-store.ts
  unauthorized-probe.ts
  event-stream.ts

web/src/app/events/             React / TanStack 胶水
  EventBridge.tsx                 ← dev-trace 的 DEV 短路必须**内联在这里**，见下
  query-invalidation-adapter.ts   ← 唯一接触 QueryClient 的地方
```

> ⚠️ **不要把 dev-trace 抽成独立模块。** `GATE-APP-079` 明令：
> `import.meta.env.DEV` 的短路必须**内联写在 EventBridge effect 的调用点**，
> 只有内联才能让 Vite/terser 把整个右侧（含 `ensureTraceBuffer`/`pushTraceEvent` 调用）折成死代码，
> 生产 bundle 才真的不含 buffer。源码注释要求任何重构前先 `grep __neigeEvents__ web/dist/assets/*.js` 复验。
> 前一版把它列为 `dev-trace.ts` 独立文件——**那恰好是这条 gate 禁止的重构**。

reducer 输出 effect，由端侧 adapter 执行：

```ts
type EventEffect =
  | { type: 'persist-cursor'; id: number }
  | { type: 'invalidate'; keys: QueryKey[] }
  | { type: 'clear-cache' }
  | { type: 'reconnect' }
```

### 4.7.1 排序不变量：用类型消除，不用运行时守卫

`INV-APP-019` 要求 `setSyncEventVersion → subscribe → start`（`eventBridge.test.tsx:165` 专锁），
`INV-APP-001` 要求 bridge 挂在 `ServerCompatGate` **内部**。三分之后这条顺序**跨越三个模块边界**。

> ❌ **前一版方案（"未经 `setSyncEventVersion` 就 `start()` 会抛"）不成立**，它只能保证
> `set < start`，下面这段仍会通过守卫：
> ```ts
> setSyncEventVersion(v); start(); subscribe(['*']);   // subscribe 晚于 start，守卫无感
> ```
> 且现有语义允许 `setSyncEventVersion(null)` 清空版本（`events.ts:278`），
> 单凭 `!== null` 无法区分"从未调用"与"显式设 null"。

**改用原子配置 + 类型分裂**——让非法顺序无法被写出来：

```ts
// systems/events —— 未配置的 stream 根本没有 start()
interface UnconfiguredEventStream {
  configure(opts: { syncEventVersion: number | null; topics: Topic[] }): ConfiguredEventStream
}
interface ConfiguredEventStream {
  start(): void
  stop(): void
}
```

`subscribe` 折进 `configure` 的 `topics`，三步坍缩成一步，**排序不变量消失而不是被守卫**。

> ⚠️ **这会改变两条现存契约，必须显式迁移而不是悄悄冲突**：
>
> **①** `events.test.ts:303` 锁定了"未 set 时允许 start，并接受任意 eventVersion"。新 API 下不再可能。
> 对应 oracle 条目标 `migration: skipped` + `skip_reason: 由 configure() 原子化取代，非法顺序在类型层不可表达`。
>
> **②** `events.ts:624-626` 的注释明写了一个**测试逃生口**：
> > "For tests that need a connected stream without the bridge in scope, construct `new EventStream(url)` directly and call `start()`"
>
> 类型分裂会封死它。**必须保留等价逃生口**——建议 `EventStream.forTest(url).configure({...}).start()`，
> 让测试仍能绕开 bridge，但仍走 configure（不给"跳过配置"的口子）。

### 4.7.2 类型消除不了的两条，仍需 contract test

| 不变量 | 为什么类型管不了 |
|---|---|
| `INV-APP-001` — EventBridge 必须挂在 `ServerCompatGate` **内部** | 这是组件树的位置约束，类型系统看不见 |
| `INV-APP-020` — bridge 是**唯一**的 `start()` 调用方 | 类型能保证"调 start 前已 configure"，保证不了"只有一处调" |

这两条由 app 层 contract test 锁定，不能指望 §4.7.1 的类型分裂覆盖。

### 4.8 `core` 不含 JSX

**决定：不允许 `core/render/`。** 两轮 review 有分歧，裁决理由如下。

原方案想用黑名单 lint（禁 `react-dom`/`useState`/`useEffect`/`on(Key|Pointer|Touch)*`/`createContext`）来允许"无状态 JSX"。实际的绕过路径：

- `onClick` / `onChange` / `onSubmit` / `onFocus` / `onWheel` 都不在正则里
- `useRef` / `useLayoutEffect` / `useInsertionEffect` / `useReducer` / `useSyncExternalStore` 未禁
- `document` / `window` / `location` / `navigator` / `matchMedia` / `ResizeObserver` 未禁
- hook 可 alias：`import { useEffect as afterRender }`
- 可以渲染一个内部有 state / portal / context 的**导入组件**
- class component、ref callback、`dangerouslySetInnerHTML` 未覆盖
- `createPortal` 可从 wrapper 间接导入，不出现 `react-dom` 字面量

而且 `gates-types.yaml:710-718` 已经记录了同类绕过在本仓真实发生过（computed member 完全绕过规则）。要封死这些，lint 会变成一个不完整的 React effect system。

**更重要的是：跨端共享 JSX 目前没有真实用例。** subagent 核对了 `SpecConversation.tsx:106-110`——四份 markdown 管线**行为已经发散**，而 `ReportLink` 天然端相关、只能注入。

所以：

```
core/markdown/                  parse · normalize · sanitize-ast-policy · outline · block schema
web/src/features/report/render/ React renderer（端侧）
```

**不要为一个不存在的用例放宽 `core` 的定义。** 将来实测两端 JSX 映射确实相同，再抽 `render-react` 包。

### 4.9 `core/markdown` 的边界与可配置 outline

实读两套 outline，它们**不是同一个函数的两次实现**，而是配置不同：

| | report | file-viewer |
|---|---|---|
| 解析 | mdast，`fromMarkdown()` 在 `report-outline.ts:72` | 手写解析，支持 setext / fence / inline（`file-viewer-markdown-toc.tsx:9`） |
| 层级 | H1–H2 | H1–H4 |
| ID | `<blockId>-h<n>`（`:56`），**按 block 内局部 ordinal** | `md-h-<n>`，**全文件全局 ordinal** |

所以 core 的抽象**不能**是固定的 `outline(markdown)`，必须参数化：

```ts
parse(md): NormalizedMarkdownAst
extractOutline(ast, { maxDepth, headingId, textPolicy }): HeadingOutline[]
```

> ⚠️ **但这三个参数不足以覆盖 report 的完整 `deriveOutline()`。** 实读 `report-outline.ts`：
> 它还要把**非 prose block 挂到最近 heading 下**，无 heading 时创建 `number: null` 的顶层项（`:66`），
> 并做连续编号与 `children` 组装。**这些不是 AST heading policy，是 report 的业务组合。**
>
> 正确切法：
> - `core/markdown.extractOutline()` — 只返回标准 heading outline
> - `features/report` 的**纯组合函数** — 把 heading outline 与非 prose blocks 拼成 `ReportOutlineItem[]`
>
> 参数的额外约束：`headingId` 必须能拿到 heading 节点、**全局/局部 ordinal** 和调用上下文；
> `textPolicy` 必须是**冻结的策略类型**而非无约束 callback（否则 core 的行为不可预测）。

**`sanitize` 的边界要写清**：core 里的是 **sanitize-**ast**-policy** —— 输出平台无关的安全中间 AST。
一旦 sanitize schema 依赖 React 允许的元素/属性、`ReportLink`、URL transform 或 `dangerouslySetInnerHTML`，
它就**属于 renderer adapter，不属于 core**。命名上刻意不叫 `sanitize`，避免实现者理解成"core 直接产出可安全插入 DOM 的 HTML"。

> ✅ **oracle 已同步（无需再改）**：`INV-DUP-004`（按 id 检索，勿用行号）原文要求四处 react-markdown 配置
> "视为一组收敛"，与本节"各端 renderer"冲突。**处理方式是保留原 `statement`/`why`，在 `why` 末尾追加架构裁决**——
> 收敛目标改为：**共享 parse / normalize / sanitize-ast-policy 为单一内核；端侧 renderer 显式配置行为差异
> （链接组件、heading id 注入、URL transform）。即：收敛内核，不收敛 renderer。**
>
> **不要去覆盖它的原 statement** —— 保留原文 + 追加裁决是刻意的，这样能看到契约的演变而不是只看到结论。
>
> **尚待阶段 1 接口冻结时处理**（不是实现阶段——它属于 `core/markdown` 冻结面）：
> `INV-DUP-005` 要求的"一个内核 + 两个 id 策略"需写死；`ReportBlock` 类型须先落 `core/domain`。

## 5. 技术栈

| | 决定 | 依据 |
|---|---|---|
| React 19 + Vite + TanStack Router/Query + zod | **沿用** | 栈不变则老代码可作参考，知识可迁移 |
| CSS Modules + `@layer` | **采用** | §3 病灶 6/7 |
| Astryx | **采用，锁死精确版本** | spike：零构建配置、14/14 组件齐、tree-shaking 有效。但 **5.5 周 12 版、67% 带 breaking 且无 codemod** → 升级必须当独立任务排期 |
| Astryx `<Theme>` 组件 | **不用** | 它往 `<html>` 写 `data-theme`，与自有主题机制撞车。只用它的组件，主题归 `app/theme` 管 |
| `astryx.css` 148.6 kB / 25.8 kB gzip | **接受** | 单体，与用量无关，`exports` 无组件级子路径，不可裁剪 |
| Astryx Tooltip | **禁用** | 触屏 tap 无反应（源码 `if (!target.matches(':focus-visible')) return`），只有键盘可达 |

---

## 6. 需在阶段 1 冻结的接口

按依赖顺序，越靠前越先冻结：

| 序 | 接口 | 消费方 |
|---|---|---|
| 1a | `core/state`：`Persistent<T>` **条件返回类型** · codec · storage port（**无 React**） | 所有层。硬闸是类型不是 lint，必须第一个落 |
| 1b | `ui/state`：React hook wrapper（`useState`/`useReducer` 的受控出口） | 所有组件。**放 `ui` 不放 `app`**——`ui` 是 UI 侧最底层，`systems`/`features`/`app` 都能合法向下依赖它；放 `app` 会迫使下层向上 import，破坏单向规则 |
| 2 | `core/keys`：持久化 key 工厂 + storage adapter port | api / events / session |
| 3 | `core/api`：契约 / schema / 错误规范化（transport 由端注入） | 所有 features |
| 4 | `styles/tokens`：token 定义 + **十类形状契约**（原写"六类"，实读 `calm-tokens.test.ts` 是十类，且单模标量内部还有 7 个形状各异的子族） | 所有样式 |
| 5a | `core/events`：protocol / reducer / invalidation-plan（**不 import QueryClient**） | systems · app |
| 5b | `systems/events`：`UnconfiguredEventStream` → `configure()` → `ConfiguredEventStream`（§4.7.1）。**冻结面必须含四项**：①handler 注册（`on`/`onConnectionState`）属于哪个 typestate、会不会漏第一帧；②`configure()` 本身**不得连接**（INV-APP-021）；③重复 `configure()` 的语义；④唯一 start ownership（INV-APP-020，typestate 管不了） | app |
| 5c | `core/markdown`：`NormalizedMarkdownAst` · `parse()` 的返回/error 通道 · `sanitize-ast-policy` · `TextPolicy` · `HeadingIdPolicy` · `HeadingOutline`，**外加 §9 七项语义决定**（方言 / rawHTML / 文本规则 / depth / ID 策略与稳定性 / 重复标题 / malformed）。<br>★ report 的 block-to-heading 归组与 fallback 顶层项**不在此面**，属 `features/report` 的纯组合函数 | features/report · systems/fs-viewers |
| 6 | `systems/cards/public.ts`：registry / lifecycle / host / resolver，**并重新导出完整接口类型**（见 §8 裁决 6） | app · features/wave · today |
| 7a | `ui/directory-browser`：注入 `listDir` port 的通用浏览器 | schema-form · NewTaskForm · Wave 快捷创建 |
| 7b | `ui/schema-form/fields/DirectoryField`：表单字段包装 | SchemaForm（由卡片 create schema 驱动） |
| 7c | `ui/dialog` · `ui/menu` · `ui/focus`：props 面 + child-view stack | features 多处 |
| 8 | `styles/`：`@layer` 层序 + 全局类 manifest + unlayered 例外 manifest（selector+property 粒度）+ `data-*` 约定 | 所有组件 |

**接口变更协议**：冻结后，agent 不得自行修改。发现不够用 → 提 change request 回接口层，由 orchestrator 裁决并广播。**宁可一个 agent 阻塞，不可两个 agent 各改一版。**

---

## 7. 文件所有权

并行的前提：**每个文件恰好一个 owner slice，没有共享可写文件。**

冻结后的接口文件（§6）在实现阶段**全部只读**。全局样式层（`styles/`）owner 是 S0，实现阶段不接受修改——需要新全局类必须走 change request。

### 7.1 两件不同的事，次序不能倒

| | 是什么 | 状态 | 何时产出 |
|---|---|---|---|
| **oracle slice 归一化** | 1127 条契约各归哪个模块 | ✅ **已完成** | 阶段 0 |
| **实现文件 ownership manifest** | 未来每个**文件**归哪个 agent 写 | ⏳ 未产出 | **阶段 1 的产出，不是它的前提** |

> 前一版把 manifest 当成冻结的前提——**次序错了**。manifest 要列出未来的文件路径，
> 而文件路径由接口冻结决定（`systems/events` 拆成几个文件、`ui/state` 放哪，都是冻结时才定）。
> 正确次序：**接口冻结 → 落出模块清单 → 生成 ownership manifest → 才可派实现 agent**。
> 所以阶段 1 的出口条件里包含 manifest，阶段 1 的入口条件里不包含。

### 7.2 归一化结果

> ⚠️ **归一化"已完成"不等于全部归属已定案。** `NORMALIZATION-REPORT.md` 里仍有 **13 条 / 6 项**
> 标为「需人工裁决」——它们有可用的临时归属（不阻塞阅读），但**层的选择存在实质分歧**。
> **这 13 条必须在阶段 1 内裁决完毕**，否则据此生成的 ownership manifest 会把分歧固化进文件划分。
> 这是阶段 1 的出口条件之一，不是实现阶段的事。

`docs/oracle/owner-aliases.yaml` + `NORMALIZATION-REPORT.md`：148 个 distinct `owner_slice` → **114 个规范值，1127/1127 归层**。

分布：features 407 · systems 271 · app 138 · ui 132 · styles 57 · core 26 · none 96。

> **`core` 只有 26 条**——跨端共享面比预想小得多。这从数据侧支持了 §4.8 收紧 `core` 的裁决：为 26 条里的一小部分去背整个 React effect system 的 lint 负担不划算。

> ⚠️ **200 条恒真断言风险**：`test_tier: browser` 共 585 条，其中 **200 条当前 `verification_owner: unit`**（systems 100 · features 54 · ui 38 · app 6 · core 1 · styles 1）。它们涉及布局／几何／滚动／真实焦点／canvas／PTY，留在 jsdom 里断言恒真。实现阶段单 slice 的验证成本要按此上调。

---

## 8. 已裁决

| # | 问题 | 裁决 | 依据 |
|---|---|---|---|
| 1 | `core` 能否含无状态 JSX | **不允许**。改 `core/markdown` + 各端 render | §4.8。黑名单 lint 有 8 类绕过路径且本仓有先例；且跨端共享 JSX **当前没有真实用例**（4 份管线行为已发散，`ReportLink` 只能注入） |
| 2 | `DirectoryPicker` 归属 | **两层，全在 `ui/`**：`ui/directory-browser`（注入 `listDir` port）+ `ui/schema-form/fields/DirectoryField`。**不设 `features/wave/create` 那层** | 实读 `SchemaForm.tsx:119-126`：字段的唯一消费方是 SchemaForm，由**卡片 create schema** 的 `type: 'directory' / 'file'` 驱动（codex cwd、file-viewer path），**与 wave create 无专属关系**。放进 `features/wave/create` 会让 `ui/schema-form` 向上 import features——反而制造新的层级破损。当前它直接调 API（`DirectoryPicker.tsx:240`），必须改注入 |
| 3 | `systems/editor` | **沿用 folder-level split，不建 npm workspace**；按内部端口拆 `public.ts` / `model/` / `adapters/` / `ui/` / `generated/` | 两轮 review 一致。`editor/README.md:12-21` 明说 scaffold only，oracle 仅 3 条，无证据达到 workspace 规模 |
| 4 | `events` 归属 | 三分：`core/events`（纯）+ `systems/events`（平台）+ `app/events`（React 胶水） | §4.6/4.7 |
| 5 | 重复实现检测 | **硬编码清单**，展开成具体 import/path/entry 约束（不能只引 oracle ID —— oracle ID 不是 lint 配置）。范围是 `INV-DUP-001..010` **全部**（实测到 010，前一版写 006 是截断）及后续追加项。AST 相似度只作非阻断报告 | 阈值会被重命名/抽 helper/JSX 展开漂移，相似表单会误报。只写 001..006 会把已知重复排除在闸门外 |
| 6 | `declare module` 类型注册 | **保留机制，但接口面必须可直读**：`systems/cards/public.ts` 重新导出合并后的完整接口类型；§3.2 的 `no-module-runtime-state` 豁免 type-only 声明 | `GATE-CARD-083/084` 证明它是类型穷尽的唯一机制，无法改注入。但单读 `registry.ts` 会看到"用了未声明字段"——用 `public.ts` 的再导出消除这个心智负担 |
| 7 | `ui` 可依赖的 core 类型白名单 | **只允许三类**：①branded ID 类型（`WaveId`/`CoveId`/`CardId` 等，无字段无方法）；②无障碍原语类型（role 枚举、focus 目标描述）；③基础设施类型（`Persistent<T>`、codec、storage port）。判据是类型不得携带业务 domain 字段；`core/domain` 始终禁止 | "`ui` 不得 import domain"过严，Dialog/Menu 确实需要标识与 a11y 类型。但白名单必须封闭可枚举，否则会变成 domain 的后门 |

**升级 `systems/editor` 为 workspace 的触发条件**（写死，免得凭感觉）：出现第二个 JS 消费端 / 需要独立版本发布 / 依赖树或构建耗时需要隔离。

## 9. 阶段 1 内必须冻结的语义



`core/markdown` 的语义细节**直接决定 `NormalizedMarkdownAst` 的类型与 `textPolicy` 的取值域**，
而 §6 又要求阶段 1 冻结这两个接口——所以它们不可能留到冻结之后。必须一并定死：

| 项 | 为什么它决定类型 |
|---|---|
| CommonMark / GFM 方言 | 决定 AST 节点种类 |
| raw HTML 是否保留 | 决定 AST 是否含 `html` 节点，直接影响 sanitize-ast-policy |
| setext / fence / 图片 alt / inline code 的文本规则 | 决定 `textPolicy` 的枚举值 |
| heading depth 范围 | `maxDepth` 的合法域 |
| ID 策略与跨版本稳定性 | `headingId` 的签名（要不要 ordinal、局部还是全局） |
| 重复标题处理 | ID 冲突消解，影响返回类型 |
| malformed markdown 行为 | 抛错还是降级——决定返回类型是否带 error 通道 |
