# #985 切片 6 PR-B 实现评审 r6（codex，收敛检查）

范围：`c71e4132` → `7ebbfd48`。结论：**BLOCKER 1 / MAJOR 0 / MINOR 3**。
Rust 均以题定 PATH、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置执行（环境约束见
`docs/_985-s6b-impl-notes.md:173`）。本 worktree 没有 `web/node_modules`，web/vitest **未实际执行**，
仅结构性复核；实现方记录 web 1232、fe 758、编排方 3402/0（`docs/_985-s6b-impl-notes.md:165-171`）。

## BLOCKER

### B1 — `budget > ceiling` 的孤根短路会绕过新增 legacy 占用，普通投影可提交 `Σ live_spec > B`

- **结论**：`wave_tree_term` 仅凭数值 `budget > ceiling` 返回 `NotInTree`
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:222-240`）；但 per-wave ceiling 刻意只扣 block 在飞，
  legacy 只在 tree capacity 中扣（`crates/calm-truth/src/db/sqlite/task_projection.rs:625-647`）。一旦短路，
  `tree_share=None`，新增的 legacy 口径完全失效。这个洞正是本轮第 1 项与第 4 项交叉产生。
- **触发条件**：孤根默认 B=32、ceiling=31、已有 2 条升级 legacy running；正常 report 再声明 31 个
  不同 block key。普通写经 `project_tasks_tx → wave_tree_term`（`crates/calm-truth/src/db/sqlite/task_projection.rs:987-1004`），
  返回 `NotInTree` 后 31 个全落行，库存从 2 增到 **33 > 32**；不会经过只用于 B/N 改动的整树后验
  （`crates/calm-server/src/wave_report.rs:177-220`）。
- **实际验证**：临时测试
  `db::sqlite::wave_tree_budget_tests::probe_singleton_shortcut_must_not_ignore_legacy_tree_occupancy`
  使用生产 `project_tasks_tx`，结果 **RED 0/1**：`default B=32 exceeded after shortcut: 33`；已复原。
- **最小修法**：孤根 shortcut 同一非递归读取 `non_block_live_spec`；只有
  `budget - non_block_live_spec > ceiling` 才能 `NotInTree`（相等仍由 tree 诊断），或保守地只要存在
  非终结 non-block spec 就返回 `Share`。补上述 2+31 回归；占用谓词复用
  `crates/calm-truth/src/db/sqlite/task_projection.rs:469-472`。

## MINOR

### m1 — sibling overage 冻结时，诊断让用户等待错误的 wave

- **结论/触发条件**：root 固定占用 5 > share 4、child 为 3 < 4 时，child 正确被冻结，却收到
  `occupied=3, share=4` 并声称“this wave's slice ... is used up / let an in-flight task in this wave finish”
  （参数生成 `crates/calm-truth/src/db/sqlite/task_projection.rs:924-950`；Rust 文案
  `crates/calm-types/src/report_blocks/tasks.rs:281-287`；web 同样指向本 wave，`web/src/pages/report-blocks/task.tsx:81-83`）。
  用户即使让 child 任务结束也仍不能解冻 root overage。
- **实际验证**：在
  `db::sqlite::wave_tree_budget_tests::legacy_member_overage_freezes_new_blocks_across_the_tree`
  临时断言冻结诊断须满足 `occupied >= share`，**RED 0/1**，打印 `occupied: 3, share: 4`；已复原。
- **最小修法**：`TreeShare` 携带首个 overage member id/occupancy，冻结诊断明确让该成员终结；或使用不谎称
  local slice 已满的通用冻结文案，并补 Rust + web render 验收（冻结来源目前只压成 bool，
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:301-307`）。

### m2 — TOML 解析 fail-closed，但 Cargo 合法 workspace glob 会让 SQL 门静默空跑

- **结论/触发条件**：语法错、缺 `workspace.members`、非数组/非字符串都会 panic，确实 fail-closed
  （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:471-486`）；但 member 被直接 `workspace.join`，不存在的
  路径由扫描器静默返回（`crates/calm-truth/tests/bounded_wave_tree_sql.rs:489-497`、`:515-520`）。将当前显式成员
  （`Cargo.toml:3-18`）合法改为 `members=["crates/*"]` 后，字面 `crates/*` 不存在，所有 crate 漏扫。
- **实际验证**：临时改上述 glob，
  `every_recursive_parent_wave_cte_in_workspace_members_bounds_its_recursive_variable` **STILL-GREEN 1/1 (0.00s)**；已复原。
- **最小修法**：用 `cargo metadata --no-deps` 取得已展开的 workspace member manifest 路径；至少展开 glob、
  断言每个 root/Cargo.toml 存在且 `files` 非空（当前只断言 member 字符串非空，
  `crates/calm-truth/tests/bounded_wave_tree_sql.rs:496`、`:521-536`）。

## 修订轮 5 的五处，哪几处修出了新洞

1. **占用含 legacy**：树内查询正确排除了 terminal、非 spec、跨树行
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:110-127`）；冻结每次按当前库存重算，pending 可 cancel、其余按状态机终结后解冻
   （`crates/calm-truth/src/db/sqlite/task.rs:178-191`、`crates/calm-truth/src/model.rs:361-378`）。但与孤根 shortcut 交叉修出 **B1**，与文案交叉修出 **m1**。
2. **两条后置条件**：触发正确，未见合法生产序列误拒；member 分支管固定占用超 share，total 是 share-map/库存失配的冗余 fail-closed
   （`crates/calm-server/src/wave_report.rs:229-250`）。同时关闭两分支时，child 409 与 total helper 分别 **RED 0/1、RED 0/1**；已复原。
3. **SQL 合法形状门**：现有生产 CTE 全通过；门只白名单直接参数比较合取叶，CASE/常量/函数/postfix 拒绝
   （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:304-357`）。HAVING、IN、named/bare 参数、CAST 等合法有界改写会误红，
   但可改写为 `alias.depth <= ?N`，属明确的安全侧维护约束，不升级严重性。
4. **`>=` → `>`**：相等诊断分叉已修且验收通过，但数值短路未纳入新 legacy 占用，修出 **B1**
   （`crates/calm-truth/src/db/sqlite/wave_tree.rs:227-235`）。
5. **TOML 结构解析**：解析失败 fail-closed，排版问题已修；Cargo glob 语义未实现，修出 **m2**。

## `acceptance_19` 的时序敏感面判断

### m3 — barrier 已确定，但 1 秒到达 deadline 仍会在重载下误红（PR-A 遗留）

**仍有测试 harness 的 wall-clock 敏感面，但生产顺序断言不再靠时序窗。** `wait()` 入口 hook 建立 happens-before
（`crates/calm-server/src/operation/driver.rs:267-278`），observer 只在收到 hook 后发布 entered
（`crates/calm-server/tests/scheduler.rs:968-976`）；因此原 25ms 假 barrier 已消失。剩余敏感面是两处硬编码 1s：
从 spawn `sweep_all` 到达 barrier 超过 1s 就误失败（`crates/calm-server/tests/scheduler.rs:6353-6360`、`:6436-6443`）。
全量并发、CPU/IO 饥饿或 SQLite writer 调度超过 1s 时会翻；这解释一次全门红、定向与后三次全绿的形状
（`docs/_985-s6b-impl-notes.md:165`）。本片未改该测试；blame/提交边界指向 PR-A，属于 **PR-A 遗留 MINOR，另开 issue**。
实际跑 `acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive`：单次 **PASS 1/1**，随后重复 **20/20 PASS**。
最小修法：保留 Notify barrier，把 1s 改为与本测试其余防挂一致的 30s/统一测试 deadline；不要恢复 sleep。

## 实际回归

- `calm-truth --lib wave_tree_budget_tests`：**23/23 PASS**；`bounded_wave_tree_sql`：**17/17 PASS**。
- `calm-server --lib operation::child_wave_adapter::tests::`：**12/12 PASS**；policy patch：**8/8 PASS**；
  `whole_tree_total_postcondition_rejects_an_over_budget_inventory` 与 singleton equal-bound：各 **1/1 PASS**。
- 所有临时变异/探针后均执行 `git checkout -- .`；对应生产测试位置见
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:873-1050`、`crates/calm-server/src/operation/child_wave_adapter.rs:1127-1244`。

## 可以合入了吗

**NO。** 不修 B1 的可达错误后果不是“覆盖不足”，而是升级库孤根经普通 report 写把 `Σ live_spec` 从 2 推到
**33，超过 B=32**；核心树预算不变量被提交态直接破坏（短路点 `crates/calm-truth/src/db/sqlite/wave_tree.rs:227-240`）。
m1/m2 与 `acceptance_19` 单独都只是 MINOR；修复 B1 后它们不阻止合入。

## git status --short

```text
?? docs/_985-s6b-impl-review-r6-codex.md
```
