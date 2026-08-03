# `fe/` 重写执行 Plan

状态：草案 v1，取代 `_fe-rewrite-design.md` 的 §5/§6/§8
日期：2026-08-02

---

## 0. 目标校正

**目标不是把现有 web 搬到新目录，是拿到更干净的架构。**

这条校正推翻了前一版设计的三个前提：

| 前一版假设 | 校正后 |
|---|---|
| oracle = 像素对齐现有 web | oracle = **不丢能力 + 不丢踩坑教训**，与外观无关 |
| shell 几何必须先冻结，否则页面对不齐 | 约束不存在。新架构自己定几何 |
| 切片按现有页面/域划分 | 切片按**新架构的边界**划分，不继承旧耦合 |

**推论**：前一版把视觉回归基线当 oracle 是错的方向。视觉只需满足"设计系统内部一致"，不需要"跟老的一样"。

---

## 1. "更干净"的具体定义

不是形容词。以下是现有架构被两轮 review 和代码扫描定位到的**具体病灶**，新架构必须逐条解决——这张表就是"干净"的验收标准。

| # | 病灶 | 证据 | 新架构的解 |
|---|---|---|---|
| 1 | 卡片创建逻辑住在路由里 | `app/router.tsx:415 addCardWithValues` / `:481 createFromEntry` / `:531 addCardOfKind` | 卡片系统独立子系统，路由只做导航 |
| 2 | 模块级单例满天飞，不可测试、不可多实例 | `cards/registry.ts:210-219` 三张 Map + warned Set；`cards/resolver.ts:22`；`cards/builtins/index.ts:23` boot-once；`wave-fs-viewers/registry.ts:16`；`api/events.ts:628`；`api/onUnauthorized.ts:26` | 显式注入，容器在 `app/` 组装 |
| 3 | Context 跨域穿透，依赖方向失控 | `ThemeContext` 跨 5 个域；`ModalViewContext` 定义在 `ui/Dialog` 却被 `DirectoryPicker`/`NewTaskForm` 消费（primitive 被业务反向依赖） | context 只允许出现在 `app/**` 与 primitive 自有目录，可 lint |
| 4 | 持久化 key 硬编码多处 | `calm:sync:cursor` 在 `api/events.ts:152`、`providers.tsx:82`、`SessionProvider.tsx:65` **各写一次** | 统一 key 工厂，单一出处 |
| 5 | 同一 UI 逻辑多份拷贝 | `react-markdown` pipeline **5 份**（`WaveReportPage.tsx:381`、`report-blocks/index.tsx:116`、`task.tsx:24,31`、`file-viewer-markdown.tsx:376`、`SpecConversation.tsx:107`）；TOC 两套发散实现；`Settings.tsx:111-249` 手写重复整套 `.schema-form*` markup；`.calm-select*` 在 `NewTaskForm` 和 `Cove.tsx:508-536` 各一份 | 单一 renderer 模块 |
| 6 | 6,392 行单一全局 CSS，599 个全局类 | `calm.css` | CSS Modules + 受控全局层 |
| 7 | 高扇出共享类无归属 | `.go` 跨 8 文件 6 域、`.card-drag-handle` 跨 7 文件、`.calm-prose`、`.sr-only`、`.status-pill` | 提为组件或进显式全局层，二选一，不留中间态 |
| 8 | 运行时按 class 查 DOM | `input/wheelRouter.ts:14-17` 硬编码 `.modal-overlay`/`.modal-panel`/`.xterm-view`；`Dialog.tsx:200-207` 靠 DOM 结构上溯 portal root | 一律 `data-*` 属性 |
| 9 | 根目录散件，无层归属 | `src/` 下 `CalmApp.tsx`/`XtermView.tsx`/`WaveGrid.tsx`/`WaveList.tsx`/`LoginPage.tsx`/`Icon.tsx` | 全部归层 |
| 10 | 死代码 | `shared/components/WaveContext.ts:36` 定义并 provide，**全仓无消费者** | 不迁移 |
| 11 | lint 历史包袱 | `eslint.config.js` 里 `reportUnusedDisableDirectives: 'off'` + 一批规则 shim 成 `'off'` | 新仓全开，无 shim |

**验收**：每条都要有对应的机器检查（lint / 架构测试 / CI 脚本）。没有检查的条目不算解决——它会在半年内长回来。

---

## 2. Oracle 的正确定义

Oracle 由三部分组成，**都不是截图**：

### 2.1 能力清单（Capability Inventory）
枚举现有 web 的全部用户可达能力：路由、每页的操作、每种卡片的行为、每种 report block、每个键盘快捷键、每个错误态与空态。

来源：`e2e/*.spec.ts`（38 个 spec）、`docs/a11y-contract.md`、路由表、registry 注册项。

形态：结构化 YAML/JSON，每条带 `id` + 来源引用 + owner slice。

用途：新 fe 逐条勾掉，**这是"没丢功能"的唯一判据**。

### 2.2 不变量集（Invariant Set）
现有 37,363 行测试锁定的踩坑教训。前一版 §0.3 手工列了 7 条，两轮 review 证明**覆盖率 <20%**。

**方法论纠正**：不再维护人工精选短表。改为：
```
不变量族 → authoritative test glob → owner slice → 迁移状态
```
并要求**迁移清单证明**：老测试里每个 `contract` / `regression guard` / 带 issue 号的注释都已被归类（归类结果可以是"故意不迁"，但必须显式）。

已知必须纳入的族（两轮 review 产出，非穷尽）：
- **`docs/a11y-contract.md` 整份 326 行 11 节** — 前一版零引用。含逐页 Tab 顺序、rename 的 name/description 拆分、focus-visible 政策、motion 政策、每类对象的 role+name 语义
- Dialog 焦点与排序族 — inert effect 必须**声明在** focus-restore effect 之前（React 按声明顺序跑 cleanup，`Dialog.tsx:183-193`）、inert 精确复原 `prior` 值、restore 前 `document.contains` 守卫、每次 Tab 重查 focusables、child-view stack 保持挂载、故意不用原生 `<dialog>`
- Menu / roving 族 — ArrowLeft/Right **故意不处理**、typeahead 单字母跳过当前项而多字母包含、Space 在缓冲非空时进缓冲、`queueMicrotask` 补焦点
- 事件流族 — `setSyncEventVersion → subscribe → start` 顺序、EventBridge 必须挂 `ServerCompatGate` 内、`cove.updated` write-through 且不造 phantom、`_replay_complete` / `_snapshot_required` 语义
- 会话族 — 401 不重试、tri-state `unknown` 期渲染 `null`、非 401 传输错误不跳登录、`dbInstanceId` 变化静默清缓存硬刷新
- 类型闸门族 — `invalidationPolicies.ts:21` 事件穷尽、zod↔ts-rs 一致性（`schemas.test.ts:211-226`）、`TS_RS_LARGE_INT="number"`、`Persistent<T>` 的**条件返回类型**（硬闸，lint 只是提示层）
- token 族 — `calm-tokens.test.ts` 的**六类**形状契约、z-index 六级严格递增、`--font-mono` 与 `MONO_STACK` 逐字节相同
- 卡片生命周期族 — 卡片不因离开 viewport 卸载、换序两 mutation 必须串行不可 `Promise.all`
- 终端协议族 — stale close 忽略、0-geometry 不 resize、snapshot 先 scrollback 后 data、OSC 10/11/12 抑制、CONNECTING 期 theme 排队
- 跨语言族 — `themeRgb.ts` ↔ `XtermView` ↔ Rust `RequestTheme::default_dark()` 三处同步

### 2.3 契约测试（Contract Test）
新架构自己的接口测试。**不从老代码翻译，是新写的**——它测的是新接口，不是旧实现。

---

## 3. 为什么重写可以并行（修正前一版的错误论证）

前一版说"重写不适合并行"，那个论证建立在**照抄现有结构**的假设上——照抄就会继承现有的全部耦合（10 组硬耦合、5–6 层依赖），当然并行不了。

**新架构的价值恰恰是可以重新划边界。** 正确的并行策略是**接口优先**，不是切片优先：

```
传统（切片优先）：按页面切 → 页面间共享接口 → 耦合 → 串行
正确（接口优先）：先冻结所有跨模块接口 → 实现只依赖接口 → 真正并行
```

接口层很小（见 §4 阶段 1），冻结后每个模块的实现互不可见，agent 各写各的。

### 3.1 并发度的诚实分析

| 阶段 | 可并发 | 真实约束 |
|---|---|---|
| 架构设计 | **1**（我做） | 不可并行，但小 |
| 能力清单 + 不变量提取 | **30–50** | 纯读取，无写冲突。唯一约束是任务切分不重叠 |
| 接口冻结 | **3–5** | 接口之间互相引用，需要少量协商 |
| 实现 | **20–30** | 见下面三条 |
| 集成 | **1–3** | 不可压缩 |

**实现阶段的三个真实约束**（这才是并发度上限的来源）：

1. **写冲突** — 两个 agent 改同一文件必然冲突。解：worktree 隔离 + **文件所有权表**（每个文件恰好一个 owner slice，没有共享可写文件）。这是可以设计掉的，不是硬约束。

2. **接口漂移** — 实现中发现接口不够用。解：**接口变更协议**——agent 不得自行改接口，必须提 change request 回到接口层，由我裁决并广播。宁可某个 agent 阻塞，不可两个 agent 各改一版。

3. **你的 review 带宽** — 这是**唯一的硬约束**。缓解方式是把 review 从"逐 PR 看代码"降级为"看自动判据 + 抽样"：
   - contract test 全绿 → 自动过第一关
   - 该 slice 的能力清单条目全勾 → 自动过第二关
   - 不变量迁移清单全覆盖 → 自动过第三关
   - **你只 review 架构决策和接口，不 review 实现细节**

   三关全自动通过的 PR，人工只抽样。这样你的负载与 agent 数量解耦。

### 3.2 现实工期

不给"几天/几个月"这种没有依据的数字。**阶段 3 的第一个模块跑完，才有真实的单模块耗时**，乘以模块数才是工期。这个数在阶段 3 开头两小时内就能拿到。

可以先说的：阶段 1–2 是小时级（纯读取 + 小量设计），阶段 4 是主体，阶段 5 不可压缩。

---

## 4. 阶段划分

### 阶段 0 — 架构设计（我，不并行）
产出 `docs/_fe-architecture.md`：
- 层定义与依赖方向（单向，可 lint）
- §1 那 11 条病灶逐条的解法 + 对应机器检查
- 模块清单与**文件所有权表**（每个文件恰好一个 owner）
- 技术栈确认（React 19 + Vite + TanStack 不变；Astryx 待 spike 结论）

**出口**：双通道 review 收敛。

### 阶段 1 — 接口冻结（3–5 agent）
把所有跨模块接口写成 TS 类型 + contract test，**先于任何实现**：
- 卡片系统：registry / lifecycle / host / resolver 契约
- 状态层：`Persistent<T>` 条件类型 wrapper（硬闸必须先在）
- 事件层：EventBridge 接口、invalidation 策略的穷尽类型
- API 层：client / schema / key 工厂
- 样式层：token 定义 + 全局层边界 + `data-*` 约定
- primitive 层：Dialog / Menu / 焦点管理的 props 面

**出口**：接口冻结，进入变更协议管辖。

### 阶段 2 — Oracle 构建（30–50 agent，与阶段 1 并行）
纯读取，产出 §2.1 能力清单 + §2.2 不变量集。

切分：按老代码的测试文件与文档分片，每个 agent 负责若干文件，产出结构化条目。**不重叠、不写代码。**

**出口**：迁移清单证明覆盖完成——老测试里每个 contract/regression guard 都已归类。

### 阶段 3 — 实现（20–30 agent）
每个 agent 拿到：冻结的接口 + 该模块的能力条目 + 该模块的不变量条目 + 老代码（只读参考）。

写新实现 + 新 contract test。**不照抄老代码结构。**

**出口**：三关自动判据全绿。

### 阶段 4 — 集成与切换
- 整应用挂 `/calm2`，由 calm-server/nginx 静态分流（review 确认可行且几乎零成本）
- **路由级混合已否决**：两套 bundle 各持 queryClient、各开 WS、共用 IndexedDB 与 `calm:sync:cursor`，跨 bundle 导航会互相清缓存、互相触发硬刷新
- 真实后端 shadow 运行，不只 mock + replay

---

## 5. 待决事项

1. **冻结期政策** — 上一轮问过未答。新 fe 开发期间老 web 的 P0 修复走哪、普通需求冻结多久。不定这条，阶段 3 产出会与移动中的 oracle 脱节
2. **`common` 边界** — 两份 review 有分歧。Codex：不放 UI 是对的；subagent：report 渲染层该共享（mobile 定位就是只读阅读，重合近 100%，且 markdown pipeline 已有 5 份拷贝）。我倾向按"是否含交互模型"划而非按文件后缀，可 lint 表达为：`common` 禁 `react-dom` / `useState` / 键盘 handler / `createContext`
3. **Astryx** — spike 未回
4. **视觉基线是否还要** — 按 §0 的校正，它不再是 oracle。但作为"新 fe 有没有意外退化"的参考仍有价值，且成本低。建议保留为非阻塞报告

---

## 6. 与前一版的差异摘要

| | `_fe-rewrite-design.md` | 本 plan |
|---|---|---|
| 目标 | 对齐现有 web | 更干净的架构 |
| oracle | 视觉回归基线 | 能力清单 + 不变量集 + 契约测试 |
| 切片依据 | 现有页面/域 | 新架构边界 |
| 并行策略 | 切片优先（继承旧耦合，5–6 层串行） | **接口优先**（冻结接口后扁平并行） |
| 不变量 | 人工短表（覆盖率 <20%） | 族 + test glob + 迁移清单证明 |
| shell 几何约束 | 必须先冻结 | 不存在（不要求像素对齐） |
