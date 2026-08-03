# Oracle 归一化重标报告

**范围**：`docs/oracle/*.yaml` 全部 **1127** 条。统计见下方「统计」节（由脚本重算）。
**动作**：owner_slice 归一化 + 新增 `runtime_layer` / `verification_owner` / `test_tier` + `migration` 枚举化。
**P8b-1 更新**：补齐 source/test 行号，落盘 §6 裁决，并按语义修正 6 条 id/kind；改名条目以
`former_id` 保留退休句柄。历史归一化动作仍见下文。

映射表：`docs/oracle/owner-aliases.yaml`（含 `aliases` / `per_entry` / `split_owners` / 需人工裁决）。

---

## 1. 148 → 114：owner_slice 映射统计

| 指标 | 值 |
| --- | --- |
| 原始 distinct owner_slice | **148** |
| 规范 owner_slice | **114** |
| 发生合并的规范值 | 27（吃掉 61 个原始值） |
| `shared/` 筐废除 | 104 条全部重新分配（无一残留） |
| `NONE` owner | 2 条 → 逐条裁决（`per_entry`） |
| 一格双值 / 跨 slice | 8 条 → 主 owner + `【亦涉及】` |

规范值格式统一为 `<layer>/<slice>[/<detail>]`，`runtime_layer` 由前缀派生 —— 单一事实来源。

### 主要合并（同物异名）

| 条数 | 规范值 | 合并来源 |
| ---: | --- | --- |
| 50 | `features/wave/create` | shared/new-task-form(29) · wave/create(17) · web/components/new-task-form(4) |
| 50 | `none/e2e/infra` | infra/e2e(40) · web/e2e(5) · web/e2e/trace(2) · web/e2e/setup(2) · web/e2e/wave-create(1) |
| 44 | `ui/dialog` | ui/dialog(33) · web/ui/dialog(11) |
| 39 | `app/shell/sidebar` | shared/sidebar(19) · shell/sidebar(10) · web/shell/sidebar(10) |
| 37 | `features/wave/page` | pages/wave(25) · web/pages/wave(12) |
| 35 | `features/cove/page` | pages/cove(24) · web/pages/cove(11) |
| 28 | `systems/terminal/card` | cards/terminal(27) · web/cards/terminal(1) |
| 24 | `app/router` | app/router(17) · app/router → cards/create(7) |
| 24 | `systems/events-transport/stream` | api/events(23) · "api/events, api/onUnauthorized"(1) |
| 22 | `features/wave/list` | wave/list(10) · web/wave-list(8) · wave/list-view(4) |
| 20 | `ui/roving` | ui/hooks(17) · web/hooks/roving-tabindex(3) |
| 18 | `systems/terminal/xterm-view` | terminal/xterm-view(16) · web/xterm-view(2) |
| 18 | `systems/cards/card-head` | cards/card-head(16) · web/cards/card-head(2) |
| 17 | `ui/directory-browser` | shared/directory-picker(14) · ui/directory-browser(3) |
| 17 | `app/events-glue/event-bridge` | app/event-bridge(16) · web/app/event-bridge(1) |

（其余 13 组每组 ≤ 13 条：`systems/terminal/protocol` · `systems/cards/codex` · `features/wave/grid` ·
`ui/atoms` · `features/report/layout` · `systems/cards/plugin` · `features/wave/row` ·
`features/report/page` · `ui/calm-select` · `systems/cards/overlays` · `ui/schema-form` ·
`app/router/navigation`，完整表见 `owner-aliases.yaml`）

## 统计（由 `docs/oracle/*.yaml` 重算，勿手工编辑）

**总计 1127 条**：invariant 694 / capability 290 / gate 143。
唯一 id 1127，`owner_slice` 收敛为 114 个规范值。

### runtime_layer 分布

| runtime_layer | 条数 | 说明 |
|---|---|---|
| `features` | 407 | 业务域 |
| `systems` | 271 | 子系统 |
| `app` | 138 | 组装层 |
| `ui` | 131 | 交互原语 |
| `none` | 100 | 非运行时（lint / CI 闸门 / 构建 / e2e 基础设施） |
| `styles` | 54 | 全局样式层 |
| `core` | 26 | 平台无关逻辑 |
| **合计** | **1127** | |

> **`core` 仅 26 条** —— 跨端共享面远小于预期。这从数据侧支持了收紧 `core`（禁 JSX）的裁决。

### 归层率

**100%（1127/1127）**。每条都有确定的 `runtime_layer` 与规范 `owner_slice`：无 `NONE`、无一格双值、无跨 slice 悬空。
（归一化执行前为 795/1126 = 70.6%；`INV-APP-105` 系 review 阶段补录。）

### test_tier

| test_tier | 条数 | 含义 |
|---|---|---|
| `browser` | 585 | 布局／几何／滚动／真实焦点／canvas／PTY —— jsdom 里断言恒真 |
| `jsdom` | 424 | 纯逻辑与 DOM 结构断言 |
| `static` | 80 | lint / 类型 / CSS 解析，不需运行 |
| `none` | 38 | 由 review 或架构约束保证 |

> ⚠️ **200 条恒真断言风险**：`test_tier: browser` 共 585 条，其中 **200 条当前 `verification_owner` 不是 e2e**
> （systems 100 · features 54 · ui 38 · app 6 · core 1 · styles 1）。
> 它们留在 jsdom 里断言永远通过。实现阶段单 slice 的验证成本须按此上调。

### verification_owner

| verification_owner | 条数 |
|---|---|
| `unit` | 621 |
| `e2e` | 383 |
| `css` | 48 |
| `review-waiver` | 25 |
| `lint` | 27 |
| `architecture` | 9 |
| `build` | 6 |
| `null` | 8 |

### migration

| 值 | 条数 |
|---|---|
| `pending` | 1119 |
| `skipped` | 8 |

`skipped` 全部 8 条（均带 `skip_reason`）：

| id | 原因 |
|---|---|
| `INV-APP-019` | 被 events typestate 取代（`configure()` 原子化，非法顺序类型层不可表达） |
| `INV-APP-105` | 同上；本条系 review 从 `events.test.ts:303` 补录，7 个提取 agent 全漏 |
| `INV-DEAD-001` | `WaveContext` —— provide 但全仓零消费者 |
| `INV-DEAD-002` | `useSpecCurrentRun.latestTool` —— 恒为 `{null,null}` 的未接线占位 |
| `INV-DEAD-003` | `isCompletedMessageItem` 的未用 `itemType` 参数 |
| `INV-A11Y-052` | 服务端 `validate_overlay_payload` 强制，前端无实现责任 |
| `E2E-INV-LIFECYCLE-012` | 服务端 actor middleware 强制，前端无实现责任 |
| `E2E-INV-TERMINAL-005` | upgrade 前服务端关闭语义，前端无实现责任 |


## 6. 需人工裁决（13 条 / 6 项）

已有可用临时归属，不阻塞实现；下述分歧请在实现阶段前拍板（详细候选见 `owner-aliases.yaml` 末节）。

| # | 原始 owner | 条数 | 临时归属 | 分歧 |
| --- | --- | ---: | --- | --- |
| 1 | `kernel/overlay` | 1 | `core/domain/overlay` | 是 **Rust 内核**契约，不属任何 web 层；可选整体移出本 oracle |
| 2 | `auth/actor` | 1 | `features/auth/actor` | `X-Calm-Actor` 由服务端 middleware 强制，前端只是遵守方 |
| 3 | `wave/report` · `wave/backlinks` · `ws/terminal` | 4 | `features/report/{write,backlinks}` · `systems/terminal/protocol` | 描述的是 REST/WS **协议**本身；若独立出 `core/api` + `core/events-protocol` 应整体搬迁 |
| 4 | `design/color-system` | 5 | `styles/color-system` | 同时是设计系统契约与 e2e harness；备选 `none/e2e/color-anchor`（会让 layer 从 styles 变 none） |
| 5 | `web/components` (INV-A11Y-059) | 1 | `ui/activation` | 横切 a11y 政策，实现散在 `features/wave/row` 与 `ui/editable-title` |
| 6 | `cards/report` (INV-CARD-226) | 1 | `features/report/exclude-cards` | 代码住 `web/src/cards/` 但语义纯属 report |

## 7. 校验

全部 7 个文件通过：`yaml.safe_load` 可解析 · 13 个字段齐全（skipped 条目 14 个）· 枚举值合法 ·
1127 个 `id` 无重复 · 逐行 diff 确认只有 `owner_slice` / `migration` 被改写、只有新增四字段与
8 条 `why` 追加，`id`/`statement`/`source`/`authoritative_test`/`kind`/`family` 零改动。

---

## 补录更新（架构 review r3/r4 后）

- **总条数 1126 → 1127**：新增 `INV-APP-105`（"未 set 就能 start"契约，7 个提取 agent 全漏，
  由 review 从 `web/src/api/events.test.ts:303` 反查补出），标 `migration: skipped`。
- **skipped 3 → 5**：新增 `INV-APP-019`（被 events typestate 取代）与 `INV-APP-105`。
- **`INV-DUP-004` why 追加架构裁决**（原 statement/why 保留，末尾追加推翻说明）：原"收敛成一个
  markdown 渲染原语"与 `core` 禁 JSX 裁决冲突，裁决改为"收敛内核，不收敛 renderer"。
- 复验：1127 条全部通过 schema / 枚举 / 唯一 id / 归层 检查；5 条 skipped 全带 `skip_reason`。
