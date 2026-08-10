# #985 切片 6 PR-B —— 施工笔记（树级预算 + 两个强制点）

依据：`docs/_985-s6-design.md` 第二部分 §8（含 v5 ✅ 裁决框）、`docs/_985-s6b-fork.md`。
变异映射：`docs/_985-s6b-mutation-map.md`。

## 1. 交付清单（对照任务书）

| 交付项 | 落点 |
|---|---|
| `waves.tree_task_budget` | `crates/calm-truth/migrations/0072_wave_tree_task_budget.sql`（`INTEGER NULL`，**无 DB DEFAULT**，原地 additive） |
| 写入面 | `model.rs::WavePatch.tree_task_budget`（double-option）+ `wave.rs::wave_update_tx` 定向单列 UPDATE + `routes/waves.rs` 的 user-only 闸 / `0..=64` 校验 / `patch_has_other_changes` / `projection_policy_changed` + OpenAPI-TS 生成物 |
| 单一真源 | `wave_create_tx` 固定列清单**显式写 NULL**；root-only 守卫落在 `wave_update_tx`（共用 in-tx writer） |
| 强制点一 | `child_wave_adapter.rs::prepare_tx`：库存要求 `inventory < B`，形状还要求创建后 `members + 1 <= B`；任一失败 ⇒ `sub-wave-tree-budget-exhausted` |
| 强制点二 | `task_projection.rs::evaluate_schedulability`：`effective_ceiling = min(spec_task_ceiling, share)` |
| B/N 共用重投影 | `wave_report.rs::tasks_rebuild_tree_tx`：一次枚举预计算全树 share；B PATCH 与 child-wave 创建都在同一写事务调用；裁 pending 后复核 live，in-flight 超额则整笔回滚 |
| 事务上界 | `MAX_TREE_TASK_BUDGET=64`，结合 `N+1<=B` 封顶成员数；递归树查询固定 2 次，成员处理 O(N)，由 `tree_cte_queries` seam 守住 |
| fail-closed | `WaveTreeTerm::RootUnresolved` ⇒ `effective_ceiling=0` + 每条声明追加 `tree_root_unresolved`，但继续走完 withdrawal / 已删块合成 / read-state 主干 |
| 非树短路 | `wave_tree_term` 先做非递归形状判断，再用与点一相同的 `wave_tree_budget()` 取有效 B（NULL ⇒ 32）；仅孤根且 `B >= spec_task_ceiling`、树项可证不更紧时短路，否则按 `N=1, share=B` 执行上界 |
| 诊断码 | `tree_budget_exhausted` / `tree_root_unresolved`；Rust recovery-action 契约为单一来源，web 交叉校验“该拧哪个旋钮”，缺参回退与 Rust 同为 share=0 |
| 文档 | `985-doc-as-plan.md` D.4 #7 / §8 / C.2 / C.4 / §12.1 #19 #22 |

## 2. 结构性决定（都能在设计里找到依据）

**2.1 所有有界树 SQL 收进 `calm-truth/src/db/sqlite/wave_tree.rs`。**
原因是硬约束不是审美：`evaluate_schedulability` 住 `calm-truth`，`child-wave` adapter 住
`calm-server`，而 `calm-server` 依赖 `calm-truth`（反向不成立）。要让两边**共用同一个静态
门禁**（设计 §8 ⚠️ 框明令），SQL 只能住在下游 crate。PR-A 的 `MAX_WAVE_TREE_DEPTH` /
`WAVE_ROOT_DEPTH_SQL` 原样搬过去，adapter 侧 `pub use` 回原路径，公开契约不变。

**2.2 修复轮 2 删除登记制，直接守「不存在无界递归 parent-wave CTE」的性质。**
独立集成门从根 `Cargo.toml` 解码并扫描**全部 workspace member** 的生产 Rust / SQL，解码所有
字符串字面量和宏 token 内文本，剥离 SQL 行/块注释后检查：触及 `parent_wave_id` 的递归成员必须
用 alias-qualified depth 上界作 ON/WHERE 的**合取项**；`true OR bound` 两种顺序都拒绝。
它不维护 crate/常量名单，也不依赖顶层 `pub const + concat!` 这一种 AST；内联 mod、包装宏、
static、块表达式和新增 workspace crate 的绕过均逐一变异为 RED。

**2.3 递归只携带 `id` + `depth`，`created_at` 由外层 JOIN 读。**
配额顺序要 `(created_at, id)`，但设计要求向下 CTE 只投影 `id`。做法是
`WITH RECURSIVE down(id, depth)` + 外层 `JOIN waves w ON w.id = d.id ORDER BY w.created_at, w.id`。
去重也在外层（`min(depth) GROUP BY id`），环上不会重复计数。

## 3. 设计缺口 —— 我选了什么、依据是什么（**不静默决定**）

**G1. `tree_task_budget` 改动要不要触发重投影？设计没写。**
修复轮 4 最终裁决：**要，且 B PATCH 与 N 增长都在同一 IMMEDIATE 事务内调用同一个整树例程**。
理由不是用 rebuild 重新裁决历史顺序（fork C7 证伪的仍是那种方案），而是 B 本身是每个成员
确定性 share 的输入；只重投影根会让子 wave 的旧 pending 无限期保留并可直接 claim。例程一次
枚举成员并预计算 share，再把 tree term 传给各投影；没有新增持久载体或跨事务协议。旧 pending
在提交前被裁；若不可删除的 in-flight 仍超过新 share，则 B PATCH / child-wave 创建回滚，而不是
提交退化的树总量上界。每个发生变化的成员仍按自身 wave/cove scope 发 `PlanUpdated`。

**G2. root-only 守卫落 route 还是落 DB？设计只说「第 4/5 项还要加非 root ⇒ 拒绝」。**
选：**落 `wave_update_tx`（共用 in-tx writer）**，route 只保留 user-only 与取值校验。
依据：§7 #17/#20 两条都是「守卫只放 route ⇒ 直调 repo 绕过」被判红的形状；fail-closed
的那一边就是把守卫放在所有入口共用的那一层。REST 侧因此返回 409 而非 400 —— 若评审要
400，改 route 加一次前检即可，DB 守卫不动。

**G3. 强制点一用 `>=` 还是 `>`？设计写 `count >= budget`，照做。**
补一条依据：认领中的父任务本身就是一条非终结 spec 行，会被全树计数数到；用 `>` 会让
`count == budget` 时还能再开一棵子树，越过上界之后才被 schedulability 发现。变异 M7 实测红。

**G3b. 两个强制点的相容性（修复轮 1 补裁决）。**
点一除库存外复用 `can_add_tree_member(B,N) = (N+1 <= B)`；点二继续用 v5 的
`deterministic_share`。纯性质测试遍历 `B,N∈[0,64]`，双向断言
`can_add_tree_member(B,N) == 创建后所有成员 share > 0`，既证明 soundness 也证明边界允许性；
真实 adapter 交错再钉住接线：B=2 的首个 child 完成父任务后，库存已回落但第二个 child 因
成员数被拒。孤根按有效 B 与 ceiling 比较：默认 B=32、ceiling=40 时不能短路，PATCH 回 NULL
仍只准入 32；B>=ceiling 时才保留零递归优化。

**G4. 「求不到根」包含哪些形态？设计只说「求不到根」。**
选（fail-closed 一侧）：① 向上 CTE 返回 0 行或 >1 行；② 根深度 > `MAX_WAVE_TREE_DEPTH`；
③ 树成员里出现超深节点；④ 成员集合**不含**发起查询的那个 wave。四者都判
`RootUnresolved`。依据：§8 M-B2「一条断链会让整棵子树无约束」；④ 是「我们要按之划分预算的
形状，不是这个 wave 实际所在的形状」，同一条理由。
修复轮 1 给③加了「从中毒树根发起」的生产查询验收；④ 因数据库自然形状不可构造，抽出
`tree_share_from_members` 纯 seam 直接喂一个不含 caller 的成员集。两条各自的单点变异都实测红。

**G5. 两个 bound 同时收紧时报哪个诊断？设计没写。**
选：`share <= spec_task_ceiling` 时报 `tree_budget_exhausted`（并指名根 wave），否则维持
`spec_task_ceiling`。相等时单独抬 ceiling 仍无法多放一条，必须指向根预算；Rust 的
`TASK_DIAGNOSTIC_ACTIONS` 固化 recovery action，web 测试读取该契约并校验文案旋钮。严格 `<`
变异在 `share=ceiling=32` 下实测红。

## 3.1 修复轮 2 实现收口

- B1：孤根短路从“列恰为 NULL”改成“同源有效 B 可证不更紧”，消除 PATCH 清回默认解除上界。
- B2：删除登记表与 AST 枚举，换成 crate-wide SQL 字符串性质门；SQL 注释不能伪造 depth 谓词。
- M1：库存与成员守卫各用另一守卫明确放行的 fixture，且拒绝后的零写入在同一 tx 内观察。
- M2：ORDER BY 结构门先去 SQL 注释；行为夹具固定 id 顺序与 created_at 顺序相反。
- M3：Rust recovery action 为单一契约，web 做跨文件校验；两边任改一边均有实测红变异。
- MINOR：相等份额归因、双向相容性、migration fixture 集合相等、web 缺参回退均补齐。

## 3.2 修复轮 3 实现收口

- B1：`effective_limit(Option<i64>, default)` 成为 ceiling 与 B 的共同解析函数；shortcut、投影状态、
  `wave_tree_budget` 都不再自行解释裸列。`ceiling=NULL,B=1` 只准 1 条。
- B2：根预算 PATCH 同事务枚举 `WAVE_TREE_MEMBERS_SQL` 并逐成员 rebuild；子 pending 在响应前被裁，
  随后的生产 claim seam 返回 0。
- M-1/M1/M-2：性质门覆盖 calm-truth + calm-server 的生产 Rust 和 crate 内 `.sql`；解析单个 CTE
  括号体与 UNION 递归成员，`RECURSIVE` 可选，只接受 ON/WHERE 中绑定递归 alias 的正序/反序上界。
  各 literal 不再跨文件/跨 const 拼接，仅 literal-only `concat!` 按真实语义合并。
- MINOR：删除 mutation-map 对退休登记门的引用并重写已过期的 crate-wide 结论；web 现在显示
  `root_wave_id`，多树场景能直接指出应修改的根。

## 3.3 修复轮 4 实现收口

- B1：`tasks_rebuild_tree_tx` 成为 B/N 两个触发点的唯一整树重投影例程；承重性质验收在生产
  projection/claim/admission/adapter 路径跑 B=8、B=12 两个构造。删 N 触发调用后精确 RED 为
  `(9>8, 15>12)`。
- M1：成员只枚举一次，每个投影消费预计算 `WaveTreeTerm`；分组 live 后置复核是第二次且最后
  一次递归树查询。`tree_cte_queries` 要求查询数恒为 2；退回逐成员求树后实测 6 并 RED。
  `MAX_TREE_TASK_BUDGET=64` 同时封顶合法 N 与写事务成员循环。
- B2：SQL 门只接受不处于任一可绕过 OR 支路的 alias-qualified depth 合取项；生产谓词两种
  `OR 1=1` 排列均实测 RED。crate 清单删除，改从 workspace manifest 扫所有成员；provider 探针 RED。
- web：从 Rust action 表解析实际 action 后渲染两个 capacity 诊断，断言均为 `Review capacity`；
  web 单边删 tree action 后 56 中 1 failed。

## 4. 停下来没做的（**不就地扩范围**）

- **N1. 既有 `non_user_policy_patches_are_forbidden_without_rows_or_events` 是恒真断言。**
  M13 的过程实测发现：`X-Calm-Actor: ai:codex` 走 REST 时 `to_actor_id()` 造出空 card id 的
  `AiCodex`，下游另有一条 403，所以「删掉 user-only 闸 ⇒ 仍是 403」。我**只加固了自己的**
  那条断言（改成断言理由字符串），**没有**去改既有的 `spec_task_ceiling` / `automation_policy`
  测试 —— 那是 PR-A 之前就存在的形状，改它要连带审视 REST 的 AI actor 归因路径（另一个载体）。
  **建议单开 issue**：`ai:*` actor 经 REST 的空 card id 403 把所有 user-only 闸的验收变成恒真。
- **N2. 没有新增持久载体。** 整树例程只在现有 PATCH / child-wave 写事务内编排既有投影；
  这不是 fork C7 所否决的“用 rebuild 顺序决定共享 pending 配额”。
- **N3. 没有另加 claim 时的树级共享计数。** B/N 变化后用逐成员 live 后置复核关闭 in-flight
  超额；日常 claim 仍只消费确定性 per-wave share，不引入依赖兄弟投影产物的路径依赖。
- **N4. `tree_task_budget` 没有进 `Wave` / `WaveRow`。** 照 `spec_task_ceiling` /
  `parent_wave_id` 的先例（doc-as-plan §11：`WaveRow` 的显式 SELECT 有 8 处，sqlx 运行时才炸）。
  读它的只有内核 SQL。r3 m-4 仍仅为可观测性缺口；补公开字段会扩 OpenAPI/event replay/8 处显式
  SELECT，超出本轮预算正确性修复。
- **N5. SQL 注释剥离没有抽成生产公共 API。** `wave_tree.rs` 的 ORDER 结构断言与集成性质门都只在
  测试编译期运行，但前者只需固定常量的简单检查，后者需要带引号状态机并扫描外部文件；为消除
  测试辅助代码重复而公开新库 API 得不偿失。r3 m-3 记录保留，性质由各自行为夹具独立购买。
- **N6. 运行时任意 `format!/push_str` 生成 SQL 仍不是静态门可完备证明的对象。** 门覆盖 Rust
  literal、literal-only `concat!`、所有 workspace member 的 `.sql`（含常规 include/query_file/migration 载体）；
  若未来确需动态构造递归树 SQL，应迁到可扫描 `.sql`/完整 literal，而不是削弱门。

## 5. 门（实际数字）

| 门 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt --all --check` | 干净（无输出） |
| clippy | `cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings` | `Finished dev profile in 1m 12s`，0 warning |
| 测试 | `cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci` | **3395 tests run: 3395 passed, 89 skipped**（104 binaries，55.475s；编译 1m45s） |
| 生成物 | `web: npm run gen:api` 后 `git diff --exit-code -- src/api/openapi.json src/api/generated.ts src/api/generated-terminal.ts src/api/generated-events.ts src/editor/types/` | 干净，无生成物漂移 |
| web build | `npm run build` | 成功，`built in 825ms`（仅既有 CSS highlight / chunk-size 警告） |
| web test | `npm run test` | **85 files / 1232 tests passed**，`Type Errors no errors` |
| fe lint | `npm run lint` | `no dependency violations found (102 modules, 232 dependencies)` |
| fe build | `npm run build` | 成功，`built in 203ms` |
| fe test | `npm run test`（含 `test:wire` + `test:mock-drift`） | **758 passed / 1 skipped**，wire 与 mock 均无漂移 |

环境：`CARGO_BUILD_JOBS=6`，nextest 取自 `.local-bin`，`NEIGE_CODEX_BIN` 全程未设置，
web/fe 最终全门显式使用 Node `v22.22.2`。

修复轮 4 的复原后定向回归：`calm-truth` wave-tree 集合 **31/31**，全 workspace SQL 性质门
**15/15**，`calm-server` child + policy 套件 **18/18**，web report-block 文案 **56/56**。

修复轮 4 最终 Rust/web/fe 全门全绿；web/fe 从第一条正式命令起均显式使用 Node 22.22.2，且
实现 worktree 内 `node_modules` 存在，vitest 确实执行（不是缺依赖的结构性复核）。

## 6. 已知代价（已登记进 doc-as-plan §12.1 #19）

`share = floor(B/N)` ⇒ `B=32`、`N=10` 时每个 wave 只剩 3 条未结额度：一棵大树里单个 wave
会比它独立存在时更早撞上限。**校准装置 = `tree_budget_exhausted` 的发生率**；频繁触发说明
`tree_task_budget = 32` 定低了 —— **调常数，不要改分配律**（改成共享计数会直接击穿 D.1 #11，
见变异 M1）。
