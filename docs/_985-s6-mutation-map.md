# #985 切片 6 PR-A 变异映射

记录日期：2026-08-06。表内只登记本工作树上实际改动并执行过测试的变异；每次执行后均已恢复。`docs/_985-s6-design.md` §7 当前实际有 **33** 个编号行，不是任务描述所称的 30 个，因此这里按 33 行全量列出，避免静默漏项。

| 验收 # | 实际执行的变异（文件与表达式） | 实际变红的测试全名 | 结果 |
|---|---|---|---|
| 1 | `crates/calm-truth/src/db/sqlite/task_projection.rs:1035`：删除 UPSERT 变更检测中的 `OR tasks.spawn IS NOT excluded.spawn`，保留 SET。 | `db::sqlite::task_projection::tests::acceptance_1_spawn_only_projection_change_updates_row_and_changed_keys` | 红（exit 101）。 |
| 2 | `crates/calm-server/src/scheduler/mod.rs:107`：把 `fence_revision_matches` 写死为 `true`。 | `acceptance_2_claim_fence_rejects_spawn_edit_after_resolution_without_side_effects` | 红（exit 101，行变成 `Dispatched`）。 |
| 3a | `crates/calm-server/src/scheduler/mod.rs:897` 附近：claim 后、`drive_spawn` 前把 frozen `spawn` 改成 `in-wave`。 | `acceptance_3a_claim_frozen_spawn_routes_live_after_post_claim_report_edit` | 红（exit 101）。 |
| 3b | `crates/calm-server/src/scheduler/mod.rs:2073`：`resume_dispatched` 把 frozen `spawn` 改成 `in-wave`。 | 补强 fixture 后：`acceptance_3b_claim_frozen_spawn_routes_recovery_without_report_reread` | **首跑仍绿**（1 passed）：fixture 没注册 in-wave 假 adapter，错误路由被“没有 adapter”遮住；补入 `CardSpawnAdapter` 后，同一变异红（exit 101）。 |
| 3c | `crates/calm-server/src/scheduler/mod.rs:1055`：tx 内重读后强制 `frozen.spawn = "in-wave"`。 | `acceptance_3c_claim_success_uses_transaction_reread_spawn` | 红（exit 101）。 |
| 4 | `crates/calm-types/src/report_blocks/tasks.rs:580`：只把显式 `null` 规范化成 `broken-null`，缺席仍为 `in-wave`。 | `report_blocks::kinds::tests::acceptance_4_missing_explicit_in_wave_and_null_spawn_normalize_identically` | 红（exit 101）。 |
| 5 | `crates/calm-server/src/operation/child_wave_adapter.rs:154` 后：在真实 `prepare_tx` 中把 payload `goal` 覆盖成 `current-goal`。 | `operation::child_wave_adapter::tests::acceptance_5_child_seed_uses_all_four_frozen_fields_and_parent_cwd` | 红（exit 101）。本轮只实际变异了 goal；acceptance/context/cwd 的三个独立变异未执行。第一次误改到另一处局部变量只造成编译错误，未计入证据。 |
| 5b | `crates/calm-server/src/operation/child_wave_adapter.rs:154`：删除第一副作用前的 `refuse_if_context_stale`。 | `acceptance_5b_stale_frozen_context_refuses_real_child_operation` | 红（exit 101，waves 计数增加）。 |
| 6 | `crates/calm-server/src/operation/child_wave_adapter.rs:157`：深度判断从 `>=` 改成 `>`。 | `operation::child_wave_adapter::tests::acceptance_6_real_adapter_writes_direct_parent_and_enforces_depth_three` | 红（exit 101）。direct-parent→root 的第二个独立变异未执行。 |
| 7 | `crates/calm-server/src/operation/child_wave_adapter.rs:42`：删除递归 CTE 唯一的 `WHERE up.depth <= ?2`。 | `operation::child_wave_adapter::tests::acceptance_7_two_cycle_fails_fast_with_cycle_reason` | 红：编译后用例运行超过 60 秒，外层 150 秒 timeout 终止；随后恢复截断。 |
| 8 | `crates/calm-server/src/operation/child_wave_adapter.rs:106`：零行分支改成 `Ok((parent_wave_id, 0))`。 | `operation::child_wave_adapter::tests::acceptance_8_missing_root_fails_closed` | 红（exit 101）。 |
| 9 | `crates/calm-server/src/scheduler/mod.rs:1260`：遇 `sub-wave-depth-exceeded` 时提交普通 worker fallback。 | `acceptance_9_depth_exhaustion_fails_parent_without_in_wave_fallback` | 红（exit 101，父任务仍 `Dispatched`）。 |
| 10 | `crates/calm-server/src/operation/child_wave_adapter.rs:154`：删除 child-wave adapter 的 stale fence，再运行遍历 registry 的真实 adapter 测试。 | `every_registered_task_adapter_refuses_material_context` | 红（exit 101，child-wave 产生 `TxOutput`）。 |
| 11 | 三次实际尝试：① `crates/calm-server/src/scheduler/mod.rs:1743` 把专用臂谓词改成 `sub-wave-mutant`；② `:293` 删除 `task_has_running_liveness_deadline` 中的 spawn 排除；③ `:1743` 直接让 sub-wave 臂调用 `fail_running_liveness_timeout`。 | 第三次：`acceptance_11_sub_wave_parent_survives_two_timeout_sweeps_without_deadline` | ①、②均**仍绿**（各 1 passed）：分别被后备谓词与更早的专用臂遮蔽；③ 红（exit 101）。这证实设计所述两个内层站点在正确臂序下是死代码。 |
| 12 | `crates/calm-truth/src/db/sqlite/task.rs:144` 的 sub-wave running UPDATE 同时写 `worker_card_id='mutant-worker'`、`running_deadline_ms=1`。 | `acceptance_12_sub_wave_running_stamp_has_no_worker_or_deadline` | 红（exit 101；先红在 worker id）。deadline-only 独立变异未执行。 |
| 13 | `crates/calm-server/src/scheduler/mod.rs:647`：把 gate 分流条件改成 `false && snapshot.gate_json.is_some()`。 | `acceptance_13_done_quiescent_child_routes_parent_through_gate` | 红（exit 101，得到 `Done` 而非 `Verifying`）。未单独执行“删除 TaskCompleted 事件”变异。 |
| 13b | `crates/calm-server/src/scheduler/mod.rs:643` 与成功 SQL guard：删除 quiescence 条件。 | `acceptance_13b_and_13c_inflight_child_blocks_then_eventually_closes_parent` | 红（exit 101，父任务过早 `Verifying`）。 |
| 13c | `crates/calm-server/src/scheduler/mod.rs:618`：把 `pending` 加回 `inflight_count` 的状态集合。 | `acceptance_13d_done_child_with_pending_block_or_legacy_fails_with_count` | 红（exit 101，父任务留在 `Running`）。上一轮误标 M13c 的 `pending_count > 1000` 也红在同一测试，但这里只登记补跑后的准确设计变异。 |
| 13d | `crates/calm-server/src/scheduler/mod.rs:702`：理由码改成 `child-wave-incomplete-mutant`。 | `acceptance_13d_done_child_with_pending_block_or_legacy_fails_with_count` | 红（exit 101）。 |
| 13e | `crates/calm-server/src/scheduler/mod.rs:1271`：child-create 的 `OperationOutcome::Stuck` 直接 `Ok(())`。 | `acceptance_13e_failed_and_stuck_at_both_operation_levels_close_once` | 红（exit 101，`create/stuck` 留在 `Dispatched`）。本次 mutant 只删 create/Stuck；正向测试仍表驱动四臂。 |
| 14 | `crates/calm-server/src/scheduler/mod.rs:685`：删掉 child 不存在的失败映射（令 `None` race-lost）。 | `acceptance_14_failed_canceled_and_deleted_child_have_distinct_parent_reasons` | 红（exit 101，deleted reason 为空）。Failed/Canceled 两臂未分别变异。 |
| 14b | 后端：`crates/calm-truth/src/db/sqlite/task_projection.rs:203` 把 `child_wave_deleted` 写成 `None`；前端：`web/src/pages/report-blocks/task.tsx:131` 把 tombstone 条件改成 `false && verdict.childWaveDeleted`。 | `db::sqlite::task_projection::tests::acceptance_14b_and_22_read_dto_marks_deleted_child_and_never_exposes_spawn`；`degraded blocks > acceptance 14b renders a deleted child wave as a non-clickable tombstone` | 两个变异均红（Rust exit 101；Vitest 1 failed / 50 skipped）。 |
| 15 | `crates/calm-server/src/scheduler/mod.rs:1791`：从 `sweep_all` 删除 `reconcile_all_child_wave_tasks()`。 | `acceptance_15_lost_event_sweep_closes_child_parent` | 红（exit 101）。 |
| 16 | `crates/calm-server/src/scheduler/mod.rs:565`：让 live `reconcile_child_wave` 立即 return。 | `acceptance_16_live_and_sweep_use_the_same_guarded_conclusion` | 红（exit 101）。 |
| 17 | `crates/calm-truth/src/db/sqlite/wave.rs:133`：把 raw writer reopen 守卫前置为 `false && ...`。 | `db::sqlite::sub_wave_tree_tests::acceptance_17_raw_lifecycle_writer_refuses_reopen_of_referenced_child` | 红（exit 101，writer 返回 Ok）。 |
| 18 | `crates/calm-server/src/scheduler/mod.rs:657`：从成功 flip SQL 删除 child 仍为 Done 的 `EXISTS` guard。 | `scheduler::tests::acceptance_18_child_success_flip_rechecks_child_state_in_its_sql_guard` | 红（exit 101，源码中 guard 计数从 2 变 1）。 |
| 19 | `crates/calm-server/src/scheduler/mod.rs:1317`：bootstrap `idempotency_key` 从稳定 key 改成 `None`。 | `acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive` | 红（exit 101，start-op 计数为 2）。 |
| 20 | `crates/calm-truth/src/db/sqlite/wave.rs:222`：让 tx 内 descendant guard 永不执行。 | `db::sqlite::sub_wave_tree_tests::acceptance_20_repo_wave_delete_refuses_descendant_and_names_it`；`cards_deletable::acceptance_20_wave_delete_route_refuses_descendant_and_names_child` | 两入口均红（Repo 丢 child id；REST 从 409 退化成 FK 500）。cove 删除的正向控制由 #21b 覆盖。 |
| 21 | `crates/calm-truth/migrations/0071_sub_wave_tree.sql:4`：临时加 `ON DELETE CASCADE`（执行后恢复；未触碰 0070 及以前）。 | `db::sqlite::sub_wave_tree_tests::acceptance_21_migration_uses_no_action_self_fk_and_partial_indexes` | 红（exit 101，读到 CASCADE）。 |
| 21b | 同一 0071 FK 临时改成 `ON DELETE RESTRICT`。 | `db::sqlite::sub_wave_tree_tests::acceptance_21b_cove_delete_removes_a_same_cove_wave_tree` | 红（exit 101，FK constraint failed）。 |
| 21c | `crates/calm-server/src/operation/child_wave_adapter.rs:173`：child `cove_id` 改成固定错误值 `cross-cove-mutant`。 | 旁路保护红：`operation::child_wave_adapter::tests::acceptance_6_real_adapter_writes_direct_parent_and_enforces_depth_three`；指定 tripwire `db::sqlite::sub_wave_tree_tests::acceptance_21c_cross_cove_edge_is_a_loud_delete_tripwire` **没有红**。 | **仍绿（验收缺口）**：指定 #21c 测试为 1 passed，因为它只用 raw SQL 自造跨-cove 边，未驱动 child adapter；同一 mutant 被 #6 的真实 adapter 测试抓红。 |
| 22 | `crates/calm-truth/src/db/sqlite/task_projection.rs:150`：把 `child_wave_id` 的序列化 key 临时重命名成 `spawn`。 | `db::sqlite::task_projection::tests::acceptance_14b_and_22_read_dto_marks_deleted_child_and_never_exposes_spawn` | 红（exit 101，DTO JSON 含 `spawn`）。 |
| 23 | `crates/calm-types/src/report_blocks/kinds.rs:257`：给公共 kind/spawn 校验加 `false &&`，等价删除拒绝。 | `report_blocks::kinds::tests::acceptance_23_sub_wave_rejects_claude_and_terminal_at_common_write_validation` | 红（exit 101，校验返回 Ok）。 |

## 汇总

- 33 个编号行均有实际执行记录，没有整行“未实现”。
- 含“仍绿”证据的编号行 3 个（#3b、#11、#21c），共 4 次实际仍绿尝试；其中 #3b 已通过补强 fixture 修复，#11 是设计已承认的死代码形状，#21c 是尚存的验收缺口。
- 独立变异覆盖仍不完整的子断言已在实现说明中逐条列出；没有把未执行的子变异写成已验证。

## 修复轮 1

以下变异均在本工作树实际执行，命令保持 `NEIGE_CODEX_BIN` 未设置、
`CARGO_BUILD_JOBS=6`，且 PATH 含指定 `.local-bin`；每次运行后均用反向补丁复原。

| 修复项 | 我改坏了什么 | 对应测试与实际结果 |
|---|---|---|
| B1 | 跳过 DELETE 写事务开头的 `wave_require_leaf_tx`，让 teardown 后的 `wave_delete_tx` 才拒绝 descendant。 | `acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal` **红**（1 failed，断言捕获 terminal 进程已被杀；harness/registry/socket/DB 也在同一 fixture 受守护）。 |
| B2 | 把 `mark_sub_wave_running` 提到 bootstrap `wait` 前，成功臂改 `Ok(())`。 | 第一版 hook 阻塞在同步 `submit()` 内，变异 **仍绿**（1 passed），说明接缝仍没到 `wait`；改成 adapter 返回 durable `Parked`、observer 在通知前保留 25ms 调度窗并继续阻塞后，同一变异使 `acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive` **红**（1 failed，阻塞期父状态为 `running` 而不是 `dispatched`）。恢复后的测试还会在阻塞点丢弃 runtime、重建后断言 child/bootstrap op 各 1 条且 mint 1 次。 |
| B3 | 保留事务内 `SELECT tasks.child_wave_id` 但强制清理分支使用 `None`。 | `acceptance_13e_failed_and_stuck_at_both_operation_levels_close_once` **红**（1 failed，真实 adapter 提交后的 child 仍为 `planning`，不是 `failed`）。测试对 create 的 Failed/Stuck 两臂都先驱动真实 child/bootstrap adapter，并在收场后 leaf-first 删除 child、parent。 |
| M1 | 从共享 `bounded_wave_ancestor_cte!` 删除唯一的 `WHERE up.depth <= ?2`。 | `upward_cte_keeps_its_only_cycle_termination_guard` 与 `acceptance_7_two_cycle_fails_fast_with_cycle_reason` **2 条全红**；运行时用例在约 500ms 的 `tokio::time::timeout` 处失败，不再挂到外层 SIGTERM。 |
| M2-a | 单独删除 `task_mark_sub_wave_running_tx` 的 `status='dispatched'`。 | `acceptance_12_sub_wave_running_stamp_has_no_worker_or_deadline` **红**（1 failed，已 failed 行 `rows_affected=1`，会被复活）。 |
| M2-b | 单独删除同一 UPDATE 的 `spawn='sub-wave'`。 | 同一测试 **红**（1 failed，`in-wave` 行 `rows_affected=1`）。 |
| M2-c | 单独删除同一 UPDATE 的 `child_wave_id IS NOT NULL`。 | 同一测试 **红**（1 failed，无 child 行 `rows_affected=1`）。 |
| M3 | 把抽出的 success flip 中 child-Done `EXISTS` 真谓词改成 `AND 1=1`，不保留源码注释 oracle。 | `acceptance_18_success_flip_rechecks_done_after_its_snapshot` **红**（1 failed；同一 IMMEDIATE tx 先读 Done、再删 child 后 guarded flip 错误命中 1 行）。 |
| M4-a | child adapter 提交内单独把 fresh child 改成 `lifecycle='planning'`。 | `acceptance_5_child_seed_uses_all_four_frozen_fields_and_parent_cwd` **红**（1 failed，期望 `draft`）。 |
| M4-b | 同一位置单独写 `archived_at=101`。 | 同一测试 **红**（1 failed，期望 NULL）。 |
| M4-c | 同一位置单独写 `pinned_at=102`。 | 同一测试 **红**（1 failed，期望 NULL）。 |
| M4-d | 同一位置单独写 `terminal_at=103`。 | 同一测试 **红**（1 failed，期望 NULL）。 |
| #21c | 把 parent cove reader 改成选取表内另一个 cove，使真实 child adapter 写出跨-cove 边。 | `acceptance_21c_real_adapter_never_writes_a_cross_cove_edge` **红**（1 failed，跨-cove 全表计数为 1）；该测试不再 raw SQL 自造被测边。 |

本节所有变异均已复原；恢复态由后续定向基线与 §9 全门再次验证。

## 修复轮 2

以下变异均在 `NEIGE_CODEX_BIN` 未设置、`CARGO_BUILD_JOBS=6`、PATH 含指定
`.local-bin` 的目标分支 worktree 实际执行。每次只改一个承重点，取得红灯后立即用反向
`apply_patch` 复原；`0071_sub_wave_tree.sql` 的 CASCADE 变异也已复原，未修改已发布迁移。

| 修复项 | 我改坏了什么 | 对应测试与实际结果 |
|---|---|---|
| B1-a（writer 槽） | 在 phase-1 marker 写事务内、`wave_mark_deleting_tx` 后插入 6s sleep。 | `wave_delete_external_teardown_does_not_hold_the_sqlite_writer` **红**（1 failed，1.116s）：1s 硬 barrier 等不到 marker commit / 锁外 teardown 入口；不是 nextest slow-timeout。恢复实现还在锁外 barrier 期间用 250ms 硬 timeout 断言无关 `cove_create` 成功。 |
| B1-b（拒删全不变） | 从 `wave_mark_deleting_tx` 删除 leaf fence。 | `acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal` **红**（1 failed，0.398s）：terminal 进程已改变；同一 fixture 还守护 registry/socket/四张 DB 表。 |
| B1-c（崩溃恢复） | 从既有 terminal sweep 删除 `resume_marked_wave_deletions`。 | `durable_wave_delete_marker_refuses_new_resources_and_restart_sweep_finishes` **红**（1 failed，2.175s）：仅提交 marker、丢弃 AppState 并重建后，terminal 进程仍存；fixture 在失败时强制回收测试进程，未留泄漏。 |
| B1-d（快照后的 operation） | 令 operation driver 无条件跳过 `wave_deletions` 外部 IO fence。 | `deleting_wave_fails_queued_bootstrap_before_external_io` **红**（1 failed，0.132s）：queued bootstrap 从预期 `Failed('wave-deleting')` 变成 `Succeeded`。marker 提交与 operation external phase 还由同一个短暂 drive fence 串行，fence 在 process/socket teardown 前释放。 |
| B2-success | 单独把 `guarded_child_success_flip_tx` 的 child-Done `EXISTS` 改成 `1=1`。 | `acceptance_18_success_flip_rechecks_done_after_its_snapshot` **红**（1 failed，0.121s）：delete 交错错误更新 1 行。 |
| B2-incomplete | 单独把 `guarded_child_incomplete_flip_tx` 的 child-Done `EXISTS` 改成 `1=1`。 | `acceptance_18_incomplete_flip_rechecks_done_after_its_snapshot` **红**（1 failed，0.110s）：delete 交错错误更新 1 行。两个原 oracle 站点至此各有独立行为断言。 |
| M1（#21c loud） | 临时把 0071 self-FK 改成 `ON DELETE CASCADE`。 | `acceptance_21c_cross_cove_edge_is_a_loud_delete_tripwire` **红**（1 failed，0.113s）：raw SQL 毒边下 `cove_delete_tx` 错误成功；真实 adapter 的全表零错边测试同时保留。 |
| M2（确定性 barrier） | 把 parent `mark_sub_wave_running` 提到 bootstrap `wait` 前，成功臂改成 `Ok(())`。 | `acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive` **红**（1 failed，0.168s）：`wait_entered` happens-before 后稳定观测到错误的 `Running`；无 25ms sleep。 |

所有 mutant 均已复原。复原态扩大定向集合（wave-delete / durable recovery / #20 /
#13d / 两处 #18 / #19 / queued-operation fence / 两条 #21c / child adapter #5/#6）为
**32 passed / 0 failed / 3401 skipped，0.456s**。

## 修复轮 3

本轮按裁决删除从未发布的 0072 两阶段持久删除，不再对 marker 状态机做变异。
以下命令均保持 `NEIGE_CODEX_BIN` 未设置、`CARGO_BUILD_JOBS=6`，Rust PATH 含指定
`.local-bin`；每个单点 mutant 拿到红灯后均用反向补丁复原。

| 修复项 | 我改坏了什么 | 对应测试与实际结果 |
|---|---|---|
| route descendant 前检 | 给 route 的普通读查询加 `AND 0`，仅保留最终事务 guard。 | `acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal` **红**（1 failed，0.406s）：最终 409 前 terminal 已被杀，命中 `terminal process changed`；同 fixture 还守 process/registry/socket/DB。 |
| `wave_delete_tx` 权威 guard | 在 `wave_delete_tx` 跳过 `wave_require_leaf_tx`。 | `acceptance_20_repo_wave_delete_refuses_descendant_and_names_it` **红**（1 failed，0.129s）：直调退化成 FK 错误且不再指名 child id。 |
| 写事务内无外部 IO | 把确定性 teardown hook 从写事务前搬进 `write_with_actor_events_typed` 闭包。 | `wave_delete_external_teardown_does_not_hold_the_sqlite_writer` **红**（1 failed，0.359s）：无关 writer 在 250ms barrier 超时。 |
| card owner NotFound（0072 删除审计） | 初版降范围随 `wave_require_not_deleting_tx` 一起删掉了它顺带承担的 owner-wave existence check。 | 首次全 nextest 中 `card_with_codex_create_tx_rolls_back_on_invalid_wave` 与 `card_with_terminal_create_tx_rolls_back_on_invalid_wave` **2 条红**（3341 passed / 2 failed，34.018s）：错误不再是 typed `NotFound`。恢复为不含 marker 语义的纯 existence check。 |
| child-flip success | 把 `guarded_child_success_flip_tx` 的 child-Done 复核改成 `1=1`。 | `acceptance_18_success_flip_rechecks_done_after_its_snapshot` **红**（1 failed，0.111s）：delete 交错错误命中 1 行。 |
| child-flip incomplete | 把 `guarded_child_incomplete_flip_tx` 的 child-Done 复核改成 `1=1`。 | `acceptance_18_incomplete_flip_rechecks_done_after_its_snapshot` **红**（1 failed，0.122s）：delete 交错错误命中 1 行。 |
| child-flip terminal（共用 helper） | 删除 `guarded_child_terminal_flip_tx` 的 outcome guard。 | `acceptance_18_terminal_flip_rechecks_all_three_outcomes_after_its_snapshot` **红**（1 failed，0.119s）：Deleted 后复活的 child 仍错误关闭 parent。 |
| child-flip terminal/Failed | 只把 Failed outcome guard 改成 `1=1`。 | 同一三理由 oracle **红**（1 failed，0.227s）：Failed snapshot 后 reopen 仍错误命中 1 行。 |
| child-flip terminal/Canceled | 只把 Canceled outcome guard 改成 `1=1`。 | 同一三理由 oracle **红**（1 failed，0.352s）：Canceled snapshot 后 reopen 仍错误命中 1 行。 |

恢复态定向集合覆盖 success / incomplete / terminal 三个复核点、route / Repo / cove
三个删除入口、拒删全不变和锁外 teardown：**8 passed / 0 failed / 3424 skipped，0.435s**。
本节没有仍绿变异，所有 mutant 均已复原。
