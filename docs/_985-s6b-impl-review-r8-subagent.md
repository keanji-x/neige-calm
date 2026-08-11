# #985 切片 6 PR-B 实现评审 r8（subagent 通道，最终收敛检查）

对象：`c71e4132` → `e9a1052a`。全部结论带文件:行号，全部验证在 `/tmp/wtb3` 实跑。
测试命令：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= cargo test ...`。
基线：`cargo test -p calm-truth --lib` **355 passed / 0 failed**；`-p calm-types` 219 passed。
web 只做结构性复核（本 worktree 无 `node_modules`，**未实际执行**）。

**结论汇总：BLOCKER 0 / MAJOR 1 / MINOR 3。**

## MAJOR-1 冻结态「最小 B」只保证 occupancy 装得下，不保证能再准入一条；自成员超额时按建议照做仍 0 准入

**结论**：`minimum_tree_task_budget` 在 `admission_frozen` 分支取
`max(该成员 share 首次增长的最小 B, 全员装回所需最小 B)`
（`crates/calm-truth/src/db/sqlite/task_projection.rs:937-947`）。当**超额成员就是被读的这个 wave 自己**时，两个条件在同一个 B 同时满足，而此时 `share == 该成员 occupancy`，
`tree_capacity = share - tree_occupied = 0`（`task_projection.rs:639-641`），准入仍为 0。
与 r7 修的那条回归**同形状**：诊断点名的旋钮按它给的数值照做，什么都不发生。

**触发条件**（与 r7 表驱动第 8 行唯一差别是 `target_index: 1 → 0`，超额落在自己而非 sibling）：
升级态（0068 legacy 回填），N=2 树的成员 0 有 5 条 legacy live spec，`tree_task_budget=4`
⇒ `share=2 < 5` ⇒ 冻结，诊断 `minimum_tree_task_budget = 9`。抬到 9（`share(9,2,0)=5`，
`5 ≤ 5` 通过 `require_tree_budget_postcondition`，`crates/calm-server/src/wave_report.rs:231-252`，
即真实 PATCH **会接受**）后准入仍是 0，诊断改口说「至少 11」。

**证据**：计算点 `task_projection.rs:937-947`（`> share.share`，未与本成员 occupancy 比较）；
解冻最小值 `crates/calm-truth/src/db/sqlite/wave_tree.rs:264-273`（只要求 `fixed_live <= share`，等号即可）；
容量点 `task_projection.rs:638-641`；验收表唯一的冻结用例把超额放在 sibling
（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:780-789`，`target_index: 1`），这一族从未被实例化。

**我实际跑过的验证**：
1. 自写最小复现（临时加入 `wave_tree_budget_tests.rs`，已复原）
   `db::sqlite::wave_tree_budget_tests::r8_self_overage_frozen_minimum_is_insufficient`
   → **FAILED**：`frozen actions = {"raise_tree_task_budget": Some(9)}, admitted = 0`；
   `after raising to 9: admitted = 0, actions = {"raise_tree_task_budget": Some(11)}`。
2. 自写穷举扫描（临时）`r8_exhaustive_capacity_action_sweep`：
   `N∈1..3 × B∈0..6 × index × ceiling∈0..5 × {无占用, 3 条 legacy live}`，
   对每个被拒实例执行诊断点名的**全部**动作（ceiling+1；预算=诊断携带的最小值）后要求准入增加
   → **FAILED，210 例**，全部落在「自成员超额」族。
3. 候选修法验证：`task_projection.rs:939` 的 `> share.share` 改为 `> share.share.max(tree_occupied)`
   后重跑，扫描失败 **210 → 35**，复现 **PASS**，`the_diagnosed_capacity_action_increases_admission`
   仍 **PASS**（不回归）。已复原。

**最小修法**：即上述一行；并在 r7 的表里补一行 `target_index = 冻结成员本身`（第 8 行改 `target_index: 0`）。

## MINOR-1 冻结态从不点名本地 ceiling，`ceiling=0` 时两个旋钮都挡着而只报一个

`task_projection.rs:927-934`：`admission_frozen` 无条件走 `tree_bound`，永不产生 `spec_task_ceiling`。
若该 wave 的 `spec_task_ceiling=0`（或 ≤ 自身 block 占用），解冻后本地 ceiling 仍是 0 容量。
**验证**：上述扫描在应用 MAJOR-1 候选修法后剩余的 **35 例全部是 `C=0 & 冻结`**
（`grep -cv "C=0 preblock=3"` → 0）。
与「平局必须同时点名两个旋钮」的原则相违，但冻结确实是支配性原因、解冻后下一轮就会报 ceiling，
可作已登记缺口。最小修法：冻结分支同时 push `ceiling_diagnostic` 当 `ceiling_capacity == 0`。

## MINOR-2 `budget == MAX_TREE_TASK_BUDGET` 时文案变成「至少 0」/ 空数字

`minimum_for_target` 的搜索区间是 `budget+1..=64`（`task_projection.rs:938`），预算已是上限时为 `None`，
参数缺席；渲染侧 `crates/calm-types/src/report_blocks/tasks.rs:287-290` 用 `unwrap_or_default()`，
web 侧 `web/src/pages/report-blocks/task.tsx:83-90` 用 `?? ''`。
**验证**：临时单测渲染（已复原）实际输出 `…raise tree_task_budget on the root wave to at least 0 …`。
可达（PATCH 允许 `budget=64`，`crates/calm-server/src/routes/waves.rs:1282-1291`），需 64 条 live spec 行；
替代出路「等在途工作跑完」仍在，属文案缺陷。最小修法：`None` 时改说「已达上限」。

## MINOR-3 `tasks_rebuild_tree_tx` 手搓 `TreeShare` 时硬写 `admission_frozen: false`

`crates/calm-server/src/wave_report.rs:207-212`。Σ ≤ B 由 `require_tree_budget_postcondition`
（`wave_report.rs:231-252`）兜住，不构成破口；但这是生产侧「重新实现 term」，
未来给冻结加行为会静默漏掉。建议注释点明它依赖后置条件。

---

## 修订轮 7 的五处，哪几处修出了新洞

| # | 修订内容 | 判定 |
| --- | --- | --- |
| 1 | 平局同时点名两个旋钮（`task_projection.rs:927-1021`） | **无新洞**。变异 A（平局只 push ceiling 诊断）→ `the_diagnosed_capacity_action_increases_admission` **FAILED**：`default singleton tie: {"raise_spec_task_ceiling": None} did not increase admission: 32 -> 32`。 |
| 2 | 8 行表驱动验收（`wave_tree_budget_tests.rs:709-790`） | **有洞（覆盖洞）**。8 行是**手挑构造**不是枚举；缺「超额成员 = 目标成员」与「ceiling 占用 > 0」两族 → 直接放过 MAJOR-1 与 MINOR-1（我的穷举扫描 210 例红）。 |
| 3a | 目标 B 取「share 首次增长的最小 B」 | **无新洞（非冻结路径）**。变异 B（改回朴素 `budget+1`）→ **FAILED**：`tie on a remainder recipient: {"raise_tree_task_budget": Some(6)} did not increase admission: 2 -> 2`。 |
| 3b | 冻结态再取 `max(解冻最小 B)` | **有新洞 = MAJOR-1**。变异 C（冻结分支丢掉 `zip(minimum_budget_to_unfreeze)`）→ **FAILED**：`sibling legacy overage freezes the target: {"raise_tree_task_budget": Some(6)} did not increase admission: 0 -> 0`，说明它买住了 sibling 方向、恰恰漏掉自身方向。 |
| 4 | 删两条恒真测试 + `repair_wave_tree` 下线 | **无新洞**。整树入口的承重点是生产代码里的 `tree_cte_queries != 2` 硬失败（`wave_report.rs:224-229`），不是被删的断言；`repair_wave_tree` 全仓无残留引用（仅 docs）。 |
| 5 | 旧条目更新 | **抽查 3 条属实**。① R7-m3（mutation-map:281）：把 `("tree_root_unresolved","repair_wave_tree")` 加回 `tasks.rs:70` 后该测试 **FAILED**（`Diagnostic::coded` 在 `tasks.rs:179` 断言注册表）。② R5-m1（:260）：`singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling` **1 passed**，`child_wave_adapter.rs:1239` 确实断言两入口同为 `["spec_task_ceiling","tree_budget_exhausted"]`。③ M9（:257）：替代承重点 `wave_report.rs:224-229` 确在生产代码中。 |

## 可以合入了吗

**NO**（仅因 MAJOR-1；其余三条 MINOR 可登记）。

- **可达的错误后果**：升级态（0068 legacy 回填）里超额成员就是用户正在看的 wave 时，
  诊断给出的 `minimum_tree_task_budget`（复现里为 9）**被真实 PATCH 路径接受**，
  但准入仍为 0，诊断改口要 11。用户按机器给的数值操作一次得到零效果。
- **为什么不能作为已登记缺口合入**：与本轮唯一要修的 r7 回归同形状、同函数、同提交内新增分支；
  新验收把这个数明确解释为「可用的最小值」（`wave_tree_budget_tests.rs:842-844`），
  登记等于让刚立的断言在一族可达输入上为假。修法一行 + 表里一行数据，实测扫描失败 210→35 且不回归。

## git status --short

```
?? docs/_985-s6b-impl-review-r8-subagent.md
```
（所有临时变异/临时测试已 `git checkout -- .` 复原。）
