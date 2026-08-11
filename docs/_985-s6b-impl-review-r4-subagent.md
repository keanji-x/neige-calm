# #985 切片 6 PR-B —— 实现评审 r4（收敛检查 / CHANNEL = subagent）

范围：`c71e4132` → `3635a8c1`。环境：`PATH` 含 `.local-bin`，`CARGO_BUILD_JOBS=6`，
`NEIGE_CODEX_BIN` 未设置。**本 worktree 无 `web/node_modules`，所有 web 结论均为结构性复核，
未实际执行 vitest**（已如实标注）。所有变异均已 `git checkout --` 复原，末尾附 `git status --short`。

基线（本轮实跑）：
- `cargo test -p calm-truth --lib -- wave_tree` → **30 passed / 0 failed**
- `cargo test -p calm-truth --test bounded_wave_tree_sql` → **13 passed / 0 failed**
- `cargo nextest run -p calm-server -E 'test(wave_projection_policy_patch) or test(child_wave_adapter)'` → **16 passed / 0 failed**

分级：**BLOCKER 0 / MAJOR 1 / MINOR 4**。

---

## 一、修订轮 3 的五处，哪几处修出了新洞

| # | 修订点 | 结论 |
|---|---|---|
| 1 | 统一 `effective_limit` | **干净**。无第三读者、默认同源 |
| 2 | 整树重投影 | **PATCH 侧干净；但"配额影响范围"与"重投影范围"仍不一致** → MAJOR-1 |
| 3 | SQL 性质门判据 | **非空洞（实证）；但两处规避面 + 一处误红** → MINOR-1/2/4 |
| 4 | 单一诊断契约 | Rust→web 方向成立；web→Rust 方向**有一处未被任何断言覆盖** → MINOR-3 |
| 5 | 过期条目重跑 | 抽查 10 条全部属实，见第二节 |

### 1. 统一解析函数 —— 干净

`effective_limit`（`crates/calm-truth/src/db/sqlite/wave_tree.rs:42`）确为唯一入口。全仓
`grep tree_task_budget|spec_task_ceiling` 的生产读者只有三处，全部经它：
`wave_tree.rs:180`（shortcut ceiling）、`wave_tree.rs:277-283`（`wave_tree_budget`）、
`task_projection.rs:465`（投影状态 ceiling）。`child_wave_adapter.rs:159` 走的是同一个
`wave_tree_budget()`，不自解释裸列。两侧默认常量分别是 `DEFAULT_SPEC_TASK_CEILING`
（`wave_tree.rs:32`）与 `DEFAULT_TREE_TASK_BUDGET`（`wave_tree.rs:28`），**语义上就应该是两个常量**
（一个是 per-wave，一个是 per-tree），不是漏统一。

实跑验证（两条都 RED，见第二节 R3-B1a/R3-B1b）。

### 2. 整树重投影 —— 范围仍窄于"配额影响范围"（MAJOR-1）

**MAJOR-1｜树生长会缩小每个既有成员的 share，但没有任何路径重投影它们；`Σ_v live_spec(v) ≤ B` 可被生产路径证伪。**

- 结论：`wave_tree.rs:111-112` 把 `Σ share = B` 明确升格为 `Σ_v live_spec(v) ≤ B` 的依据。
  该式**不成立**。r3 只给"B 变了"（`routes/waves.rs:1330-1346`）配了整树重投影；
  "N 变了"（`child_wave_adapter.rs:167-173` 创建成员）同样改变每个成员的 share，
  却**不重投影任何既有成员**。这正是 r3-B2 修复形状的孪生洞。
- 触发条件：root 先把自己的额度用满（此时它的 share 还大），随后在库存 `< B` 的窗口里加成员。
  两道生产准入（`child_wave_adapter.rs:161` 库存、`:168` 成员上界）**全部放行**。
- 证据：`crates/calm-truth/src/db/sqlite/wave_tree.rs:111`、`:129`；
  `crates/calm-server/src/operation/child_wave_adapter.rs:159-173`（创建后无重投影）；
  `crates/calm-server/src/routes/waves.rs:1338-1352`（只有 PATCH 有整树重投影）；
  `docs/_985-s6b-impl-notes.md:39` 的 G1/N3 只登记了 "B 变了" 这一侧。
- **我实际跑过的验证**（临时探针，已复原）：
  - `probe_tree_total_exceeds_budget_after_admitted_growth`：B=8，root 投影 4 条后依次加两个成员，
    每一步都先用生产函数 `wave_tree_spec_inventory` / `wave_tree_member_count` /
    `can_add_tree_member` 断言"点一会放行"。结果
    `per_wave=[3,4,2] total=9 budget=8` —— **超 1**。
  - `probe_excess_grows_with_tree_width`：B=12，root 投影 6 条后加 3 个成员，同样每步断言点一放行。
    结果 `counts=[3,3,3,6] total=15 budget=12` —— **超 3，且随树宽增长**。
  - 对照：既有 30 条 `wave_tree_budget_tests` 全绿，**没有一条断言过全树总量**——
    它们只断言单 wave 的 share 与诊断，所以这个洞对现有验收集是不可见的。
- 最小修法（二选一，二者都不改分配律）：
  a) 点一在成员上界之外再加一条：**创建后每个既有成员的 live 计数 ≤ 其新 share**，否则拒；
  b) 或在 child 创建的同一事务内，复用 `routes/waves.rs:1338` 那段整树重投影
     （能裁掉 pending，但裁不掉已 in-flight 的超额，所以仍需 a 的兜底）。
  **无论选哪条，`wave_tree.rs:111-112` 那句断言在本 PR 内必须改成它真正买到的性质**
  （"每次准入时刻全树库存 < B"），否则仓库里留着一条假不变量。

其余关于点 2 的问句，逐条答：
- PATCH 子 wave 的 `spec_task_ceiling` **不**影响别人的 share —— 正确，`routes/waves.rs:1344`
  该分支只重投影自身（ceiling 是 per-wave 输入），范围与影响一致。
- 原子性：整树重投影在同一 `write_with_actor_events_typed` 写事务内（`waves.rs:1334-1360`），
  失败整体回滚；`PlanUpdated` 按各成员自身 scope 发（`:1381-1392`）。无问题。
- 大树性能：N 受 `can_add_tree_member`（`N+1 ≤ B`）间接封顶，最坏 N=B；单次 PATCH 的
  `tasks_rebuild_tx` 次数因此有界。可接受。

### 3. SQL 性质门 —— 非空洞，但有两处规避面 + 一处误红

先证**非空洞**（这是"门自己与生产同源/失效即绿"的关键）：门是从磁盘按路径读生产文件
（`bounded_wave_tree_sql.rs:445-453`），不共享生产宏。实测两条：

| 变异 | 目标 | 结果 |
|---|---|---|
| MUT-A：删 `bounded_wave_descendant_cte!` 的 `WHERE down.depth <= ?2` | `every_recursive_parent_wave_cte_in_production_crates_bounds_its_recursive_variable` | **RED**（12 passed / 1 failed） |
| MUT-B：删 `bounded_wave_ancestor_cte!` 的 `WHERE up.depth <= ?2` | 同上 | **RED**（12 passed / 1 failed） |

**MINOR-1｜析取式假通行证可以骗过判据。** `predicate_bounds_recursive_depth`
（`bounded_wave_tree_sql.rs:318-366`）只要求"存在一处 `alias.depth <=` 比较落在 ON/WHERE 里"，
不要求它是合取项。实跑 MUT-C：把生产的 `WHERE down.depth <= ?2` 改成
`WHERE 1=1 OR down.depth <= ?2`（语义上完全无界）→ 门 **STILL-GREEN（13 passed）**。
最小修法：命中点向前回溯到最近的 `or`，若该比较处在 `OR` 的任一支上则不计为 bound。

**MINOR-2｜登记表从"CTE 级"降级成了"crate 级"，仍是登记表。**
`bounded_wave_tree_sql.rs:446` 硬编码 `["calm-truth","calm-server"]`。实跑探针：在
`crates/calm-provider/src/zz_probe.sql` 放一条无界 parent-wave 递归 CTE → 门
**STILL-GREEN（13 passed）**。当前 `grep -rl parent_wave_id crates/*/{src,tests,migrations}`
确实只有这两个 crate（所以今天不是真洞），但**没有任何断言钉住这一点**。
最小修法：加一条元断言 —— 扫全 `crates/*`，凡文件含 `parent_wave_id` 而其 crate 不在列表里就红。

**MINOR-4｜合法的非限定 bound 会被误红。** MUT-D：把生产的 `WHERE down.depth <= ?2` 改成
`WHERE depth <= ?2`（SQLite 合法，且确实终止）→ 门 **RED**。方向是 fail-closed，可接受，
但失败信息会把一条正确 SQL 说成"没有 bound"。最小修法：在报错文案里加一句"限定名必需"。

误红面复核（均在复原态实跑为绿，含在 13 passed 里）：反序 `?2 >= down.depth`、
同语句无关递归 CTE、同文件两条无关字面量、literal-only `concat!` —— 三类历史误红确已消掉。

### 4. 单一诊断契约 —— 只有一个方向真的红

- Rust 改一边 → 红：**成立**。`report-blocks.test.tsx:895-901` 直接读
  `crates/calm-types/src/report_blocks/tasks.rs` 抽 `TASK_DIAGNOSTIC_ACTIONS`，并硬编码
  两个期望值；契约本身（`tasks.rs:69-72`）改任一格都红。**未实际执行 vitest**，但断言不可能恒真：
  期望值是字面量，与被读源文件不同源。
- web 改一边 → 红：**只覆盖了文案**（`treeCopy` 的 `/top wave/` + `root_wave_id`）。
  **MINOR-3｜`web/src/pages/report-blocks/task.tsx:17-18` 的 action→标签映射
  （`raise_spec_task_ceiling` / `raise_tree_task_budget` → `Review capacity`）没有任何断言。**
  全仓只有 `report-blocks.test.tsx:900-901` 提到这两个字符串，且那是读 Rust 的那条。
  把 `:17` 的字面量打错，"Review capacity" 静默退化成 "Open related item"，**结构上无测试会红**。
  最小修法：在 `report-blocks.test.tsx` 里用 `actions.get('tree_budget_exhausted')` 作为
  `diagnostic.action` 渲染一次 `ReportTaskBlock`，断言出现 `Review capacity`。
  （结构性复核，未执行。）

---

## 二、旧变异条目抽查结果（10 条，全部实跑）

| 旧条目 | 我打的变异 | 目标测试 | 实测 | 与登记是否一致 |
|---|---|---|---|---|
| M2 | 删向下 CTE 的 depth 谓词 | `every_recursive_parent_wave_cte_..._bounds_its_recursive_variable` | **RED** | 一致 |
| M2' | 删向上 CTE 的 depth 谓词（登记未单列） | 同上 | **RED** | 补强 |
| M4 | `RootUnresolved` 臂改回 `(ceiling, None, false)` | `unresolvable_root_fails_closed_for_every_declaration` | **RED** | 一致 |
| M9 | shortcut 恒报"在树中" | `a_non_tree_wave_runs_zero_recursive_tree_queries` | **RED** | 一致（7.1 声称属实） |
| M11 | `deterministic_share` 丢余数 | `shares_over_a_real_tree_sum_to_the_budget` | **RED** | 一致 |
| R3-B1a | shortcut ceiling 裸 `unwrap_or(0)` | `a_null_ceiling_and_tiny_budget_still_bind_a_singleton_root` | **RED** | 一致 |
| R3-B1b | `DEFAULT_TREE_TASK_BUDGET` 32→31 | `resetting_an_explicit_budget_to_null_keeps_the_default_bound` | **RED** | 一致 |
| R3-B2 | `tree_budget_changed = false`（退回只重投影根） | `tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed` | **RED** | 一致 |
| R2-M1a | 点一库存 `>=` → `>` | `acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full` | **RED** | 一致（r3 改了 `wave_tree_budget` 解码路径，仍红） |
| R1-M6 | `projection_policy_changed` 去掉 `tree_task_budget` | `tightening_tree_budget_immediately_deletes_pending_projection_and_emits_plan_updated` | **RED** | 一致（r3 重排了该段，结论未过期） |

**它没想到会受影响的旧条目**：`R2-M1a/M1b` 依赖 `wave_tree_budget()`，而 r3-B1 改写了该函数的
解码路径，7.1 的保质期表里没有登记它们 —— 我补跑了 R2-M1a，**仍 RED**，结论未过期。
另外 `wave_tree_budget_tests` 里 **30 条没有一条覆盖"树形状变化后的全树总量"**，
这是 MAJOR-1 对既有变异集完全不可见的原因（不是某条变异过期，而是这一维度从未被断言）。

---

## 三、可以合入了吗

**YES（带一处必做的本 PR 内改动）。**

理由：本片没有 BLOCKER；四条 MINOR 都是门的硬度/覆盖问题，不影响生产正确性。唯一的 MAJOR-1
不是回归 —— PR-B 之前树上限根本不存在，本 PR 严格收紧（探针里超出量 9/8、15/12，
而非无界）。它是"性质比声称的窄"，而**声称本身是可以现在就改对的**。

合入条件（必做，≤3 行改动）：把 `crates/calm-truth/src/db/sqlite/wave_tree.rs:111-112` 的
`Σ_v live_spec(v) ≤ B` 改成本 PR 真正买到的性质（"每次成员准入时刻全树非终结 spec 库存 < B；
成员集合变化不回溯既有成员的既有额度"），并在 `docs/_985-s6b-impl-notes.md` §4 补一条
N6 登记该缺口。

跟进（另开 issue，不在本片扩范围）：MAJOR-1 的真正修法（点一增加"既有成员不得超新 share"
准入，或创建事务内整树重投影）；MINOR-1（`OR` 支不计为 bound）；MINOR-2（crate 列表元断言）；
MINOR-3（web action→标签断言）。

---

```
git status --short
?? docs/_985-s6b-impl-review-r4-subagent.md
（仅本报告；所有变异/探针均已复原，无残留）
```
