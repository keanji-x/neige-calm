# #985 切片 6 PR-B 实现评审 r7（收敛检查）— subagent 通道

范围 `c71e4132..017d55d9`。环境：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。
复原态定向门（本机实跑）：`calm-truth --lib` **357/357**、`bounded_wave_tree_sql` **18/18**、
`calm-server --lib` **693/693**、`domain_api_suite` **242/242**。web **未实际执行**（无 `node_modules`），
仅结构性复核。临时改动已全部 `git checkout -- .` 复原。**BLOCKER 0 / MAJOR 1 / MINOR 5**。

---

## MAJOR-1 平局归因让「抬 spec_task_ceiling」在**默认配置**下变成空操作（既有旋钮的回归）

- **结论**：`tree_capacity == ceiling_capacity` 时诊断只名 local ceiling，但此时两侧同时绑定，
  **单独抬 ceiling 不会让准入增加一条**。默认部署就是平局：`spec_task_ceiling` 默认 32、
  `tree_task_budget` 默认 32、孤根 `N=1 ⇒ share=32`。
- **触发条件（零配置可达）**：新建 wave（不设任何旋钮），报告写 34 条 spec 声明。
- **证据**：`crates/calm-truth/src/db/sqlite/task_projection.rs:923-926`（`tree_capacity < ceiling_capacity`
  才归因 tree）+ `:635-646`（`capacity = ceiling_capacity.min(tree_capacity)`）；
  文案/动作 `crates/calm-types/src/report_blocks/tasks.rs:66-77`。
- **我实际跑过的验证**（就地探针，已复原）：
  - `probe_default_singleton_root_tie`：默认孤根 34 声明 → 落 32 条，诊断
    `code=spec_task_ceiling action=raise_spec_task_ceiling`；照做把 ceiling 抬到 33 后
    **仍是 32**（`PROBE default after raise_spec_task_ceiling=33 -> 32`）。PR-B 之前这一步会变成 33。
  - `probe_tie_case_diagnosed_action_increases_admission`（root+child、ceiling=2、B=4、share=2）：
    **RED** `following raise_spec_task_ceiling did not increase admission: 2 -> 2`；
    同一探针继续跑，第二轮诊断确实翻成 `raise_tree_task_budget`，再抬 B 到 8 后 2→3（两步自愈）。
- **本轮新验收只买了两个实例**：`the_diagnosed_capacity_action_increases_admission`
  (`wave_tree_budget_tests.rs:698-718`) 的循环只有 `(ceiling,budget) = (4,2)`、`(2,4)` 两组
  **严格不等**构造；平局与 legacy 平局（`ceiling == share - legacy`）都不在其中。
  而同一文件 `:630-669` `an_equal_tree_share_reports_the_local_ceiling_knob` 恰把平局态钉成「正确」——
  两条验收互相矛盾，是本轮修复点上长出的同形新洞。
- **最小修法**：平局时同时给出两个绑定旋钮（该 candidate 追加 `tree_budget_exhausted`，保留
  `spec_task_ceiling`），并把新验收改成表驱动 `(4,2)`/`(2,4)`/平局 `(2,4)+N=2`/legacy 平局 `(3,5,L=2)`，
  语义改成「照**每一条**动作做完后准入必须增加」。约 ≤30 行。

## MINOR

1. **冻结态文案与事实不符**：`admission_frozen` 时（`task_projection.rs:923-925`）无论本 wave
   份额是否用尽都发 `tree_budget_exhausted`，文案却说 “this wave's slice of {share} is used up”
   (`calm-types/src/report_blocks/tasks.rs:283-291`)；`legacy_member_overage_freezes_new_blocks_across_the_tree`
   (`wave_tree_budget_tests.rs:1144-1155`) 只盯了后半句。动作 `raise_tree_task_budget` 本身是对的。
2. **`a_singleton_tree_runs_two_constant_size_recursive_queries` 名不副实**
   (`wave_tree_budget_tests.rs:842-867`)：删短路后 `tree_cte_queries` 在任何可解析树上恒为 2
   (`wave_tree.rs:198,217`)，无法区分孤根与 64 成员树；“constant size” 没有任何断言。
   真正承重的是 `crates/calm-server/src/wave_report.rs:221-226`（见变异 M-B，8 failed，**不含**这条）。
3. **`repair_wave_tree` 没有对应 API**：`WavePatch` 不含 `parent_wave_id`，文案
   (`tasks.rs:292-296`) 让用户「修复或删除子波链接」，实际只有删 wave 一条路
   (`crates/calm-truth/src/db/sqlite/wave.rs:271-283`)。该状态只能来自数据损坏，故仅 MINOR。
4. **旧 schema 夹具无结构性护栏**：`crates/calm-server/tests/cases/migration_0068_projection_policy.rs:66-93`
   的 baseline `waves` 是手写子集，缺列只在运行期以 `no such column` 暴露（本轮正是这么发现的）。
   本次补的 `created_at` 是真实 head 列、且是最小补丁（M-D 已证），**没有**掩盖别的缺列。
5. **读路径版本数的文档漂移**：`task_projection.rs:498-530` 仍写「只剩两处读在单语句之外」，
   删短路后每次 `evaluate_schedulability` 又多 3 条自治语句（`wave_tree.rs:193,212,221`）。
   性能无碍：`EXPLAIN QUERY PLAN` 实测下行 CTE 走
   `SEARCH w USING INDEX idx_waves_parent_wave_id (parent_wave_id=?)`（0071 偏索引可用）。

---

## 一、修订轮 6 的五处，哪几处修出了新洞

| # | 修复 | 结论 |
|---|---|---|
| 1 | 删孤根短路 + `NotInTree` | **无新洞**。全仓无 `NotInTree` 残留读者；读/rebuild/helper 均无隐式依赖；偏索引仍生效；唯一副作用是 MINOR-2 的测试名。 |
| 2 | 新验收「照诊断做，容量真增加」 | **有新洞**：只买两个严格不等实例，漏平局与 legacy 平局（MAJOR-1）。 |
| 3 | 平局取本地旋钮 | **有新洞**：默认配置即平局，动作无效（MAJOR-1）。 |
| 4 | `workspace_member_roots` fail-closed | **无新洞**。根 `Cargo.toml:3-18` 是 14 条显式路径、无 glob、无虚拟 manifest；误伤面为零，且 `a_missing_workspace_member_root_fails_closed` 是真 `should_panic`。 |
| 5 | 旧 schema 夹具补 `created_at` | **无新洞**，补的是最小夹具（M-D 已证必要），治理面见 MINOR-4。 |

## 二、`Σ_v live_spec(v) ≤ B` 在含 legacy 的真实部署下成立吗

**成立**（升级前既有超额除外，且实现从不主动扩大它）。依据：

1. 每次投影 `live_spec(v) = tree_occupied(v) + admitted(v) ≤ share(v)`——`tree_occupied =
   ceiling_occupied + non_block_live_spec`（含 0068 legacy），`capacity = min(ceiling_cap,
   share - tree_occupied)`：`task_projection.rs:624-646`；`Σ_v share(v) = B` 由
   `deterministic_share` + `shares_sum_to_the_budget_including_the_remainder`
   (`wave_tree.rs:138-146`、`:372-395`) 锁住。
2. 改 `N` 的唯一生产写者是 child-wave 创建，它在**同一 tx** 内跑整树重投影 + 逐成员/总量后置条件：
   `crates/calm-server/src/operation/child_wave_adapter.rs:202,272`、
   `crates/calm-server/src/wave_report.rs:217-226,228-249`；改 `B` 走 `routes/waves.rs:1334-1341`
   （root-only 由 `wave.rs:237-249` 强制）。任一成员留有超份额在飞 ⇒ `Conflict` 全写回滚。
3. 减 `N` 只放松；删 wave 要求叶子（`wave.rs:271-283`），`parent_wave_id` 生产侧只在
   child-wave 创建时写一次（`child_wave_adapter.rs:202`，全仓无第二个生产写点）。
4. legacy 超额（升级日 `Σ legacy > B`）：所有成员 `capacity = 0`，并由
   `admission_frozen`（`wave_tree.rs:250-256`）冻结整树，Σ 只会随终态化下降。
   我用「`non_block_live_spec` 恒 0」变异确认这条占用是承重的（4 条 RED）。

## 三、可以合入了吗

**NO** —— 仅因 MAJOR-1，修法已收敛到一处（平局同时命名两个旋钮 + 新验收改成含平局的表，≤30 行）。
不修的可达后果：**默认配置**下一个 wave 写满 32 条 spec 后，UI 给出
“Review capacity → raise this wave's ceiling”，用户把 `spec_task_ceiling` 从 32 抬到 64，
准入仍是 32——PR-B 对既有旋钮引入的静默回归（PR-B 前该操作会生效）。

若编排方选择带 issue 合入：`Σ ≤ B` 成立、无数据面风险、第二次投影会正确翻成
`raise_tree_task_budget`（实测 2→3），两步自愈；但本轮恰是为买「诊断可操作」而加的验收，
留洞会让该性质名存实亡。

---

## 附：我实际跑过的变异（每条施加后立即反向复原）

| # | 改坏什么 | 结果 |
|---|---|---|
| M-A | `task_projection.rs:925` 平局 `<` 改回 `<=` | **RED**：`an_equal_tree_share_reports_the_local_ceiling_knob` + `singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling` 各 1 failed |
| M-B | `project_tasks_with_tree_term_tx` 丢弃传入 term、逐成员重跑 `wave_tree_term` | **RED**：`calm-server --lib` 8 failed / 693（含 `whole_tree_live_spec_never_exceeds_budget_across_admitted_growth_sequences`）；证明 `wave_report.rs:221` 计数缝仍承重 |
| M-C | `non_block_live_spec` 恒 0（legacy 不占份额） | **RED**：`calm-truth --lib` 4 failed（两条 singleton legacy + 冻结 + 消耗） |
| M-D | 回退旧 schema 夹具的 `waves.created_at` | **RED**：`no such column: w.created_at`（`migration_0068_projection_policy.rs:135`），证明补列必要且非掩盖 |
| M-E（我的新构造） | 不改实现，按平局构造跑新验收的性质 | **RED**：默认孤根 32→32、`(2,4)` 树 2→2（见 MAJOR-1） |

## 附：`git status --short`

```
（干净：无输出）
```
