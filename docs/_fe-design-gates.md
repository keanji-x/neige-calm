# 前端设计系统的机器可执行化（enforcement design）

**Scope**：只设计**执行机制**与**规则形状**，不产出最终规则清单（规则本体由 `docs/_fe-design-system.md` 并行给出）。
**Repo**：`fe/`（worktree `.claude/worktrees/997-c1-today/fe`）。
**原则**：复用既有四层闸门基础设施（stylelint 插件 / node CSS-AST 审计 / ESLint 自定义插件 / vitest browser tier + mutation runner），**不另起一套并行系统**。

---

## 0. 既有基础设施盘点（复用点，逐条给路径）

| 能力 | 现有实现 | 复用方式 |
| --- | --- | --- |
| stylelint 自定义插件 | `fe/tools/styles/stylelint-plugin.mjs:34`（`stylelint.createPlugin`），配置注入见 `fe/stylelint.config.js:5-16`（从 YAML 读白名单） | 新增 `neige-calm/*` 设计规则，token 清单同样在 config 里从 `tokens.css` 派生后注入 |
| PostCSS AST 审计（跨文件/跨语言） | `fe/tools/styles/audit.ts:39-91`（返回 `{rule,message}[]`）、`fe/tools/styles/repository-check.mjs:180-230`（遍历全树 + 清单比对 + CLI 入口） | 新增 `fe/tools/design/audit.ts` + `repository-check.mjs`，同形状返回 `DesignViolation[]` |
| 规则注册表 + 集合相等元测试 | `fe/tools/styles/audit.ts:5-8`（`STYLE_RULES` 冻结数组）+ `fe/tools/styles/styles.test.ts:91-106`（evidence map 的 key 集合必须**等于** `STYLE_RULES`） | 新增 `DESIGN_RULES`，同样的双向集合相等测试；**新增规则不写 evidence 就红** |
| ESLint 自定义规则 | `fe/tools/architecture/plugin.mjs:9-18`、规则实现如 `no-class-dom-query.mjs`；接线 `fe/eslint.config.js:56-78` | 新增 `no-inline-style-values` 等 TSX 侧规则，挂进同一 plugin |
| 白名单纪律（路径级） | `fe/tools/architecture/allowlists.mjs:1-21`（显式仓库相对路径、禁 glob、逐条理由注释）+ 陈旧性测试 `fe/tools/architecture/architecture-rules.test.ts:261-284`（**每条白名单必须真的仍然违规**，否则红） | 设计规则的**文件级**豁免照抄该形状 |
| 白名单纪律（值级 + 有效期 + 使用绑定） | `fe/web/src/styles/unlayered-exceptions.yaml` + `fe/tools/styles/repository-check.mjs:148-178`：expiry 格式校验、过期即红、**未被使用的条目也红**、选择器作用域绑定 | 设计规则的**值级**豁免（这是会缩小的那类）照抄该形状，并加"棘轮"字段 |
| fixture 正/反双向约定 | `fe/tools/architecture/architecture.test.ts:224-264`：目录遍历 → positive 必须 exit 0、negative 必须非 0、negative 诊断必须**恰好命中一条规则**（`toEqual(new Set([expected]))`, line 245-246）且**每个 fixture 文件都必须参与违规**（line 248-261） | 设计 fixture 完全同构：`tools/design/fixtures/<rule-id>/{positive,negative}/` |
| tracked-fixture 检查 | `fe/tools/architecture/check-tracked-fixtures.mjs:17-33`（`git ls-files` 对比磁盘目录，防"未 add 的 fixture 假绿"）；另一形状见 `fe/tools/test-tier/checker.ts:47-54`（`FIXTURE_MANIFEST` 双向集合相等） | **必须扩展**：目前它只覆盖 `tools/architecture/fixtures` 与 `tools/mock/fixtures`，`tools/styles/fixtures` 就没被覆盖（既有缺口）。新增 `tools/design/fixtures` 时一并补上 |
| 闸门可失败性证明 | `fe/tools/mutation/manifest.json` + `fe/tools/mutation/runner.ts:120-149`（`validateManifest`）/ `192-216`（`judgeMutation`：dead-mutation / under-red / over-red 三向判决）；已有 CSS 型先例 `app-theme-swap-light-dark-bg`（manifest.json:166-173，target = `web/src/styles/tokens.css`，expected_red = 一条 browser 契约） | 每条设计规则至少一条 mutation；命名空间需扩展（见 §5.3） |
| tier 能力序 | `fe/tools/test-tier/checker.ts:15-20`（`EXPECTED_PROJECTS`）+ `docs/oracle/SCHEMA.md:31-33`：**"jsdom 没有布局，'断言没有发生重排'在 jsdom 里永远通过"**；凡布局/几何/滚动/真实焦点/canvas/**计算样式**一律 `browser` | 设计规则里所有"渲染后才成立"的断言，oracle 条目 `test_tier: browser`，测试文件名 `*.browser.test.tsx` |
| 测试工程 | `fe/vitest.config.ts:5-38`（`platform-independent` node / `web-dom` jsdom / `browser` chromium-headless-playwright）、`fe/playwright.config.ts` | 静态审计进 `platform-independent`（`tools/**/*.test.ts`），计算样式进 `browser` |
| 浏览器取色既有手法 | `fe/web/src/app/theme/theme.browser.test.tsx:11-17` `pixelLuminance()`：1×1 canvas 填充任意 CSS 颜色 → 读回 sRGB 字节 | **对比度检查直接复用这条光栅化通道**（见 §3），无需引入色彩库 |

---

## 1. 规则分类学：形状 → 层

四个可用层（成本从低到高）：

| 层 | 载体 | 何时跑 | 能看到什么 | 看不到什么 |
| --- | --- | --- | --- | --- |
| **L1 stylelint** | `neige-calm/*` 插件规则 | `npm run lint:css`（实测全树 **1.0s**） | 单声明、单规则块、**单文件内**全部规则（rule 拿到整个 postcss root） | 跨文件、TSX、`composes:` 解析、级联、渲染结果 |
| **L2 node CSS-AST 审计** | `tools/design/audit.ts` + vitest `platform-independent` | `npm test` | 全树 CSS + 全树 TS/TSX（`typescript` AST，见 `repository-check.mjs:50-74` 的 join 手法）、清单双向比对 | 级联、`var()` 求值、布局、真实绘制 |
| **L3 ESLint** | `architecture/*` 规则 | `npm run lint:js` | TSX 内的 `style={{...}}`、字符串里的原始值、组件 props | CSS 文件 |
| **L4 vitest browser** | `*.browser.test.tsx`（chromium headless） | `npm run test:browser`（实测 4 文件 9 用例 **4.5s** wall） | `getComputedStyle` 真实求值、`var()` 解析、级联、几何、真实焦点、canvas 光栅 | 全路由真实数据下的组合（那是 playwright） |
| **L5 playwright** | `fe/e2e/*.spec.ts` | `npm run e2e` | 整路由、真实后端、`emulateMedia` 全局态 | —（最贵） |

### 1.1 规则形状对照表

| ID | 规则形状 | 层 | 为什么更便宜的层做不到 |
| --- | --- | --- | --- |
| **DS-TOKEN-001** | token-only 值：受管属性不得出现原始 px / rem / hex / rgb / oklch 字面量 | **L1** | 无更便宜层；纯单声明谓词 |
| **DS-TOKEN-002** | TSX 内联样式/字符串里的原始值（`style={{gap: '7px'}}`、`` `${n}px` ``） | **L3** | stylelint 看不到 TSX；L2 也能做但 ESLint 有现成的 scope/const 解析（`no-class-dom-query.mjs:20-30` 的 module-const 解析可直接复用） |
| **DS-SCALE-003** | 刻度成员性：`font-size` 只能取 `--text-*`、`padding/gap/margin` 只能取 `--space-*`、`border-radius` 只能取 `--radius-*` | **L1** | 无更便宜层。token 清单从 `tokens.css` 派生后经 config 注入，形状同 `stylelint.config.js:5-7` |
| **DS-SCALE-004** | 刻度成员性的跨文件形态：`composes:` 引入的类是否仍在刻度内 | **L2** | stylelint 只看单文件，`composes: x from './y.module.css'` 的目标在别的文件 |
| **DS-FOCUS-005** | 配对声明：**同一文件内**出现 `outline: none/0` 就必须存在 `:focus-visible` 且带非零 outline/box-shadow | **L1** | stylelint 规则拿到整个 root，同文件配对是它能力内的最贵形状；再往上是 L2（跨 `composes`） |
| **DS-STATE-006** | 状态覆盖：交互类必须同时定义 `:hover` 与 `:focus-visible` | **L2** | "哪些类是交互类"只有把 CSS 类名与 TSX（`<button>`/`onClick`/`role="button"`/`tabIndex`）**join** 才知道；stylelint 看不到 TSX。CSS 侧只有 `.btn` 这种命名启发式，是假闸门 |
| **DS-TYPE-007** | 排印配对：出现 `font-size` 就必须有来自阶梯的 `line-height`（同规则块，或同文件 surface 根上有阶梯值） | **L1** | 同文件配对，L1 足够；用 L2 只是把便宜的检查搬贵 |
| **DS-TYPE-008** | 一个 surface 上不超过 N 种不同字号 | **L2** | "surface"≠"文件"：一个 surface 由 feature module + 它 import 的 ui module 共同构成，跨文件聚合 stylelint 做不到 |
| **DS-MOTION-009** | duration/delay 只能取 `--motion-*` 阶梯（含 `transition`/`animation` 简写解析） | **L1** | 单声明谓词 |
| **DS-MOTION-010** | 含动效的文件必须有 `@media (prefers-reduced-motion: reduce)` 块 | **L1** | 同文件存在性 |
| **DS-MOTION-011** | reduced-motion **真的生效**：在 `emulateMedia({reducedMotion:'reduce'})` 下受管元素的 `transition-duration`/`animation-duration` 计算值 ≤ 0.01s | **L4** | L1 只能证明"有一个块"，证明不了它选择器写对了、被更晚的层覆盖了没有。这正是 `feedback_vacuous_invariant_audit` 说的空洞不变量 |
| **DS-CONTRAST-012** | 对比度下限（正文 4.5:1、大字/非文本 3:1），**两个主题** | **L4** | 需要 `var()` 求值 + 级联 + 半透明 overlay 合成 + sRGB 光栅；静态只能看到 `color: var(--text-2)` 而不知道它压在哪块背景上 |
| **DS-DENSITY-013** | 密度/行高：列表行 `getBoundingClientRect().height` ∈ [min,max]；可点击目标 ≥ 24×24 CSS px | **L4** | 需要布局引擎。jsdom 里 `getBoundingClientRect()` 全零（见 §2） |
| **DS-FOCUS-014** | 焦点环**真的画出来**：`:focus-visible` 下 outline 宽度非零且与背景对比 ≥ 3:1 | **L4** | 需要真实焦点 + 计算样式 + 光栅取色 |
| **DS-SWEEP-015** | 全路由扫一遍 DS-CONTRAST-012 / DS-DENSITY-013（真实数据、两个主题） | **L5** | browser tier 只挂载受控 fixture；真实数据下的长文本/空态只有整应用能覆盖 |

### 1.2 每条规则的双向样例（must be red / must stay green）

> `feedback_state_what_must_be_green`：只给"什么该红"会实现得过严并挡住合法代码。

| ID | must be **red** | must stay **green** |
| --- | --- | --- |
| DS-TOKEN-001 | `padding: 7px;` | `padding: var(--space-4);`；`inline-size: 100%`；`grid-template-columns: minmax(0, 1fr)`；`transform: translateY(-1px)`（非受管属性）；`@media (width >= 60rem)`（媒体查询不是声明） |
| DS-TOKEN-002 | `<div style={{ gap: '7px' }} />` | `<div style={{ gap: 'var(--space-4)' }} />`；`style={{ ['--row-count' as string]: rows }}`（写自定义属性做数据通道） |
| DS-SCALE-003 | `font-size: var(--space-6);`（用错阶梯） | `font-size: var(--text-sm);`；`font-size: inherit`；`line-height: var(--leading-snug)` |
| DS-SCALE-004 | `composes: pad7 from './legacy.module.css';` 而 `.pad7` 用了原始值 | `composes: chip from '../../ui/chip/chip.module.css';` 且目标全在刻度内 |
| DS-FOCUS-005 | 文件里有 `outline: none;` 而全文件无 `:focus-visible` | 有 `outline: none;` **且**同文件 `.x:focus-visible { outline: var(--space-1) solid var(--accent); }`；或压根没有 `outline: none`（规则对不涉及的文件必须静默） |
| DS-STATE-006 | `<button className={s.row}>` 且 `.row:hover` 存在但 `.row:focus-visible` 不存在 | 只有 `:hover` 但对应元素是 `<div>` 非交互（纯装饰 hover）；或 `:focus-visible` 来自 `composes` 的共享类；或交互态由 `:focus-within` 在父级承担并在豁免表登记 |
| DS-TYPE-007 | `.title { font-size: var(--text-lg); }` 且同文件无阶梯 line-height | `.title { font-size: var(--text-lg); line-height: var(--leading-tight); }`；或 surface 根 `.page { line-height: var(--leading-base) }` 且 `.title` 只改字号（继承合法） |
| DS-TYPE-008 | 一个 surface 聚合出 6 种不同 `--text-*`（今日 `today.module.css` 正是 6 种） | 4 种及以内；同一 token 出现 20 次算 1 种 |
| DS-MOTION-009 | `transition: opacity 180ms ease;` | `transition: opacity var(--motion-snappy) ease;`；`transition: none`；`animation-duration: var(--motion-pulse)` |
| DS-MOTION-010 | 文件里有 `animation: pulse ...` 但无 reduced-motion 块 | 有动效且有 `@media (prefers-reduced-motion: reduce) { ... }`；文件完全无动效（不得因"没有 reduced-motion 块"而红） |
| DS-MOTION-011 | reduce 下测得 `transition-duration: 0.24s` | reduce 下 `0s`；元素本来就没有 transition |
| DS-CONTRAST-012 | 暗色主题下 `.meta`（`--text-3` on `--surface-card`）测得 3.9:1 | 4.51:1；被登记为"装饰性非文本、不承载信息"的 hairline；`aria-hidden` 的纯装饰元素 |
| DS-DENSITY-013 | 列表行测得 22px（低于 min 28px）；图标按钮 20×20 | 32px 行高；按钮 24×24（含 padding 命中区）；文本自然多行撑高的行（规则只对 `data-nc-row` 这类登记过的密度承载体生效） |
| DS-FOCUS-014 | Tab 到按钮后 `outlineWidth === '0px'` | `outlineWidth: '2px'` 且环与背景 3.2:1；焦点提示由 `box-shadow` 提供（两种通道都必须接受） |
| DS-SWEEP-015 | `/today` 暗色下任意可见文本 < 4.5:1 | 全部 ≥ 阈值；被 fixture 豁免表覆盖的已知条目 |

---

## 2. 空洞断言陷阱：哪些**必须** browser tier

`docs/oracle/SCHEMA.md:31-33` 已经写死这条纪律。下表是设计系统里踩得到的具体形态。

| ID | 在 jsdom 里的具体失败模式（删掉 CSS 依然绿） |
| --- | --- |
| **DS-CONTRAST-012** | jsdom 的 `getComputedStyle(el).color` 只回放**内联样式**与极少数继承属性，对来自 `<style>`/CSS Module 的规则不做级联，**完全不解析 `var()`**。于是返回 `''`。`expect(contrast(fg, bg)).toBeGreaterThan(4.5)` 里 `fg=''` → 光栅化得到透明黑 → 与任意背景的"对比度"要么是 NaN 要么恒高。把整个 `tokens.css` 删掉，断言不变色。 |
| **DS-DENSITY-013** | jsdom 无布局引擎：`getBoundingClientRect()` 恒为 `{0,0,0,0}`，`offsetHeight` 恒 `0`。`expect(rect.height).toBeLessThan(40)` **永真**。这正是"断言没有发生重排在 jsdom 里永远通过"的同族。 |
| **DS-FOCUS-014** | 两重空洞：(a) jsdom 有 `document.activeElement` 但**没有 `:focus-visible` 启发式**（它由浏览器的输入模态决定），选择器不匹配；(b) 即使匹配，`outlineWidth` 仍返回 `''`。`expect(getComputedStyle(el).outlineWidth).not.toBe('0px')` 对 `''` **通过**——删掉焦点环 CSS 照样绿。 |
| **DS-MOTION-011** | jsdom 的 `matchMedia` 是 stub（`matches` 永 false，除非测试自己 mock），`@media` 块根本不参与求值；`transition-duration` 返回 `''`。"reduced-motion 下动效被关掉"在 jsdom 里是纯粹的自证。 |
| **DS-TYPE-008（渲染态）** | 静态版（数 token）在 L2 是诚实的。但若写成"渲染后页面上不超过 N 种字号"，jsdom 读不到计算 `font-size` → 只会数到 0 种 → 永真。**结论：DS-TYPE-008 保持 L2 静态形态，不要伪装成运行时断言。** |

**能留在 jsdom / static 的**：DS-TOKEN-001/002、DS-SCALE-003/004、DS-FOCUS-005、DS-STATE-006、DS-TYPE-007/008、DS-MOTION-009/010 —— 它们的谓词全部是**源码文本层面**的，不依赖渲染。

### 2.1 怎么证明 browser 测试本身不空洞

三道，缺一不可（对应 `feedback_mutation_verify_critical_assertions` 与 `feedback_test_must_drive_production_wiring`）：

1. **前置条件自检（same-test preflight）**——断言探针自己活着，而不是断言目标：
   - `expect(document.styleSheets.length).toBeGreaterThan(0)` 且能在其中找到被测类名（证明 CSS Module 真的进来了）；
   - `expect(getComputedStyle(el).color).toMatch(/^(rgb|oklch|color)\(/)`（证明拿到的**不是空串**，这一条单独就能把 §2 表里全部空洞形态打红）；
   - 对比度探针：`expect(rasterize(fg)).not.toEqual(rasterize(bg))`（证明光栅通道在工作）；
   - 几何探针：`expect(rect.height).toBeGreaterThan(0)`（证明布局引擎在工作）。
   现成先例：`tools/test-tier/layout.browser.test.ts` 就是这类 "layout engine 在不在" 的探针，并被 `checker.ts:55,75-78` 强制必须存在且映射到 browser project。**新增一个 `tools/design/computed-style.browser.test.ts` 探针，用同样的方式在 tier checker 里钉死。**
2. **同文件反向 fixture**——测试内挂一个**已知违规**的 sentinel（`.ds-sentinel-low-contrast { color: var(--text-4); background: var(--paper) }`），断言同一个 checker 函数**报告**它。checker 只要退化成恒真，sentinel 断言立刻红。这就是 `styles.test.ts` 全篇"正反两向"的浏览器版。
3. **mutation manifest 条目**——照抄 `manifest.json:166-173` `app-theme-swap-light-dark-bg`：patch 直接改 `web/src/styles/tokens.css` 的一个颜色/尺寸 token，`expected_red` 只列这条 browser 契约，`why_more_than_one` 解释为什么只有它红。`judgeMutation`（`runner.ts:192-216`）的 `dead-mutation` 判决会把"改了 token 却没有任何测试变红"直接判失败——这正是空洞断言的机器化捕手。

---

## 3. 对比度检查：真实机制

### 3.1 颜色对从哪来（不手维护）

**两级派生，静态是必要条件，浏览器是权威。**

**L2 静态（必要条件，快）**：`tools/design/contrast-pairs.ts` 从 CSS AST 派生"token 网格"：
- 扫全树 CSS，收集每条规则块里出现的 `color: var(--X)` 与 `background/background-color: var(--Y)`；
- 用 postcss 的父子关系 + 同文件选择器前缀关系，建立"前景 token 在哪些背景 token 之上可能出现"的**上界集合**（宁可多算）；
- 对每个 (X, Y) 对，用 `tokens.css` 里的字面量算比值。
这一层只需要 `tokens.css` + CSS AST，**没有任何手写清单**；它抓的是"这两个 token 本来就不该配在一起"的硬错。

**L4 浏览器（权威）**：`tools/design/contrast.browser.test.tsx` 从 **DOM 派生**：
```
for each surface in SURFACE_REGISTRY:
  render(<Surface />)                       // 受控 fixture，非真实数据
  for theme of ['light', 'dark']:
    document.documentElement.dataset.theme = theme
    for el of root.querySelectorAll('*'):
      if (!hasOwnTextNode(el) || isAriaHidden(el) || rect.width === 0) continue
      fg = computed(el).color
      bg = effectiveBackground(el)          // 见下
      ratio = wcagContrast(rasterize(fg), rasterize(bg))
      assert(ratio >= floorFor(el))
```
- `effectiveBackground(el)`：从自身向上走祖先，把每层 `background-color` **按 alpha 合成**到累积底色上，直到遇到 alpha=1 的层为止。**必须合成而不是跳过**——`tokens.css:26-33` 的 `--surface-toggle-overlay` / `--overlay-hover*` 全是 `oklch(0% 0 0 / 0.045)` 这类半透明层，跳过就会把对比度算高。
- `rasterize(cssColor)`：1×1 canvas 填色读回 sRGB 字节，**直接复用 `web/src/app/theme/theme.browser.test.tsx:11-17` 已经在跑的手法**。这样任何 CSS 颜色语法（oklch / rgba / color-mix）都不需要自己实现色彩空间转换，浏览器替我们做。

**没有一份手写颜色对清单。** 唯一手写的是 `SURFACE_REGISTRY`（哪些组件算一个 surface），而它被一条**集合相等元测试**钉死：registry 的 key 集合必须等于 `web/src/features/*` 与 `web/src/app/shell` 下有 `*.module.css` 的目录集合（形状同 `architecture.test.ts:268-280` / `styles.test.ts:102`）。新增一个 feature 而不登记 surface → 直接红，不可能静默漏检。

### 3.2 两个主题怎么覆盖

- 主题是 `[data-theme="dark"]` 属性选择器（`tokens.css:105`），不是媒体查询，所以在同一个 browser 用例里 `document.documentElement.dataset.theme = 'light' | 'dark'` 切换即可重测——`theme.browser.test.tsx:29-34` 已证明这条通道会真实重绘。
- `ThemeMode` 只有 `light | dark | system`，`system` 解析到前两者之一（`app/theme/public.tsx` 的 `resolved`），**没有第三种被渲染的主题**，所以两轮穷尽。这一点由既有契约 `E2E-CAP-THEME-011 exposes exactly three parseable modes` 保证，不需要新的枚举断言。
- L5 sweep 额外用 `page.emulateMedia({ colorScheme })` 覆盖"用户系统偏好 + 未持久化选择"的首帧。

### 3.3 token 变了怎么保持诚实

| 机制 | 说明 |
| --- | --- |
| **零快照** | 不存任何比值 golden。每次运行都从 `tokens.css`（静态层）和实时级联（浏览器层）重新推导。改 token → 下一次运行自动重算。 |
| **surface registry 集合相等** | 见 §3.1，新 surface 不可能静默不覆盖。 |
| **token 形状契约已存在** | `web/src/styles/tokens.contract.test.ts:79-83` 已经把 token 清单**独立钉死**（`INVENTORY` 双向相等）。删/加 token 必然先撞它。`gates-types.yaml` 的 `GATE-TOKENS-001` 明确说手写钉死是**刻意设计**，不要改成自动发现。 |
| **mutation 条目（2 条）** | ①`design-contrast-darken-text-2`：patch 把 `--text-2` 在 dark 下调到 `oklch(45% ...)`，`expected_red` = 对比度 browser 契约；②`design-contrast-drop-alpha-compositing`：patch 把 `effectiveBackground` 的合成分支改成"跳过半透明层"，`expected_red` = 使用 overlay 的那个 surface 的用例。第②条防的是"检查器自己算错还全绿"。 |
| **棘轮豁免** | 见 §4：豁免条目记录 `measured_ratio`；若实测**优于**记录值 → 条目陈旧 → 红，必须删。改好了颜色却留着豁免，闸门会逼你删。 |

### 3.4 公式与阈值（写死出处）

| 项 | 取值 | 出处 |
| --- | --- | --- |
| sRGB 线性化 | `c ≤ 0.04045 ? c/12.92 : ((c+0.055)/1.055)^2.4` | WCAG 2.2 *relative luminance* 定义 |
| 相对亮度 | `L = 0.2126 R + 0.7152 G + 0.0722 B`（线性化后） | 同上 |
| 对比度 | `(L_lighter + 0.05) / (L_darker + 0.05)` | WCAG 2.2 *contrast ratio* 定义 |
| 正文文本 | **≥ 4.5:1** | SC 1.4.3 (AA) |
| 大字文本 | **≥ 3.0:1**，大字 = ≥ 24 CSS px，或 ≥ 18.66 CSS px 且 `font-weight ≥ 700` | SC 1.4.3 |
| 非文本（焦点环、承载信息的边界、图标） | **≥ 3.0:1** | SC 1.4.11 |
| 禁用态 | 豁免（SC 1.4.3 明文排除 inactive control） | SC 1.4.3 例外 |

> 现实提醒：本设计系统的 `--text-base: 13px`、最大也才 `--text-display: 36px`，**"大字"豁免几乎用不到**（只有 `--text-display` 36px 和 `--text-display-sm` 26px 够 24px 线）。checker 必须真的读计算 `font-size`/`font-weight` 来判大字，不能按 token 名猜。
>
> APCA（WCAG 3 草案）**不采用**：它尚未定稿、阈值随版本漂移，把未定稿标准写进闸门会制造"标准更新即全红"的假红。可以作为人工 review 的参考量输出到报告里，但不作为判决依据。

---

## 4. 白名单住在哪里，以及不让它变垃圾场的纪律

### 4.1 三种豁免，三个位置

| 豁免种类 | 位置 | 形状来源 |
| --- | --- | --- |
| **文件级**（"这个文件整体不适用某规则"，应当极少） | `fe/tools/design/allowlists.mjs` | 完全照抄 `tools/architecture/allowlists.mjs:1-21`：导出只读数组、**仓库相对路径、禁 glob**、每条上方一行架构理由 |
| **值级 / 声明级**（存量欠账，**必须缩小**） | `fe/web/src/styles/design-exceptions.yaml` | 照抄 `unlayered-exceptions.yaml` + `repository-check.mjs:148-178` 的四重绑定：`path` + `selector` + `property` + `expiry`，再加 `rule`、`reason`、`issue` |
| **对比度豁免**（测量型） | `fe/tools/design/contrast-exceptions.yaml` | 同上，另加 `theme`、`measured_ratio`（棘轮字段） |

### 4.2 不让它变垃圾场的六条机器纪律

| # | 纪律 | 现成实现 / 新增 |
| --- | --- | --- |
| 1 | **每条豁免必须仍然真的违规**（陈旧即红） | 现成：`architecture-rules.test.ts:266-282` —— 遍历白名单逐条跑规则，`expect(...).toBe(true)` 附消息 `"${entry} is stale"`。设计规则照做 |
| 2 | **每条豁免必须被用到**（未命中即红） | 现成：`repository-check.mjs:174-176` `unused exception ...` |
| 3 | **有效期强制**：`expiry` 必须是合法日历日期且未过期 | 现成：`repository-check.mjs:152-159`（含 `2026-02-30` 这种"格式对但不存在"的日期检测） |
| 4 | **精确绑定**：豁免只对 `selector + property`（对比度再加 `theme`）生效，不是整文件开洞 | 现成：`repository-check.mjs:164-173` 的 selector/property 精确匹配 |
| 5 | **棘轮**（对比度专属）：若实测 ratio **优于** `measured_ratio`，条目判陈旧 → 红，必须删或下调 | 新增。把"改好了但豁免还在"变成硬错，豁免只能单向收缩 |
| 6 | **计数上限**：测试里写死 `MAX_DESIGN_EXCEPTIONS = <当前条数>`，断言 `entries.length <= MAX`；该常量只允许**减小** | 新增。加一条豁免必须在同一个 PR 里显式改这个数字，review diff 上一眼可见 |

### 4.3 明确拒绝的形状

- ❌ **glob 白名单**（`web/src/features/**`）——现有约定第一句就是"必须是仓库相对文件，绝不能是 glob"（`allowlists.mjs:2`）。
- ❌ **可再生成的 baseline 文件** / `--update-snapshot` 式脚本。一旦存在"重跑一下就绿"的按钮，闸门就死了。
- ❌ **`warn` 级别**。设计规则只有 `error` 和"尚未接线"两态。
- ❌ **裸 `/* stylelint-disable */`**。stylelint 侧打开 `reportDescriptionlessDisables: true` + `reportNeedlessDisables: true` + `reportInvalidScopeDisables: true`，与 JS 侧 `reportUnusedDisableDirectives: 'error'`（`eslint.config.js:22`）和 `eslint-comments/require-description`（`eslint.config.js:37`）对齐；再加一条 L2 审计：disable 注释总数不得超过写死的上限。

---

## 5. Fixture 计划

### 5.1 目录与约定

```
fe/tools/design/
  audit.ts                    # DESIGN_RULES 冻结数组 + 纯函数审计（对应 styles/audit.ts）
  repository-check.mjs        # 全树驱动 + CLI 入口（对应 styles/repository-check.mjs）
  stylelint-plugin.mjs        # L1 规则（对应 styles/stylelint-plugin.mjs）
  allowlists.mjs              # 文件级豁免
  design.test.ts              # 正/反遍历 + 规则注册表集合相等 + 白名单陈旧性
  contrast.browser.test.tsx   # L4
  computed-style.browser.test.ts  # L4 非空洞探针（tier checker 钉死）
  fixtures/
    <rule-id>/positive/case.css        # 必须 0 违规
    <rule-id>/negative/case.css        # 必须恰好 1 条、且只属于本规则
    <rule-id>/negative/case.tsx        # 需要 CSS×TSX join 的规则才有
    browser/<surface>/...              # L4 挂载 fixture
```

驱动测试完全照抄 `architecture.test.ts:224-264`：遍历 `fixtures/` 下每个目录 → positive 必须 0 违规 → negative 必须非 0 → negative 的违规**规则名集合恰好等于 `{expected}`**（防"负面 fixture 顺手触发了别的规则"）→ negative 目录下**每个文件都必须参与违规**（防"塞了个没人看的文件"）。

### 5.2 逐规则 fixture 表

| ID | positive fixture | negative fixture（**唯一**违规） | 证明可失败的 mutation |
| --- | --- | --- | --- |
| DS-TOKEN-001 | `@layer ui{.a{padding:var(--space-4);inline-size:100%}}` | 同文件同选择器，只把 `var(--space-4)` 换成 `7px` | 把受管属性集合里删掉 `padding` |
| DS-TOKEN-002 | `<div style={{gap:'var(--space-4)'}}/>` | `<div style={{gap:'7px'}}/>` | 把 `JSXAttribute` 分支的 `style` 名字改成 `styles` |
| DS-SCALE-003 | `font-size:var(--text-sm)` | `font-size:var(--space-6)` | 把 property→前缀映射表里 `font-size` 的值改成通配 |
| DS-SCALE-004 | `composes: chip from './chip.module.css'`（目标合规） | 同上但目标类含原始值 | 让 `composes` 解析在跨文件时直接 `return []` |
| DS-FOCUS-005 | `outline:none` + `:focus-visible{outline:...}` | 只有 `outline:none` | 把"同文件存在 `:focus-visible`"的判定改成恒 true |
| DS-STATE-006 | `.row:hover{} .row:focus-visible{}` + `<button className={s.row}>` | 删掉 `:focus-visible` 规则块（TSX 不变） | 让 TSX join 只认 `<button>` 不认 `onClick` |
| DS-TYPE-007 | `font-size:var(--text-lg);line-height:var(--leading-tight)` | 删掉 `line-height` | 把 line-height 阶梯集合改成"任意值都算" |
| DS-TYPE-008 | 4 种 `--text-*` 的 surface | 5 种（N=4） | 把上限常量 4 改成 99 |
| DS-MOTION-009 | `transition:opacity var(--motion-snappy) ease` | `transition:opacity 180ms ease` | 关掉 `transition` 简写的分词，只看 `transition-duration` |
| DS-MOTION-010 | 有动效 + reduced-motion 块 | 有动效无块 | 把"文件含动效"的探测改成恒 false |
| DS-MOTION-011 | reduce 下 duration 0s | fixture surface 上一个硬编码 `transition-duration:.24s !important` | 把 emulate 的 media 从 `reduce` 改成 `no-preference` |
| DS-CONTRAST-012 | surface fixture 全部达标 | sentinel `.ds-low{color:var(--text-4);background:var(--paper)}` | ①改 `--text-2` 暗色值 ②让 alpha 合成分支跳过半透明层 |
| DS-DENSITY-013 | 行高 32px 的 fixture | 行高 22px 的 fixture | 把 min 从 28 改成 0 |
| DS-FOCUS-014 | 焦点环 2px + 3.2:1 | `outline:none` 且无 box-shadow 替代 | 把"outline 或 box-shadow 二选一"改成只看 outline（会让合法的 box-shadow 方案误红 → over-red 判决抓住） |
| DS-SWEEP-015 | — （playwright，不进 mutation manifest） | — | 由 DS-CONTRAST-012 的 mutation 间接覆盖 |

### 5.3 tracked-fixture 怎么看见它们（两处必改）

1. **`fe/tools/architecture/check-tracked-fixtures.mjs`**：目前只对 `tools/architecture/fixtures`（line 17）与 `tools/mock/fixtures`（line 34）做 `git ls-files` 对比。新增 `tools/design/fixtures` 走同一条 `gitLsFiles` + `directoriesUnder` 差集逻辑（line 17-33）。**顺带补上目前完全没被覆盖的 `tools/styles/fixtures`** —— 这是既有缺口，一个没 `git add` 的 styles fixture 今天不会被任何东西发现。该脚本已挂在 `package.json:21` 的 `lint:js` 链上，不需要新的 CI 步骤。
2. **`fe/tools/test-tier/checker.ts:47-55`**：`FIXTURE_MANIFEST` 与 `BROWSER_PROBE` 是写死的双向集合。新增 `tools/design/computed-style.browser.test.ts` 作为**第二个 browser probe**并写进 checker，这样"设计系统的浏览器探针被误删或被挪出 browser project"会直接红。

### 5.4 mutation manifest 的命名空间扩展

`runner.ts:139-147` 只认 `oracle:` 与 `arch-rule:` 两个命名空间；`arch-rule` 的取值来自 `architecturePlugin.rules` 的 key（`run.mjs:45`）。新增 ESLint 型设计规则（DS-TOKEN-002）**自动**落进 `arch-rule:`，无需改动。stylelint / node-audit 型规则需要新命名空间：

```
// run.mjs:45 旁边
const designRuleNames = new Set(DESIGN_RULES);          // 来自 tools/design/audit.ts 的冻结数组
validateManifest(manifest, { oracle, 'arch-rule': ..., 'design-rule': designRuleNames }, trackedPaths);
```
`validateManifest` 用 `Object.hasOwn(namespaces, namespace)` 做通用查表（`runner.ts:144`），加一个 key 即可，**判决逻辑一行不用改**。同时 `DESIGN_RULES` 与 `design.test.ts` 里 evidence map 的 key 集合双向相等（`styles.test.ts:91-106` 形状），保证"规则存在但没有任何 fixture / 没有任何 mutation"不可能。

---

## 6. 落地排序（诚实版）

### 6.1 今天的实测存量（在 worktree 上跑出来的，不是估计）

| 度量 | 实测 |
| --- | --- |
| `*.module.css` 里含原始长度字面量的声明行 | **82 行**（其中 `1px` 出现 37 次、`6px` 11 次、`2px` 7 次） |
| CSS 里的 hex 颜色 | **0** |
| `font-size` 声明 | **64 条，全部是 `var(--text-*)`**，无一原始值 |
| `line-height` 声明 | **12 条**（对 64 条 font-size） |
| 含 `:hover` 的 module 文件 | 10 / 11 |
| 含 `:focus-visible` 的 module 文件 | **1 / 11**（仅 `settings.module.css`） |
| `outline: none` / `outline: 0` | **0 处** |
| `transition` / `animation` 声明 | **2 处** |
| `@media (prefers-reduced-motion)` | **1 处**（`shell.module.css:339`） |
| 单文件最多不同字号 | **6**（`today.module.css`） |

### 6.2 分档

| 档 | 规则 | 理由 |
| --- | --- | --- |
| **A：直接 `error`，本 PR 生效** | DS-SCALE-003（仅 `font-size` 维度）、DS-TOKEN-001（仅颜色维度）、DS-MOTION-009、DS-MOTION-010、DS-TOKEN-002 | 存量违规为 0 或 ≤2。**但 0 违规的规则必须同时交负面 fixture + mutation**（`feedback_vacuous_invariant_audit`）：DS-FOCUS-005 今天 0 处 `outline:none`，不给反向证据就是空洞不变量 |
| **A′：直接 `error`，但先测后定阈** | DS-CONTRAST-012、DS-FOCUS-014、DS-DENSITY-013 | 先落"只测量、打印报告、不判决"的一个 PR，读到真实数字后**在下一个 PR 里直接开 error + 把当天不达标项写进带 expiry 的豁免表**。不允许"先 warn 观察一段时间" |
| **B：有限、递减、带日期的豁免表** | DS-TOKEN-001（长度维度，82 行）、DS-TYPE-007（约 52 处缺 line-height）、DS-STATE-006（10 个文件缺 focus-visible）、DS-TYPE-008（1 个 surface 超标） | 规则一次性开 `error`，存量**逐条**（不是逐文件、不是 glob）进 `design-exceptions.yaml`，每条带 `expiry` + `issue` |
| **C：只在 sweep 层，不阻塞每个 PR** | DS-SWEEP-015 | playwright 成本高，放 nightly / merge-queue，不进 PR 必过闸门 |

### 6.3 豁免不会变成永久答案的机制

1. **`expiry` 是硬红**（`repository-check.mjs:157-158` 已实现）：到期当天 CI 变红，没有宽限期、没有自动续期入口。
2. **分批到期，不是同一天**：82 条长度豁免按文件切成 4 批，expiry 分别 +30/+60/+90/+120 天。同日全到期会制造一次"不得不整批延期"的压力，那正是豁免变永久的路径。
3. **计数常量单调递减**（§4.2 #6）：`MAX_DESIGN_EXCEPTIONS` 每批清理后必须下调，PR diff 上可见。
4. **陈旧即红 + 未用即红**（§4.2 #1/#2）：修好了却没删豁免 → 红。这让"顺手修了"必然连带清理。
5. **对比度棘轮**（§4.2 #5）：改善即失效。
6. **豁免表本身进 ownership 变更请求**：`package.json:21` 已挂 `tools/ownership/check-readonly-change-requests.mjs`，把 `design-exceptions.yaml` 纳入受管路径，新增条目需要显式变更请求，而不是随手一行 YAML。

### 6.4 明确拒绝"可以静默新增违规"的任何方案

拒绝清单（任一出现即视为闸门失效）：

| 被拒方案 | 为什么 |
| --- | --- |
| 自动生成 / 可重生成的 baseline 文件 | 一个 `npm run design:baseline` 就能把新违规洗白，闸门归零 |
| 规则用 `warn` 而非 `error` | 警告不会阻塞合并，等于没有 |
| 按目录/glob 豁免 | 目录下新增的文件自动获得豁免——这就是静默新增 |
| eslint/stylelint 的 `off` override 段 | 与 `check-eslint-hygiene.mjs`（`architecture.test.ts:155-158` 的 `eslint-no-off-shims` fixture）冲突，本来就已被禁 |
| 无描述的 disable 注释 | §4.3 已封 |
| 豁免条目不带 `expiry` 也能通过 schema | schema 必填（`repository-check.mjs:203-206` 的形状），缺字段直接 throw |

**净效果**：新增一处违规的**唯一**合法路径 = 在同一个 PR 里编辑一个被 ownership 管控的 YAML，写上 selector、property、理由、issue、到期日，并把计数上限往上调（可见 diff）。没有任何一条路径能让违规不留痕。

---

## 7. 不该自动化的部分（会制造虚假信心的）

| 领域 | 为什么 checker 会骗人 | 人工 review 该问的问题 |
| --- | --- | --- |
| **字号选得对不对** | DS-SCALE-003 只证明"在阶梯里"。整页全用 `--text-display` 也 100% 合规 | 「这个元素的视觉权重，和它在信息架构里的重要性一致吗？」 |
| **对比度达标 ≠ 可读** | 4.5:1 是下限不是目标。细字重 + 低色度 + 小字号可以合规但难读；纯色相对比（红/绿同亮度）在 WCAG 2.x 下算高分但对色觉障碍者不可分 | 「达标的这一处，在真实屏幕/亮度/字重下读起来吃力吗？信息只靠颜色区分吗？」 |
| **间距节奏与光学对齐** | 全部取自 `--space-*` 依然可以毫无节奏；光学居中（图标 vs 文字基线）在数值上永远"不对齐" | 「这一屏的留白有呼吸感吗？哪几处的对齐是数值对但看着歪？」 |
| **动效的性格与必要性** | DS-MOTION-009 只管时长在阶梯里，管不了缓动曲线的性格，更管不了"这个动效根本不该存在" | 「这个动效在解释什么状态变化？去掉它用户会丢失什么信息？」 |
| **密度是否适配任务** | DS-DENSITY-013 只有下限（可点击性）。"该稀疏的地方过密"是设计判断 | 「这个视图是扫读还是精读？当前密度支持哪一种？」 |
| **"一个 surface"的边界** | surface registry 是人写的。机器只能校验它与目录结构一致，校验不了这个切分是否反映真实的视觉单元 | 「用户会把这块当成一个整体吗？它和相邻块共享同一套排印规则合理吗？」 |
| **色彩语义** | 机器能查 `--warn` 用在哪，查不出"这里用警告色是否夸大了严重性" | 「这个状态色传达的紧迫感，和后果的严重性匹配吗？」 |
| **空态 / 错误态文案与图形** | 完全在闸门之外 | 「空态告诉用户下一步做什么了吗？」 |

对应到 oracle：这些条目应写成 `verification_owner: review-waiver`、`test_tier: none`、`authoritative_test: NONE`——`SCHEMA.md:29-33` 正是为这类条目准备的。`a11y-contract.yaml` 的 meta 已经诚实记录了"110 条里 74 条无锁定测试"，设计系统同样必须诚实记账，而不是给这些条目硬造一个恒真断言。

---

## 8. 成本

### 8.1 实现规模（估）

| ID | 实现 | LOC（含类型注释，不含 fixture） |
| --- | --- | --- |
| DS-TOKEN-001 | stylelint 规则 + 受管属性表 | ~70 |
| DS-TOKEN-002 | ESLint 规则（可复用 `no-class-dom-query.mjs:20-30` 的 const 解析） | ~90 |
| DS-SCALE-003 | stylelint 规则 + token 前缀映射 | ~60 |
| DS-SCALE-004 | node 审计（`composes` 解析） | ~110 |
| DS-FOCUS-005 | stylelint 规则（root 级 walk） | ~50 |
| DS-STATE-006 | node 审计（CSS × TSX join，最贵的静态规则） | ~180 |
| DS-TYPE-007 | stylelint 规则 | ~60 |
| DS-TYPE-008 | node 审计（surface 聚合 + registry） | ~120 |
| DS-MOTION-009 | stylelint 规则（含简写分词） | ~80 |
| DS-MOTION-010 | stylelint 规则 | ~40 |
| DS-MOTION-011 | browser 测试 | ~60 |
| DS-CONTRAST-012 | 光栅 + WCAG + alpha 合成 + DOM 遍历 | ~200 |
| DS-DENSITY-013 | browser 测试 | ~70 |
| DS-FOCUS-014 | browser 测试 | ~80 |
| 共用基础设施 | `audit.ts` 骨架 + `repository-check.mjs` 驱动 + 豁免 schema/棘轮 + `check-tracked-fixtures` 扩展 + `run.mjs` 命名空间 | ~300 |
| **合计** | | **≈ 1.6k LOC + ≈ 60 个 fixture 文件** |

> 按 `feedback_tiered_review_by_change_size`（PR 目标 ~1k 行），这至少切成 2 个 PR：**PR-A = 基础设施 + 全部 L1/L2 静态规则**，**PR-B = L4 browser 三条 + 豁免棘轮 + mutation 条目**。

### 8.2 CI 时间（在本机 worktree 实测的基线）

| 环节 | 当前实测 | 新增后估计 |
| --- | --- | --- |
| `lint:css`（stylelint 全树 13 个 CSS 文件） | **1.0s** wall | +0.2~0.4s（6 条新规则都是同一次 AST walk 内的额外访问） |
| `platform-independent` + `web-dom`（静态审计所在） | 全套 `vitest run` **9.8s** wall / 118s CPU（85 文件 927 用例） | +0.3~0.8s（node 审计是纯内存 AST，最贵的是 DS-STATE-006 的全树 TSX 解析，可与 `repository-check.mjs` 复用同一次 `ts.createSourceFile`） |
| `browser` project | **4.5s** wall（4 文件 9 用例；chromium 已装，import 2.6s 是固定开销） | **+3~6s**：新增 2 个 browser 文件（固定 import 开销 ~1s），对比度扫 11 surfaces × 2 themes ≈ 22 次挂载 + 每次 ~50 元素的 `getComputedStyle` + canvas 光栅。**canvas 光栅要做结果缓存**（同一 CSS 颜色字符串只栅一次），否则会退化到数千次 `getImageData`，这是这条规则唯一的性能悬崖 |
| `e2e`（playwright，DS-SWEEP-015） | 未测（需起 vite dev server） | **+20~60s**，因此放 nightly，不进 PR 必过闸门 |
| **mutation runner** | 每条 mutation 跑一次**完整** `vitest run`（`run.mjs:73`）= **~11s wall / ~120s CPU** | **这是真正的成本项**：新增 ~14 条 mutation → 全量模式 +2.5 分钟 wall / +28 分钟 CPU。缓解：`selectedEntries`（`runner.ts:151-156`）按变更路径筛选，只有改到 `tools/design/**` 或 `web/src/**/*.module.css` 的 PR 才触发相应条目；全量只在 merge queue / nightly 跑 |

**PR 必过闸门净增：约 +4~7s wall。** 可接受。真正需要治理的是 mutation 全量（建议保持 `--base` 选择模式，全量放 nightly）。

---

## 9. 落地检查清单（给实现 PR 用）

- [ ] `DESIGN_RULES` 冻结数组 + `design.test.ts` 里 evidence map 的 key 集合双向相等（形状同 `styles.test.ts:91-106`）
- [ ] 每条规则：positive fixture 0 违规 / negative fixture **恰好 1 条且只属本规则** / negative 目录每个文件都参与违规（形状同 `architecture.test.ts:244-262`）
- [ ] `check-tracked-fixtures.mjs` 覆盖 `tools/design/fixtures` **并补上遗漏的 `tools/styles/fixtures`**
- [ ] `tools/test-tier/checker.ts` 的 `FIXTURE_MANIFEST` / probe 列表纳入新的 browser 探针
- [ ] `run.mjs` 增加 `design-rule` 命名空间；每条规则至少一条 mutation，且 `expected_red` 是**跑出来测到的**而不是猜的（`judgeMutation` 的 over-red/under-red 会抓）
- [ ] 豁免表：expiry 校验 / 过期红 / 未用红 / 精确绑定 / 棘轮 / 计数上限，六条齐全
- [ ] 所有涉及计算样式、几何、真实焦点、canvas 的 oracle 条目 `test_tier: browser`；不可自动化的写 `verification_owner: review-waiver` + `test_tier: none`，**不造恒真断言**
- [ ] 每条 browser 测试含 §2.1 的 preflight 自检 + 同文件 sentinel 反例
