# #985 切片 6 PR-B 实现评审 r7（codex，收敛检查）

范围：`c71e4132` → `017d55d9`。结论：**BLOCKER 1 / MAJOR 0 / MINOR 0**。
Rust 均以题定 PATH、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置执行；本 worktree 无
`web/node_modules`，web **未实际执行**，只做结构性复核（copy/test 对应点：
`web/src/pages/report-blocks/task.tsx:81-84`、`web/src/pages/report-blocks/report-blocks.test.tsx:827-832`）。

## BLOCKER

### B1 — “照诊断做一次就增加容量”的性质在平局与多成员余数场景均为假

- **结论**：诊断选择比较的是两个剩余容量，平局固定报本地 ceiling
  （`crates/calm-truth/src/db/sqlite/task_projection.rs:918-926`），而动作契约固定为只提高该旋钮
  （`crates/calm-types/src/report_blocks/tasks.rs:65-72`）。但实际容量是两者最小值
  （`crates/calm-truth/src/db/sqlite/task_projection.rs:629-646`）：平局时只提高 ceiling，tree share
  仍给出原容量，故动作不可能释放准入。这直接违反该 action 的生产契约，不只是少覆盖。
- **触发条件 1 / 错值**：孤根 `ceiling=share=B=2`、同一报告 4 个干净声明；先落 2 行并收到
  `raise_spec_task_ceiling`，按动作把 ceiling 提到 3 后仍只落 **2** 行，而非 `>2`。现有平局测试只断
  code/action（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:630-668`），效果测试只构造
  `(C,B)=(2,4)/(4,2)` 两个严格绑定孤根（`:698-717`），所以实现改坏而常驻测试仍绿。
- **触发条件 2 / 错值**：两成员按 `(created_at,id)` 排 root、child，`B=2` 时 share=`[1,1]`，
  child `ceiling=4`；child 收到 `raise_tree_task_budget` 后把 B 提到 3，确定性余数给 root，share
  变成 `[2,1]`（公式：`crates/calm-truth/src/db/sqlite/wave_tree.rs:128-145`），child 准入仍为
  **1→1**。所以即使非平局，`B+1` 也不保证帮助被诊断成员。
- **实际验证**：临时生产投影测试
  `probe_equal_bounds_action_must_increase_admission` 与
  `probe_tree_action_must_help_the_diagnosed_member`：**RED 0/2**，分别打印 `2 -> 2`、`1 -> 1`；
  同时复原态 `the_diagnosed_capacity_action_increases_admission` **PASS 1/1**，证实现有验收漏掉反例。
  临时测试后已执行 `git checkout -- .`。
- **最小修法**：平局不能继续复用单旋钮 `raise_spec_task_ceiling`；增加“同时提高 ceiling 与 tree
  budget”的复合 action/诊断（或明确列出两个都要改）。tree action 还须携带能使当前 member 的 share
  真正 `+1` 的最小 B 目标，而不是隐含 `B+1`。把效果验收扩成 `N/index × <,=,> × legacy/freeze`
  表，每例读取动作并执行一次，再断言同一报告准入严格增加；生产归因落点在
  `crates/calm-truth/src/db/sqlite/task_projection.rs:923-965`。

## 修订轮 6 的五处，哪几处修出了新洞

1. **删除短路/`NotInTree`：未见新洞。** 枚举只剩 fail-closed 与 Share
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:154-163`）；普通读/写统一走同一 exhaustive match
   （`crates/calm-truth/src/db/sqlite/task_projection.rs:606-616`），整树 rebuild 显式注入 Share
   （`crates/calm-server/src/wave_report.rs:203-215`）。孤根现固定跑两条 CTE
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:188-225`），没有残留分支或恒真验收。
2. **诊断效果验收：修出 B1。** 它只买了两个严格绑定的 `N=1, occupancy=0` 实例
   （`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:698-717`），没有买平局、余数或 freeze。
3. **平局取本地旋钮：修出 B1。** `min` 平局后改变任一单侧都不改变最小值；代码却承诺本地动作
   能释放准入（`crates/calm-truth/src/db/sqlite/task_projection.rs:918-965`）。
4. **workspace fail-closed：没有静默漏扫，但会显式拒绝合法 glob。** helper 把 member 字面 join 后
   要求其 `Cargo.toml` 存在（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:489-505`）。临时把当前
   manifest 改为 Cargo 合法 `members=["crates/*"]` 后，性质门 **RED 0/1** 并明确要求先展开 glob；
   当前根本身就是 virtual manifest 且显式成员基线 **PASS**（`Cargo.toml:1-18`）。这是安全侧维护限制，
   不是运行时洞；若要支持 glob，最小改法是用 `cargo metadata --no-deps` 取展开后的 member roots。
5. **旧 schema fixture：是最小补列，未掩盖 migration。** `waves.created_at` 自 0001 就是基线列
   （`crates/calm-truth/migrations/0001_init.sql:20-28`），当前成员排序生产查询必读它
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:94-102`）；fixture 只补该既有列
   （`crates/calm-server/tests/cases/migration_0068_projection_policy.rs:68-84`）。临时删除后目标验收
   **RED 0/1**：`no such column: w.created_at`；复原后 migration 两测 **PASS 2/2**。

## 旧变异证据保质期抽查

- **M9**：旧“孤根零递归”已正确删除；临时重引入返回正确 Share 但 `tree_cte_queries=0` 的孤根早退，
  `a_singleton_tree_runs_two_constant_size_recursive_queries` **RED 0/1（0!=2）**；新语义断言位于
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:838-866`。
- **R3-B1c（map 中 R1-B1c）**：临时恢复孤根无限 share 绕过，
  `an_explicit_budget_applies_to_a_singleton_root` **RED 0/1**，B=1 时后两条错误准入；断言在
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:869-894`。
- **R5-m1**：旧 shortcut 比较已无落点；按当前替代点把归因 `<` 变回 `<=`，
  `an_equal_tree_share_reports_the_local_ceiling_knob` **RED 0/1**；选择点在
  `crates/calm-truth/src/db/sqlite/task_projection.rs:923-926`。三次变异后均已 `git checkout -- .`。

## `Σ_v live_spec(v) ≤ B` 在含 legacy 的真实部署下成立吗

**对本 build 新准入的状态成立；作为升级瞬间的无条件字面命题不成立。** legacy/non-block 非终结行
进入固定占用（`crates/calm-truth/src/db/sqlite/wave_tree.rs:104-118`），当前 wave 容量会扣它们且任一
成员超 share 时整树冻结（`crates/calm-truth/src/db/sqlite/wave_tree.rs:239-256`、
`crates/calm-truth/src/db/sqlite/task_projection.rs:630-645`）；B/N 写再有 per-member 与 total 两道后验
（`crates/calm-server/src/wave_report.rs:219-248`）。两组 r6 构造分别 **PASS**，库存锁为 32/6
（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1037-1091`）。若升级前已 `K>B`，旧行不可裁，
字面 `Σ≤B` 暂时为假，但所有新容量为 0、只会随 legacy 终结收敛；这是明示退化契约
（`docs/architecture/985-doc-as-plan.md:1223-1233`），本轮未发现主动新增超额路径。

## 实际回归

- `calm-truth --lib wave_tree_budget_tests`：**26/26 PASS**；`bounded_wave_tree_sql`：**18/18 PASS**。
- `calm-server --lib operation::child_wave_adapter::tests`：**12/12 PASS**；policy PATCH + 0068 migration：
  **10/10 PASS**。测试落点分别见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:1`、
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:531`、
  `crates/calm-server/src/operation/child_wave_adapter.rs:136`、
  `crates/calm-server/tests/cases/migration_0068_projection_policy.rs:43-59`。
- web 因无 `web/node_modules` **未实际执行**；实现方 1232 / fe 758、编排方 3406/0 仅作已给事实引用。

## 可以合入了吗

**NO。** 不修 B1 的可达错误后果是：`C=S=2` 的孤根按诊断把 ceiling 提到任意大，准入仍为 **2**；
两成员后序 child 按诊断把 B 从 2 提到 3，准入仍为 **1**。用户执行系统承诺的 recovery action 后
没有恢复任何容量，且现有“动作有效”验收仍绿；错误选择与动作生成在
`crates/calm-truth/src/db/sqlite/task_projection.rs:918-965`。

## git status --short

```text
?? docs/_985-s6b-impl-review-r7-codex.md
```
