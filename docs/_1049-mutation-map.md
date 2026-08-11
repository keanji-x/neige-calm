# #1049 变异映射

实测日期：2026-08-11。每条变异均只在本地临时应用；取证后已恢复正确实现。

## M1：同 key 无条件从净新增容量中豁免

- 改坏点：将 `task_projection.rs` 的
  `transfers_allowed = !tree_root_unresolved && !tree_admission_frozen`
  临时改成 `transfers_allowed = true`。这等价于不顾任一成员已经
  over-share，只要同 key 命中 legacy pending 就无条件按 transfer 减免。
- 必须红：
  `over_share_same_key_declarations_do_not_unfreeze_sibling_growth`。
- 实测命令：

  ```text
  RUSTC_WRAPPER= CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target \
    PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH \
    cargo nextest run -p calm-truth \
    -E 'test(over_share_same_key_declarations_do_not_unfreeze_sibling_growth)'
  ```

- 实测输出片段（exit 100）：

  ```text
  FAIL calm-truth db::sqlite::wave_tree_budget_tests::over_share_same_key_declarations_do_not_unfreeze_sibling_growth
  panicked at crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1468:5:
  an over-share member must not transfer
  Summary: 1 test run: 0 passed, 1 failed, 403 skipped
  ```

该红灯证明安全测试不是只观察最终总数：错误实现一旦把 over-share
legacy pending 翻入可裁的 block pending 桶，就在兄弟增长之前被直接检出。
