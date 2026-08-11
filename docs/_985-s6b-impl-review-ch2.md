# #985 切片 6 PR-B —— 实现评审（通道 ch2，对抗性变异）

对象：`c71e4132..63940d9c`，工作树 `/tmp/wtb2`。跑法 `PATH=…/.local-bin:$PATH CARGO_BUILD_JOBS=6`，
`NEIGE_CODEX_BIN` 全程未设置。基线：`-p calm-truth --lib` **343 passed**；`-p calm-server`
**2296 passed / 1 skipped**。本报告 12 条自设变异 + 4 条行为探针，全部实跑。

## BLOCKER

### B1. 无子 wave 的根上，`tree_task_budget` 被完全忽略 —— 设计公式 + D.4 #7 双双落空

**结论**：`share(W,T)` 只在「有父或有子」时才算。一个 `parent_wave_id IS NULL` 且暂无子 wave 的根，
即使人显式把 `tree_task_budget` 设成 0，投影照样按 `spec_task_ceiling`（默认 32）准入。
设计 §8 ✅v5 框写的是 `T = W 所在的树，N = |T|`，单点树 N=1 ⇒ `share = B` ⇒
`effective_ceiling = min(ceiling, B)`；实现给的是 `ceiling`。本片自己改写的 §12.1 #7
「树内 spec 非终结行 ≤ `tree_task_budget`（PR-B 已交付）」在 N=1 上**为假**。

**证据**
- `crates/calm-truth/src/db/sqlite/wave_tree.rs:159-169` `wave_is_in_tree`（有父 **或** 有子才算在树里）
- `crates/calm-truth/src/db/sqlite/wave_tree.rs:176-181` 短路 ⇒ `NotInTree`
- `crates/calm-truth/src/db/sqlite/task_projection.rs:596` `WaveTreeTerm::NotInTree => (ceiling, None)`
- 对照：强制点一**认这个预算**——`crates/calm-server/src/operation/child_wave_adapter.rs:151-160`
  的 root 就是这个孤根，`inventory >= budget` 照样拒绝建子 wave。同一个 wave、同一个旋钮：
  **建子 wave 被拒，本 wave 自己无限排** —— 任务书第 3 条那种自相矛盾的镜像形态。
- 现有测试把 fail-open **写进了验收**：`wave_tree_budget_tests.rs:598-611`
  `a_non_tree_wave_is_bounded_only_by_its_own_ceiling` 只用默认 32（== ceiling 默认 32），
  读起来无害 —— 典型「恒真外衣」。

**我跑过的验证**（临时探针，已复原）
- `ch2_probe_lone_root_with_explicit_budget`（budget=1、ceiling 32、3 条声明）
  → `PROBE: budget=1 on childless root admitted 3 of 3 declarations`
- `ch2_probe_zero_budget_lone_root`（budget=0，走 `project_tasks_tx`）
  → `PROBE: budget=0 on childless root materialized 2 rows`
- `ch2_probe_root_with_child_then_child_deleted`：有子时 share=1，删子后 `term = NotInTree`
  （上界从 1 跳回 32，且无任何诊断）

**最小修法**：非树分支改成读该 wave 自己的 `tree_task_budget`（一条**非递归**语句，
`tree_cte_queries` 仍为 0，M9 零调用接缝不受影响）并返回 `Share{root: self, members: 1, share: budget}`；
配套把 `a_non_tree_wave_is_bounded_only_by_its_own_ceiling` 改成显式设一个 < ceiling 的 budget。

---

## MAJOR

### M1. G4「求不到根的四种形态」里，③ 成员超深 与 ④ 成员集合不含自己 —— 单独变异全绿

**攻击**：`wave_tree.rs:211-213` 的 `over_deep` 改成常量 `false`；另一轮把 `wave_tree.rs:214` 的
`position(...)` 改成 `.or(Some(0))`。**验证**：两轮各跑 `-p calm-truth --lib` → **都 343/343 全绿**。

**为什么是真缺口**：探针 `ch2_probe_over_deep_from_root` 显示，长度 6 的超深链**从根发起**时，
唯一让它 fail-closed 的就是 `over_deep`（`PROBE: chain_len=6 root term = RootUnresolved`）。
现有 `an_over_deep_chain_fails_closed`（`wave_tree_budget_tests.rs:555`）只从**最深那个**发起，
走的是形态②。删掉 ③ 后，中毒树的根会按**被截断的成员表算出的错误 N** 继续正常准入。

**最小修法**：各补一条验收 —— ③ 从**根**发起断言 `RootUnresolved`；④ 无法自然构造时，
把它降级成 `debug_assert!` 或直接删掉这条不可达分支（别留没有判别力的假防线）。

### M2. `tree_task_budget` 触发重投影（施工笔记 G1 的裁决）—— 零覆盖

**攻击**：`crates/calm-server/src/routes/waves.rs:1327-1329` 的 `projection_policy_changed`
去掉 `|| p.tree_task_budget.is_some()`。**验证**：`-p calm-server` → **2296 passed，全绿**。

**说明**：M12（`patch_has_other_changes`）红是因为它影响状态码与事件数；`projection_policy_changed`
只影响「同一事务里是否重投影本 wave」，而 `wave_projection_policy_patch.rs:176-284` 只断言了
列值 + 403 + 400 + 事件数。同文件里 `spec_task_ceiling` 有这条
（`tightening_policy_immediately_deletes_pending_projection_and_emits_plan_updated`）。
**最小修法**：照抄它，改成 PATCH 收紧 `tree_task_budget` 后断言 pending 行被删 + `PlanUpdated`。

### M3. 登记制的「机器联系」被 rustfmt 合法的换行方式绕过

**攻击**：加一条**不登记**的常量
`pub const WAVE_TREE_LEAKY_SQL: &str = concat!(bounded_wave_descendant_cte!(), "SELECT id FROM down");`
**验证**：`-p calm-truth --lib wave_tree` → **22/22 全绿**；再跑 `cargo fmt -p calm-truth`
（rustfmt 把它排成 `… =\n    concat!(bounded_wave_descendant_cte!(), …);`）—— **仍 22/22 全绿**。

**根因**：`wave_tree.rs:296-307` 数的是字面 needle `concat!(\n    bounded_wave_`。它锁的是
**一种排版**，不是「宏被展开了几次」。实现方 M3（从名单里删一条已有常量）能红，只证明了
「名单不能变短」，**没有**证明「新常量不能漏登记」—— 后者才是 §7.1 要防的形状。
附带：门禁只扫 `wave_tree.rs` 自己，模块头「每一处 `parent_wave_id` 递归遍历都住这里」没有全仓约束。
**最小修法**：正则 `concat!\s*\(\s*bounded_wave_`，或让宏展开时自注册；另加一条全仓
`WITH RECURSIVE … waves` 白名单扫描。

### M4. `RootUnresolved` 的提前返回把 §6.5 的 withdrawal 边沿一起吞掉了

**结论**：`task_projection.rs:597-609` 在求不到根时**直接 return**，绕过了 `:761-805` 的
withdrawal 边沿计算与 `:846-863`「已删块 ⇒ 合成 withdrawal 判决」。于是根断链的 wave 里，
**running 的任务被人撤回 `ready` 不会被识别为 withdrawal**，`mark_context_material_tx` 不触发。

**验证**（探针 `ch2_probe_withdrawal_edge_under_unresolved_root`；对照组孤立 wave、实验组 2-环
里的 wave，其余完全相同）：`PROBE control.withdrawal=Some(Ready) unresolved.withdrawal=None`

**说明**：方向上不是「多跑了任务」，但它把一条**独立的** fail-closed 语义（撤回 ⇒ 标 material）
在一个本就异常的窗口里静默关掉了。现有 `unresolvable_root_fails_closed_for_every_declaration`
只断言「一条都不准入」，看不到这一层。

**最小修法**：不要提前 return —— 把 `RootUnresolved` 表达成 `effective_ceiling = 0` +
给每条判决追加 `tree_root_unresolved` 诊断，让主干（withdrawal / 已删块合成判决 / read state）
照常跑完。补一条验收：不可解根 + running 行 + 撤回 ready ⇒ `withdrawal == Some(Ready)`。

---

## MINOR

### m1. 「向下 2-环」验收测的是生产到不了的形状
`parent_wave_id` 是**函数图**（至多一个父），从 `parent_wave_id IS NULL` 的根向下闭包**不可能有环**；
环只在无根分量里，而那种分量在 `wave_tree_term` / `prepare_tx` 都先被求根挡掉。
`a_downward_two_cycle_terminates_quickly`（`wave_tree_budget_tests.rs:620-645`）是手工从**环内节点**
发起 SQL 的。实测：`wave_tree.rs:204` 生产侧 `.bind(MAX_WAVE_TREE_DEPTH + 1)` → `.bind(i64::MAX)`
仍 **343/343 全绿**；我构造「根下挂 2-环」的探针也证明这形状造不出来。即向下 CTE 的截断在生产里是
**深度截断**而非环终止，`wave_tree.rs:47-49` 的注释说过头了。

### m2. `(created_at, id)` 次序：判别力约 3/4，且 created_at 相等零覆盖
`wave_tree.rs:79` 的 `ORDER BY w.created_at, w.id` 改成 `ORDER BY w.id`
→ `shares_over_a_real_tree_sum_to_the_budget` 红（22 tests: 21 passed, 1 failed）。但 id 随机，
该断言在 id 序下仍有 1/4 概率恰好成立 —— 概率性红。helper `stamp_created_at`（`:76-83`）又显式
回避了 created_at 相同的情形，而同毫秒建 wave 是常态（分配本身仍确定，tie 落 id，纯覆盖缺口）。

### m3. 两个强制点数的不是同一个集合
强制点一（`wave_tree.rs:87-92`）数 `declared_by='spec'` 的**全部 origin**；强制点二的 `occupied`
（`task_projection.rs:620-623`）只数 `origin='block'`。`origin='legacy'` 的 spec 行确实存在
（`task_projection.rs:1075`、`:1125`）。方向上一更严，但 D.4 #7 那句「树内 spec 非终结行 ≤ budget」
在有 legacy 行时不成立。建议把 §12.1 #7 收敛到 `origin='block'`。

### m4. root-only 守卫是**时点**检查（仅读码，未跑变异）
`wave.rs:222-244` 只在 PATCH 那一刻看 `parent_wave_id`。当时是根、之后被挂到别人下面的 wave，
会带着非 NULL 的 `tree_task_budget` 留在表里（读侧只读 root，不产生错误行为，但表里多一个值）。
建议 `child_wave_adapter` 写 `parent_wave_id` 时顺手把子 wave 的该列置 NULL。

---

## 一、实现方自述的缺口，哪些属实、哪些被低估

| 自述 | 我的复核 |
|---|---|
| **第 8 条：M5c「只删 `wave_create_tx` 显式 NULL 仍绿，两道防御冗余」** | **属实，解释成立。** 我独立复跑同一变异（`wave.rs:47-58` 去掉 `tree_task_budget`/`NULL`）→ **343/343 全绿**。判别力确由 `every_created_wave_lands_a_null_tree_task_budget` 的 `pragma_table_info.dflt_value IS NULL`（`wave_tree_budget_tests.rs:199-206`）提供。冗余没有掩盖别的东西。**不构成问题。** |
| **M13：user-only 闸已强化成断言拒绝理由** | **属实。** `wave_projection_policy_patch.rs:196-212` 断言响应体同时含 `tree_task_budget` 与 `user-only`，不是裸 403。既有 `spec_task_ceiling` 那条恒真断言未动、已单开 #1043 —— 这个范围切割我同意。 |
| **M7（`>=` vs `>`）实测红** | **属实**，我复跑确认：`operation::child_wave_adapter::tests::acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full` 红（`-p calm-server --lib`：258 passed / 1 failed）。 |
| **M1「共享计数 ⇒ rebuild 序分叉必红」** | **属实且强于自述。** 我自写一版共享计数（扣兄弟全部非终结 spec 行）→ **5 条红**，含 `two_rebuild_orders_over_one_tree_agree_byte_for_byte`、`shares_do_not_move_when_siblings_accumulate_pending_rows`。承重性质（share 与投影产物无关）**覆盖是真的**。 |
| **M3「登记制有机器联系」** | **被低估（见 MAJOR M3）。** 它只证明了名单不能变短，没证明新常量不能漏登记；换个 rustfmt 合法排版即可绕过。 |
| **M4「fail-closed 不是可选项」** | 属实（我复跑：`RootUnresolved` 改成 `(ceiling, None)` ⇒ `unresolvable_root_fails_closed_for_every_declaration` 红）。但**只覆盖形态①**；形态③④ 见 MAJOR M1，且 fail-closed 分支自身引入了 MAJOR M4 的副作用。 |
| **G1「budget 改动触发重投影」** | **未被任何测试固定**（MAJOR M2）。自述把它列成已交付的裁决，实际是零覆盖的接线。 |
| 迁移 `0072_` additive / 可重放 | 属实：单条 `ALTER TABLE … ADD COLUMN`，不重建 `waves`；`-p calm-truth --lib` 全绿含 migration 相关套件，未见钉在旧 schema 上的测试被打断。 |

## 二、我自己设计的变异里，哪几条打穿了

按「改坏实现，测试不红 / 行为与设计不符」计，**6 条打穿**：

1. **B1**（探针，非变异）：孤根上的 `tree_task_budget` 完全失效 —— budget=0 仍落 2 行。
2. **M1a** `over_deep = false` → 343/343 全绿。
3. **M1b** 成员表找不到自己 → `.or(Some(0))` → 343/343 全绿。
4. **M2** 去掉 `projection_policy_changed` 里的 `tree_task_budget` → calm-server 2296 全绿。
5. **M3** 加一条一行排版的未登记 CTE 常量（`cargo fmt` 后依然）→ wave_tree 门禁 22/22 全绿。
6. **M4**（探针）：不可解根下 withdrawal 边沿 `Some(Ready)` → `None`。

**没打穿的**（覆盖是真的，如实记）：共享计数（5 红）、删树项 `min`（3 红）、fail-open 根（1 红）、
`>=`→`>`（1 红）、`ORDER BY` 去掉 `created_at`（1 红，概率性）。

## 三、可以合入了吗

**NO。**

最小阻塞集：
1. **B1**：非树分支按 `N=1` 的树处理（或在设计里显式裁决「孤根不受树预算约束」并同步改 §8 公式与
   D.4 #7 —— 但那样强制点一必须跟着放行，否则两个强制点自相矛盾）；并把
   `a_non_tree_wave_is_bounded_only_by_its_own_ceiling` 改成非恒真形态。
2. **M2**：补「PATCH 收紧 `tree_task_budget` ⇒ 当场裁掉超额 pending 行」的路由级验收。
3. **M4**：`RootUnresolved` 改成 `effective_ceiling = 0` + 追加诊断走完主干；补撤回边沿验收。

M1 / M3 建议同批修；若分批，至少补上 M1 的形态③ —— 它是「中毒树的根按错误 N 继续准入」的唯一防线。
MINOR 四条不阻塞。

---

```
$ git status --short
（空 —— 所有临时变异与探针已 git checkout -- . 复原，
  复原后 cargo nextest run -p calm-truth --lib = 343 passed）
```
