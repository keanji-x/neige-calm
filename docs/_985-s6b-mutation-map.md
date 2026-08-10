# #985 切片 6 PR-B —— 变异映射（全部**实际执行过**）

> 规矩同 PR-A：**没跑过的不写**。每条给出「改了哪个文件的哪个表达式 → 哪个测试全名红了」。
> 仍绿的照实记，并说明为什么它仍绿。
> 执行方式：脚本逐条打补丁 → 只跑目标测试 → `git checkout --` 复原。
> 交付前 `git status --short` 干净（仅剩本轮的正常改动）。

跑法：
- `calm-truth` 单测：`cargo test -p calm-truth --lib <name>`
- `calm-server`：`cargo nextest run -p calm-server -E 'test(<name>)'`

## 一、必红且**已实测红**（12 条）

| # | 变异（改坏什么） | 目标测试全名 | 结果 |
|---|---|---|---|
| M1 | `task_projection.rs` 的 `WaveTreeTerm::Share(share) => (ceiling.min(share.share), …)` 换成**共享计数**：`ceiling.min(share.budget - 全树其它 wave 的非终结 spec 行数)` | `db::sqlite::wave_tree_budget_tests::two_rebuild_orders_over_one_tree_agree_byte_for_byte` | **RED** |
| M2 | `wave_tree.rs` 的 `bounded_wave_descendant_cte!` 删掉 `WHERE down.depth <= ?2`（唯一终止装置） | `db::sqlite::wave_tree::tests::bounded_tree_sql_keeps_its_only_cycle_termination_guard` | **RED** |
| M3 | `BOUNDED_WAVE_TREE_SQL` 里删掉 `WAVE_TREE_MEMBERS_SQL`（模拟「新增 CTE 漏登记」） | `db::sqlite::wave_tree::tests::every_bounded_tree_cte_expansion_is_registered` | **RED** |
| M4 | `RootUnresolved` 臂从 fail-closed 改成「没有树就跳过树项」（`(ceiling, None)`） | `db::sqlite::wave_tree_budget_tests::unresolvable_root_fails_closed_for_every_declaration` | **RED** |
| M5a | migration `0072_` 的列加上 `DEFAULT 32`（照 `spec_task_ceiling` 的旧形状） | `db::sqlite::wave_tree_budget_tests::every_created_wave_lands_a_null_tree_task_budget`（`dflt_value` 断言） | **RED** |
| M5b | 在 M5a 之上再把 `wave_create_tx` 固定列清单里的 `tree_task_budget`/`NULL` 删掉 | 同上（子 wave 实测 `Some(32)`，即「每个子 wave 各拿一份预算」） | **RED** |
| M6 | `wave_update_tx` 删掉 root-only 守卫（保留 UPDATE） | `db::sqlite::wave_tree_budget_tests::tree_task_budget_patch_on_a_child_is_refused_by_the_shared_writer` | **RED** |
| M7 | 强制点一的 `inventory >= budget` 改成 `>`（差一） | `operation::child_wave_adapter::tests::acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full` | **RED** |
| M9 | `wave_tree_term` 删掉非树 wave 的短路（永远走递归遍历） | `db::sqlite::wave_tree_budget_tests::a_non_tree_wave_runs_zero_recursive_tree_queries` | **RED** |
| M10 | 树上界的诊断码从 `tree_budget_exhausted` 换回 `spec_task_ceiling`（跨 wave 归因被抹掉） | `db::sqlite::wave_tree_budget_tests::over_share_declarations_are_diagnosed_against_the_root_wave` | **RED** |
| M11 | `deterministic_share` 去掉余数分配（只留 `floor(B/N)`） | `db::sqlite::wave_tree_budget_tests::shares_over_a_real_tree_sum_to_the_budget` | **RED** |
| M12 | `routes/waves.rs` 的 `patch_has_other_changes` 去掉 `tree_task_budget`（纯该列的 PATCH 被当空补丁短路） | `wave_projection_policy_patch::tree_task_budget_patch_matches_the_spec_task_ceiling_surface` | **RED** |
| M13 | 同文件 user-only 闸去掉 `tree_task_budget` | 同上 | **RED**（见下方「一次被证伪的恒真断言」） |

（M13 与 M12 目标测试相同，故上表 13 行、12 个不同变异点。）

### M13 值得单独记：它第一次跑出来是 **STILL-GREEN**

初版断言只写 `assert_eq!(status, FORBIDDEN)`。删掉 user-only 闸后它**仍绿** —— 因为
`X-Calm-Actor: ai:codex` 经 REST 进来时 `to_actor_id()` 造出的是**空 card id** 的
`AiCodex`，下游另有一条 403（`"AiCodex/AiClaude/AiSpec actor has empty card id"`）。
即：**这条断言测的根本不是我这道闸**。修法是断言**理由**而不是状态码 ——
现在断言响应体同时含 `tree_task_budget` 与 `user-only`，M13 才变红。

> 顺带如实登记：既有的 `non_user_policy_patches_are_forbidden_without_rows_or_events`
> （`spec_task_ceiling` / `automation_policy` 那条，PR-B 之前就有）是**同一个恒真形状**，
> 删掉它那半道闸大概率也仍绿。**本 PR 没有改它**（不扩范围），登记在
> `docs/_985-s6b-impl-notes.md` 的「设计缺口 / 已知遗留」里。

## 二、实测**仍绿**（1 条，如实记）

| # | 变异 | 目标测试 | 结果与原因 |
|---|---|---|---|
| M5c | 只把 `wave_create_tx` 固定列清单里的 `tree_task_budget`/`NULL` 删掉（**不动 migration**） | `every_created_wave_lands_a_null_tree_task_budget` | **STILL-GREEN**。因为 `0072_` 刻意**不给 DB DEFAULT**，省略该列插入本来就是 NULL —— 两道防线**冗余**，单独去掉任一道都观察不到。判别力由 M5a/M5b 提供：M5a 打掉「列无 DEFAULT」这道，M5b 在此之上再打掉显式写入那道，才暴露出「每个子 wave 各拿一份 32」。**不改测试去迎合**：冗余是刻意的（fork D11 的两条落点都留着），恒真的那半由 M5a/M5b 覆盖。 |

## 三、未实现 / 未做变异（0 条未实现，2 条未做变异并说明）

- **未实现的交付项：0 条。**
- 未单独做变异的点，各有覆盖它的变异：
  1. 「整个强制点一删掉」—— 由 M7（差一）覆盖同一条断言；再做一遍是同一测试的第二次红。
  2. 「向下 CTE 携带非 id 列」—— 无法只靠删一行制造（要重写 CTE 与外层 JOIN）；
     它由 M2（删截断）+ `every_bounded_tree_cte_expansion_is_registered`（登记制）
     共同把守，且宏本身是**唯一**产出点（源码扫描断言展开数 == 登记数）。

## 四、这些变异**共同**买到的性质

1. **树项不读投影产物** —— M1 是它的直接证伪装置：任何「数兄弟行」的写法都会让两种
   rebuild 序分叉。
2. **环上必然终止的唯一装置是 depth 截断** —— M2。`UNION ALL`↔`UNION` 在 PR-A 已实证
   换不出区别，所以本片不再拿它当变异。
3. **登记制有机器联系** —— M3。名单与成员之间不是「人记得加」，源码扫描会红。
4. **fail-closed 不是可选项** —— M4。
5. **单一真源** —— M5a/M5b/M6：列没有 DEFAULT、每条建 wave 路径显式写 NULL、
   root-only 守卫落在共用 in-tx writer（不是 route）。
6. **跨 wave 归因说得出口** —— M10。
7. **写入面真的通电** —— M12/M13。
