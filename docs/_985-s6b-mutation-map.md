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
| M2 | `wave_tree.rs` 的 `bounded_wave_descendant_cte!` 删掉 `WHERE down.depth <= ?2`（唯一终止装置） | `every_recursive_parent_wave_cte_in_workspace_members_bounds_its_recursive_variable` | **RED；R4 已把判据升级为全 workspace、逐 CTE 递归成员/alias 绑定/合取项性质门** |
| M3 | `BOUNDED_WAVE_TREE_SQL` 里删掉 `WAVE_TREE_MEMBERS_SQL`（模拟「新增 CTE 漏登记」） | 原 `every_bounded_tree_cte_expansion_is_registered` | **历史 RED，登记表与该实例变异已退休。** 当前等价风险由 R2-B2a-d 的四类“新增无界 CTE”性质变异覆盖。 |
| M4 | `RootUnresolved` 臂从 fail-closed 改成「没有树就跳过树项」（`(ceiling, None)`） | `db::sqlite::wave_tree_budget_tests::unresolvable_root_fails_closed_for_every_declaration` | **RED** |
| M5a | migration `0072_` 的列加上 `DEFAULT 32`（照 `spec_task_ceiling` 的旧形状） | `db::sqlite::wave_tree_budget_tests::every_created_wave_lands_a_null_tree_task_budget`（`dflt_value` 断言） | **RED** |
| M5b | 在 M5a 之上再把 `wave_create_tx` 固定列清单里的 `tree_task_budget`/`NULL` 删掉 | 同上（子 wave 实测 `Some(32)`，即「每个子 wave 各拿一份预算」） | **RED** |
| M6 | `wave_update_tx` 删掉 root-only 守卫（保留 UPDATE） | `db::sqlite::wave_tree_budget_tests::tree_task_budget_patch_on_a_child_is_refused_by_the_shared_writer` | **RED** |
| M7 | 强制点一的 `inventory >= budget` 改成 `>`（差一） | `operation::child_wave_adapter::tests::acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full` | **RED（修复轮 2 重新成立）**。修复轮 1 加入成员守卫后，旧 B=1 fixture 会先撞成员上界，曾让这条变异 **STILL-GREEN**；R2 改为 `B=2, inventory=2, N+1=2` 后重跑，`unwrap_err()` 收到 `Ok(TxOutput)`，库存守卫独立红。 |
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
  2. 「向下 CTE 携带非 id 列」—— 无法只靠删一行制造（要重写 CTE 与外层 JOIN）；环上终止
     不再归因于已删除的登记门，而由 M2/R3-SQL3 直接购买：递归成员必须约束其**递归 alias 自己的**
     `depth`。即使后来多携带列，显式深度上界仍是终止真因；“只携带 id+depth”保留为查询形状约束。

## 四、这些变异**共同**买到的性质

1. **树项不读投影产物** —— M1 是它的直接证伪装置：任何「数兄弟行」的写法都会让两种
   rebuild 序分叉。
2. **环上必然终止的唯一装置是 depth 截断** —— M2。`UNION ALL`↔`UNION` 在 PR-A 已实证
   换不出区别，所以本片不再拿它当变异。
3. **生产执行面都受性质门约束** —— R3-SQL1/SQL2：`calm-truth`、`calm-server` 的生产 Rust
   字符串与 crate 内 `.sql` 都被扫描，不再依赖登记名单。
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
| R1-B2 | 从 `WAVE_TREE_MEMBERS_SQL` 删除整句 `ORDER BY w.created_at, w.id` | `wave_tree::tests::quota_member_sql_keeps_its_total_order_definition` | **历史 RED；R2 已升级并重验**：旧门可被注释满足；现门先剥 SQL 注释，行为夹具也固定成 id 顺序与 created_at 顺序相反。只留同文注释时两门都 RED。 |
| R1-B3 | 新增 rustfmt 后仍保持单行的 `pub const X: &str = concat!(bounded_wave_descendant_cte!(), "");`，只 re-export、不登记 | 原 `wave_tree::tests::every_bounded_tree_cte_expansion_is_registered` | **历史 RED，但实例门已删除并由 R2-B2a-d 取代。** 该 AST 枚举只覆盖顶层 const + concat，不能证明「不存在无界递归 CTE」；当前门不再维护登记表。 |
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
| R1-B2-pre | 删除 `ORDER BY` 整句后，只跑两条反插入序 / 同时间戳行为用例 | `quota_remainder_follows_created_at_not_insertion_order` + `quota_remainder_breaks_equal_created_at_ties_by_id` | **修复轮 1 当时 STILL-GREEN（2/2），该结论已过期。** R2 固定 id 与 created_at 反序后，同一删除/注释变异使第一条 RED（第二条继续守 tie-break）；见 R2-M2。 |

### 5.3 覆盖替换审计

- 默认孤根“零递归”原性质仍由 `a_non_tree_wave_runs_zero_recursive_tree_queries` 购买；新 N=1
  用例只收紧显式预算分支，没有删掉原短路覆盖。
- 原 `RootUnresolved` fail-closed 由既有“所有声明不可调度”继续购买；新实现去掉早退后，withdrawal
  边沿与已删块合成 verdict 分别由 R1-M3 的同一交错购买。
- 登记清单与 AST 枚举已删除。当前门扫描 `calm-truth` + `calm-server` 的 `src/**/*.rs` 与两个
  crate 内全部 `.sql`（覆盖 migrations / `include_str!` / `query_file!` 的常规 `.sql` 载体）；Rust
  每个 literal 独立检查，仅对真实 literal-only `concat!` 合并。判据限定到单个 CTE 的递归成员：
  该成员同时自引用并触及 `parent_wave_id` 时，ON/WHERE 必须上界约束递归 CTE alias 的 `depth`。
- 配额顺序的方向由固定 id/created_at 反序行为用例购买，同毫秒 id tie-break 由第二条行为用例购买；
  真正 `ORDER BY` 子句的存在性由剥离 SQL 注释后的结构断言购买。
- 点一的库存 `>=` 与成员上界现在各有对方明确放行的 adapter 交错；两者组合的 soundness 与
  边界允许性由 `can_add == every share > 0` 的双向性质购买。

## 六、修复轮 2（r2 双通道收敛项，全部实际执行并复原）

| # | 改坏什么 | 红的测试 | 实测结果 |
|---|---|---|---|
| R2-B1 | 孤根短路恢复为 `tree_task_budget IS NULL`，不比较同源有效 B 与 ceiling | `resetting_an_explicit_budget_to_null_keeps_the_default_bound` | **RED**：PATCH 回 NULL 后 40/40 schedulable，期望 32 |
| R2-B2a | 在内联 `mod` 放一条无 depth 的递归 parent-wave SQL | `every_recursive_parent_wave_cte_in_workspace_members_bounds_its_recursive_variable` | **RED**：报告该 SQL 无 depth bound；R3 与 b/c/d 捆绑重跑，四条分别报告 |
| R2-B2b | 同一无界 SQL 改由包装宏生成 | 同上 | **RED** |
| R2-B2c | 同一无界 SQL 写成 `static` | 同上 | **RED** |
| R2-B2d | 同一无界 SQL 写成块表达式 `const` | 同上 | **RED** |
| R2-M1a（重跑旧 M7） | 库存 `inventory >= budget` 改为 `>` | `acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full` | **RED**：`B=2, inventory=2, N+1=2` 下错误放行；修复轮 1 后曾被成员守卫遮蔽的证据已恢复 |
| R2-M1b | 删除成员上界守卫 | `acceptance_tree_budget_never_admits_a_zero_share_member` | **RED**：`B=2, inventory=1, N+1=3` 下错误放行；库存守卫明确不触发 |
| R2-M1c | 把成员拒绝移到 child wave skeleton 写入之后 | 同上 | **RED**：同一未回滚 tx 内 waves 从 2 变 3，证明零写入断言不再依赖 `drop(tx)` 回滚 |
| R2-M2 | 删除真实 `ORDER BY`、只留 `/* ORDER BY w.created_at, w.id */` | `quota_member_sql_keeps_its_total_order_definition` + `quota_remainder_follows_created_at_not_insertion_order` | **RED + RED**：注释不能满足结构门；固定 id/时间反序让行为不依赖查询计划 |
| R2-M3a | Rust 动作契约把 tree 旋钮改为 `raise_spec_task_ceiling` | web `keeps capacity copy aligned with the Rust recovery-action contract` | **RED**：期望 `raise_tree_task_budget` |
| R2-M3b | web 正份额文案把 “top wave” 改成 “wave settings” | 同上 | **RED**：文案不再指向契约旋钮 |
| R2-m1 | `can_add_tree_member` 的边界从 `<=` 改为 `<` | `enforcement_points_are_compatible_for_every_budget_and_member_count` | **RED**：`B=2, after N=2` 本应允许却拒绝 |
| R2-m2 | tree 诊断归因的 `share <= ceiling` 恢复严格 `<` | `an_equal_tree_share_reports_the_tree_knob` | **RED**：`share=ceiling=32` 错归 `spec_task_ceiling` |
| R2-m3 | migrations 目录新增 `0073_r2_probe.sql`，不更新升级日 fixture | `head_schema_fixture_lists_every_migration_from_0068_through_head` | **RED**：集合右侧多出 0073 |
| R2-m4 | web 缺 share 时恢复严格 `share === 0` | web `gives tree_budget_exhausted a human explanation and next action` | **RED**：渲染出空的 “this wave can hold .” 分支 |

复原审计：上述临时 SQL、宏、static、migration 0073、条件与文案变异均已删除/恢复；最终定向门
再次全绿后才运行 §9 全门。

## 七、修复轮 3（r3 双通道阻塞集，全部实际执行并复原）

环境：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web 使用
Node 22.22.2。每个临时实现/SQL/文案补丁均用反向 `apply_patch` 复原。

| # | 改坏什么 | 红的测试 | 实测结果 |
|---|---|---|---|
| R3-B1a | singleton shortcut 的 ceiling 改回裸 `Option` 的 `NULL→0` | `a_null_ceiling_and_tiny_budget_still_bind_a_singleton_root` | **RED**：5 条全部 schedulable，期望 1 |
| R3-B1b | 有效 B 的 NULL 默认从 32 改成 31（不再与统一解析契约一致） | `resetting_an_explicit_budget_to_null_keeps_the_default_bound` | **RED**：31 条 schedulable，期望 32 |
| R3-B2 | 根预算 PATCH 退回只重投影根（成员枚举分支强制关闭） | `tightening_root_tree_budget_culls_descendant_pending_before_it_can_be_claimed` | **RED**：子 pending count 仍为 1，未到 claim 断言即失败 |
| R3-SQL1 | 在 `calm-truth/src` 注入无界 parent-wave 递归 CTE | `every_recursive_parent_wave_cte_in_workspace_members_bounds_its_recursive_variable` | **RED**：报告 calm-truth 文件与 CTE |
| R3-SQL2 | 在 `calm-server/src` 注入同形无界 CTE | 同上 | **RED**：报告 calm-server 文件与 CTE |
| R3-SQL3 | 递归臂无 bound，只在外层 `SELECT` 写 `WHERE depth<=?2` | 同上 | **RED**：报告 CTE body 无递归变量 bound |
| R3-SQL4 | 省略 SQLite 可选的 `RECURSIVE` 关键字且不设 bound | 同上 | **RED**：`WITH down...` 仍被识别并报告 |
| R3-SQL5 | 用 `guard.depth<=?2` 给真正的递归变量伪造通行证 | 同上 | **RED**：错误 alias 不再满足判据 |
| R3-m5 | web 不渲染 `root_wave_id`，只写泛称 top wave | `keeps capacity copy aligned with the Rust recovery-action contract` | **RED**：缺少 `wave-root-985` |

三类旧误红的常驻正例在复原态实跑为绿：`?2 >= down.depth`、同语句无关递归 CTE +
`parent_wave_id`、同文件两条无关字面量；literal-only `concat!` 的真实拼接反例也被识别。性质门复原态
**13/13**。

### 7.1 受本轮实现影响的旧证据保质期刷新

| 旧条目 | 为什么受影响 | R3 重跑 |
|---|---|---|
| M9 | 改了 singleton shortcut 解码路径 | 把形状判断强制成“在树中”后 `a_non_tree_wave_runs_zero_recursive_tree_queries` **RED** |
| R2-B1 | 改了 shortcut 的完整比较式 | 强制 singleton 恒 `NotInTree` 后 reset 用例 **RED：40/40，期望 32** |
| R2-B2a–d | 性质门已完全重写 | 内联 mod / 包装宏 / static / 块 const 四种无界 SQL 同批注入，门列出 **4 条独立 violation** |
| R2-M3b | web tree 文案新增根 ID | 把 `top wave` 改成 `wave settings` 后契约测试仍 **RED**，同时保留根 ID 断言 |
| R1-M6 | `projection_policy_changed` / 整树重投影路径在 r3 被重排 | 删除 `tree_task_budget` 后目标仍 **RED**：pending `1 != 0` |

其余旧条目没有经过本轮改动的执行路径或判据；不伪造“重跑”记录。复原搜索确认没有
`R3_*_MUTANT`、强制 false 分支或临时默认值残留。

## 八、修复轮 4（D.4 #7 收口，全部实际执行并复原）

环境：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web 使用
Node 22.22.2。每个单点补丁均用反向 `apply_patch` 复原。

| # | 改坏什么 | 红的测试 | 实测结果 |
|---|---|---|---|
| R4-B1 | 删除 child-wave 写入 parent 后的 `tasks_rebuild_tree_tx` 调用 | `whole_tree_live_spec_never_exceeds_budget_across_admitted_growth_sequences` | **RED**：评审两构造精确复现 `left=(9,15), right=(8,12)` |
| R4-M1 | 整树循环退回每个成员调用 `tasks_rebuild_tx`，丢弃预计算 tree term | 同上 | **RED**：`tree_cte_queries=6`，期望与 N 无关的固定 2；在第一个 N=2 构造即拒绝 |
| R4-B1b | 删除裁 pending 后的逐成员/全树 live 后置复核 | `tightening_root_tree_budget_below_inflight_inventory_is_rejected_atomically` | **RED**：PATCH 返回 200，期望 409；证明不可删除的 in-flight 超额不能提交 |
| R4-B2a | 生产向下 CTE 改成 `WHERE 1=1 OR down.depth<=?2` | `every_recursive_parent_wave_cte_in_workspace_members_bounds_its_recursive_variable` | **RED**：报告生产 CTE 的 depth 比较不是合取项 |
| R4-B2b | 生产向下 CTE 改成 `WHERE down.depth<=?2 OR 1=1` | 同上 | **RED**：同上，反序析取也不能通行 |
| R4-m2 | 在 `calm-provider/src` 新增无界 parent-wave `.sql` | 同上 | **RED**：报告 provider 文件；证明 crate 集合来自 workspace manifest 而非两 crate 清单 |
| R4-M1b | 共用 writer 的预算校验退回只拒绝负数（允许 65） | `tree_task_budget_patch_on_a_child_is_refused_by_the_shared_writer` | **RED**：`MAX_TREE_TASK_BUDGET+1` 的 `unwrap_err()` 得到 `Ok(Wave)` |
| R4-m3 | web action→label 分支删除 `raise_tree_task_budget` | web `keeps capacity copy aligned with the Rust recovery-action contract` | **RED**：56 中 1 failed，`Review capacity` 只有 1 个、期望 2 |

### 8.1 受本轮路径改写影响的旧证据保质期刷新

| 旧条目 | 为什么受影响 | R4 重跑 |
|---|---|---|
| R1-M6 | `projection_policy_changed` 仍决定是否进入新的 B 共用例程 | 删除 `tree_task_budget` 后目标 **RED**：pending `1 != 0` |
| R3-B2 | 根预算分支改为共用 O(N) 例程 | 强制 `tree_budget_changed=false` 后目标 **RED**：descendant pending `1 != 0` |
| R2-M1a / M7 | N 变化后新增后置复核可能遮蔽库存差一 | `>=` 改 `>` 后目标仍 **RED**：写后复核拒绝，但错误不再是 `sub-wave-tree-budget-exhausted`，原测试的理由断言抓住接线退化 |

复原态定向门：wave-tree（含全声明序列性质）**31/31**、承重生产路径 **1/1**、SQL 性质门
**15/15**、child + policy **18/18**、web report-block **56/56**。没有
`zz_r4_wave_tree_probe.sql`、`false &&`、逐成员 `tasks_rebuild_tx` 或析取式生产谓词残留。

## 九、修复轮 5（r5 裁决收尾，全部实际执行并复原）

环境：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。每个变异均用
`apply_patch` 单点施加，目标测试结束后立即反向复原。

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R5-B1 | tree share 占用排除全部 non-block live spec（等价于排除升级 legacy） | `legacy_live_spec_consumes_tree_share_until_it_terminates` | **RED**：`K=3,B=2` 的普通报告写错误落下 2 条新 block，库存 `5 != 3` |
| R5-B1b | 固定关闭“任一成员不可裁占用 > share ⇒ 整树冻结” | `legacy_member_overage_freezes_new_blocks_across_the_tree` | **RED**：`K=B=8` 但 root legacy `5>share 4` 时，child 错误准入 1 条，`1 != 0`，总量会升到 9 |
| R5-B2-member | `member_overage` 后置条件改成 `if false &&` | `child_creation_409s_when_inflight_member_exceeds_its_new_share` | **RED**：`B=8`、root 5 条全 claim、`N:1→2` 的真实 adapter 错误返回 `Ok(TxOutput)`，child/card/event 均已进入未提交 tx |
| R5-B2-total | `total > budget` 后置条件改成 `if false &&` | `whole_tree_total_postcondition_rejects_an_over_budget_inventory` | **RED**：故意破坏 share/inventory 一致性的 fail-closed seam 返回 `Ok(())`；正确 `Σshare=B` 下该分支数学上冗余，seam 专门锁内部不一致兜底 |
| R5-M1a | 让直接合取叶语法门把任意含 recursive depth 的 `CASE` 当上界 | `semantic_or_constant_fakes_cannot_satisfy_the_bound_grammar` | **RED**：`CASE WHEN 1=1 THEN 1 ELSE down.depth <= ?2 END` 被测试点名 |
| R5-M1b | 允许 `alias.depth <= <常量>` | 同上 | **RED**：`<= 9223372036854775807` 被测试点名 |
| R5-m1 | singleton shortcut 的 `budget > ceiling` 改回 `>=` | `singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling` | **RED**：普通入口 `spec_task_ceiling`、整树入口 `tree_budget_exhausted`，同文档诊断分叉 |
| R5-m2 | TOML parser 退回按行猜 `workspace.members` | `cargo_legal_workspace_member_formatting_is_parsed_structurally` | **RED**：Cargo 合法单行数组被解析为 `[]`，期望两个成员 |

### 9.1 受本轮占用/相等边界改写影响的旧证据保质期刷新

| 旧条目 | 为什么受影响 | R5 重跑 |
|---|---|---|
| R4-B1 | 新 tree occupancy 改写了正常报告写的容量计算，可能提前挡住原 9/15 构造 | 删除 child 创建后的整树重投影，承重验收仍 **RED：`(9,15) != (8,12)`** |
| R2-m2 | 诊断归因从比较 share/ceiling 改成比较两种剩余 capacity | `tree_capacity <= ceiling_capacity` 改严格 `<` 后 `an_equal_tree_share_reports_the_tree_knob` **RED** |
| M9 | singleton shortcut 的相等边界由 `>=` 改成 `>`，默认孤根不再是 `NotInTree` | 对明确非绑定的 `B=32 > ceiling=31` 关闭 shortcut，`a_non_tree_wave_runs_zero_recursive_tree_queries` **RED**：错误返回 `Share` |
| R4-B1b | 后置条件抽成 helper，原“整段删除”变异的行位置与覆盖已过期 | 由 R5-B2-member + R5-B2-total 分拆刷新，两条各自 **RED**；不再用一个变异同时关闭两个谓词 |

本轮没有 STILL-GREEN 变异。复原搜索确认没有 `false &&`、CASE/常量临时放行、旧按行 manifest
解析器或 `Vec::<...>::new()` 跳过整树重投影残留。复原态定向门：wave-tree **33/33**、SQL
性质门 **17/17**、child adapter **12/12**、total postcondition **1/1**、policy PATCH **8/8**。
