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

## 十、修复轮 6（删除短路 + 诊断可操作性，全部实际执行并复原）

环境：`.local-bin` nextest、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web 变异用
Node 22.22.0。每个变异均以 `apply_patch` 施加并反向复原。

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R6-B1a | 在 `wave_tree_term` 恢复孤根早退，直接给 `share=i64::MAX`（任意形式语义短路） | `singleton_default_budget_counts_legacy_occupancy_before_admission` | **RED**：live spec `33 != 32`，恢复了 codex 的默认 B=32 构造 |
| R6-B1b | 同一短路 | `singleton_explicit_budget_counts_legacy_occupancy_before_admission` | **RED**：live spec `8 != 6`，恢复了 subagent 的 B=6 构造 |
| R6-B1c | 旧 schema fixture 删除基线 `waves.created_at`（等价于短路仍遮住真实 tree query 的旧夹具） | `migration_backfills_preexisting_task_and_block_declaration_adopts_it` | **RED**：首轮全门报 `no such column: w.created_at`；补齐 head-schema 最小夹具后定向 **PASS 1/1**，未改 migration |
| R6-B2a | 诊断归因把 `<` 反成 `>`，让严格绑定场景指向另一个旋钮 | `the_diagnosed_capacity_action_increases_admission` | **RED**：照 `raise_tree_task_budget` 做后准入仍为 `2 → 2` |
| R6-B2b | 平局归因把 `<` 改为 `<=` | `an_equal_tree_share_reports_the_local_ceiling_knob` | **RED**：找不到 `spec_task_ceiling` / `raise_spec_task_ceiling` |
| R6-m1-rust | Rust tree 文案恢复“let an in-flight task in this wave finish” | `over_share_declarations_are_diagnosed_against_the_root_wave` | **RED**：缺少通用的 `tree's excess in-flight work`；冻结 sibling 构造也常驻断言不得出现 `task in this wave` |
| R6-m1-web | web tree 文案恢复“let an in-progress task in this wave finish” | `report-blocks.test.tsx` 的 `gives tree_budget_exhausted a human explanation and next action` | **RED**：Node 22 下 56 中 1 failed |
| R6-m2 | 删除 workspace member root 的 `Cargo.toml` 存在性断言 | `a_missing_workspace_member_root_fails_closed` | **RED**：`should_panic` 未发生，证明扫描面会静默缩小 |

SQL 常量界、未限定列、匿名参数与引号标识符没有放宽；实现笔记 §3.5/N7 明确记录为刻意拒绝，
理由是门只证明 alias-qualified、编号参数化的直接合取叶，未来扩形状必须先补正反矩阵。

### 10.1 短路相关旧证据保质期刷新

| 旧条目 | 修复轮 6 处置 / 实跑结果 |
|---|---|
| M9 | 原“非树零递归”买的是短路性能性质，随短路一并删除；r6 替换的 `a_singleton_tree_runs_two_constant_size_recursive_queries` 对所有可解析树恒为 2，修复轮 7 也删除。恢复孤根早退仍由 R6-B1a/b 正确性验收 RED；整树入口退回逐成员递归由 R7-m2 的真实 `calm-server --lib` 变异 RED |
| R1-B1c（评审文字也曾写作 R3-B1c） | 原“孤根显式预算不得 NotInTree”不再依赖枚举分支；恢复孤根早退后 `an_explicit_budget_applies_to_a_singleton_root` **RED**：B=1 仍放过后两条 |
| R3-B1a | shortcut 自行解 NULL 的变异已无落点；恢复孤根早退后 `a_null_ceiling_and_tiny_budget_still_bind_a_singleton_root` **RED**：准入 `5 != 1` |
| R5-m1 | `budget > ceiling` 条件已随 shortcut 删除；r6 的“平局单归 local ceiling”证据在 r7 语义下失效，由 R7-B1a/R7-B1d 取代。复原态 `singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling` **PASS 1/1**，两入口现在都同时给出 `spec_task_ceiling` + `tree_budget_exhausted` |

所有临时早退、反向比较、fail-open 与旧文案均已反向复原。复原态定向门：`calm-truth --lib`
**357/357**（wave-tree **26/26**）、SQL **18/18**、child adapter **12/12**、web report-block
**56/56**；最终全门数字见实现笔记 §5。

## 十一、修复轮 7（平局双绑定 + 组合空间效果性质，全部实际执行并复原）

环境：`.local-bin` nextest、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web 使用
Node 22.22.0。每个生产/文案变异均以 `apply_patch` 施加并反向复原。

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R7-B1a | 平局分支删掉 tree 诊断，退回只报 `raise_spec_task_ceiling` | `the_diagnosed_capacity_action_increases_admission` | **RED 0/1**：默认孤根平局照动作后 `32 → 32`；这条就是题定默认配置回归的杀手变异 |
| R7-B1b | tree 动作携带的“该成员 share 首次增长的最小 B”退回朴素 `B+1` | 同一 8 行表驱动性质 | **RED 0/1**：`N=3,B=5,index=1` 余数接收者在 `B=6` 时 share 仍为 2，照两个平局动作后 `2 → 2` |
| R7-B1b-freeze | freeze 目标只算当前成员下一格，忽略让所有 legacy overage 装回 share 所需的更高 B | 同一性质 | **RED 0/1**：sibling 有 5 条不可裁 legacy、`N=2,B=4`；只抬到 6 后整树仍冻结，目标成员 `0 → 0` |
| R7-B1c（刷新 R6-B2a） | 严格归因比较 `<` 反成 `>` | 同一性质 | **RED 0/1**：strict-local 孤根被误报 tree，照做后 `2 → 2` |
| R7-B1d（刷新 R6-B2b） | 严格 tree 分支把 `<` 放宽成 `<=`，平局被吞成 tree 单诊断 | 同一性质 | **RED 0/1**：默认孤根只抬 B 到 33，`32 → 32` |
| R7-m1-rust | 跳过 `admission_frozen` 专用渲染，恢复普通“slice used up”句 | `legacy_member_overage_freezes_new_blocks_across_the_tree` | **RED 0/1**：实际打印当前 wave `slice of 4 is used up`，缺少真实 tree-wide freeze 原因 |
| R7-m1-web | web 跳过 `admission_frozen` 专用渲染 | `report-blocks.test.tsx` | **RED**：59 中 1 failed，缺少 `immutable in-progress work than its share` |
| R7-m2 | 整树循环丢弃预计算 term，改为每成员调用 `tasks_rebuild_tx` 重新递归 | `calm-server --lib` 的整树查询计数后置 | **RED**：已执行 234/693 时累计 **8 failed**、其余 459 因 fail-fast 未运行；错误均为递归查询 `4/6 != 2`，包含 `whole_tree_live_spec_never_exceeds_budget_across_admitted_growth_sequences` 与入口一致性测试，证明已删除的孤根恒 2 断言不是承重点 |
| R7-m3 | 把不存在 API 的 `repair_wave_tree` 重新登记为 recovery action | `tree_root_unresolved_has_human_copy_without_a_fake_user_action` | **RED 0/1**：构造器发现期望假 action、实际为 `None` |
| R7-D4 | 在 D.4 #7 后临时恢复“升级 legacy 无例外、任意瞬间无条件 `Σ≤B`”句 | `legacy_live_spec_consumes_tree_share_until_it_terminates` | **STILL-GREEN 1/1**：实现验收只能证明退化行为，不能证明架构文案没有撒谎；因此 D.4 精确措辞仍需人工评审承重 |
| R7-m5 | 在读路径注释临时恢复“core 外只有后两种读”的旧版本数说法 | `the_diagnosed_capacity_action_increases_admission` | **STILL-GREEN 1/1**：纯注释漂移无运行时红灯，已复原；修复靠代码序与评审核对 |

### 11.1 组合空间与旧证据保质期

效果性质不是两个例子的列表，而是一张 8 行输入表：strict-local、strict-tree、默认孤根
`C=S=32`、`N=3/B=5` 的余数内 `index=1` 与余数外 `index=2`、`N=2/B=2` 的零余数后序成员、
`share=5/legacy=2/ceiling=3` 的容量平局，以及 sibling legacy overage freeze。每行从实际拒绝读取全部
容量 action，tree action 使用诊断携带的 minimum B，完成后重投影同一报告并断言准入严格增加。

r6 的 R6-B2a（严格方向）由 R7-B1c 重跑仍 RED；R6-B2b 与 R5-m1 原先钉住“平局归 local”这一旧
语义，已主动退役并由 R7-B1a/B1d 的效果杀手变异替换。`singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling`
已改为两入口都返回双诊断，复原态定向 **PASS 1/1**。旧 schema fixture 的 `created_at` 与 workspace
fail-closed 本轮未动；其 R6-B1c/R6-m2 证据不受归因变更影响。

## 十二、修复轮 8（冻结态联合目标 + 无解诊断，全部实际执行并复原）

环境：`.local-bin` nextest、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web 变异显式使用
Node 22.22.0。所有生产/文案变异均以 `apply_patch` 单点施加，目标测试结束后立即反向复原。

修复前先把评审扫描固化：`N=1..=3`、`B=0..=6`、全部 target index、`ceiling=0..=5`、
目标 legacy 占用 `∈{0,3}`，共 **504** 个实际 SQLite 投影组合。每个拒绝读取全部容量动作、执行一次
并重投影同一报告；修前精确 **210** 个无效，修后 **0**。这是跨过每个 N 至少两个余数边界的最小
稠密网格；`B=64` 的普通/冻结无解边界另用生产验收覆盖，避免把一个稀疏上限乘进整张网格。

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R8-B1 | 联合目标搜索退回只要求 `new_share > current_share`，不要求 `new_share > target_occupancy` | `the_diagnosed_capacity_action_increases_admission` | **RED 0/1**：**210** 个 self-overage 组合执行建议后仍 `0 → 0`；含 `B=0,N=1,legacy=3` 的错误 minimum 3 |
| R8-B2a | 冻结分支删除 `ceiling_capacity==0` 时的 local ceiling 诊断 | 同一穷举验收 | **RED 0/1**：B1 已正确后残余精确 **35** 个，全部 `C=0 && frozen`，tree 动作后仍 `0 → 0` |
| R8-B2b | 冻结双归因错误判断裸 `ceiling==0`，忽略非零 ceiling 已被在飞 block 占满 | `a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds` | **RED 0/1**：`ceiling=3, occupied=3` 缺 `spec_task_ceiling` / `raise_spec_task_ceiling` |
| R8-B3-action | minimum 不存在时恢复无条件 `raise_tree_task_budget` | `an_unreachable_tree_budget_target_reports_no_raise_action` | **RED 0/1**：构造器 fail-closed，`Some("raise_tree_task_budget") != None` |
| R8-B3-rust | Rust 无 minimum 分支恢复伪建议 “at least 0” | 同上 | **RED 0/1**：缺“当前配置无法通过抬高预算解除”，边界断言在假 0 文案处失败 |
| R8-B3-web | web 无 minimum 分支恢复 `minimum ?? ''` 的空数字建议 | `report-blocks.test.tsx` | **RED 2/59**：实际渲染 `to at least .`，无解说明与跨源契约两处同时失败 |

### 12.1 受冻结归因/动作可用性改写影响的旧证据刷新

| 旧条目 | R8 重跑 |
|---|---|
| R7-B1a（平局漏 tree） | 新穷举门 **RED：48** 个动作无效，均只抬 local ceiling 后准入不增 |
| R7-B1b（minimum 退回 `B+1`） | 新穷举门 **RED：320** 个动作无效，覆盖余数位置与 self-overage，不再依赖一行手挑数据 |
| R7-B1b-freeze（忽略全员解冻 minimum） | `legacy_member_overage_freezes_new_blocks_across_the_tree` **RED**：sibling overage 构造给 8，期望可执行的 9 |
| R7-B1c / R6-B2a（严格方向反转） | 新穷举门 **RED：246** 个动作无效 |
| R7-B1d（`<` 放宽成 `<=`，平局吞成 tree-only） | 新穷举门 **RED：48** 个动作无效 |
| R7-m1-rust | `legacy_member_overage_freezes_new_blocks_across_the_tree` **RED**：错误回退成 “slice … used up” |
| R7-m1-web | web report-block 定向门 **RED 1/59**：缺 `immutable in-progress work than its share` |

本轮没有 STILL-GREEN 运行时变异。复原搜索确认没有 `false &&`、裸 `ceiling==0`、反向/放宽容量
比较、无条件 tree raise action、假 `at least 0` 或 web 空 minimum 残留。

## 十三、修复轮 9（local minimum 对称修法 + 文档状态补维，全部实际执行并复原）

环境：`.local-bin` nextest、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置；web 变异使用
Node 22.23.2。所有实现、测试与文案变异都以 `apply_patch` 单点施加，运行后立即反向复原。

基线旧网格为 **504** 格、**0** 红、2.618s。只加入 `block_inflight∈{0,3}`、仍执行旧生产
`ceiling+1` 时，1008 格精确 **252** 个无效 action；生产改为
`max(ceiling, ceiling_occupied)+1` 后 **0** 红，复原态网格 **1008/1008 PASS，5.945s**。

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R9-B1a | local minimum 退回 `ceiling+1`，丢掉 occupancy floor | `the_diagnosed_capacity_action_increases_admission` | **RED 0/1**：精确 **252** 个 `block_inflight=3 && ceiling<3` 组合执行全部动作后不增长 |
| R9-B1b | local minimum 只写 `occupied+1`，丢掉 current-ceiling floor | 同一网格 | **RED 0/1**：精确 **73** 个普通 `ceiling>occupied` 组合拿到未抬高甚至降低的目标 |
| R9-B1c | 生产不携带 `minimum_spec_task_ceiling` | 同一网格 | **RED 0/1**：首个 local action 在 `ceiling action must carry an occupancy-safe minimum` fail-closed |
| R9-B1-rust | Rust local renderer 忽略诊断 minimum、退回 `ceiling+1` | `a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds` | **RED 0/1**：`ceiling=1,occupied=3` 错报 `at least 2`，期望 4 |
| R9-B1-web | web 丢弃 local minimum 的数字分支 | `report-blocks.test.tsx` | **RED 4/62**：ordinary、tied、frozen 与跨源动作契约均缺精确目标 |
| R9-B2 | 从网格删除 `block_inflight=3` 轴 | 同一网格的规模断言 | **RED 0/1**：`left: 504, right: 1008`，防止验收空间退回实现者自选状态 |
| R9-M1 | 冻结 local 诊断重新写 `bounds_tied=true` | `a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds` + `legacy_member_overage_freezes_new_blocks_across_the_tree` | **RED 0/2**：两条都抓到冻结被伪装成“份额与 local 双满” |
| R9-M2 | Rust frozen 文案恢复不打印 minimum 的旧句 | 同上两条 Rust 验收 | **RED 0/2**：分别缺 `at least 4` 与 `at least 9` |
| R9-N1 | web tied unavailable 分支方向反转 | `report-blocks.test.tsx` | **RED 1/62**：错误建议 local 目标 65，缺“无更高合法目标” |
| R9-N2 | `capacity_raise_unavailable` 再次只在 `tree_context.is_some()` 时写入 | `an_unreachable_tree_budget_target_reports_no_raise_action` + 新网格 | **STILL-GREEN 2/2**：当前 false-action 调用都带 tree context；修复的是评审指出的未来不可达 panic 脆点，尚无可达生产 seam |
| R9-N3 | wiring 注释恢复“网格只变 remaining local capacity”的旧假陈述 | `git diff --check` + focused wiring test | **STILL-GREEN 1/1**：纯注释漂移无运行时 oracle；依靠就地状态/排除族清单与评审核对承重 |

### 13.1 受 local minimum 与网格补维影响的旧证据保质期刷新

下列旧条目都以效果网格计数，因空间从 504 变为 1008 且 local action 改为读取诊断 minimum，旧数字
失效，已逐条重跑。sibling-only freeze、无解上限、migration/SQL/后置条件等不经过该网格的旧条目不受
本轮两处改动影响。

| 旧条目 | r9 扩维后重跑 |
|---|---|
| R8-B1（tree minimum 忽略 target occupancy） | 网格 **RED：666** 个 action 无效；`a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds` 同时红，tree minimum `3 != 4` |
| R8-B2a（冻结不登记 local ceiling） | 网格 **RED：339** 个 action 无效；nonzero-occupancy 专测同时找不到 ceiling diagnostic |
| R8-B2b（用裸 `ceiling==0` 识别 local 绑定） | 网格 **RED：228** 个 action 无效；nonzero-occupancy 专测同时红 |
| R7-B1a（平局漏 tree） | 网格 **RED：70** 个 action 无效 |
| R7-B1b（tree minimum 退回 `B+1`） | 网格 **RED：788** 个 action 无效 |
| R7-B1c / R6-B2a（严格方向反转） | 网格 **RED：272** 个 action 无效 |
| R7-B1d（`<` 放宽为 `<=`，平局 tree-only） | 网格 **RED：70** 个 action 无效 |

复原搜索确认没有 `_removed` minimum key、`false &&`、裸 `ceiling==0`、反向/放宽容量比较、
`minimum_for_target=Some(B+1)`、强制 `bounds_tied=true` 或 web unavailable 反向条件残留。

## 十四、修复轮 10（flaky oracle + 同毫秒 tie-break 裁决，全部实际执行并复原）

环境：`.local-bin` nextest、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。所有变异均以
`apply_patch` 单点施加，运行后立即反向复原。

| # | 改坏什么 | 变红的测试 | 实际结果 |
|---|---|---|---|
| R10-B1 | 删除 `legacy_member_overage_freezes_new_blocks_across_the_tree` 在 link 后固定 root=1、child=2 的两行 fixture | `legacy_member_overage_freezes_new_blocks_across_the_tree` | **RED（循环第 8 次）**：前 7 次 PASS，第 8 次实际诊断 `at least 10`，精确 9 oracle 在 0.108s 红；证明不固定时间仍会复现，而不是把断言放宽掩盖 |
| R10-B2a | 冻结 minimum 退回朴素 `B+1` | `legacy_member_overage_freezes_new_blocks_across_the_tree` | **RED 0/1**：生产错报 `at least 5`，期望精确 9 |
| R10-B2b | 同一个朴素 `B+1` 变异独立运行新 tie-break 用例 | `equal_created_at_with_child_id_first_requires_ten_to_unfreeze` | **RED 0/1**：生产错报 `at least 5`，期望精确 10 |
| R10-B3 | 把固定占用成员 SQL 的同毫秒 id 次序反成 `ORDER BY w.created_at,w.id DESC` | `equal_created_at_with_child_id_first_requires_ten_to_unfreeze` | **RED 0/1**：child 不再先拿余数，minimum 从精确 10 变成 9；新用例确实钉住已裁决的 UUID tie-break 方向 |

本轮没有 STILL-GREEN 运行时变异。复原搜索/`git diff` 确认生产
`task_projection.rs`、`wave_tree.rs` 无残留变更；正式差异只含两条测试与设计/实施文档。
