# #1191 重构设计：手机端导航框架 v1（r3 定稿）

目标：把 PR #1191 的手机 IA 从「8 个组件 state + 1 条 window 事件总线」改成**单一所有者 + URL 承载导航身份**。
只搭框架，不动产品内容。基线：`1191-refactor`（已合入 origin/main，commit 6b25ed47，lint/typecheck/test 全绿）。

三轮双路评审。**被证伪的假设全部保留在案**，否则会重犯。

---

## 0. 前两轮被证伪的假设

### 0.1 r1：「手机卡片详情复用既有 `?card=`」——错

- 手机详情渲染 `panelCards`（`router/public.tsx:1426-1431`）= visible **+ unknown**；
  `?card=` 合法集是 `gridItems`（`:1432-1444`）= **只有 visible**。
  `knownCard`（`:1466`）不命中时 `:1468-1471` 的 effect 直接 `replace` 掉 `?card=`
  → **unknown card 的详情页在 URL 模型下永远打不开**，而现在能打开。
- 且 `?card=` 命中 ⇒ `knownCard` ⇒ `onCloseBoard` 有值 ⇒ `boardOpen` 为真 ⇒
  `wave/page/public.tsx:127-133` 强制关闭手机面板。`?panel=cards&card=y` 是自相矛盾的 URL。

⇒ 手机卡片详情 v1 **保持组件 state**，显式豁免。

### 0.2 r1：「`/pages`、`/coves` 变成真路由」——错

它们是有完整 a11y 契约的模态层（`shell/public.tsx:222-228` 的 `role="dialog"`/`aria-modal`，
`:127-151` 的 focus trap + Escape + Tab 环绕，`:296` 的 `inert`，且 `responsive.contract.test.tsx:41-56` 正在断言）。
路由化会额外产生：a11y 契约整套消失、`/cove/$coveId` 与手机二级列表不等价
（`features/cove/page/public.tsx:76-166` 有文档空态/会话/重命名且无 compact 分支）、
桌面凭空多两个产品面、以及 `<div key={currentPath}>`（`shell/public.tsx:302`）导致
`/wave/x → /pages → 回来` 整树重挂载丢滚动位置——**真实体验回退**。

⇒ **导航目的地进 URL，瞬时覆盖层不进 URL。** 这是本设计的中心原则。

### 0.3 r2：「panel → report 用 replace，只留一条重复项，无副作用」——错

两路各自读 `@tanstack/history` 源码确认：`replace` 只覆盖当前 index，**不与前一项合并**。
`R → P(push) → R(replace)` 得到 `[R, R]`；每次「开面板→关面板」循环**净增一条同 URL 条目**。
开关三次后按硬件返回要按四次才离开报告，前三次画面无变化——静默的无界增长，比 r2 想避免的更糟。

⇒ 见 §1.1 的 marker 双分支方案。

### 0.4 r2：secondary 三元公式——逻辑错误

r2 写成 `onWaveRoute ? (section === null) : (section === 'coves' && ...)`。
从 `/wave/x` 点开 Coves 再选 cove 时 pathname 仍是 wave，第一分支返回 false 并**整个跳过 cove 分支**
⇒ Coves 二级页会露出 dock。现行为本就是两个条件 OR（`shell/public.tsx:109-115`）。见 §2.1。

### 0.5 实现阶段：双路评审结论相反，靠**执行**裁决

实现完成后跑实现级双路评审，**第一路判定「无阻塞，可以合并」，第二路找到两个真实运行时缺陷**。
第二路是对的，而且它是跑出来的不是读出来的。两条都已复现并修复：

1. **非法 card 回弹会吞掉 `panel`**（违反 §1.4）。
   `waveSearchFromLocation()` 在**解析原始 location 时**就执行 card/panel 互斥，
   把 `{card:bad, panel:tasks}` 归一成只剩 card；随后 patch `{card: undefined}` 时 panel 已无从恢复。
   实测 `sameWaveSearch({searchStr:'?card=bad&panel=tasks&from=cove'},'w1',{card:undefined})` 返回 `{from:'cove'}`。
   **互斥必须在重建输出时执行，不能在解析阶段。**
   原有测试只覆盖「重复 card 被拒后保留 panel」与「设置 card 时清 panel」，恰好绕开这个交叉点。

2. **`?panel=` 让桌面侧栏「可见但键盘/读屏不可达」**。
   `desktopPanelSurface` 只要 URL 带 `panel` 就被打 `aria-hidden` + `inert`，**无任何视口条件**，
   而桌面上手机面板本身是 `display:none`。分享的深链在桌面打开、或手机开着面板变宽都会踩到。
   修复需**两半**：变宽时从 URL 清掉 `panel`（保持 URL 诚实）**加上**注入处按视口把关（深链冷启动时 effect 还没跑）。

> **测试可观测性的边界（重要）**：注入处的视口把关在 jsdom 里**不可观测**——清理 effect 在 `act` 内 flush，
> 测试拿不到那一帧；实测删掉视口门槛后 15 条集成用例**全绿**。
> 因此该决策被提成纯函数 `renderedMobilePanel()` 并在单测断言。
> 教训：**集成测试全绿证明不了一个它在时序上根本观察不到的分支。**

### 0.6 跨特性冲突：#1173 与 #1191 的产品决策直接对撞

`browser-coarse` 在 `origin/main` 上全绿（21 passed），在本分支 3 red，**引入点是合并提交本身**，
不是任何一刀实现（已二分确认；也已排除 `vitest.config.ts` 的 `dedupe` 与 `optimizeDeps`，两者都不是元凶）。

失败断言是 `expected +0 to be 28`——**测到 0，元素根本没渲染**。
根因：`browser-coarse` 在**手机视口**下断言 #1173 exchange 轨的触控几何；
而 #1191 的移动 IA 要「手机全屏 Chat 隐藏该桌面侧轨」，由 `ui/drawer` 的 `@media (width < 60rem)` 实现。
本 PR **完全没碰** `features/chat/`。两个特性单独都绿，合起来必红。

**裁决**：改 #1173 coarse 测试的视口为 coarse-pointer + 宽屏，不回退 #1191 的隐藏。
`pointer: coarse` 指触摸，不等于窄屏；#1173 真正的不变量（24×28、28px pitch、320px cap）在宽屏 coarse 下依然成立。

> **实施时又精确了一层**：`@media (width < 60rem)` 判的是 **Vitest 给每个 suite 的 iframe**（默认 414×896），
> **不是** Playwright context 的 viewport。只改 `contextOptions.viewport` 反而变成 4 red；
> 必须**两个都设**（`contextOptions.viewport` 管页面/`screen`，`browser.viewport` 管 iframe/媒体查询）。
> 选**竖屏** 1024×1366 而非横屏：文件里 `@media (pointer: coarse) and (orientation: landscape)` 那条 fixture
> 的全部价值在于「此处 false、转屏即 true」，横屏会让它在此 match 从而失去约束力。

并**补了一条反向断言锁住手机视口下该轨不存在**（`ui/drawer/mobile.browser.test.tsx`）——
这次冲突之所以表现成一个看不出来源的 `+0`，正是因为 #1191 的这个决策只活在 CSS 和 PR 描述里，没有测试守着。

---

## 1. v1 的 URL 模型

| 层级 | 承载 | 状态 |
|---|---|---|
| Today / Cove / Wave / Settings | 既有路由 | 不动 |
| Report 二级面板 | `?panel=outline\|cards\|tasks\|conversations` | **新** |
| Report 返回来源 | `?from=pages\|cove` | **新** |
| 块锚点 `#$blockId`、桌面 card overlay `?card=` | 既有 | 不动 |
| 手机卡片详情 / Pages·Coves 覆盖层 / Pages 的 Pinned·Recent 分组 | 组件 state | **显式豁免** |

三项豁免**都要在代码注释里写明理由**，否则下一个人会以为是漏了。

### 1.1 面板开关的 history 策略（marker 双分支）

已核实 `@tanstack/history/dist/esm/index.d.ts:24` 有 `canGoBack()`，
`@tanstack/react-router/dist/esm/index.d.ts:39` 导出 `useCanGoBack`，且 navigate 支持 `state`。

| 转移 | 策略 |
|---|---|
| report → panel | `push`，并写 `state: { ncPanelPushed: true }` |
| panel A → panel B | `replace`（保持 marker） |
| panel → report | marker 存在 **且** `canGoBack()` ⇒ `router.history.back()`；否则 `replace` |
| report → 别的 wave | `push`（既有行为不变） |

`back()` 分支保证不留重复项；`replace` 分支覆盖冷启动深链（分享出去的 `?panel=cards`），
**绝不无条件 `history.back()`**——那会直接退出应用。

测试必须**执行一次 `history.back()` 并断言每一步的 location**，只断言 `history.length` 抓不到 `[R,R]` 缺陷。
读 `router.history.length`，不是 `window.history.length`（jsdom 里后者恒为 1）。

### 1.2 `?from=` 语义

缺省（无 `from=`）回落 **`pages`**，即当前默认值（`shell/public.tsx:112`），避免行为回归。
`from=cove` 返回时恢复到哪个 cove 由 `coveIdOf(waveId)` 从 workspace 派生（`shell/public.tsx:169-170` 已有映射）
⇒ **`mobileCoveRestoreId` 直接删除，派生优于存储**。

### 1.3 两个出口，职责分开

r2 只给了「保留」的出口，没有任何出口能**写入** `from`——而 `from` 的唯一写入点是跨 wave 导航
（`shell/public.tsx:242` MobilePages、`:254` MobileCoves）。必须两件事分开：

1. **`NavTarget` 的 wave 分支增加 `panel?` / `from?`**，`useGo` 的 search 构造扩成三字段**显式构造**。
   纪律不变：未传即清空，防止参数跨 wave 泄漏（`navigation.ts:45-49` 的既有理由）。
2. **`useGoSameWave(expectedWaveId, patch, options?)`** 只负责**保留**，不负责设置：
   - 携带 `expectedWaveId`，与当前 wave 不符时等同 `go()`（这样「去掉同 wave 判据」才有可变异对象）。
   - **绝不 `search: prev => ({...prev, ...patch})`**——那会继承未知参数、非法数组与重复 key，
     破坏白名单式重建。必须从原始 location **重新解析**并只重建白名单三字段 `card`/`panel`/`from`。
   - 用 own-property 区分「未提及」与显式 `{ panel: undefined }`。
   - 定义 `card` 与 `panel` 的互斥：`card` 存在时 `panel` 必须清空（§0.1 已证明二者矛盾）。

### 1.4 逐调用点决策表（含 hash，实现时逐条核对）

`grep -n "name: 'wave'"` 全仓，共 9 个调用表达式：

| 调用点 | panel | from | hash |
|---|---|---|---|
| `router/public.tsx:1472` 非法 card 回弹 | 保留 | 保留 | 保留（现行为清空，是回归修复） |
| `:1488` 同 wave report link | 清空 | 保留 | 按既有 |
| `:1491` 跨 wave report link | 清空 | 清空 | 按既有 |
| `:1496` Outline/Task 锚点 | 清空 | 保留 | 保留 |
| `:1518` 建 terminal 后开 card | 清空 | 保留 | 按既有 |
| `:1525` 桌面开 card | 清空 | 保留 | 按既有 |
| `:1533,1538` 关 card | 清空 | 保留 | 按既有 |
| `:1561` backlinks | 同 wave 保留 from / 跨 wave 全清 | | 按既有 |
| `shell/public.tsx:214` 新建 wave 后跳转 | 清空 | 清空 | 清空 |

`:1488` 与 `:1491` 现在是**两个分支同一句 `go()`**，改造时必须拆开。
新增出口（4 个 `openPanel`、各 Back、Escape、shell 的 Pages/Coves 清 panel）同样逐个决策。

### 1.5 解析器

`panel`/`from` 解析器与 `cardIdFromSearchString` 并列，**复用同一条「重复 key 即拒绝」规则**，
非法值一律丢弃不抛错（URL 是用户可编辑输入）。`validateSearch`（`router/public.tsx:415-422`）相应扩展。

---

## 2. 删除的平行机制

### 2.1 `ui/mobile-page` 事件总线整个删除

| 事件 | 现调用点 | 替代 |
|---|---|---|
| `mobile-page-root` 发布 | `shell/public.tsx:326,349` | shell 直接 `setMobileSection(...)` + `useGoSameWave({panel: undefined}, {replace:true})` |
| `mobile-page-root` 订阅 | `wave/page/public.tsx:149-153` | 面板由 props 驱动，订阅消失 |
| `mobile-secondary` 发布 | `ui/mobile-page:4-6`、`mobile-coves.tsx:25-26`、`wave/page/public.tsx:155-158` | shell 派生（见下） |
| `mobile-secondary` 订阅 | `shell/public.tsx:153` | 同上 |

**正确公式（两个条件 OR，不是三元）**：

```ts
const secondary =
     (onWaveRoute(currentPath) && mobileSection === null)
  || (mobileSection === 'coves' && selectedCoveId !== null);
```

同时删掉 `shell/public.tsx:115` 的 `currentPath.includes('/wave/')`，改用 `routeParamFromPath`。

### 2.2 `selectedCoveId` 上提到 shell：必须连同转移语义一起搬

`mobile-coves.tsx:21` 还有一个 `motion` state，`:30-33`/`:60-62` 与 `selectedCoveId` 的每次转移严格耦合。
**只上提 id 会把一次转移拆给两个所有者**，正是本设计要消灭的形状 ⇒ `motion` 改为由 id 变化派生，或一并上提。

上提后**丢失了「组件卸载即重置」语义**。
只有 `from=cove` 返回时才由 wave 自己的 `coveId` 设置。
`mobile-coves.tsx:25-26` 的卸载清理可安全删除——新公式已把 `mobileSection === 'coves'` 作为合取项。

> **实现更正（本节原文写错，勿照旧文档回退）**
>
> 原文要求「在六个出口显式清空 `selectedCoveId`」**并且**「dock 点 Coves 回到根列表」。
> 两者叠加使后者**不可达**：出口都清空后，没有任何可达状态能让 dock 在 selection 尚存时被按下，
> 于是守卫它的测试只能用 `fireEvent` 去点一个 `inert` 的 dock——**一条空断言**。
>
> 落地实现改为 **只在进入时重置**（`openMobileSection`，commit `93605565`），
> 出口只 `setMobileSection(null)`、不清 selection。可证成立：`setMobileSection(非 null)` 的唯一写点就是
> `openMobileSection`，且渲染与 secondary 公式都合取了 `mobileSection === 'coves'`，故残留 selection 不可观察。
> 这样那条产品规则重新变成**可达且可用 `userEvent` 验证**的。
>
> 教训与 §0.4 同类：**一条「此后 X 不得发生」的断言，必须能指出哪个可达状态会违反它。**
>
> 另：§1.2 的 `coveIdOf(waveId)` 也不必要——`WaveRouteBody` 手上就有 `wave.coveId`，
> 同一事实且不会在 workspace 加载中途查空。实现用了后者。

### 2.3 死掉的 state、context、死分支

删：`mobileReportSource`、`mobileCoveRestoreId`、`mobileSecondaryOpen`、`MobileReportNavigationContext`。
留：`mobileSection`、`mobileCardId`、`mobileCardMotion`（§0.1 豁免）。
`wave/page/public.tsx` 的四个 setter 连写出现 9 次（`:167-202` 4 次、`:361-468` 5 次）收敛成 `openPanel(kind)` / `closePanel()`。
删死分支 `shell/public.tsx:256,258`（`<Sidebar>` 只在 `narrowRail === false` 时渲染，两处恒真/永不执行）。

**更正**：删掉 `MobileReportNavigationContext` **不会**减少 createContext allowlist 条目——
allowlist 按文件只有一项，理由本就是仍保留的 New-wave context（`tools/architecture/allowlists.mjs:18-22`）。S8 移出范围。

### 2.4 分层（硬约束）

`.dependency-cruiser.cjs:9` 的 `features-no-app` 是 **error 级**，`WavePage` 不能 import `app/**`
（文件头 `wave/page/public.tsx:9-15` 也明写导航只能走 callback）。
⇒ URL 一律由 `WaveRouteBody`（app 层）读取校验，把 `panel`/`mobileBackLabel`/`onOpenPanel`/`onClosePanel`/`onMobileBack`
作为 props 注入；`WavePage` 保持纯渲染。`:135-147` 的 Escape 改为调用注入的 `onClosePanel`。

### 2.5 焦点契约（纳入 v1，不留给实现期自由发挥）

- 开面板 ⇒ 焦点进面板容器（对齐 `shell/public.tsx:129` 的既有做法）。
- 关面板 ⇒ 焦点回三点菜单按钮（opener restore）。
- 冷启动深链带 `?panel=` ⇒ 首次渲染焦点进面板容器。
- 硬件返回（POP）关闭面板 ⇒ 同「关面板」。

---

## 3. 收敛与正确性

### 3.1 `userVisibleWaves(waves, coves)` 下沉 core —— 动机更正

**不是现网泄漏**：`coveListQueryOptions` 已在查询层 `visibleCoves`（`providers/queries.ts:220`），
`useWorkspace` 只对这些 cove fan-out（`:414`）。真实问题是**组件边界的第二层防御 sidebar 有、Pages 没有**
（`sidebar.tsx:106-108` vs `mobile-pages.tsx:23-24`），与 `cove.ts:55-63` 声明的意图不一致。
收敛成 core 纯函数两处共用。测试构造 system cove + 其下 wave，断言不出现。

### 3.2 `ui/viewport/public.ts` —— 唯一的 `useCompactViewport` + 收窄的门禁

替换三份复制（`drawer:20-33`、`today:120-131`、`shell:108,117-124`）。

duplication-manifest 登记**是空门禁**：`exportedNames()` 只识别导出同名符号，
而三份里两份是内联 `useState + matchMedia`、一份是未导出本地函数。

**规则必须收窄成两条独立 alternation，各配独立单违规 fixture**（`fe/AGENTS.md:19-23`：
只覆盖一个分支的 fixture 在另一分支被删时仍会全绿）：

- (a) 禁止 `ui/viewport` 之外 import `RAIL_COLLAPSE_QUERY`（`styles/breakpoints.ts:9`）。当前 3 处违规。
- (b) 禁止 `ui/viewport` 之外调用**参数含 static width media query** 的 `matchMedia`；豁免 `*.test.*`。

**不能笼统禁 `matchMedia`**——已核实 8 处合法用法：`app/theme/public.tsx:26,27,35,37`（prefers-color-scheme）、
`ui/drawer/public.tsx:194`（prefers-reduced-motion）、`features/chat/thread/thread.coarse.browser.test.tsx:260,432-435,698`（pointer）。
需在规则里写明「动态表达式会静默逃逸」。
实现先例可抄：`tools/architecture/plugin.mjs:8-17`、`no-direct-persistence.mjs:48`（filename 豁免）、
`eslint.config.js:57-71`（带 options 的路径 allowlist）、`architecture-rules.test.ts:35-48`（fixture harness）。

### 3.3 `DOCK_ITEMS` + `dockSelection()`

`Object.freeze` 必须**深冻**：数组内每个对象也要冻，否则 `no-module-runtime-state.mjs:91-112,185-210` 报错。

`aria-controls` / `aria-expanded` **保留且正确**——Pages/Coves 确实控制着 `#mobile-workspace-navigation`，
Today/Me 是路由跳转本就不该有。用可选字段 `opensSection` 驱动这个差异。
（r1 把「Today/Me 漏了 aria-controls」当缺陷要补齐，方向是反的，此处更正。）

`shell/public.tsx:115,182,183` 三处路由字面量换成 `pathFor()`/`routeParamFromPath()`。
CSS 写死的列数 `4`（`shell.module.css:216`）由 `DOCK_ITEMS.length` 经 CSS 变量承载。

### 3.4 `--mobile-dock-h`：最小修复 + 真验收

**不升格为 token**：token 名集合被 `styles/tokens.contract.test.ts:105-116` 精确锁定，
`StyleToken` 是闭集（`styles/public.ts:62-82` + `public.contract.test.ts:22-78`），
且 `styles/` 目录 `readonly: true`（`module-file-inventory.yaml:120`）；
且 `:root` 静态默认值承载不了 `.shellMobileSecondary` 的归零那一半（`shell.module.css:168`）。

v1 只做真 bug 修复：消费处一律 `var(--mobile-dock-h, 0px)` 兜底。
消费点共**三处**：`drawer.module.css:404,406` 与 `page.module.css:945,951,952`（r2 漏了后者）。

**验收标准必须重写**：`drawer/mobile.browser.test.tsx:15` 的内联 `blockSize` 在 `<main>` 上，
而 mobile 下 drawer 是 `position: fixed`（`drawer.module.css:399-404`），fixed 的包含块是视口，
**父高度对它无影响** ⇒ 删掉那行修复前后都可能通过，证明不了任何事。
真验收：把 `<Drawer open>` 渲染在 `.shell` **之外**，`getComputedStyle` / bounding rect 断言
`block-size` ≈ 视口高与 `bottom` 位置；变异「去掉 `, 0px` 兜底」必须红。

---

## 4. 测试策略

**B3 是验收前提**：现有 11 张截图挂在 `MobileShellFrame`（`mobile.browser.test.tsx:22-90`）这个 AppShell 副本上，
删掉 `shell/public.tsx:314` 的 `inert` 仍全绿。重构后必须渲染真实 `AppShell`。

**可抄的真实 router 先例**（r2 引用错了）：`responsive.contract.test.tsx:9,20` 把
`@tanstack/react-router` 和 `navigation.ts` **整个 mock 掉**（`useGo: () => vi.fn()`），证明不了可驱动真实 router。
正确先例是 `app/router/wave-cards-panel.test.tsx:161-163` 与 `read-fallbacks.contract.test.tsx:27-30`：
`createAppRouter` + `router.update({ history: createMemoryHistory(...) })` + `RouterProvider`，
而 `createRootRoute` 渲染的就是 `ShellRoute → AppShell`（`router/public.tsx:394`），整条链路天然可达。

保留 mock 版 `responsive.contract.test.tsx` 不动（它的低成本 `inert` 断言有价值），新集成测试另起文件。

三层：**纯函数单测**（解析器含重复 key/非法值/空值、`dockSelection`、`useGoSameWave` 同/跨 wave 判据、返回标签、`userVisibleWaves`）
→ **jsdom 集成**（真实 AppShell + memory history，断言 URL 与逐步 back 的 location）
→ **browser 截图**（只保留几何，渲染真实 AppShell）。

**变异验证（须登记进 `fe/tools/mutation/manifest.json`——runner 精确校验 expected-red 集，
见 `tools/mutation/run.mjs:65-71`）**，每条记录**哪条**测试红了：

1. 删 `validateSearch` 的 `panel` 校验 → 非法值测试红（**测试必须真的经过 route validated search 才会红**）。
2. `panel A → panel B` 的 `replace` 改 `push` → history 断言红。
3. 关面板的 `back()` 分支改成无条件 `replace` → **重复条目断言红**（守卫 §0.3 的修复）。
4. `useGoSameWave` 去掉同 wave 判据 → 跨 wave 参数泄漏测试红。
5. `userVisibleWaves` 退化成 `visibleWaves` → system cove 测试红。
6. 删 `shell/public.tsx:296` 的**模态** `inert`（不是 dock 的 `:314`）→ contract 测试红。
7. 去掉 `var(--mobile-dock-h, 0px)` 的 `, 0px` → drawer 几何断言红。

---

## 5. 范围边界

**v1 做**：§1–§4 全部（含 §2.5 焦点契约）。

**v1 不做**：B2 Today 内容恢复（但**必须删掉** `today/mobile.browser.test.tsx:24-27` 的
**4 条**反向断言：Waiting on you / Running / Recent / Terminal——把功能缺失固化成通过条件，是地雷）；
S5 WavePage 双实现收敛（§0.1 证明 card 详情绕不过它）；`/pages`·`/coves` 真路由化（§0.2，前置条件是 CovePage 的 compact presentation）；
S6 响应式范式、S8 allowlist 粒度（§2.3 已说明无关）、S9 `.page > :first-child` seam、N1 `!important`、N2 `--touch-target`、N5 optimizeDeps。

**预估规模**：~700–900 行。

---

## 6. 门禁与冻结清单（实现前逐项确认）

1. **只读 inventory**（`module-file-inventory.yaml` 自身 `readonly: true`，`:9`）：删 `ui/mobile-page` 条目、加 `ui/viewport`。
2. **新增 ESLint 规则要改冻结的 `fe/eslint.config.js`（`:14`）与 `fe/package.json`（`:17`）的 `lint:js` 串。**
3. 上述每个触碰冻结文件的提交都要带 `OWNERSHIP-CHANGE: <exact path> — <理由> (#1191)` trailer。
   **squash 合并会丢 trailer**，压缩前必须逐条捞出带进 body。
4. `ownership-manifest.mjs` 动态读 YAML，**不需要**手工再生成。
5. `tools/test-tier/checker.ts:82-91` 要求每个 tracked 测试文件**恰好命中一个** test project ⇒
   新增 jsdom 集成测试与 rule fixture 的命名必须落在既有 include glob 上，否则 `check-test-tier` 直接红。
6. **路由 contract 测试**（`router/public.contract.test.tsx:59-65,87-99`）精确锁定「四条路由 / 其他无 loader」。
   v1 不加路由 ⇒ 这条**不该动**；若它红了说明有人偷加了路由，是信号不是噪音。
7. 不新增 `<a href>`，遵守 INV-A11Y-061（`navigation.ts:27-35`）。
8. 提交信息**不得**含 `Co-Authored-By`。
