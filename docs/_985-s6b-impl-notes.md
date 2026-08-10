# #985 切片 6 PR-B —— 施工笔记（树级预算 + 两个强制点）

依据：`docs/_985-s6-design.md` 第二部分 §8（含 v5 ✅ 裁决框）、`docs/_985-s6b-fork.md`。
变异映射：`docs/_985-s6b-mutation-map.md`。

## 1. 交付清单（对照任务书）

| 交付项 | 落点 |
|---|---|
| `waves.tree_task_budget` | `crates/calm-truth/migrations/0072_wave_tree_task_budget.sql`（`INTEGER NULL`，**无 DB DEFAULT**，原地 additive） |
| 写入面 | `model.rs::WavePatch.tree_task_budget`（double-option）+ `wave.rs::wave_update_tx` 定向单列 UPDATE + `routes/waves.rs` 的 user-only 闸 / `>= 0` 校验 / `patch_has_other_changes` / `projection_policy_changed` + OpenAPI-TS 生成物 |
| 单一真源 | `wave_create_tx` 固定列清单**显式写 NULL**；root-only 守卫落在 `wave_update_tx`（共用 in-tx writer） |
| 强制点一 | `child_wave_adapter.rs::prepare_tx`：`wave_tree_spec_inventory(root) >= wave_tree_budget(root)` ⇒ `sub-wave-tree-budget-exhausted` |
| 强制点二 | `task_projection.rs::evaluate_schedulability`：`effective_ceiling = min(spec_task_ceiling, share)` |
| fail-closed | `WaveTreeTerm::RootUnresolved` ⇒ 该 wave 全部声明不可调度 + `tree_root_unresolved` |
| 非树短路 | `wave_tree_term` 先做一条**非递归**判断，非树 ⇒ 零递归查询（`tree_cte_queries == 0` 是可计数接缝） |
| 诊断码 | `tree_budget_exhausted` / `tree_root_unresolved`（Rust 文案 + web 人话文案 + 下一步动作 + 集合相等元测试 16 ⇒ 18） |
| 文档 | `985-doc-as-plan.md` D.4 #7 / §8 / C.2 / C.4 / §12.1 #19 #22 |

## 2. 结构性决定（都能在设计里找到依据）

**2.1 所有有界树 SQL 收进 `calm-truth/src/db/sqlite/wave_tree.rs`。**
原因是硬约束不是审美：`evaluate_schedulability` 住 `calm-truth`，`child-wave` adapter 住
`calm-server`，而 `calm-server` 依赖 `calm-truth`（反向不成立）。要让两边**共用同一个静态
门禁**（设计 §8 ⚠️ 框明令），SQL 只能住在下游 crate。PR-A 的 `MAX_WAVE_TREE_DEPTH` /
`WAVE_ROOT_DEPTH_SQL` 原样搬过去，adapter 侧 `pub use` 回原路径，公开契约不变。

**2.2 登记制按 fork D9 的建议做成清单 + 机器联系。**
`BOUNDED_WAVE_TREE_SQL: &[&str]` 收四条片段，门禁遍历清单；另加一条**源码扫描**元测试，
断言本文件里宏展开的次数 == 清单长度 —— 否则「新增 SQL 漏登记」不会有任何东西变红
（正是 §7.1 记的「名单式常量与实际调用之间没有机器联系」）。变异 M3 实测红。

**2.3 递归只携带 `id` + `depth`，`created_at` 由外层 JOIN 读。**
配额顺序要 `(created_at, id)`，但设计要求向下 CTE 只投影 `id`。做法是
`WITH RECURSIVE down(id, depth)` + 外层 `JOIN waves w ON w.id = d.id ORDER BY w.created_at, w.id`。
去重也在外层（`min(depth) GROUP BY id`），环上不会重复计数。

## 3. 设计缺口 —— 我选了什么、依据是什么（**不静默决定**）

**G1. `tree_task_budget` 改动要不要触发重投影？设计没写。**
选：**要**，且**只重投影被 PATCH 的那个 wave**（并入 `projection_policy_changed`）。
依据：`spec_task_ceiling` 的先例（`routes/waves.rs:1310/1318`）；树内其余 wave 在各自
下次投影时收敛，这与「人把 ceiling 调低到在飞数以下」是**同一条已批准的退化语义**
（doc-as-plan §4.2 / §8）。**没有**去做「整树 rebuild」—— fork C7 已证伪那条路，
而且它会引入新的批量重建工作单元（全仓无先例）。

**G2. root-only 守卫落 route 还是落 DB？设计只说「第 4/5 项还要加非 root ⇒ 拒绝」。**
选：**落 `wave_update_tx`（共用 in-tx writer）**，route 只保留 user-only 与取值校验。
依据：§7 #17/#20 两条都是「守卫只放 route ⇒ 直调 repo 绕过」被判红的形状；fail-closed
的那一边就是把守卫放在所有入口共用的那一层。REST 侧因此返回 409 而非 400 —— 若评审要
400，改 route 加一次前检即可，DB 守卫不动。

**G3. 强制点一用 `>=` 还是 `>`？设计写 `count >= budget`，照做。**
补一条依据：认领中的父任务本身就是一条非终结 spec 行，会被全树计数数到；用 `>` 会让
`count == budget` 时还能再开一棵子树，越过上界之后才被 schedulability 发现。变异 M7 实测红。

**G4. 「求不到根」包含哪些形态？设计只说「求不到根」。**
选（fail-closed 一侧）：① 向上 CTE 返回 0 行或 >1 行；② 根深度 > `MAX_WAVE_TREE_DEPTH`；
③ 树成员里出现超深节点；④ 成员集合**不含**发起查询的那个 wave。四者都判
`RootUnresolved`。依据：§8 M-B2「一条断链会让整棵子树无约束」；④ 是「我们要按之划分预算的
形状，不是这个 wave 实际所在的形状」，同一条理由。

**G5. 两个 bound 同时收紧时报哪个诊断？设计没写。**
选：`share < spec_task_ceiling` 时报 `tree_budget_exhausted`（并指名根 wave），否则维持
`spec_task_ceiling`。依据：§12.2 C「人话 + 下一步动作」—— 报错要指向**真正能改的那个旋钮**；
两者相等时（默认 32 == 32 且 N=1）保持既有码，不改既有验收。变异 M10 实测红。

## 4. 停下来没做的（**不就地扩范围**）

- **N1. 既有 `non_user_policy_patches_are_forbidden_without_rows_or_events` 是恒真断言。**
  M13 的过程实测发现：`X-Calm-Actor: ai:codex` 走 REST 时 `to_actor_id()` 造出空 card id 的
  `AiCodex`，下游另有一条 403，所以「删掉 user-only 闸 ⇒ 仍是 403」。我**只加固了自己的**
  那条断言（改成断言理由字符串），**没有**去改既有的 `spec_task_ceiling` / `automation_policy`
  测试 —— 那是 PR-A 之前就存在的形状，改它要连带审视 REST 的 AI actor 归因路径（另一个载体）。
  **建议单开 issue**：`ai:*` actor 经 REST 的空 card id 403 把所有 user-only 闸的验收变成恒真。
- **N2. 没有做「整树 rebuild」/ `tree_rebuild_tx`。** fork C7 证伪 + 它是新的批量工作单元。
- **N3. 没有做丙-1（claim 事务里的树级在飞检查）。** 设计说它只能当补充，且会碰
  scheduler 的 claim 路径 —— 新面，超出本片。
- **N4. `tree_task_budget` 没有进 `Wave` / `WaveRow`。** 照 `spec_task_ceiling` /
  `parent_wave_id` 的先例（doc-as-plan §11：`WaveRow` 的显式 SELECT 有 8 处，sqlx 运行时才炸）。
  读它的只有内核 SQL。

## 5. 门（实际数字）

| 门 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt --all --check` | 干净（无输出） |
| clippy | `cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings` | `Finished dev profile in 1m 05s`，0 warning |
| 测试 | `cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci` | **3364 tests run: 3364 passed, 89 skipped**（103 binaries，34.2s） |
| 生成物 | `web: npm run gen:api` 后 `git diff --exit-code -- src/api/openapi.json src/api/generated.ts src/api/generated-terminal.ts src/api/generated-events.ts src/editor/types/` | 干净（产物已提交：`WavePatch.tree_task_budget`） |
| web build | `npm run build` | 成功（仅既有 chunk-size 警告） |
| web test | `npm run test` | **85 files / 1227 tests passed**，`Type Errors no errors` |
| fe lint | `npm run lint` | `no dependency violations found (102 modules, 232 dependencies)` |
| fe build | `npm run build` | 成功，`built in 214ms` |
| fe test | `npm run test`（含 `test:wire` + `test:mock-drift`） | **758 passed / 1 skipped**，wire 与 mock 均无漂移 |

环境：`CARGO_BUILD_JOBS=6`，nextest 取自 `.local-bin`，`NEIGE_CODEX_BIN` 全程未设置，
web/fe 用 Node 22。

## 6. 已知代价（已登记进 doc-as-plan §12.1 #19）

`share = floor(B/N)` ⇒ `B=32`、`N=10` 时每个 wave 只剩 3 条未结额度：一棵大树里单个 wave
会比它独立存在时更早撞上限。**校准装置 = `tree_budget_exhausted` 的发生率**；频繁触发说明
`tree_task_budget = 32` 定低了 —— **调常数，不要改分配律**（改成共享计数会直接击穿 D.1 #11，
见变异 M1）。
