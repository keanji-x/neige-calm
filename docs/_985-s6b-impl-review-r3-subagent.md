# #985 切片 6 PR-B —— 实现评审 r3（收敛检查 / CHANNEL_NAME = subagent）

范围 `c71e4132..0f8e30ab`。环境：`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。
所有「实测」条目均为在 `/tmp/wtb3` 就地打补丁 → 跑目标测试 → `git checkout --` 复原。

基线：`cargo test -p calm-truth --lib` **350 passed / 0 failed**；
`cargo test -p calm-truth --test bounded_wave_tree_sql` **4 passed**。

结论：**BLOCKER 1 / MAJOR 2 / MINOR 5 —— 不可合入。**

---

## BLOCKER

### B-1 短路里的 `ceiling` 与投影的 `ceiling` 不同源：`spec_task_ceiling IS NULL` 的孤根整条树预算失效

- **结论**：修订轮 2 把「有效 **B**」统一到了 `wave_tree_budget()`，但比较式的**另一边** `ceiling`
  引入了**第二个真源**。`wave_tree_shortcut` 直接把裸列解成 `i64`
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:160-168`，`w.spec_task_ceiling` → NULL 解成 **0**），
  而投影用 `row.spec_task_ceiling.unwrap_or(DEFAULT_SPEC_TASK_CEILING)`
  （`crates/calm-truth/src/db/sqlite/task_projection.rs:466-469`，NULL → **32**）。
  于是 `budget >= ceiling` 退化成 `budget >= 0`（恒真）→ 返回 `NotInTree`
  （`wave_tree.rs:181-183`）→ `effective_ceiling = state.ceiling = 32`，**显式设置的 tree_task_budget 被整条丢弃**。
- **触发条件**：孤根 wave（无父无子）+ `PATCH spec_task_ceiling: null`（路由自己文档化的
  "pass null to reset to the kernel default"，`crates/calm-server/src/routes/waves.rs:1274-1281`；
  `wave_update_tx` 对 `Some(None)` 直接 bind NULL，`crates/calm-truth/src/db/sqlite/wave.rs:206-212`）
  + `tree_task_budget < 32`。
- **证据/实测**：临时用例 `r3_probe_null_ceiling_with_tiny_tree_budget`
  （`wave_tree_term` 打印 `NotInTree, tree_cte_queries: 0`）：
  `tree_task_budget=1`，声明 5 个 key → **5 个 schedulable，期望 1**，
  `cargo test -p calm-truth --lib r3_probe_null_ceiling_with_tiny_tree_budget` → **FAILED（left: 5, right: 1）**。
  同批的 `r3_probe_null_ceiling_still_projects` 通过，说明这不是解码报错、而是**静默错值**——更坏。
  现有 `resetting_an_explicit_budget_to_null_keeps_the_default_bound`（`wave_tree_budget_tests.rs:810`）
  只把 **budget** 置 NULL，从没把 **ceiling** 置 NULL，所以整条路径无覆盖。
- **最小修法**：`wave_tree_shortcut` 读 `Option<i64>` 并复用同一个默认值（把
  `DEFAULT_SPEC_TASK_CEILING` 提到 `wave_tree.rs` 或 `pub(super)` 后共用），
  即 `row.map(|(in_tree, c): (i64, Option<i64>)| (in_tree == 1, c.unwrap_or(DEFAULT_SPEC_TASK_CEILING).max(0)))`。
  外加一条 `ceiling=NULL, budget=1` 的验收（本条 probe 可直接落地）。

---

## MAJOR

### M-1 B2 性质门只扫 `calm-truth/src`；无界递归 CTE 放到 calm-server 完全漏过

- **结论**：门的根目录是 `Path::new(env!("CARGO_MANIFEST_DIR")).join("src")`
  （`crates/calm-truth/tests/bounded_wave_tree_sql.rs:200`）。但**本轮之前**这两条 CTE 就住在
  `crates/calm-server/src/operation/child_wave_adapter.rs`（本 commit 才搬走），
  而该文件今天仍在执行 `WAVE_ROOT_DEPTH_SQL` / `WAVE_BOUNDED_PATH_SQL`
  （`crates/calm-server/src/operation/child_wave_adapter.rs:24-30, 148`）。
  「不需要登记表」的前提是「性质对**所有**代码成立」，现在只对一个 crate 成立。
- **实测**：把一条无 depth 截断的 `WITH RECURSIVE ... parent_wave_id ...` const 追加到
  `crates/calm-server/src/operation/child_wave_adapter.rs` 末尾 →
  `cargo test -p calm-truth --test bounded_wave_tree_sql` → **4 passed（STILL-GREEN）**。已复原。
- **最小修法**：把扫描根从 `CARGO_MANIFEST_DIR/src` 改成 workspace 根下 `crates/*/src`
  （`CARGO_MANIFEST_DIR/../..`），并把 `migrations/*.sql` 一并作为纯文本喂给同一个 `sql_tokens` 检查。

### M-2 性质门的「性质」写错了位置：外层 `WHERE depth` 与省略 `RECURSIVE` 两种真实无界写法都放行

- **结论**：`recursive_parent_cte_is_bounded` 只在 CTE **文本切片**（从 `with recursive` 到下一个
  `with recursive` 或末尾，`bounded_wave_tree_sql.rs:171-196`）里找任意 `where … depth <`，
  **不区分截断在递归臂内还是外层 SELECT**；识别又硬性要求 `with` 后紧跟 `recursive`
  （`bounded_wave_tree_sql.rs:179`）。两个漏洞都对应**真会死循环**的 SQL。
- **实测（临时加进 `bounded_wave_tree_sql.rs` 的 probe，已复原）**：
  - `r3_probe_outer_where_depth`：`) SELECT id FROM down WHERE depth <= ?2` → 门判定 **无违规（漏过）**；
    同一条 SQL 直接喂 `sqlite3 :memory:`（2-环 a↔b）→ `timeout 5` **exit=124（不终止）**。
  - `r3_probe_no_recursive_keyword`：`WITH down(id) AS (… parent_wave_id …)`（省略 `RECURSIVE`）→ 门判定 **无违规**；
    `sqlite3 :memory:` 实测该写法**照样递归**（`LIMIT 5000` 取到 5000 行）。
  - 二者均为 assert 失败，即门当前**不红**。
- **最小修法**：(a) 识别改成 `with`（`recursive` 可选）；(b) 切片改成到 CTE 定义体的**配对右括号**为止，
  depth 谓词必须落在该括号内（而不是整条语句内）。两条都只动这个 test 文件。

---

## MINOR

- **m-1 误红面**：`?2 >= down.depth` 这种反序合法截断会被判违规（probe `r3_probe_reversed_operand_order_false_red` 实测**被标违规**）；
  同一语句里出现无关递归 CTE + `parent_wave_id` 也误红（`r3_probe_unrelated_recursive_cte_false_red` 实测误红）；
  更麻烦的是 `decoded_string_groups` 会把**整个文件的所有字符串字面量拼成一个 group**
  （`bounded_wave_tree_sql.rs:47-49`），实测两条**互不相干**的常量（一条含 `WITH RECURSIVE`、一条含
  `parent_wave_id`）同文件即误红（`r3_probe_two_literals_in_one_file_false_red`）。误红会诱导后人削弱门。
  修法：切片限定在单个字面量 group 内，并接受 `>=`/`>` 的反序形式。
- **m-2** 门不覆盖 `include_str!` / `migrations/*.sql` / `query_file!`（同 M-1 修法一并解决）。
- **m-3** 两套 SQL 注释剥离实现语义不同：`wave_tree.rs:294-309`（行内 `--` 直接 split，无字符串状态机）
  vs `bounded_wave_tree_sql.rs:53-125`（带引号状态机）。今天两处输入都是固定常量所以无害，但重复实现迟早分叉。
- **m-4** `tree_task_budget` 只写不可读：只出现在 `WavePatch`（`crates/calm-truth/src/model.rs:178-183`、
  `web/src/api/generated.ts:2252`），`Wave` 结构体/GET 无此字段。诊断文案让用户「raise the limit on the top wave」，
  但 UI 上没有可读回的值。与 0068 `spec_task_ceiling` 的既有做法一致，故只记不拦。
- **m-5** web 文案从不渲染 `root_wave_id`（`web/src/pages/report-blocks/task.tsx:76-79`），
  Rust 句子却点名了根 wave（`crates/calm-types/src/report_blocks/tasks.rs:277-287`）；多树场景读者不知道该去哪个 wave 调。

---

## 一、修订轮 2 的五处，哪几处修出了新洞

| 修法 | 是否修出新洞 | 依据 |
|---|---|---|
| **1. B2 性质检查** | **是（M-1 + M-2 + m-1/m-2）** | 只扫一个 crate；截断位置不校验；缺 `RECURSIVE` 关键字整条漏过；同时有三类误红 |
| **2. B1 新短路条件** | **是（B-1，BLOCKER）** | 有效 B 统一了，`ceiling` 却没有：`NULL→0` vs `NULL→32`，孤根 + NULL ceiling 时预算整条失效 |
| **3. M1 成员/库存守卫解耦** | 否 | 两条守卫各自可独立触发（R2-M1a/M1b 我方复核逻辑成立）；`parent_wave_id` 生产写入点唯一（`child_wave_adapter.rs:202`），无第三条绕行路径；`can_add == 每份额 > 0` 的双向性质由 `enforcement_points_are_compatible_for_every_budget_and_member_count` 全域枚举购买 |
| **4. M2 剥注释 + 反序夹具** | 否 | 实测：把 `ORDER BY w.created_at, w.id` 改成 `-- ORDER BY …` → `quota_member_sql_keeps_its_total_order_definition`、`quota_remainder_follows_created_at_not_insertion_order`、`shares_over_a_real_tree_sum_to_the_budget`、`zero_share_diagnostic_explains_the_shape_and_effective_actions` **4 条同时 RED**。同 `created_at` 的退化由 `quota_remainder_breaks_equal_created_at_ties_by_id` 单独守 |
| **5. 诊断跨源校验** | 否（仅 m-5） | 用 node 复跑 web 侧正则抽取：对当前 `tasks.rs` 抽出 3 组映射；把 `raise_tree_task_budget` 改名后抽取结果随之变化 → `toBe` 断言必红。反向由 `Diagnostic::coded` 内的 action 断言（`tasks.rs:179-185`）守。**注意：本 worktree 无 `web/node_modules`，vitest 未实际执行**，此条为结构性复核而非跑测 |

## 二、旧变异条目里，哪几条已经失真（抽查 6 条）

全部**重新实跑**，**没有发现失真**：

| 条目 | 变异 | 目标测试 | r3 复跑结果 |
|---|---|---|---|
| M9 | 去掉非树短路 | `a_non_tree_wave_runs_zero_recursive_tree_queries` | **RED（1 failed）** |
| R2-B1 | 短路无视有效 B（恒 `NotInTree`） | `an_explicit_budget_applies_to_a_singleton_root` + `resetting_an_explicit_budget_to_null_keeps_the_default_bound` | **RED（2 failed / 20）** |
| R2-m2 | `share <= ceiling` → `<` | `an_equal_tree_share_reports_the_tree_knob` | **RED（1 failed）** |
| M11 | `deterministic_share` 去掉余数 | `shares_over_a_real_tree_sum_to_the_budget` 等 | **RED（5 failed / 29）** |
| M4 | `RootUnresolved` 改成跳过树项 | `unresolvable_root_fails_closed_for_every_declaration` + `unresolved_root_preserves_withdrawal_and_deleted_block_read_verdicts` | **RED（2 failed / 20）** |
| R2-M2 | 注释掉真实 `ORDER BY` | 见上表第 4 行 | **RED（4 failed / 29）** |

**但变异映射的一句话结论已经过期**：`docs/_985-s6b-mutation-map.md:110-111` 写
「crate-wide token/string 扫描直接购买『任何触及 `parent_wave_id` 的递归 CTE 都有 depth 截断』」。
按 M-1/M-2 实测，它购买到的实际是「**calm-truth/src 内、写成 `WITH RECURSIVE`、且语句任意位置出现
`where … depth <` 的**递归 CTE」。R2-B2a–d 四条变异全部落在这三个限定内，所以它们红得**真实但不充分**。
该行需要按修好后的门重写。

## 三、可以合入了吗

**NO。** 存在 1 条 BLOCKER（B-1，用户可达路径上树预算静默失效，已用可复现失败用例证明）
与 2 条 MAJOR（B2 性质门的覆盖面与性质形状）。三条的修法都很小（一个 `Option<i64>` +
测试文件里的扫描根/切片规则），建议同一修复轮内一并处理，并把本篇的
`r3_probe_null_ceiling_with_tiny_tree_budget`、`r3_probe_outer_where_depth`、
`r3_probe_no_recursive_keyword` 三条 probe 落地成常驻用例。

---

```
$ git status --short
?? docs/_985-s6b-impl-review-r3-subagent.md
```
