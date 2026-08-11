# #1049 变异映射

实测日期：2026-08-11。下列变异均临时应用到生产代码，取证后恢复；命令均在
`1049-net-transfer` 工作树执行，且使用共享 target 与指定 PATH。

## 结果总表

| 变异 | 承重测试 | 实测 |
|---|---|---|
| M1：冻结树也允许同 key transfer | `over_share_same_key_declarations_do_not_unfreeze_sibling_growth` | RED，exit 100 |
| M2：彻底禁用 transfer | `full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory` | RED，exit 100 |
| M3：所有干净候选都算 transfer | 同上 | RED，exit 100 |
| M4：freeze 边界 `fixed > share` 改为 `>=` | 同上 | RED，exit 100 |
| F1-a：transfer 同时免 ceiling | `legacy_pending_transfer_is_rejected_at_zero_ceiling_with_a_diagnostic` | RED，exit 100 |
| F1-b：ceiling 拒绝 transfer 但不产诊断 | 同上 | RED，exit 100 |
| M5：whole-tree rebuild 固定非冻结 verdict | `frozen_whole_tree_rebuild_does_not_admit_sibling_declarations` | RED，exit 100 |

## 实跑证据

### M1：`transfers_allowed = true`

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-truth -E 'test(over_share_same_key_declarations_do_not_unfreeze_sibling_growth)'
FAIL calm-truth db::sqlite::wave_tree_budget_tests::over_share_same_key_declarations_do_not_unfreeze_sibling_growth
panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1588:5:
an over-share member must not transfer
```

### M2：`transfers_allowed = false`

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-truth -E 'test(full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory)'
FAIL calm-truth db::sqlite::wave_tree_budget_tests::full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory
panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1382:5:
all same-key declarations must transfer
```

### M3：`let transfer = transfers_allowed`（删除 legacy key 匹配）

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-truth -E 'test(full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory)'
FAIL calm-truth db::sqlite::wave_tree_budget_tests::full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory
panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1416:5:
assertion failed: !outcome.diagnostics[3].schedulable
```

### M4：freeze 边界由 `fixed > share` 改为 `fixed >= share`

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-truth -E 'test(full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory)'
FAIL calm-truth db::sqlite::wave_tree_budget_tests::full_share_legacy_pending_same_key_declarations_transfer_without_new_inventory
panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1382:5:
all same-key declarations must transfer
```

### F1-a：transfer 不检查且不消耗 ceiling slot

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-truth -E 'test(legacy_pending_transfer_is_rejected_at_zero_ceiling_with_a_diagnostic)'
FAIL calm-truth db::sqlite::wave_tree_budget_tests::legacy_pending_transfer_is_rejected_at_zero_ceiling_with_a_diagnostic
panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1464:5:
assertion failed: !outcome.diagnostics[0].schedulable
```

### F1-b：被 ceiling 挡住的 legacy transfer 跳过诊断 push

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-truth -E 'test(legacy_pending_transfer_is_rejected_at_zero_ceiling_with_a_diagnostic)'
FAIL calm-truth db::sqlite::wave_tree_budget_tests::legacy_pending_transfer_is_rejected_at_zero_ceiling_with_a_diagnostic
panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1465:5:
left: []; right: ["spec_task_ceiling"]
```

### M5：rebuild 的 term 固定 `admission_frozen=false`

```text
RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH cargo nextest run -p calm-server --features calm-server/codex-e2e -E 'test(frozen_whole_tree_rebuild_does_not_admit_sibling_declarations)'
FAIL calm-server operation::child_wave_adapter::tests::frozen_whole_tree_rebuild_does_not_admit_sibling_declarations
panicked at crates/calm-server/src/operation/child_wave_adapter.rs:1199:9:
assertion `left == right` failed: a frozen rebuild must not admit a sibling declaration; left: 1; right: 0
```
