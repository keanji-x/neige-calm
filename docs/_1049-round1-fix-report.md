# #1049 第 1 轮修复报告

日期：2026-08-11。基线提交：`6476f3e6`。

## 三条必修

1. F1：`task_projection.rs` 不再把 ceiling/tree 压成单一 `capacity`。干净候选按
   `(block_index, key)` 单次遍历；transfer 扣 1 个 ceiling slot、0 个 tree slot，net-new
   各扣 1 个。拒绝项记录实际被哪条腿挡住；ceiling 拒绝的 transfer 明确产生
   `spec_task_ceiling`，不再依赖 net-new 的尾部循环。
2. 弱 oracle：`wave_tree_budget_tests.rs` 将两腿拆开。新增 `ceiling=0` 反例、
   `K=3/ceiling=2/tree=8` 的按文档顺序部分收编，并把 full-share 改成
   `tree=3/ceiling=8`，分别覆盖 ceiling 紧/tree 松与 tree 紧/ceiling 松。
3. M5：选择方案 (a)，保留 whole-tree rebuild 的真实 fixed inventory 冻结 verdict。
   新增 `frozen_whole_tree_rebuild_does_not_admit_sibling_declarations`：父 wave 有 2 条
   legacy pending、B=2，空子 wave 有未物化声明；失败事务内断言子 wave 仍 0 行、
   整树 live 仍为 B。理由：冻结 verdict 决定后置 Conflict 前是否产生兄弟行，已有明确
   可观察的事务内状态；固定 `admission_frozen=false` 时实测子 wave 变为 1 行。

## 变异实测

| 变异 | 结果 / 承重测试 |
|---|---|
| M1 冻结树也允许 transfer | RED：over-share sibling growth |
| M2 禁用全部 transfer | RED：full-share transfer |
| M3 所有候选都算 transfer | RED：distinct key 被接纳 |
| M4 `fixed >= share` 即冻结 | RED：exact-full 被冻结 |
| F1-a transfer 也免 ceiling | RED：ceiling=0 transfer 被接纳 |
| F1-b ceiling 拒绝但无诊断 | RED：期望 `spec_task_ceiling`，实际 `[]` |
| M5 rebuild 固定非冻结 | RED：冻结子 wave 由 0 行变 1 行 |

完整命令、失败测试名与 panic 行见 `_1049-mutation-map.md`；所有临时变异均已恢复。

## 硬门

| 门 | 结果 |
|---|---|
| `legacy_live_spec_consumes_tree_share_until_it_terminates` | PASS |
| `legacy_member_overage_freezes_new_blocks_across_the_tree` | PASS |
| `whole_tree_live_spec_never_exceeds_budget_across_admitted_growth_sequences` | PASS |
| `child_creation_409s_when_inflight_member_exceeds_its_new_share` | PASS |

四条 D.4 #7 测试期望一字未改，定向合跑 4/4 PASS。其余本地门：
`cargo fmt --all --check` PASS；workspace clippy（all-targets、codex-e2e、`-D warnings`）
PASS；calm-truth/calm-server 关键词定向 nextest 89/89 PASS。未跑全工作区 nextest。

## 不确定处

无。
