# #985 切片 6 PR-A 实现评审 r3（subagent）

评审对象：`9d30006a` → `83e98325`（含修复轮 2 `9ac5e710`）。设计定稿 `docs/_985-s6-design.md` v4。
结论先行：**NO**。r2 的 2 BLOCKER 均已在行为上收敛（B1 的写事务不再跨外部 IO、M1 的 #21c 两个
合取项并存、B2 的 25ms 换成 `wait_entered` 确定性 barrier，均实测确认）。但修复轮 2 新增的
`0072` 两阶段删除是一套**承重的持久状态机 + 并发栅栏**，它的四条关键机制里有三条
**整段删掉全绿**；加上第三个 child-flip 复核点同样整段删掉全绿。没有找到可复现的线上正确性破坏，
因此不判 BLOCKER。

全部命令：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH CARGO_BUILD_JOBS=6`，
`NEIGE_CODEX_BIN` 未设置（`env | grep NEIGE_CODEX_BIN` 空）。基线定向集
**12 passed / 0 failed，0.482s**；扩展集 **175 passed / 0 failed**。

---

## 一、`0072` 这套新机制有没有引入新洞

先说没打穿的（这些我实际跑过，不重复攻击）：

- **中间态的读/写路径**。`deleting` 期间新建资源在两层被拒：typed Rust guard
  （`crates/calm-truth/src/db/sqlite/card.rs:84`、`out_of_domain.rs:86`、`session_row.rs:427`、
  `crates/calm-server/src/operation/child_wave_adapter.rs:158`）+ `0072` 的 5 条 BEFORE INSERT
  trigger。调度侧走**唯一可调度谓词**
  `crates/calm-truth/src/db/sqlite/task.rs:211` 的 `NOT EXISTS(wave_deletions)`，标记后 wave 直接
  等价于「行已消失」。`terminal_lifecycle::durable_wave_delete_marker_refuses_new_resources_and_restart_sweep_finishes`
  **PASS 0.134s** 覆盖 card / terminal / child attach 三条。
- **marker 的生命周期**。`wave_deletions.wave_id REFERENCES waves(id) ON DELETE CASCADE` +
  `PRAGMA foreign_keys = ON`（`crates/calm-truth/src/db/sqlite/mod.rs:208`、`:218`）⇒ 删 wave 即清
  marker；`/dev/reset` 的 `DELETE FROM waves`（`crates/calm-server/src/replay.rs:373`）同样带走它。
  「marker 在、wave 不在」不可达，因此不存在 resume 永远 NotFound 的死循环。
- **两条 migration 同 PR**。`0071` / `0072` 均 additive（`ALTER TABLE ADD COLUMN` / `CREATE TABLE` +
  `CREATE TRIGGER`），无表重建。`migration_replay_harness::synthetic_fixture_replays_from_every_version`
  按全 schema 指纹比对（含 trigger，`tests/cases/migration_replay_harness.rs:38`）：
  **PASS 134.960s**（>120s 的 O(M²) 形状 r2 已三方基线定性为非本片退化）。
- **fence 释放点之后的窗口**。`pause_drives_for_wave_delete` 与 `drive()` 争同一把
  `drive_mutex`（`crates/calm-server/src/operation/driver.rs:125`、`:351`），而 `drive()` 持锁跨越
  整个 batch 含外部 IO ⇒ marker 提交时不存在「已过检查、尚未起活」的 in-flight external phase；
  释放之后新起的 drive 一律读到 marker。这条推理成立，但**没有任何测试锁住它**，见 M2。

下面四条是打穿的。

### M1（MAJOR）fence 认 wave 的那半条分支，生产全靠它，测试一条没有

- **结论**：`fail_if_wave_deleting_before_external_io` 用两条分支找 wave：
  `output.data["wave_id"]` 与 `target_type == "wave"`
  （`crates/calm-server/src/operation/driver.rs:699`–`:707`）。**所有会起活的生产 adapter 走的是
  第一条**（codex `codex_adapter/mod.rs:390`、`:855`；claude `claude_adapter/mod.rs:507`、`:877`；
  terminal `terminal_adapter.rs:303`、`:644`；spec-harness-start `spec_harness_start_adapter.rs:417`；
  task-verify `task_verify_adapter.rs:707` 的 `FrozenVerify`）。而唯一的 fence 测试用的 fixture
  adapter 走的是**第二条**（`crates/calm-server/tests/scheduler.rs:929`–`:932` 的
  `TxOutput::new("wave", Some(wave_id), …)`）。
- **攻击**：把 `data["wave_id"]` 分支整段删掉，只留 `target_type == "wave"`。
- **实际验证**：`binary(scheduler) + terminal_lifecycle:: + sub_wave_tree_tests:: +
  child_wave_adapter::tests::`，变异态 **127 passed / 0 failed / 0 red，2.161s**；
  `deleting_wave_fails_queued_bootstrap_before_external_io` 依旧 PASS。恢复态同集 PASS。
- **顺带的生产缺口**（同一根因，未单独计条）：`wave_id` 是**未类型化的 per-adapter 约定**，
  没有 registry set-equality 元测试。实测两个生产 adapter 两条分支都不满足 ——
  `spec_harness_interrupt_adapter.rs:78`–`:86`（`target_type="runtime"`，data 只有
  `runtime_id`/`reason`）与 `spec_harness_shutdown_adapter.rs:79`–`:85`（data 只有 `runtime_id`）；
  `claude_restart_adapter.rs:259` 把它写成**可为 null 的 `Option`**，null 时 `Value::as_str`
  返回 `None`，fence 静默 no-op。前两者的外部 IO 只做拆卸，我**没有**证明能造出真实泄漏，
  所以按测试洞而非 BLOCKER 计。
- **最小修法**：把 wave 归属提成 `TxOutput` 的必填字段或 `ProviderAdapter` 的定向方法
  （required-over-option），加一条遍历 `registered_adapter_kinds()` 的元测试断言每个
  会起活的 kind 都能被 fence 解析出 wave；fence 测试至少各覆盖一条分支。

### M2（MAJOR）drive fence 是整套两阶段的 happens-before，删掉它全绿

- **结论**：`resume_wave_deletion` 里的 `pause_drives_for_wave_delete()` 是「marker 提交」与
  「operation external phase」之间**唯一**的串行点（`crates/calm-server/src/routes/waves.rs:1484`、
  `:1486`）。它决定了「快照之后不会有已通过检查的 external phase 继续起活」。
- **攻击**：删掉 fence 获取与 `drop`，只留 `prepare_wave_deletion`。
- **实际验证**：变异态 `binary(scheduler) + terminal_lifecycle:: + sub_wave_tree_tests:: +
  child_wave_adapter::tests:: + cards_deletable:: + binary(spec_card_reset)`：
  **175 passed / 0 failed，2.192s**。恢复态同集 **175 passed / 0 failed，4.173s**。
- **最小修法**：用已有的 `WaveDeleteTeardownHook` + `install_wait_entered_hook_for_test`
  写一条交错：drive 停在 external phase 入口 ⇒ 并发 DELETE 的 marker 事务必须等到 drive 放行
  才提交；再断言该 op 最终 `Failed{"wave-deleting"}` 且没有新进程。

### M3（MAJOR）「marker 后不得新增 teardown-owned 资源」五条里，两条整段删掉全绿

- **结论**：`0072` 枚举了 5 类资源。测试只锁 card / terminal / child wave 三类
  （`tests/cases/terminal_lifecycle.rs:530`、`:541`、`:565`）。`worker_sessions` 与
  `workspace_leases` 两条**零覆盖**。
- **攻击**：删掉 `wave_deleting_blocks_session_insert` 与 `wave_deleting_blocks_lease_insert`
  两条 trigger（`crates/calm-truth/migrations/0072_wave_deletions.sql:41`、`:49`），**并**删掉
  `crates/calm-truth/src/db/sqlite/session_row.rs:427` 的 Rust guard。
- **实际验证**：变异态 M2 同集再并 `test(session)`：**296 passed / 0 failed，2.412s**。
- **为什么这条不只是覆盖问题**：`active_runtime_ids` 快照只取
  `('starting','running','idle','turn_pending')`（`routes/waves.rs:1391`），teardown 只 shutdown
  快照里的 runtime（`:1423`–`:1427`），而 `wave_delete_leaf_tx` 会
  `DELETE FROM worker_sessions WHERE wave_id`（`crates/calm-truth/src/db/sqlite/wave.rs:320`）。
  快照之后落地的 session 行会被**静默删除、其 harness 进程/socket 永不 shutdown** —— 正是 B1
  当初要消灭的泄漏形状。这条 trigger 是承重件。
- **最小修法**：给两类各补一条单违规 fixture（marker 提交后 `session_create` / lease 取得必须
  被拒且 registry 与 DB 不变），并覆盖「trigger 单独生效」与「Rust guard 单独生效」两个变体。

### M4（MAJOR）第三个 child-flip 复核点没被覆盖 —— r2 教训在第三个站点复发

- **结论**：r2 把 Done 复核拆成 success / pending-incomplete 两个 helper 各配行为 oracle
  （`crates/calm-server/src/scheduler/mod.rs:353`、`:383`）。但同一个
  `reconcile_child_wave_task` 里还有**第三个 guarded flip**：`None` / `Failed` / `Canceled`
  三理由共用的内联 `child_guard`（`scheduler/mod.rs:790`–`:806`，`changed==0 ⇒ race_lost` 在 `:816`）。
  它同样是「快照 advisory、UPDATE 才是权威」的复核点，没有被抽出，也没有 oracle。
- **攻击**：把 `AND ({child_guard})` 整段从 SQL 里删掉，只留 `status IN ('dispatched','running')`。
- **实际验证**：变异态 `binary(scheduler) + binary(dispatcher) + binary(spec_card_reset)`：
  **152 passed / 0 failed / 0 skipped，2.257s**。恢复态同集 PASS。
- **最小修法**：按 r2 的同一形状抽出 `guarded_child_terminal_flip_tx` + `_for_test`，
  三理由各写一条「同事务内先观察终态、再把 child 翻到别的 lifecycle、再调生产 helper ⇒
  `rows_affected == 0` 且行仍 `dispatched`」。

---

## 二、我自己设计的变异里，哪几条打穿了

四条设计、四条打穿（对照组均在恢复态复跑并全绿）：

1. **删 `output.data["wave_id"]` 分支** ⇒ 127/127 全绿。生产起活路径的 fence 无测试。（M1）
2. **删 `pause_drives_for_wave_delete`** ⇒ 175/175 全绿。整套两阶段的 happens-before 无测试。（M2）
3. **删 session/lease 两条 trigger + session 的 Rust guard** ⇒ 296/296 全绿。（M3）
4. **删失败臂的 `child_guard` 复核** ⇒ 152/152 全绿。（M4）

判据一致：**被测代码与断言共用同一事实来源** —— 三条 fence/trigger 的「唯一断言」都由一条
happy-path 集成测试隐式提供，而那条测试的通过路径压根不经过被删的机制。

按简报要求未重复攻击：SLOW migration replay（本轮仍跑了一次确认 `0072` 可重放）、
B3 自查/stamp 原子性、M2/M4 单变量负例表、B2 25ms 窗（已换确定性 barrier，
`driver.rs:281`–`:285` + `tests/scheduler.rs` 的 `install_wait_entered_hook_for_test`）。

---

## 三、MINOR

- **m1 `wave_deletions` 没在权威文档登记**。`docs/architecture/985-doc-as-plan.md:1855` 明写
  「**新增机制必须在此登记一行**」，附录 C.2/C.3 登记了 `parent_wave_id`、`tree_task_budget`，
  但 `grep -n "wave_deletions\|0072"` 在 `985-doc-as-plan.md` 与 `_985-s6-design.md` **均零命中**。
  一个新的持久真源的四格（载体 / 谁写 / rebuild 怎么重放 / migration 怎么 backfill）完全没写。
  修法：补 C.2 一行（载体 = 新表；谁写 = `wave_mark_deleting_tx` 唯一写口；rebuild = 运行期状态、
  随 wave 级联、不从文档重放；backfill = 无，additive）。
- **m2 恢复 sweep 不在 boot 跑**。`terminal_sweeper::spawn` 显式跳过首 tick
  （`crates/calm-server/src/terminal_sweeper.rs:105`），`SWEEP_INTERVAL = 30s`（`:85`），
  而 `recover_operations_on_boot`（`crates/calm-server/src/lib.rs:203`）与 boot parked sweep
  排在它**之前**且不受 fence 保护。设计 D.1 #5b 对 context sweep 有「源码序断言」，
  对更强的 durable delete marker 没有对应约束。测试是手工调 `sweep`
  （`tests/cases/terminal_lifecycle.rs:592`），因此没有钉住生产顺序。
- **m3 teardown 永久失败没有退避与告警**。`harness.shutdown().await?`（`routes/waves.rs:1425`）
  失败即整条 resume 返回 Err，只落一条 `warn!`（`:1516`），marker 每 30s 重试一次、无上限、
  无健康信号；wave 会永久半死（列表可见、任何资源创建被拒）。附录 C.5 的健康信号表也没有
  对应 gauge。修法：给 marker 加 `attempts` / `last_error` 并进 health 快照。
- **m4 快照的活跃态清单是手抄**。`routes/waves.rs:1391` 的 `IN ('starting','running','idle',
  'turn_pending')` 与 `WorkerSessionState::is_active_authority`（`crates/calm-types/src/worker.rs:351`）
  逐字重合但无 lockstep 断言；另外 teardown 只遍历 DB 快照，registry 里 session 已离开该集合
  的 harness handle 永不 shutdown。
- **m5 typed Conflict 与 trigger ABORT 不可区分**。两层的错误串都含 `deleting`，而测试断言正是
  `contains("deleting")`（`tests/cases/terminal_lifecycle.rs:537`、`:548`），所以「Rust guard 给
  409、trigger 只兜底」这条分层根本没被验证；只留 trigger 时用户拿到的是 500。
- **m6 恢复路径丢失 actor**。`resume_marked_wave_deletions` 一律用 `ActorId::Kernel`
  （`routes/waves.rs:1509`），崩溃恢复后的 `WaveDeleted` 归因从发起人变成内核，marker 表里
  也没存原 actor。

---

## 四、可以合入了吗

**NO。**最小阻塞集：**M2**（drive fence 加交错测试）、**M3**（session/lease 两条单违规 fixture，
其中 session 是真实泄漏面）、**M4**（第三个 flip 站点抽出 + 三理由 oracle）、**M1**（wave 归属
类型化 + registry 元测试）。四条都是**新引入的承重机制零覆盖**，不是既有债。
m1 建议随本轮一起补（一行表格 + 四格），其余 MINOR 不阻塞。

若判定「本轮只补 M2/M3/M4 三条测试、M1 的 registry 元测试另开 issue」，我不反对 ——
但 M1 里 `spec-harness-interrupt` / `spec-harness-shutdown` 两个 kind 的 fence 事实上为 no-op
这一点必须显式写进 issue，不能留在评审文档里。

```text
$ git status --short
?? docs/_985-s6-impl-review-r3-subagent.md
```
