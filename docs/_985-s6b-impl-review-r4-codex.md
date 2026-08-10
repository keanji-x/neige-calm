# #985 切片 6 PR-B 实现评审 r4（codex）

范围：`c71e4132..3635a8c1`。环境按要求设置 `PATH`、`CARGO_BUILD_JOBS=6`，并用
`env -u NEIGE_CODEX_BIN` 保证未设置。web 缺 `web/node_modules`，以下 web 结论均为结构性复核，
**未实际执行** vitest。

## BLOCKER

### B1 — SQL 性质门把“出现在 WHERE/ON”误当成“逻辑上必然约束”

- 结论：新门仍可被 `WHERE down.depth <= ?2 OR 1=1` 绕过；生产递归在环上不再有上界，门却绿。
  生产 SQL 的终止性明确只靠该谓词（`crates/calm-truth/src/db/sqlite/wave_tree.rs:63`、
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:74`）。
- 触发条件：上界比较位于一个恒真的析取项中；同形还有 `... OR guard=guard`。
- 证据：判据找到一次比较便立即 `return true`（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:318`、
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:346`），且把 `or` 只当 RHS 截断符
  （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:333`、
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:338`），没有验证比较处于必经合取路径。
  crate-wide 门完全依赖这个判据（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:443`、
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:470`）。
- 实际验证：临时把生产谓词改成 `WHERE down.depth <= ?2 OR 1=1`；
  `every_recursive_parent_wave_cte_in_production_crates_bounds_its_recursive_variable` **STILL-GREEN 1/1**。
  未跑环行为测试，避免执行已知无界递归；随后已 `git checkout -- .`。
- 最小修法：解析 ON/WHERE 的布尔结构，只接受不处于任一 `OR` 可绕过分支的上界；至少新增
  `bound OR true`、`true OR bound` 两个反例，且用同一生产变异要求性质门 RED。

## MAJOR

### M1 — 整树重投影在合法大树上为 O(N²) 的长写事务

- 结论：范围与预算影响范围一致、失败也原子，但实现对每个成员重新执行一次全树成员查询；
  `tree_task_budget` 仅校验非负、无上限（`crates/calm-server/src/routes/waves.rs:1285`、
  `crates/calm-server/src/routes/waves.rs:1290`），所以合法 N 不受固定 32 约束。
- 触发条件：用户先设很大的 B，并逐步形成大分支树，再 PATCH 根 B；成员准入只要求
  `N+1 <= B`（`crates/calm-server/src/operation/child_wave_adapter.rs:167`、
  `crates/calm-server/src/operation/child_wave_adapter.rs:173`）。
- 证据：route 先枚举 N 个成员，再逐个调用 `tasks_rebuild_tx`
  （`crates/calm-server/src/routes/waves.rs:1339`、`crates/calm-server/src/routes/waves.rs:1355`）；
  每次 rebuild 又经 `evaluate_schedulability` 调 `wave_tree_term`
  （`crates/calm-truth/src/db/sqlite/task_projection.rs:939`、
  `crates/calm-truth/src/db/sqlite/task_projection.rs:566`），树上成员再全量执行
  `WAVE_TREE_MEMBERS_SQL`（`crates/calm-truth/src/db/sqlite/wave_tree.rs:235`、
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:239`）。整个过程持有 `BEGIN IMMEDIATE`
  （`crates/calm-truth/src/db/sqlite/events.rs:345`、`crates/calm-truth/src/db/sqlite/events.rs:416`）。
- 实际验证：`wave_projection_policy_patch::tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed`
  **PASS 1/1**，但夹具只有根+子两成员（`crates/calm-server/tests/cases/wave_projection_policy_patch.rs:451`、
  `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:473`），没有规模门；结构复核确认 N 次全树查询。
- 最小修法：在一次成员枚举中计算每个成员的 share，并向批量 rebuild 传入预计算树项；或给 B/N
  设受契约约束的硬上限。保留单事务：closure 任一步失败会 rollback
  （`crates/calm-truth/src/db/sqlite/events.rs:348`、`crates/calm-truth/src/db/sqlite/events.rs:352`）。

## MINOR

### m1 — web 的 recovery-action 消费映射仍是未受跨文件门约束的第二份字面量

- 结论：Rust 改 action 值会被 web 硬编码期望抓住，但 web 单独把
  `raise_tree_task_budget` 从 `RelatedBlocks` 识别式删掉/拼错，跨文件测试仍绿；UI 会从
  “Review capacity” 退化为 “Open related item”。重复映射在
  `web/src/pages/report-blocks/task.tsx:17`、`web/src/pages/report-blocks/task.tsx:19`。
- 触发条件：只改 web 的 action→label 分支，不改诊断 copy。
- 证据：Rust 单一表位于 `crates/calm-types/src/report_blocks/tasks.rs:69`；web 测试只解析该表、
  调 `taskDiagnosticText` 并检查文案（`web/src/pages/report-blocks/report-blocks.test.tsx:892`、
  `web/src/pages/report-blocks/report-blocks.test.tsx:919`），没有 render `RelatedBlocks`。
- 实际验证：**未实际执行**（无 `web/node_modules`）；逐语句结构性复核。
- 最小修法：新增 `ReportTaskBlock` 渲染断言，分别用两个 capacity action 断言
  `Review capacity`，并用非 capacity action 断言 `Open related item`。

### m2 — “其余旧条目不经过本轮路径”的保质期声明漏了 R1-M6

- 结论：R1-M6 正好改动本轮重写的 `projection_policy_changed`/重投影路径，却未列入刷新表；
  声明“不经过本轮改动路径”不准确（`docs/_985-s6b-mutation-map.md:168`、
  `docs/_985-s6b-mutation-map.md:174`）。
- 触发条件：按文档审计哪些旧证据因 route 重写需要重跑。
- 证据：旧 R1-M6 登记在 `docs/_985-s6b-mutation-map.md:96`；当前决策点在
  `crates/calm-server/src/routes/waves.rs:1328`、`crates/calm-server/src/routes/waves.rs:1338`。
- 实际验证：删除 `projection_policy_changed` 中的 `tree_task_budget` 后，
  `wave_projection_policy_patch::tightening_tree_budget_immediately_deletes_pending_projection_and_emits_plan_updated`
  **RED 0/1**（pending `1 != 0`，断言在
  `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:431`、
  `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:438`）；随后已复原。
- 最小修法：把 R1-M6 加入 §7.1；结果仍红，属于证据清单修正，不是生产正确性缺口。

## 修订轮 3 的五处，哪几处修出了新洞

1. 统一解析函数：**未见新洞**。ceiling 的 projector/shortcut 与 B 都调用 `effective_limit`
   （`crates/calm-truth/src/db/sqlite/task_projection.rs:465`、
   `crates/calm-truth/src/db/sqlite/wave_tree.rs:185`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:281`）；
   生产搜索未发现第三个裸读 enforcement reader。两者是独立默认常量
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:30`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:33`），
   但比较用各自有效值，不要求默认相同。
2. 整树重投影：**正确性未见新洞，性能修出 M1**。只有 B PATCH 扩到整树；子 wave ceiling
   只重建自己，不改变 shape/share（`crates/calm-server/src/routes/waves.rs:1328`、
   `crates/calm-server/src/routes/waves.rs:1348`）。写入、投影、事件同事务，失败回滚
   （`crates/calm-truth/src/db/sqlite/events.rs:346`、`crates/calm-truth/src/db/sqlite/events.rs:416`）。
3. SQL 性质门：**修出 B1**。反序比较、无关 CTE、分离字面量正例已常驻
   （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:590`、
   `crates/calm-truth/tests/bounded_wave_tree_sql.rs:618`、
   `crates/calm-truth/tests/bounded_wave_tree_sql.rs:630`），但不理解析取语义。
4. 单一诊断契约：Rust 生产者由表驱动且 constructor 校验
   （`crates/calm-types/src/report_blocks/tasks.rs:75`、`crates/calm-types/src/report_blocks/tasks.rs:179`）；
   copy 方向成立，web action 消费反向留下 m1。
5. 过期条目重跑：抽查结果属实，但刷新范围漏了仍然有效的 R1-M6，见 m2。

## 旧变异条目抽查结果

- M9：强制默认孤根走树路径后，
  `db::sqlite::wave_tree_budget_tests::a_non_tree_wave_runs_zero_recursive_tree_queries` **RED 0/1**，
  得到 `Share` 而非 `NotInTree`（断言：`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:765`）。
- R2-B1：singleton 恒 `NotInTree` 后，
  `db::sqlite::wave_tree_budget_tests::resetting_an_explicit_budget_to_null_keeps_the_default_bound`
  **RED 0/1**，40 条通过而非 32（断言：`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:895`）。
- R3-B2：强制 `tree_budget_changed=false` 后，
  `wave_projection_policy_patch::tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed`
  **RED 0/1**，子 pending 保持 1（断言：
  `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:477`、
  `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:482`）。
- 未想到受影响的旧条目：R1-M6，实测仍 RED，见 m2。以上每个变异后均执行 `git checkout -- .`。

## 基线

- `bounded_wave_tree_sql`：**PASS 13/13**；`db::sqlite::wave_tree_budget_tests`：**PASS 21/21**；
  `domain_api_suite::wave_projection_policy_patch`：**PASS 7/7**。测试入口分别在
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:443`、
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1`、
  `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:155`。

## 可以合入了吗

**NO**。B1 允许无终止保证的递归 SQL 通过本轮声称的性质门；先补布尔谓词判定与对应生产变异门。
M1 应在合入前至少用批量预计算或明确上限收敛。结论依据分别在
`crates/calm-truth/tests/bounded_wave_tree_sql.rs:318` 与 `crates/calm-server/src/routes/waves.rs:1352`。

## git status --short

```text
?? docs/_985-s6b-impl-review-r4-codex.md
```
