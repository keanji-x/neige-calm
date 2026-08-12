# #985 切片 6 PR-B —— 实现评审 r3（codex）

对象：`c71e4132..0f8e30ab`。环境：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH`、
`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。复原态定向门：calm-truth 20/20 + 4/4、
calm-server 8/8、web 56/56；测试入口分别见 `docs/_985-s6b-mutation-map.md:8-10`。

## BLOCKER

### B1. `spec_task_ceiling=NULL` 被短路解成 0，孤根显式小预算可再次绕过

- **结论**：PATCH `spec_task_ceiling:null` 的契约是恢复默认 32，但 shortcut 把原始 NULL 直接解到
  `i64`，SQLite/sqlx 此处得到 0；于是任何 `B>=0` 都被判成 `NotInTree`，后续又使用正确的 ceiling=32，
  树项被完全跳过（`crates/calm-truth/src/model.rs:170-173`；
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:160-168,176-190`；
  `crates/calm-truth/src/db/sqlite/task_projection.rs:466-469,570-583`）。
- **触发条件**：孤根 PATCH `spec_task_ceiling=null, tree_task_budget=1`，再投影 3 条声明；两字段都是合法
  人写面，writer 会把 ceiling 真写成 NULL（`crates/calm-server/src/routes/waves.rs:1274-1289`；
  `crates/calm-truth/src/db/sqlite/wave.rs:208-213,227-245`）。
- **实际验证**：临时生产链测试
  `db::sqlite::wave_tree_budget_tests::r3_probe_null_ceiling_uses_kernel_default_in_shortcut`
  → **FAILED**：预期只准 1 条，实际 3/3 schedulable；复原后现有 tree 套件 20/20（现有 reset 只覆盖
  `tree_task_budget=NULL`，fixture ceiling=40，见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:807-865`）。
- **最小修法**：shortcut 用与 `wave_projection_state` 同一个 effective-ceiling 函数/SQL `COALESCE`
  读取 32，禁止再解原始列；补 `ceiling NULL + B=1` 验收（
  `crates/calm-truth/src/db/sqlite/task_projection.rs:464-469`）。

### B2. 根预算 PATCH 只重投影根，子 wave 的 pending 会在 B=0 后继续可 claim

- **结论**：`tree_task_budget` 是整树输入，但 PATCH 事务只调用 `tasks_rebuild_tx(tx, &id)`；“其余树等
  下次投影”会留下子 wave pending，而设计只允许既有 **in-flight** 退化并明确要求裁掉超额 pending
  （`crates/calm-server/src/routes/waves.rs:1323-1341`；`docs/architecture/985-doc-as-plan.md:2034`）。
- **触发条件**：root+child，child 已有 pending，人在 root PATCH B=0；child 没有后续文档编辑也没有必然的
  “下次投影”（`crates/calm-server/src/routes/waves.rs:1325-1327`）。claim 事务只复核 lifecycle、
  `task_budget`、依赖和 per-wave in-flight，不读 `tree_task_budget`/share（
  `crates/calm-server/src/scheduler/mod.rs:1164-1174,1237-1286`）。
- **实际验证**：临时路由测试
  `wave_projection_policy_patch::r3_probe_root_budget_patch_culls_descendant_pending_rows`
  → **FAILED**：PATCH 200 后 child pending 仍为 1，预期 0；复原态
  `wave_projection_policy_patch::tightening_tree_budget_immediately_deletes_pending_projection_and_emits_plan_updated`
  → **PASS**，但它只有 N=1（`crates/calm-server/tests/cases/wave_projection_policy_patch.rs:343-394`）。
- **最小修法**：root budget PATCH 的同一 IMMEDIATE 事务中枚举 bounded members 并逐 wave rebuild、按 wave
  发 `PlanUpdated`；另在 claim fence 重读 tree term，封住 PATCH 与 claim 的并发窗（
  `crates/calm-server/src/routes/waves.rs:1333-1373`；`crates/calm-server/src/scheduler/mod.rs:1237-1286`）。

## MAJOR

### M1. B2 性质门只验证“某处出现 depth<”，不验证它截断递归变量

- **结论**：门只在一个 `WITH RECURSIVE` token 段里寻找任意 `WHERE` 后 15 token 内的
  `depth <|<=`；它不校验 alias、递归项或 depth 递增关系。因此常量 `guard.depth<=?2` 能给无界递归
  发通行证（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:156-189`）。
- **覆盖边界**：只扫 calm-truth 的 `src/**/*.rs`，不扫别的 crate、`.sql` 或 tests；只解源码
  `LitStr`，没有宏展开，所以 `include_str!`、`query_file!`、`stringify!`、运行时 `format!/push_str`
  都可不可见；反向还会把同一 group/file 的无关 literal 拼接，合法非递归 `WITH RECURSIVE` 也可误红
  （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:14-49,198-208`）。
- **实际验证**：把 descendant 递归步改成有效 SQL
  `CROSS JOIN (SELECT 0 AS depth) guard WHERE guard.depth<=?2`，目标
  `every_recursive_parent_wave_cte_in_the_crate_has_a_depth_bound` → **STILL-GREEN 1/1**；该谓词恒真，
  2-cycle 会无限生成（真实终止点原在 `crates/calm-truth/src/db/sqlite/wave_tree.rs:52-63`）。
- **最小修法**：用 SQLite AST/专用 query builder 验证递归 SELECT 的谓词约束“递归 CTE alias.depth”，
  并扫描 workspace 生产 `.rs/.sql`、展开 `include_str!`；至少补恒真 depth、外层 depth、`stringify!`、
  `include_str!` 和合法非递归正例（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:220-274`）。

## MINOR

### m1. 变异映射仍有一处引用已删除的登记门

- **结论/触发**：表头已说 M3/旧 R1-B3 退休，却仍称“非 id 列”由
  `every_bounded_tree_cte_expansion_is_registered` 共同把守，会误导后续保质期审计
  （`docs/_985-s6b-mutation-map.md:18,56-58,86`）。
- **实际验证**：`cargo test -p calm-truth --lib every_bounded_tree_cte_expansion_is_registered`
  → **PASS, running 0 tests**；当前实际门名在 `docs/_985-s6b-mutation-map.md:17`。
- **最小修法**：删掉 `:56-58` 的旧测试名，改写成当前性质门及其已知边界
  （`docs/_985-s6b-mutation-map.md:110-111`）。

## 修订轮 2 的五处，哪几处修出了新洞

1. **B2 性质检查：有新洞**，见 MAJOR-M1；范围/宏/运行时/误红边界由扫描器本身确定
   （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:14-49,156-208`）。
2. **B1 shortcut：有新洞**，即 ceiling NULL 的 BLOCKER-B1；两个强制点的 B 确实同调
   `wave_tree_budget`（`crates/calm-truth/src/db/sqlite/wave_tree.rs:180,225,256-265`；
   `crates/calm-server/src/operation/child_wave_adapter.rs:159`）。PATCH ceiling 会在目标 wave 同 tx rebuild，
   但 root budget 的跨成员 pending 另成 BLOCKER-B2（`crates/calm-server/src/routes/waves.rs:1328-1341`）。
3. **M1 解耦：未见新洞**。库存 B=2/N=1/inventory=2 与成员 B=2/N=2/inventory=1 各自唯一触发，
   且允许性双向等价已补（`crates/calm-server/src/operation/child_wave_adapter.rs:769-890`；
   `crates/calm-truth/src/db/sqlite/wave_tree.rs:347-357`）。
4. **M2 剥注释：未形成新洞**。结构 stripper 不识别 SQL 字符串，假 ORDER BY literal 变异使结构测试
   **STILL-GREEN**，但固定反序行为测试 **RED**；同 created_at 时固定 id 次序兜底
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:292-317`；
   `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:318-373`）。
5. **诊断跨源：未见新洞**。web 正则读取 Rust action registry并钉 tree/spec 两个旋钮；只改 Rust action
   或只改 web “top wave”均 **RED**。边界是动作契约，不是逐字 copy 相等
   （`web/src/pages/report-blocks/report-blocks.test.tsx:892-915`；
   `crates/calm-types/src/report_blocks/tasks.rs:65-79,261-289`）。

## 旧变异条目里，哪几条已经失真

- **已失真且当前文档已标记**：M3 与 R1-B3 的登记测试已退休；R1-B2-pre 的“行为仍绿”结论已过期
  （`docs/_985-s6b-mutation-map.md:18,86,102`）。本轮假 ORDER BY literal 复核得到结构绿、行为红，
  再次证明旧 R1-B2-pre 已不能复用（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:318-347`）。
- **抽查未失真 1**：M7 `inventory >=`→`>`；
  `operation::child_wave_adapter::tests::acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full`
  → **RED**（`docs/_985-s6b-mutation-map.md:23`）。
- **抽查未失真 2**：R1-B1a `can_add_tree_member=true`；
  `db::sqlite::wave_tree::tests::enforcement_points_are_compatible_for_every_budget_and_member_count`
  → **RED**（`docs/_985-s6b-mutation-map.md:82`）。
- **抽查未失真 3**：R1-B1c singleton 恒 `NotInTree`；
  `db::sqlite::wave_tree_budget_tests::an_explicit_budget_applies_to_a_singleton_root`
  → **RED**（`docs/_985-s6b-mutation-map.md:84`）。

## 可以合入了吗

**NO。** 两个 BLOCKER 都能在合法 PATCH 后让新的 spec 工作越过当前树预算；性质门另有确定性
STILL-GREEN 绕法（`crates/calm-truth/src/db/sqlite/wave_tree.rs:160-190`；
`crates/calm-server/src/routes/waves.rs:1323-1341`；
`crates/calm-truth/tests/bounded_wave_tree_sql.rs:156-189`）。

```text
$ git status --short
?? docs/_985-s6b-impl-review-r3-codex.md
```
