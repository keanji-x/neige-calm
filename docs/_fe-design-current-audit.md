# 新前端视觉层现状审计 (`fe/web/src`)

审计对象：`design/fe-rewrite-architecture` 分支，`.claude/worktrees/997-c1-today/fe`，运行实例 `http://127.0.0.1:5180/`。
方法：读全部 11 份 `*.module.css` + `tokens.css` + `entry.css`；Playwright/Chromium 对 `/`、`/cove/<id>`、`/wave/<id>`、`/settings` 在 1440×900 与 390×844、light/dark 各截图并 dump `getComputedStyle`。所有 "computed" 标注的数字来自实测，非推断。产物在 `/tmp/claude-1000/-mnt-data2-kenji-neige-calm/b458771f-889c-4baa-b33b-1b0dbfd603c2/scratchpad/current/`。

用户的判词是"怎么这么丑"。下面是具体原因。结论先行：

> **不是配色不好看，也不是 token 定得差。是根本没有 base/reset 层——`body` 还带着浏览器默认的 8px margin、`content-box`、`16px`、Times New Roman；再叠加五个 slice 各自发明的间距/尺寸/字号，导致 92 个 token 里 40 个从未被用过，间距标尺 14px 以上整段空置，每页 12px 内边距、控件高度 18/18.5/20/22/24/25/28/29/38/43/47 十一种、正文只有 11–12.5px、内容占版面 14–46%。移动端侧栏隐藏规则被同文件后面的规则覆盖，整个窄屏布局是坏的。**

---

## 0. 三个"这不是丑，这是坏了"级别的缺陷（先看这个）

| # | 缺陷 | 证据 | 后果 |
|---|---|---|---|
| B1 | **base/reset 层声明了但从未实现** | `web/src/styles/entry.css:1` 声明 `@layer reset, vendor, tokens, base, astryx, ui, features, overrides;`，但 `:3-4` 只 import 了 `tokens.css` 与 `vendor.css`。`reset` 与 `base` 两层**空**。`styles/README.md` 明确写"不增加…全局视觉规则" | 见 B2/B3/B4 |
| B2 | `body { margin: 8px }`（UA 默认）从未清零 | computed, light, `/`：`body.margin = "8px"`；`.shell` 的 `x=8, y=8, w=1424`（视口 1440）。dark 截图 `cove-dark-desktop.png` 像素 (2,2) = **`rgb(255,255,255)`**，而 (400,400) = `rgb(10,14,17)` | **深色模式下整个 app 被一圈纯白 8px 边框包住**。这一条单独就能让人说"丑" |
| B3 | 无 `box-sizing: border-box` | computed：`html/body/#root.boxSizing = "content-box"`；`.page` 声明 `min-height:100%` + `padding:12px` → 实测 `1152×924` 在 `1152×900` 的 `main` 里 | 每条路由都**永久出现纵向滚动条**（`scrollHeight 916 > clientHeight 900`），底部再露 16px 白条 |
| B4 | `html`/`body` 从未设 `font-family` / `font-size` / `color-scheme` | computed：`body.fontFamily = "Times New Roman"`, `fontSize = "16px"`, `colorScheme = "normal"` | 见 §1 的 16px 泄漏；`color-scheme` 没写 → 深色模式下**原生滚动条、`<select>`、输入法候选框全是亮色** |
| B5 | **移动端侧栏隐藏规则被覆盖，窄屏布局失效** | `shell.module.css:14-22` 的 `@media (width < 60rem) { .rail { display:none } }` 写在 `.rail { display:flex }`（`:24-33`）**之前**，同层同特异度 → 后者胜。computed, 390px, `/`：`.rail` = `display:flex`，`374×255`，`main` 被推到 `y=263` | 390px 下侧栏变成一块占屏 30–53% 高的全宽死区（`/cove` 449px、`/wave` 412px）。见 `today-light-mobile.png` / `wave-dark-mobile.png` |

B1–B4 是同一个根因。**先补 reset/base 层，视觉观感立刻改善一大截，代价约 20 行 CSS。**

---

## 1. Token 覆盖率

### 1.1 定义了但**从未使用**的 token（40 / 92 = 43%）

在 11 份 `*.module.css` 中 `var(--x)` 引用数为 0 的：

| 家族 | 未用 token | 说明 |
|---|---|---|
| 颜色（位置） | `--bg`, `--paper` | 全部走了 alias `--surface-bg`/`--surface-paper` |
| 颜色（语义） | `--error`, `--overlay-scrim`, `--cal-event-waiting-bg` | |
| 颜色（别名） | `--text-label`, `--text-meta`, `--text-decorative`, `--surface-hover-overlay` | **十族契约里专门造的语义别名，一个都没被消费**——所有人直接写 `--text-2/3/4` |
| Surface | `--surface-code`, `--surface-panel-head`, `--surface-terminal`, `--surface-toggle-overlay` | `--surface-terminal` 未用，但 today 有个"终端"卡片（`today.module.css:83` 用 `--surface-card`） |
| Overlay | `--overlay-active` | 因为**全站没有任何 `:active` 状态**（§5） |
| 字体 | `--font-serif`, `--font-display`, `--font-code` | `--font-display` 未用 → 大标题也走 `--font-sans` |
| 字号 | `--text-display-sm` (26px) | 8 级字号只用了 6 级 |
| 行高 | `--leading-tight`, `--leading-loose` | 5 级只用 3 级；且见 §7，绝大多数元素根本没设行高 |
| 字距 | `--tracking-normal`, `--tracking-widest` | |
| 圆角 | `--radius-xs` (2px), `--radius-xl` (10px) | 6 级只用 4 级 |
| **间距** | `--space-px`(1), `--space-7`(14), `--space-8`(16), `--space-9`(20), `--space-10`(24), `--space-12`(32) | **14 级只用 8 级，且 14px 以上整段空置**（唯一例外 `--space-11`=28px 被当成 row 右内边距用了一次）。这是"页面挤在角落"的直接量化证据 |
| 动效 | `--motion-instant`, `--motion-quick`, `--motion-snappy`, `--motion-medium`, `--motion-slow` | 6 个只用 `--motion-pulse` 一个。**全站零 `transition`**（§5） |
| z-index | `--z-base`, `--z-raised`, `--z-sticky`, `--z-modal`, `--z-toast` | 6 个只用 `--z-overlay` 一个 |

使用最多的 8 个：`--space-2`(48)、`--text-sm`(34)、`--space-1`(31)、`--text`(26)、`--radius-sm`(26)、`--hairline`(25)、`--text-4`(24)、`--text-3`(23)。
**整个 app 的视觉词汇实际只有 ~15 个 token。**

### 1.2 写死的原始值（应当是 token 的）

全量，按文件排序：

| file:line | 原始值 | 属性 | 应当是 |
|---|---|---|---|
| `app/shell/shell.module.css:7` | `17rem` | `grid-template-columns` | 无对应 token（缺 size 家族） |
| `app/shell/shell.module.css:8` | `100dvh` | `min-height` | 无对应 token |
| `app/shell/shell.module.css:14` | `60rem` | `@media` 断点 | **无断点 token**，与 `today.module.css:77` 重复手写 |
| `app/shell/shell.module.css:32` | `100dvh` | `max-height` | 同上 |
| `app/shell/shell.module.css:93,94` | `28px` | `inline/block-size` (`.iconCove`) | 无 control-size token |
| `app/shell/shell.module.css:148` | `2px` | `gap` (`.coveGroup`) | `--space-1` |
| `app/shell/shell.module.css:159,160` | `18px` | `.chevron` 尺寸 | 无 control-size token |
| `app/shell/shell.module.css:197,198` | `20px` | `.coveDelete` 尺寸 | 无 |
| `app/shell/shell.module.css:212,213` | `8px` | `.swatch` 尺寸 | `--space-4`（语义错配，见 §2） |
| `app/shell/shell.module.css:249` | `2px` | `gap` (`.waveList`) | `--space-1` |
| `app/shell/shell.module.css:270,271` | `28px` | `.avatar` 尺寸 | 无 |
| `app/shell/shell.module.css:286` | `10rem` | `min-inline-size` (`.menu`) | 无 |
| `features/cove/page/page.module.css:23,24` | `12px` | `.swatch` 尺寸 | **同一个"cove 色点"在 shell 里是 8px，这里是 12px** |
| `features/settings/settings.module.css:53` | `40rem` | `max-width` (`.card`) | 无 content-width token；**全站唯一的宽度约束** |
| `features/settings/settings.module.css:109` | `2px` | `outline-width` | 无 focus-ring token |
| `features/settings/settings.module.css:110` | `1px` | `outline-offset` | `--space-px` |
| `features/today/today.module.css:56,57` | `8px` | `.dot` 尺寸 | 与 shell `.swatch` 同义、值相同、各写一遍 |
| `features/today/today.module.css:77` | `60rem` | `@media` 断点 | 重复手写 |
| `features/today/today.module.css:79` | `22rem` | grid 第二列 | 无 |
| `features/today/today.module.css:181` | `2px` | `gap` | `--space-1` |
| `features/today/today.module.css:182` | `6px` | `min-height` | `--space-3` |
| `features/today/today.module.css:186,187` | `5px` | `.dayDot` 尺寸 | **第三种"点"尺寸** |
| `features/today/today.module.css:212` | `3px` | grid 第一列（事件色条） | 无 |
| `features/today/today.module.css:237` | `2px` | `gap` | `--space-1` |
| `features/today/today.module.css:269,270` | `6px` | `.flag` 尺寸 | **第四种"点"尺寸** |
| `features/wave/lifecycle-badge/…:24,25` | `6px` | `.dot` 尺寸 | 第五处 |
| `features/wave/page/page.module.css:30,31` | `24px` | `.back` 尺寸 | 无 |
| `features/wave/page/page.module.css:68,69` | `6px` | `.coveDot` 尺寸 | 第六处 |
| `features/wave/row/row.module.css:40,41` | `6px` | `.glyph` 尺寸 | 第七处 |
| `features/wave/row/row.module.css:58` | `2px` | `gap` | `--space-1` |
| `features/wave/row/row.module.css:104,105` | `6px` | `.coveDot` 尺寸 | 第八处 |
| `features/wave/row/row.module.css:118` | `3px` | `block-size`（进度条） | 无 |
| `features/wave/row/row.module.css:136,137` | `22px` | `.action` 尺寸 | 无 |
| `features/wave/row/row.module.css:143` | `1` | `line-height` | `--leading-none` |
| `features/wave/row/row.module.css:159` | `26px` | `inset-inline-end` | 手算的魔数（22 + 4） |
| `features/wave/row/row.module.css:169` | `2px` | `inset-inline-end` | `--space-1` |
| `app/shell/shell.module.css:79,129,169,206` | `1` | `line-height` ×4 | `--leading-none` |
| 36 处 | `1px solid` / `1px dashed` | 全部 border | **无 border-width token**（token 契约十族里没有这个家族） |

内联 `style={{}}`（8 处）——全部是数据驱动的 cove 颜色 + 一个进度百分比，属正当用法，不算违规：
`features/today/public.tsx:210,266`、`features/wave/row/public.tsx:78,92`、`app/shell/sidebar.tsx:196,364`、`features/cove/page/public.tsx:65`、`features/wave/page/public.tsx:76`。
但注意：**cove 颜色来自 API 的任意 hex（`#5B8DEF`），不经过任何对比度/主题变换**，深色模式下直接照搬。

### 1.3 未被 token 覆盖、但 render 出来的值

| 值 | 来源 | 出现位置 |
|---|---|---|
| **`16px` 字号** | UA 默认，经 `font: inherit` 泄漏 | Today 页 **7 个**元素、Cove **9 个**、Wave **5 个**、Settings **10 个**（computed, light）。见 §7 |
| `"Times New Roman"` | UA 默认 | `html`/`body`/`#root`（computed）。任何漏在 `.shell` 外或 `font: inherit` 链断掉的文字都会是衬线体 |
| `outline: auto 1px rgb(16,16,16)` | Chromium 默认焦点环 | 除 `settings .input` 外**所有** 25 个可聚焦控件（computed, `/`）。见 §5 |
| `line-height: normal` | UA 默认 | `body` 及绝大多数容器 |

---

## 2. 标尺违规

### 2.1 "本该是 token X"

| 值 | 出现次数 | 对应 token | 位置 |
|---|---|---|---|
| `2px` | 6 | `--space-1` | shell:148,249；today:181,237；row:58,169 |
| `1px` | 1 | `--space-px` | settings:110 |
| `6px` | 1（min-height） | `--space-3` | today:182 |
| `1`（line-height） | 5 | `--leading-none` | shell:79,129,169,206；row:143 |

### 2.2 "语义上不该复用 space token 的尺寸"——**缺失的 token 家族**

这一类才是主要问题：五个 slice 都需要"一个小圆点""一个图标按钮""一条边框"，但 token 契约里**没有 size / control-height / border-width / breakpoint / content-width 家族**，于是每人各拍一个数。

**"色点/状态点"——同一个概念，8 处独立实现，5 种尺寸：**

| 尺寸 | file:line | 语义 |
|---|---|---|
| `5px` | `today.module.css:186` | `.dayDot` 日历当天事件点 |
| `6px` | `today.module.css:269` | `.flag` 事件状态点 |
| `6px` | `lifecycle-badge.module.css:24` | `.dot` 徽章点 |
| `6px` | `wave/page/page.module.css:68` | `.coveDot` 面包屑点 |
| `6px` | `wave/row/row.module.css:40` | `.glyph` 行状态点 |
| `6px` | `wave/row/row.module.css:104` | `.coveDot` 行内 cove 点 |
| `8px` | `shell.module.css:212` | `.swatch` 侧栏 cove 点 |
| `8px` | `today.module.css:56` | `.dot` 统计点 |
| `12px` | `cove/page/page.module.css:23` | `.swatch` cove 页 cove 点 |

**同一个 cove 的色点，在侧栏是 8px、在 cove 页头是 12px、在 wave 行里是 6px、在 wave 面包屑里是 6px。**

**"图标按钮"——4 种尺寸：** `18px`(chevron)、`20px`(coveDelete)、`22px`(row action)、`24px`(wave back)、`28px`(iconCove/avatar)。

**边框宽度**：36 处 `1px`，零 token。
**断点**：`60rem` 手写 2 次（shell:14, today:77），零 token。
**内容宽度**：全站只有 `settings.module.css:53` 的 `max-width: 40rem`，其余三页无约束。

### 2.3 圆角

`--radius-sm`(4) ×26、`--radius-md`(6) ×8、`--radius-lg`(8) ×2、`--radius-pill` ×14；`--radius-xs`(2)/`--radius-xl`(10) 未用。
问题在于**同一族元素圆角不一致**（computed, light, `/`）：

| 元素 | 圆角 | 位置 |
|---|---|---|
| `.coveRow`（侧栏 cove 行） | **6px** | shell:181 |
| `.row`（侧栏 wave 行，紧邻其下） | **4px** | row.module.css:26（`.wrapperCompact .row`） |
| `.row`（cove 页 wave 行，同一组件） | **6px** | row.module.css:15 |

同一个 `WaveRow` 组件，在侧栏渲染 4px、在 cove 页渲染 6px；侧栏里它又和上面 6px 的 cove 行贴在一起。

---

## 3. 垂直律动与密度（五个 slice 的数字并排）

全部 computed, light, 1440×900。

### 3.1 页面级

| | Today | Cove | Wave | Settings |
|---|---|---|---|---|
| 文件 | `today.module.css:2` | `cove/page:2` | `wave/page:2` | `settings:2` |
| `padding` | **12px** (`--space-6`) | **12px** | **12px** | **12px** |
| 根 `gap` | **12px** (`--space-6`) | **10px** (`--space-5`) | **12px** (`--space-6`) | **10px** (`--space-5`) |
| `min-height` 属性 | `min-height:100%` | `min-height:100%` | `min-block-size:100%` | `min-height:100%` |
| 实测尺寸 | 1152×924 | 1152×924 | 1152×924 | 1152×924 |
| 内容 max-width | 无 | 无 | 无 | 40rem（仅卡片） |
| 主区留白率 | **66%** | **86%** | **79%** | **54%** |

四个页面里两个用 `--space-6` 两个用 `--space-5` 做根 gap，没有任何理由；`--space-6`=12px 作为 1440px 视口下的页面内边距是**桌面端最小可接受值的一半**。

### 3.2 区块与卡片

| 元素 | file:line | padding | gap | radius | 实测 |
|---|---|---|---|---|---|
| Today `.card` | `today:83` | 8px | 6px | 8px | 766×140 / 352×214 |
| Settings `.card` | `settings:49` | 8px | 6px | 8px | 658×175 |
| Wave `.card`（列表项） | `wave/page:144` | 6px | 6px | 6px | **1128×28** |
| Wave `.empty` | `wave/page:126` | 8px | — | 6px | — |
| List `.empty` | `list:15` | 8px | — | 6px | — |
| Cove header 底边距 | `cove/page:19` | `padding-bottom: 6px` | 6px | — | 1128×45 |
| Wave header | `wave/page:13` | 0 | 6px | — | 1128×86 |

Today 与 Settings 的卡片是同一套（8/6/8）——好；Wave 的"卡片"是完全不同的东西（6/6/6，高 28px）。同名 `.card`，三种物件。

### 3.3 控件高度——**11 种，无标尺**

| 高度 | 控件 | file:line |
|---|---|---|
| **12px** | `.sectionTitle`（"COVES"） | shell:114 |
| **17px** | `.badge`（Draft 徽章） | lifecycle-badge:2 |
| **18px** | `.themeToggle` / `.chevron` / `.delete`(wave) | shell:60 / shell:157 / wave/page:87 |
| **18.5px** | `.newCove`（"+"） | shell:122 |
| **20px** | `.coveDelete` / `.newWave` / `.delete`(cove) | shell:195 / cove/page:48 / cove/page:63 |
| **22px** | `.action`(pin/remove) / `.nav`(周切换) | row:130 / today:119 |
| **24px** | `.back` / `.input`(settings) | wave/page:27 / settings:98 |
| **25px** | `.row`（侧栏 wave 行） | row:24 |
| **28px** | `.avatar` / `.iconCove` / `.primary`/`.secondary`/`.radio` | shell:269,90 / settings:119,134,154 |
| **29px** | `.coveRow` / `.row`（cove 页） | shell:173 / row:7 |
| **38px** | `.title`（可编辑标题） | editable-title:2 |
| **43px** | `.event`（今日议程条目） | today:210 |
| **47px** | `.day`（日历格） | today:140 |

**没有任何两个 slice 商定过一个控件高度。** 最小的可点击控件是 18×15.3px（`.nav`，computed），远低于任何触控/可用性下限。

### 3.4 侧栏

| | 值 | 位置 |
|---|---|---|
| 宽度 | `17rem` = 272px | shell:7 |
| 内边距 | 8px | shell:28 |
| 根 gap | 8px | shell:27 |
| 行高（cove） | 29px | computed |
| 行高（wave） | 25px | computed |
| 行间 gap | 2px | shell:148,249（写死） |

侧栏 8px 内边距 vs 主区 12px 内边距 vs 卡片 8px 内边距——三套。

---

## 4. 跨 slice 不一致（同一概念，不同实现）

| # | 概念 | A | B | 差异 |
|---|---|---|---|---|
| C1 | **cove 色点** | `shell.module.css:211-217` `inline-size:8px` | `cove/page/page.module.css:22-28` `width:12px; height:12px` | 尺寸 8 vs 12；一个用逻辑属性一个用物理属性 |
| C2 | **wave 行圆角** | `row.module.css:15` `--radius-md` | `row.module.css:26` `--radius-sm` | 同组件两种圆角，在侧栏与 cove 页各显一种 |
| C3 | **空状态** | `wave/list/list.module.css:15-24`：`padding:8px; 1px dashed; radius-md; --text-3; leading-snug` | `wave/page/page.module.css:126-133`：`padding:8px; 1px dashed; radius-md; --text-3;`（**无 leading**） | 近乎复制粘贴但漏了 `line-height` |
| | | `shell.module.css:253-257`：`color:--text-4; padding-inline:4px;` **无边框无圆角** | | 侧栏空态与页面空态是两种视觉语言 |
| C4 | **面包屑** | `wave/page/page.module.css:19-25` `font-size: --text-xs`(11px)，分隔符 `.crumbSeparator` | `settings.module.css:13-19` `font-size: --text-sm`(12.5px)，分隔符 `.crumbSep` | 字号不同、类名不同、`.crumbLink` 有 hover 背景而 `.crumb` 只有下划线 |
| C5 | **破坏性按钮** | `cove/page/page.module.css:63-77`：`--text-sm`, `padding 2px 6px`, hover 只改**文字色**为 warn | `wave/page/page.module.css:87-103`：`--text-xs`, `padding 2px 6px`, hover 只改**边框色**为 warn-border | 字号 12.5 vs 11；hover 语义一个改字色一个改边框 |
| | | `shell.module.css:195-209 / 322-325`：`.coveDelete` 无边框、默认 `opacity:0`、hover 改背景+字色 | `row.module.css:168-174`：`.remove` 无边框、`opacity:0`、hover 只改字色 | 四个"删除"，四种表现 |
| C6 | **主操作按钮** | `cove/page:48-57` `.newWave`：`background: --accent-soft; color: --accent; border: --hairline` | `settings:119-127` `.primary`：`background: --accent-soft; color: --text; border: --accent` | 同为 accent-soft 底，一个 accent 字 + hairline 边，一个 text 字 + accent 边 |
| | | `new-wave/new-wave.module.css:85-88` `.submit`：`background: --accent-soft; color: --accent; border: --hairline` | | 三个主按钮两种配方 |
| C7 | **主按钮 hover** | `cove/page:59-61` `.newWave:hover { background: --overlay-hover }` | 同 `new-wave:90-93` | **hover 时把 accent-soft 换成半透明灰 → 强调色消失，看起来像被禁用** |
| C8 | **区块小标题** | `today.module.css:93-98` `.cardTitle`：`--text-sm`, `--text-3`, `tracking-wide`, `uppercase` | `settings.module.css:60-65` `.cardTitle`：完全相同 | 好——但 |
| | | `wave/page/page.module.css:118-124` `.sectionTitle`：`--text-sm`, `--text-2`, `tracking-wide`, `font-weight:600`, **不 uppercase** | `shell.module.css:114-120` `.sectionTitle`：`--text-xs`, `--text-4`, `tracking-wider`, `uppercase` | 四个"小标题"三种规格；"Cards" 与 "COVES" 与 "NETWORK" 互不成体系 |
| C9 | **表单输入** | `settings.module.css:98-106`：`--surface-bg` 底，`--font-mono`，有 `:focus-visible` | `new-wave.module.css:22-33`：`--surface-card` 底，`--font-mono`，**无 focus 样式** | 底色相反（一个用页面底一个用卡片底），焦点态一有一无 |
| C10 | **hover 遮罩强度** | `.coveRow:hover` → `--overlay-hover` (shell:317) | `.day:hover` → `--overlay-hover-faint` (today:154) | 同为"列表行 hover"，强度不同 |
| | | `.radio:hover` → `--overlay-hover-faint` (settings:164) | `.event:hover` → `--overlay-hover` (today:225) | 同页内也不一致 |
| C11 | **返回控件** | `wave/page:27-39` `.back`：24×24 方形图标按钮 `←` | `pending-route.module.css:41-51` `.back`：带 chip 底色的文字按钮 | 同名同义，两种物件 |
| C12 | **物理 vs 逻辑属性** | `shell/today/cove-page` 混用 `width/height`、`min-height`、`padding-bottom` | `wave/*`、`lifecycle-badge` 一律 `inline-size/block-size`、`min-block-size`、`padding-block` | 无统一约定；`cove/page:19` `padding-bottom` 与 `wave/page` 的 `padding-block` 并存 |

---

## 5. 缺失的交互状态

grep 全量结果（11 份 module css）：

| 状态 | 出现次数 | 位置 |
|---|---|---|
| `:hover` | 21 | 各处 |
| `:focus-visible` | **1** | `settings.module.css:108` |
| `:focus-within` | 2 | `shell.module.css:330`、`row.module.css:154`（用于**显示**悬浮按钮，不是焦点环） |
| `:active` | **0** | — |
| `:disabled` | 3 | `new-wave:95`、`settings:129,144` |
| `transition` | **0** | — |
| `box-shadow` | **0** | — |
| `animation` | 1 | `shell.module.css:221`（`--motion-pulse`） |

### 5.1 无自定义焦点环的控件（全部 25 个可聚焦元素，除 settings 的 2 个 input）

实测（Playwright 逐个 `.focus()` 后读 `outlineStyle/Width/Color`），**除 `settings .input` 外，每一个控件的焦点态都是 Chromium 默认 `outline: auto 1px rgb(16, 16, 16)`**：

`/` 与全站侧栏：`.brand`、`.collapseToggle`、`.themeToggle`、`.newCove`、`.chevron`、`.coveRow`、`.coveDelete`、`.row`(WaveRow)、`.action .pin`、`.action .remove`、`.avatar`、`.menuItem`、`.coveInput`、`.iconCove`
`/`：`.nav`(×2)、`.day`(×7)、`.event`(×2)
`/cove`：`.title`(EditableTitle)、`.newWave`、`.delete`
`/wave`：`.back`、`.crumb`(×2)、`.title`、`.delete`
`/settings`：`.crumbLink`、`.primary`、`.secondary`、`.radio`(×3)
新建 wave 表单：`.textarea`、`.input`、`.select`、`.checkbox`、`.cancel`、`.submit`

这是 a11y 缺陷，不是打磨项：焦点环颜色是 UA 常量 `rgb(16,16,16)`，**不随主题变化，也不用 `--accent`**；`settings .input` 是唯一用了 `--accent` 的，因此全站焦点表现是两套。

### 5.2 零过渡

`transition` 出现 0 次。**每一个 hover 都是瞬时跳变**——21 处 hover 全部无缓动。6 个 `--motion-*` token 只有 `--motion-pulse` 被用。这是"廉价感"的主要来源之一。

### 5.3 禁用态

`.primary:disabled` / `.secondary:disabled`（settings:129,144）只写 `opacity:.5`；`.submit:disabled`（new-wave:95）写 `opacity:.5; cursor:not-allowed`——连 `cursor` 都不一致（settings 用 `default`）。其余所有按钮无禁用样式。

### 5.4 选中/激活态

`.rowActive`（shell:190 / row:34）与 `.daySelected`（today:163）用同一配方（`accent-soft` 底 + `accent` 边）——这一点是一致的。但 `.radioOn`（settings:168）额外改了 `color: --text`，`.dayToday`（today:158）用 `color: --accent; font-weight:600`，`.pinAlways`（row:164）用 `color: --accent`——四种"被选中"的表达。

---

## 6. 对比度（实测计算，非估算）

方法：从 computed style 取每个含文本节点的 `color` 与其最近的非透明祖先 `background-color`，OKLCH→sRGB→相对亮度→WCAG 比值。半透明前景做合成。大字标准 = ≥24px 或 ≥18.66px 且 ≥700。

### 6.1 文本对比度失败（light：14 / 63 对；dark：14 / 63 对）

**所有失败都指向同一个原因：`--text-4` 被当作正文/元信息颜色使用。**

| 类 | file:line | 前景 | 背景 | Light | Dark | 需要 |
|---|---|---|---|---|---|---|
| `.dayName`（周一~周日字母，选中格内） | `today:168` | `--text-4` | `--accent-soft` | **1.86** | **1.72** | 4.5 |
| `.count`（侧栏 wave 计数，选中行内） | `shell:230` | `--text-4` | `--accent-soft` | **1.86** | **1.72** | 4.5 |
| `.dayName`（普通格） | `today:168` | `--text-4` | `--surface-card` | **1.91** | **2.10** | 4.5 |
| `.kernelOwned` | `wave/page:170` | `--text-4` | `--surface-card` | **1.91** | **2.10** | 4.5 |
| `.cardNote`（"Card runtime lands in a later slice."） | `wave/page:176` | `--text-4` | `--surface-card` | **1.91** | **2.10** | 4.5 |
| `.hint`（"Appearance is stored on this device only."） | `settings:72` | `--text-4` | `--surface-card` | **1.91** | **2.10** | 4.5 |
| `.sectionTitle`（"COVES"） | `shell:114` | `--text-4` | `--surface-rail` | **2.03** | **2.33** | 4.5 |
| `.chevron`（▾ 折叠箭头） | `shell:157` | `--text-4` | `--surface-rail` | **2.03** | **2.33** | 4.5 |
| `.count`（侧栏 wave 计数） | `shell:230` | `--text-4` | `--surface-rail` | **2.03** | **2.33** | 4.5 |
| `.coveDelete`（×） | `shell:195` | `--text-4` | `--surface-rail` | **2.03** | **2.33** | 4.5 |
| `.lifecycle`（行内生命周期文字） | `row:80` | `--text-4` | `--surface-bg` | **2.07** | **2.30** | 4.5 |
| `.crumbSeparator`（/） | `wave/page:63` | `--text-4` | `--surface-bg` | **2.07** | **2.30** | 4.5 |
| `.cwd`（`/tmp/demo-b`） | `wave/page:105` | `--text-4` | `--surface-bg` | **2.07** | **2.30** | 4.5 |
| `.crumbSep`（/） | `settings:36` | `--text-4` | `--surface-bg` | **2.07** | **2.30** | 4.5 |

`--text-4` 的语义别名叫 `--text-decorative`（`tokens.css:40`）——**它确实是装饰色，但被 24 处当文字用**。最差的两处（1.72/1.86）是 accent-soft 上的 text-4，几乎不可读。

其余 49 对全部通过；最低通过值 light 4.74、dark 4.89，说明 `--text-2/--text-3` 的层级是健康的。**修 `--text-4` 一个 token 就清掉全部 28 个文本失败。**

### 6.2 非文本 / UI 边界对比度（需要 3:1）

| 对 | Light | Dark | 结论 |
|---|---|---|---|
| `--hairline` on `--bg` | **1.22** | **1.33** | FAIL |
| `--hairline` on `--surface-rail` | **1.20** | **1.35** | FAIL |
| `--hairline` on `--surface-card` | **1.13** | **1.21** | FAIL |
| `--hairline-strong` on `--surface-card` | **1.36** | **1.63** | FAIL |
| `--warn-border` on `--bg` | **1.55** | **2.06** | FAIL |
| `--accent-soft` on `--bg`（选中态色块） | **1.12** | **1.33** | FAIL |
| `--surface-card` on `--bg`（卡片与页面的分界） | **1.09** | **1.10** | FAIL |
| `--surface-chip` on `--bg` | 1.12 | 1.18 | FAIL |
| `--surface-rail` on `--bg`（侧栏与主区） | **1.02** | **1.01** | FAIL |
| `--accent` on `--bg` | 5.28 | 7.91 | ok |
| `--warn` on `--bg` | 4.47 | 6.78 | ok |
| `--success` on `--bg` | 4.61 | 8.86 | ok |
| `--error-text` on `--bg` | 6.37 | 7.93 | ok |

**这是"看起来平、看起来糊"的量化解释：** 侧栏与主区的底色差是 **1.02:1**（light）/ **1.01:1**（dark）——**肉眼几乎不可分**，全靠 1px 的 `--hairline`（对比度 1.20）撑着。卡片与页面 1.09:1。所有边界都远低于 3:1。整个界面没有任何层次深度：没有阴影（`box-shadow` 0 次）、底色不分、描边不可见。

`--accent-soft` 作为"选中"的唯一信号，与背景只差 1.12:1；选中的 cove 行之所以还能看出来，是靠 1px 的 `--accent` 边框，而不是那块底色。

---

## 7. 排版

### 7.1 有没有字号标尺

token 定义了 8 级（11 / 12.5 / 13 / 15 / 18 / 22 / 26 / 36），`--text-display-sm`(26) 未用。**但 UA 的 16px 通过 `font: inherit` 大量泄漏，实际渲染出的是一套 7 级的"非标尺"。**

单页面渲染出的不同字号数（computed，仅统计自身含文本的元素）：

| 页面 | 字号分布（size × 元素数） | 不同字号数 |
|---|---|---|
| `/` (Today) | 11px×15, 12.5px×13, **16px×7**, 13px×7, 36px×3, 15px×2, 18px×1 | **7** |
| `/cove/<id>` | 12.5px×9, **16px×9**, 11px×8, 15px×1, 22px×1 | **5** |
| `/wave/<id>` | 11px×19, 12.5px×7, **16px×5**, 15px×1, 22px×1 | **5** |
| `/settings` | **16px×10**, 11px×9, 12.5px×9, 15px×1, 22px×1 | **5** |

`16px` 在 Settings 页是**出现最多的字号**——它不是任何人设计的。

### 7.2 16px 泄漏的可见后果

`settings.module.css:119-166` 的 `.primary` / `.secondary` / `.radio` 只写 `font: inherit`，**没有 `font-size`**（对比 `cove/page:57`、`shell:128` 等处都补了 `font-size: var(--text-sm)`）。
computed, `/settings`：`.primary`(Save) 50.5×28 @ **16px**、`.secondary`(Reset) 55.8×28 @ **16px**、`.radio`(Light/Dark/System) 48.7/47.8/67.3×28 @ **16px**。
截图 `settings-light-desktop.png` 里这五个按钮明显比页面上任何其他控件大一号——它们看起来像**未被样式覆盖的原生浏览器按钮**，这是 Settings 页"最丑"的直接原因。

同样漏 `font-size` 的还有 `.nav`（`today:119`，渲染 16px）、`.coveDelete`（`shell:195`，16px）、`.action`（`row:130`，16px）、`.day`（`today:140`，16px，靠子元素覆盖）。

### 7.3 字重

`font-weight` 在 11 份 CSS 里只出现 **2 次**（`wave/page:121`、`today:160`，都是 600）。其余全部是 UA 默认：`<h1>/<h2>/<strong>` 得到 700，其他 400。
computed, `/settings`：400×26、**700×4**——那 4 个 700 是 UA 给标题标签的，不是设计决定的。
**没有字重层级，只有"标签碰巧是不是 heading"。**

### 7.4 行高

`--leading-*` 只被用了 4+2+1 = **7 次**（`--leading-none`×4 用于图标按钮、`--leading-snug`×2、`--leading-base`×1）。
其余全部 `line-height: normal`（UA）。中文正文在 `normal`（≈1.2）下明显偏挤——截图 `today-light-desktop.png` 的终端占位段落是全站唯一设了 `--leading-base` 的正文（`today:103`），也是唯一读起来正常的一段。

### 7.5 行长

`today .placeholderBody` 实测 **748px 宽 @ 12.5px 等宽** ≈ **每行 100+ 字符**（最佳 45–75）。
`wave .card` 实测 **1128px 宽**，内容是 `kind`+`title`+两个标签——`.cardNote` 被 `margin-inline-start:auto`（`wave/page:177`）推到最右，与标题之间隔着 **~950px 空白**（见 `wave-light-desktop.png`）。这不是排版，是把两个词钉在 1128px 的两端。

---

## 8. 布局

| 指标 | Today | Cove | Wave | Settings |
|---|---|---|---|---|
| 主列宽（computed） | 1152 | 1152 | 1152 | 1152 |
| 内容宽 | 1128 | 1128 | 1128 | 658（卡片） |
| `max-width` | 无 | 无 | 无 | 40rem（仅卡片） |
| 内容底边 y | 317 | **135** | **198** | 425 |
| 主列留白率 | 66% | **86%** | **79%** | 54% |

**Wave / Cove 页的问题，精确描述：**

1. **没有内容宽度上限。** 只有 `settings.module.css:53` 写了 `max-width: 40rem`；`wave/page.module.css:2`、`cove/page/page.module.css:2`、`today.module.css:2` 都没有。于是 wave 的卡片行被拉满 1128px。
2. **拉满之后再用 `margin-inline-start: auto` 把元信息推到远端。** `wave/page:177`（`.cardNote`）、`wave/row:81`（`.lifecycle`）、`shell:231`（`.count`）、`cove/page:45`（`.actions`）。结果：截图 `wave-light-desktop.png` 里 "wave-report / kernel-owned" 在左，"Card runtime lands in a later slice." 在右，中间 950px 空白；`cove-dark-desktop.png` 里 wave 标题在左、"Draft" 在 1379px 处。**眼睛要横扫整个屏幕才能把一行读完。**
3. **纵向只用了顶部一条带。** Cove 页所有内容在 y=135 之前结束，剩下 765px（**86%**）是空的；Wave 页 y=198 之前结束，剩 79%。内容既没有居中、也没有撑开、也没有分栏——它就是贴在左上角。
4. **页内边距 12px。** 1440px 视口下 12px 的页面内边距，加上 §0-B2 的 8px body margin，让内容从 x=20 开始——紧贴边缘。
5. **只有 Today 页有真正的布局**（`today:70-81` 的两列 grid，`minmax(0,1fr) 22rem`），也是四页中留白率最低（66%）、观感最完整的一页。其余三页没有 grid，只有一根 flex column。

**移动端（390×844）：** 见 §0-B5。侧栏未隐藏，占 255–449px 全宽死区；主内容被推到 y=263~457。`/wave` 上卡片行在 350px 宽里被 flex 挤成 `wave-report | wave-r… | kernel-owned | Card runtime lands in a later slice.` 四列换行（见 `wave-dark-mobile.png`），完全不可读。四条路由都无横向溢出（`scrollWidth == 390`）——这是唯一的好消息。

---

## 9. 十条最高杠杆的修复（按"可见改善 / 改动量"排序）

| # | 修复 | 改动量 | 可见效果 |
|---|---|---|---|
| **1** | **补 `base` layer**：`*,*::before,*::after{box-sizing:border-box}`；`html,body{margin:0;padding:0;height:100%}`；`body{font-family:var(--font-sans); font-size:var(--text-base); line-height:var(--leading-base); background:var(--surface-bg); color:var(--text)}`；`:root{color-scheme:light}` / `[data-theme=dark]{color-scheme:dark}` | ~15 行，1 个新文件 + `entry.css` 一行 import | 消灭深色模式的**白框**、消灭永久滚动条、消灭 Times New Roman、消灭 16px 泄漏（下条一起）、修好原生滚动条与表单控件的深色模式。**单条改动收益最大** |
| **2** | **修移动端侧栏**：把 `shell.module.css:14-22` 的 media block 移到 `.rail` 规则之后（或提高特异度） | 移动 9 行 | 390px 下侧栏不再占掉半屏；窄屏从"坏"变成"能用" |
| **3** | **`--text-4` 退出正文用途**：把 24 处 `color: var(--text-4)` 中用于文字的改为 `--text-3`，`--text-4` 只留给分隔符/描边 | ~20 行单值替换 | 清掉 **28 个 WCAG 失败**（light 14 + dark 14），整体从"糊"变清晰 |
| **4** | **给页面加内容宽度与呼吸**：`--content-max: 60rem`（新 token）+ `margin-inline:auto`；页面 padding 从 `--space-6`(12) 提到 `--space-9`(20)/`--space-10`(24)，根 gap 统一 `--space-8`(16) | 4 个 `.page` 规则，~12 行 | Wave/Cove 的 950px 横向空洞消失；留白率 86%/79% 降到合理区间；用上了目前完全空置的 space-8..10 |
| **5** | **补全局 `:focus-visible`**：base 层一条 `:where(a,button,input,select,textarea,[tabindex]):focus-visible{outline:2px solid var(--accent); outline-offset:2px; border-radius:inherit}` | ~4 行 | 25+ 个控件从 UA 黑环变成主题一致的可见焦点环；a11y 缺陷闭合 |
| **6** | **补边界层次**：`--hairline` 提到与底色 ≥3:1；给 `.card` 类加一层极轻 `box-shadow`；把 `--surface-rail` 与 `--bg` 的 1.02:1 拉开 | tokens.css 4 个值 + 2 条规则 | 侧栏/主区/卡片终于分得开；整个界面从"一张灰纸"变成有层次 |
| **7** | **补 `transition`**：base 层给交互元素加 `transition: background-color var(--motion-quick), color var(--motion-quick), border-color var(--motion-quick)` | ~3 行 | 21 处瞬时跳变变成缓动；"廉价感"消失。用上 5 个空置的 motion token |
| **8** | **统一控件高度与图标按钮尺寸**：定 `--control-sm: 24px / --control-md: 28px / --control-lg: 32px` 与 `--icon-btn: 24px`，把 11 种高度收敛到 3 种 | 新 token 家族 + ~15 处替换 | 侧栏、页头、Settings 的按钮终于同一水平线；Settings 的"原生按钮感"彻底消失（配合 #9） |
| **9** | **Settings 三个按钮补 `font-size`**：`settings.module.css:119/134/154` 各加 `font-size: var(--text-sm)` | 3 行 | Settings 页最刺眼的问题当场消失（16px→12.5px，与全站一致） |
| **10** | **统一色点尺寸与"删除"按钮**：定 `--dot-sm: 6px / --dot-md: 8px`，收敛 9 处；把四种删除按钮合成一个 `ui/` 组件 | ~12 处替换 + 1 个新组件 | cove 色点不再在四个位置四种大小；破坏性操作有统一语言 |

**排序理由**：#1 一条修掉 4 个"坏了"级缺陷；#2 修掉整个窄屏；#3 一个 token 清掉全部对比度失败；#4 修掉最刺眼的版面问题。前四条合计约 60 行改动，覆盖用户看到的绝大部分丑陋。#5–#7 是系统性补齐（焦点/层次/动效），#8–#10 是收敛工作，量大但每条单独可见性较低。

---

## 附：审计产物

- 截图 16 张：`{today,cove,wave,settings}-{light,dark}-{desktop,mobile}.png`
- `computed.json`：每个路由/主题/视口下所有可见元素的 24 个 computed 属性 + 字号/字族/字重直方图 + 焦点态实测
- `wcag.py`：OKLCH→sRGB→WCAG 比值计算（本文档所有对比度数字由它产出）

路径：`/tmp/claude-1000/-mnt-data2-kenji-neige-calm/b458771f-889c-4baa-b33b-1b0dbfd603c2/scratchpad/current/`

探针脚本 `fe/probe-audit.mjs`、`fe/web/probe2.mjs` 已按要求删除。
