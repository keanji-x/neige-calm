# `_fe-rewrite-design.md` — 独立 review（subagent 通道）

范围：只读审阅。所有"读了代码得出"的结论都带文件:行号；标注【推测】的是经验判断。

## 总体判断

**不能直接拿去驱动并行 agent。** 方向（不换栈、局部性 > DRY、mock 从 openapi 生成、S1 先行重估）是对的，但有四个硬缺口，每一个都会让 N 个 agent 各自发挥：

1. **§0.3 不变量表覆盖率 < 20%**。最致命的是：`docs/a11y-contract.md`（326 行、11 节，含 Tab 顺序、rename 契约、focus-visible 政策、motion 政策）**全文档零引用**。这份文件是唯一记录"WavePage Tab 顺序应该是什么"的地方，重写后没人能自证对齐。
2. **§5.3 判据仍是开放问题**。文档自己承认"没有客观判据，N 个 agent 有 N 种理解"，然后把判据留给 review 决定——所以现在这份文档发出去，就是那个 N 种理解的状态。
3. **§6 的 "S1 后 S2–S10 可并行" 与代码不符**。至少 S8↔S6、S8↔S3、S8↔路由、S10↔S7、S10↔S5、S1↔S6 是真耦合（见 §5）。
4. **§2 点名的 lint 机制在本仓不存在**。`no-restricted-imports` 没有 "zones" 概念（zones 是 `eslint-plugin-import` 的 `import/no-restricted-paths`），而 `web/package.json` 未装 `eslint-plugin-import`。

另外三处目录契约漏洞：`fe/web/src` 的目录树没有给 `shared/`（26 个实现文件、`shared/state` 有 33 个 importer）、`input/`（滚轮路由子系统，5 文件）、`hooks/useOverlayState`（`Persistent<T>` 的另一半）、`editor/`（#330 AI-first 编辑器脚手架）、`wave-fs-viewers/`（16 文件注册表）安排位置。这五块合计约 60 个实现文件，没有归属 = 每个 agent 自己造一个家。

---

## 1. §0.3 不变量表的遗漏（逐条，带出处）

表里现有 7 条都对，但漏掉的至少有下面这些。按"丢了会疼"排序。

### A. `docs/a11y-contract.md` — 整份文件未被引用（最大遗漏）
- **§3.1 逐页 Tab 顺序**：Sidebar→main 的确切顺序、CovePage、WavePage 的 stop 序列。无 lint、无单测，只有这份文档 + `e2e/a11y-keyboard.spec.ts`。
- **§3.4 WaveList 键盘契约**：`Alt+ArrowUp/Down` 交换 `sort`（乐观）、`Delete/Backspace` 删卡、每个 `<li>` 带 `aria-keyshortcuts`；**grid 视图故意不做键盘重排**（"mouse-only by design"）。典型"故意不做"型不变量。
- **§5 rename 契约**：accessible name = 值，动词走 `aria-describedby` + `.sr-only`，且 sr-only span 必须放在 `<h1>` **外面**（否则污染 heading 名，screen reader 会念 "Rename cove name: Atlas, heading level 1"）；Enter/F2 进编辑，Enter commit / Esc cancel / **blur 也 commit**；退出后用 `restoreFocus` boolean ref 等 input 卸载后再 focus。
- **§6 focus-visible 政策**：禁止裸 `outline: none`（必须配 `:focus-visible`，绝不用 `:focus`）；**禁止加全局 `*:focus { outline: none }`**；soft ring = `2px box-shadow var(--accent-soft)`，hard = outline + offset `var(--accent)`。CSS Modules 下这条最容易悄悄破。
- **§7 motion 政策**：`prefers-reduced-motion: reduce` 全局折叠到 0.01ms（`!important` 必需，要赢过内联 `animation:`）；loading 用文字不用 spinner；JS **不得**监听 `animationend`/`transitionend`。
- **§2.x** 每类对象（cove/wave/card/terminal/codex/iframe/overlay）的 role+name 语义。

### B. Dialog 源码里的排序型踩坑（contract test 只锁了一部分）
- **inert effect 必须声明在 focus-restore effect 之前**（`Dialog.tsx:183-193` 注释 + `Dialog.test.tsx:230`）：React 按声明顺序跑 cleanup，顺序反了 restore 目标还在 inert 毯子下，`focus()` 静默失败。纯声明顺序不变量，重写 100% 会丢。
- 初始 focus 走 `requestAnimationFrame` 延后一帧（`Dialog.tsx:245`）。
- inert 恢复要**精确复原**先前的 `inert`/`aria-hidden` 值（`prior` 数组，`:209-223`），不是无脑 remove。
- restore 前 `document.contains(target)` 守卫（`:277`）。
- 每次 Tab **重新查询** focusables，不缓存（`:302-308`），否则 pushView 后陷阱失效。
- `[tabindex="-1"]` 故意排除出陷阱循环但保留为兜底 focus 目标；`isFocusable` **故意不按可见性过滤**（jsdom 无布局，过滤会 false-negative 全部）（`:100-130`）。
- child-view stack：push 时外层 children 保持挂载 `display:none` 以保住半填表单；**child view 打开时禁用 overlay 点击关闭**（`:347`）；Esc 先给 `view.onEscape`。
- `hideTitleRow`：视觉头隐藏但 `title` 仍供 `aria-label`——dialog 永不无名（#891，`:46-55`）。
- body scroll lock 并精确还原 `prevOverflow`（`:174-180`）。
- **故意不用原生 `<dialog>`**（UA 默认跨主题不可控）。

### C. ConfirmDialog
- `confirmDisabled`：异步 confirm 期间禁 Confirm、**保持 Cancel 可用**（contract test G）；两种模式 Pattern A stay-open-while-pending / Pattern B close-then-await（`ui/README.md:192-215`）。
- Esc / overlay / × 三条 dismiss 全部路由到同一个 `onCancel`（调用点只有一个回调）。
- 破坏性操作**一律** ConfirmDialog，禁 `window.confirm` 与内联确认。

### D. `useRovingTabindex`
- **ArrowLeft/ArrowRight 故意不处理**（可编辑后代要用光标左右键）（`useRovingTabindex.ts:32-36`）。
- typeahead：500ms idle 重置；单字母缓冲**跳过**当前项（同首字母循环），多字母缓冲**包含**当前项（`:119-135`）。
- Space 在缓冲非空时进缓冲而非激活（`:292-303`）。
- itemCount 缩小时 clamp，变大时不跳（`:180-185`）。
- ref 回调里 `queueMicrotask` 补焦点（菜单首次打开时 ref 晚于 effect）（`:328`）；`setActiveIndex` 设为同值也要强制 focus（React bail-out）（`:199-205`）。

### E. `ui/README.md` 里的层级规则（文档说要"升格为模板"，但规则本身没搬进表）
- 一个 primitive 一个 role；primitive **不得**有业务逻辑（不得 import `api/` / `cards/`）；**禁止 re-export shim**（提取时直接改全部调用方）。
- Menu 的 class 全部由调用方传入（`wrapClassName`/`menuClassName`/`itemClassName`/`emptyClassName`），primitive 自己不硬编码，roving 焦点项追加 `is-active`。**CSS Modules 下这个 API 形态会崩**（hash 后的类名不能跨包传字符串）——文档没处理这个冲突。

### F. theme / 跨语言主题
- **只有 ThemeProvider 能写 `document.documentElement.dataset.theme`**（`theme.tsx:157-162`，#22 验收条 4）。文档只写了"首帧不闪烁"。
- **这条有一个刻意的例外**：`app/router.tsx:370-397` 和 `hooks/useTodayTerminal.ts:130` 在点击时**直接读** `dataset.theme` 而不用 `useTheme()`——订阅会重渲 wave 子树、触发 Router `<Match>` Suspense、**remount XtermView 并抹掉 `pendingThemeRef`**（#177）。例外和主规则一样重要。
- `?testMounts=1` 才挂 `window.__calmSetTheme`（`theme.tsx:186-195`）：e2e 靠它在不导航的前提下切主题，导航会 unmount XtermView 毁掉观测。
- `api/themeRgb.ts`：`LIGHT/DARK_THEME_RGB` 与 `XtermView` 的 `LIGHT_THEME/DARK_THEME`、Rust `RequestTheme::default_dark()`（`crates/calm-server/src/routes/theme.rs`）**三处必须同步**，经 OSC 10/11 交给 daemon。跨语言不变量。
- localStorage key `calm.theme`，读写全 try/catch（隐私模式会抛）。

### G. providers / session / 事件流（一条都没进表）
- **EventBridge 必须挂在 `ServerCompatGate` 内部**，兼容判定落定前不得开 WS（#198 concern 1，`providers.tsx:113-122`）；事件流单例在 bridge 显式 `start()` 前**惰性**（#215）。
- `setSyncEventVersion` → `subscribe` → `start` **顺序**（#198 concern 2，`eventBridge.test.tsx:165` 专门锁）。
- 401 **不重试**（`retryUnless401`，#189）；401 → `SessionProvider` 渲染 LoginPage 且 **router 绝不挂载**（route loader 不得与 auth gate 竞态）；tri-state 的 `unknown` 期渲染 `null`（不闪 LoginPage）；非 401 传输错误**不**跳登录页。
- `dbInstanceId` 变化 → 清 qc + IDB + WS cursor 并硬刷新，**静默无确认框**（`providers.tsx:177-203`）。
- `QueryRestoreGate`：`isRestoring` 时渲染 `null`（避免空缓存闪烁）。
- 所有 localStorage/indexedDB 访问包 try/catch，降级不崩（`providers.tsx:239-283`）。
- **`invalidationPolicies` 的类型级穷尽**：`definePolicies<{ [K in EventKind]: InvalidationPolicy<K> }>`（`invalidationPolicies.ts:21`）——每个 wire 事件必须有显式策略，不需失效的要写 `noop('reason')`。新增事件漏配 = 编译失败。这是全仓最强的"新增事件"闸门，文档完全没提。
- `_replay_complete` → 防御性 `invalidateQueries()`；`_snapshot_required` → `queryClient.clear()`。

### H. 类型/契约闸门
- **zod ↔ ts-rs 一致性测试**（`api/schemas.test.ts:211-226`）：`wireEventSchema` 推断出的类型必须等于生成的 Event union。文档 §4.2 只管了 mock↔openapi 的 drift，漏了这条。
- `TS_RS_LARGE_INT = "number"`（`.cargo/config.toml:26`）：Rust i64 → TS `number` 而非 `bigint`，与 zod `z.number()`、OpenAPI 三方一致。
- 未知 `ev` 必须 safeParse 拒绝；未映射事件 dispatch 不抛。

### I. `calm-tokens.test.ts` — "light/dark 双向对等"严重简化了
实际是**六类形状契约**（1193 行）：positional（`:root` + dark 双份 oklch）、concrete surface、**alias（只在 `:root`，禁止 dark 覆盖）**、单模标量（type/leading/tracking/radius/spacing/motion/z-index：禁止 dark 覆盖）、status color（**dark 侧统一 L=74%**，`:682`）、font alias（必须是 `var(--font-{sans,serif,mono})` 引用）。外加：z-index 六级严格递增 `base<raised<sticky<overlay<modal<toast`（`:833`）；`--overlay-scrim` 故意是 `rgba()` 唯一例外（`:769`）；`--font-mono` 与 `font-stack.ts` 的 `MONO_STACK` **逐字节相同**（`:1161`）；`--r` 是 `--radius-xl` 的 back-compat alias；孤儿 token 软检测（`:1168`）。
**且 token 清单是手写钉死的数组**——加 token 必须同 commit 改测试，这是刻意的"可 review 的 diff"设计。文档 §1.1 说 token 单一源改成 TS，等于要重新设计这整套闸门，没写怎么设计。

### J. `eslint.config.js` 注释里的"为什么"
- card-head 守卫的**豁免清单**：`card-head-observing-pill` / `card-head-icon--letter` / `card-head-icon--c{0..7}` / `card-drag-handle` / `codex-card-head` 是合法的，边界锚定 regex 故意放过（`:77-93`）。
- `Persistent<T>` 的**硬闸门是 `shared/state.ts` 的条件返回类型**（塌成 `never`），eslint 规则只是人类可读层，且无 type checker 时降级为纯文本匹配。文档 §2 只写了 eslint 规则，漏了条件类型这一层——只搬规则不搬类型 = 闸门失效。
- `no-raw-primitive-role` 只匹配字符串字面量，`role={expr}` **故意不查**（已知缺口）。
- `reportUnusedDisableDirectives: 'off'` + 一批 shim 成 `'off'` 的规则是**历史包袱**；新仓库应该反过来全开——文档没写新基线该收紧到什么程度。

### K. e2e 侧
`e2e/color-system-anchor.spec.ts`：表单控件 computed `color-scheme`/`backgroundColor`/`caretColor` 双主题锚定，含 `transparentAllowlist`（`wave-title-input`/`cove-title-input`/`cove-nav-edit-input` 故意透明）。这类正是新 CSS 架构最容易砸掉的。

---

## 2. §1.1 `common` 边界

**方向对，"零 `.tsx`" 这条线画错了地方。**

对的部分（直接说对，不用改）：交互模型确实不同，共享 Dialog/Menu/手势层必然长 `isMobile`；"按规范共享（同 token、同命名法、同契约模板）"是正确的默认。

反例（读代码得出）：**report 的内容渲染层**是真正值得共享的 UI 代码，而且恰好是最贵的一块。
- `§0.1` 说 `fe/mobile` 是"只读/轻交互"——那么移动端的主要价值就是 report 阅读，与桌面 report 的重合度接近 100%。
- 这块的成本：`report` 是 CSS 最大域（859 行），且 pipeline 重——`react-markdown` + `remark-gfm` 在仓里已经有 **4 份独立拷贝**（`WaveReportPage.tsx:381`、`report-blocks/index.tsx:116`、`report-blocks/task.tsx:24,31`、`file-viewer-markdown.tsx:376`、还有 `SpecConversation.tsx:107`），`mdast-util-from-markdown` 的 TOC 有两套发散实现（`pages/report-outline.ts` vs `file-viewer-markdown-toc.tsx:9-11` 手写正则）。"零共享"会把这个已经存在的重复病复制到第二个端。
- `cards/builtins/wave-report.tsx`（zod schema + 类型，8 个 importer，只依赖 `zod` + registry 类型）是**已经干净可提取的共享契约模块**——它应该进 `common`，文档的"不进 common: 任何 .tsx"会把它挡在门外（它是 `.tsx` 但里面是 schema）。

**建议**：把边界从"文件后缀"改成"是否含交互模型"，并用可 lint 的判据表达：
- 允许 `common/render/`：无状态、无焦点管理、无 portal、无键盘 handler，props-in → JSX-out；样式由调用方经 className prop / CSS 变量注入。
- `common/**` 禁止 import `react-dom`、禁止 `useState`/`useEffect`/`useRef`、禁止 `on(Key|Pointer|Touch)*` 属性、禁止 `createContext`。这四条都是纯 AST 规则，比"禁止 .tsx"更贴近真实分界，也更难绕。

---

## 3. §2 依赖方向的 lint 可行性（逐条）

**机制层的事实错误**：`no-restricted-imports` 没有 zones；zones 是 `eslint-plugin-import` 的 `import/no-restricted-paths`（`from`/`target`/`except`），而 `web/package.json` 里**没有** `eslint-plugin-import`。现实选项：装 `eslint-plugin-import` 用 `no-restricted-paths`（会解析到磁盘路径），或 `eslint-plugin-boundaries`（更贴层次模型），或 flat-config 每层一个 `files:` override + `no-restricted-imports.patterns`（能做，但只匹配 import 说明符**字符串**）。文档必须点名一种，否则 4 个 agent 会配出 4 套。

| 规则 | 判断 |
|---|---|
| `common` ✗→ web/mobile/ui | 可做（`no-restricted-paths` 或 monorepo 包边界，靠 workspace 天然隔离更稳） |
| `ui/**` ✗→ 业务层 | 可做 |
| 业务层横向禁止 | **只覆盖 import 图，覆盖不了真实耦合**（见下） |
| 禁 barrel | 可做，自定义规则单文件 AST（"只含带 source 的 export"）即可 |
| 禁 import `.css` | 可做；注意 pattern 要放行 `*.module.css`，否则 CSS Modules 自己被封 |
| 移植三条自定义规则 | 可做；但 `no-persistent-in-usestate` **必须连 `shared/state.ts` 的条件返回类型一起搬**，否则只剩软提示 |
| stylelint 字面量闸门 | 可做，`.module.css` 仍是纯 CSS；但见下面 `composes` 的洞 |

**绕过路径（按现实概率排序）**：
1. **相对路径 vs alias**：`patterns` 匹配字符串，`../../common/domain/x` 与 `@/common/domain/x` 是两个字符串。必须两种都写，或改用会解析路径的 `no-restricted-paths`。这是最现实的绕过。
2. **re-export 链**：`no-restricted-imports` 只看 `ImportDeclaration`，**不看 `export ... from` / `export * from` 的 source**。`ui/x.ts` 里写 `export * from '../wave/y'` 直接穿墙。禁 barrel 能缓解不能封死；`import/no-restricted-paths` 会查。
3. **type-only import**：base 规则会报 `import type`，很多层间只需类型，agent 被误报就会加 `eslint-disable`。应显式用 `@typescript-eslint/no-restricted-imports` 的 `allowTypeImports`，并**逐条规定**哪条允许类型穿透。文档没定 = N 种做法。
4. **动态 `import()`**：字面量能查，`import(variable)` 查不到（S1 的 `candles`/`CodePane` 都是 lazy，这条会被用到）。
5. **React context / 模块级单例**：`ui` 组件 `useContext(业务 Context)` 在 import 图上完全干净。这是依赖方向 lint 的**根本盲区**，而本仓恰恰重度用它（`ThemeContext` 被 S1/S4/S7/S8/S9 五个域消费；`CardInstanceReactCtx` 定义在 `cards/registry.ts:391` 被 S8/S9 消费；`ModalViewContext` 定义在 `ui/Dialog` 被 S10 的 `DirectoryPicker`/`NewTaskForm` 消费——**这条正好违反"ui 不得被业务层反向依赖契约"的精神**）。建议补一条可 lint 的：`createContext` 只允许出现在 `app/**` 与 primitive 自有目录。
6. **CSS 侧横向依赖**：CSS Modules 的 `composes: x from '../other/y.module.css'` 是 CSS 里的 import，eslint 完全看不见。要 stylelint 侧另配（`selector`/`at-rule` 层面的 restricted-syntax）。文档 §2 只封了 JS 侧的 `.css` import。
7. **"塞进 common 再取"**：把业务逻辑放进 `common/domain` 再从 `ui` 引用，层次规则不管语义。无解，靠 review。

---

## 4. §5.3 对齐判据

**推荐：结构 diff 作为阻塞门，像素 diff 作为非阻塞报告。** 理由都是从代码得出的：

1. **仓里已有结构 diff 的既成范式**：`e2e/color-system-anchor.spec.ts` 就是读 computed style 的具体属性（`backgroundColor`/`color`/`caretColor`/`colorScheme`）+ rect，双主题，写基线报告，用 `expect.soft`。脚手架应该**扩展它**而不是新开像素通道。
2. **像素 diff 在本项目必然长期红**：report 页里有 `lightweight-charts`（canvas）、`XtermView`（xterm canvas/webgl）、plugin iframe（`@modelcontextprotocol/ext-apps` 宿主）、CodeMirror（自带主题）。这四处像素 diff 对 DPR、字体 hinting、渲染后端敏感。agent 只能靠调阈值过门，阈值一松整个门就失效——**一个会被绕过的门比没有门更糟**。

**具体阈值策略**（写进 §5.3 当规格）：
- **Tier 0 硬归零 — token 解析值**：每个受测元素读 computed 的 `color` / `background-color` / `border-color` / `font-family` / `font-size` / `font-weight` / `line-height` / `letter-spacing` / `border-radius` / `z-index`，×light/dark。任何差异失败。
- **Tier 1 硬归零 — 可访问性结构**：Playwright `ariaSnapshot()` 对老/新逐路由比对，role + accessible name + 层级完全一致。这是 agent 自证"没改语义"的最便宜信号，且能直接对老 web 跑（`getByRole` 已是全仓约定，`a11y-contract.md §8.1`）。同时跑 axe（已有 `a11y-axe.spec.ts`）。
- **Tier 2 阈值 — 几何**：具名元素 boundingBox 相对其容器的 x/y/w/h。容器级（主区、卡片外框、rail）容差 **0**；块级元素 **≤2px 绝对或 ≤1% 相对（取大）**；纯文本节点放宽到 **4px**（换行/hinting）。违规数必须为 0 才算达标。
- **Tier 3 非阻塞 — 全页像素**：出 HTML 报告给人看，不进 gate；canvas / iframe / xterm 区域用 mask 排除。

**基线确定性（§5.2 缺、必须补，否则基线会自己变红）**：固定 `deviceScaleFactor`；固定字体（复用 `font-stack.ts` / `MONO_STACK`，CI 里装同一套或全部走 mask）；**强制 `prefers-reduced-motion: reduce`**（全局 0.01ms 折叠已存在，正好当确定性开关）；**冻结 clock**——`shared/relativeTime.ts` 有 6 个 importer，replay 数据固定但"3 分钟前"会随墙钟漂。

---

## 5. §6 切片并行安全性

结论：**S1 收敛后 S2–S10 可并行是不成立的。** 下面是读代码找到的隐藏耦合（含子 agent 的定位）。

**必须提前的先行切片（S0.5，S1 之前或并行）**：
- **S0.5-a `shared/state` + `Persistent<T>`**：33 个 importer、eslint 强制、且硬闸是条件返回类型。所有切片的第一行 `useState` 都依赖它。
- **S0.5-b token 层 + 全局 styles/**：`calm.css` 6.4k 行是单一全局表，`:root` 里 176 个自定义属性。token 没落地前任何组件的 `.module.css` 都写不出来。
- **S0.5-c 卡片契约模块**：`cards/builtins/wave-report.tsx`（8 个 importer、只依赖 zod + registry 类型）与 registry 的类型面，S1 和 S8 同时需要。

**不能并行的组合**：
| 组合 | 证据 |
|---|---|
| **S8 ↔ S6** | `renderCard`/`sizeFor`/`getEntry` 经 `WaveCard.tsx:1` → `WaveGrid.tsx:16`/`WaveList.tsx:48`；`react-grid-layout` 的 vendor CSS（`WaveGrid.tsx:13-14`）+ app 覆盖（`calm.css:6510-6592`, `:3778 .card-drag-handle`）跨 S6/S8。拆开写必然打架。 |
| **S8 ↔ S3** | `CalmApp.tsx:44` 挂 `useWheelRouter`，`input/wheelRouter.ts:3-5` 依赖 `cards/lifecycle`、`cards/registry.getEntry`、`cards/resolver`。**shell 不能脱离卡片注册表重写。** |
| **S8 ↔ 路由（S0）** | 卡片创建逻辑住在路由里：`app/router.tsx:415 addCardWithValues` / `:481 createFromEntry` / `:531 addCardOfKind` / `:436-452` 的契约错误与 `assertRouterCreateAllowed`。S8 的一半在 S0 的文件里。 |
| **S10 ↔ S7** | `Settings.tsx` **手写重复**了 `.schema-form*` 的整套标记（`:111-249` 十余处），与 `SchemaForm.tsx` 同源不同码。 |
| **S10 ↔ S5** | `.calm-select*` 与 `.new-task-form-*` 在 `NewTaskForm.tsx` 和 `Cove.tsx:508-536` 两边各写一份。 |
| **S1 ↔ S6** | `WaveReportPage.tsx:35` 直接 import `SpecConversation`；`.calm-prose`/`.report-prose` 跨 S1/S6/S9。 |
| **S2 ↔ S10** | `ModalViewContext` 定义在 `ui/Dialog/Dialog.tsx:89`，被 `DirectoryPicker.tsx:32,50` 和 `NewTaskForm.tsx:79,226` 消费——primitive 的 API 面由业务侧需求决定，S2 不能先冻。 |

**跨所有切片的全局耦合（必须在 S0 就定死，否则每个 agent 各定一次）**：
- **高扇出共享 class**：`.go`（8 文件 / 6 域）、`.synth`、`.col`、`.calm-prose`、`.card-drag-handle`（7 文件）、`.status-pill`/`.pill`、`.sr-only`。CSS Modules 下这些要么进 `styles/` 全局层，要么变成 common 组件——文档 §3.1 的"宁可重复"对 `.go` 这种是错的（8 处按钮各写一份 = 视觉必然发散）。
- **按 class 查 DOM 的运行时代码**：`input/wheelRouter.ts:14-17` 硬编码 `.modal-overlay, .modal-panel` / `[data-wheel-card]` / `.xterm-view`；`Dialog.tsx:200-207` 靠 DOM 结构上溯 portal root。**CSS Modules 会 hash 掉这些类名**，这类代码必须先迁到 `data-*` 属性。文档完全没提这个迁移。
- **模块级单例**：`cards/registry.ts:210-219` 三张 Map + warned Set、`cards/resolver.ts:22`、`cards/builtins/index.ts:23` 的 boot-once 守卫、`wave-fs-viewers/registry.ts:16`、`api/events.ts:628`、`api/onUnauthorized.ts:26`。
- **localStorage key 命名空间**：`calm:sync:cursor`（在 `api/events.ts:152`、`providers.tsx:82`、`SessionProvider.tsx:65` **硬编码三次**）、`calm:db_instance_id`、`calm.theme`、`calm:sidebar:*`、`calm:report-rail:*` 等。重写要么统一到 common 的 key 工厂，要么复制三份的坑再来一次。
- 顺带：`WaveContext`（`shared/components/WaveContext.ts:36`）被 `Wave.tsx` provide 但**全仓无消费者**——重写时别照抄。

---

## 6. 整体方案风险

**最大失败模式：冻结期无限延长 → 两个前端并行演进 → 新 fe 永远追不上移动中的 oracle。** 26.6k 行实现 + 37.4k 行测试，S1 之前无人知道真实工期（文档自己承认）。文档把这个列为开放问题 6，但**没有任何缓解机制**。建议写死：
- 冻结期上限（S1 实测 × 剩余切片超过 N 周就砍范围），到点强制回到决策。
- 冻结期内老 web 只接受两类改动：P0 生产 bug、后端契约兼容性。每条改动必须**同 PR 在 fe backlog 落一条 issue**，否则重写完就丢。
- oracle 基线必须可重打，且 diff 报告要能区分"基线变了"和"新 fe 退化了"——否则老 web 一改，所有 agent 的达标清单同时变红且无法归因。

**第二失败模式：S1 之后没有"停"的分支。** §8 步骤 4 说回来重估——这条对，直接说对。但缺预设的 fallback：应明确"若 S1 实测 > X，方案降级为**在 `web/` 内原地逐目录迁 CSS Modules**，不建 `fe/`"。这个 fallback 的存在本身是对整个方案最重的挑战：§0.4 已经决定不换栈，那么重写的全部增量就是**目录布局 + CSS 架构**两样——而这两样都能原地增量做（`editor/` 已经是"folder-level split，不建 workspace"的先例，见 `src/editor/README.md`）。文档应该正面回答"为什么不原地迁"，而不是默认全量重写。

**切换策略（§7 问题 5，答案可以给死，不必留作开放问题）**：
- ✅ **整应用双挂载 + 入口级切换可行且几乎零成本**：`router.tsx:158-162` 的 `basepath: '/calm'` 镜像 Vite `base`，把新 fe 挂到 `/calm2` 由 calm-server/nginx 静态分流即可。这直接把"冻结期"变成"两个都活着"，缓解了最大失败模式。
- ❌ **路由级混合（`/wave/*` 走新、其余走老）不可行**：两套 bundle 各持一个模块级 `queryClient`（`providers.tsx:84`）、各开一条 WS、共用同一个 `IDB_DB_NAME` 与同一个 `calm:sync:cursor`；跨 bundle 导航会互相清缓存、`dbInstanceId` 检测互相触发硬刷新（`providers.tsx:177-203`）。这条应该在文档里显式否掉，否则会有 agent 去试。

**第三：mock 的事件面缺口。** §4.2 "从 openapi.json 生成"是对的，但**WS 事件不在 openapi 里**——`generated-events.ts` 来自 ts-rs。建议写死：mock 只负责 REST，事件序列一律走已有的 replay server trace fixture（`e2e/_setup/` + `a11y` project），禁止手写事件。

**第四：§1.2 "同一条 `gen:api` 同时产出两处"实际做不到。** ts-rs 的目标路径**硬编码在 Rust 源码属性里**：`crates/calm-types/src/*.rs` 多处 `#[ts(export, export_to = "web/src/api/generated-events.ts")]`，而 `TS_RS_EXPORT_DIR` 是 `.cargo/config.toml:17` 的全局单值。要么改 Rust 属性（污染冻结期的后端），要么 `gen:api` 后加一步 copy + 一条 CI drift 检查。**文档必须选一种写死**，否则两个 agent 会各选一种，然后 drift。
