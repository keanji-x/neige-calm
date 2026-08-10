# #985 切片 6 PR-B 实现评审 r9（codex）

评审范围：`c71e4132..d2adea3d`。结论：BLOCKER 0 / MAJOR 1 / MINOR 0。

## BLOCKER

无；核心树上界的既定依据链未重审，当前容量合成仍为
`min(ceiling-本地 block in-flight, share-本地全部固定 spec)`；
`crates/calm-truth/src/db/sqlite/task_projection.rs:628-649`。

## MAJOR

### MAJOR-1：冻结双归因漏掉 `ceiling < occupied`，两条动作照做一次仍 `0→0`

- **结论**：r8 新分支把所有 `ceiling_capacity==0` 都登记成同一个 local raise 动作，却不给
  本地可执行目标；`ceiling==occupied` 时 `ceiling+1` 有效，`ceiling<occupied` 时无效。
  分支与无目标 action 生成点为 `crates/calm-truth/src/db/sqlite/task_projection.rs:990-1032`。
- **触发条件 / 错值**：可达升级态 `N=2,B=32`，index 0 有 17 条 legacy，目标 index 1 有
  3 条 block in-flight，且用户把目标 ceiling 从 3 降到 1。tree share 均为 16，整树冻结；
  诊断给 `minimum_tree_task_budget=34` 和 `raise_spec_task_ceiling`。把 B 设为 34、按当前验收的
  “执行一次”语义把 ceiling 设为 2 后，tree 已解冻但本地 `2-3=0`，准入仍 **0→0**。
  容量算式见 `crates/calm-truth/src/db/sqlite/task_projection.rs:628-649`；产品文档明确承认
  “人把 ceiling 调低到既有在飞行数以下”的可达退化，见 `docs/architecture/985-doc-as-plan.md:2062`，
  REST 也只拒绝负数，见 `crates/calm-server/src/routes/waves.rs:1275-1280`。
- **证据**：504 格先删除目标全部 block 行，只生成 `legacy∈{0,3}`，所以永远没有
  `ceiling_occupied>ceiling`；`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:700-738`。
  它把 local action 固定解释为 `ceiling+1`，见同文件 `:765-789`。新增专测仅买
  `ceiling=3,occupied=3`，见同文件 `:919-969`。Rust tied 文案不报 occupied，web tied 文案也只说
  “raise both”，见 `crates/calm-types/src/report_blocks/tasks.rs:260-286`、
  `web/src/pages/report-blocks/task.tsx:67-73`。
- **我实际跑过的验证**：临时新增并已复原
  `db::sqlite::wave_tree_budget_tests::review_frozen_wave_with_local_ceiling_overage_needs_an_executable_local_target`
  → **FAIL 0/1**，tree minimum 实际为 34，执行 `ceiling 1→2` 与 `B 32→34` 后断言仍不 schedulable；
  常驻等值专测 `a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds` 复原态 **PASS 1/1**，
  其夹具边界见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:919-969`。
- **最小修法**：给 ceiling 诊断携带 `minimum_spec_task_ceiling=ceiling_occupied+1`，Rust/web tied
  文案都明确该值；性质门执行该值，并加入 block occupancy 的 `{0, exactly-full, overfull}` 轴。
  生产已有 `ceiling_occupied`，落点为 `crates/calm-truth/src/db/sqlite/task_projection.rs:628-633,990-1023`。
- **不能登记后合入**：这是 `d2adea3d` 新增冻结双归因分支的同形状回归，且直接推翻本轮
  “执行全部命名动作后同一报告必须多准入”的效果契约；该契约见
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:638-641`。

## 穷举验收本身

- 边界确为 `N=1..3 × B=0..6 × 全 index × C=0..5 × target legacy∈{0,3}`，计数 504；
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:627-636,700-725,797-803`。复原态测试体
  **PASS 1/1，2.705s**；两包全门 **571 passed / 0 failed / 49 skipped，3.125s**，CI 时限可接受。
- 它的效果 oracle 是独立 SQL 行数 `before/after`，不是由 `deterministic_share` 算期望；
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:644-672,781-803`。我把生产条件退回
  `new_share > current_share` 后同名测试 **FAIL 0/1，精确 210 例 0→0**，证明 B1 断言有效；
  生产正确条件见 `crates/calm-truth/src/db/sqlite/task_projection.rs:937-949`。
- 但其 block 删除与 legacy-only 占用轴排除了 MAJOR-1；这不是“建议多测一种写法”，而是测试声称
  覆盖动作有效性却放过上面的可达错值；`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:627-641,721-738`。

## 无解、双归因与 B3 文案

- tree 无解判定本身正确：目标必须 `share>max(current_share,tree_occupied)`，冻结时再与全树
  unfreeze minimum 联合；任一项在 64 内不存在就不登记 action；
  `crates/calm-truth/src/db/sqlite/task_projection.rs:937-987`。上限普通满载、冻结满载和 local 同绑
  均由 `an_unreachable_tree_budget_target_reports_no_raise_action` 锁住，见
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:806-912`，复原态 **PASS 1/1**。
- 我临时恢复无解 tree action 后，该测试 **FAIL 0/1**，并先被构造器的动作可用性断言截获；
  `crates/calm-types/src/report_blocks/tasks.rs:178-194`。说明 B3 Rust 动作门有效。
- B3 Rust/web 语义一致：两端都在 minimum 缺席时只给等待/减成员，不渲染 `at least 0`/空值；
  `crates/calm-types/src/report_blocks/tasks.rs:308-323`、`web/src/pages/report-blocks/task.tsx:85-96`。
  改坏任一侧会由各自门变红：Rust 门见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:842-910`；web 门见
  `web/src/pages/report-blocks/report-blocks.test.tsx:917-925`。web 因无 `web/node_modules`，**未实际执行**。

## 修订轮 8 的四处，哪几处修出了新洞

1. 联合 tree 目标搜索：未发现新洞；条件与联合点见 `crates/calm-truth/src/db/sqlite/task_projection.rs:937-952`。
2. 冻结双归因：**修出 MAJOR-1**；只判零剩余容量，未为本地 overage 给可执行目标，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:990-1032`。
3. 无合法 B 时撤销动作/假数字：未发现新洞；Rust/web 落点见 `crates/calm-types/src/report_blocks/tasks.rs:178-194,308-323`、`web/src/pages/report-blocks/task.tsx:85-96`。
4. 504 格验收：效果 oracle 有效、耗时合格，但 block occupancy 轴被删除，放过 MAJOR-1；
   `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:627-641,700-738,765-803`。

## 可以合入了吗

**NO。** `ceiling=1, occupied=3` 是文档明确允许且 REST 接受的状态；r8 新登记的 local 动作与精确
tree 动作一起执行一次后仍 **0→0**。这是本轮修复点的新回归，不是可另记的覆盖建议；
`docs/architecture/985-doc-as-plan.md:2062`、`crates/calm-truth/src/db/sqlite/task_projection.rs:990-1032`。

## `git status --short`

```text
?? docs/_985-s6b-impl-review-r9-codex.md
```
