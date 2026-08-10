# #985 切片 6 PR-A 实现评审 r4（codex）

对象：`9d30006a..f12767f8`。结论：**YES**。未发现 BLOCKER / MAJOR / MINOR；降范围没有
删掉第二个既有契约，删除入口和三个 child-flip 复核点均收敛。命令均显式
`env -u NEIGE_CODEX_BIN`，`CARGO_BUILD_JOBS=6`，PATH 含指定 `.local-bin`。

## BLOCKER

无。删除的 marker 状态机明确不再是本片契约；当前 leaf 正确性边界仍在最终写事务内
（`crates/calm-truth/src/db/sqlite/wave.rs:217`、`:222`、`:229`）。

## MAJOR

无。route、raw Repo 与 cove 三个入口分别由前检+最终 guard、最终 guard、同 cove cascade
承载（`crates/calm-server/src/routes/waves.rs:1510`、`:1525`；
`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:316`、`:321`；
`crates/calm-truth/src/db/sqlite/cove.rs:147`、`:182`）。

## MINOR

无。断言有效性变异及其互补 oracle 结果见后文（
`crates/calm-server/tests/cases/terminal_lifecycle.rs:434`、`:447`；
`crates/calm-server/tests/scheduler.rs:6013`、`:6202`）。

## 降范围有没有删出洞

**没有发现第二例。** 我逐项检查了以下购买关系：

1. **marker/恢复状态机本身**：durable intent、崩溃续删、marker 后禁止资源复活、operation
   external-phase fence、scheduler 把 marked wave 视作不存在，均明确整体撤出而非换载体；当前
   scheduler 只读正常 wave lifecycle，sweeper 只做 orphan terminal sweep（
   `docs/_985-s6-impl-notes.md:119`、`:125`；
   `crates/calm-truth/src/db/sqlite/task.rs:203`、`:210`；
   `crates/calm-server/src/terminal_sweeper.rs:117`、`:120`）。这些不是降范围后仍声称的性质。
2. **marker phase-1 顺带的 wave existence + leaf 检查**：route 先 `wave_get` 保留 404，再普通读
   direct child 保留快失败；最终 `wave_require_leaf_tx` 才是所有 wave-delete 的正确性边界，
   `DELETE` 的零行结果仍给 typed NotFound（`crates/calm-server/src/routes/waves.rs:1490`、`:1514`；
   `crates/calm-truth/src/db/sqlite/wave.rs:229`、`:236`、`:280`、`:284`）。
3. **card guard 顺带的 owner-wave typed NotFound**：已由无 marker 语义的纯 existence check 接回，
   且发生在 sort/insert 前（`crates/calm-truth/src/db/sqlite/card.rs:83`、`:86`、`:90`、`:93`）。
   恢复态 `card_with_codex_create_tx_rolls_back_on_invalid_wave`、
   `card_with_terminal_create_tx_rolls_back_on_invalid_wave` 均 PASS。
4. **terminal owner/唯一性**：事务内与 out-of-domain 两个写口仍先给 missing-card NotFound，再查
   per-card duplicate（`crates/calm-truth/src/db/sqlite/card.rs:577`、`:582`、`:591`；
   `crates/calm-truth/src/db/sqlite/out_of_domain.rs:83`、`:85`、`:94`）。
5. **child parent existence/同 cove/depth**：删除 deleting guard 后，生产 adapter 仍在同一事务里
   做 root/depth 与 parent row 查询，child 复制 parent cove 后才写 parent edge（
   `crates/calm-server/src/operation/child_wave_adapter.rs:146`、`:158`、`:163`、`:168`、`:172`、`:187`）。
6. **session/lease owner结构**：删除 marker admission 不会删除原有 owner FK；session 是 required
   `waves` FK，lease 是 `waves` FK + cascade（`crates/calm-truth/migrations/0045_worker_sessions.sql:1`、`:3`；
   `crates/calm-truth/migrations/0056_workspace_leases.sql:7`、`:10`）。marker 后禁止 attach 则按裁决
   不再承诺（`docs/_985-s6-impl-notes.md:125`、`:126`）。
7. **拒删全不变**：route 的 direct-child 前检发生在 snapshot/turn/process/socket/harness teardown
   之前（`crates/calm-server/src/routes/waves.rs:1510`、`:1525`、`:1526`）。恢复态
   `acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal` PASS，oracle 同时守
   process/socket/registry/四表（`crates/calm-server/tests/spec_card_reset.rs:1727`、`:1731`、`:1735`、
   `:1737`、`:1762`、`:1769`）。

## 降范围后的删除路径

- **route**：前检只负责体验；它与最终事务间新建 child 时，外部 teardown 已执行、最终 guard
  返回 409。代码注释与登记都直说了这个交错，没有把前检伪装成 correctness fence（
  `crates/calm-server/src/routes/waves.rs:1510`、`:1512`；`docs/_985-s6-impl-notes.md:130`、`:131`）。
  `acceptance_20_wave_delete_route_refuses_descendant_and_names_child` PASS（
  `crates/calm-server/tests/cases/cards_deletable.rs:531`、`:568`、`:571`）。
- **`Repo::wave_delete`**：overlay 清理与 authoritative leaf guard 在同一 IMMEDIATE 事务中；冲突
  回滚且错误指名 child（`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:316`、`:318`、`:321`；
  `crates/calm-truth/src/db/sqlite/wave.rs:229`、`:236`）。
  `acceptance_20_repo_wave_delete_refuses_descendant_and_names_it` PASS（
  `crates/calm-truth/src/db/sqlite/sub_wave_tree_tests.rs:69`、`:80`、`:81`）。
- **cove 删除**：刻意不走 leaf guard；先清每个 wave 的无 FK 资源，再以单条 cove delete cascade
  整棵同 cove 树（`crates/calm-truth/src/db/sqlite/cove.rs:148`、`:161`、`:177`、`:182`）。
  `acceptance_21b_cove_delete_removes_a_same_cove_wave_tree` PASS（
  `crates/calm-truth/src/db/sqlite/sub_wave_tree_tests.rs:85`、`:96`、`:104`）。
- **写事务内无外部 IO**：snapshot 在 `:1362`–`:1388`，turn/process/socket/harness teardown 在
  `:1390`–`:1409`，最终事务只做 DB 删除/lease release，worktree sweep 在事务返回后（
  `crates/calm-server/src/routes/waves.rs:1362`、`:1396`、`:1401`、`:1405`、`:1423`、`:1438`、
  `:1452`、`:1453`）。成功态 terminal/harness/lease/overlay 定向用例均 PASS。

## child-flip 复核点有没有第四处

**没有。** 全仓扫描 `child_wave_id`、child lifecycle JOIN、`WaveLifecycle::{Done,Failed,Canceled}`
和所有 task terminal UPDATE 后，advisory snapshot 只在 `reconcile_child_wave_task`，并恰分三条：
success、Done+pending incomplete、deleted/failed/canceled 共用 terminal（
`crates/calm-server/src/scheduler/mod.rs:778`、`:787`、`:823`、`:856`、`:882`）。三条最终 SQL
分别重读 child lifecycle/quiescence/outcome（`crates/calm-server/src/scheduler/mod.rs:399`、`:412`、
`:429`、`:441`、`:459`、`:471`）。

排除的近邻也逐一看过：projection 的 `child_wave_deleted` 只是 read DTO（
`crates/calm-truth/src/db/sqlite/task_projection.rs:435`、`:439`）；child operation 失败收敛在同一
writer tx 内现读现写（`crates/calm-server/src/scheduler/mod.rs:1535`、`:1547`、`:1571`、`:1573`）；
child reopen guard 同样在 `wave_update_tx` 内现查 parent reference 后写（
`crates/calm-truth/src/db/sqlite/wave.rs:134`、`:137`、`:163`）。它们都不是第四个快照后复核点。

三个负例恢复态均 PASS：`acceptance_18_success_flip_rechecks_done_after_its_snapshot`、
`acceptance_18_incomplete_flip_rechecks_done_after_its_snapshot`、
`acceptance_18_terminal_flip_rechecks_all_three_outcomes_after_its_snapshot`（
`crates/calm-server/tests/scheduler.rs:6115`、`:6156`、`:6202`）。

## 我实际跑过的验证与断言有效性

- 恢复态合并定向集：**17 passed / 0 failed / 3415 skipped，0.456s**；覆盖上述三个 flip、
  三个删除入口、拒删全不变、writer barrier、owner NotFound，以及 route 的 terminal/harness/
  lease/overlay teardown（测试入口见 `crates/calm-server/tests/scheduler.rs:6013`、`:6115`、`:6202`；
  `crates/calm-server/tests/cases/terminal_lifecycle.rs:253`、`:389`、`:462`）。
- **writer 变异**：把 teardown hook 搬进 `write_with_actor_events_typed` 的闭包；
  `wave_delete_external_teardown_does_not_hold_the_sqlite_writer` **FAIL 1/1**，250ms 无关 writer
  barrier 命中 `Elapsed`（`crates/calm-server/tests/cases/terminal_lifecycle.rs:438`、`:447`）。
- **我设计的共源攻击**：把 Failed outcome guard 改成恒假。新的 terminal child-flip 负例单独
  **PASS**（恒假也会给 0），但互补正例
  `acceptance_14_failed_canceled_and_deleted_child_have_distinct_parent_reasons` **FAIL 1/2**，
  Failed parent detail 从期望值变成 `None`（`crates/calm-server/tests/scheduler.rs:6013`、`:6026`；
  负例断言在 `:6258`、`:6268`）。因此单条负例不自证，但组合 oracle 不与实现共用事实源。
- 两次变异后都执行了 `git checkout -- .`；最终恢复态 17/17 再跑通过。

## 可以合入了吗

**YES。** 没有遗留 MINOR；更没有为已整体移出本片的 `0072` 状态机重新扩面。

最终 `git status --short`：

```text
?? docs/_985-s6-impl-review-r4-codex.md
```
