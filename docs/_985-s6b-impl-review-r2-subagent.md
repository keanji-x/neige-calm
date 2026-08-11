# #985 切片 6 PR-B 代码评审 r2（CHANNEL = subagent）

对象：`c71e4132` → `511dcd37`。环境：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、
`NEIGE_CODEX_BIN` 未设置；web 用 Node 22.22.2（`web/node_modules` 临时软链自主仓，已删除）。

**基线全绿（实跑）**：`cargo test -p calm-truth --lib` 350 passed；
`-p calm-server --lib child_wave` 9 passed；`-p calm-types --lib report_blocks` 50 passed；
`vitest run src/pages/report-blocks/report-blocks.test.tsx` 54 passed。

分级：**BLOCKER 0 / MAJOR 3 / MINOR 5**。

---

## 一、修复轮 1 的几处，哪几处修出了新洞

### MAJOR-1 —— 短路收紧成 `IS NULL` 后，孤根仍绕过默认树预算；点一点二在同一个 wave 上互相矛盾

**结论**：`wave_tree.rs:187-201` 把非树短路收紧成「`tree_task_budget IS NULL` 才 `NotInTree`」，
但设计 v5 框写的条件是「**可证**默认树项不会比默认 wave ceiling 更紧」
（`docs/_985-s6-design.md` §8 v5 框）。实现只判了 `IS NULL`，**没有判可证性**。
`spec_task_ceiling` 上界不存在（`routes/waves.rs:1274-1279` 只校验 `>= 0`，默认 32 见
`task_projection.rs:18`），人把 ceiling 调到 40 时默认树项（B=32, N=1, share=32）**就是**更紧的那条，
却被短路跳过 —— 这正是简报点 3 的「把可证不生效写成恰好现在不生效」。

同一个 wave 上两个强制点因此**给出不同的 B**：点一 `wave_tree_budget()`（`wave_tree.rs:262-272`，
NULL⇒32）会用 32 拒绝建子 wave（`child_wave_adapter.rs:160-166`），点二却完全不加树项。
副作用：`routes/waves.rs:1288` 的文案「pass null to reset to the kernel default」在孤根上
是**解除上界**，不是恢复 32。

**触发条件**：wave 无 parent 无 child、`tree_task_budget IS NULL`、`spec_task_ceiling > 32`。

**我实际跑过的验证**：临时探针
`calm_truth::db::sqlite::wave_tree_budget_tests::probe_null_budget_singleton_ignores_the_kernel_default`
（ceiling=40、40 条声明，先 NULL 后显式 32，两次都走 `evaluate_schedulability`）：
`PROBE1 NULL-budget singleton schedulable = 40` / `PROBE1 explicit-32 singleton schedulable = 32`，
断言 `NULL 与显式 32 必须一致` **FAILED（left 40, right 32）**。已复原。

**最小修法**（二选一，都是几行）：
- 代码向文档收敛：`wave_tree.rs:188-196` 的 `None` 臂改成
  `Share { root_id: wave_id, budget: DEFAULT_TREE_TASK_BUDGET, members: 1, share: DEFAULT_TREE_TASK_BUDGET }`。
  零递归性质不变（`tree_cte_queries` 仍为 0，`a_non_tree_wave_runs_zero_recursive_tree_queries` 仍绿），
  NULL ≡ 显式 32 也成立。
- 或保留 `NotInTree`，但把条件写成**可证式**：`budget IS NULL && ceiling <= DEFAULT_TREE_TASK_BUDGET`，
  并同步改 `wave_tree.rs:132-135` 的文档注释与 §8 v5 框。

配套断言：把上面那条探针（NULL 与显式 `DEFAULT` 结果相等，ceiling 取 `DEFAULT+8`）留成正式用例 ——
它对两个方向都有判别力，且不与被测代码共用事实来源。

### 复核结论：其余修复点未修出新洞（逐条实测）

- **点一/点二相容性不变量非恒真**：`enforcement_points_are_compatible_...`（`wave_tree.rs:374-387`）
  在 `B ≥ 2` 时真实断言，`can_add_tree_member` 放宽即红（实跑 R1-B1a 形状：把
  `wave_tree.rs:126` 改成恒 `true`，`cargo test -p calm-server --lib acceptance_tree_budget_never_admits_a_zero_share_member`
  **FAILED**）。见 MINOR-1/MINOR-2 的两点保留意见。
- **成员数上界与库存准入是合取，不遮蔽**：`child_wave_adapter.rs:158-173` 两个 `if` 顺序独立返回，
  错误串各自唯一（`unfinished spec task(s)` vs `member wave(s)`），新验收断言的是后者的串。
- **并发建两个子 wave 不产生 TOCTOU**：`operation/repo_sqlite.rs:282` 的
  `begin_immediate_tx` 让 `prepare_tx` 的读也在写锁内（#930 统一规则），点一的
  「读成员数 → 写 waves」不可交错。**非问题**。
- **`RootUnresolved` 改成不早退后没有污染诊断**：探针 `probe_unresolved_root_diagnostic_set`
  实跑，两条 verdict 各自只有 `tree_root_unresolved`，**没有**多出误导的 `spec_task_ceiling`
  （因为该诊断先把 verdict 置为不可调度，`candidates` 为空）。我原本怀疑此处，**证伪**。
- **B2 的排序断言与行为用例是互补而非重叠**：把 `wave_tree.rs:84` 改成
  `ORDER BY w.created_at, w.id DESC`（文本仍含针）后，结构断言绿、
  `quota_remainder_breaks_equal_created_at_ties_by_id` **RED**。方向/tie-break 由行为买，
  存在性由文本买，配对成立（见 MINOR-3 的残留）。
- **Rust / web 文案不同源，各自有独立锁**：两侧措辞本就不同（`in-flight` vs `in-progress`），
  只改一侧只红一侧，符合预期；残留见 MINOR-5。

---

## 二、我自己设计的验证里，哪几条发现了问题

### MAJOR-2 —— `share == ceiling` 时诊断把人指向一个无效动作

**结论**：`task_projection.rs:876-879` 的 `filter(|share| share.share < ceiling)` 用了严格小于。
当 `share == ceiling` 时 `tree_bound = None`，走 `spec_task_ceiling` 分支
（`task_projection.rs:904-915`，action = `raise_spec_task_ceiling`）。可此时
`effective_ceiling = min(ceiling, share)`，**单独提高 ceiling 一条都放不出来** —— 恰是这段注释
（`task_projection.rs:868-873`，援引 §12.2 C「每条诊断欠读者一个可行的下一步」）声称要避免的形状。
默认配置直接命中：ceiling 默认 32、孤根显式 `tree_task_budget=32` ⇒ share=32=ceiling。

**触发条件**：`share == ceiling` 且有被裁掉的候选声明。

**我实际跑过的验证**：探针 `probe_share_equal_to_ceiling_attribution`（root+child，B=2，
child ceiling=1 ⇒ share=1=ceiling，两条声明）：
`PROBE3 code=spec_task_ceiling action=Some("raise_spec_task_ceiling") args={ceiling:1, occupied:0}`，
`PROBE3 share=TreeShare { budget: 2, members: 2, share: 1 }`。已复原。

**最小修法**：`task_projection.rs:878` 改为 `share.share <= ceiling`。既有
`a_tighter_wave_ceiling_still_reports_the_ceiling_diagnostic`（`ceiling < share`）不受影响，仍绿。
补一条 `share == ceiling ⇒ tree_budget_exhausted` 的用例。

### MAJOR-3 —— B3 的 AST 枚举只覆盖一种写法；真正危险的那种写法无人看守

**结论**：`wave_tree.rs:322-345` 只遍历 `syn::File::items` 顶层的 `Item::Const`，
且要求初始化式**恰好**是 `concat!` 宏。三种写法可零告警绕过登记，另有一种危险写法根本不在
枚举范围内 —— 而 `wave_tree.rs:11-15` 的模块文档对此下了全称断言。

**我实际跑过的验证**（每次单点改 `wave_tree.rs`，跑
`cargo test -p calm-truth --lib every_bounded_tree_cte_expansion_is_registered`，全部复原）：

| 绕过写法 | 结果 |
|---|---|
| 常量放进同文件的内联 `pub mod hidden_probe { … }` | **仍绿**（`items` 不递归 `Item::Mod`） |
| 包装宏：`macro_rules! probe_members_sql` 内部展开 `concat!(bounded_wave_descendant_cte!(), …)`，常量写 `= probe_members_sql!()` | **仍绿**（`mac.path` 不是 `concat`） |
| `pub static` 而非 `pub const` | **仍绿** |
| **手写、无 `depth` 谓词的 `WITH RECURSIVE … JOIN down ON w.parent_wave_id = down.id`** 顶层常量 | `cargo test -p calm-truth --lib wave_tree` **29 passed / 0 failed**（只有我自己的探针红）——**无人看守的正是唯一危险形状** |

前三种展开的仍是宏、天生有界，安全后果为零；**第四种才是模块存在的理由**，而它零覆盖。
r1 的 R1-B3 变异恰好挑了枚举器认得的那一种写法，属「变异挑门禁、不挑风险」。

**最小修法**：把断言换成 fail-closed 的反向扫描 —— 对 `wave_tree.rs`（以及全 crate `.rs`）
做源码级检查：凡出现 `WITH RECURSIVE` 且文本含 `parent_wave_id`，必须同时含
`depth <= ?2`；并把该扫描扩到 `crates/` 全目录（目前另一处 `wave_vcs/gc.rs:271` 与 waves 无关，
可显式白名单）。这条既覆盖新增未登记，也覆盖手写无界，比枚举 `const` 形状稳。

---

## 三、MINOR

- **MINOR-1（恒真断言）**：`child_wave_adapter.rs:891-897` 的
  `assert_eq!(before, after, "a shape-refused creation must write nothing")` 不可能失败 ——
  上一行是 `drop(tx)`（回滚），无论守卫在不在，都不会有行落库。实测：把
  `can_add_tree_member` 改成恒 `true` 后，该用例在 `child_wave_adapter.rs:885` 的
  `unwrap_err()` 就 panic 了，**根本走不到**那条计数断言。修法：把守卫拆到
  `prepare_tx_and_advance`（真正 commit 的路径）上断言，或删掉这条装饰性断言。
- **MINOR-2（单向性质）**：`enforcement_points_are_compatible_...` 只买「点一不能太松」。
  `can_add_tree_member` 收得过紧（如改成 `< budget`）该测试仍绿；紧的方向目前只由
  adapter 用例里 B=2 建第一个 child 的 `unwrap()` 顺带买到。另外 `B ∈ {0,1}` 与 `N > B`
  的组合被 `continue` 全部跳过，边界零断言。修法：加一行
  `assert!(can_add_tree_member(b, b - 1))` 与 `assert!(!can_add_tree_member(b, b))`。
- **MINOR-3（文本锁）**：`quota_member_sql_keeps_its_total_order_definition`
  （`wave_tree.rs:311-316`）锁的是字面串。等价改写（`ORDER BY w.created_at ASC, w.id ASC`）会**误红**；
  在其后追加 `LIMIT` 之类不会红。已知它是「删整句 ORDER BY 行为用例仍绿」的唯一补丁
  （mutation-map §5.2），可接受，但值得在注释里写明它锁的是文本不是行为。
- **MINOR-4（fixture 清单无集合门）**：`crates/calm-server/tests/cases/migration_0068_projection_policy.rs:9-17`
  是手工维护的 `include_str!` 清单，本轮 R1-M7 补进了 0072，但**没有补上「清单 == migrations 目录」
  的集合相等元测试**，0073 会原样复发同一个洞（fix 打了实例、没打类）。
- **MINOR-5（两侧缺参回退不一致）**：`share` 缺失时 Rust 走 `unwrap_or_default()` ⇒ 0 ⇒ 零份额文案
  （`calm-types/src/report_blocks/tasks.rs:245-249`），web 的 `d.messageArgs.share === 0`
  为假 ⇒ 走正份额文案并渲染出 `so this wave can hold .`（空洞）。实跑 web 探针，实际输出
  `"…so this wave can hold . Raise the limit on the top wave or let an in-progress task in this wave finish."`。
  当前唯一生产者恒填 `share`，故只是 MINOR。修法：`task.tsx:76` 改成 `(d.messageArgs.share ?? 0) === 0`。

---

## 四、可以合入了吗

**NO** —— 至少 MAJOR-1 要先修（孤根 NULL 预算绕过默认树预算 + 两个强制点对同一 wave 用不同的 B，
且与 §8 v5 框写死的「可证」条件不符）。MAJOR-2 是一个字符（`<` → `<=`），MAJOR-3 是把
枚举门换成 fail-closed 扫描，两条建议同轮一并做。MINOR 可随修复轮顺手带上，不单独阻塞。
本轮未发现 BLOCKER：确定性配额分割本身、点一点二的合取关系、并发准入、`RootUnresolved`
的 fail-closed 与读态保全，均实测成立。

---

```
$ git status --short
（空）
```
