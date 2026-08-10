# #985 切片 6 PR-B 实现评审 r5（收敛检查）— subagent 通道

对象：`c71e4132` → `9f3fbed1`，工作树 `/tmp/wtb3`。
结论：**BLOCKER 0 / MAJOR 0 / MINOR 5**。可以合入。

跑过的基线（全绿）：
- `calm-truth --test bounded_wave_tree_sql`：15 passed
- `calm-truth --lib`：352 passed
- `calm-server --lib child_wave_adapter`：10 passed
- `calm-server --test domain_api_suite wave_projection_policy_patch`：8 passed
- web/fe：**未实际执行**（本 worktree 无 `web/node_modules`），只做结构性复核。
- `domain_api_suite` 全量：**未跑完**（共享 cargo 锁被并行评审通道长期占用），只跑了上面这条过滤子集。所有变异均已还原，`git status --short` 仅剩本报告。

---

## 一、修订轮 4 的五处，哪几处修出了新洞

| # | 处置 | 结论 |
|---|---|---|
| 1 | 承重验收 | **不是性质测试，是两组固定构造**；但确实承重（变异实测 (9,15)）。真正的兜底谓词（整树后置条件）**无任何测试** → MINOR-1 |
| 2 | 第三个触发点 | **没有**。删除只能删叶（`wave.rs:271`），N 只减不增，Σshare 恒为 B；cove 删除整树同去（跨 cove 父边被 `acceptance_21c` 禁掉）。**无发现** |
| 3 | O(N) 预计算 | **与逐成员计算不完全一致**：孤根 `budget == ceiling` 时诊断码分叉 → MINOR-2；同事务读写顺序无问题 |
| 4 | 合取判据 | `CASE WHEN` 仍能骗过（漏红）→ MINOR-3；`HAVING` / `IN` 被误红（偏严，安全方向） |
| 5 | manifest 推导 | **实测有效**，`.sql` 与新 crate 都被扫到。**无发现** |

---

## MINOR-1 — 整树后置条件是唯一兜底谓词，但零覆盖；生长路径的 409 也无验收

**结论**：`wave_report.rs:220-233` 的 `member_overage` / `total > budget` 是 `Σ ≤ B` 在**生长**路径上唯一的强制点；把它整段关掉，`child_wave_adapter` 10 条 + `wave_projection_policy_patch` 8 条**全绿**，而一条合法生产序列直接越界。

**触发条件**：root 已有 > ⌈B/(N+1)⌉ 条在飞（dispatched/running/verifying）spec 行时创建 child。准入点一只查 `inventory < budget` 与 `can_add_tree_member`，两者都放行。

**证据**：`crates/calm-server/src/wave_report.rs:220`（member_overage）、`:229`（total）；准入点一 `crates/calm-server/src/operation/child_wave_adapter.rs:272`；`can_add_tree_member` `crates/calm-truth/src/db/sqlite/wave_tree.rs:134`。

**我实际跑过的验证**：
- 变异 B2：把 `wave_report.rs:223` / `:229` 两个 `if` 改成 `if false && …` → `cargo test -p calm-server --lib child_wave_adapter` **10 passed**（含承重验收），`--test domain_api_suite wave_projection_policy_patch` **8 passed**。**全部 STILL-GREEN**。
- 自写探针 `zz_probe_growth_with_inflight_overage`（B=8、root 投影 5 条并全部 claim、再经真实 adapter 建 child、child 投 4 条）：变异态输出 `PROBE total_live_spec=9 budget=8` → **越界**；还原后同一探针拿到 `Conflict("wave tree change would leave member … with 5 unfinished spec task(s), above its new share of 4")` → **生产实现 fail-closed 正确**。探针已删除。

**最小修法**：把上面那条探针序列固化成一条验收（断言 `prepare_tx` 返回 `Conflict` 且 waves/tasks 零写入）。顺带在设计文档里写明：child-wave 创建从此会因在飞工作 409 —— 这是自驱主路径上的新失败模式，目前既无测试也无文档。

---

## MINOR-2 — 预计算 tree term 与逐成员计算在「孤根 budget == ceiling」处分叉

**结论**：同一个 wave、同一份文档，走 `tasks_rebuild_tree_tx` 得 `tree_budget_exhausted`，走 `tasks_rebuild_tx` 得 `spec_task_ceiling`。准入条数相同，但诊断码/动作/文案不同 —— 这是「rebuild ≡ incremental」(D.1 #11) 的漂移，且与 R2-m2 已锁死的契约（`share == ceiling` 归树旋钮）矛盾。

**触发条件**：孤根（无父无子），`effective_budget == effective_ceiling`（默认值即 32 == 32），PATCH 一次 `tree_task_budget`。

**证据**：短路臂 `crates/calm-truth/src/db/sqlite/wave_tree.rs:205` 用 `budget >= ceiling → NotInTree`；归因过滤器 `crates/calm-truth/src/db/sqlite/task_projection.rs:894` 用 `share.share <= ceiling`。两个闭区间在相等处重叠，而 `tasks_rebuild_tree_tx` 恒发 `Share`（`crates/calm-server/src/wave_report.rs:203-208`）。

**我实际跑过的验证**：自写探针 `zz_probe_singleton_rebuild_paths_agree`（ceiling=2、budget=2、3 条声明，同一 tx 内先后调两个入口）：
`PROBE rebuild paths disagree — left(plain): ["spec_task_ceiling"] right(tree): ["tree_budget_exhausted"]`。探针已删除。
反向确认契约方向：重放 R2-m2（`task_projection.rs:894` 改 `<`）→ `an_equal_tree_share_reports_the_tree_knob` **RED**，即相等处应归树旋钮 ⇒ 短路臂那一侧是错的一侧。

**最小修法**：`wave_tree.rs:205` 的 `budget >= ceiling` 改为 `budget > ceiling`（一个字符），并补一条「两个 rebuild 入口对同一孤根产出同一诊断码」的断言。

---

## MINOR-3 — 性质门：`CASE WHEN` 漏红；`HAVING` / `IN` 误红

**结论**：合取判据只看括号作用域里的 `or`，不看 `CASE`，因此 `CASE WHEN 1=1 THEN 1 ELSE down.depth <= ?2 END` 被判为「有界」，而它实际恒真、递归不终止。误红面同时扩大了两类真实有界写法。

**证据**：`crates/calm-truth/tests/bounded_wave_tree_sql.rs:323`（`comparison_is_conjunct` 只扫 `or`）、`:315`（`enclosing_condition_start` 只认 `on`/`where`，故 `HAVING` 一律不算 bound）、`:369`（只认 `<`/`<=`，故 `IN` 不算 bound）。

**我实际跑过的验证**：临时加探针 `zz_probe_equivalent_unbounded_shapes` 跑 6 种等价写法，输出：
`case_when=false(漏)` / `nested_parens_or=true` / `or_outer_scope=true` / `subquery_bound=false` / `having_bound=true(误红)` / `in_bound=true(误红)`。
嵌套括号与外层作用域 OR 两种绕法**都被挡住**，这两处不是洞。探针已删除。

**最小修法**：`comparison_is_conjunct` 里把 `case` 与 `or` 同等对待（出现在比较所在或外层作用域即判为可绕过）。`HAVING`/`IN` 的误红是安全方向，可只在文档里记一句「用 `WHERE alias.depth <= x` 这一种写法」。

---

## MINOR-4 — O(N) 的可数缝只能抓「回退到全量入口」，抓不到「循环里新增递归查询」

**结论**：`project_tasks_with_tree_term_tx` 把 `tree_cte_queries` **硬编码为 0**（`crates/calm-truth/src/db/sqlite/task_projection.rs:996`），起点 `2` 也是字面量（`crates/calm-server/src/wave_report.rs:196`）。所以这条守卫度量的是「有没有走回 `project_tasks_tx`」，不是「实际执行了几条递归 SQL」；在循环里直接新加一条 `wave_tree_*` 递归调用不会被计数。N=1 时（孤根走短路、贡献 0）也永远等于 2，无判别力。

**我实际跑过的验证**：变异 B4（`wave_report.rs:212` 的 `Some(tree_term)` 改 `None`）→ `wave_projection_policy_patch::tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed` **RED（500 vs 200）**，即守卫对「回退」这一类**非空洞**；同批另 7 条绿。

**最小修法**：或者接受当前语义并把注释改成「回退检测」，或者让计数落到真正执行 SQL 的函数上（`wave_tree.rs` 内加一个 per-connection 计数器）。

---

## MINOR-5 — `origin='legacy'` 的非终结 spec 行不计入份额占用，但计入树库存

**结论**：`occupied` 只数 `declared_by='spec' AND origin='block'`（`crates/calm-truth/src/db/sqlite/task_projection.rs:610-614`），裁剪用的 `existing` 也只选 `origin='block'`（同文件 `:1010`）；而树库存 `WAVE_TREE_SPEC_INVENTORY_SQL` 数**所有** `declared_by='spec'` 非终结行（`crates/calm-truth/src/db/sqlite/wave_tree.rs:110`）。同一个树成员上，一条 legacy 非终结 spec 行既不占份额也不被裁剪，却计入 Σ。

**为什么仍判 MINOR**：`origin='legacy'` 只由迁移 `0068_projection_policy_columns.sql:3` 回填产生；本 build 无任何生产写入方会新造 legacy 行（`task_insert_tx` 在 `crates/calm-truth/src/db/sqlite/task.rs:49` 全仓**零生产调用方**，实测 grep），并且 `task_projection.rs:1109` 会在再投影时把匹配声明的 legacy 行收编为 `block`。因此对本 build 新建的数据 `Σ ≤ B` 不受影响；只有「升级库里带着 pre-0068 在飞 spec 行的 wave 后来加入树」这一路才可能超。

**最小修法**：`occupied` 与 `existing` 的过滤改成 `origin IN ('block','legacy')`，与树库存同源。

---

## 二、`Σ_v live_spec(v) ≤ B` 现在真的成立吗

**成立**（对本 build 生产写入方创建的数据）。依据是三条闭合的链：

1. **每个成员的上界**：`evaluate_schedulability` 的 `effective_ceiling = min(ceiling, share)`（`task_projection.rs:601`），`capacity = effective_ceiling - occupied`（`:615`），投影后 `live(v) = occupied(v) + admitted(v) ≤ share(v)`；非终结状态集恰好是 `pending ∪ {dispatched,running,verifying}`（`model.rs:361-369` vs `task_projection.rs:437`、`wave_tree.rs:110`），无第三类状态漏网。
2. **份额求和**：`Σ share = B` 由 `deterministic_share` 的余数前缀分配保证，`wave_tree::tests::shares_sum_to_the_budget_including_the_remainder` + `every_declaration_sequence_within_member_shares_respects_whole_tree_budget`（穷举 B∈[0,12]×N∈[1,12]）已锁。
3. **B/N 变化的两个触发点都过整树后置条件**：PATCH（`routes/waves.rs:1339`）与 child 创建（`child_wave_adapter.rs:272`）都走 `tasks_rebuild_tree_tx`，先按新份额裁 pending，再按成员逐一校验 `live ≤ share`，任一超出整事务回滚（实测 409 + 零事件 + 任务状态不变）。第三个能改 N 的路径只有删除，而删除受 `wave_require_leaf_tx`（`wave.rs:271`）约束只能删叶，N 只减 ⇒ Σshare 不变、各份额只增，上界自动保持。

唯一能让它不成立的构造是 MINOR-5 的 legacy 行，且需要一个升级库 + 一条 pre-0068 的在飞 spec 行落在树成员上。生产写入方无法造出这种行。

## 三、变异证据保质期抽查（≥3 条，全部重放）

| 旧条目 | 重放 | 结果 |
|---|---|---|
| R2-m2 | `task_projection.rs:894` `<=` → `<` | **RED** `an_equal_tree_share_reports_the_tree_knob`，仍属实 |
| R1-M4 | `wave_tree.rs` `over_deep` 恒 false | **RED** `an_over_deep_chain_fails_closed`，仍属实 |
| M9 | 关掉非树短路 | **RED** `a_non_tree_wave_runs_zero_recursive_tree_queries`，仍属实 |
| R3-SQL1/2 的等价升级版 | 往 `crates/neige-cli/src/main.rs`（旧硬编码清单**不含**该 crate）与 `crates/calm-truth/migrations/0072_*.sql` 各注入一条无界 CTE | **RED，两处分别报告** —— manifest 推导 + `.sql` 扫描均实测有效 |

## 四、可以合入了吗

**YES**。无 BLOCKER、无 MAJOR。MINOR-1（补生长路径 409 验收）与 MINOR-2（`wave_tree.rs:205` 一字符）建议本片顺手带上，其余可入 issue。

---

```
$ git status --short
?? docs/_985-s6b-impl-review-r5-subagent.md
```
