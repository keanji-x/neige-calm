# #985 切片 6 PR-A 实现评审 r4（subagent，收敛检查）

对象：`9d30006a` → `f12767f8`。设计定稿 `docs/_985-s6-design.md` v4。

结论：**BLOCKER 0 / MAJOR 0 / MINOR 4 → 可以合入（YES）。**

所有命令：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH CARGO_BUILD_JOBS=6`，
`NEIGE_CODEX_BIN` 未设置，工作树 `/tmp/wt6b`。恢复态定向集合
（`acceptance_18` ×3 / `acceptance_20` ×3 / `acceptance_21` ×4 / 全部 `wave_delete` /
全部 `cove_delete` / 两条 card owner NotFound）：**41 passed / 0 failed / 2605 skipped，0.463s**。

---

## 一、降范围有没有删出洞（逐项）

裁决是把 `0072_wave_deletions.sql` 及其配套整体移出本片。我对 `aff81e5e → f12767f8`
的**每一处删除**做了「它顺带买过什么 / 现在谁买」的逐项复核，并对 `9d30006a → f12767f8`
做了净 diff 对照（净 diff 是判据：净零 = 回到基线 = 不可能删出洞）。

| # | 被删的东西 | 它顺带买过的性质 | 现在谁买 | 判定 |
|---|---|---|---|---|
| 1 | `card.rs:83` 的 `wave_require_not_deleting_tx` | owner-wave 存在性 ⇒ typed `NotFound` | 已恢复为纯 existence check，与基线**逐字节相同**（`crates/calm-truth/src/db/sqlite/card.rs:84-91` vs `9d30006a:card.rs:83-90`） | **修订方已抓到并修好**；我独立变异复验（见 §三 M3） |
| 2 | `card.rs:582` `terminal_create_tx` 的 owner+marker 复合查询 | parent card 存在性 ⇒ `NotFound` | 净 diff 对基线**只剩变量名 `exists`→`owner`**，语义零差 | 无洞 |
| 3 | `out_of_domain.rs:85` `terminal_create` 同上 | 同上（pool 版） | 同上，净 diff 只有变量名 | 无洞 |
| 4 | `session_row.rs:427` `session_insert_tx` 的 guard | wave 存在性 ⇒ `NotFound`（**r2 新加的**，基线无） | 无人买 —— 但这是回到 `origin/main` 行为，不是新洞；`worker_sessions.wave_id` FK 仍 fail-closed | 无洞 |
| 5 | `task.rs:209` `wave_lifecycle_and_budget_tx` 的 `NOT EXISTS(wave_deletions)` | 「marked wave 对 scheduler 不存在」 | 该性质随机制整体移出，未偷换载体 | 无洞（净 diff 对基线为 0） |
| 6 | `wave.rs` 的 `wave_mark_deleting_tx` / `wave_require_not_deleting_tx` | 两阶段 marker | 已声明不承诺 | 无洞 |
| 7 | `child_wave_adapter.rs:157` 的 `wave_require_not_deleting_tx` | ① marker 拒绝；② **parent wave 存在性 ⇒ `NotFound`** | ②**无人买**：`root_and_depth` 对不存在的 parent 走 `[]` 分支 ⇒ `Conflict("sub-wave-depth-exceeded")`（`crates/calm-server/src/operation/child_wave_adapter.rs:99-114`） | **MINOR-1**（理由码误报，非数据损坏） |
| 8 | 0072 的 6 个 trigger（child/card/terminal/session/lease insert） | 「raw writer 也被 marker 拒绝」 | 随机制移出；本片没有依赖它们的其它不变量（0071 self-FK `NO ACTION` 仍在，`acceptance_21` 绿） | 无洞 |
| 9 | route 里 `session_projection_active_for_card` 逐 card → `worker_sessions.wave_id` 直查（`routes/waves.rs:1372-1378`） | 「活跃 harness 全部 teardown」 | 状态集合与 `session_projection.rs:195` **逐字相同**，且从「每 card 至多 1 条（LIMIT 1）」放宽为「wave 下全部」= **超集**，teardown 只会更全 | 无洞；且**被测试绑住**（见 §三 M2） |
| 10 | 删掉的唯一测试 `durable_wave_delete_marker_refuses_new_resources_and_restart_sweep_finishes` | 只测 marker 状态机 | — | 无洞（`acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal` 保留在 `crates/calm-server/tests/spec_card_reset.rs:1623`，我实跑绿） |
| 11 | 文档/附录 C 的 `wave_deletions` 登记 | — | 全仓 `rg 0072\|wave_deletions\|wave_mark_deleting\|require_not_deleting` 只剩 `card.rs:83` 一条注释 | 无残留 |

**结论：除 #7 外没有第二处「删出洞」。** #7 是 MINOR。

## 二、降范围后的删除路径是否正确

- **分工清楚。** route 前检是不持锁普通读，注释明确写了「Experience-only preflight」
  （`crates/calm-server/src/routes/waves.rs:1509-1512`）；唯一正确性载体是
  `wave_delete_tx` 首行的 `wave_require_leaf_tx`（`crates/calm-truth/src/db/sqlite/wave.rs:222-224`），
  且 `wave_delete_leaf_tx` 已私有化，raw 入口绕不过去。
- **三个入口各有一条。** route = `cards_deletable::acceptance_20_wave_delete_route_refuses_descendant_and_names_child`；
  Repo 直调 = `db::sqlite::sub_wave_tree_tests::acceptance_20_repo_wave_delete_refuses_descendant_and_names_it`；
  cove 删除 = 正向控制 `acceptance_21b_cove_delete_removes_a_same_cove_wave_tree`（guard 必须**不**挡）。三条实跑全绿。
- **写事务内确实没有外部 IO。** `delete_wave` 的顺序是
  snapshot → `teardown_wave_deletion`（turn interrupt / terminal reap / harness shutdown）
  → `finish_wave_deletion`（唯一 `write_with_actor_events_typed`，即 `BEGIN IMMEDIATE`，
  见 `crates/calm-truth/src/db/sqlite/events.rs:345`）：`routes/waves.rs:1524-1526`。
  那条确定性断言**真的会红**（我用与修订方不同的变异独立复验，见 §三 M4）。
- **残留竞态的登记诚实且与代码一致**，但**少写了一半的残留态**：见 MINOR-2。

## 三、我自己设计并实际跑过的断言有效性验证

| 变异 | 我改坏了什么 | 实跑结果 |
|---|---|---|
| **M1** | `scheduler/mod.rs:884` 生产调用点**不再调用** `guarded_child_terminal_flip_tx`，就地写一条**无 outcome guard** 的等价 UPDATE（helper 本身不动） | `acceptance_18` ×3 + `acceptance_13/13b/13d/13e` + `acceptance_14/15/16` + `14b`：**11 passed / 0 failed，全绿** ⇒ **MINOR-3** |
| **M2** | `routes/waves.rs:1374` 活跃 session 状态集合缩成 `state IN ('running')` | `spec_card_reset::wave_delete_shuts_down_active_spec_harness` **红**（`left:1 right:0`，`spec_card_reset.rs:1611`）；同批 63 条全绿 ⇒ 该枚举被绑住 |
| **M3** | 删除 `card.rs:84-91` 恢复回来的 owner-wave existence check | `repo::card_with_codex_create_tx_rolls_back_on_invalid_wave` 与 `repo::card_with_terminal_create_tx_rolls_back_on_invalid_wave` **2 条全红**（`repo.rs:1156` `matches!(err, NotFound)`）⇒ 修订方补回的那一处确实被绑住 |
| **M4** | 不动测试 hook：在 `routes/waves.rs:1524` teardown **前**起一个 `begin_immediate_tx` 并跨 teardown 持有 | `wave_delete_external_teardown_does_not_hold_the_sqlite_writer` **红**（`terminal_lifecycle.rs:447` `unrelated writer was blocked ... Elapsed`）⇒ 该断言不是靠搬 hook 才会红，是真的钉住「写事务不跨外部 IO」 |

---

## MINOR

### m1. child-wave adapter 对「parent wave 不存在」误报 `sub-wave-depth-exceeded`

- **结论**：随 #7 删掉的 guard 顺带买了 parent 存在性 ⇒ `NotFound`。现在
  `root_and_depth` 的空结果分支不区分「深度超限」「无 root」「parent 根本不存在」，
  统一落 `Conflict("sub-wave-depth-exceeded")`。
- **触发条件**：父 wave 在 claim 之后、child-wave operation 的 `prepare_tx` 之前被删
  （route 的 forge fence 只拦 forge-action，不拦 child-wave op：`routes/waves.rs:1503`）。
- **证据**：`crates/calm-server/src/operation/child_wave_adapter.rs:99`、`:104`、`:113`；
  消费方 `crates/calm-server/src/scheduler/mod.rs:1432-1434` 按字符串映射理由码。
- **我跑过的验证**：`acceptance_8_missing_root_fails_closed` 与 `acceptance_9` 在恢复态均绿
  —— 它们只钉「fail-closed」和「不做 in-wave fallback」，**都不区分这两个理由码**，所以
  这条误报没有任何断言反对。
- **危害**：只是父任务 `status_detail` 误导（父 wave 本身正在被删，其 task 随后级联消失），不损坏数据。
- **最小修法**：`root_and_depth` 的 `[]` 分支先查 `SELECT 1 FROM waves WHERE id=?1`，
  空则返回 `CalmError::NotFound`；对应加一条负例。

### m2. 残留竞态的登记只说了一半

- **结论**：`docs/_985-s6-impl-notes.md`「已知安全竞态」写的是「teardown 已发生，最终事务
  返回 409 …… 不损坏数据、可重试」。实际残留态还包括：`finish_wave_deletion` 回滚后
  **terminal 行/`worker_sessions` 行都还在且状态是 active，但对应进程已被杀、harness 已
  从 registry 移除**（`routes/waves.rs:1398-1404`；`harness/run_loop.rs:184-190` 的
  `shutdown()` 不改 session 行）。也就是说该 wave 会呈现「读端仍 live、实际已死」，
  直到用户重试删除或进程重启。
- **触发条件**：前检通过后、`wave_delete_tx` 前 scheduler 为该 wave 的 sub-wave 任务建 child。
- **我跑过的验证**：无定向测试覆盖此残留态（`acceptance_20_descendant_refusal_...` 覆盖的是
  **前检命中**的路径，即「什么都没动」；实跑绿）。
- **最小修法**：登记文字补上「terminal/session 行存活但进程已死」，并在另开的 issue 里点名。

### m3. `acceptance_18` 三条 oracle 绑的是 helper，不是生产接线

- **结论**：三条 oracle 都经 `guarded_child_*_flip_for_test` 直调 helper
  （`crates/calm-server/src/scheduler/mod.rs:487`、`:498`、`:509`）。**生产若不再调用
  helper、就地写一条无 guard 的 UPDATE，三条 oracle 与全部 child 相关验收都不红**（M1 实测 11/11 绿）。
  按「测试必须驱动生产接线」这条判据，这三条断言与被测代码共用的是 *helper*，不是 *生产路径*。
- **减轻因素（因此只判 MINOR 而非 MAJOR）**：生产的 snapshot 与 flip 在**同一个 `BEGIN IMMEDIATE`
  写事务**内（`scheduler/mod.rs:786`→`crates/calm-truth/src/db/sqlite/events.rs:345`），
  writer 槽独占，snapshot 不可能被并发写者作废。这三条 guard 在**当前**形状下是纵深防御、
  不是活的正确性载体；design v4 §7 #18 的「snapshot 只是 advisory」也因此暂时是空转的。
- **最小修法**：任选其一 —— ① 把 oracle 改成驱动 `reconcile_child_wave_for_test`，
  在 fixture 里用另一连接在 snapshot 与 flip 之间提交（当前 IMMEDIATE 下会 busy，
  故实际做法是把 helper 调用点的存在性做成一条源码/注册表 meta 断言）；
  ② 在设计文档里明确记「guard 是纵深防御，正确性由 IMMEDIATE 事务购买」，
  并把 §7 #18 的措辞降级，避免下一轮再把它当活承重点。

### m4. driver `TxCommitted` 的 `required_output` 上提是顺带的语义收紧

- **结论 / 证据**：`crates/calm-server/src/operation/driver.rs:450` 把
  `required_output(&op)?.clone()` 提到分支之前。改动前，`phases` 含 `SpawnStarted` 且不含
  `AppServerInteract` 的 adapter（正是新的 child-wave：`child_wave_adapter.rs:23-29`）
  **不会**调用它；改动后会，缺 `tx_output` 时从「静默进入 SpawnStarted」变成
  `CalmError::Internal`（`operation/mod.rs:974-978`）。
- **危害**：`prepare_tx_and_advance` 总会写 `tx_output`，因此不可达，方向也是 fail-closed；
  但这是共享 driver 热路径上的顺带语义变更，没有任何 oracle。
- **我跑过的验证**：恢复态定向集合 41/41 绿；未构造缺 `tx_output` 的 fixture（不可达）。
- **最小修法**：加注释说明这是刻意的 fail-closed 收紧，或把 `clone()` 移回两个分支内。

---

## child-flip 复核点有没有第四处

我按「**在快照之后、写之前需要复核 child 状态**」的定义独立扫了全部 child 相关写者，
结论：**生产上确有第四处需要复核 child 的写点 —— `mark_sub_wave_running` —— 但它用的是
互斥的 task-status CAS 而非 child 复核，且该选择是正确的**，因此修订方声称的「三处」在
「需要 child 复核」这一口径下成立。逐项：

| 候选写点 | 是否在快照后复核 child | 判定 |
|---|---|---|
| `reconcile_child_wave_task` success 臂（`scheduler/mod.rs:832`） | 是，`guarded_child_success_flip_tx:399` 的 `EXISTS(child.lifecycle='done')` + quiescence | 已枚举（第 1 处） |
| 同上 incomplete 臂（`:860`） | 是，`guarded_child_incomplete_flip_tx:429` | 已枚举（第 2 处） |
| 同上 terminal 臂（`:884`） | 是，`guarded_child_terminal_flip_tx:459` + 三理由各自 `sql_guard()`（`:383-392`） | 已枚举（第 3 处） |
| `mark_sub_wave_running`（`:1509`）→ `task_mark_sub_wave_running_tx`（`calm-truth/src/db/sqlite/task.rs:142`） | **否** —— 只有 `status='dispatched' AND spawn='sub-wave' AND child_wave_id IS NOT NULL` | **不是洞**：与 reconcile 的互斥由 `status IN ('dispatched','running')` vs `status='dispatched'` 的 CAS 完成，两个方向的交错都收敛（先 reconcile ⇒ 本 UPDATE 0 行；先本 UPDATE ⇒ reconcile 命中 `running`）。child 被删后最多多停留一个 sweep 周期，`reconcile_all_child_wave_tasks:756` 扫 `dispatched/running` 兜底。已由 `acceptance_12` 的三条单变量负例（修复轮 1 M2-a/b/c）钉住 |
| `fail_child_wave_task`（`:1520`） | 是，事务内重读 `tasks.child_wave_id` + `wave_get_tx` 复核 `is_terminal()`（`:1548`、`:1571-1573`） | 已由修复轮 1 B3 钉住，不是「快照后」形状 |
| `wave_update_tx` 的 reopen 拒绝（`calm-truth/src/db/sqlite/wave.rs:135-145`） | 是，同一事务内查 `tasks WHERE child_wave_id=?1` | 已由 `acceptance_17` 钉住 |
| `drive_child_wave` 写 `child_wave_id`（`:1445-1500`） | N/A，child 此刻刚由 adapter 在同事务创建 | 不适用 |
| `wave_delete_tx` / `cove_delete_tx` | N/A，删的是 child 本身 | 不适用 |

**答案：没有第四处**（`mark_sub_wave_running` 是同类写点但正确地用 CAS 而非 child 复核；
这一点值得写进设计文档，避免下一轮把它当遗漏）。

---

## 可以合入了吗

**YES。** BLOCKER 0、MAJOR 0，只剩 4 条 MINOR：m1（理由码误报，另开或随手修）、
m2（登记文字补半句）、m3（oracle 绑定层级 + 设计措辞降级）、m4（一行注释）。
四条都不影响本片承诺的不变量，也都不需要在合入前落地。

降范围本身复核干净：11 个删除项里只有 1 项（child adapter 的 parent 存在性）没有新买主，
且危害限于理由码；其余 10 项要么净 diff 回到基线、要么由已声明不承诺的机制承载。
删除路径的三个入口、写事务内无外部 IO、拒删全不变三条承诺我都用**独立设计的变异**验过会红。

```text
$ git status --short
（空）
```
