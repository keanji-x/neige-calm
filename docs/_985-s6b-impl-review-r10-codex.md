# #985 切片 6 PR-B 实现评审 r10（codex）

评审范围：`c71e4132..7471f802`；修复轮 10 的实现落点是固定 oracle 与新增 tie-break 验收，分别见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1394`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1590`。

## 分级结论

- **BLOCKER：无。** 两条成员查询采用同一持久化 `(created_at,id)` 全序，份额计算仍精确满足 `Σ share=B`，freeze minimum 逐候选检查全部成员；未发现可达错值未登记或 r10 新回归（`crates/calm-truth/src/db/sqlite/wave_tree.rs:94`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:104`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:128`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:258`）。
- **MAJOR：无。** D.1 #11 的精确定义比较同一份文档的增量投影与 rebuild，并不比较两次等价创建所得的不同 UUID 数据库；UUID 抽签不击穿该已声明性质（`docs/architecture/985-doc-as-plan.md:1347`、`docs/architecture/985-doc-as-plan.md:1998`、`docs/_985-s6-design.md:802`）。
- **MINOR（已登记，不阻断）：同毫秒余数抽签。** 该缺口已如实列入 §12.1 #24，且同时写出容量与恢复 minimum 的外显差异，没有夸大成 rebuild 不稳定，也没有淡化成纯测试问题（`docs/architecture/985-doc-as-plan.md:1630`、`docs/_985-s6-design.md:806`）。

### MINOR：同毫秒创建序列不可预测

- **结论：** 这是公平性/可预测性缺口，不是持久状态确定性或份额守恒缺口，可作为已登记缺口合入（`docs/_985-s6-design.md:806`、`docs/architecture/985-doc-as-plan.md:1630`）。
- **触发条件：** `N=2`、非整除预算、兄弟 `created_at` 相同；UUIDv4 较小者先拿余数，故 root 固定占用 5、child 固定占用 3 时可分别产生 minimum 9 或 10（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1590`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1612`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:131`）。
- **证据：** 生产 SQL 明确升序 `(created_at,id)`，且 minimum 对该顺序下所有固定占用求首个可行 B（`crates/calm-truth/src/db/sqlite/wave_tree.rs:109`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:264`）。
- **实际验证：** 基线用例精确得到 9/10；将固定占用 SQL 临时改为 `ORDER BY w.created_at,w.id DESC` 后，新用例在精确 10 断言处 RED，实际文案为 `at least 9`（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1526`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1645`）。
- **最小修法：** 本 PR 无需修；未来若购买创建序列可预测性，应新增持久、不可变、单调 `quota_order` 并在创建事务内唯一分配（旧行按现有全序回填）。只提高时钟精度仍可能 tie，现有登记方向准确（`docs/_985-s6-design.md:811`、`docs/architecture/985-doc-as-plan.md:1630`）。

## 同毫秒抽签的裁决是否站得住

**同意。** `(created_at,id)` 对同一批持久行给出相同成员 index；`deterministic_share` 只依赖 B/N/index，且不读取 pending 等投影产物，所以 rebuild 顺序不改变 share 或投影结果（`crates/calm-truth/src/db/sqlite/wave_tree.rs:94`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:128`、`crates/calm-truth/src/db/sqlite/task_projection.rs:598`）。

相反，“哪个等价创建序列拿到余数”跨越的是不同 UUID 持久状态；它确实改变单 wave 容量与可执行恢复 minimum，但未改变 `Σ share=B`，也未破坏“同一份文档”的 D.1 #11（`docs/_985-s6-design.md:798`、`docs/architecture/985-doc-as-plan.md:1347`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:131`）。没有发现其它已声称性质被抽签破坏（`docs/architecture/985-doc-as-plan.md:1209`、`docs/architecture/985-doc-as-plan.md:1216`）。

## 两条 oracle 与 R10-B3

- 原 flaky 用例固定 root=1、child=2，仍精确断言 minimum=9，并实际用该值恢复准入；不是 `{9,10}` 放宽（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1394`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1526`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1533`）。
- 新用例强制同时间且 child id 在前，精确断言 minimum=10，并实际设置 10 后断言 schedulable（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1599`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1605`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1645`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1653`）。
- R10-B3 独立复核成立：DESC 变异 0/1 RED，minimum 实际变 9；随后已在临时 worktree 执行 `git checkout -- .`，复原态重新由升序 SQL承重（`crates/calm-truth/src/db/sqlite/wave_tree.rs:118`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1645`）。

## 实际跑过的验证

- 指定环境且 `NEIGE_CODEX_BIN` 未设置：两条目标测试 **2/2 PASS**；随后 `cargo nextest run -p calm-truth wave_tree_budget_tests` **27/27 PASS**（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1388`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1594`）。
- `cargo fmt --all --check`、`git diff --check` 均通过；DESC 变异验证后复原态无生产 diff（`crates/calm-truth/src/db/sqlite/wave_tree.rs:118`）。
- Web 仅结构性复核，**未实际执行**（worktree 无 `web/node_modules`）：生成字段、恢复文案及 Rust action 对齐测试结构一致（`web/src/api/generated.ts:2252`、`web/src/pages/report-blocks/task.tsx:99`、`web/src/pages/report-blocks/report-blocks.test.tsx:906`）。

## 可以合入了吗

**YES。** 无 BLOCKER / MAJOR；唯一剩余项是已准确登记的 MINOR，且 r10 精确修复了 flaky oracle、没有在修复点引入新洞（`docs/architecture/985-doc-as-plan.md:1630`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1394`、`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1645`）。

## `git status --short`

```text
 D docs/_985-s6b-impl-review-r9-subagent.md
?? docs/_985-s6b-impl-review-r10-codex.md
```
