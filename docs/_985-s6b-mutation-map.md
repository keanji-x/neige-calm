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

## 五、修复轮 1（双通道阻塞集，全部实际执行并复原）

环境同上：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web
变异使用 Node 22.22.2。每条均为单点补丁，目标测试结束后立即反向 `apply_patch` 复原。

### 5.1 实测红

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R1-B1a | `can_add_tree_member` 恒返回 true | `wave_tree::tests::enforcement_points_are_compatible_for_every_budget_and_member_count` | **RED**：`B=0, N=2` 出现零份额 |
| R1-B1b | adapter 的成员数准入条件改成恒 false（保留库存准入） | `child_wave_adapter::tests::acceptance_tree_budget_never_admits_a_zero_share_member` | **RED**：第二个 child 被实际创建，`unwrap_err` 拿到 `Ok` |
| R1-B1c | 孤根显式预算仍返回 `NotInTree` | `wave_tree_budget_tests::an_explicit_budget_applies_to_a_singleton_root` | **RED**：B=1 仍准入 3/3 |
| R1-B2 | 从 `WAVE_TREE_MEMBERS_SQL` 删除整句 `ORDER BY w.created_at, w.id` | `wave_tree::tests::quota_member_sql_keeps_its_total_order_definition` | **RED**：排序定义缺席 |
| R1-B3 | 新增 rustfmt 后仍保持单行的 `pub const X: &str = concat!(bounded_wave_descendant_cte!(), "");`，只 re-export、不登记 | `wave_tree::tests::every_bounded_tree_cte_expansion_is_registered` | **RED**：AST 实际集合多出 `X`，零 warning |
| R1-M1a | Rust 正份额文案恢复为“等树里别处任务完成” | `wave_tree_budget_tests::over_share_declarations_are_diagnosed_against_the_root_wave` | **RED** |
| R1-M1b | Rust 把 `share==0` 专用分支改成不可达 | `wave_tree_budget_tests::zero_share_diagnostic_explains_the_shape_and_effective_actions` | **RED** |
| R1-M1c | web 正份额文案恢复为“wait for tasks elsewhere” | `report-blocks.test.tsx` 的 `gives tree_budget_exhausted a human explanation and next action` | **RED**：54 中 1 failed |
| R1-M1d | web 把 `share===0` 专用分支改成不可达 | 同上 | **RED**：54 中 1 failed |
| R1-M2 | 删除 Rust `tree_root_unresolved` 整个渲染臂 | `calm-types::tree_root_unresolved_always_has_human_copy_and_a_next_action` | **RED**：message 为空 |
| R1-M3 | `RootUnresolved` 恢复成直接 `return Ok(Vec::new())` | `wave_tree_budget_tests::unresolved_root_preserves_withdrawal_and_deleted_block_read_verdicts` | **RED**：撤回 verdict 集为空 |
| R1-M4 | 成员集合的 `over_deep` 恒置 false | `wave_tree_budget_tests::an_over_deep_chain_fails_closed` | **RED**：从中毒树 root 发起得到 Share |
| R1-M5 | caller 不在成员集合时用 `.or(Some(0))` 伪造下标 | `wave_tree::tests::a_resolved_member_set_that_omits_the_caller_fails_closed` | **RED**：错误返回 Share |
| R1-M6 | `projection_policy_changed` 删除 `tree_task_budget` | `wave_projection_policy_patch::tightening_tree_budget_immediately_deletes_pending_projection_and_emits_plan_updated` | **RED**：pending 行仍为 1 |
| R1-M7 | 升级日手工 head-schema fixture 省略既有 migration 0072 | `migration_0068_projection_policy::migration_backfills_preexisting_task_and_block_declaration_adopts_it` | **RED**：`no such column: w.tree_task_budget`（全 workspace 门实测）；补应用 0072 后目标 1/1 |

### 5.2 实测仍绿（不隐藏）

| # | 变异 | 目标测试 | 结果与处置 |
|---|---|---|---|
| R1-B2-pre | 删除 `ORDER BY` 整句后，只跑两条反插入序 / 同时间戳行为用例 | `quota_remainder_follows_created_at_not_insertion_order` + `quota_remainder_breaks_equal_created_at_ties_by_id` | **STILL-GREEN（2/2）**。当前 SQLite 的 `GROUP BY id` 临时给出了同一顺序，行为用例仍与查询计划共用事实来源。因此保留它们覆盖方向/tie-break，并新增直接守住 SQL 定义存在性的 R1-B2；在同一未复原变异上，R1-B2 已实测红。 |

### 5.3 覆盖替换审计

- 默认孤根“零递归”原性质仍由 `a_non_tree_wave_runs_zero_recursive_tree_queries` 购买；新 N=1
  用例只收紧显式预算分支，没有删掉原短路覆盖。
- 原 `RootUnresolved` fail-closed 由既有“所有声明不可调度”继续购买；新实现去掉早退后，withdrawal
  边沿与已删块合成 verdict 分别由 R1-M3 的同一交错购买。
- 原登记清单的“删一项会红”没有丢：AST 实际集合与登记名集合做**集合相等**，两个方向都比较；
  R1-B3 专门购买过去缺失的“新增不登记会红”，且不依赖排版。
- 配额顺序的方向和同毫秒 id tie-break 仍由行为用例购买；`ORDER BY` 的**存在性**由结构断言购买。
- 点一的库存 `>=` 既有 M7 保留；新增成员上界由 adapter 交错购买；两者组合不产生零份额由
  `B,N` 性质测试购买。
