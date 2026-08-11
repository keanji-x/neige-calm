# #985 切片 6 PR-B — 实现评审 r10（channel: subagent）

范围 `c71e4132..7471f802`；重点复核 `7471f802`（仅 1 个测试文件 + 4 份文档，**零生产改动**，
`git show --stat 7471f802`）。环境：`.local-bin` nextest、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。
web 本轮只做结构性复核，**未实际执行**（本 worktree 无 `web/node_modules`）。

## 一、同毫秒抽签的裁决是否站得住

**同意**。逐条查证：

1. **持久化全序成立**。`waves.created_at` 生产**从不被更新**（全仓 `UPDATE waves SET` 38 处，唯一写
   `created_at` 的是测试 helper `wave_tree_budget_tests.rs:78`）；`waves.id` 无 `COLLATE`
   （`migrations/0001_init.sql:21`），SQLite BINARY 序与 Rust `String` 序一致 ⇒
   `ORDER BY w.created_at, w.id`（`wave_tree.rs:101`/`:118`）对同一批行恒定 ⇒ rebuild 稳定。
2. **`Σ share = B` / D.1 #11 不受影响**。`deterministic_share`（`wave_tree.rs:138-146`）只读
   `(budget, members, index)`，均来自持久 shape，不读投影输出；`wave_tree.rs:400` 求和门 +
   `wave_tree_budget_tests.rs:411/434` 两条 rebuild 门覆盖之。
3. **两强制点相容性不受影响**：`can_add_tree_member`（`wave_tree.rs:150-152`）是 `N+1<=B`，与顺序无关。
4. **诊断 minimum 仍可执行**：`tree_share_from_member_inventory`（`wave_tree.rs:264-273`）在**实际持久
   顺序**上求最小可行 B ⇒ 抽到哪边返回的都是真能解冻的动作；新用例 1653-1661 行实跑验证准入恢复。
5. **没有别的已声称性质被破坏**：设计 §8 对余数只有一句机械定义（`docs/_985-s6-design.md:798`），
   全仓无「按创建顺序公平/先创建先拿」类声称 ⇒ 被破坏的只有创建序可预测性/公平性。
6. **跨路径一致性**：`wave_report.rs:221` 的 `wave_tree_spec_inventory_by_member` 虽用 `ORDER BY d.id`，
   但 `require_tree_budget_postcondition`（`:231-253`）按 `member_id` 查 `BTreeMap`，不按下标配对，无错配。

## 二、§12.1 #24 与设计旁注是否如实

如实。`docs/architecture/985-doc-as-plan.md:1630` 与 `docs/_985-s6-design.md:806-812` 的三段（全序
买到 rebuild 稳定 + Σ=B、没买到创建序可预测、完整修法是事务内分配持久单调 `quota_order`）与上面
查证一一对上。「只提高时间精度仍会 tie」准确（`wave_create_tx` 每次独立取时钟，精度只降概率不消除）；
「随机 id 兜底没解决抽签」亦准确。

## 三、两条用例是否钉住裁决方向（全部实跑）

- 基线：两条用例 `PASS`（0.119s / 0.129s）。修复后连续 **25/25 PASS**（CI profile，独立进程）。
- **R10-B3 复核通过**：把 `WAVE_TREE_MEMBERS_WITH_FIXED_SPEC_SQL`（`wave_tree.rs:118`）改成
  `ORDER BY w.created_at, w.id DESC` ⇒ `equal_created_at_with_child_id_first_requires_ten_to_unfreeze`
  **RED**，实际诊断退成 `at least 9`；同一变异下 `legacy_...across_the_tree` 仍绿 ⇒ 新用例是唯一钉住
  该 minimum 走向的门。已 `git checkout -- .` 复原。
- **R10-B1 复核通过（根因而非掩盖）**：删掉 `wave_tree_budget_tests.rs:1392-1393` 两行时间夹具后
  循环复跑，第 7 次 / 第 2 次分别复现 `at least 10` 红（我跑了两轮循环）。与排查报告的「第 8 次」
  同一概率现象，根因描述属实。已复原。

## 四、分级发现

**BLOCKER：0。MAJOR：0。**

### MINOR-1 — `WAVE_TREE_MEMBERS_SQL` 的 tie-break 方向没有任何门（假门形状）

- 触发：只改 `wave_tree.rs:101` 的 `ORDER BY w.created_at, w.id` → `... w.id DESC`。
- 证据：`wave_tree.rs:394` 的静态门用 `normalized.contains("ORDER BY w.created_at, w.id")`，
  而 `"... w.id DESC"` **仍包含该子串** ⇒ 门不会红。行为侧也无覆盖：
  `quota_remainder_breaks_equal_created_at_ties_by_id`（`:382`）走 `share_of` → `wave_tree_term`
  → 用的是 `:118` 那条 SQL，碰不到 `:101`。
- **我实跑的验证**：施加该变异后 `cargo nextest run -p calm-truth -p calm-server --locked`
  = **2707/2707 PASS，STILL-GREEN**。已复原。
- 不是 NO：当前两常量 ORDER BY 一致，无可达错值，属**缺门**。若日后分叉，`wave_report.rs:181`
  整树重投影的下标序会与 `wave_tree_term` 不一致（份额错配，Σ 仍为 B）。
- 最小修法：`:394` 改 `ends_with`/等值，或加「两常量 ORDER BY 子句相同」静态门。建议另开 issue。

### MINOR-2 — §12.1 #24 未点名最用户可见的那一面：PATCH/改形 409 的抽签

- `require_tree_budget_postcondition`（`wave_report.rs:238-246`）按「每成员 live ≤ share」判 409；
  余数归属抽签 ⇒ 等价创建序列下同一次 `tree_task_budget` PATCH / 改形可能一次 200 一次 409。
- 这在语义上已被 #24 的「容量呈抽签」覆盖，只是没写成用户可见症状。**建议**在 #24 追一句即可。

### MINOR-3 — 新用例 1607 行 `assert!(child < root, ...)` 恒真（由 1599-1603 构造直接蕴含），无害。

### 同类残留扫描（无发现）

按「≥2 wave + `link` + 未固定 `created_at`」机扫三个测试文件，命中 8 个函数：
`:482`(B=4,N=2)、`:522`(B=2,N=2)、`:604`(B=32,N=2) 余数皆 0，其余五个与份额无关。
**没有同形状的潜在 flaky 残留。**

### 修复点新洞

无。`7471f802` 零生产改动；`legacy_...across_the_tree` 仍是精确 minimum=9（未放宽成 `{9,10}`），
同毫秒分支由新用例独立覆盖。

## 五、可以合入了吗

**YES。** 0 BLOCKER / 0 MAJOR；三条 MINOR 均无可达错值，MINOR-1 建议另开 issue。

```
$ git status --short
?? docs/_985-s6b-impl-review-r10-subagent.md
```
