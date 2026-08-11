# #985 切片 6 PR-B —— 实现评审 r5（codex）

范围：`c71e4132` → `9f3fbed1`。分级：**BLOCKER 1 / MAJOR 1 / MINOR 1**。
Rust 均以题定 PATH、`CARGO_BUILD_JOBS=6`、未设置 `NEIGE_CODEX_BIN` 执行。所有临时变异后均执行
`git checkout -- .`。本 worktree 无 `web/node_modules`，web/vitest **未实际执行**，只做结构性复核
（实现方报告的 web 1232 passed、fe 758 passed 未在本 worktree 复跑）。

## BLOCKER

### B1 — 升级遗留的在飞 spec 行不占 share，普通报告写可使 `Σ live_spec > B`

- **结论**：树库存统计所有非终结 `declared_by='spec'` 行（`crates/calm-truth/src/db/sqlite/wave_tree.rs:105`），
  但投影容量只扣 `origin='block'` 的在飞行（`crates/calm-truth/src/db/sqlite/task_projection.rs:606`、
  `crates/calm-truth/src/db/sqlite/task_projection.rs:610`）；0068 又把既有行回填成
  `declared_by='spec', origin='legacy'`（`crates/calm-truth/migrations/0068_projection_policy_columns.sql:2`）。
- **触发条件**：升级后有 1 条不同 key 的 legacy running；用户把孤根 B 收到 1（该状态本身合法），
  再通过正常 report edit 声明一个新 block key。普通写只调用 `project_tasks_tx`
  （`crates/calm-server/src/wave_report.rs:824`），不会走整树后置复核（该复核仅在
  `crates/calm-server/src/wave_report.rs:218`）。容量被算成 1，新 pending 落库后总数为 2。
- **证据**：现有升级验收只测“同 key 被 block adoption”（`crates/calm-server/tests/cases/migration_0068_projection_policy.rs:104`、
  `crates/calm-server/tests/cases/migration_0068_projection_policy.rs:135`），没有“legacy 在飞 + 不同新 key”。所谓声明序列性质测试
  先假定每个成员 `live<=share` 再枚举（`crates/calm-truth/src/db/sqlite/wave_tree.rs:393`、
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:403`），正好把待证前提当输入。
- **实际验证**：临时加入
  `db::sqlite::wave_tree_budget_tests::mutant_legacy_inflight_must_consume_tree_share`，用生产
  `project_tasks_tx`（现有 helper 在 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:141`）；**RED 0/1**：
  `tree budget B=1 was exceeded by 2 live spec rows`。复原后 `cargo test -p calm-truth --lib wave_tree` **PASS 31/31**。
- **最小修法**：保留 per-wave ceiling 的 block-only occupancy，但另算“所有 spec in-flight”的 tree occupancy，
  `capacity=min(ceiling-block_occupied, share-tree_occupied)`；补永久升级用例，断言不同 key 也不得使库存越 B
  （两类 occupancy 的现有汇合点：`crates/calm-truth/src/db/sqlite/task_projection.rs:590`、`:615`）。

## MAJOR

### M1 — “合取项”词法门仍接受语义上恒真的伪上界

- **结论**：新逻辑只记录同/外层括号是否出现 `OR`（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:318`、`:349`），
  随后见到 `alias.depth <= 非递归 RHS` 就返回 true（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:366`、`:382`）。
  因而 `CASE WHEN depth<=?2 THEN 1 ELSE 1 END` 与 `depth<=?2 IS FALSE` 都能放行无限递归。
- **触发条件**：未来任一 workspace SQL 用上述合法 SQLite 谓词包装比较；当前生产宏仍是直接 bound，
  尚未被击穿（`crates/calm-truth/src/db/sqlite/wave_tree.rs:57`、`:74`）。
- **证据**：判据不解析 `CASE/THEN/ELSE/END`，也不验证比较表达式后的 postfix token；CTE 最终只消费该布尔值
  （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:354`、`:407`）。
- **实际验证**：临时测试 `mutant_case_when_true_branch_cannot_fake_a_bound` 与
  `mutant_boolean_postfix_cannot_invert_a_bound` 均 **RED 0/1**（门返回空 violation）；复原态
  `cargo test -p calm-truth --test bounded_wave_tree_sql` **PASS 15/15**。
- **最小修法**：将允许语法收窄为完整 conjunct AST：比较节点本身必须是合取叶，外面只准括号，禁止
  `CASE/IS/=/函数` 再包装；或用 SQL parser 证明每条布尔路径都有有限上界。对应入口在
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:323`、`:354`。

## MINOR

### m1 — crate 集合是“按当前排版猜 manifest”，cargo 合法改排版会让门空跑

- **结论**：`workspace_member_roots` 手写逐行找 `members=[`，只在后续独占 `]` 行停止，且只断言
  解析结果非空（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:482`、`:489`、`:503`），不是真正解析 manifest。
- **触发条件**：把当前合法多行 members（`Cargo.toml:3`）格式化成同一行；解析器把后续 TOML 行当不存在的路径，
  `production_sources_below` 静默跳过（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:16`）。
- **证据**：Rust 只扫派生 root 的 `src`，`.sql` 扫整个派生 root（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:511`、`:515`）；
  当前 14 个 member、测试专用 `calm-truth-test-harness/src` 和 crate 内 `include_str!` 的 `.sql` 均覆盖，
  但没有 build.rs；仓库当前也无 build.rs。该问题是发现机制的未来静默失效，不是今天漏掉现存生产 SQL。
- **实际验证**：临时把 `Cargo.toml:3` 的 members 改成 cargo 合法单行后，
  `every_recursive_parent_wave_cte_in_workspace_members_bounds_its_recursive_variable` **STILL-GREEN 1/1（0.00s）**。
- **最小修法**：用 `cargo metadata --no-deps` 或 TOML parser 取 workspace members，并断言每个 root 的
  `Cargo.toml` 存在、扫描文件数非零（替换 `crates/calm-truth/tests/bounded_wave_tree_sql.rs:482`）。

## 修订轮 4 的五处，哪几处修出了新洞

1. **承重验收修出/暴露 B1**：它是 B=8、B=12 两个固定脚本，不是生成式性质测试
   （`crates/calm-server/src/operation/child_wave_adapter.rs:1054`、`:1090`）；临时删 child 重投影时目标
   **RED `(9,15)!=(8,12)`**，但 legacy 维度不在脚本内（`:1120`）。
2. **两个触发点未见第三个上界洞**：B PATCH 在 `crates/calm-server/src/routes/waves.rs:1331`、`:1340`；
   N 增加在 child 写 parent 后重投影（`crates/calm-server/src/operation/child_wave_adapter.rs:202`、`:272`）。
   leaf 删除同时删该 wave tasks，N 减少只扩大剩余 share（`crates/calm-truth/src/db/sqlite/wave.rs:264`、`:307`）；
   cove 删除整批 waves/tasks（`crates/calm-truth/src/db/sqlite/cove.rs:148`、`:168`）。无生产 unlink/move 路径。
3. **O(N) 未见新洞**：成员、B 各读一次后按同一 `(created_at,id)` 下标预计算（`crates/calm-server/src/wave_report.rs:181`、`:196`、`:203`），
   同一 IMMEDIATE tx 内逐成员写并后验（`crates/calm-truth/src/db/sqlite/events.rs:345`、`:348`、
   `crates/calm-server/src/wave_report.rs:218`）。退回逐成员 tree term 的变异使承重测试 **RED**：
   `tree_cte_queries=6, expected 2`（计数断言 `crates/calm-server/src/wave_report.rs:234`）。
4. **SQL 合取修出 M1**；嵌套 `OR` 已拒，但 `CASE`/postfix 绕过。误红也扩大：两支都有限的
   `depth<=?2 OR depth<=?3` 会被统一拒绝（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:336`），属可接受 fail-closed。
5. **manifest 推导修出 m1**；web action→label 已补真实 render 三分支断言
   （`web/src/pages/report-blocks/report-blocks.test.tsx:920`、`:944`），结构上与消费分支
   `web/src/pages/report-blocks/task.tsx:17` 对齐；**web 未实际执行**。

## `Σ_v live_spec(v) ≤ B` 现在真的成立吗

**NO（升级遗留行存在时）**，见 B1；对全新、全部 `origin='block'` 的生产状态转移，B/N 两次重投影、
逐成员 share 与后置总量检查是闭合的（`crates/calm-server/src/wave_report.rs:169`、`:218`、`:229`）。
承重测试只能证明两组 fresh-block 构造，代数枚举又以前提 `live<=share` 开始
（`crates/calm-server/src/operation/child_wave_adapter.rs:1059`、`crates/calm-truth/src/db/sqlite/wave_tree.rs:394`）。

## 旧变异证据保质期抽查（本轮实跑）

- R1-M6：删 `projection_policy_changed` 的 tree 项，
  `tightening_tree_budget_immediately_deletes_pending_projection_and_emits_plan_updated` **RED 0/1，1!=0**
  （断言 `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:451`）。
- R3-B2：强制 `tree_budget_changed=false`，
  `tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed` **RED 0/1，后代仍 1**
  （`crates/calm-server/tests/cases/wave_projection_policy_patch.rs:488`、`:495`）。
- R2-M1a：库存 `>=` 改 `>`，`acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full`
  **RED 0/1**；新后验改报 member over-share，理由断言仍抓到接线退化
  （`crates/calm-server/src/operation/child_wave_adapter.rs:930`、`:961`）。三次均已复原。

基线另实跑：承重生产测试 **PASS 1/1**（`crates/calm-server/src/operation/child_wave_adapter.rs:1059`）；
预算收紧原子拒绝 **PASS 1/1**（`crates/calm-server/tests/cases/wave_projection_policy_patch.rs:512`）。

## 可以合入了吗

**NO**。B1 直接证伪本片核心不变量，M1 仍允许无终止保证的 SQL 通过安全门；先修二者。
判断依据：`crates/calm-truth/src/db/sqlite/task_projection.rs:610`、
`crates/calm-truth/tests/bounded_wave_tree_sql.rs:366`。m1 可后续修，不单独阻塞。

## git status --short

```text
?? docs/_985-s6b-impl-review-r5-codex.md
```
