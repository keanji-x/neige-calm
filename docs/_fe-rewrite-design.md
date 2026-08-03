# `fe/` 前端重写 — 框架与规范设计

状态：**草案 v1，待双通道 review**
作者：orchestrator
日期：2026-08-02

---

## 0. 决策与前提

### 0.1 决策

在 `fe/` 下**并行重建**前端，独立开发、mock 后端，完成后一次性切换。现存 `web/` 冻结、继续服务生产，切换前不做结构性改动。

目标形态：`fe/web`（桌面，先做，对齐现有 web）+ `fe/mobile`（移动，后做，只读/轻交互）+ `fe/common`（共享逻辑层）。

### 0.2 已量化的现状（重写规模的事实基础）

| 项 | 数字 |
|---|---|
| 实现代码（排除 test / generated） | 26,596 行 |
| 测试代码 | 37,363 行 |
| 生成代码（OpenAPI / ts-rs） | 6,134 行 |
| `calm.css` | 6,392 行，599 个类 |
| 带 `className` 的实现文件 | 58（共 72 个 tsx），695 处 |
| **路由** | **4 条**：`/`、`/cove/$coveId`、`/wave/$waveId`、`/settings`（+ Login） |
| 卡片类型 | 6：terminal / codex / spec / iframe / file-viewer / wave-report |
| report block 类型 | 5：prose / chart.candles / table / app / task |

CSS 行数按域分布（前 10 占 60%）：
```
report 859   wave 663   side 541   file 359   today 301
cal    298   rb   251   card 245   new  189   dirpicker 153
```

**这是重写可行性的关键**：路由面极小（4 条），CSS 高度按域聚集，切片边界天然清晰。

### 0.3 必须承接的既有资产（不可丢失）

现存 `web/` 的 37,363 行测试锁定了一批**踩坑得来的不变量**。重写**不是**从零设计，这些必须逐条翻译进新 fe 的 contract test：

| 不变量 | 出处 | 为什么不能丢 |
|---|---|---|
| Menu 选中时**同步** restore 焦点到 trigger，且**先于** `onSelect` | `ui/Menu/Menu.contract.test.tsx` | 否则 `onSelect` 里打开的 Dialog 会快照到即将卸载的 menuitem，关闭时 restore 失效 |
| Menu 外部点击关闭**不**抢回焦点 | 同上 | 用户已主动点向别处，抢焦点是敌意行为 |
| ConfirmDialog 默认焦点在 Cancel，rapid-Enter 不得触达 Confirm | `ui/ConfirmDialog/ConfirmDialog.contract.test.tsx` | `window.confirm()` 做不到，这是破坏性操作的护栏 |
| Dialog 打开时背景兄弟节点 `inert` + `aria-hidden` | `ui/Dialog/Dialog.test.tsx` | 焦点陷阱的正确实现 |
| 主题切换首帧不闪烁（`<html data-theme>` 在首次 paint 前就位） | `app/theme.tsx` | |
| 持久态不得进入 `useState`（`Persistent<T>` 品牌） | `eslint-rules/no-persistent-in-usestate.cjs` | |
| token light/dark 双向对等 | `calm-tokens.test.ts` | |

> **给 review 的问题**：这张表是我从代码里扫出来的，很可能不全。review 的一项职责是补全它。

### 0.4 技术栈：只换规范，不换栈

保持 **React 19 + Vite + TanStack Router/Query + zod**。理由：
- Tauri 是 WebView 壳，SSR 零价值，Next.js 是纯负担
- 栈一致 → 老代码可作 oracle 逐行对照，知识可迁移，失败时代码能捡回来

**变的是三样**（也正是重写的全部意义）：
1. CSS Modules 从第一天（替代 6,392 行全局 CSS）
2. 组件目录自足（样式/类型/测试同目录）
3. 移动优先的布局规范（`dvh` / `@container` / safe-area），而非从桌面缩放

Astryx 是否引入，取决于 spike 结论（见 §7）。

---

## 1. 目录契约

```
neige-calm/
├── web/                          # 冻结。生产在跑，切换前不动
└── fe/
    ├── AGENTS.md                 # 全域规范入口
    ├── package.json              # workspaces root
    ├── common/                   # 逻辑层 — 无 DOM、无样式、无 JSX
    │   ├── AGENTS.md
    │   ├── api/
    │   │   ├── generated.ts       # ← gen:api 产出，勿手改
    │   │   ├── generated-events.ts
    │   │   ├── client.ts          # fetch 封装、错误规范化
    │   │   └── schemas.ts         # zod 边界校验
    │   ├── domain/                # wave / cove / report / block 模型与纯函数
    │   ├── query/                 # queryKey 工厂、invalidation 策略
    │   └── tokens/                # design token 单一源（导出为 CSS 变量 + TS 常量）
    ├── mock/                      # 从 openapi.json 生成的 mock server
    │   ├── AGENTS.md
    │   ├── generate.ts            # 生成器（不手写 handler）
    │   └── scenarios/             # 具名场景数据集
    ├── web/                       # 桌面端
    │   ├── AGENTS.md
    │   └── src/
    │       ├── app/               # router / providers / eventBridge / theme
    │       ├── ui/                # 交互原语（Dialog/Menu/…），一 primitive 一 ARIA pattern
    │       ├── shell/             # Sidebar / TitleBar / 布局骨架
    │       ├── report/            # report 页 + 5 种 block 渲染器
    │       ├── wave/  cove/  today/  settings/
    │       ├── cards/             # 6 种卡片
    │       └── styles/            # 全局层：reset、token 注入、第三方覆盖
    └── mobile/                    # 移动端，S1 完成后才开工
        └── src/{app,shell,report,ui,styles}/
```

### 1.1 `common` 的边界（**待你确认的关键决策**）

**本设计的立场：`common` 放逻辑，不放 UI 组件。**

- ✅ 进 common：API client、生成类型、zod schema、domain 模型与纯函数、queryKey 工厂、invalidation 策略、design token
- ❌ 不进 common：任何 `.tsx` 组件、任何 `.module.css`

理由：桌面（hover / 右键 / 多栏 / 拖拽）与移动（tap / 手势 / 单栏 / sheet）交互模型不同。共享 UI 组件会长出 `isMobile` 分支，两端都被拖累——这是跨端项目的经典失败模式。

**"common 组件规范"按规范共享而非代码共享落地**：同一套 token、同一套命名法、同一套契约模板（§3.3），两端各自实现像素。

> ⚠️ 这与你原话"一些 common 组件规范"可能有出入。若你要的是**组件代码**共享，`common` 的边界要重划，且需要接受上述代价——请在 review 时明确。

### 1.2 类型契约同源

`gen:api` 只产出到 `fe/common/api/`，`fe/web` 和 `fe/mobile` 都从 `common` import。**不允许任何一端自建类型副本。**

切换前的过渡期，`web/`（老）继续用自己那份 `web/src/api/generated.ts`，由同一条 `gen:api` 脚本同时产出两处，保证不 drift。

---

## 2. 依赖方向与 lint 强制

```
      mock ──────┐
                 ▼
  common ──► web  /  mobile
     ▲
     └── 不得反向依赖任何 UI 包
```

层内方向（`fe/web/src` 内部）：
```
app  ──►  shell / report / wave / cove / today / settings / cards
              │
              ▼
             ui        (交互原语，不得 import 任何业务层)
              │
              ▼
           common
```

**每一条都必须有对应 lint，否则 agent 会违反它。** 规范无 lint = 规范不存在。

| 规则 | 实现 |
|---|---|
| `common` ✗→ `web`/`mobile`/`ui` | `no-restricted-imports` zones |
| `ui/**` ✗→ `report`/`wave`/`cove`/`today`/`cards`/`common/domain` | 同上 |
| 业务层 ✗→ 其他业务层（横向） | 同上，跨域只能经 `app` 或 `common` |
| 禁止 barrel file（`index.ts` 纯转出） | 自定义规则（见 §3.2） |
| 禁止 import `.css`（除 `styles/` 全局层） | 自定义规则，强制 CSS Modules |
| 禁止裸 `role="dialog"/"menu"/"menuitem"` | 移植 `no-raw-primitive-role.cjs` |
| 禁止 `Persistent<T>` 进 `useState` | 移植 `no-persistent-in-usestate.cjs` |
| `useState`/`useReducer` 只能从 `@/state` import | 移植 `no-restricted-imports` + `no-react-state-hook-members.cjs` |
| CSS 内禁止颜色/间距字面量 | 移植 `.stylelintrc.cjs`（CSS Modules 仍是纯 CSS，闸门原样可用） |

---

## 3. AI 友好的前端规范

核心原则：**agent 只靠文件路径就能定位全部相关信息，不需要全局理解。**

### 3.1 局部性 > DRY（最重要一条）

一个组件的实现、样式、类型、测试在**同一目录**：

```
report/blocks/task/
  task.tsx
  task.module.css
  task.types.ts          # 仅当 props/payload 类型复杂到值得分文件
  task.test.tsx          # 行为
  task.contract.test.tsx # 不变量（见 §3.3）
  AGENTS.md              # 仅当该目录有非显然的约束
```

agent 要改 `ReportTaskBlock` 时，不该需要在 6,392 行全局 CSS 里搜 `rb-task` 再判断有没有别处覆盖它。**这是 CSS Modules 的真正价值——不是作用域，是可定位性。**

推论：**宁可重复，不要为复用而抽象到远处**。同一段 40 行样式在两个组件里各写一份，比抽到 `shared/styles.css` 让 agent 跨文件推理要好。抽象的门槛是"第三次出现且语义相同"。

### 3.2 命名可推导，禁止 barrel

- 路径 = 组件名 = module 名：`report/blocks/task/task.tsx` 导出 `ReportTaskBlock`，样式在 `task.module.css`
- **禁止 barrel file**：`index.ts` 纯转出会让 agent 追 import 多跳一层，且掩盖循环依赖。import 一律写全路径
- 例外：`report/blocks/index.tsx` 这类**有实际逻辑的分发器**（现有代码里它做 kind → 渲染器的 switch + zod 校验）不算 barrel，允许

### 3.3 契约文档贴在代码旁

现存 `web/src/ui/README.md` **是这条规范的最佳范例**，直接升格为全域模板。它的好处：三段式结构、每条规则写了"为什么"、明确列出"故意不做什么"。agent 拿它当上下文的质量远高于任何自动生成的文档。

每个 `ui/` primitive 和每个 report block **必须**写全三段：

```markdown
### Visual contract
消费哪些 token；是否新增 class；变体如何暴露（prop，不是调用点内联样式）

### Accessibility contract
role 与 accessible name 的计算方式；拦截哪些键；焦点的初始/恢复/陷阱；
**明确写出"故意不做"的部分**（例：sub-mode 下禁用 click-outside）

### Test contract
测试用什么选择器找到它（一律 `getByRole(role, {name})`，永不 `data-testid`）；
单测锁了什么；什么下放给 e2e
```

### 3.4 测试即规格：contract test 与 behavior test 分离

- `*.test.tsx` — 行为：渲染正确、交互触发正确回调
- `*.contract.test.tsx` — **不变量**：§0.3 那类踩坑教训，写清楚"为什么这条存在"

**并行 agent 重写一个组件时，contract test 是它唯一可靠的验收信号。** 流程强制：**先从老代码翻译 contract test，再写实现**。翻译不是复制——老测试针对老结构，agent 需要提取其断言的不变量并在新结构下重述。

### 3.5 每层一个 `AGENTS.md`

内容固定四节：本层放什么 / 不放什么 / 依赖方向 / 本层契约模板。

比一个巨大的根 CLAUDE.md 有效得多——agent 改哪里就读哪里，上下文精准。

### 3.6 显式优于隐式

- props 一律显式 TS 类型，禁止 `React.FC` 隐式 children
- 跨边界数据一律过 zod（沿用现有 `api/schemas.ts` 模式）
- 必填字段类型为必填，**不用 `Option<T>` + 默认值**（沿用既有约定）
- 禁止隐式全局：无 `window.__X__`、无未声明的 env 读取

---

## 4. mock 后端

### 4.1 两条腿，各司其职

| | 用途 | 保真度 |
|---|---|---|
| `fe/mock` | 开发期。快、离线、可造边界数据（空态/错误/超长/并发冲突） | 中 |
| **cargo replay server** | 集成/验收期。**对齐 web 的最终判据** | 高 |

**现存资产**：`web/e2e/_setup/replay-server.setup.ts` 已经会启动一个 cargo replay 二进制，`playwright.config.ts` 的 `a11y` / `color-anchor` project 指向 `REPLAY_BASE_URL`。新 fe 直接复用，不必重建。

### 4.2 mock 必须从 `openapi.json` 生成

**硬性要求：不手写 handler。** 手写 mock 三周后就与后端脱节，然后 agent 在错误的契约上并行开发三周——这是重写方案里最大的单点风险。

生成器读 `openapi.json` + `generated-events.ts`，产出 handler 骨架；场景数据放 `scenarios/`，由人/agent 填充但**结构受生成类型约束**。

契约漂移检测进 CI：`gen:api` 后若 mock 生成物有 diff 而未提交，gate 失败。

---

## 5. 对齐 oracle 与脚手架

**这是并行 agent 的前置条件。** 没有客观判据，N 个 agent 对"对齐 web"会有 N 种理解。

### 5.1 现状缺口

现有 Playwright 五个 project（`chromium` / `a11y` / `color-anchor` / replay setup-teardown）**没有任何视觉回归**——全库零 `toHaveScreenshot`。`color-anchor` 只锚 token 值，`axe` 只查无障碍，都不会告诉你某张卡片 padding 差了 4px。

**"对齐 web" 当前不可测量。**

### 5.2 Oracle：老 web 截图基线（任务 #3）

对现存 `web/` 打基线，指向 replay server（数据确定）：

- 路由：`/`、`/cove/$id`、`/wave/$id`（含 report 视图）、`/settings`、login
- report 5 种 block 各一张（prose / chart.candles / table / app / task）
- 卡片 6 种各一张
- 关键交互态：Dialog 开、Menu 开、Sidebar 折叠/展开、对话抽屉开
- **每项 × light/dark 双主题**
- 视口：桌面基准 1440×900；另存 390×844（iPhone 尺寸）供 mobile 阶段用

### 5.3 脚手架：逐页 diff 工具（任务 #4）

并行 agent 需要**自助**判定是否达标，不能靠人肉看图：

```
fe/tools/align/
  align.config.ts     # 路由映射：old path ↔ new path
  run.ts              # 同时起 old web + new fe，同一 replay 数据，逐路由截图
  report.ts           # 输出 per-route diff 报告（像素差 % + 差异区域高亮）
```

要求：
- agent 自己能跑：`npm run align -- --route /wave/$id`
- 输出机器可读（JSON）+ 人可读（HTML 报告）
- **阈值策略要明确**：哪些差异可接受（抗锯齿、字体 hinting）、哪些必须归零（布局位移、颜色、字号）
- per-route 达标清单，agent 完成一个路由就能自证

> **给 review 的问题**：像素级 diff 对"重写"是否过严？重写必然带来微小差异（新 CSS 架构下 margin 折叠行为可能不同）。备选方案是**结构对齐**（DOM 树 + computed style 关键属性对比）而非像素对齐。请评估哪种更适合作为并行 agent 的自助判据。

---

## 6. 切片划分（并行 agent 的输入）

### 6.1 依赖图

```
S0 骨架 + common/api + mock + lint 全套      ← 阻塞一切，必须先单独完成
        │
        ├── S1 report 竖切（含 5 种 block）   ← 决策点，先于其余业务切片
        │
        └── 以下在 S1 收敛后可并行：
            S2 ui primitives（Dialog/Menu/ConfirmDialog + contract test 翻译）
            S3 shell（Sidebar / TitleBar / 布局骨架）
            S4 Today 页
            S5 Cove 页
            S6 Wave 页 + SpecConversation
            S7 Settings + Login
            S8 cards: terminal / codex / spec / iframe
            S9 cards: file-viewer 系列（含 CodeMirror / markdown / TOC）
            S10 NewTaskForm / DirectoryPicker / SchemaForm
```

### 6.2 S1 是决策点，不要跳过

report 竖切做完，才知道：新规范下一个页面要几个 PR、Astryx 能不能用、"对齐"到什么程度实际可达。

**拿 S1 的实际数字乘以剩余切片数，才是重写的真实工期。现在的任何估算都是猜。**

选 report 打头的理由：它是唯一在 5 种 block 上覆盖了全部渲染形态的页面（markdown / 图表 / 表格 / iframe / 结构化），且刚经历 #960/#967/#975/#985 大量变更，是最新的架构样本。

### 6.3 每个切片的交付定义

一个切片算完成，必须同时满足：
1. contract test 从老代码翻译完毕并通过
2. 对齐脚手架在该路由/组件上达标
3. 该层 `AGENTS.md` 写完
4. 全套 lint（含 §2 的依赖方向）通过
5. a11y：`getByRole` 选择器可用，axe 无 violation

---

## 7. 开放问题（review 必须回答）

1. **`common` 边界**：逻辑层（本文立场）还是含 UI 组件？——决定 §1.1，写进去后不好改
2. **对齐判据**：像素 diff 还是结构 diff？——决定 §5.3，且决定并行 agent 能否自助
3. **§0.3 不变量表是否完整**？——漏一条就是重写丢一个踩坑教训
4. **Astryx 是否采用**？——待 spike（任务 #5）结论：peer dep 的 StyleX 到底要不要配 Vite（官方文档与 npm 元数据说法冲突）、0.2.0 beta 的 breaking 风险
5. **切换策略**：一次性切还是按路由灰度？现有 `basepath: '/calm'`，是否可用路由级分流做渐进切换以降低回滚成本
6. **老 web 冻结期多长可接受**？冻结期内的生产需求如何处理——双写还是排队

---

## 8. 执行顺序

| 步 | 内容 | 依赖 |
|---|---|---|
| 0 | 本文档双通道 review 到收敛 | — |
| 0' | Astryx spike（并行） | — |
| 1 | 老 web 视觉基线（oracle） | — |
| 2 | 对齐脚手架 | 1 |
| 3 | S0 骨架 + common + mock + lint | 0 |
| 4 | **S1 report 竖切 → 重估工期** | 2, 3 |
| 5 | S2–S10 并行派发 | 4 |
| 6 | `fe/mobile` 开工 | 4 |

步骤 4 之后必须回到你这里重新评估：拿真实数字决定是否继续。
