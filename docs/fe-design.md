# neige-calm 前端设计与视觉规范

状态：定稿 v2（唯一权威文档）
范围：`fe/web/src` 全部四条路由 + 共享外壳。**不覆盖 <640px 移动端**（§7.1）。
读法：本文自足。实现任何一页不需要打开别的文档，也不需要问人。

本文只有两种句子：**告诉实现者要做什么**，和**告诉实现者为什么**。理由压缩成一句，跟在规则旁边。

## 怎么用这份文档

| 你要做什么 | 读哪几节 |
|---|---|
| 落地前置（第一步，其余全部依赖它） | §0 **全读**：§0.0 是每条前置都要走的 commit 程序；token 见 §0.1 + **§0.1a（它的契约后果，漏了必红）**；冻结 `ui/dialog` 的八条变更请求见 §0.4 |
| 实现某一页 | §0 → §2 → §7 → §8.x → 其中引到的组件（§6）→ §5 状态 |
| 新增/修改一个组件 | §2 → §3 → §6 同类组件的四段式 |
| 判断一个视觉争论 | §1 五原则 → §3 通道预算 |
| 写/改闸门 | §9（受管属性清单在 §9.1） |
| 改造现有文件 | §11 |

目录：§0 前置与变更请求 · §1 产品与五原则 · §2 基础 · §3 层级 · §4 动作 · §5 状态 · §6 组件 · §7 页面框架 · §8 四个页面 · §9 闸门 · §10 未决 · §11 迁移

**术语（全文只用这些名字）**：**主区** = `<main>` 整体（1440 下 1240）；**主列** = 内容网格左列（848），**§7.3 与 §9 的空矩形规则只量它**；**面板列** = 内容网格右列（308），不说"次要列"；**rail** = 常驻左栏；**chrome** = rail + 页头这类常驻框架；**未建成** = 实现尚未落地的区域（不说"暂缓""后面的 slice""占号"）；**色调** = 文字颜色（不说"前景""文字色"）；**surface** = 有名字的底色面（不说"面""底"）；**hairline** = 1px 分隔线（不说"发丝线""分隔线"）；**身份点** = 表示 cove 身份的圆点（不说"色点"）；**档** = 按钮强调档（不说"档位""强调档"）；**层** 只指 CSS `@layer`，页面注意力层写 **P0–P3**，视觉层级写**层级**；**矩阵**只指 §5.1 的交互状态矩阵。

---

## 0. 先落地这些前置（其余全部依赖）

`web/src/styles/**`（含 `tokens.css`、`global-classes.yaml` 与两个契约测试）、`ui/dialog/public.tsx`、`ui/confirm-dialog/copy.ts` 都在 readonly 目录下；`repository-check.mjs` 的属性允许表不在，但它同样钉着本文。**下面这些前置变更落地之前，本文任何一条都不可实现。** 每项的理由跟着它在正文里的规则走；这里只给值、给契约后果、给落地程序。

**本文是设计权威。** 当产品需要的形状与某个冻结面今天的能力不符时，本文既不把设计弯成冻结面的形状，也不假装冻结面已经支持它：**写出设计要什么 → 一句话写清冻结面为什么表达不了 → 把所需变更登记进 §0.4/§0.5，精确到可以照着实现。** §0.1 的 token 与 §0.4 的对话框是同一类事情。

**每一条变更都要带它的契约后果。** 冻结面的形状不只写在源文件里，也写在钉着它的契约测试里；只改源文件、不改测试，落地那一刻就是红的。本文对 §0 每一组变更都同时给出"改哪个文件"与"哪条断言跟着改"。

#### 0.0 一道全局程序闸门：`OWNERSHIP-CHANGE` trailer（§0 的每一条都受它管）

`fe/module-file-inventory.yaml` 把 `fe/web/src/styles`、`fe/web/src/ui/dialog`、`fe/web/src/ui/confirm-dialog` 三个目录整体标成 `readonly: true`。`fe/tools/ownership/validator.ts` 的 `readonly-change-trailer` 规则会逐 commit 逐路径检查：**任何改动（含新建）这些目录下文件的 commit，其 message 里必须有一条恰好匹配 `/^OWNERSHIP-CHANGE:\s+(\S+)\s+—\s+\S.+\s+\(#\d+\)$/m` 的行，且捕获到的路径与被改动的那个文件路径逐字相等。**

因此 §0 的落地 commit 必须携带这些 trailer（一个被改文件一行，破折号是 `—` 不是 `-`）：

```
OWNERSHIP-CHANGE: fe/web/src/styles/tokens.css — 设计系统 token 落地 (#997)
OWNERSHIP-CHANGE: fe/web/src/styles/tokens.contract.test.ts — 新 token 族的清单与形状断言 (#997)
OWNERSHIP-CHANGE: fe/web/src/styles/public.ts — 新 token 族进封闭联合 (#997)
OWNERSHIP-CHANGE: fe/web/src/styles/public.contract.test.ts — TOKEN_INVENTORY 同步 (#997)
OWNERSHIP-CHANGE: fe/web/src/styles/global-classes.yaml — 登记十一个全局类 (#997)
OWNERSHIP-CHANGE: fe/web/src/styles/dialog.css — 冻结对话框类名的样式住所 (#997)
OWNERSHIP-CHANGE: fe/web/src/styles/breakpoints.ts — 断点常量的唯一声明处 (#997)
OWNERSHIP-CHANGE: fe/web/src/ui/dialog/public.tsx — CR-1…CR-3、CR-6…CR-8 (#997)
OWNERSHIP-CHANGE: fe/web/src/ui/confirm-dialog/copy.ts — CR-5 参数化 (#997)
```

三点提醒，每一点都是真的会红：**新建文件也算改动**（`dialog.css` / `breakpoints.ts` 都要 trailer）；**路径是文件不是目录**（写 `fe/web/src/styles` 不能覆盖它下面的文件）；**`fe/tools/styles` 与 `fe/tools/ownership` 不是 readonly**，§0.3 改 `repository-check.mjs` 不需要 trailer，`fe/stylelint.config.js` 是 readonly、改它需要。

### 0.1 `tokens.css` 新增/改值

| # | Token | light | dark |
|---|---|---|---|
| 1 | `--weight-normal` / `--weight-medium` / `--weight-semibold` | `400` / `500` / `600` | 同 |
| 2 | `--row-h-sm` / `--row-h` / `--row-h-lg` | `24px` / `28px` / `48px` | 同 |
| 3 | `--control-h-sm` / `--control-h` / `--control-h-lg` | `20px` / `28px` / `32px` | 同 |
| 4 | `--rail-w` / `--rail-w-collapsed` / `--panel-w` / `--drawer-w` | `200px` / `44px` / `308px` / `396px` | 同 |
| 5 | `--measure-prose` / `--measure-form` / `--measure-page` / `--measure-board` | `616px` / `544px` / `1180px` / `1280px` | 同 |
| 6 | `--measure-list` / `--measure-doc` | `720px` / `748px` | 同 |
| 7 | `--warn-text` | `oklch(45% .16 30)` | `oklch(78% .13 30)` |
| 8 | `--success-text` | `oklch(45% .14 145)` | `oklch(78% .13 145)` |
| 9 | `--error-soft` / `--error-border` | `oklch(96% .028 25)` / `oklch(85% .05 25)` | `oklch(26% .05 25)` / `oklch(40% .05 25)` |
| 10 | `--text-on-accent` | `var(--bg)` | `var(--bg)` |
| 11 | `--shadow-float` | `0 1px 2px oklch(0% 0 0/.05), 0 12px 32px oklch(0% 0 0/.08)` | `0 1px 2px oklch(0% 0 0/.3), 0 12px 32px oklch(0% 0 0/.45)` |
| 12 | `--dot-sm` / `--dot-md` | `6px` / `8px` | 同 |
| 13 | `--surface-rail`（**改值**，不是新名） | `98%` → `oklch(96.4% .004 240)` | `15%` → `oklch(13% .008 245)` |
| 14 | `--glyph-sm` / `--glyph` | `14px` / `16px` | 同 |
| 15 | `--slot-h` / `--rule-h` | `240px` / `2px` | 同 |
| 16 | `--menu-w-min` / `--menu-w-max` | `180px` / `320px` | 同 |
| 17 | `--cove-1` … `--cove-8` | 八个 `oklch(62% .09 H)`，H = 20/60/110/150/200/245/290/330 | 八个 `oklch(70% .10 H)`，同 H |

第 9 项是本次修订新增：没有 `--error-soft` / `--error-border`，错误盒只能穿琥珀色，直接推翻 §2.1 写死的 warn/error 分工。第 15–16 项把正文里剩下的裸像素（终端槽、进度轨、菜单宽）收进 token，让 §9.1 的禁原始 px 能真正执行。第 17 项让 cove 身份色不再是内核传来的自由 hex。

**合计 41 个新名字 + 1 处改值**（第 13 项 `--surface-rail` 是改值，它已在清单里）。

#### 0.1a 只改 `tokens.css` 不够 —— 两个冻结件把 token 清单双向钉死了（**CR-0**）

**设计要什么**：上表的 41 个新 token 可用。**冻结面为什么表达不了**：token 清单同时被三处独立钉死，加一个名字会同时判红两条断言。逐条核过：

| 钉死处 | 断言 | 加新 token 的后果 |
|---|---|---|
| `web/src/styles/tokens.contract.test.ts:79–82` | `it('keeps the independently pinned token inventory exact')` → `expect(actual.sort()).toEqual([...INVENTORY, '--font-sans', '--font-serif', '--font-mono'].sort())`，`actual` = `tokens.css` 的 `:root` ∪ `[data-theme="dark"]` 里出现的全部自定义属性名 | **加任何一个 token 立刻红**——它是精确相等，不是包含 |
| `web/src/styles/public.ts` | `export type StyleToken = ColorToken \| ScalarToken \| FontToken \| ZIndexToken`，四族全部是字符串字面量的封闭联合 | 新 token 名不在联合里；而**盒尺度 / 字重 / cove 身份 / 阴影这四类在现有四族里一族都不属于** |
| `web/src/styles/public.contract.test.ts:56`（+ `:66–69`） | `expectTypeOf<StyleToken>().toEqualTypeOf<(typeof TOKEN_INVENTORY)[number]>()`；另有一条 `@ts-expect-error` 断言 `const unknown: StyleToken = '--new-token'` 必须编译失败 | 类型联合与那份 92 项的扁平 `TOKEN_INVENTORY` **双向**相等，改一边不改另一边就红 |

**改成什么**（四件事，一次原子变更，缺一条就红）：

**① `public.ts` 增加两个顶层族、扩两个既有族。** 新族名如下，命名沿用"这些 token 度量的是什么"而不是"它们用在哪"：

```ts
export type BoxScaleToken =                    // 24 项，§9.1 盒尺度组的完整词汇
  | '--row-h-sm' | '--row-h' | '--row-h-lg'
  | '--control-h-sm' | '--control-h' | '--control-h-lg'
  | '--rail-w' | '--rail-w-collapsed' | '--panel-w' | '--drawer-w'
  | '--measure-prose' | '--measure-form' | '--measure-page' | '--measure-board'
  | '--measure-list' | '--measure-doc'
  | '--slot-h' | '--rule-h' | '--dot-sm' | '--dot-md'
  | '--glyph-sm' | '--glyph' | '--menu-w-min' | '--menu-w-max';

export type ShadowToken = '--shadow-float';    // 1 项：值是复合 box-shadow，不是颜色也不是标量

export type WeightToken =                      // 3 项，并入 ScalarToken
  | '--weight-normal' | '--weight-medium' | '--weight-semibold';

export type CoveIdentityToken =                // 8 项，并入 ColorToken
  | '--cove-1' | '--cove-2' | '--cove-3' | '--cove-4'
  | '--cove-5' | '--cove-6' | '--cove-7' | '--cove-8';

// SemanticColorToken 追加 5 项：
//   '--warn-text' | '--success-text' | '--error-soft' | '--error-border' | '--text-on-accent'
// ScalarToken 追加 WeightToken；ColorToken 追加 CoveIdentityToken；
export type StyleToken = ColorToken | ScalarToken | FontToken | ZIndexToken | BoxScaleToken | ShadowToken;
```

24 + 1 + 3 + 8 + 5 = **41**，与上表逐项对得上。**为什么盒尺度与阴影是顶层族而不是塞进 `ScalarToken`**：`ScalarToken` 今天的四个成员（字号/行高/字距/圆角/间距/动效）在 `tokens.contract.test.ts` 里各带一条形状断言，盒尺度虽然也是 px 但语义上是"度量一个盒子"，§9.1 的受管属性表按族取值——把它混进 `ScalarToken`，`font-size: var(--rail-w)` 就成了合法写法。

**② `public.contract.test.ts` 的 `TOKEN_INVENTORY` 追加同样 41 个名字**（它是扁平字面量数组，顺序无关，`toEqualTypeOf` 只比集合）。

**③ `tokens.contract.test.ts` 新增五个分组常量并入 `INVENTORY`，各带形状断言**（现有每一族都有一条，新族不能例外，否则 §0.1 的值可以随意漂移）：

| 新常量 | 成员 | 形状断言 |
|---|---|---|
| `BOX_SCALE` | 上面 24 项 | `it.each` → `root.get(name)` 匹配 `/^\d+(?:\.\d+)?px$/` 且 `dark.has(name) === false`（与 `TYPE_SCALE` 同款） |
| `WEIGHTS` | 3 项 | `root.get(name)` 匹配 `/^[1-9]00$/` 且 `dark.has(name) === false`；另加一条 `expect([400,500,600])` 精确锁死三个值（§2.2 只有三档） |
| `COVE_IDENTITY` | 8 项 | 两个主题都是 oklch 字面量（与 `CONCRETE_SURFACES` 同款）；另加一条断言八项**色相两两不同** |
| `SHADOW` | `--shadow-float` | 只断言 `root.has` && `dark.has`——它是复合值，正则化没有意义；两个主题必须都给值（dark 下阴影更重，§0.1 #11） |
| `THEMED_ALIASES` | `--text-on-accent` | `root.get` 与 `dark.get` **都**匹配 `/^var\(--[a-z0-9-]+\)$/`。**它不能进 `ALIASES`**：`ALIASES` 那条断言写死 `dark.has(name) === false`，而 `--text-on-accent` 两个主题都要给值（§0.1 #10），进 `ALIASES` 必红 |

`MISC` 追加 `--warn-text` / `--success-text` / `--error-soft` / `--error-border` 四项——`MISC` 的 `it.each` 断言明暗双主题都在场，另一条写死三个名字的 `it.each(['--cal-event-waiting-bg','--error-text','--warn-border'])`（`:127`）要同步扩成四加四项，否则新色只有存在性没有形状。

**④ `--surface-rail` 的改值不需要动断言**：它已在 `CONCRETE_SURFACES` 里，断言是"两个主题都是 oklch 字面量"，新值仍然满足。改值本身不改任何契约，只改 §10-7 那次复测的输入。

**落地顺序不存在**：这是一次原子提交（`tokens.css` + 两个 `.ts` + 一个 `.contract.test.ts` 同批），先改任何一边都会红。commit 需要 §0.0 列出的四条 trailer。

**明确不申请**（免得下一个人重新争论）：

| 不申请什么 | 因为 |
|---|---|
| 焦点环宽度/偏移 token | 只声明一次，一次性的值不是 token |
| 阴影阶梯 | 只有一个浮层级别；阶梯会诱导"用深度表达高度"，而这个调色板表达不了 |
| 第五个 surface 级别 | light 端已经没有明度空间 |
| 第二个 accent | accent 的含义是"此处、此刻"，第二个就把它变成装饰 |
| 断点 token | 媒体查询读不到自定义属性——`@media (width < var(--x))` 不存在，做成 token 也无法被使用。断点改由 §0.1b 的一个 TS 常量声明一次 |
| border-width token | 全仓 36 处全是 `1px`；第二种宽度是设计变更，不是 token |

### 0.1b 新建 `web/src/styles/breakpoints.ts`（**CR-0b**，本文三处引用它）

**这个文件今天不存在**（`web/src/styles/` 下只有 `font-stack.ts` 与 `public.ts` 两个 `.ts`）。本文 §7.1 与 §9.1 都把"断点只声明一次"当成既成事实，那是错的——要新建：

```ts
/** 唯一的断点。rail 在这以下折叠成图标条（§7.1）。CSS 侧手写 `@media (width < 60rem)` 并注释指向这里。 */
export const RAIL_COLLAPSE_REM = 60;
```

只有一个导出，因为只有一个断点（640px 以下不在本文范围内，§7.1）。它落在 readonly 的 `fe/web/src/styles` 下，新建也要 §0.0 的 trailer。**它不需要契约测试**：它没有形状可漂移，唯一的风险是 CSS 里的 `60rem` 与它对不上——那由 §9.1"媒体查询里的一切不受管"之外的一条 node 审计兜：全仓 `@media (width` 的数值必须等于这个常量。

### 0.2 `global-classes.yaml` 登记十一个全局类

`fe/tools/styles/repository-check.mjs` 对所有非 `.module.css` 的 CSS 做 `compareGlobalClassManifest`：CSS 里实际出现的全局类集合与 `global-classes.yaml` **双向相等**，多一个报 `CSS-only class`，少一个报 `manifest-only class`。该文件今天是 `[]`，含义是"任何未登记全局类都禁止"。

本文要登记的**封闭清单恰好十一项**：`base` 层定义的 `.tnum` 与 `.calm-prose`（§2.7），加 `styles/dialog.css` 定义的九个对话框类——冻结原语今天写死的八个（`dialog-overlay` / `dialog-overlay-wide` / `dialog-panel` / `dialog-panel-wide` / `dialog-header` / `dialog-body` / `dialog-child-view` / `confirm-dialog-actions`，理由与归属见 §0.4 CR-4），加 CR-7 新增的 `confirm-dialog-label`（双标签同槽的那一格）。先写 CSS 后写 manifest（或反过来）都会判红——这是一次原子变更，不是"实现待办"。**第十二项需要改本文**。

**§9.2 的低对比哨兵不占这份清单的名额**：`.ds-sentinel-low-contrast` 只存在于 browser 层测试自己的 fixture `.module.css` 里（module 类经过哈希，不是全局类），`compareGlobalClassManifest` 只扫非 module 的 CSS，因此看不见它。**禁止**把哨兵写进 `dialog.css`、`base.css` 或任何其它非 module 文件——那会立刻要求它成为第十二项。

### 0.3 `data-*` 属性一律 `data-nc-` 前缀（**全仓重命名**）

`auditDataAttributes` 拒绝生产 TSX 里除 `data-theme` / `data-testid` / `data-nc-<kebab-case>` 之外的任何 `data-*`。因此本文用到的属性**穷尽为六个**：`data-nc-action`（`primary|secondary|tertiary|destructive`，动作按钮的强调档，§4.1）、`data-nc-role`（`row|icon|menu-item|tab|cell`，组件按钮的种类，§4.1）、`data-nc-state`（`open|selected|checked|busy`）、`data-nc-header-rows`（`1|2|3`，页头实际渲染的行数，§6.4）、`data-nc-scrolled`、`data-nc-page-title`。

**现有代码用的是 `data-variant="danger|primary"`**（唯一一处生产写法在 `web/src/ui/dialog/public.tsx:129`，且它在 `repository-check.mjs` 的 `LEGACY_DATA_ATTRIBUTES` 里有一条精确到 `web/src/ui/dialog/public.tsx:data-variant` 的豁免）。选定的名字是 `data-nc-action`，值用 `destructive` 而不是 `danger`——与 §4 的四档词汇同名，一个词汇表管到底。**这是一次全仓重命名，三个落点**：改 `dialog/public.tsx:129`（需要 §0.0 的 trailer）；删掉 `tools/styles/repository-check.mjs` 的那条 legacy 豁免（`tools/` 不是 readonly，不需要 trailer；**豁免留着不删会被"未用即红"判负**，§9.2）；改 `web/src/styles/README.md:33` 里把 `data-variant` 描述成"冻结接口中既有的遗留项"的那句（README 在 readonly 目录下，也要 trailer）。**没有任何测试断言 `data-variant`**——逐条 grep 过全仓，只有上面三处，所以这一步不牵动契约测试。

### 0.4 `ui/dialog/public.tsx` 变更请求（冻结原语必须先动）

**先记录这个文件今天真实的样子**（逐行核对 `web/src/ui/dialog/public.tsx`，本文其它地方不得复述与此不符的行为）：

| 事实 | 行 |
|---|---|
| `ConfirmDialogProps` = `open / title / description? / confirmLabel? / cancelLabel? / onConfirm / onCancel / destructive? / confirmDisabled?`——**没有任何初始焦点入口** | 11–14 |
| `cancelRef` 是 `ConfirmDialog` 函数体内的私有 `useRef`，`initialFocusRef={cancelRef}` 是**写死**的，不经 props 进出 | 126–127 |
| Confirm 用**真** `disabled={confirmDisabled}` | 129 |
| 只要有 `title` 且没传 `hideTitleRow`，`Dialog` 的标题行里**总是**渲染一个 `aria-label="Close"` 的 `×`；它在 DOM 里排在 `.dialog-body` 之前，因此是 `focusables(panel)[0]`。`ConfirmDialog` 从不传 `hideTitleRow`，所以确认对话框**永远有第三个按钮，而且它是第一个 Tab 停靠点** | 115–118 |
| `hideTitleRow` 隐藏的是**整个标题行**（连标题一起），没有"只去掉 `×`"这个能力 | 115 |
| `focusables()` 排除 `[disabled]`、`[inert]` 内的元素，以及任何祖先 `display:none` / `visibility:hidden` 的元素 | 19–31 |
| 面板结构写死八个**全局类名**：`dialog-overlay` / `dialog-overlay-wide` / `dialog-panel` / `dialog-panel-wide` / `dialog-header` / `dialog-body` / `dialog-child-view` / `confirm-dialog-actions` | 107–129 |
| **这八个类今天在全仓没有任何 CSS**（`grep` 只命中该 TSX），`web/src/styles/global-classes.yaml` 是 `[]`——也就是说**今天每一个对话框都是无样式渲染的** | — |
| `confirmDisabled` 是布尔，且是 Confirm 按钮**唯一**的状态入口：按钮上只渲染 `data-variant` / `disabled` / `onClick` 三样，没有任何属性能让 CSS 或测试分辨"前置条件未满足"与"执行中" | 129 |
| `confirmLabel` 是 `string`（不是 `ReactNode`），且是 Confirm 按钮**唯一**的文本子节点；`ConfirmDialog` 不接受 `children`、不接受 render prop | 12 / 129 |
| `Dialog` 有 `restoreFocusRef`，但 **`ConfirmDialog` 不转发它**——它只往下传 `open/title/onClose/initialFocusRef` | 9 / 125–127 |
| `Dialog` 的焦点归还是 `if (target && document.contains(target)) target.focus()`：**触发元素被卸载时静默什么也不做**，焦点落回 `<body>` | 88–89 |

**订正一处本文旧版的失实陈述**：旧版 §5.1 说"已发布的契约测试锁着 `confirmDisabled` 的真 `disabled` 行为"。逐文件读过 `ui/dialog/` 的三个测试文件（`public.contract.test.tsx` 17 行、`public.test.tsx` 113 行、`effect-order.test.tsx` 28 行）——**它们一个字都没提 `confirmDisabled`**。真正钉住"执行中 → 真 `disabled`"的是三条**调用方**测试，全部在非冻结目录下：`features/cove/page/public.contract.test.tsx:45`（`INV-CONFIRM-001`）、`features/wave/page/public.contract.test.tsx:37`、`app/shell/sidebar.test.tsx:166`。这个区别是要紧的：改它们**不需要**动冻结目录，也不需要 trailer。

八条编号变更请求（CR-1…CR-8）加一条续条 CR-5a。每条给：设计要什么 → 冻结面为什么表达不了 → 改成什么 → 不改会怎样。（token 侧的 CR-0 / CR-0b 在 §0.1a / §0.1b。）

**CR-1 · `ConfirmDialog` 的初始焦点覆盖。** §6.13 的打字确认要求初始焦点在输入框上（全产品唯一一个初始焦点不在 Cancel 上的对话框，理由见那里）。冻结面把 `initialFocusRef` 写死成私有的 `cancelRef`，props 上没有入口。**改**：`ConfirmDialogProps` 增加 `initialFocusRef?: RefObject<HTMLElement | null>`，转发时取 `initialFocusRef ?? cancelRef`——不传时行为逐字节不变，已发布的契约测试不动。

**CR-2 · "置位前先聚焦 Cancel"由原语自己执行。** §5.1 需要的保险是：Confirm 变成真 `disabled` 的那一刻，焦点不能掉出面板（`focusables()` 排除 `[disabled]`，焦点陷阱会因此少一个成员）。**调用方做不到这件事**——`cancelRef` 是私有的，没有 props 暴露它，所以"调用方在置位前先 `cancelRef.current?.focus()`"这条规则既写不出来也测不出来，本文旧版写过它，那是错的。**改**：把这个动作放进 `ConfirmDialog` 自己：Confirm **获得真 `disabled` 的那一刻**（CR-6 之后即进入 `confirmState='blocked'`），若 `document.activeElement` 正是那个 Confirm 按钮，则把焦点移到 `cancelRef.current`。`'busy'` **不触发**这条——busy 不摘掉可聚焦性，把焦点从用户按下的那颗按钮上挪走反而是一次意外跳转。测试写在原语的测试里（模拟：聚焦 Confirm → 置 `'blocked'` → 断言 `document.activeElement` 是 Cancel；再置 `'busy'` → 断言焦点**没有**移动）。**不采用**"把 ref 通过 props 暴露给调用方"：那把一条不变量摊派给每一个调用点，多一个入口就多一次漏写。

**CR-3 · 关闭 `×` 的可选性与它在焦点顺序里的位置。** 设计要的确认对话框 anatomy 是 `[标题][主体][Cancel][Confirm]`：一个危险确认不该提供第三个、无文字标签、且恰好排在最前的逃生口——`Escape` 与 `Cancel` 已经是两条完备的退出路径。冻结面只有"整行都不要"这一个开关。**改**：`DialogProps` 增加 `hideClose?: boolean`（只去掉 `×`，标题保留），`ConfirmDialog` 恒传 `hideClose`。**在此之前**：§6.10 的 anatomy 与 §6.13 的焦点顺序按真实 DOM 读，即 `× → …`。对于**保留** `×` 的普通 `Dialog`（本文今天没有这种用法，将来的抽屉/子视图可能有），本文接受它是第一个 Tab 停靠点，不要求改 DOM 顺序——`focusables()` 走 `querySelectorAll` 的文档序，把它挪到末位意味着改 header 的 DOM 结构，代价高于收益。

**CR-4 · 九个全局类的归属与 manifest 后果。** §6.10 的全部视觉规格（面板 `--paper` / `--radius-lg` / `--shadow-float` / `padding` / 遮罩 / 底部 `gap`）只能落在冻结原语写死的那八个类名上。它们今天零 CSS，这正是 `global-classes.yaml` 还能是 `[]` 的原因；给它们写样式，`compareGlobalClassManifest` 的双向相等就要求它们登记进 manifest。**改**：新增一个非 module 的 `web/src/styles/dialog.css`，经 `@import … layer(ui)` 进入，只定义这八个类**加 CR-7 的 `confirm-dialog-label`**；`global-classes.yaml` 变成**恰好十一项**（`calm-prose`、`tnum`，加这九个）。**因此本文旧版"`.tnum` 与 `.calm-prose` 是全应用仅有的两个全局类"是错的**，正确表述见 §0.2 与 §2.7：全局类是一个**封闭清单**，今天恰好十一项，新增一项要改本文。**不采用**"把冻结对话框改成 CSS module"：类名是它已发布的外部形状，三个调用方都贴着它，换掉的成本远大于登记十一个名字。

**CR-5 · `DELETE_COVE_COPY` 的参数化。** `web/src/ui/confirm-dialog/copy.ts` 用 `INV-DUP-010` 把删除文案声明为唯一住所。**订正一处旧版的失实陈述**：该文件声明的是**两个**常量（`DELETE_WAVE_COPY` 与 `DELETE_COVE_COPY`）；文件头注释里的"三处"说的是**整个文件**服务的三个入口（sidebar / cove 页 / wave 页），而 `DELETE_COVE_COPY` 的调用方实际是**两处**（`app/shell/sidebar.tsx:286`、`features/cove/page/public.tsx:97`），第三处（`features/wave/page/public.tsx:122`）读的是 `DELETE_WAVE_COPY`。§6.13 要的是 `Delete <cove 名>?` 与带 wave 数的后果句。**改**：把 `DELETE_COVE_COPY` 从冻结常量改成同文件里的一个纯函数 `deleteCoveCopy(coveName, waveCount)`，返回冻结对象（字段见 CR-5a）。`DELETE_WAVE_COPY` **保持常量不动**——删一条 wave 不参数化。**`INV-DUP-010` 不变且更强**——它保护的是"只有一个声明处"，不是"只有一个字符串"；两个 cove 入口继续读同一个函数。

**CR-5a · `deleteCoveCopy` 的返回形状要装得下 §6.13 的两行正文。** §6.13 的主体是**两条格式不同的句子**：后果行 `This deletes N waves. This cannot be undone.`（`--text-sm`/400/`--text`）与提示行 `Type <cove 名> to confirm.`（`--text-xs`/400/`--text-3`，cove 名 `--font-mono`+`--text-2`）。今天的三元组只有一个 `description` 槽，装不下两条；而把提示行搬到调用点写就是两个入口各写一遍，正是 `INV-DUP-010` 要防的。**改**：返回**四元组** `{ title, consequence, prompt, confirmLabel }`，四个字段全是 `string`，`copy.ts` 保持 `.ts`（不引入 `createElement`）；两行的**排版**由 §6.13 的组件负责，`copy.ts` 只拥有字符串。**单复数写死**：`waveCount === 1` → `This deletes 1 wave.`，其余（含 0）→ `This deletes N waves.`——§2.2 的"计数为零渲染 0"在这里就是 `This deletes 0 waves.`。**不改会怎样**：`description` 里塞两句话，提示行的字号/色调/mono 全部落不下来，§6.13 的主体规格无法实现。

**CR-6 · 两种"不能按"必须能被 CSS 与测试分辨。** 设计要的是三个互斥的 Confirm 状态：**ready**（可按）、**blocked**（名字没打对，§6.13——一个真正的不可用）、**busy**（删除在飞行中，§5.1/§6.10）。§5.1 给 blocked 与 busy 的配方**刻意不同**：blocked 是真 `disabled` + `--text-4` + `--surface-chip` 灰芯片；busy **保留实心红填充**、只把标签换掉（在用户指针底下把实心红换成灰芯片是一次颜色跳变，违反原则 3；而在 `--error` 上写 `--text-3` 直接跌破对比度地板）。**冻结面为什么表达不了**：`confirmDisabled` 一个布尔同时承担两者，按钮上只渲染 `data-variant`/`disabled`/`onClick`，CSS 选不出来、测试也断言不出来。

**改**：**用一个三态枚举替换那个布尔**——

```ts
// ConfirmDialogProps：删掉 confirmDisabled?: boolean，换成
confirmState?: 'ready' | 'blocked' | 'busy';   // 默认 'ready'
```

渲染规则（Confirm 按钮上）：

| `confirmState` | 真 `disabled` | 其它属性 | 点击 |
|---|---|---|---|
| `'ready'`（默认） | 无 | 无 | 触发 `onConfirm` |
| `'blocked'` | **有** | 无 | 不可能（真 `disabled`） |
| `'busy'` | **无** | `aria-busy="true"` + `aria-disabled="true"` + `data-nc-state="busy"` | 原语内部第一行 `if (confirmState !== 'ready') return` 拦截 |

`'ready'` 逐字节等于今天的 `confirmDisabled={false}`，`'blocked'` 逐字节等于 `confirmDisabled={true}`，所以**行为默认不变**。`data-nc-state="busy"` 属于 §0.3 那六个属性，不需要新名字。**不采用两个布尔**（`confirmDisabled` + `confirmBusy`）：两者可同时为真，那个状态没有定义，CSS 还得写优先级；枚举让"互斥"由类型系统保证。

**契约后果（必须一起改，否则落地即红）**：三条调用方测试今天断言"执行中 → 真 `disabled`"——`features/cove/page/public.contract.test.tsx:45`、`features/wave/page/public.contract.test.tsx:37`、`app/shell/sidebar.test.tsx:166`。它们要改成断言 `aria-disabled === 'true'` + `data-nc-state === 'busy'` + `hasAttribute('disabled') === false`。**它们守护的不变量原样保留且更强**：`INV-CONFIRM-001` 说的是"确认对话框不会搁浅"——Cancel 全程可用、Confirm 不能重复触发、拒绝后 pending 被清掉。busy 写法把这三条全部保住，还额外保住了"焦点不掉出面板"（真 `disabled` 会把焦点扔掉，§5.1）。四个调用点（`cove/page/public.tsx:100`、`wave/page/public.tsx:125`、`sidebar.tsx:280`/`:289`）从 `confirmDisabled={pending}` 改成 `confirmState={pending ? 'busy' : 'ready'}`；§6.13 的 cove 删除再多一档：`confirmState={pending ? 'busy' : (matches ? 'ready' : 'blocked')}`。**不改会怎样**：§5.1 与 §6.10 无法同时满足——执行中的实心红按钮到底是红是灰，没有答案。

**CR-7 · Confirm 需要第二个标签节点。** §6.10 要求 Confirm 的标签原地换成 `Deleting…` 且**宽度恒定**，§9 的 browser 闸门断言"busy 前后 `getBoundingClientRect().width` 差值为 0"。§5.1 第 4 条的唯一实现手法是**双标签同槽**（两个标签都在 DOM 里、叠在一个单元格网格里，非当前的那个 `visibility: hidden`），而 §5.1 已经明确否掉了退路 `min-inline-size`（§9.1 里它只能取盒尺度 token，没有一个等于"这个标签的宽度"）。**冻结面为什么表达不了**：`confirmLabel` 是 `string` 不是 `ReactNode`，Confirm 按钮把它当唯一文本子节点渲染，调用方没有任何办法塞进第二个节点。

**改**：

```ts
// ConfirmDialogProps 增加
confirmBusyLabel?: string;   // 默认 undefined
```

- `confirmBusyLabel === undefined`（默认）→ 按钮渲染 `{confirmLabel}`，与今天**逐字节相同**。
- 给了值 → 按钮的内容换成一个两子节点的单元格网格：

```tsx
<span className="confirm-dialog-label">
  <span aria-hidden={confirmState === 'busy'}>{confirmLabel}</span>
  <span aria-hidden={confirmState !== 'busy'}>{confirmBusyLabel}</span>
</span>
```

`dialog.css` 里配套的三条声明（这就是 §0.2 第十一个全局类的全部用途）：

```css
.confirm-dialog-label { display: grid; grid-template-areas: "label"; }
.confirm-dialog-label > * { grid-area: label; }
[data-nc-state="busy"] .confirm-dialog-label > :first-child,
:not([data-nc-state="busy"]) .confirm-dialog-label > :last-child { visibility: hidden; }
```

宽度于是等于两个标签中较宽的那个，静止态与 busy 态像素级相同。**为什么是 `confirmBusyLabel: string` 而不是 `confirmLabel: ReactNode`**：后者把"两个标签怎么叠"摊派给每一个调用点，多一个入口就多一次漏写，而这条规则有一道 browser 闸门在量它——不变量要住在原语里（与 CR-2 同一条理由）。**`aria-hidden` 是载重的，不是保险**：`visibility: hidden` 会把节点移出无障碍树，但 jsdom 单测不加载全局 CSS，没有 `aria-hidden` 时按钮的可及名会变成 `Delete cove Deleting…`，`getByRole('button', { name: 'Delete cove' })` 当场失败。**不改会怎样**：§6.10 的"标签原地换成 `Deleting…`"与 §9 的等宽闸门两条都做不出来。

**CR-8 · 删除成功之后，焦点与路由的落点。** §5.2 写着"模态对话框关闭时必须把焦点还给触发元素"。**冻结面为什么表达不了**：删除成功后触发元素**一定**已经卸载——cove 页页头的 `Delete` 随这一页消失，rail 里 cove 行的删除入口随行消失——而 `Dialog` 的归还逻辑是 `if (target && document.contains(target)) target.focus()`（`:88–89`），对卸载的目标静默什么也不做，焦点落回 `<body>`；`DialogProps` 上虽有 `restoreFocusRef`，但 `ConfirmDialog` 不转发它（`:125–127`），调用方连自救的入口都没有。

**改**（两半，缺一半都不成立）：

**① 原语转发。**

```ts
// ConfirmDialogProps 增加，原样透传给 Dialog
restoreFocusRef?: RefObject<HTMLElement | null>;
```

不传时 `Dialog` 仍旧回落到 `previouslyFocusedRef`，行为不变。

**② 落点写死。** 全应用一条规则，四条路由共用：**销毁一个实体成功后，路由到它的领域父级，焦点落到新页面的页面标题元素**（`[data-nc-page-title]`，§6.4）。

| 删除 | 路由去哪 | 焦点落到 |
|---|---|---|
| cove（页头入口或 rail 入口） | `/`（Today —— 它是根，永远存在；cove 没有领域祖先，§6.4） | Today 的页面标题 |
| wave（wave 页入口或 rail 入口） | 该 wave 的 cove 页 | cove 页的页面标题 |

实现要点三条：`[data-nc-page-title]` 元素带 `tabIndex={-1}`（只为可编程聚焦；它不进 Tab 序，正数 `tabindex` 仍然禁止，§5.2），并把 §5.2 的 `:focus:not(:focus-visible) { outline: none }` 选择器组扩一项 `[data-nc-page-title]`，这样编程聚焦不画环；`restoreFocusRef` 由外壳持有、由**当前页面**的标题元素挂上去，所以它天然指向"删除之后那一页"的标题（冻结原语 `:87` 的注释正是为此写的：cleanup 读的是调用方**最新**的 ref，不是挂载时的节点）；**顺序写死**：`await 删除` → `导航` → 再把对话框 `open` 置 false，这样 `Dialog` 的 cleanup 跑的时候新页面的标题已经挂载。

**不改会怎样**："focus at every step"在最后一步没有答案，键盘用户删完一个 cove 之后焦点在 `<body>` 上，下一次 Tab 从文档开头重来。

### 0.5 构建期常量（Settings 的 ABOUT 依赖它）

`core/api/generated/wire.ts`（710 行）里**没有** `version` / `build` / `data_dir` 任何字段，`vite.config.ts` 也没有 `define`。§8.4 的 ABOUT 节据此定死：

- `version` / `build` 是**构建期**信息，不是接口数据。`vite.config.ts` 加两个 `define`：`__NC_VERSION__`（取 `package.json` 的 `version`）与 `__NC_BUILD__`（取 `git rev-parse --short HEAD`，不可用时取 `'dev'`），在 `env.d.ts` 里声明类型。这是前端自己的变更，不需要内核动。
- `data dir` 是**内核拥有**的信息，今天没有任何 wire 字段。按 §5.3 的"未建成 = 缺席"：**ABOUT 只渲染 version 与 build 两行，`data dir` 这一行整行不渲染**，不画虚线盒、不写"暂无"。将来内核暴露它时，按同样的行格式加进去即可，不需要改本文其它任何一条。

---
## 1. 这个应用是什么

neige-calm 是一个**给 agent 工作流用的工作台**。人把工作切成 **wave**（一件事），wave 归到 **cove**（一个工作区/一个仓库目录）。agent 在 wave 里跑：开终端、改文件、写报告，中途会**卡住并等人**（`blocked` / `reviewing` / `failed`，或某张卡片 `any_card_needs_input`）。人一天进出这个界面几百次，每次只问一个问题：**有没有东西在等我？** 问完就走，或者进去把它解开。

这不是 dashboard，不是内容站，不是演示。它是**家具**：长期在屏幕上，被扫视而不是被欣赏，安静时应该几乎没有颜色。

从这个定位推出五条原则。每条给一个能在代码里指出来的后果，和一个**很诱人但错误**的反例。

### 原则 1 — 行是原子，卡片不是

主要动作是"扫一列 wave，找出需要我的那一个"。信息单位是**共享背景上的一行**，靠节奏和 hairline 分隔，不是各自带 padding/border/radius/底色的卡片。

- **后果**：列表渲染成固定 `--row-h` 的行，`gap: var(--space-1)`，行本身无边框；边框（如果有）属于容器。
- **反例（诱人）**：给 cove 页每条 wave 加 `border + radius + surface + padding`。12 条 wave 就是 12 圈边框、12 个圆角、24 条内边距，眼睛每行都要重新找左文本边。当前 build 在 wave 页正是这么做的。
- **它不是磁贴仪表盘。**

### 原则 2 — 层级是预算，不是调料

强调是有限资源。每个 surface 有且只有一件最重要的事。把第二件东西也变"重要"不会抬高它，只会拉低第一件。

- **后果**：每个 surface 一个主强调、一个主操作、一处 accent 填充。
- **反例**：当前 cove 页页头把标题、计数、"New wave"、"Delete" 画成同样的视觉重量——页面没有焦点，而且**销毁动作和创建动作一样响**。
- **它不是一块每样东西都在喊的看板。**

### 原则 3 — 在持续变化中保持安静

agent 在人阅读的同时往界面里写。任何在数据到达时移动、闪烁、重排的东西，都会让人丢掉正在读的位置。

- **后果**：状态变化用**静态**的色调/点/徽章表达，过渡最多 `--motion-quick`；全应用只允许一个循环动画（running 指示点的 `--motion-pulse`）。没有入场动画、没有错峰出现、没有骨架屏微光。
- **反例**：进度条出现时给行做高度动画；每次 refetch 淡入议程列表。两者都是"标准打磨"，在这里都是错的——一天几百次进入，300ms 入场就是几百次的 300ms 空白。
- **它不是 demo。**

### 原则 4 — 密度是承诺，不是副产品

行高、控件高、rail 宽是**声明出来的数字**，不是 padding 加行高凑出来的结果。

- **后果**：行写 `min-block-size: var(--row-h)`；控件写 `block-size: var(--control-h)`，`padding-block: 0`，垂直居中交给 flex/grid。padding 用来在盒子里居中，不用来造盒子。
- **反例**：legacy 靠三处互不相干的 padding 决策**碰巧**收敛到 28px 行；重写已经把这份运气丢了——当前 build 没有任何一条行高规则，同一个列表里单行行和双行行高度不同。
- **它不是舒适阅读器。**

### 原则 5 — 颜色要么承载意义，要么什么都不承载

只有一个 accent，它的意思是"**此处、此刻**"：选中、焦点、running、唯一的主操作。语义色（`--warn` / `--error` / `--success`）表示人必须处理的状态。没有任何东西为了好看而着色。

- **后果**：静止页面的截图应该接近单色，accent 像素远低于 2%。
- **反例**：给每个 cove 一个身份色点、再给它的行染色、再给它的页头上色。色点本身已经够了，其余把身份变成噪声，并摧毁"accent = 注意力"这个契约。
- **它不是色彩编码的界面。**

---

## 2. 基础（Foundations）

以下数字全部以 token 表达，前置条件见 §0。

### 2.1 颜色角色

`tokens.css` 用 oklch 定义，light 在 `:root`，dark 在 `[data-theme="dark"]`。组件**只**引用语义名，永远不写原始颜色，也永远不写 `[data-theme=...]` 选择器（主题切换是 token 的事，组件不该知道主题）。**因此下表不给 oklch 值**——值只有两个住所：`tokens.css` 与 §0.1。

| 角色 | token | 语义 |
|---|---|---|
| 地面 | `--bg` | 应用地板。`<body>`、页面、主区 |
| 纸面 | `--paper` | 可读可写的内容面：报告文档、输入框、浮层 |
| 导航底 | `--surface-rail` | 常驻 chrome。**两个主题里都比 `--bg` 暗**——仅有的两个方向稳定的 surface 之一 |
| 材质 | `--surface-card` | 有自己边界的"一个物件"：卡片、面板、板上瓦片的底 |
| 面板头 | `--surface-panel-head` | **仅**面板/卡片头部条的底（§6.5）。它是同一个物件内部的"帽子"，不是兄弟区域，所以不违反 §3.2 规则 5 |
| 芯片 | `--surface-chip` | 次级按钮底、禁用填充、进度轨 |
| hairline | `--hairline` | 同级之间的分隔、区域边缘 |
| 强 hairline | `--hairline-strong` | 可交互盒子的边框（输入框、次级按钮） |
| 正文 | `--text` | 用户来看的东西本身 |
| 支撑 | `--text-2` | 仍需阅读的次级内容：字段标签、次级按钮文字、rail 里的普通 wave 行 |
| 元信息 | `--text-3` | 真但附带：时间、计数、路径、提示、空态文案 |
| 装饰/禁用 | `--text-4` | **实测 1.86–2.33:1，永远不承载要读的文字**。只用于分隔符字形、静止状态点、以及禁用元素上的文字 |
| accent | `--accent` | 选中、焦点、running、唯一主操作 |
| accent 软 | `--accent-soft` | "选中/开启"的软填充。**使用点穷尽为四处**：列表行选中、图标按钮开启态、菜单项勾选、输入框焦点环（§5.2）。**禁止做按钮填充**——按钮穿上它，选中的行和"这里有个按钮"就是同一个物件 |
| 等待 | `--warn` / `--warn-text` | 点/条/边框/填充用 `--warn`；文字一律 `--warn-text`（`--warn` 作为文字在 light 下只有 4.00–4.12） |
| 等待软底/边 | `--warn-soft` / `--warn-border` | **等待**盒的底与边 |
| 错误 | `--error` / `--error-text` | `--error` 只做填充/点/边框；文字一律 `--error-text` |
| 错误软底/边 | `--error-soft` / `--error-border` | **错误**盒的底与边（§0.1 #9） |
| 成功 | `--success` / `--success-text` | 同上 |
| 遮罩 | `--overlay-scrim` | 对话框背后 |
| 叠层 | `--overlay-hover-faint` / `--overlay-hover` / `--overlay-hover-strong` / `--overlay-active` | **分工写死**：faint = 本来就有底色的元素再 hover（selected 行、chip、已开启的图标按钮）；默认 = **行**与菜单项的 hover；strong = 图标按钮的 hover（盒子小，需要更实的命中区提示）；active = 任何元素的按下态 |

**语义分工，写死**：`--warn` = 系统在等人（wave 需要输入、审批挂起）。`--error` = 操作失败、有东西要修。**等待的 wave 永远不是红的，崩掉的 wave 永远不是琥珀色的**——包括盒子的底与边：等待盒用 `--warn-soft/-border`，错误盒用 `--error-soft/-border`，两套不混用。

**两个主题是两套设计，不是一套加滤镜。** `--surface-card` / `--surface-chip` 在 light 下比 `--bg` **暗**（凹陷），在 dark 下比 `--bg` **亮**（凸起）——同一个 token，两个相反的读法。因此**高度差永远不由明度方向表达**，只由 surface 的**名字**和 hairline 表达；任何把 `--surface-card` 说成 "raised" 的命名或注释都是错的。

**对比度地板**：正文 **4.5:1**（不主张大字豁免——chrome 里最大的文字是 18px/600，够不着 24px 线）；承载状态或框住控件的非文本 **3:1**（焦点环、状态点、输入框边框）。装饰性 hairline **豁免**，代价是它永远不能是某个含义的唯一载体。**hairline 的实测对比度只在这里给一次：1.22 light / 1.33 dark**（`--hairline` 对 `--bg`）；本文其它地方引用它一律指回这一句，不复述数字。禁用态豁免（WCAG 明文排除 inactive control）。

**color-scheme 必须写在 CSS 里，永远不能用 JS 赋值。** Lightning CSS 把 `light-dark()` 降级成一个由 CSS 里的 `color-scheme` **声明**驱动的开关，JS 赋的值不武装这个开关，于是每一处 `light-dark()` 颜色都解析成空；dev server 不压缩，这个 bug 在开发时看不见。`base.css` 已写死，不要搬到 JS 里。

**z 阶梯**（`tokens.css` 已有，本文三处引用即此）：`--z-base` 0 普通内容 · `--z-raised` 2 行内浮起的 hover 动作与吸底细线 · `--z-sticky` 4 吸顶页头（§6.4）· `--z-overlay` 10 菜单/弹出/抽屉 · `--z-modal` 100 对话框 · `--z-toast` 1000 toast。`z-index` 只能取这六个之一（§9.1）。

**废弃别名**（`tokens.css` 里存在，本文一律不用；清理与 §0 同批走 change request，在那之前只是"不许新增引用"）：`--surface-bg`（用 `--bg`）、`--surface-paper`（用 `--paper`）、`--surface-toggle-overlay` / `--surface-hover-overlay`（用 `--overlay-*`）、`--text-label` / `--text-meta` / `--text-decorative`（用 `--text-2/3/4`）、`--cal-event-waiting-bg`（用 `--warn-soft`）、`--font-serif`（无用途）。**保留但本文暂无使用点**：`--surface-terminal`、`--surface-code`——终端槽落地时它们是终端底与代码块底。

### 2.2 字体与字号

```
--font-sans : -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", …
--font-mono : ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace
--font-numeric = --font-sans   --font-code = --font-mono   --font-display = --font-sans
```

字号阶梯是**加法的，不是乘法的**：级差是固定像素，不是 1.25× 模数。它分三段，**相邻级差只在段内成立**：密排段 11 / 12.5 / 13（级差 0.5–2px，小到相邻两级能坐在同一行里而不改变行高）；文档段 13 / 15；hero 段 18 / 22 / 26 / 36（不参与相邻规则）。**这里的"密排段"是级差的分组，不是 §3.2 规则 3 说的 chrome**——那条规则管的是 chrome 模块**显式设置**的字号，13 是 `body` 默认值、靠继承到达，两处不是一回事（旧版两处都叫 chrome，是这条矛盾的来源）。

| token | px | 用在哪 |
|---|---|---|
| `--text-xs` | 11 | 元信息、段标签、徽章、面包屑、表单提示、机器标识 |
| `--text-sm` | 12.5 | **UI 主力**：行标题、按钮、输入值、控件标签、时钟 |
| `--text-base` | 13 | `body` 默认；文档正文、终端、代码、文档 H2/H3 |
| `--text-md` | 15 | **只**给文档 H1。禁止出现在 chrome 里（离 body 2px、离 `--text-lg` 3px，用在 chrome 上读起来像事故） |
| `--text-lg` | 18 | 页面标题。**每页只有一个 18px 元素**——文档 H1 是 15px，所以带 H1 的报告不会产生第二个 |
| `--text-xl` | 22 | **只**给整页空态的 hero 一行 |
| `--text-display-sm` / `--text-display` | 26 / 36 | 无使用点（时钟已降到 `--text-sm`，§3.3） |

字重只有三档：**400 / 500 / 600**。禁止 300、700、`bold`——13px 下 600 与 700 在多数屏幕上不可分辨，两者会漂移成互换使用。

行高：`--leading-none 1` / `-tight 1.15` / `-snug 1.3` / `-base 1.5` / `-loose 1.65`。**字号越大行高越小。** 控件盒子用 `--leading-none`（让行高由 `block-size` 决定，不由文字撑）；密行用 `-snug`；文档正文用 `-loose`。

字距：`--tracking-tighter -0.02em` / `-tight -0.01em` / `-normal 0` / `-wide .02em` / `-wider .06em` / `-widest .08em`。**负字距只允许 `--text-lg` 及以上**（18px 以下收紧会伤可读性）。**任何 `text-transform: uppercase` 必须同时给 `--tracking-wider` 或 `--tracking-widest`**——11px 大写不加字距不可读。

**排版角色表**（每个角色只有这一种写法）。mono 与 numeric 例外写进角色名，其余一律 sans：

| 角色 | 字号 | 字重 | 色调 | 行高 |
|---|---|---|---|---|
| 空态 hero（整页空态唯一） | `--text-xl` | 500 | `--text-2` | tight |
| 页面标题（四页完全一致，`--tracking-tight`） | `--text-lg` | 600 | `--text` | tight |
| 文档 H1 / H2 / H3 | `--text-md` / `--text-base` / `--text-base` | 600 | `--text` / `--text` / `--text-2` | snug |
| 文档正文（measure 616） | `--text-base` | 400 | `--text` | loose |
| 面板/卡片标题 | `--text-sm` | 600 | `--text` | tight |
| 段标签（"COVES"，uppercase + `--tracking-wider`，全应用唯一的"装饰"） | `--text-xs` | 600 | `--text-3` | none |
| **行标题（内容区）** | `--text-sm` | 500 | `--text` | tight |
| **行标题（rail 的 wave 行）** | `--text-sm` | 400 | `--text-2` | tight |
| **行标题（rail 的 cove 行）** | `--text-sm` | 500 | `--text` | tight |
| 行次行 | `--text-xs` | 400 | `--text-3` | snug |
| 控件标签（按钮/标签页/菜单项） | `--text-sm` | 400 | `--text` / `--text-2` | none |
| 表单字段标签 | `--text-xs` | 500 | `--text-2` | none |
| 表单提示 / 空态 / 加载 / 未建成（同一个角色，四种场合） | `--text-xs` | 400 | `--text-3` | snug |
| 元信息（时间/计数/时长/百分比，`--font-numeric` + `tabular-nums`） | `--text-xs` | 400 | `--text-3` | none |
| 机器标识（路径/id/cwd/branch，**mono**） | `--text-xs` | 400 | `--text-3` | snug |
| 徽章标签（`--tracking-wide`，**必须显式设 `--text-xs`**，见 §6.6） | `--text-xs` | 500 | 随状态 | none |
| 代码/终端（`--font-code`） | `--text-sm` | 400 | `--text` | base |
| 面包屑（祖先 / 当前）——"你在这里"只加一档字重 | `--text-xs` | 400 / 500 | `--text-3` / `--text-2` | none |
| 禁用文字（`--text-4` 作为文字色的唯一合法场合） | 继承 | 继承 | `--text-4` | 继承 |

行标题在 rail 与内容区分两个角色是刻意的：rail 是导航，内容区是内容；rail 内部 cove 行又比 wave 行高一档，因为它是**导航的父级**（三元组以 §6.2 为准）。**三种 wave 行变体的完整三元组以 §6.3 为准**，本表与 §7.5 都指向那里。

**数字**：任何**能在原地变化**的数字必须 `font-variant-numeric: tabular-nums` + `--font-numeric`——时钟、时长、计数、百分比、表格列。文档正文**不要**（比例数字更好看）。时长用定长格式（`04:07` 而不是 `4:07`）。计数为零渲染 `0`，绝不留空——空格子的意思是"未知"，在 agent 工作台里这个区别是真的。数值列右对齐，表头跟列对齐。全局 `.tnum` 在 `base` 里定义一次并登记进 manifest（§0.2），组件引用而不是各自重写。

**UI 字符串一律英文，文档正文中文，不做 i18n。** 两条派生规则：**相对时间**（向下取整，全小写）`< 60s` → `now`；`< 60m` → `Nm`；`< 24h` → `Nh`；`< 7d` → `Nd`；`≥ 7d` → `Nw`；`≥ 30d` → 绝对日期 `Aug 10`。**生命周期短语** = 内核 lifecycle 值原样小写。`core/domain/wave.ts:9–12` 的 `waveLifecycleSchema` **穷尽为九个**：`draft` / `planning` / `dispatching` / `working` / `blocked` / `reviewing` / `done` / `canceled` / `failed`。前端不改写、不翻译、不加形容词。**任何对它做 `switch` 的地方必须覆盖九个分支**（旧版这里只列了六个，照抄会编译失败）。九个值在本文里的归宿是完备的：`isRunning` 覆盖 `planning`/`dispatching`/`working`（`wave.ts:230–232`），`isWaitingForUser` 覆盖 `blocked`/`reviewing`/`failed`（`:199–201`），其余四个（`draft`/`done`/`canceled` 与任何将来新增值）走 §8.2 的"其它 → 状态点 `--text-4`"。

### 2.3 间距

`--space-0/px/1/2/3/4/5/6/7/8/9/10/11/12` = `0 / 1 / 2 / 4 / 6 / 8 / 10 / 12 / 14 / 16 / 20 / 24 / 28 / 32`。基本单位是 2px。

**允许的步长**（三档是**用途表，不是划分**——同一个 token 可以出现在多档里，`--space-6` 三档都在；`--space-0` 与 `--space-px` 三档都可用，0 与 1px 不构成节奏）：行内（在设了 `--row-h*` / `--control-h*` 的组件里）`--space-1,2,3,4,6`——28px 行里出现 20px 间距不是间距，是 bug；chrome 内边距 `--space-3,5,6,7`——`--space-5`(10) 与 `--space-7`(14) 是 rail 专用（§7.4），不出现在内容区；区块之间与页面内边距 `--space-6,8,9,10,11,12`。

**可机检的核是一条上界，不是三个集合**：在设了 `--row-h*` / `--control-h*` 的选择器里，`gap` / `padding*` / `margin*` 不得取大于 `--space-6`(12px) 的 token。**这三档只管 `gap` / `padding` / `margin`**；`inset*` 与 `translate` 是**定位**不是间距，由 §9.1 管值（只能取 `--space-*`），不受分档约束——行内绝对定位的 hover 动作落在 24px 处（`--space-10`），那不是"28px 行里的 24px 间距"。

**常用配对**：

| 场景 | 值 |
|---|---|
| 列表项之间 | `--space-1`（2px），与 28px 行高合成 30px 步距 |
| 段标签到它第一行 | `--space-2`（4px） |
| 区块之间 | `--space-8`（16px） |
| 主列 ↔ 面板列 | `--space-10`（24px） |
| 页面内边距 | inline 24 / block-start 20 / block-end 28（底部多 8px，让滚到底时最后一行不贴边） |

三条硬规矩：**行内间距 < 区块间距 < 区域间距，永远**；**内容元素禁止纵向 margin**（纵向节奏一律来自父级 `gap`——margin 会合并、`gap` 不会，而且 margin 造的节奏无法通过测量一个容器验证；例外是文档流 `.calm-prose`）；**同一个组件不能在同一轴上既写 `gap` 又给子元素写 `margin`**。

### 2.4 圆角

`--radius-xs 2` / `-sm 4` / `-md 6` / `-lg 8` / `-xl 10` / `-pill 999`。圆角随**尺度**递增而不是随高度递增：越大的面越需要更软的角，才能读出它是一整块。

| 层级 | 圆角 |
|---|---|
| 控件（按钮、输入框、图标按钮、行） | `--radius-sm` |
| 材质（卡片、面板、错误盒、空态盒） | `--radius-md` |
| 浮层（菜单、弹出、对话框、抽屉） | `--radius-lg` |
| 点、徽章、头像 | `--radius-pill` |

**控件的圆角永远不大于装它的 surface。** `--radius-xs` 与 `--radius-xl` 目前无用途，新用途需要说明理由。

### 2.5 密度

| token | 值 | 用在哪 |
|---|---|---|
| `--row-h-sm` | 24px | rail 里的 wave 行、菜单项、树行、页头的面包屑行与标识行 |
| `--row-h` | 28px | 默认单行：cove 行、议程行、文件行、日历格、卡片清单行 |
| `--row-h-lg` | 48px | 双行 wave 行（标题 + 生命周期行） |
| `--control-h-sm` | 20px | **行内**图标按钮、徽章 |
| `--control-h` | 28px | 默认：按钮、输入框、下拉、标签页、chrome 图标按钮 |
| `--control-h-lg` | 32px | 页头主操作、对话框输入框、面板头部条 |
| `--rail-w` / `--rail-w-collapsed` | 200 / 44 | 左栏展开 / 图标条 |
| `--panel-w` / `--drawer-w` | 308 / 396 | 面板列 / 对话抽屉（未建成，规格见 §7.6） |
| `--measure-prose` / `--measure-doc` | 616 / 748 | 文档正文 / 文档容器（正文居其内） |
| `--measure-list` / `--measure-form` / `--measure-page` | 720 / 544 / 1180 | 列表行宽度上限 / 表单 / 页面内容上限（**起始对齐**不居中） |
| `--measure-board` | 1280px | 卡片板的**舒适上限**，不是硬约束（见下） |
| `--slot-h` | 240px | 未建成槽与终端槽的标准高度；也是 §7.3 空矩形上限——同一个数字只声明一次 |
| `--rule-h` | 2px | 进度轨、分段控件指示条。两处统一，不设第二种细线厚度 |
| `--dot-sm` / `--dot-md` | 6 / 8px | 状态点、行内身份点 / cove 自己页头的身份点 |
| `--glyph-sm` / `--glyph` | 14 / 16px | 图标按钮里的字形本身（区别于它的盒子） |
| `--menu-w-min` / `--menu-w-max` | 180 / 320px | 菜单宽度 |

**全应用只有三种行高、三种盒高。** 第四种是规范变更，不是组件决策。三种盒高是**基线高度**，任何需要落在同一条基线上的盒子都可以用（面板头部条、徽章），不限于可交互控件。

**`--measure-board` 1280 > `--measure-page` 1180 是刻意的**：卡片板与终端不受页面上限约束（`--measure-page` 只管文本流内容），1280 是它们的舒适上限。1440 视口下主区可用宽是 1240 − 48 = 1192，所以 1280 只有在 **≥1528**（1280 + `--rail-w` 200 + 页面内边距 48）的视口上才真正生效；在那之前板就是"不封顶"。

命中区：`--control-h-sm`（20px）**只允许**出现在一个本身就是更大点击目标的行里（WCAG 2.5.8 的间距豁免：24px 直径圆不相交即可）。独立控件 ≥ 28px。这是桌面 WebView + 鼠标，44px 的触屏指引不适用，套用会毁掉密度。

### 2.6 动效

| token | 值 | 唯一用途 |
|---|---|---|
| `--motion-instant` | 0.06s | 按下反馈 |
| `--motion-quick` | 0.1s | **默认**：hover、色调变化、opacity 显隐、焦点环的 `outline-color` |
| `--motion-snappy` | 0.15s | 菜单/弹出开关、折叠箭头旋转 |
| `--motion-medium` | 0.24s | 对话框进入、抽屉滑出、rail 折叠 |
| `--motion-slow` | 1s | 不确定进度扫描 |
| `--motion-pulse` | 2.2s | **全应用唯一的循环动画**：running 指示点 |

可动画属性**穷尽列举**：`opacity`、`color`、`background-color`、`border-color`、`outline-color`、`transform`/`translate`/`rotate`/`scale`、`fill`、`stroke`，以及**零偏移零模糊的 `box-shadow` 环**（只为 §5.2 的输入框焦点变体，且只过渡颜色）。**永远不要动画** `height/width/inline-size/block-size/margin/padding/top/left/right/bottom/font-size/flex-basis/gap`——它们会在一个正在接收 agent 输出的界面上触发重排。唯一例外是用户主动折叠 rail 时的 `grid-template-columns`。做 `opacity` 显隐时，透明态**必须同时阻断点击**，但用哪一种取决于它还要不要键盘可达：**必须保持可聚焦**的（`:hover` / `:focus-within` 显隐的行内动作，§4.5）用 `pointer-events: none`；**完全下线**的元素才用 `visibility: hidden`。`visibility: hidden` 会把元素移出焦点顺序，于是 `:focus-within` 永远不会因它触发——这不是理论，冻结的 `Dialog` 的 `focusables()` 明写着过滤掉 `visibility: hidden`（`ui/dialog/public.tsx:24–31`）。

缓动：出现用 `ease-out`，消失用 `ease-in`，pulse 用 `ease-in-out`。没有 `cubic-bezier` 字面量，没有弹簧，没有回弹。

**路由切换、挂载、数据到达时没有任何入场动画。** 唯一的例外是浮层（对话框、抽屉、菜单）的开启，配方写在各自的 §6 条目里。焦点环永远不做尺寸过渡（快速 Tab 时必须立刻在场）：`outline-color` 可以过渡，`outline-width` / `outline-offset` 不行。

`base` 层带一条全局 reduced-motion 灭灯开关（用 `!important`——它在最早的层里，反而压得住后面所有层）。任何含动效的文件自己也要带一个 `@media (prefers-reduced-motion: reduce)` 块。"减少"不等于"弄坏"：状态变化仍然立刻发生并保持可读，不能因为显隐动画被关掉就变成不可见。

### 2.7 层与基线

```
@layer reset, vendor, tokens, base, astryx, ui, features, overrides;
```

| 层 | 装什么 |
|---|---|
| `reset` | **不许用 `var()`**：`box-sizing: border-box`、`html/body { margin:0 }`、标题/列表清零 margin、`button,input,select,textarea { font: inherit; color: inherit }`、`button { background:none; border:none; padding:0 }`、媒体元素 `display:block`、`table { border-collapse: collapse }` |
| `vendor` | 第三方 CSS 原样，只经 `@import … layer(vendor)` 进入 |
| `tokens` | `tokens.css`，只有自定义属性 |
| `base` | **必须用 `var()`**：`body` 的字体/字号/行高/色调/背景、`color-scheme`、`h1..h6 { font-size: inherit; font-weight: inherit }`（UA 把字重绑在标签名上，正是"所有标题长得一样"的机械原因）、唯一的焦点配方、`::selection`、`::placeholder { color: var(--text-3); opacity: 1 }`（Firefox 默认 .54 会把占位符压到任何对比度地板以下）、滚动条、`.tnum`、`.calm-prose`、reduced-motion 灭灯。**`.tnum` 与 `.calm-prose` 必须同时登记进 `global-classes.yaml`（§0.2）** |
| `astryx` | `astryx-theme.css`，把 Astryx 变量接到我们的 token（§2.8） |
| `ui` / `features` | 组件样式，**外加唯一一个非 module 文件 `styles/dialog.css`**（冻结对话框写死的八个全局类 + CR-7 的 `confirm-dialog-label`，§0.4 CR-4）。**禁止**：`font: inherit`、`background: none`、`border: none`、`padding: 0`、`cursor: pointer` 这些写在 button 上的复位（`base` 的活）；禁止重写裸元素选择器；禁止 `!important`；禁止原始颜色字面量 |
| `overrides` | **本文不使用。** 它存在只是为了给未来的一次性逃逸留一个有名字的地方；今天往里写任何东西都是缺陷 |

重写之所以"没有层级"，机械原因就是**全仓 11 个 `.module.css` 里有 9 个各自写 `font: inherit`（共 29 处）**，谁都没设过字重——**字重通道从来没接上过**。（唯二没写的是 `wave/list/list.module.css` 与 `wave/lifecycle-badge/lifecycle-badge.module.css`。）`reset.css` / `base.css` 已落地，那 29 处全部删掉是 `base` 的活，§11 逐文件列了。

### 2.8 Astryx：保留，由我们的 token 驱动

`@astryxdesign/core` 提供行为与结构完备的组件，与我们的 token 名冲突经实测为零。`styles/astryx-theme.css` 把它的变量接到我们的 token 上（已实现）：外观是我们的，行为与结构是它的。三条实现约束：

1. 第三方 CSS **只能**经 `@import … layer(vendor)` / `layer(astryx)` 进入；JS 侧 `import 'pkg/style.css'` 禁止（它会静默逃出层序）。
2. `entry.css` 里 `vendor.css` **必须**排在 `astryx-theme.css` 之前——两者都落在 `astryx` 顶层层里，子层优先级按首次出现顺序，颠倒了就把默认调色板交还给了 Astryx。
3. 未映射的变量**故意**保留 Astryx 默认值。半映射比整体照抄更容易 review，而它的色相坡道在我们的词汇里还没有对应物。

它的组件清单（`EmptyState` / `Kbd` / `Timestamp` / `StatusDot` / `Toolbar` …）可以当作"一套完整系统需要什么"的自查表。

---
## 3. 层级怎么表达

**本文存在的理由就是这一节。** 当前 build 的失败不是配色差，是**所有东西都是同一个字重、同一个色调、同一个字号**，于是没有任何东西读起来比别的更重要。

### 3.1 八个通道

| 通道 | token 词汇 | 强度 | 余光可见 | 主要用途 |
|---|---|---|---|---|
| 字号 | `--text-*` | 最强 | 是 | **只在页面/文档级** |
| 字重 | 400 / 500 / 600 | 强 | 是 | 行标题、当前位置、段标签 |
| 色调 | `--text` → `--text-2` → `--text-3` | 中 | 弱 | 内容 vs 支撑 vs 元信息 |
| 位置 | 顺序、固定的右端列 | 中 | 是 | **免费**：第一 = 最重要；右边缘 = 状态 |
| 间距 | `--space-*` | 中 | 是 | 分组、分节 |
| Surface | `--surface-*` | 中 | 是 | **只在区域之间**（rail vs 主区） |
| 边框 | `--hairline*` | 弱 | 否 | "这是个可交互盒子" / "区域到此为止" |
| accent/语义色 | `--accent`、`--warn`、`--error` | 最强 | 是 | 选中、焦点、需要动作的状态 |

### 3.2 预算规则

1. **一个元素上最多三个通道，默认两个。** 两个是常态（字重 + 色调，或字号 + 色调）；第三个必须有写明的理由，全页最多一处。*可机检的内核*：任何单个元素不得**同时**设置非继承的 `font-size`、`font-weight ≥ 600`、非 `--text`/`--text-2` 的 `color`、以及非透明的 `border-color`——四个通道压在一个元素上永远是缺陷。三通道由人 review（§9.3）。**允许超过两个通道的元素是一份封闭清单，今天恰好四项**（增加一项要改本文）：① 等待态的 wave 行（字重 + `--warn-text` + `--warn` 点——**三个通道的具体取值以 §6.3 的变体表为唯一权威，本条不复述数字**）；② 任何 selected 态（`--accent-soft` + `--accent` 边框 + 字重，§5.1）；③ 生命周期徽章（字号 + 文字色 + 边框 + 填充 + 形状——全设计装饰的上限，理由见 §6.6，它不触机检内核因为字重是 500 而不是 ≥600）；④ 分段控件的选中段（字重 + 色调 + `--rule-h` 指示条，§6.11）。
2. **字号只在页面/文档级承载层级。** 行内、卡片内、页头条内、表单内，层级只由字重和色调承担——28px 行里的字号差会造成基线漂移，打断横向扫描线。
3. **chrome 模块显式设置的字号只有三种：11 / 12.5 / 18**（`--text-xs` / `--text-sm` / `--text-lg`）。**不设 `font-size` 从而继承 `body` 的 13 是合法的**，那不是第四种声明；13/15 的**显式**用法只出现在文档与终端这类"望向外来文本的取景窗"里，不计入。写成"三种"而不是"最多四种"，是为了让它真的能被检查——一个永远用不满的上限等于没有上限。
4. **边框永远不承载重要性。** 它只说"这是个可交互盒子"或"区域到此为止"。
5. **Surface 只在区域之间承载层级，不在区域之内。** 同一 surface 上的两个兄弟元素不得靠给其中一个加 `--surface-*` 底色来区分。选中（`--accent-soft`）和 hover（`--overlay-*`）是**状态**，不是层级；面板头部条（`--surface-panel-head`）是同一个物件的内部结构，不是兄弟。
6. **位置是免费通道，先用它。** 凡是"放第一个"或"推到右边缘"能表达的，就不许再花一个颜色或字重通道。
7. **降低优于抬高。** 要让 X 突出，先把周围所有东西降一档色调，再考虑抬 X。密集界面里不重要的元素远多于重要的，便宜的动作是往下。

### 3.3 "最大字号 = 主强调"是必要条件，不是充分条件

四条路由上最大的文字都是**页面标题**，而页面标题是 chrome，不是 P0；其中三条是列表驱动的，它们的 P0 是一个**区域**（一列行），而行内禁止用字号承载层级。所以：

> **P0 由位置 + 密度 + "全页唯一出现语义色的地方"共同指定。** 页面标题满足"最大字号只有一个元素"且**四页完全相同**，于是它退回到框架里而不参与竞争。"每页只有一个 18px 元素"仍然必需——出现第二个是缺陷——**但它不充分，任何检查器都不该被理解为在证明这一页有层级**。

直接推论：**时钟从 36px 降到 `--text-sm`，推到页头右边缘**。它是环境信息，不是可行动信息；一个以"有没有东西在等我"为职责的页面，主强调不可能是时钟。时钟的全部信号变成"位置"这一个通道。

---

## 4. 动作（Actions）

当前 build 最大的缺口：cove 页上 `.newWave` 与 `.delete` 除两条颜色声明外**逐字节相同**。"创建一个东西"和"销毁一个东西"视觉等重。

模型是**两轴**：强调（实心 / 次级 / 幽灵）× 语气（中性 / 危险）。危险是一个**修饰轴**，不是第五档。没有 tonal 档，没有 elevated 档——按钮上带阴影读起来就是消费级 app，密集工作台的高度感应该长在**面板**上而不是控件上。

### 4.1 四档的几何与配方

| 档 | 填充 | 边框 | 文字 |
|---|---|---|---|
| **主（primary）** | `--accent` | `1px solid --accent` | `--text-on-accent` |
| **次（secondary）** | `--surface-chip` | `1px solid --hairline-strong` | `--text` |
| **三（tertiary）** | 透明 | `1px solid transparent` | `--text-2` |
| **危险（destructive）** | 透明 | `1px solid transparent` | **`--error-text`** |

**四档只管"动作按钮"这一类 `<button>`。** 全应用的 `<button>` 分两类，**每个 `<button>` 恰好属于一类，二者不叠加**：

- **动作按钮** —— 带 `data-nc-action="primary|secondary|tertiary|destructive"`，几何由本节定死。
- **组件按钮** —— 带 `data-nc-role="row|icon|menu-item|tab|cell"`，几何由它自己的 §6 条目定死（行 §6.0/§6.2/§6.3、图标按钮 §6.7、菜单项 §6.9、分段控件的段 §6.11、日历格 §6.15）。它们不是"没有强调档的动作"，而是另一种东西：一整行是它自己的目标，一个日历格是一个日期。

没有这个区分，"每个 `<button>` 带 `data-nc-action`"就会强迫行、图标按钮、菜单项去穿 28px + `--space-6` 的按钮几何，那与它们各自的规格逐项冲突。`data-nc-role` 同样是 `data-nc-<kebab-case>`，`auditDataAttributes` 放行（§0.3）。**"每 surface ≤1 个 primary"只数 `data-nc-action`。**

四档**几何完全相同**：`block-size: var(--control-h)`（页头用 `--control-h-lg`）、`padding-inline: var(--space-6)`、`--radius-sm`、边框宽 1px，逐项一致；差别恰好两处——填充与文字色调（边框色跟着填充走）。一排按钮必须落在同一条基线、同一个高度、同一个宽度节奏上，层级必须在"灰度 + 一个色相"下可读，而不是靠形状。每一档在静止态都带一条透明边框，这样 hover/focus 时加边框不会改变布局。

**默认档是 tertiary。** 次级只在"必须不用读就能找到"时使用（工具栏、主操作旁边的 Cancel）。一页所有按钮都是次级，等于没有层级。

**anatomy**：`[可选 --glyph-sm 前置图标] [标签]`，`gap: var(--space-2)`。按钮最多同时用**图标 + 标签**两个通道，不得再叠边框粗细变化或第二种颜色。**状态**见 §5.1；实心 primary 的 hover 是 `--accent` 叠 `--overlay-active`（更深，不是更浅）。

### 4.2 每个 surface 一个主操作

surface = 一个页面的主区、一个对话框、一个弹出层、一个内联表单。**零个是允许的，而且常见**（wave 页就没有主操作）。实心 accent 只保留给页面/对话框级的主操作；卡片级的"主操作"最高只能到 secondary——否则一个有 12 张卡片的 wave 页会有 13 个主操作，也就是一个都没有。

档位由 `data-nc-action="primary|secondary|tertiary|destructive"` **属性**声明，不是靠临时类名（属性名的由来与全仓重命名见 §0.3）。这是"每 surface ≤1 个主操作"和整张状态矩阵能被机器检查的前提。

**只有 primary 允许实心 `--accent` 填充**，`--accent-soft` 禁止做按钮填充（§2.1）。**没有"全应用唯一的实心 accent"这回事**——cove 页的 `+ New wave`、Settings 的 `Save`、整页空态的 `New cove`、对话框的 Confirm 各自是自己那个 surface 的唯一 primary。口径只有一个：**每 surface ≤ 1 个 primary**。

### 4.3 危险动作：静止即着色

**决定：危险动作在静止态就是红的**（`--error-text` 文字，透明填充，透明边框）；hover/focus 时补上 `--error-soft` 填充 + `--error-border` 边框（**不是** `--warn-*`——删除是 error 语气，§2.1 的分工写死了）。理由：只在 hover 才出现的红，把警告从键盘路径上**完全删掉**了，鼠标路径上也要等指针已经压上去才出现——信号恰好在做决定的那一刻缺席。防呆需要冗余信号，冗余信号必须在场。

避免"警报疲劳"靠的不是藏起颜色，而是**降低强调档并拉开距离**：危险动作用最低能用的强调档，与任何良性动作之间至少隔 `--space-6`，在菜单里排在分隔线之后的最末位。**危险动作不得与良性动作混在同一组里**；同组时排最后。单独成组（如 Settings 的 `DANGER` 节）时它既是第一个也是最后一个，那是合法的。

**实心红按钮全应用只有一个位置**：危险确认对话框里的确认键（`background: --error`；文字 `--text-on-accent`；`border-color: --error`；实测 4.82 light / 7.93 dark）。它的完整状态：hover 叠 `--overlay-active`；active 同 hover 且不做过渡；busy 见 §5.1。屏幕上同时出现两个实心红按钮就是 bug。

**确认阶梯**（从轻到重，不要越级）：① **撤销优于确认**——可逆的事直接做，配一个带 Undo 的 toast（§6.14）。② **普通确认对话框**（§6.10）——不可逆但代价小；按钮文案用具体动词（"Delete wave"，不是"OK"）。③ **打字确认**（§6.13）——只给"罕见且灾难性"的操作，**全产品只有一个这样的操作：删除 cove（级联删除它所有 wave）**。"只有一处"说的是**操作**，不是入口：cove 页页头的 `Delete` 与 rail 里 cove 行的删除入口（§6.2）走的是同一个打字确认对话框、同一份文案（§0.4 CR-5）。一个操作有两种确认强度，才是真正的漏洞。用多了它就变成新的标准，也就不再是保护。

危险对话框的**初始焦点永远落在安全动作上**：普通确认对话框是 Cancel（冻结原语今天就是这么写的，`ui/dialog/public.tsx:126–127`），打字确认是那个输入框（它本身就是安全动作——不匹配时按 Enter 什么也不发生；这需要 §0.4 CR-1）。这条可以直接写成测试，而且它是 §5.1 busy 规则的前提。

### 4.4 图标按钮的可发现性

- 必须有非空 `aria-label`；`title` 属性或自定义 tooltip **不能**替代无障碍名。两者都要有：`aria-label` 给无障碍树，真 tooltip 给视力用户。装饰性 SVG 加 `aria-hidden="true"` 与 `focusable="false"`。
- hover 时必须改变背景，让命中区可见。
- 纯图标只对**重复出现的、每行/每卡一个**的动作合法（标签会重复 N 次）；**每页只出现一次**的动作必须带文字——一排五个没有任何文字的图标是记忆力测验。
- 危险的、或没有明显逆操作的图标动作，必须在某个带文字标签的菜单里有第二条路径。
- **图标资产**：真 SVG 图标集是后面的一个 slice。在那之前用文本字形，穷尽为 `▾ ‹ › ← × ★ ● ◉ ▪ ▫`；尺寸由 `font-size: var(--glyph-sm | --glyph)` 表达，盒子由 `--control-h-sm | --control-h` 表达。新增字形要在这里加一个。

### 4.5 hover 显隐的动作

`opacity: 0` 直到 `:hover` / `:focus-within` 才出现的行内动作，**只允许**用于"另有其它到达路径"（右键菜单，或该条目自己的页面）的行级快捷操作。它们必须：在 `:focus-within` 时出现（键盘可达）；静止态用 `opacity: 0` + `pointer-events: none`，**不用 `visibility: hidden`**——那会把它移出焦点顺序，`:focus-within` 就永远不会触发（§2.6）；**在静止态就预留空间**，出现时不许让行重排；如果它是个开关且当前为"开"（比如已 pin），就**永久保持 opacity 1**——否则"取消"这个动作无从发现。

**预留宽度是算出来的，不是猜的**：一行最多两个 `--control-h-sm` 动作，行的 `padding-inline-end: calc(var(--control-h-sm) * 2 + var(--space-2))` = 44px。第三个动作必须进右键菜单。

**hover 永远不会移除已有的背景。** 当前 build 的 `.newWave:hover { background: var(--overlay-hover) }` 把 accent 填充换成了灰——按钮在你指向它时变得**更不显眼**。hover 只朝一个方向走：更高对比。

---

## 5. 状态

### 5.1 交互状态的通用语义

| 状态 | 规则 |
|---|---|
| rest | 幽灵/三级控件透明；其余按 §4.1 |
| hover | 按 §2.1 的叠层分工加一层。**hover 不是层级信号，永远不能比 selected 更强**。不可交互元素上出现 hover 是 bug |
| active（按下） | 换成 `--overlay-active`。进入时不做过渡，离开时短过渡 |
| focus-visible | **叠加**在当前状态之上，永不替换它 |
| disabled（真正不可用：表单未改动、权限不足） | `color: --text-4` + `--surface-chip` 填充 + `cursor: default` + 真 `disabled` 属性，**不可聚焦**。**绝不用 `opacity`**——它嵌套时会连乘，还会把边框一起淡掉（控件同时丢掉标签和形状），并把元信息静默压到任何对比度地板之下 |
| **busy（动作执行中）** | 见下 |
| selected | `--accent-soft` 填充 + `1px --accent` 边框 + **标题字重一律 600**（不是"比静止态高一档"——rail 的 wave 行静止是 400，加一档只到 500，会跟 "WAITING ON YOU" 分区的 500 撞成同一个读数）。**至少两个通道**（色块 + 字重），因为 3–5% 的色块在某些面板上根本看不见 |
| invalid | 边框 `--error-border` + 一条 `--error-text` 消息。颜色永远不是唯一载体 |

**busy 的唯一写法**（四件事，四处复述必须逐字一致：这里、§5.3、§6.10、§8.4）：

1. `aria-busy="true"` + `aria-disabled="true"` + `data-nc-state="busy"`，**不写真 `disabled` 属性**，激活事件在 handler 里被拦截（第一行 `if (busy) return`）。
2. **视觉上保留静止态的填充与边框**，只把文字色降一档、并 `cursor: default`：透明填充的档（tertiary / destructive / secondary）降到 `--text-3`；**实心填充的档（primary、危险确认里的实心红）保持 `--text-on-accent`**——在 `--accent` 或 `--error` 上写 `--text-3` 直接跌破对比度地板。**busy 不穿 disabled 的皮**：`--text-4` 只准出现在真 `:disabled` 上（§9 的门禁就是这么写的，照旧版的 `--text-4` 会被自己的门禁判红）；而且在用户指针底下把一个实心红按钮换成灰芯片，是一次颜色跳变，违反原则 3。"按不动了"这个信号由标签承担，不由颜色承担。
3. 标签**原地**换成 `Saving…` / `Deleting…`（**不是**"保持标签"——那样按钮在执行中没有任何可见变化）。
4. **宽度恒定，用双标签同槽实现，不用 `min-inline-size`**：按钮内容是一个单元格网格（`display: grid; grid-template-areas: "label"`），静止标签与 busy 标签**都在 DOM 里、叠在同一格**，非当前的那个 `visibility: hidden`。宽度于是等于两者中较宽的一个，静止态与 busy 态像素级相同。旧版写的 `min-inline-size` 锁不出来——§9.1 里 `min-inline-size` 只能取盒尺度 token，没有一个等于"这个标签的宽度"；`grid-template-*` 与 `visibility` 都不受管（§9.1），所以这条写得出来也测得出来（测法：量静止态与 busy 态的 `getBoundingClientRect().width`，断言相等）。

为什么不用真 `disabled`：`disabled` 元素不可聚焦，用户按下按钮的那一刻焦点会从它身上掉出去；在对话框里这会直接打破焦点陷阱（`Dialog` 的 `focusables()` 明确排除 `[disabled]`，`ui/dialog/public.tsx:19–23`）。

**唯一的例外，且是刻意的**：`ConfirmDialog` 的 Confirm 在 **blocked** 态（§6.13 的"名字没打对就不能按"）用真 `disabled`——那是一个真正的不可用，配方走上表的 disabled 行（`--text-4` + `--surface-chip`）。**执行中不是这个例外**：`busy` 态走上面四条的 `aria-disabled` 写法，保留实心红填充。两者今天由同一个布尔 `confirmDisabled` 承担、CSS 与测试都分辨不出来，这正是 §0.4 **CR-6** 存在的理由：那个布尔被换成 `confirmState: 'ready' | 'blocked' | 'busy'`。**在 CR-6 落地之前，删除进行中的 Confirm 就是灰芯片**——这是一个已知缺陷，不是一条可以照着实现的规格；不要把它写进新代码。

blocked 这个例外的代价由两件事补掉：**危险对话框的初始焦点在安全动作上（§4.3），Cancel 在整个执行过程中保持可用**，所以焦点陷阱永远至少有一个成员；以及 **Confirm 由可用变 `blocked` 的那一刻，若焦点正在它身上，焦点被移到 Cancel**。第二件事**由 `ConfirmDialog` 自己执行，不是调用方的责任**——`cancelRef` 是它的私有 `useRef`（`:126`），props 上没有出口，"调用方先 `cancelRef.current?.focus()`"这条规则写不出来也测不出来。它需要 §0.4 **CR-2**。

对话框之外的任何 busy 控件（Settings 的 `Save`/`Reset`、内联新建的提交、行内动作）一律走上面四条的 `aria-disabled` 写法。

三条硬约束：**hover 与 selected 对任何组件都不得渲染成同一结果**；**每一个可聚焦元素只要有 `:hover` 规则，就必须有对应的 `:focus-visible` 规则，且给出等强或更强的可见效果**（键盘用户看到的不能比鼠标用户少；这条只管**可聚焦元素自身的选择器**，"hover 时改变某个装饰性子元素的颜色"不需要复制一份）；状态一律用 `data-nc-state` / `aria-*` 表达，不用裸类名——这样整张矩阵可以用属性选择器检查。

### 5.2 焦点环 —— 全应用一个配方

```css
/* @layer base，只声明这一次 */
:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
:where(button, [role='button'], a, summary, [data-nc-page-title]):focus:not(:focus-visible) { outline: none; }
```

（`[data-nc-page-title]` 在选择器组里，是因为 §0.4 CR-8 会用 `.focus()` 把它作为删除成功后的落点——编程聚焦不该画环。）

**用 `outline`，不用 `box-shadow`。** 理由是机械的：`box-shadow` 会被 `overflow: hidden` 的祖先裁掉——卡片板和可滚动的 rail 正是这样的祖先；`outline` 不会被裁，而且现代浏览器会尊重 `border-radius`。宽度 2px、对比 ≥3:1 是唯一给出数字的可及性标准（焦点态与非焦点态在**同一批像素**上比较，所以画在元素**外面**是最省事的达标方式）。两个被认可的变体：**内嵌** `outline-offset: -2px`，给通栏的行、标签页、菜单项，或任何被 `overflow: hidden` 祖先裁切的元素；**输入框** `outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-soft)`，因为 2px 外框套在 1px 边框外会读成双边框（这里的 `box-shadow` 是"环"不是"高度"，合法）。

**任何文件出现 `outline: none` / `outline: 0`，同文件同选择器必须有替代的焦点表现。** 列表和菜单用 roving tabindex（整个列表一个 tab 停靠点，方向键在内部移动）——40 个 wave 的 rail 不能要按 40 次 Tab。焦点只在模态对话框里被困住，而模态对话框必须困住它、关闭时必须还给触发元素、必须能 `Escape` 关闭。**触发元素在关闭时已被销毁的那一类（删除成功）由 §0.4 CR-8 定死落点：路由到领域父级，焦点落到新页面的 `[data-nc-page-title]`。**可聚焦的行要设 `scroll-margin-block-start: var(--header-h)`（页头把自己的实际高度写在 `--header-h` 上，§6.4），保证聚焦元素不会被吸顶页头盖住。禁止正数 `tabindex`。

### 5.3 空态 / 加载 / 错误 / 未建成

**空态**分三档：**内联**（已有内容的页面里某个空列表）= 一行"表单提示"角色的文字，装在 `--row-h` 高、`1px dashed var(--hairline)` 的盒子里；**区域**（整个面板列为空）= 一行提示文字 + 一个 tertiary 动作；**整页** = 一行 hero（`--text-xl` / 500 / `--text-2`）+ **一个** primary 动作。

空态文字用 `--text-3`，必须读起来像"这里什么都没有"，而不是像内容。空容器画**虚线** hairline（这是"有个容器但里面没东西"与"有东西"的区别），绝不用实线、绝不填色。**空态没有插图、没有图标**，一句话，加上（如果存在的话）那个动作。空态不道歉、不解释功能、不占用比它替代的内容更多的纵向空间。

**最重要的一条**：**当一个区域的空缺只有唯一一个解法时，就在内容本该出现的位置和尺寸上直接渲染那个动作的界面本身，而不是渲染一段描述加一个别处的按钮。** cove 页没有 wave → 直接在第一行的位置展开并聚焦新建 wave 的输入（组件规格见 §6.12）；rail 没有 cove → 第一行位置就是内联新建输入。

**加载**：

| 场景 | 渲染什么 | 为什么 |
|---|---|---|
| < 200ms | 什么都不渲染（不闪、不转圈） | 闪一下比等一下更贵 |
| > 200ms | 该区域内一行 `--text-3` 文字（`Loading waves…`），复用内联空态的盒子 | |
| 重新拉取已有数据 | 旧数据留在原地 + 一个 `--text-3` 指示，**绝不进入加载态** | 内容不消失、不移动 |
| 任何时候 | 无骨架屏、无微光 | 骨架屏在赌布局可预测，而行内容是变长的；形状错误的幽灵比什么都不显示更糟 |
| 控件执行中 | 按 §5.1 的 busy 写法（四条，一条不少）：`aria-busy` + `aria-disabled` + `data-nc-state="busy"`，标签换成 busy 标签，宽度靠双标签同槽恒定 | 转圈会改宽度，`disabled` 会丢焦点 |

那个 200ms 计时器是一个共享的 `useDelayedPending(200)`，不是每个组件各写一个 `setTimeout`——四个页面各自实现会得到四种行为。**它住在 `web/src/ui/state/public.ts`**：它是纯 React 状态，不碰 API，因此 `features/**` 可以直接 import 它（`features` 禁止 import 的是 `app/**`，不是 `ui/**`——`features/settings/public.tsx` 今天就在 import `ui/state/public.ts`）。页面从 props 收到裸的 `pending` 布尔，自己调这个 hook 得到"是否该显示加载文字"。

**错误**：`[--dot-sm --error 点] [消息 --error-text] [Retry，tertiary]`，装在 `--error-soft` 底、`1px solid --error-border`、`--radius-md`、`padding: var(--space-3) var(--space-4)` 的盒子里，**就地出现在失败的那个区域内**，不做整页横幅（除非整页失败）。堆栈信息放在 `Details` 折叠里（§6.14），`--font-mono` / `--text-xs`。有位置可放的错误不要用 toast（toast 只用于"用户已经导航走之后完成的动作"的结果）。

**未建成的区域**（这是一类缺陷，不是两个实例）：

> 实现尚未落地的区域，**按真实内容将占据的几何**渲染（高度取 `--slot-h` 或该槽的真实几何），`1px dashed var(--hairline)`，无填充，双轴居中**一行**文字（`--text-xs` / 400 / `--text-3`），形如 `<名词> is not wired up yet.` 或 `No <名词> yet.`，**至多六个英文单词**。此外什么都没有：没有模块路径、没有文件名、没有 slice 名、没有 README 引用、没有契约说明、没有道歉、没有图标、没有动作。

理由：**形状**是有用的信息（它教会用户将来会得到什么布局），**那句话**是一次性的说明，超出这两者的都是一个开发者写给另一个开发者的便条，只不过被渲染进了产品里。**推论（写成规则）**：**任何点名模块路径、文件、slice、契约、工单或 README 的字符串，都不得作为 UI 渲染在任何界面上。**

---
## 6. 组件

格式统一：**anatomy → 尺寸 → 状态 → 它刻意不做什么**。状态若未特别说明，就是 §5 的通用语义。

### 6.0 行（通用）

全应用最主要的可交互元素。cove 行、wave 行、议程行、卡片清单行、文件行、菜单项都是它的特例；本文其它地方说"通用行"指的就是这一条。

- **anatomy**：`[可选前导槽（点或图标）] [主内容，可省略号] [可选尾部元信息] [可选 hover 动作]`，整行是一个 `<button>` 或 `<a>`（行本身就是目标，行内不再放"打开"按钮）。作为 `<button>` 时它带 `data-nc-role="row"`，**不带 `data-nc-action`**——它不是四档动作按钮，几何由本节定（§4.1）。
- **尺寸**：`min-block-size` 取三个 `--row-h*` 之一；`padding-block: 0`（垂直居中交给 grid）；`padding-inline: var(--space-3)`（rail）或 `var(--space-4)`（内容区）；`--radius-sm`；`display: grid` + 显式列；每个可能省略号的子项写 `min-inline-size: 0`；有 hover 动作时 `padding-inline-end: calc(var(--control-h-sm) * 2 + var(--space-2))`。列表项之间 `gap: var(--space-1)`。
- **状态**：rest 透明；hover `--overlay-hover`；active `--overlay-active`；focus-visible 用内嵌变体（`outline-offset: -2px`）；selected `--accent-soft` + `1px solid var(--accent)` + **主内容字重 600**（一律 600，不是"+1 档"，§5.1）；selected 再 hover 用 `--overlay-hover-faint`。列表用 roving tabindex。
- **刻意不做**：给自己加边框或独立底色（原则 1）；靠 `margin-inline-start: auto` 把状态推到远端（那会在 720px 宽的行上制造几百像素的空白，用固定列宽）；主内容换行（永远省略号，全文放 `title`）；超过一对 hover 动作。

### 6.1 Rail 分区

- **anatomy**：段标签 + 可选的尾部动作 → 一列行。
- **尺寸**：标签占一个 `--row-h-sm` 槽；**不加自己的 `padding-inline`**——rail 的 inline 内边距只有一层，写在 rail 上（`--space-5`，§7.4）；行间 `gap: var(--space-1)`；分区之间 `--space-6`。
- **状态**：标签不可交互。尾部动作是一个 `--control-h`（28px）图标按钮，**静止即可见**（不 hover 显隐）——分区级动作没有"行"可供发现，也因此不能用 20px（§6.7 禁止行外 20px）。
- **刻意不做**：折叠/展开（分区是稳定的，折叠会藏起用户正在监视的状态）；显示计数徽章（计数长在行上）；画 hairline（间距已经够了）。**行数为零的分区整个不渲染**——没有标签、没有虚线盒。这就是为什么 rail 静止时看起来空、有活时看起来完整。

### 6.2 Cove 行

- **anatomy**：`[折叠箭头] [身份点] [名称，省略号] [计数，tabular]`。
- **三元组**：`--text-sm` / **500** / `--text`（rail 与内容区一致）。cove 行是导航的父级，比它下面的 wave 行（400 / `--text-2`）高一档——§2.2 的角色表指向这里。
- **尺寸**：`--row-h`；`grid-template-columns: var(--control-h-sm) var(--dot-md) minmax(0,1fr) auto`（20 / 8 / 1fr / auto）；`gap: var(--space-2)`；`padding-inline: var(--space-3)`；`--radius-sm`。折叠箭头是一个 20px 盒子里的 `--glyph`（16px）字形——**盒子是控件高，字形是字形尺寸，16 不是第四种控件高**。
- **状态**：§6.0。选中 = `--accent-soft` + `--accent` 边框 + 名称 **600**。**身份点在所有状态下保持自己的颜色**——它是身份，不是状态，也不 pulse。
- **身份色**：身份点写 `background: var(--cove-color)`，由行根元素以自定义属性注入（`style={{ '--cove-color': 'var(--cove-3)' }}`，§9 允许的数据通道）。**值只能是 `--cove-1..8` 之一**，槽位 = cove id 的稳定哈希 mod 8。内核的 `color` 字段前端不直接渲染；若它将来成为用户可选项，存的也必须是槽位名而不是自由 hex。
- **刻意不做**：用 cove 身份色给行底染色；有子项时变高；静止态显示删除动作（hover 显隐，且在右键菜单里有第二条路径）。**那个删除动作与 cove 页页头的 `Delete` 是同一个操作**，走同一个打字确认（§6.13 / §4.3）——入口有两个，确认强度只有一种。

### 6.3 Wave 行

**四个变体。三元组、行首那一列装什么、等待态怎么加重，全部在这里定死。本文其它地方（§2.2、§3.2、§7.5、§8）一律以本表为准，且不得复述具体数字。**

| 变体 | 高 | 行数 | 行首 6px 列 | 静止标题 字号/字重/色调 | **等待态标题** | 用在哪 |
|---|---|---|---|---|---|---|
| 默认 | `--row-h-lg` 48 | 2 | 状态点 | `--text-sm` / 500 / `--text` | 500 / `--warn-text` | cove 页主列 |
| 紧凑 | `--row-h` 28 | 1 | 状态点 | `--text-sm` / 400 / `--text` | **500 / `--warn-text`** | Today 主列的 WAITING / RUNNING / RECENT；cove 页面板列 |
| 议程 | `--row-h` 28 | 1 | **cove 身份点** | `--text-sm` / 400 / `--text` | 500 / `--warn-text`（**无点**，见下） | Today 面板列的议程 |
| rail | `--row-h-sm` 24 | 1 | 状态点 | `--text-sm` / 400 / `--text-2` | 500 / `--warn-text` | rail |

**"等待态"的成员判定与 `needsUserAttention` 逐字一致**：`isWaitingForUser(lifecycle) || anyCardNeedsInput`（`core/domain/wave.ts:214–216`）。

**等待态一律把字重抬到 500**，四个变体没有例外。这是 §3.2 那份四项封闭清单第①项赖以成立的第三个通道（字重 500 + 标题 `--warn-text` + 点 `--warn`）；把紧凑变体的等待行留在 400，那条清单第①项就只剩两个通道，而 Today 的 P0 也就失去了余光可见度。**静止态**才是 400——这两者不冲突，本表把它们放在同一行里正是为了让下一个人看得见。

**议程变体只与紧凑变体差一件事**：行首那 6px 列装的是 **cove 身份点**（`--cove-1..8`）而不是状态点。理由是列表的语义：Today 面板列的议程**跨 cove**且不按状态筛选，"这条是谁家的"才是那一列该回答的问题；而主列的 WAITING / RUNNING 两节已经按状态分了节，它们的点是那两节存在的理由（§8.1 P1）。派生三条：议程变体**不渲染状态点**，等待只由标题的字重 + 色调两个通道表达（面板列不是 P0 的住所，§8.1）；议程变体**不渲染相对时间**（面板 308px，且议程本身按日期分组，相对时间是冗余）；议程变体可以在标题前多一个**小时标签**槽（`--text-xs` / 400 / `--text-3`，`.tnum`），仅当这一条来自 `ScheduledEvent`（§8.1）。

- **anatomy**：`[行首点 --dot-sm] [第一行：(可选小时标签) 标题 · (右)相对时间] [第二行：生命周期短语]` + hover 显隐的 pin/remove。**紧凑、议程、rail 三个变体丢掉第二行**，不是缩小它——所以**生命周期短语在这三种行里不存在**，本文任何一处要求它们显示生命周期都是错的。
- **尺寸**：`grid-template-columns: var(--dot-sm) minmax(0,1fr) auto`，**列间 `column-gap: var(--space-2)`**（与 §6.2 同）；两行之间 `row-gap: var(--space-1)`；行首点 `align-self: center`——它跨两行**居中**，不与第一行基线对齐（它标的是整行，不是标题）；`padding-inline: var(--space-4)`（rail 变体 `var(--space-3)`）；`padding-block: 0`，48px 里的 30.7px 内容由 grid 居中，余量不写死；行宽上限 `--measure-list`（cove 页用法）；`--radius-sm`。**一屏两种密度是刻意的**：rail 24–28px，内容区 48px；这是 legacy 的签名手法，不是漂移。
- **状态**：§6.0。等待态按上表加重标题，并给**状态点** `--warn`（议程变体没有状态点，故只有标题两个通道）。running 态给状态点 `--accent` 并 `--motion-pulse`；身份点**任何状态下都不变色、不 pulse**（§6.2）。
- **刻意不做**：用卡片边框或每行独立底色；给标题换行；显示超过一对 hover 动作；**为进度条预留空间**（见下）。

**关于第二行与进度条 —— 已定的事实。** `progress` / `eta` / `now` 这三个 overlay 在整个仓库里除一个测试 fixture 外**没有任何写入方**，在生产中永久为空。因此：**任何界面都不得为它们预留视觉空间**（契约缝隙保留，`waveActivityFrom` 继续解码，但布局不能假设它会被填满）；**第二行由生命周期短语承载**——那是一个真实的、永远存在的列，不是由 `now` 承载，这是"48px 双行行"能成立的全部理由；**进度轨这段 CSS 不写**——一段永远走不到的分支不是"以防万一"，是死代码，现有 `row.module.css` 里那段**删除**（§11）。将来真有写入方时它的规格是：`--rule-h`、贴在行 block-end 边缘、通栏、`--surface-chip` 轨 / `--accent` 填充、`--z-raised`、画在 48px 之内不额外增加行高、**步进**永不做宽度动画。

`any_card_needs_input` 与它们不同：**内核确实在写它**（`crates/calm-server/src/card_fsm.rs`），"有卡片在等你"是可靠信号，可以承载一页的 P0。

### 6.4 页头

- **anatomy**：三行 —— `[返回图标按钮] [面包屑]` / `[身份点?] [标题] [计数/徽章*] [弹簧] [主操作?] [危险操作?] [时钟?]` / `[机器标识]`。**除标题外每个槽位都可空**；标题行永远不省略，且恰好含一个页面标题元素（带 `data-nc-page-title`）。
- **尺寸**：面包屑行与标识行各 `--row-h-sm`（24），标题行 `--control-h-lg`（32），行间 `gap: var(--space-3)`；标题块与动作组之间 `--space-6`；页头下方到内容 `--space-8`。
- **`--header-h` 是每页实算的，不是常量。** 它由**实际渲染了几行**决定：标题行恒在（32），每多渲染一行加 `--row-h-sm` + `--space-3`（24 + 6 = 30）。页头元素带 `data-nc-header-rows="1|2|3"`，CSS 里三条声明把它算出来：

```css
[data-nc-header-rows]     { --header-h: var(--control-h-lg); }                                              /* 32 */
[data-nc-header-rows="2"] { --header-h: calc(var(--control-h-lg) + var(--row-h-sm) + var(--space-3)); }     /* 62 */
[data-nc-header-rows="3"] { --header-h: calc(var(--control-h-lg) + var(--row-h-sm) * 2 + var(--space-3) * 2); } /* 92 */
```

  **四页的取值**（由 §6.4 的规则推出，不是逐页选择）：**Today 32**（根，无面包屑；无机器标识）· **cove 62**（无领域祖先 → 无面包屑；有 cwd 标识行）· **Settings 62**（有面包屑；无机器标识）· **wave 92**（三行俱全）。把 `--header-h` 写成静态 92 是旧版的错：四页里只有一页真的是 92，而 `scroll-margin-block-start` 用错了值就会把聚焦的行藏到吸顶页头后面或空出 60px。
- **吸顶**：`position: sticky` 在主区滚动容器顶部，`--z-sticky`，背景 `--bg`；block-end 的 hairline **只在 `[data-nc-scrolled]` 时出现**，只过渡 `border-color`。**该属性由主区滚动容器的 `scroll` 监听在 `scrollTop > 0` 时写在主区元素上**，用 `requestAnimationFrame` 节流，一处实现四页复用；不用 `IntersectionObserver`（哨兵元素会在页头高度变化时错位），不用 `animation-timeline`（目标 WebView 支持面未验证）。
- **规则**：第一行渲染该实体的**领域祖先**（wave 的祖先是它的 cove；cove 没有祖先，整行省略；Settings 的祖先是工作区；Today 是根）——这是规则，不是逐页选择。第三行没有机器标识就省略。
- **刻意不做**：滚动时变小（会重排用户正在读的内容）；承载超过一个主操作；居中任何东西；用与页面不同的 surface。

### 6.5 卡片 / 面板

- **anatomy**：可选头部（`--surface-panel-head`，面板标题 + 尾部控件）→ 主体 → 可选底部。
- **尺寸**：头部 `block-size: var(--control-h-lg)`，`padding-inline: var(--space-4)`，block-end hairline；主体 `padding: var(--space-4)`；卡片 `--radius-md` + `1px solid var(--hairline)` + 底 `--surface-card`。**面板列里的面板不画自己的边框**：宽度来自 §7.2 的内容网格（`--panel-w`），它与主列的分离由那 24px 的沟承担（边界决策阶梯第一步），画边框是重复表达。
- **状态**：rest / focus-within（边框 → `--hairline-strong`）/ 板上选中 / 拖拽中（后两者边框 → `--accent`）。
- **刻意不做**：投阴影；嵌套另一张卡片；给自己的 resize 做动画；用 `--radius-lg`（那是浮层的）。

**边界的决策阶梯**（按顺序走，不许跳）：① `gap` 能不能分组？能就用 gap。② 需要可见边界吗？用 `1px solid var(--hairline)`。③ 是不同**功能**的区域吗？换 surface。④ 浮在无关内容之上吗？才轮到 `--shadow-float`。**一个边界不得同时用 hairline 和 surface 变化**，除非两个 surface 的明度差小于 3.0 L（`--paper`↔`--bg` 与 `--surface-rail`↔`--bg` 正是需要它的那两对）。

**阴影不表达高度**，仅有的四个例外是菜单/弹出/对话框/toast（抽屉按弹出算）。`box-shadow` 允许的非高度用途穷尽为两种：焦点环（`0 0 0 <n>px <色>`，零偏移零模糊）与内嵌 hairline（`inset 0 0 0 1px <色>`，真边框会破坏布局时）。任何**同时**有非零模糊和非零 y 偏移的 `box-shadow` 都是高度阴影。

### 6.6 徽章 / pill

- **anatomy**：`[--dot-sm 点] [标签]`。
- **尺寸**：`block-size: var(--control-h-sm)`；`padding-inline: var(--space-2)`；`gap: var(--space-2)`；`--radius-pill`；`1px solid`；**`font-size: var(--text-xs)`，字重 500**。

**字号必须显式设，这不是可选的**：徽章长在页头标题行里（§6.4/§8.3），不设字号就继承——继承到 18px 会直接判红"每页恰好一个 18px 元素"（§9），继承到 13px 会判红"chrome 里只有 11/12.5/18"。旧版两处写"不设 `font-size`"，那让两条门禁互相夹死，怎么写都有一条红。它也不触 §3.2 的机检内核：那条禁的是**字号 + 字重 ≥600 + 非 `--text/--text-2` 色调 + 非透明边框**四件同时成立，徽章的字重是 500。它是 §3.2 那份四项封闭清单里的第三项，装饰上限就定在这里。
- **状态**（不可交互）：中性 `--surface-chip` / `--hairline` / `--text-3` / 点 `--text-4`；等待 `--warn-soft` / `--warn-border` / **`--warn-text`** / 点 `--warn`；running `--accent-soft` / `--accent` / `--text` / 点 `--accent`；错误 `--error-soft` / `--error-border` / `--error-text` / 点 `--error`。
- **刻意不做**：出现在列表的**每一行**上（行只配 6px 点 + 色调；pill 留给详情页头，且每页只有一个——每行一个彩色 pill 会让列表永久多彩，摧毁"颜色 = 注意力"）；变成按钮；用 `--text-4` 做标签。

### 6.7 图标按钮

- **anatomy**：一个 `--glyph-sm`（14px）或 `--glyph`（16px）字形，光学居中在方盒子里。
- **尺寸**：行内 `--control-h-sm`（20px），chrome 里 `--control-h`（28px）；`--radius-sm`；`display: grid; place-items: center; padding: 0`。带 `data-nc-role="icon"`，不带 `data-nc-action`（§4.1）。
- **状态**：rest 透明 / `--text-3`；hover `--overlay-hover-strong` / `--text`；active `--overlay-active`；选中/开启 `--accent-soft` / `--accent` / `1px --accent`（§2.1 允许的四个 `--accent-soft` 使用点之一）；disabled 透明 / `--text-4`。
- **刻意不做**：没有无障碍名就存在；成为某个危险动作的唯一入口；**在行外用 20px**（分区标签行、页头、工具栏一律 28px）；hover 时换字形（只有盒子和描边变）。

### 6.8 输入框 / 文本域 / 下拉

- **anatomy**：`[字段标签] [输入框] [提示 | 错误]`，`gap: var(--space-1)`；字段组之间 `gap: var(--space-4)`。
- **尺寸**：`block-size: var(--control-h)`（对话框里 `--control-h-lg`）；`padding-inline: var(--space-3)`；`padding-block: 0`；`--radius-sm`；`1px solid var(--hairline-strong)`；背景 **`--paper`**。**文本域**：`block-size: auto`，`min-block-size: calc(var(--control-h) * 3)`，`padding-block: var(--space-2)`，`resize: vertical`；装提示词或路径时用 `--font-mono`。**下拉**：与输入框像素级同高同边框，尾部加一个 `--glyph-sm` chevron，`padding-inline-end: var(--space-8)`；chevron 静止 `--text-3`、hover `--text-2`；`[data-nc-state=open]` 取焦点边框但不取环。
- **状态**：§5.1 + §5.2 的输入框焦点变体。invalid 同时渲染消息，消息用 `--error-text` 而不是 `--error`。
- **刻意不做**：浮动/动画标签；用占位符当标签（占位符是 `--text-3`，只能放例子，永远不能放字段名）；用 `--bg` 当填充（当前 settings 输入框这么做，于是 light 下输入框比卡片亮、dark 下比卡片暗——同一个组件表达两个相反的意思）；已选中的值用 `--text-3`（选中的值是内容：`--text`）。

### 6.9 菜单

- **anatomy**：浮层 → 可选段标签 → 菜单项 → `--hairline` 分隔 → 危险项**最后**。
- **尺寸**：`min-inline-size: var(--menu-w-min)`，`max-inline-size: var(--menu-w-max)`；`padding: var(--space-1)`；项 `block-size: var(--row-h-sm)`，`padding-inline: var(--space-3)`，`--radius-sm`，带 `data-nc-role="menu-item"`；面 `--paper` + `1px solid var(--hairline)` + `--radius-lg` + `--z-overlay` + `--shadow-float`。
- **状态**：rest 透明 / `--text`；hover 或 roving 焦点 `--overlay-hover`；选中/勾选 `--accent-soft` + `--accent` 前置勾；危险项静止即 `--error-text`，hover 加 `--error-soft`。roving tabindex；开启 `--motion-snappy`（opacity + 2px 位移），关闭瞬时。
- **刻意不做**：嵌套超过一级子菜单；内含表单控件；超过 12 项还不带搜索（12 项是一屏 24px 行的极限，再多就必须滚动，而滚动的菜单不可扫读）；桌面点击菜单用 hover 打开（点击打开，打开后 hover 移动）。

### 6.10 对话框

- **anatomy**：遮罩（`--overlay-scrim`）→ 面板：`[标题行：标题 + 可选 ×] [主体] [底部：Cancel, Confirm]`。**冻结原语今天在标题行里无条件渲染一个 `aria-label="Close"` 的 `×`**（`ui/dialog/public.tsx:115–118`），而且它排在主体之前，因此是 `focusables(panel)[0]`。本文要的确认对话框**没有** `×`（`Escape` 与 `Cancel` 已是两条完备退出路径，第三个无标签逃生口还抢走第一个 Tab 停靠点）——这需要 §0.4 **CR-3**；**在 CR-3 落地之前，实现者按真实 DOM 写和测：焦点顺序以 `×` 开头。**
- **尺寸**：面板 `--paper` + `1px solid var(--hairline)` + `--radius-lg` + `--z-modal` + `--shadow-float`；`inline-size: min(var(--measure-form), calc(100vw - var(--space-12)))`；`padding: var(--space-8)`；底部 `gap: var(--space-4)`，`margin-block-start: var(--space-8)`，右对齐。顺序 `[Cancel] [Confirm]`，Confirm 在最后（LTR 下最右），是 primary，危险时是实心红。**底部永远只有两个按钮**——第三个**动作**按钮意味着这个对话框在问两个问题，应该拆成两步。（标题行的 `×` 不是动作按钮，见上；CR-3 之后它也不再存在于确认对话框里。）
- **状态**：进入 `--motion-medium`（opacity + `scale(0.98)`），退出瞬时——§2.6"无入场动画"的浮层例外之一。焦点被困住、关闭时归还、`Escape` 关闭；危险对话框的初始焦点在 Cancel 上（打字确认是唯一例外，落在输入框上，§6.13 + CR-1；CR-3 落地前实际是 `×`）。**执行中**：调用方把 `confirmState` 置为 `'busy'`（§0.4 CR-6），Confirm 于是拿到 `aria-busy` + `aria-disabled` + `data-nc-state="busy"` 而**不是**真 `disabled`，**保留实心红填充与 `--text-on-accent`**（§5.1 第 2 条）；标签原地换成 `Deleting…`，由 `confirmBusyLabel` 提供、宽度靠双标签同槽恒定（§5.1 第 4 条 + §0.4 CR-7）；**Cancel 始终可用**（它取消的是等待，不是已发出的请求）。**"若焦点在 Confirm 上则移到 Cancel"由 `ConfirmDialog` 自己做**（§0.4 CR-2），且只在进入 `'blocked'` 时需要——`'busy'` 不摘掉可聚焦性。§5.1 busy 规则唯一的 `disabled` 例外是 `'blocked'`，理由写在那里。**关闭之后的焦点落点**：普通关闭还给触发元素；**销毁成功**走 §0.4 CR-8（路由到领域父级 + 焦点到新页面的 `[data-nc-page-title]`）。
- **视觉规格住在哪**：面板的这些声明只能挂在冻结原语写死的八个全局类上（`dialog-panel` / `dialog-overlay` / `dialog-header` / `dialog-body` / `confirm-dialog-actions` …），它们**今天在全仓没有一行 CSS**——也就是说对话框今天是无样式渲染的。加上 CR-7 的 `confirm-dialog-label`，`styles/dialog.css` 一共定义九个类，manifest 一共十一项。落地方式与后果见 §0.4 **CR-4 / CR-7** 与 §0.2。
- **刻意不做**：叠加（一次一个；需要对话框的对话框是设计失败）；让背后页面滚动；超过 `--measure-form` 宽。

### 6.11 标签页 / 分段控件

一个组件，两种排布：标签页（分隔线在容器 block-end）与分段控件（分隔线在每段 block-end）。§8.4 说的"标签页矩阵"就是这一条。

- **anatomy**：`[段 1] [段 2] …`，每段是 `[可选 --glyph-sm 图标] [标签]`，带 `data-nc-role="tab"`（不带 `data-nc-action`，§4.1）。
- **两种无障碍语义，按它到底在做什么选，视觉完全相同**：**切换视图**时用 `role="tablist"` / `role="tab"` + `aria-selected`，并且**必须真有一个 tabpanel**；**选一个值**时用 `role="radiogroup"` / `role="radio"` + `aria-checked`。Settings 的 Appearance 是后者（现网 `settings/public.tsx:145` 的 `<div role="radiogroup" aria-label="Appearance">` 已经是 radiogroup，段本身在 `:150` 带 `role="radio"`，**保持不变**）——把一个没有 tabpanel 的单选组说成 tablist 是无障碍语义降级，不是换皮。
- **尺寸**：段 `block-size: var(--control-h)`，`padding-inline: var(--space-6)`，段之间不留沟（边界由指示条与色调表达）；容器 block-end 一条 `1px --hairline`；指示条 `--rule-h` `--accent`，**画在段的 block-end 边缘、压在容器 hairline 之上**，`inline-size` 等于该段宽度。
- **状态**：未选中 400 / `--text-2`，无指示条；hover `--overlay-hover`；选中 500 / `--text` + 指示条；focus-visible 用内嵌变体。**选中态不用 `--accent-soft` 填充**——那是列表选中专用（§2.1）。
- **刻意不做**：给段加圆角药丸底（那是消费级分段控件，会和"选中的行"撞脸）；指示条做位移动画；超过 5 段（超过就该是下拉）。

### 6.12 内联新建编辑器

§5.3"最重要的一条"依赖的组件：空态与"加一条"共用同一个东西。

- **anatomy**：一个占据**下一行位置**的输入框，无标签、无按钮；占位符是例子（`New wave…`）。
- **尺寸**：与它**所在那个列表**的行同高同宽同内边距——cove 页的 wave 列表 `--row-h-lg`(48)；**rail 的 COVES 列表 `--row-h`(28)**（它开在 cove 行的位置上，cove 行就是 28，§6.2）；rail 的 wave 列表 `--row-h-sm`(24)。旧版括号里写的 rail = 24 与"同高"自相矛盾，因为 rail 里的内联新建落在 COVES 列表的第一行位置。`--radius-sm`；`1px solid var(--hairline-strong)`；底 `--paper`。它不是浮层，不投阴影。
- **状态**：挂载即聚焦（**仅当它是区域级空态**）。`Enter` 提交；`Escape` 取消并把焦点还给触发元素；**`blur` 不提交也不销毁**——把内容留在原地，因为静默丢弃用户刚打的字是不可接受的（现状 `sidebar.tsx` 的 blur-提交要改，§11）。提交中按 §5.1 busy：输入框 `readonly` + `aria-busy`，不清空；失败时保持内容与焦点，下方出现一行 `--error-text` 消息，`Enter` 可重试。
- **刻意不做**：整页空态时自动聚焦（那会抢走屏幕阅读器用户刚听到的页面标题），且 refetch 后不重复聚焦；配一个"取消"按钮（`Escape` 就是取消）。

### 6.13 打字确认对话框

§4.3 确认阶梯的第三档，**全产品只有一处**：删除 cove。

- **anatomy**：`[标题：Delete <cove 名>?] [主体] [底部：Cancel, Delete cove]`。标题与文案是参数化的，`web/src/ui/confirm-dialog/copy.ts` 今天把它冻结成一个无参常量 `DELETE_COVE_COPY`（`INV-DUP-010`）——改法见 §0.4 **CR-5 / CR-5a**，那条不变量保留且更强。主体两行，字符串来自 `deleteCoveCopy()` 的 `consequence` 与 `prompt` 两个字段（§0.4 CR-5a），排版在这里：后果行 `This deletes N waves. This cannot be undone.`（`--text-sm` / 400 / `--text`，N=1 时是 `1 wave`），提示行 `Type <cove 名> to confirm.`（`--text-xs` / 400 / `--text-3`，cove 名用 `--font-mono` + `--text-2`），其下是输入框。
- **尺寸**：同 §6.10；输入框 `--control-h-lg`，通栏，`--font-mono`，无占位符（占位符会让人以为可以复制粘贴它）。
- **匹配规则**：`input.trim() === coveName.trim()`，**大小写敏感、不做 Unicode 归一化**——归一化会让"看起来一样"的两个串通过，而这道门的全部价值就是"你必须真的读了那个名字"。
- **状态**：Confirm 有三态，全部走 `ConfirmDialog` 的 `confirmState`（§0.4 CR-6）：不匹配 → `'blocked'`（真 `disabled`，灰芯片配方）；匹配且未提交 → `'ready'`（实心红）；提交中 → `'busy'`（保留实心红，标签换 `Deleting…`）。**优先级写死**：`pending ? 'busy' : (matches ? 'ready' : 'blocked')`。初始焦点在**输入框**上——全产品唯一一个初始焦点不在 Cancel 上的对话框，因为输入框本身就是安全动作（不匹配时按 Enter 什么也不发生）。**冻结原语把 `initialFocusRef` 写死成私有的 `cancelRef`（`:126–127`），`ConfirmDialogProps` 上没有任何入口，所以这一条今天写不出来**：它需要 §0.4 **CR-1**。**不匹配不渲染错误消息**（用户还在打字，不是出错了）；执行中同 §6.10。
- **焦点顺序**：CR-1 + CR-3 落地后是 `输入框 → Cancel → Delete cove`。**在那之前，真实的 Tab 序是 `× → 输入框 → Cancel → Delete cove`，初始焦点在 `×` 上**（`focusables(panel)[0]`）——照旧版那句去写测试会直接红。
- **刻意不做**：用在删除 wave 上（那走普通确认对话框）；提供"复制名字"按钮；模糊匹配。

### 6.14 三个小构件：Toast / `Details` 折叠 / 面包屑

- **Toast**：`[消息] [可选单个动作，tertiary]`；`min-block-size: var(--control-h-lg)`，`padding: var(--space-2) var(--space-4)`，`--radius-md`，`--paper` + `1px solid var(--hairline)` + `--shadow-float` + `--z-toast`；固定在主区 block-end + inline-end，距边 `--space-8`；出现 `--motion-quick`（只 opacity），停留 4s（带动作 8s），`role="status"`。**一次最多一个。刻意不做**：用于有位置可放的错误（§5.3）；堆叠；带进度条或关闭图标。
- **`Details` 折叠**：原生 `<details>`，`<summary>` = `[--glyph-sm chevron] [Details]`，`block-size: var(--row-h-sm)`，`--text-xs` / 400 / `--text-3`；内容 `--font-mono` / `--text-xs` / `--text-3`，`max-block-size: var(--slot-h)` + `overflow: auto`；chevron 旋转 `--motion-snappy`，**内容展开不做高度动画**。**刻意不做**：默认展开；出现在错误盒之外。
- **面包屑**：`[祖先] [分隔符] [当前]`，最多两级（领域祖先只有一层，§6.4）；`--row-h-sm` 槽，`gap: var(--space-2)`；分隔符是 `/` 字形、`--text-4`（不渲染意义的装饰字形，`--text-4` 的合法用途）；祖先是链接（400 / `--text-3`，hover `--text-2`），当前不是链接（500 / `--text-2`）。**注意**：§9 的"渲染字符串不含 `/`"门禁量的是文本节点，分隔符是独立元素且**必须登记在门禁豁免里**，否则面包屑一上线就判红。

### 6.15 日历 / 周历条

Today 面板列的顶部构件。**一周七列，宽度由面板决定，不由固定像素决定**——固定 48px 列会得到 7 × 48 = 336 > `--panel-w` 308，那个数字算不出来。

- **anatomy**：`[月份导航行：‹ August 2026 ›] [星期字母行] [日期格行]`。只有周条，没有月视图。**它不装在 §6.5 的卡片/面板里**——它直接坐在面板列的顶部，段标签就是它的月份行。这一条是下面那个 42px 算术的前提：装进面板就要再减一层 `padding: var(--space-4)`，每列掉到 40px。
- **尺寸**：月份导航行 `--row-h`，`‹` `›` 是 `--control-h` 图标按钮，月份文字 `--text-xs` / 500 / `--text-2` 居中。星期字母行与日期格行都是 `grid-template-columns: repeat(7, minmax(0, 1fr))` + `gap: var(--space-1)`（308px 面板里每列约 42px）。星期字母 `--text-xs` / 400 / `--text-3` 居中，**七个**：`M T W T F S S`（周一开始）。日期格 `block-size: var(--row-h)`（与 §2.5 的"日历格"一致），带 `data-nc-role="cell"`，数字 `--text-sm` / 400 / `--text-2`（今天 500），`--font-numeric` + `tabular-nums`，`--radius-sm`。当日点是格子 block-end 居中的 `--dot-sm`，带 `data-nc-day-dot`，颜色 = 该日第一条议程的 cove 身份色；**一天最多一个点**（两个数据源里的同一条 wave 按 id 去重，§8.1）；无议程则不渲染。
- **状态**：rest 透明；hover `--overlay-hover`；**选中日** `--accent-soft` + `1px solid var(--accent)` + 数字 500（`--accent-soft` 的第一个使用点：这就是列表选中）。**今天与选中日是两件事**：今天永远 500 字重，选中日永远带 accent；打开页面时两者重合，这是面板列里唯一的选中元素。
- **刻意不做**：月视图或年视图；给每天画事件条（当日点已经够了）；周起始日做成配置项（周一，写死）；在格子里显示事件数；渲染非本周的日期。

---
## 7. 页面框架

四条路由共用同一个框架。**框架必须无聊，内容才能被看清。**

### 7.1 外壳

| 属性 | 值 |
|---|---|
| 网格 | `grid-template-columns: var(--rail-w) minmax(0,1fr)` |
| 高度 | `block-size: 100dvh` —— **不是 `min-height`** |
| 滚动容器 | 恰好两个：rail 与主区；`<body>` 永不滚动 |
| 断点 | **960px（`60rem`）**，一个数字，一处声明：`styles/breakpoints.ts` 导出的 `RAIL_COLLAPSE_REM`（**该文件今天不存在，§0.1b 要新建**）+ 两处手写的 `@media (width < 60rem)` 注释指向它 |
| 窄于 960px | rail 折叠成 `--rail-w-collapsed` 44px 图标条，**不是隐藏**（现状 `shell.module.css` 的 `display: none` 要改，§11） |
| 窄于 640px | **本文不覆盖**。移动端是另一份规范，在它落地之前 640px 以下按 640px 渲染并允许横向滚动 |

`min-height: 100dvh` 会让整个 app 变成一份滚动文档：rail 滚走、页头无法吸顶、短页面在折线以下留下整屏灰地。900px 是完全正常的分屏宽度，那里导航不能凭空消失。

**折叠态取消 rail 的 inline 内边距**（`padding-inline: var(--space-0)`，展开态的 `--space-5` 只在展开态生效）：44 − 2×10 = 24px 装不下 28px 的品牌盒，内容宽度必须是整 44px，每个元素自己在其中居中。

**折叠态（44px 图标条）里显示什么**（穷尽）：品牌字形（`--control-h` 盒 28px，居中于 44）；"WAITING ON YOU" 折叠成一个带计数的 `--warn` 点，占一个 `--row-h` 槽；每个 cove 一个居中的身份点（`--dot-md`，`--row-h` 槽，当前 cove 取 `--accent-soft` 底）；底部账户字形。**段标签不渲染**（44px 放不下 11px 大写），**零行分区仍然不渲染**，wave 行不出现——折叠态是"我在哪"，不是"有什么"。**折叠优先级**：用户手动折叠/展开写进本地状态并始终优先；窄于 960px 的自动折叠**不写入**该状态，窗口变宽后恢复用户的选择。

### 7.2 页面

| 属性 | 值 |
|---|---|
| 页面网格 | `grid-template-rows: auto minmax(0, 1fr)` —— 页头行 + **撑满**的内容行 |
| 页面内边距 | inline `--space-10`(24) / block-start `--space-9`(20) / block-end `--space-11`(28) |
| 内容上限 | `--measure-page` 1180，**起始对齐**——有常驻左栏时居中会让内容与导航脱钩 |
| 内容网格 | `minmax(0,1fr)` + `--space-10` + `--panel-w`(308)，在有面板列的页面上；1440 下即 **848 / 24 / 308 = 1180** |
| **主列** | 内容网格的左列，1440 下 848。**§7.3 与 §9 的空矩形规则只量它** |
| 内部 measure | 列表 720 · 文档容器 748（正文 616 居其内）· 表单 544；**板与终端不封顶**（`--measure-board` 是舒适上限，§2.5） |
| 分区间距 | `--space-8`(16) |
| 行步距 | 28 或 48 + `gap: --space-1` → 30 / 50px，一个列表内不混用（原则 4） |

1440 下的完整算术：`--rail-w` 200 + 主区 1240 = 1440；主区 1240 − 24 − 24 = **1192** 可用，内容封顶 **1180**，起始对齐，右侧余下 12px 落在页面内边距里。**表单页的右侧留白是 1180 − 544 = 636**，不是 648。

**每个可能装省略号文本的 grid/flex 子项都要写 `min-inline-size: 0`**——这是长 wave 名撑爆 rail 的那个 bug。**给正文封顶、不给板封顶**：把正文的 measure 套到卡片板上是浪费宽屏最常见的方式。

### 7.3 纵向空洞：主列内不得出现高于 `--slot-h` 的连续空矩形

针对的是当前 build 的纵向空洞（内容在 900px 高的视口里 y=135 就结束了）。三条定义，缺一条这个门禁就无法实现：**量哪里** —— 主列，不是主区全宽，否则 1180px 内容里一个 544px 的表单必然违规、而唯一的"修法"是凭空发明内容（表单右侧的 636px 是**页面留白**，不是空洞）；**什么算空** —— 没有任何渲染内容的矩形，**带 `1px dashed` 边框与一行文字的未建成槽是内容，不计入**，它正在教用户将来会得到什么形状；**边界** —— `> --slot-h`（240px）判红，恰好 240 合法，终端槽与空报告槽都是 `--slot-h` 的整数倍，刻意如此。

配套的正向要求：**一个页面永远不能只有一个页头**。没有主要内容时，显示次相关的列表（最近的 wave、最近的活动），而不是空白。面板列没有内容时它**整列不渲染**，把空间还给主列，而不是留一根空列。

### 7.4 Rail 与主区怎么分开

| 属性 | 值 |
|---|---|
| 宽度 | 200 展开 / 44 折叠（当前 build 的 272px 比 legacy 宽 36%，内容却没多） |
| 内边距 | block-start `--space-3` / **inline `--space-5`（10px，只有这一层；折叠态取 `--space-0`，§7.1）** / block-end `--space-7` |
| 行间距 | `--space-1`；分区间距 `--space-6` |
| Surface | `--surface-rail`（改值后）+ inline-end 一条 `1px --hairline` |
| 滚动 | 自己的，整个 `100dvh` |

可用宽度算得出来：200 − 2×10 = **180**；cove 行内部 12（`--space-3` ×2）+ 20（chevron 盒）+ 8（身份点）+ 3×4（gap）= 52，名称与计数分 **128px**。分区**不再叠第二层 inline 内边距**（§6.1）：若分区自己再写 `padding-inline: var(--space-3)`（6px 两侧），可用宽 180 → 168，这个数字就掉到 **116**，同时 rail 行距边界从 10px 变成 16px，下面那条"不对称的沟"论证也就失效了。

边界由三件事共同承担，按各自做的功排序：**内容节奏**（rail 统一 11–12.5px、步距 24–28px，主区一开头就是 18px/600 的标题——两个区域是不同密度，这是最强也最便宜的信号）、**不对称的沟**（rail 行距边界 10px，主区内容距边界 24px，两侧都有文字、缩进不同的 34px 间隙读起来就是一条边）、**1px hairline**（装饰性、豁免，实测值只在 §2.1 给一次，**永远不是边界的唯一载体**）。Surface 明度差排最后：改值后 ΔL ≈ 2.4（light）/ 3.0（dark）。

**目标是知觉分离（ΔL ≥ 2），不是 3:1。** 任何一对相邻的近白或近黑灰都到不了 3:1，而 WCAG 1.4.11 不管装饰性的区域填充；把"rail 对 bg 需要 ≥3:1"当成要求去做，最后会得到一个看起来像另一个应用的侧栏。

### 7.5 Rail 内部的层级

| 层 | 元素 | 高 | 字号 | 字重 | 色调 |
|---|---|---|---|---|---|
| **P0** | 当前位置（活动的 cove 行或 wave 行） | — | — | **600**（与 §5.1 一致：selected 一律 600，不是"静止态 +1 档"） | `--text` on `--accent-soft` + `1px --accent` |
| P1 | "WAITING ON YOU" 分区的行 | 24 | 见 **§6.3** rail 变体的等待态 | | |
| P1 | cove 行 | 28 | 见 **§6.2** | | |
| P2 | "PINNED" 分区、cove 下的 wave 行 | 24 | 见 **§6.3** rail 变体的静止态 | | |
| P3 | 段标签 | 一个 24 槽 | `--text-xs` | 600 | `--text-3` |
| P3 | cove 身份点 | `--dot-md` | — | — | `--cove-1..8`，任何状态下不变 |
| P3 | 每个 cove 的 wave 计数 | — | `--text-xs` | 400 | `--text-3` |
| P3 | chevron / `×` / pin | 盒 `--control-h-sm`，字形 `--glyph` | — | — | `--text-3`，hover 显隐且预留空间 |
| P3 | 品牌、账户行 | `--control-h` | `--text-sm` | 500 | `--text-2` |

wave 行的三元组、行首点与等待态加重全部以 **§6.3 的变体表**为准，本表只是把它放进 rail 的上下文，**刻意不复述任何数字**——两处各写一个数就是下一次矛盾的来源。

**rail 只有一条强调规则**：只有当前位置能用 `--accent`，只有 "WAITING ON YOU" 能出现 `--warn`，其余全是灰阶。有三种彩色状态的侧栏是状态板，不是导航。分区顺序固定，pin 不等于移动位置，零行的分区不渲染。

### 7.6 抽屉（未建成）

对话界面将来的容器，规格先定死免得它落地时和面板列打架：**覆盖**在面板列之上（`position: fixed`，贴主区 inline-end，`--z-overlay`，`--shadow-float`，`--radius-lg` 只在 inline-start 两角），`inline-size: var(--drawer-w)`；**不挤压主列**——挤压会让文档在每次开合时重排，直接违反原则 3。进入 `--motion-medium` 的 `translate`，退出瞬时；`Escape` 关闭，焦点不困住（它不是模态）。

---

## 8. 四个页面

每页给：职责 → 两秒问题 → 尺寸速查 → 层级表（P0/P1/P2/P3）→ 线框 → 状态。

层级分档的含义：**P0** = 这页是关于什么的（每页恰好一个）；**P1** = 直接支撑 P0 决策的；**P2** = 上下文，扫读不细读；**P3** = 元信息，在场但退后。

**各页表里只有规范行。** 从当前 build 迁移的一次性删除动作全部集中在 §11。

---

### 8.1 `/` — Today

**职责**：用户打开 Today，是为了知道**此刻有没有 wave 在等他**，以及重新进入他上次在做的事。
**两秒之内必须回答**：*有东西在等我吗？* 如果没有，*我不在的时候发生了什么？*
**尺寸**：内容 1180（起始对齐）→ 主列 848 / 沟 24 / 面板列 308；行 28px（主列用 §6.3 紧凑变体，面板列议程用议程变体）；终端槽 `--slot-h` 240；**页头 32px**（`data-nc-header-rows="1"`：Today 是根，面包屑行整行省略；无机器标识，标识行省略——只剩标题行，§6.4）。

主列的行网格：`grid-template-rows: auto auto var(--slot-h) minmax(min-content, 1fr)`（等待 / 运行 / 终端槽 / 最近）。最后一行用 `minmax(min-content, 1fr)` 而不是 `1fr`：内容少时它吃掉剩余高度，内容多时它撑出主区的滚动条——两种行为都要，而 `1fr` 只给前一种。

| 层 | 项 | 字号 | 字重 | 色调 | 位置 | 理由 |
|---|---|---|---|---|---|---|
| **P0** | **"等你处理"的 wave 列表** | — | — | — | 主列，最上，紧贴页头 | 这页就是为它存在的。由**位置** + **全页唯一的 `--warn` 像素**承载，不由字号承载。**空则整节不渲染**。成员 = 生命周期是 `blocked`/`reviewing`/`failed`，**或** `anyCardNeedsInput` |
| P1 | 等待行的状态点 | `--dot-sm` | — | `--warn` | 行首 6px 列（紧凑变体） | 颜色不是唯一载体——标题同时取 `--warn-text` 与 500 字重（§6.3） |
| P1 | "正在运行"列表 | — | — | — | 主列，第二节 | 同一套行；点 `--accent` + `--motion-pulse`（全应用唯一的循环） |
| P1 | 等待 / 运行 计数 | `--text-sm` | 500 | `--text-2` | 页头标题行，标题之后 | 两个数字概括两个 P0 分区。数字用 `.tnum` 且 500；"waiting" 这个词保持 400 / `--text-3` |
| P1 | 选中日的议程行 | 见 §6.3 **议程变体** | | | 面板列，日历下方 | 同一个行原子——面板窄，第二行是**丢掉**而不是缩小；行首是 **cove 身份点**（这一列跨 cove），没有状态点、没有相对时间、没有生命周期短语 |
| P2 | 页面标题（星期 + 日期） | `--text-lg` | 600 | `--text` | 页头标题行起始 | 全页唯一的 18px 元素，四页写法完全相同 |
| P2 | 周历条 | 见 §6.15 | | | 面板列顶部 | 是导航不是内容。选中日是面板里唯一的选中元素 |
| P2 | "最近"列表（RECENT） | — | — | — | 主列最后 | 页面永远不能只有一个页头，而工作区永远有历史。紧凑行，无进度轨。**完整定义见下** |
| P2 | Today 终端区域 | — | — | — | 主列，"运行中"与"最近"之间 | `--slot-h` 240 的真实几何，未建成处理（§5.3） |
| **P3** | **时钟**（`4:05 PM`） | `--text-sm` | 400 | `--text-3` | 页头标题行，**推到右边缘** | **从 36px 降下来。** 环境信息不是可行动信息。位置是它的全部信号。`--font-numeric` + `tabular-nums`，分钟跳动不产生抖动。**不显示秒** |
| P3 | 段标签（"WAITING ON YOU" / "RUNNING" / "RECENT"） | `--text-xs` | 600 | `--text-3` | 每节之上 | 大写 + `--tracking-wider` 替代字号 |
| P3 | 周历条的月份年份 | `--text-xs` | 500 | `--text-2` | 面板列顶部 | 议程行**没有**生命周期短语——议程变体丢掉第二行（§6.3） |
| P3 | 议程行的 cove 身份点、日历格的当日点 | `--dot-sm` | — | `--cove-1..8` | 议程行行首 / 格子底部 | 身份，永不表示状态 |
| P3 | 议程行的小时标签（仅 `ScheduledEvent` 那一条） | `--text-xs` | 400 | `--text-3` | 标题之前，`.tnum` | 见下"议程的两个数据源" |

**RECENT 的完整定义**（旧版只说了"主列最后 / 紧凑行 / 无进度轨"，那不够写出来）：

| | |
|---|---|
| 数据源 | 与 WAITING / RUNNING 两节**同一份** wave 列表（页面从 props 收到的全量 wave），不另发请求 |
| 筛选 | `archivedAt === null`，且**排除已经出现在 WAITING 或 RUNNING 里的 wave**（同一条 wave 在一页里出现两次会让计数与扫读都失真） |
| 排序 | `updatedAt` 倒序（`core/domain/wave.ts` 已有这个字段） |
| 取几条 | **最多 12 条**。它回答的是"我不在的时候发生了什么"，不是归档浏览器；更长的历史在 cove 页。12 也是 §6.9 给菜单的那个数字，同一个理由：再多就必须滚动，而要滚动的列表不可扫读 |
| 空 | 整节不渲染（与 WAITING / RUNNING 同一条规则）。**在任何用过的工作区里它都不空**，这就是它作为兜底的全部含义 |
| "吸收剩余高度" | 指的是它那一条 grid track 拿走剩余空间（§8.1 的行网格），**不是**要求内容填满——不足 12 条时下面就是空的，那是页边距不是空洞（§7.3 只量连续空矩形，而 RECENT 的最后一行之后就是页面 block-end 内边距） |

**议程的两个数据源 —— 不能在重写里丢掉的缝隙。** 面板列的议程由**两个**来源合并渲染，且必须共存：`ScheduledEvent[]`（按小时分桶，`TodayPageProps.scheduledEvents`，生产永远传空数组）与 wave 活动（`activeWavesOn`）。这条缝隙由 `INV-TODAY-002` 锁着（`features/today/public.tsx:14–30` 的注释 + `features/today/public.contract.test.tsx` 四条断言），**重写不得删除这条分支**——调度插件落地时它就是接入点，删掉它就是静默删掉缝隙。五条派生规格：`scheduledEvents` 保持带空数组默认值的可选 prop（不是组件内写死的 `[]`）；两个来源渲染成**同一个** §6.3 议程变体，`ScheduledEvent` 那一条多一个小时标签槽；排序 = 先按小时升序的 scheduled，再按 wave 活动；日历格的当日点按 **wave id 去重**（同一条 wave 同时出现在两个来源里只贡献一个点，§6.15）；**两个来源都空**时才渲染空态。

**通道审计。** 全页唯一叠了三个通道的元素是主列的等待行：字重 + 标题色 + 点色，具体取值见 §6.3 的变体表。三个通道、无字号、无边框——合法，且理由充分：它是 P0，必须在余光里被找到。主列也是全页唯一允许出现 `--warn` 的地方（面板列的议程行没有状态点）。

```
0        200                                                               1440
├─ rail ─┼──────────────────── main (1240) ───────────────────────────────┤
│        │←24→│                                                      │←24→│
│        │  ┌ content 1180（起始对齐，--measure-page）──────────────────┐
│        │  │ 主列 848                         │ 24 │ 面板列 308         │
│        │  ├──────────────────────────────────┤    ├───────────────────┤
│        │  │ Monday, 10 Aug  2 waiting · 1 running        4:05 PM ← 页头 32px（仅标题行）
│        │  ├─────────────────────────────────────────────────────────────┤ ← 滚动时才有 hairline
│  ┌───┐ │  │                                  │    │ ‹ August 2026    › │
│  │Wait│ │  │ WAITING ON YOU                   │    │ M  T  W  T  F  S S │
│  │ing │ │  │ ● 引用方：本轮修复          2h   │28px│10 11 12 13 14 15 16│ 28px 格
│  │ ▪ │ │  │ ● 被引用方：估值结论       40m   │28px│  ··                │
│  └───┘ │  │   ↑ 紧凑变体：状态点 --warn，     │    │                    │
│  COVES │  │     标题 500 / --warn-text，单行， │    │ TODAY              │
│   ▸ ▪  │  │     无生命周期短语（§6.3）        │    │ ▪ 引用方：本轮修复 │ 28px
│   ▸ ▪  │  │                                  │    │ ▪ 被引用方：估值   │ 28px
│        │  │ RUNNING                          │    │   ↑ 议程变体：行首是 │
│        │  │ （无 —— 整节不渲染）             │    │   cove 身份点，无时间│
│        │  │                                  │    │                    │
│        │  │ ┌ ~ / neige · today ───────────┐ │    │                    │
│        │  │ │  Terminal is not wired up yet.│ │    │ （面板列到此为止， │
│        │  │ │   （虚线，--text-3）    240px │ │    │   下面不填充）     │
│        │  │ └──────────────────────────────┘ │    │                    │
│        │  │                                  │    │                    │
│        │  │ RECENT                           │    │                    │
│        │  │ ▪ …                         28px │    │                    │
│        │  │ ▪ …  （撑到 block-end 边缘）     │    │                    │
│        │  └──────────────────────────────────┘    └───────────────────┘
│  ┌──┐  │                                                  ↓ 28px 底部内边距
└──┴──┴──┴───────────────────────────────────────────────────────────────┘
```

**空白去哪了。** 不是去底部那 764px 的空洞。纵向顺序是**决策在前、环境在后**：两个注意力分区要多高给多高，终端占固定 240px 槽，**"最近"吸收全部剩余高度**。

| 状态 | 处理 |
|---|---|
| 加载 | 页头立刻渲染（标题和时钟不需要请求）；每节按 §5.3 的加载表处理 |
| 空 —— 没有等待的 | "WAITING ON YOU" **整节不渲染**——没有标签、没有虚线盒。缺席本身就是消息。"RUNNING" 同理。页面于是从 "RECENT" 开始，而它在任何用过的工作区里都不空 |
| 空 —— 全新工作区 | 整页空态：一行 hero `Nothing here yet.` + **一个** primary 动作 `New cove`，同时 rail 里的内联新建 cove 输入已展开（**不自动聚焦**，§6.12）。无插图 |
| 空 —— 议程 | 面板列内内联变体：一行 **`Nothing scheduled.`**，`--row-h` 虚线盒。面板列**不收起**，因为它上面的日历永远不空。**这个字符串是照抄现网的，不是新写的**：`features/today/public.contract.test.tsx:31/49` 用 `INV-TODAY-002` 把它钉死，只有"两个数据源都空"时才出现。旧版写的 `Nothing today.` 会直接判红那两条断言，而它换不来任何东西——`Nothing scheduled.` 同样满足 §5.3 的空态文案要求（一句话、`--text-3`、不道歉） |
| 错误 | 区域级（§5.3）。日历挂掉不会让 wave 分区变空 |
| 未建成 —— 终端 | 真实几何 + 虚线 + 一行 `Terminal is not wired up yet.` |

**Today 刻意不显示**

- 完整的 cove/wave 树 —— 在 rail 里，每页都有；在 Today 上重复会让 rail 变成装饰。
- wave 的 cwd / id / branch —— 在 wave 页的标识行。
- 每行一个生命周期 pill —— 只有 6px 点 + 色调。
- 月视图 —— 只有周条。
- 每 cove 的统计/图表/连续天数 —— 哪里都不放。这是工作台不是仪表盘。

---
### 8.2 `/cove/$coveId` — 一个 cove

**职责**：挑出需要的那条 wave，或者新开一条。
**两秒问题**：*这个 cove 的哪条在动、哪条卡住了、在哪儿加一条？*
**尺寸**：内容 1180（起始对齐）→ 主列 848 / 沟 24 / 面板列 308；列表封顶 `--measure-list` 720；行 48px（默认变体）；**页头 62px**（`data-nc-header-rows="2"`：cove 无领域祖先 → 无面包屑行；标题行 + cwd 标识行，§6.4）。

| 层 | 项 | 字号 | 字重 | 色调 | 位置 | 理由 |
|---|---|---|---|---|---|---|
| **P0** | **wave 列表** | — | — | — | 主列，紧贴页头，封顶 `--measure-list` | 这页就是一个列表。其余全是它的标签 |
| P1 | 行第二行：生命周期短语 | `--text-xs` | 400 | `--text-3`，等待时 `--warn-text` | 行第二行起始 | **由生命周期承载，不由 `now` 承载**——`now` 在生产中永久为空 |
| P1 | 状态点 | `--dot-sm` | — | 等待 `--warn` / running `--accent` / 其它 `--text-4` | 行首 6px 列 | 点不渲染文字，所以这里用 `--text-4` 合法 |
| P1 | `+ New wave` | `--text-sm` | 400 | `--text-on-accent` | 页头标题行右侧 | 本页唯一主操作，`data-nc-action="primary"`，实心 `--accent`，`--control-h-lg` |
| P2 | cove 名（页面标题，可就地编辑） | `--text-lg` | 600 | `--text` | 页头标题行起始 | 从 22px 降到 18px：22/13 = 1.7× 读起来像落地页，18/13 = 1.38× 是密集工具的比例 |
| P2 | 相对时间 | `--text-xs` | 400 | `--text-3`，`.tnum` | 行第一行右端固定列 | 右边缘 = 状态 |
| P3 | cove 身份点 | `--dot-md` | — | `--cove-1..8` | 页头，标题之前 | 比行内的点大一号，因为它是这一页的**主题**而不是行标记 |
| P3 | wave 计数（`2 waves`） | `--text-xs` | 400 | `--text-3` | 标题之后、弹簧之前 | 它是计数，不是小标题 |
| P3 | `Delete` cove | `--text-sm` | 400 | **`--error-text`** | 页头最右，与 `+ New wave` 之间隔 `--space-6` | `data-nc-action="destructive"`。静止即着色（§4.3），只是文字红。走**打字确认**（§6.13）——全产品唯一一处 |
| P3 | 每行的 pin / remove | 盒 `--control-h-sm` | — | `--text-3` | 行右端，hover 显隐、静止预留 44px | `:focus-within` 时必须出现；已 pin 的行其 pin 永久可见 |
| P3 | cove 的 cwd / 创建时间 / 生命周期分布 | `--text-xs` | 400（数字 500） | `--text-3` | 面板列 | 查阅型上下文，不是扫读型 |

**面包屑行不渲染**：cove 没有领域祖先，它在哪由 rail 表示（§6.4 的规则，不是本页的选择）。**行内不放 cove 身份点**：这个列表里每一行的 cove 都相同，页头的身份点已经说明了。**全应用只有 Today 面板列的议程行在行首放身份点**（§6.3 的议程变体），因为只有那一列跨 cove 且不按状态分节；rail 里的 wave 行靠嵌在自己 cove 行下面表达归属，行首那一列留给状态点。

```
0        200                                                            1440
├─ rail ─┼───────────────────── main (1240) ────────────────────────────┤
│        │  ┌ content 1180 ────────────────────────────────────────────┐
│        │  │ 主列 848                          │ 24 │ 面板列 308       │
│        │  ├───────────────────────────────────┤    ├─────────────────┤
│        │  │ ▪ 双链演示  2 waves  [+ New wave]  Delete   ← 标题行 32px
│        │  │ /tmp/demo-b                    （mono）    ← 标识行 24px
│        │  ├──────────────────────────────────────────────────────────┤
│        │  │ ┌ 列表 --measure-list 720 ──────┐ │    │ COVE            │
│        │  │ │ ● 被引用方：估值结论     40m │ │48px│ waves        2  │ 28px
│        │  │ │   reviewing                   │ │    │ working      0  │ 28px
│        │  │ ├───────────────────────────────┤ │    │ blocked      2  │ 28px
│        │  │ │ ● 引用方：本轮修复       10d │ │48px│                 │
│        │  │ │   draft                       │ │    │ CWD             │
│        │  │ └───────────────────────────────┘ │    │ /tmp/demo-b     │
│        │  │  ↑ 720 measure：生命周期落在标题 │    │                 │
│        │  │    右侧 720px 处，不是 1144px     │    │ （面板列到此为止）│
│        │  └───────────────────────────────────┘    └─────────────────┘
└────────┴──────────────────────────────────────────────────────────────┘
```

**那个横向的洞，用一个数字修好。** 当前 build 的 wave 行宽是 **1144px**，算得出来：视口 1440 − rail `17rem`=272（`shell.module.css:7`）− 页面 `padding: var(--space-6)` 两侧 24（`cove/page/page.module.css:6`）。行里再用 `margin-inline-start: auto` 把生命周期标签推到那 1144 的远端，于是标题和它自己的状态之间隔着接近一整行宽的空白。**修法**：行封顶在 `--measure-list` 720px，生命周期回到第二行起始（§6.3）。主列 848 而列表 720，多出的 128px 正是 hover 动作的落脚处，也是标题在 720 处省略后"不去的地方"。

| 状态 | 处理 |
|---|---|
| 加载 | 页头用 rail 缓存的 cove 立刻渲染；列表按 §5.3 的加载表处理 |
| 空 —— 没有 wave | **内联新建编辑器（§6.12）已展开、已聚焦，占据第一行的位置与尺寸。** 不是虚线盒加一个页头按钮。页头的 `+ New wave` 保留（那是加第二条的入口），但它不是空态 |
| 错误 —— cove 加载失败 | 页面级（整页确实失败了）：`--measure-list` 宽的错误盒（`--error-soft`）+ tertiary `Retry` |
| 错误 —— 某次修改失败 | 就地出现在失败的那个控件旁；确认对话框保持挂载且 Cancel 可用 |
| 未建成 | 本页没有未建成的东西。面板列的生命周期分布如果不做，**面板整列不渲染**，主列保持 848 宽。**没做的区域是缺席，不是一个贴了标签的空盒子** |

**刻意不显示**

- wave 的卡片/终端 —— 在 wave 页；cove 是索引。
- cove 身份色铺到行/页头/背景上 —— 身份点已经够了。
- 归档的 wave —— 将来是这个列表上的过滤器，不是第二个列表。
- 每行一个"打开"按钮 —— 行本身就是目标（§6.0）。

---

### 8.3 `/wave/$waveId` — 一条 wave

**职责**：看 agent 做了什么、正在做什么，并把它解开。
**两秒问题**：*这条 wave 处于什么状态，它要我做什么吗？*
**尺寸**：内容 1180（起始对齐）→ 主列 848 / 沟 24 / 面板列 308；文档容器 `--measure-doc` 748，正文 `--measure-prose` 616；面板行 28px；**页头 92px**（`data-nc-header-rows="3"`：三行俱全——四页里只有这一页是 92，§6.4）。

这是产品真正活着的那一页，也是重写里最缺的一页。卡片运行时是后面的一个 slice——这是一个要**诚实地渲染出来**的事实，不是一段写在正文里的旁白。

| 层 | 项 | 字号 | 字重 | 色调 | 位置 | 理由 |
|---|---|---|---|---|---|---|
| **P0** | **wave 主体** —— 有报告时是报告文档，否则是卡片板 | 文档角色 | — | `--text` | 主列，748 容器内 616 正文 | 这才是 wave**是什么**。卡片运行时落地之前，这个槽按真实几何渲染未建成处理，让文档的形状先可见 |
| P1 | wave 标题（可就地编辑） | `--text-lg` | 600 | `--text` | 页头标题行起始 | 全页唯一的 18px 元素（文档 H1 是 15px，§2.2） |
| P1 | 生命周期徽章 | 见 §6.6 | | | 页头标题行，紧跟标题 | 状态是**另一类**东西，由形状（pill + 6px 点）+ 语义色承载，**永远不由字号承载**——所以它能挨着 18px 标题而不竞争。这是全页唯一带填充的元素 |
| P1 | "有卡片在等你"标记 | `--dot-sm` + 徽章 | 500 | `--warn-text` | 生命周期徽章之后 | `any_card_needs_input` 是内核在写的可靠字段 |
| P1 | 卡片清单行（§6.0 通用行） | `--text-sm` | 400 | `--text` | 面板列，`--row-h` | 从通栏主列里搬出来。卡片是你打开的对象，不是一个段落 |
| P2 | 面包屑（祖先 `Today` / 当前 cove 名 + 身份点） | 见 §6.14 | | | 页头面包屑行 | "你在这里"只加一档字重 |
| P2 | 事件/近期活动 | `--text-xs` | 400 | `--text-3` | 面板列，卡片之下 | 实时更新的文本，永不做入场动画 |
| P3 | 返回控件 | 盒 `--control-h`，字形 `--glyph` | — | `--text-3` | 页头面包屑行最前 | 是手势不是内容，最前的位置就是它的全部信号 |
| P3 | `cwd` | `--text-xs` | 400 | `--text-3` | 页头标识行起始，**mono** | 字体本身宣告"这是字面量"，不花字号/字重/颜色 |
| P3 | `kernel-owned` | `--text-xs` | 400 | `--text-3` | 卡片行右端 | |
| P3 | `Delete` wave | `--text-sm` | 400 | `--error-text` | 页头标题行最右，隔 `--space-6` | 与 cove 页的 Delete 视觉相同，但只走**普通**确认对话框（§6.10）——删一条 wave 不是灾难性操作 |
| P3 | 段标签（`CARDS` / `ACTIVITY`） | `--text-xs` | 600 | `--text-3` | 每节之上 | |

**通道审计 —— 生命周期徽章。** 它叠了字号 + 填充 + 边框 + 文字色 + 形状，是全设计里装饰最多的元素，也是 §3.2 那份四项封闭清单里的第三项。理由写明：它是全页唯一一个职责就是"被余光预注意地读到"的元素。它**必须显式设 `--text-xs`**（§6.6：不设就继承 18px 标题行，直接判红"每页恰好一个 18px 元素"），而它不触"四通道叠加"的机检内核，是因为**字重是 500 不是 ≥600**。本页其它任何元素都不得带填充。

```
0        200                                                            1440
├─ rail ─┼───────────────────── main (1240) ────────────────────────────┤
│        │  ┌ content 1180 ────────────────────────────────────────────┐
│        │  │ 主列 848                          │ 24 │ 面板列 308       │
│        │  ├───────────────────────────────────┤    ├─────────────────┤
│        │  │ ← Today / ▪双链演示                     ← 面包屑行 24px
│        │  │ 被引用方：估值结论  (● Reviewing)   Delete ← 标题行 32px
│        │  │ /tmp/demo-b                     （mono）  ← 标识行 24px
│        │  ├──────────────────────────────────────────────────────────┤
│        │  │ ┌ 文档容器 748 ─────────────────┐ │    │ CARDS           │
│        │  │ │ ┌ 正文 measure 616 ────────┐ │ │    │ ▫ wave-report   │ 28px
│        │  │ │ │  ┌ 虚线，未建成 ───────┐ │ │ │    │ ▫ codex         │ 28px
│        │  │ │ │  │                     │ │ │ │    │                 │
│        │  │ │ │  │   No report yet.    │ │ │ │    │ ACTIVITY        │
│        │  │ │ │  │                     │ │ │ │    │ Nothing yet.    │
│        │  │ │ │  │  （文档的形状，空） │ │ │ │    │  （--text-3）   │
│        │  │ │ │  │      480 = 2×--slot-h │ │ │    │                 │
│        │  │ │ │  └─────────────────────┘ │ │ │    │                 │
│        │  │ │ └──────────────────────────┘ │ │    │                 │
│        │  │ └──────────────────────────────┘ │    │                 │
│        │  └──────────────────────────────────┘    └─────────────────┘
└────────┴──────────────────────────────────────────────────────────────┘
```

**空白**：文档槽吃满主列高度——一份空文档仍然占据一份文档的空间，因为那正是它教给用户的形状。它是带虚线与一行文字的未建成槽，按 §7.3 定义 2 **不计入空矩形**。面板列短、到此为止；矮面板挨着高文档不是空洞，是页边距。

| 状态 | 处理 |
|---|---|
| 加载 | 页头用 rail 缓存的 wave（标题、生命周期、cove）无闪烁渲染；文档槽按 §5.3 的加载表处理 |
| 空 —— 没有卡片 | 板的幽灵几何：按板的真实格子尺寸铺卡片种类瓦片，`1px dashed`，点一个就在那个槽里创建。板本身落地之前，用未建成处理覆盖 |
| 空 —— 没有报告 | 报告面（`--paper`，616）空着渲染，连同它的编辑入口 |
| 空 —— 活动 | 内联：`Nothing yet.`，`--row-h` 虚线盒 |
| 错误 | 区域级。卡片拉取失败只在面板列里显示错误盒，页头和文档保持；wave 本身拉取失败才是页面级 |
| 未建成 | **恰好一次**，在 P0 槽里：`No report yet.`；板落地而运行时未落地时是 `Cards are not wired up yet.` |

**刻意不显示**

- 这个 cove 的其它 wave —— 在 rail 和 cove 页。
- 原始 id / plugin id / overlay 载荷 —— chrome 里哪儿都不放，只在错误的 `Details` 折叠里（§6.14）。
- 第二个主操作 —— **这页没有主操作**，零个合法且常见：建卡片是板上的手势，重命名是就地的，删除是危险-三级。
- 动画进度条 —— §6.3。
- 对话 —— 将来落在 396px 抽屉里（§7.6），**不是**在主列里跟文档抢位置。

---

### 8.4 `/settings`

**职责**：改一个偏好，并确认它生效了。
**两秒问题**：*我要改的那一项在哪？*
**尺寸**：内容 1180（起始对齐）→ 表单 `--measure-form` 544，**无面板列**；右侧 1180 − 544 = 636 是页面留白；控件 28px；**页头 62px**（`data-nc-header-rows="2"`：面包屑行 + 标题行，无机器标识行。少一行也少一个行间 `gap`：24 + 6 + 32 = 62，不是 92 − 24 = 68）。

没有人"浏览"设置。这一页只需要快速定位和无歧义的确认。

| 层 | 项 | 字号 | 字重 | 色调 | 位置 | 理由 |
|---|---|---|---|---|---|---|
| **P0** | **用户来找的那组字段** —— 表单本身 | — | — | — | 主列，`--measure-form` 544，起始对齐 | 这页是一个表单，没有别的 |
| P1 | 字段标签 | `--text-xs` | 500 | `--text-2` | 输入框之上 | |
| P1 | 字段值 | `--text-sm` | 400 | `--text` | `--control-h` 输入框内，底 **`--paper`** | 选中的值是内容。`--paper` 是仅有的两个方向稳定的 surface 之一 |
| P1 | `Save` | `--text-sm` | 400 | `--text-on-accent` | 动作行第一个 | 本页唯一主操作 |
| P1 | 外观分段控件 | 见 §6.11 | | | Appearance 节 | 选中段拿字重 + 色调 + `--rule-h` `--accent` 指示条，**不是** `--accent-soft` 填充。语义保持 `role="radiogroup"` / `role="radio"` + `aria-checked`（它选的是一个值，不切换视图；现网已经是这样，不要改成 tablist——§6.11） |
| P2 | 页面标题 `Settings` | `--text-lg` | 600 | `--text` | 页头标题行 | 全页唯一的 18px 元素 |
| P2 | 段标签 `NETWORK` / `APPEARANCE` / `ABOUT` | `--text-xs` | 600 | `--text-3` | 每组之上，上 `--space-8` / 下 `--space-4` | **这些标签取代卡片盒。** 按边界决策阶梯第一步：一个标签加 16px 间距，分隔两组的效果与"边框 + 圆角 + 底色 + 内边距"一样好，用一个通道而不是四个 |
| P3 | 面包屑 | 见 §6.14 | | | 页头面包屑行 | 与 wave 页完全相同 |
| P3 | `Reset` | `--text-sm` | 400 | `--text` on `--surface-chip` | `Save` 之后 | secondary：它必须不用读就能找到，而且它挨着一个 primary |
| P3 | 字段提示 | `--text-xs` | 400 | `--text-3` | 组下方 | |
| P3 | `Saved.` | `--text-xs` | 500 | `--success-text` | 动作行，`Reset` 之后 | 全应用唯一的绿像素，存在 4 秒 |
| P3 | About：version · build | `--text-xs` | 400 | `--text-3` | 最后一节 | 真实信息，而这恰恰是人们真的来设置页看的东西。**数据源见 §0.5**：两个构建期 `define`（`__NC_VERSION__` / `__NC_BUILD__`），不是接口字段——`wire.ts` 里没有它们。**`data dir` 这一行今天不渲染**：它是内核拥有的信息且没有 wire 字段，按"未建成 = 缺席"整行省略 |

**两种"不能按"要分清**（§5.1），而且它们**看起来必须不一样**：

- **表单干净** → `Save` 与 `Reset` 都是**真 disabled**：`color: var(--text-4)` + `--surface-chip` 填充 + 真 `disabled` 属性，**绝不是 `opacity: .5`**。这是 `--text-4` 作为文字色唯一被认可的场合。
- **保存中** → `Save` 与 `Reset` 都是 **busy**：`aria-busy` + `aria-disabled` + `data-nc-state="busy"`，保持可聚焦，**保留各自静止态的填充与边框**，文字降到 `--text-3`，标签换成 `Saving…`（`Reset` 的标签不变，它没有进行时）。`Save` 必须 busy 而不是真 disabled，因为焦点此刻就在它身上，真 `disabled` 会把焦点扔掉；**`Reset` 跟着 `Save` 走**——同一个动作行里两个按钮取两种"不能按"，会被读成两件不同的事。

```
0        200                                                            1440
├─ rail ─┼───────────────────── main (1240) ────────────────────────────┤
│        │  ┌ content 1180（起始对齐）────────────────────────────────┐
│        │  │ Today / Settings                        ← 面包屑行 24px  │
│        │  │ Settings                                ← 标题行   32px  │
│        │  │                                   （--header-h = 62）    │
│        │  ├──────────────────────────────────────────────────────────┤
│        │  │ ┌ --measure-form 544 ──────────┐                         │
│        │  │ │ NETWORK                      │  ← 右侧 636px 是页面    │
│        │  │ │ HTTP proxy                   │    留白，不是空洞：表单  │
│        │  │ │ [                         ]  │28px 封顶 544，第二列会是 │
│        │  │ │ HTTPS proxy                  │    凭空发明的工作        │
│        │  │ │ [                         ]  │28px                      │
│        │  │ │ [Save] [Reset]   Saved.      │28px                      │
│        │  │ │                              │                          │
│        │  │ │ APPEARANCE                   │  ← 节间 --space-8       │
│        │  │ │ ( Light │ Dark │ System )    │28px  无卡片盒            │
│        │  │ │ Stored on this device only.  │                          │
│        │  │ │                              │                          │
│        │  │ │ ABOUT                        │                          │
│        │  │ │ version   0.x.y              │  ← __NC_VERSION__        │
│        │  │ │ build     abc1234            │  ← __NC_BUILD__          │
│        │  │ │ （data dir 无数据源，整行不渲染，§0.5）                 │
│        │  │ └──────────────────────────────┘                          │
│        │  └──────────────────────────────────────────────────────────┘
└────────┴──────────────────────────────────────────────────────────────┘
```

| 状态 | 处理 |
|---|---|
| 加载 | 段标签先渲染；每组按 §5.3 的加载表处理。表单**不**先空着渲染再填——那是在用户马上要打字的地方做布局跳变 |
| 保存中 | `Save` 与 `Reset` 一起走 busy（§5.1 四条）：`Save` 标签原地换成 `Saving…`，宽度靠双标签同槽恒定；无转圈、无改宽 |
| 已保存 | `Saved.` 用 `--success-text`，`role="status"`，4 秒 |
| 空 | 不适用——表单不会空。整节数据不可用时，该节渲染标签加一行错误，不是虚线盒 |
| 错误 —— 加载 | 就地出现在受影响的那一节顶部。不是整页横幅——Network 挂了 Appearance 还能用 |
| 错误 —— 保存 | 同一个错误盒，直接放在动作行下面，紧邻产生它的控件 |
| 未建成 | 什么都不放。某组设置没实现，**这组就不存在**。一个列出自己兑现不了的分节的设置页，比一个短的设置页更糟 |

**刻意不显示**

- 主题预览色板 —— 选中时整个 app 立刻重绘，那就是预览。
- 每项设置的 "learn more" 链接。
- 账户/会话管理 —— 在 rail 的账户菜单里。
- 破坏性的工作区操作 —— 还没做；落地时单独成 `DANGER` 节放在最后，上方 `--space-12`，配确认对话框（§4.3）。

---
## 9. 什么由机器保证，什么不是

复用现有的四层闸门设施（stylelint 插件 / node CSS-AST 审计 / ESLint 插件 / vitest browser tier + mutation runner），不另起一套。

| 检查 | 层 | 什么必须红 | 什么必须保持绿 |
|---|---|---|---|
| **受管属性**（§9.1）只能用 token，禁原始 px/rem/hex | stylelint | `padding: 7px` | `padding: var(--space-4)`；`inline-size: 100%`；`minmax(0,1fr)`；`calc(var(--control-h-sm) * 2 + var(--space-2))`；媒体查询 |
| TSX 内联样式里的原始值 | ESLint | `style={{ gap: '7px' }}` | `style={{ gap: 'var(--space-4)' }}`；`style={{ '--cove-color': 'var(--cove-3)' }}` 这类自定义属性数据通道 |
| 阶梯归属：`font-size` 只取 `--text-*`、间距只取 `--space-*`、圆角只取 `--radius-*` | stylelint | `font-size: var(--space-6)` | `font-size: inherit` |
| 只有 400/500/600 三种字重 | stylelint | `font-weight: 700` | `font-weight: var(--weight-medium)` |
| `--text-4` 只出现在真 `:disabled` / 不渲染文字的元素上 | node 审计 | `.hint { color: var(--text-4) }` | `.dot { background: var(--text-4) }`；`button:disabled { color: var(--text-4) }`；面包屑分隔符字形 |
| `outline: none` 必须同文件同选择器有替代焦点表现 | stylelint | 只有 `outline: none` | 有替代；或文件里根本没有 `outline: none`（无关文件必须静默） |
| 每个**可聚焦**类同时定义 `:hover` 与 `:focus-visible` | node 审计（CSS × TSX join） | `<button className={s.row}>` 且只有 `:hover` | 纯装饰 `<div>` 的 hover；hover 时改子元素颜色的规则；焦点态由 `composes` 的共享类提供 |
| 每个列表行组件都写了 `min-block-size: var(--row-h*)` | node 审计 + browser 测量 | 无行高规则 | 三个 token 之一，实测在 ±1px 内 |
| 每个 `<button>` 带 `data-nc-action` **或** `data-nc-role`，**恰好一个** | node 审计 | 两个都缺；两个都有；写成 `data-action` / `data-variant`（`auditDataAttributes` 会先判红，见 §0.3） | `data-nc-action` 取四档之一（动作按钮）；`data-nc-role` 取 `row/icon/menu-item/tab/cell` 之一（组件按钮，几何由它的 §6 条目定，§4.1） |
| 四档**几何一致**只量 `[data-nc-action]` | stylelint | `[data-nc-action] { block-size: var(--row-h) }` | 行、图标按钮、菜单项、段、日历格各自的几何（它们是 `[data-nc-role]`） |
| busy 控件不穿 disabled 的皮 | node 审计 | `[data-nc-state="busy"] { color: var(--text-4) }`；busy 上出现真 `disabled` 属性（**CR-6 之后没有例外**——`ConfirmDialog` 的真 `disabled` 只属于 `confirmState='blocked'`，那一态不带 `data-nc-state`） | 透明档 `[data-nc-state="busy"] { color: var(--text-3) }`；实心档保持 `--text-on-accent`；两者都带 `aria-busy` + `aria-disabled`；`confirmState='blocked'` 的真 `disabled` + `--text-4` |
| busy 前后按钮宽度**相等** | browser | 点击后 `getBoundingClientRect().width` 变化 | 差值 0（双标签同槽，§5.1 第 4 条） |
| 每 surface ≤ 1 个 `[data-nc-action="primary"]` | browser | 两个 | 零个（合法且常见）；四条路由各自的那一个 |
| `--accent` 实心填充只出现在 primary 上；`--accent-soft` 不做按钮填充 | stylelint | `.submit { background: var(--accent-soft) }` | 行选中态、图标按钮开启态、菜单项勾选、输入框焦点环这四处用 `--accent-soft` |
| `--warn-*` 与 `--error-*` 不交叉 | stylelint | `.errorBox { background: var(--warn-soft) }` | 等待盒用 `--warn-soft/-border`；错误盒用 `--error-soft/-border` |
| 每页恰好一个 `data-nc-page-title` / 一个 18px 元素 | browser | 第二个 18px 元素 | 一个；`.calm-prose` 里的 H1 是 15px 所以不参与 |
| chrome 模块**显式设置**的 `font-size` 只有 `--text-xs` / `--text-sm` / `--text-lg` | node 审计（跨 feature + ui 模块聚合） | chrome 模块里出现 `--text-md` / `--text-xl` / `--text-display*` | 这三种；**不设 `font-size`（继承 13）合法**；`.calm-prose` 与终端模块登记为取景窗，豁免 |
| duration 只取 `--motion-*`；含动效的文件必须有 reduced-motion 块 | stylelint | `transition: opacity 180ms ease` | `var(--motion-snappy)`；`transition: none`；完全无动效的文件 |
| reduced-motion **真的生效** | browser（`emulateMedia`） | reduce 下测得 0.24s | 0s |
| 对比度地板，两个主题，遍历 surface registry | browser | dark 下 `--text-3` on `--surface-card` 测得 3.9 | 4.51；登记为装饰性非文本的 hairline |
| 焦点环**真的画出来**且 ≥3:1 | browser | Tab 后 `outlineWidth === '0px'` | `2px` 且 3.2:1；由 `box-shadow` 提供的输入框变体也必须接受 |
| 命中区 ≥ 24×24（或 24px 间距豁免） | browser | 20×20 独立按钮（分区标签行的尾部动作必须 28px） | 行内 20px 图标按钮，周围 24px 圆不相交 |
| 页面内边距 / 内容上限四页一致；`--header-h` 与该页**实际渲染的行数**相符 | browser | 某页 12px 内边距；某页 `--header-h` 与它的 `data-nc-header-rows` 算不出的值 | 24/20/28；`--header-h` 实测 **Today 32 / cove 62 / Settings 62 / wave 92**（§6.4；把四页都断言成 92 是旧版的错，四页里只有 wave 是 92） |
| 主列里最大空矩形 ≤ `--slot-h`（240） | browser | 内容 y=135 结束；测得 241 | 表单右侧的 636px 页面留白（**不计入**——只量主列）；虚线未建成槽（**不计入**——它是内容，§7.3） |
| 渲染出的字符串不含 `/`、`.tsx`、`README`、`slice`、`contract` | node 审计 | `Card runtime lands in a later slice.` | `No report yet.`；面包屑分隔符（独立元素，登记豁免）；`--font-mono` 标识行里的 cwd（登记豁免——它是数据不是文案） |
| `global-classes.yaml` 与非 module CSS 的全局类**双向相等** | node（`repository-check.mjs`） | CSS 里有 `.tnum` 而 manifest 为 `[]`；manifest 里有 `.foo` 而 CSS 没有；给对话框写了样式却没登记那九个类；把 §9.2 的低对比哨兵写进任何非 module CSS | manifest 恰好十一项——`calm-prose`、`tnum`（`base.css`）+ 九个 `dialog-*` / `confirm-dialog-actions` / `confirm-dialog-label`（`styles/dialog.css`，§0.2 / §0.4 CR-4 / CR-7）；哨兵住在测试自己的 `.module.css` 里，扫不到 |

### 9.1 受管属性（`受管` 的定义，闸门的度量对象）

一个禁原始像素的规则，如果不说清它管哪些属性，既无法实现也无法检查。**下面这份清单是穷尽的**：不在表里的属性可以写字面量。

| 组 | 属性 | 只能取 |
|---|---|---|
| 颜色 | `color`、`background-color`、`background`、`border-color`（及四个方向）、`outline-color`、`fill`、`stroke`、`box-shadow` 的颜色位 | 语义 token（`var(--*)`），或 `transparent` / `currentColor` / `none` |
| 间距 | `margin*`、`padding*`、`gap`、`row-gap`、`column-gap`、`inset*`、`top/right/bottom/left`、`translate` | `--space-*`，或 `0` / `auto` / `50%`，**或一个同时含 `--space-*` 与盒尺度 token 的 `calc()`/`min()`/`max()`** |
| 排版 | `font-size`、`font-weight`、`line-height`、`letter-spacing`、`font-family` | `--text-*` / `--weight-*` / `--leading-*` / `--tracking-*` / `--font-*`，或 `inherit` |
| 圆角 | `border-radius`（及四角） | `--radius-*`，或 `0` / `50%` |
| 盒尺度 | `block-size`、`inline-size`、`min-block-size`、`min-inline-size`、`max-block-size`、`max-inline-size`（含 `width/height/min-*/max-*` 的物理写法） | `--row-h*` / `--control-h*` / `--rail-w*` / `--panel-w` / `--drawer-w` / `--measure-*` / `--slot-h` / `--rule-h` / `--dot-*` / `--glyph-*` / `--menu-w-*`，或 `100%` / `100dvh` / `auto` / `fit-content` / `min-content` / `max-content` / `0`，或由这些 token 组成的 `calc()` / `min()` / `max()` |
| 动效 | `transition`、`transition-duration`、`animation`、`animation-duration` | `--motion-*`，或 `none` |
| 层 | `z-index` | `--z-*` |

**间距组那条 `calc()` 口子的边界，两侧都给出来**（§9 闸门第一行判绿的 `calc(var(--control-h-sm) * 2 + var(--space-2))` 走的就是它）。**必须绿**：`padding-inline-end: calc(var(--control-h-sm) * 2 + var(--space-2))`——这个值度量的是"两个控件宽 + 一个间距"，它是**为控件预留的位置**，本来就是盒尺度问题，§4.5 的 44px 与 §6.0 的行、§11 的 `row.module.css` 都要写它。**必须红**：`padding: calc(var(--row-h) / 2)`、`gap: calc(var(--control-h) - var(--space-2))`——**不含 `--space-*` 加项的盒尺度表达式一律判红**，因为那是拿盒高去造节奏，正是本组要禁的事。判据是机械的、可写进 stylelint：值里出现盒尺度 token 时，同一个表达式里必须至少有一个 `--space-*` 项。

**明确不受管**（可写字面量，理由跟着走）：`border-width` / `outline-width`（全仓 `1px` / `2px`，第二种宽度是设计变更不是 token）；`grid-template-*` 与 `flex-*` 里的 `fr` / `%` / `auto` / `minmax()` / `repeat()`（比例不是尺度）；`opacity`；`aspect-ratio`；媒体查询里的一切（自定义属性在那里读不到，断点是 `styles/breakpoints.ts` 的常量）。

### 9.2 非空洞证明与豁免

**关于 browser 层的三道非空洞证明**（缺一不可）：① 同测试内的前置探针（`getComputedStyle(el).color` 必须匹配 `^(rgb|oklch|color)\(`——jsdom 返回空串会让全部对比度断言恒真；`rect.height > 0` 证明布局引擎在跑）；② 同文件的已知违例哨兵（挂一个 `.ds-sentinel-low-contrast`，断言同一个 checker **报告**它——checker 一旦退化成恒真，哨兵立刻红）；③ mutation manifest 条目（改一个 token 却没有任何测试变红 = `dead-mutation` 判负）。

豁免只有三处住所（文件级 allowlist、值级 YAML、对比度 YAML），六条纪律：

- **陈旧即红**：豁免项必须仍然真的违规。
- **未用即红**。
- **`expiry` 必填且过期即红**。
- **精确绑定**到 selector + property（+ theme）。
- **对比度棘轮**：实测优于记录值即判陈旧，逼你删。
- **计数上限单调递减**。

明确拒绝：可重生成的 baseline 文件、`warn` 级别、glob 豁免、无描述的 disable 注释。

### 9.3 诚实的部分：以下由人来看，不由机器

| 领域 | checker 为什么会骗人 |
|---|---|
| **层级本身** | "每页一个 18px 元素" + "chrome 只有三种字号" + "≤1 个 primary" 是**必要条件的集合**，不是层级。整页全用 `--text-display` 也 100% 合规。**一个在扁平页面上报告"0 violations"的检查器，绝不能被读成在证明这一页有层级。** |
| **三通道叠加是否有理由** | 机器只查四通道（§3.2 的机检内核）。第三个通道该不该花，是人的判断 |
| **动作优先级选得对不对** | 机器能查 `data-nc-action` 存在且计数正确，查不出"这个动作该不该是主操作" |
| **P0 是不是对的 P0** | 完全是人的判断 |
| **达标 ≠ 可读** | 4.5:1 是下限不是目标；细字重 + 低色度 + 小字号可以合规但难读；同亮度的红/绿在 WCAG 2.x 下算高分但对色觉障碍者不可分 |
| **间距节奏与光学对齐** | 全部取自 `--space-*` 依然可以毫无节奏；光学居中在数值上永远"不对齐" |
| **动效的必要性** | 只管时长在阶梯里，管不了"这个动效根本不该存在" |
| **颜色语义的分寸** | 查得出 `--warn` 用在哪，查不出"这里用警告色是否夸大了严重性" |
| **空态文案** | 完全在闸门之外 |

这些条目在 oracle 里写 `verification_owner: review-waiver` / `test_tier: none` / `authoritative_test: NONE`，**不给它们硬造恒真断言**。

---

## 10. 尚未决定的事

| # | 问题 | 选项 | 什么能定下来 |
|---|---|---|---|
| 1 | **字重 500 在 Linux WebView 上是否存在** | (a) 存在 → 本文照写；(b) 被 fallback 到 400 或 700 → 所有 `--weight-medium` 的格子退化成"只靠色调"，§3 明显变弱，行标题与面包屑当前项需要改用别的通道 | 在目标 WebView 构建上渲染 `-apple-system` 的 400/500/600 三档并量像素。这是唯一会连锁改动多张表的未决项 |
| 2 | **48px 双行 wave 行是否够呼吸** | (a) 48（本文与全部线框采用）；(b) 56 | 用真实标题 + 生命周期第二行量一次。**不会是 66/72**——那是 legacy 在更大字号与更松行高下的数字。若不够，答案是 56，只改 Cove 与 Today 两张线框的行高 |
| 3 | **时钟是降级还是删除** | (a) 降到 `--text-sm` 推到页头右端（本文采用）；(b) 完全删除，更简单 | 产品判断。环境时间是 "calm" 这个产品的签名，所以默认保留 |
| 4 | **cove 面板列的生命周期分布值不值得建** | (a) 建，给 cove 页第二个纵向锚点，不读每一行就能回答"这个 cove 健康吗"；(b) 不建，cove 页就是一个 720px 列表加页面留白 | 用户是否会为了"健康度"扫 cove，还是永远直接钻到 wave |
| 5 | **浮层阴影的四组件豁免** | (a) `--shadow-float` 只给菜单/弹出/对话框/toast（本文采用）；(b) 全部只靠 hairline + `--paper` | dark 下量一次：`--paper` 菜单压在 `--surface-card` 面板上只差 2% 明度，肉眼判断这个边界是否真的有歧义 |
| 6 | **Settings 什么时候需要页内索引列** | 现在 3 节。到 ~5 节时，页内一根 200px 的索引列开始划算；5 节以下它是为自己而存在的 chrome | 内核已经暴露的设置项积压 |
| 7 | **`--surface-rail` 改值后所有前景的复测** | 提议值把 ΔL 从 0.8 抬到 2.4（light）；`--text-3` 在旧值上是 5.20，只有 ~0.5 余量 | 改完在真实浏览器里跑一遍两个主题的全 surface × 全前景笛卡尔积。**算术表不算数据**——浏览器对超出 sRGB 的 oklch 做 gamut mapping 的方式不同 |
| 8 | **`--cove-1..8` 的八个色相是否够分** | 八色在 `--text-3` 密度的灰底上是否两两可分，尤其 20/60（红橙）与 290/330（紫粉） | 量一次 ΔE；不够就降到 6 色，槽位映射改 mod 6，不改任何组件 |

---

## 11. 从当前 build 迁移（一次性，不是规范）

**先读 §0.0**：本节里凡是动 `fe/web/src/styles/**`、`fe/web/src/ui/dialog/**`、`fe/web/src/ui/confirm-dialog/**` 的条目，commit 都要带对应文件的 `OWNERSHIP-CHANGE` trailer，否则 `readonly-change-trailer` 判红。动 `features/**` / `app/**` / `tools/**` 的不需要。

- **全仓**：`data-variant` → `data-nc-action`，值 `danger` → `destructive`；改 `ui/dialog/public.tsx:129`、删掉 `tools/styles/repository-check.mjs` 的 `LEGACY_DATA_ATTRIBUTES` 条目、改 `web/src/styles/README.md:33` 的描述（§0.3；**没有任何测试断言 `data-variant`**）。**所有 `<button>` 补 `data-nc-action` 或 `data-nc-role`，恰好一个**（§4.1）——行、图标按钮、菜单项、分段控件的段、日历格补的是 `data-nc-role`。
- **`web/src/styles` 的 token 一批**（一次原子提交，§0.1 / §0.1a / §0.1b）：`tokens.css` 加 41 个 token + `--surface-rail` 改值；`public.ts` 加 `BoxScaleToken` / `ShadowToken` / `WeightToken` / `CoveIdentityToken` 四族并扩 `SemanticColorToken`；`public.contract.test.ts` 的 `TOKEN_INVENTORY` 同步；`tokens.contract.test.ts` 加 `BOX_SCALE` / `WEIGHTS` / `COVE_IDENTITY` / `SHADOW` / `THEMED_ALIASES` 五个分组常量、并入 `INVENTORY`、各补形状断言，`MISC` 与它那条写死三个名字的 `it.each`（`:127`）各加四项；新建 `breakpoints.ts`。
- **`ui/dialog/public.tsx` + `ui/confirm-dialog/copy.ts`**：八条变更请求 CR-1…CR-8（含 CR-5a），规格见 §0.4。新增 `web/src/styles/dialog.css`（九个全局类的样式，§6.10 + CR-7），`global-classes.yaml` 由 `[]` 变成十一项（§0.2）。
- **CR-6/7/8 的调用方一批**（`features` / `app`，**不在 readonly 目录，不需要 trailer**）：四个调用点 `confirmDisabled={pending}` → `confirmState={pending ? 'busy' : 'ready'}`（`features/cove/page/public.tsx:100`、`features/wave/page/public.tsx:125`、`app/shell/sidebar.tsx:280` 与 `:289`），cove 页那个再叠打字确认的 `'blocked'`（§6.13）；同时补 `confirmBusyLabel`（CR-7）与 `restoreFocusRef`（CR-8）。**三条断言必须跟着改**：`features/cove/page/public.contract.test.tsx:45`、`features/wave/page/public.contract.test.tsx:37`、`app/shell/sidebar.test.tsx:166` 今天断言执行中 Confirm 有真 `disabled`，改成断言 `aria-disabled === 'true'` + `data-nc-state === 'busy'` + `hasAttribute('disabled') === false`。`INV-CONFIRM-001` 守护的三件事（Cancel 全程可用、Confirm 不重复触发、拒绝后清 pending）原样保留且更强。
- **Today**：删掉 `features/today/public.tsx:78–85` 那段脚手架正文（`<section aria-label="Today terminal">` 里讲 resolve order 与 README 的那个 `<p>`，§5.3 禁止把开发者便条渲染进产品）——**目录 `features/today/terminal` 并不存在**，按目录名去找会找不到；改成 §5.3 的未建成槽 `Terminal is not wired up yet.`。`AM`/`PM` 并进时钟字符串；时钟 36px → `--text-sm` 推到页头右端。**必须原样保留的三处**：空态字符串 `Nothing scheduled.`；`scheduledEvents` 这条按小时分桶的缝隙（`INV-TODAY-002`，§8.1 的"议程的两个数据源"）；日历格里按 wave id 去重的 `data-nc-day-dot`。`features/today/public.contract.test.tsx` 的四条断言全部继续绿，一条都不改。
- **Cove**：删每行的 cove 身份点（页头的身份点已说明）；不渲染面包屑行；行封顶 `--measure-list`，删 `.lifecycle` 的 `margin-inline-start: auto`。
- **Wave**：删每张卡片上的 `"Card runtime lands in a later slice."`——**`features/wave/page/public.test.tsx:75–78` 那条 `it('notes that the card runtime is a later slice')` 正断言它存在，要一起删**（它断言的恰好是 §9 那条"渲染字符串不含 slice"必须判红的东西）；页头标识行只留 cwd；卡片 `kind` 只渲染一次（mono `kind` 是身份，title 是标签，没有 title 时只显示 kind）。
- **Settings**（逐条对着 `settings.module.css` / `settings/public.tsx` 核过，旧版这一条既写错了一项也漏了大部分）：删两个 `.card` 盒（`border` + `--radius-lg` + `--surface-card` + `padding`，段标签取代它，§8.4）；`.title` 的 `--text-xl`(22) → `--text-lg`(18)；`.cardTitle` 大写但只有 `--tracking-wide` → `--tracking-wider`（§2.2），字号 `--text-sm` → `--text-xs`、色调 `--text-3` + 600；`.primary` 的 `background: var(--accent-soft)` → 实心 `var(--accent)` + `color: var(--text-on-accent)`（§4.2 明令禁止 `--accent-soft` 做按钮填充）；`.primary:disabled` / `.secondary:disabled` 的 `opacity: 0.5` → `--text-4` + `--surface-chip`（§5.1 明令禁止 opacity）；`.hint` 与 `.crumbSep` 的 `--text-4` → `.hint` 用 `--text-3`（它是要读的文字），`.crumbSep` 保留 `--text-4`（装饰字形，合法）；`.saved` 的 `--success` → `--success-text`；`.input` 的 `--surface-bg` → `--paper`，`.page` 的 `--surface-bg` → `--bg`（§2.1 废弃别名）；`.secondary` 的 `color: var(--text-2)` → `--text`（§8.4 的 `Reset` 是 secondary）；`.input:focus-visible` 的 `outline` → §5.2 的输入框变体。**这份清单是"删完"不是"达标"**：下面八条也与 §8.4 / §6.x 的目标态冲突，按 §8 从头写不会踩，但不能拿这份清单当验收表——`.label` 的 `--text-3` → `--text-2` + 500（§8.4）；`.crumbs` 的 `--text-sm` → `--text-xs`（§6.14）；`.card` 的 `max-width: 40rem` → `--measure-form`；`.page` 的 `padding: var(--space-6)` → §7.2 的 24/20/28；`.crumbLink` / `.primary` / `.secondary` / `.radio` 四处 `font: inherit` 全删（§2.7，与 `row.module.css` 那条同性质）；`.radio` 的药丸/边框几何 → §6.11 的分段控件；`.error` 的 `--text-sm` → §5.3 的错误盒配方。另：**没有"标题下的 26px 描述段落"这个东西**——`<h1>` 之后直接就是 `<section className={styles.card}>`。
- **`features/wave/row/row.module.css`**（照旧版逐条改完仍会被 §9.1 判红，下面补齐）：补 `min-block-size: var(--row-h-lg)`；`.glyph` 与 `.coveDot` 的 `inline-size/block-size: 6px` → `var(--dot-sm)`；`.pin` 的 `inset-inline-end: 26px` → `var(--space-10)`(24)、`.remove` 的 `2px` → `var(--space-0)`（`inset*` 是定位不是间距，§2.3）；`.row` 的 `padding-inline-end: var(--space-11)`(28) → `calc(var(--control-h-sm) * 2 + var(--space-2))`(44，§4.5)；`.action` 静止态补 `pointer-events: none`（`opacity: 0` 必须阻断点击，但不能用 `visibility`，§2.6）；`--radius-md` → `--radius-sm`；`padding` → `padding-inline: var(--space-4)` + `padding-block: 0`；删 `font: inherit`（`base` 的活）；`.lifecycle` 的 `--text-4` → `--text-3`；`.rowAttention .title` 的 `--warn` → `--warn-text`；`.action` 的 `22px` → `var(--control-h-sm)`；`.remove:hover` 的 `--warn` → `--error-text`；`gap: 2px` → `var(--space-1)`；补 `max-inline-size: var(--measure-list)`；补 `:focus-visible` 配对；**删 `.progressTrack` 整段**（§6.3）。
- **`app/shell/sidebar.tsx` + `shell.module.css`**：`17rem` → `var(--rail-w)`；`min-height: 100dvh` → `block-size: 100dvh`；`@media (width < 60rem) { .rail { display: none } }` → 折叠成 44px 图标条；`.sectionTitle` / `.count` / `.chevron` / `.coveDelete` / `.empty` 的 `--text-4` → `--text-3`（段标签另加 600）；删 `.countWarn` 的 `--warn`（只有 "WAITING ON YOU" 能出现 warn）；删 `.swatchRunning` 的 pulse（身份点不是状态点）；`.brand` 的 `--text-md` → `--text-sm`；`.chevron` 18px → 盒 20 / 字形 16；`.coveRow` 的 `--radius-md` → `--radius-sm` + 补 `min-block-size: var(--row-h)`；`.coveInput` / `.menu` 的 `--surface-card` → `--paper`（菜单另加 `--radius-lg` + `--shadow-float`）；`.menuItem` 补 `block-size: var(--row-h-sm)`；`<p>No coves yet.</p>` → 第一行位置的内联新建输入（§6.12），并把现有的 blur-提交改成 `Enter` 提交 / `Escape` 取消。

---

## 本次修订关掉了哪些问题

**一、冻结面的变更请求补全（§0.4，五条 → 八条）。** 旧版只登记了 CR-1…CR-5，实现者做到一半会撞上三堵墙，每一堵都是"冻结面表达不了、调用方也自救不了"：**CR-6** 两种"不能按"（前置条件未满足 vs 执行中）挤在一个布尔里，CSS 与测试都分辨不出来，而 §5.1 给它们的配方相反——换成 `confirmState: 'ready' | 'blocked' | 'busy'`；**CR-7** `confirmLabel` 是 `string`，塞不进第二个标签节点，而 §5.1 第 4 条与 §9 的等宽闸门都要求双标签同槽——加 `confirmBusyLabel` 与第九个全局类 `confirm-dialog-label`；**CR-8** `ConfirmDialog` 不转发 `restoreFocusRef`，而删除成功后触发元素必然已卸载——转发它，并写死落点（路由到领域父级、焦点到 `[data-nc-page-title]`）。另补 **CR-5a**：`deleteCoveCopy` 返回四元组，因为 §6.13 的正文是两条格式不同的句子，三元组的单个 `description` 槽装不下。

**二、§0 每一条变更都带上了它的契约后果与落地程序。** 旧版只有 §0.3 记得"改它的契约测试"。现在：**§0.1a** 写明只改 `tokens.css` 不够——`tokens.contract.test.ts:79–82` 断言 token 清单**精确相等**、`styles/public.ts` 用封闭联合钉住同一批名字、`public.contract.test.ts:56` 让两者双向相等；盒尺度 / 字重 / cove 身份 / 阴影在现有四族里一族都不属于，于是新增 `BoxScaleToken`(24) / `WeightToken`(3) / `CoveIdentityToken`(8) / `ShadowToken`(1) 并扩 `SemanticColorToken`(5)，合计 41，逐项与 §0.1 对得上；`--text-on-accent` 单列 `THEMED_ALIASES`，因为 `ALIASES` 那条断言写死"dark 里没有覆盖"。**§0.0** 补上一道真实闸门：三个 readonly 目录下的每一个被改文件都要一条 `OWNERSHIP-CHANGE: <path> — <理由> (#997)` commit trailer，`tools/ownership/validator.ts` 逐路径检查。**§0.1b** 新建 `styles/breakpoints.ts`——旧版三处把它当既成事实引用，它并不存在。

**三、四处自相矛盾各选了一个权威。** **等待行字重**：§6.3 的变体表成为唯一仲裁者，等待态四个变体一律 500（紧凑变体的 400 是**静止态**），§3.2 与 §7.5、§8.1 全部改成指回去、不复述数字。**议程行**：§6.3 拆出第四个"议程"变体（几何同紧凑，行首装 **cove 身份点**而非状态点，无相对时间、无生命周期短语，可带小时标签），Today 的线框同步重画，§8.1 P3 那条"议程行生命周期短语"删掉。**空态文案**：用现网被 `INV-TODAY-002` 钉死的 `Nothing scheduled.`，不改测试，并把它守护的按小时分桶缝隙写进 §8.1 与 §11。**44px 的 `calc`**：§9.1 间距组开一条精确口子——含 `--space-*` 加项的盒尺度 `calc()` 合法（预留控件位本来就是盒尺度问题），不含 `--space-*` 的一律红（拿盒高造节奏正是本组要禁的）；正反例都给。另修掉两条轻矛盾：hairline 实测对比度只在 §2.1 声明一次；§9.2 的低对比哨兵住在测试自己的 `.module.css` 里，不进任何非 module CSS，所以全局类清单仍然封闭（今天十一项）。

**四、十条失实/不完整的仓库陈述按文件重核。** 生命周期**穷尽为九个**（`wave.ts:9–12`，旧版写六个，照抄会让 `switch` 编译失败），并给出九个值在本文里的完整归宿；`font: inherit` 是 **11 个 `.module.css` 里的 9 个、共 29 处**（不是"14 个模块"）；`settings/public.tsx` 的 `role="radiogroup"` 在 **145** 行；`DELETE_COVE_COPY` 的调用方是**两处**（第三处读的是 `DELETE_WAVE_COPY`）；要删的 Today 脚手架段落在 `features/today/public.tsx:78–85`（**目录 `features/today/terminal` 不存在**）；§11 的 Settings 清单补上八条漏项并改口径为"删完 ≠ 达标"；`data-variant` 的三个落点写清（**没有任何测试断言它**）；`features/wave/page/public.test.tsx:75–78` 正断言要删的那句 slice 文案，要一起删；`features/today/public.contract.test.tsx` 的四条断言全部保留。**两个复现不出来的数字**改成可复算的：当前 build 的 wave 行宽是 **1144**（1440 − 272 − 24，给出两个文件行号），不是 1128；rail 可用宽"掉到 116"补上被省略的前提（分区再叠一层 `--space-3` 两侧）。

*本文取代六份研究稿（`_fe-design-legacy-extract` / `_fe-design-current-audit` / `_fe-design-references` / `_fe-design-system` / `_fe-page-hierarchy` / `_fe-design-gates`）。那六份留在仓库里作为证据来源；实现时不需要打开它们。*
