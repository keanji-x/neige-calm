# #985 切片 6 PR-B —— 实现评审 ch1（对抗性）

对象：`c71e4132..63940d9c`。全部指控均带 `文件:行号`，全部结论均附**我实际跑过**的变异与结果。
跑法：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH CARGO_BUILD_JOBS=6 cargo nextest`，`NEIGE_CODEX_BIN` 全程未设置。
基线：`cargo nextest run -p calm-truth --lib -E 'test(wave_tree) or test(tree_budget)'` → **22 passed**（绿）。

判据始终是那一句：**被测代码和这条断言，是不是共用了同一个事实来源 / 这条断言能不能失败。**

---

## BLOCKER

### B1. 登记制门禁被 rustfmt 的换行规则击穿：新增一条**生产在用**的有界 CTE 不登记，元测试仍绿

- **结论**：`every_bounded_tree_cte_expansion_is_registered` 不是「登记制」的机器联系，它是「**特定格式**的机器联系」。设计 §7.1 要防的「名单与成员之间没有机器联系」原样存在。
- **攻击**：在 `crates/calm-truth/src/db/sqlite/wave_tree.rs:89` 处新增
  `pub const WAVE_TREE_LEAF_SQL: &str = concat!(bounded_wave_descendant_cte!(), "SELECT id FROM down");`
  —— 长度 99 字符，`cargo fmt -p calm-truth` **保持单行**，于是元测试的针
  `format!("{}(\n    bounded_wave_", "concat!")`（`wave_tree.rs:265`）**匹配不到它**：expansions 仍是 4，`BOUNDED_WAVE_TREE_SQL.len()` 仍是 4 ⇒ 相等 ⇒ 绿。
  为排除「只是 dead_code lint 会兜住」，我又加了生产读者 `wave_tree_leaf_ids()` 并在 `mod.rs` 的 `pub use` 里导出（完全真实的新增走法）。
- **证据**：`crates/calm-truth/src/db/sqlite/wave_tree.rs:257-271`（元测试）、`:92-102`（清单）。针里写死了 `\n` + 4 空格缩进。
- **实际跑过**：
  - 变异一（仅常量，单行）：`db::sqlite::wave_tree::tests::every_bounded_tree_cte_expansion_is_registered` → **STILL-GREEN**（3 tests run: 3 passed）。
  - 变异二（常量 + 生产读者 + `pub use`，无任何 warning）：同测试 → **STILL-GREEN**（3 passed）。
- **为什么是 BLOCKER**：这条元测试是本片唯一挡住「向下 CTE 忘记截断」的**结构性**装置（`a_downward_two_cycle_terminates_quickly` 在删截断时是**挂死**而不是变红，见 `wave_tree_budget_tests.rs:617-619` 作者自述）。它挡不住新增，等于环上不终止的复发路径没有守卫。
- **最小修法**：不要扫源码文本。把宏改成**自注册**形式 —— 用一个 `bounded_tree_sql! { NAME = <ancestor|descendant>, tail }` 声明宏同时产出常量并把它 push 进一个 `inventory!`/`linkme` 式清单；退一步的做法是让针对源码的正则变成 `bounded_wave_(ancestor|descendant)_cte!\(\)`（与空白无关）并断言出现次数 == 清单长度。

### B2. 配额分割的**定义本身**（`ORDER BY created_at, id`）删掉后全绿

- **结论**：v5 裁决把 `share` 定义为「按 `(created_at, id)` 升序、余数给前 r 个」。承重的是这个**顺序**。但删掉排序子句，22 条测试全绿 —— 顺序的**存在性**没有任何失败见证，只有**方向**有。
- **攻击**：`crates/calm-truth/src/db/sqlite/wave_tree.rs:84` 删除 `ORDER BY w.created_at, w.id`。
- **实际跑过**：
  - 删 `ORDER BY` → `-E 'test(wave_tree) or test(tree_budget)'` → **22 tests run: 22 passed**（STILL-GREEN）。
  - 对照：改成 `ORDER BY w.created_at DESC, w.id` → `db::sqlite::wave_tree_budget_tests::shares_over_a_real_tree_sum_to_the_budget` **FAILED**（21 passed, 1 failed）。
- **为什么绿**：`shares_over_a_real_tree_sum_to_the_budget`（`wave_tree_budget_tests.rs:278`）在无排序时恰好拿到 SQLite 的扫描序（rowid ≈ 插入序 ≈ created_at 序），断言 `index<3 得 2` 侥幸成立。**测试与被测代码共用了「SQLite 恰好这么返回」这一个事实来源。**
- **为什么是 BLOCKER**：无 `ORDER BY` 的行序是查询计划的实现细节 —— 加一个索引、换一次 `waves` 的 schema、SQLite 升级都可能改变它。届时 `share` 变成非确定的，**D.1 #11「rebuild ≡ 增量差分」在生产里静默失效**，而 `two_rebuild_orders_over_one_tree_agree_byte_for_byte` 仍会绿（它在同一进程同一计划下跑两遍）。承重性质当前处于「没有守卫」的状态。
- **最小修法**：加一条**只测顺序**的用例：造两个 wave，`created_at` 故意与插入序**相反**（`stamp_created_at(root, 2000)`, `stamp_created_at(child, 1000)`），`B=1, N=2` ⇒ 断言 **child**（created_at 更小）拿到 1、root 拿到 0。删 `ORDER BY` 时该断言必红。再补一条 `created_at` 相等、按 `id` 决胜的用例。

### B3. 强制点一放行、强制点二立刻饿死：树成员数**从不**被任何东西约束，生产路径自己就能把树推进 `share == 0`

- **结论**：`share = floor(B/N)`。当 `N > B` 时部分 wave 的 `share` 恒为 0 —— **永久**不可调度，且与在飞量无关。而强制点一（`child_wave_adapter.rs:154-157`）比的是**在飞 spec 存量** `inventory >= budget`，**根本没看成员数**。于是「本片新增的创建路径」自己就能把树养进饿死区：任务做完 ⇒ inventory 掉回去 ⇒ 继续开子 wave ⇒ N 继续涨。两个强制点口径不一致，一个放行、另一个当场判死。
- **攻击（我自己造的交错，非改坏实现）**：root(`B=2`) + 3 个子 wave（全部走生产 `wave_create_tx` + `wave_update_tx`），树内**零任务**。
- **实际跑过**：临时用例 `ch1_repro_shape_only_share_starves_and_the_advice_is_false`（`-p calm-truth --lib --no-capture`），输出：

  ```
  CH1 tree inventory=0 budget=2 -> point-1 refuses? false
  CH1 wave share=1 members=4
  CH1 wave share=1 members=4
  CH1 wave share=0 members=4      <- 永久饿死
  CH1 wave share=0 members=4      <- 永久饿死
  CH1 schedulable=false
  CH1 total tasks anywhere in tree = 0
  ```
  即：**强制点一此刻还会继续放行更多子 wave**（`0 >= 2` 为假），而已经有两个 wave 的份额是 0。余数给前缀 ⇒ 拿到 0 的恰好是**最新创建的那些**，也就是强制点一刚放进来的那些 —— 子 wave 出生即死。
- **证据**：`crates/calm-server/src/operation/child_wave_adapter.rs:154-157`（点一的谓词）、`crates/calm-truth/src/db/sqlite/wave_tree.rs:113-120`（`deterministic_share`，`members > budget` 时返回 0）、`crates/calm-truth/src/db/sqlite/task_projection.rs:600`（`ceiling.min(share.share)`）。
- **零覆盖**：`wave_tree_budget_tests.rs` 全文没有一条用例的 `members > budget`。`share_helper_matches_the_documented_formula:653` 有 `deterministic_share(2,5,4) == 0`，但只断言了算术，没有断言这在系统层面意味着什么。
- **不是在重裁决**：我**不**建议退回共享计数。修法完全在实现层：强制点一在放行前必须同时满足 `members + 1 <= budget`（即新成员的 share ≥ 1），拒绝理由用同一个 `sub-wave-tree-budget-exhausted` 家族的新码（例如 `sub-wave-tree-too-wide`）。这样点一与点二口径一致，且「出生即死的子 wave」不可达。
- **最小修法**：`child_wave_adapter.rs` 的 `prepare_tx` 在 inventory 检查旁加一次成员数检查（复用 `WAVE_TREE_MEMBERS_SQL`），并加一条验收：`B=2` 的 root + 1 子 wave，第二次创建被拒且理由指名成员数。

---

## MAJOR

### M1. `tree_budget_exhausted` 的「下一步动作」是**假的**（Rust 文案 + 前端 copy 两处）

- **结论**：文案说「**或让树里其它地方的任务做完**」/「wait for tasks elsewhere in the group to finish」。在纯形状配额下这**证明无效**：`share` 只读树形状，`capacity = share - occupied` 里的 `occupied` 只数**本 wave** 的在飞行（`task_projection.rs:619-623`）。别的 wave 做完任何数量的任务，对本 wave 的额度影响**恒为 0**。§12.2 C 要的正是「能改的那个旋钮」，这里给了一个拧不动的。
- **证据**：`crates/calm-types/src/report_blocks/tasks.rs:238-246`（`— raise tree_task_budget on the root wave, or let tasks elsewhere in the tree finish`）、`web/src/pages/report-blocks/task.tsx:76`（`or wait for tasks elsewhere in the group to finish`）。
- **实际跑过**：B3 的 repro 直接反证 —— 全树 0 个任务，两个 wave 的 share 已经是 0，`total tasks anywhere in tree = 0`，句子却仍说「等别处的任务做完」。同一输出还暴露第二处措辞错误：`this wave's slice of 0 is used up`，而实际上**从未用过**（occupied=0）。
- **现有断言为何抓不到**：`over_share_declarations_are_diagnosed_against_the_root_wave:466-471` 只断言句子 `contains(root)` 和 `contains("tree_task_budget")`。
- **最小修法**：句子改成只给真正有效的两个动作（提高 root 的 `tree_task_budget`；减少**本 wave** 的在飞任务 / 拆掉多余的子 wave）；`share == 0` 时给专门的一句（「这棵树的 wave 数已超过预算，本 wave 分到 0」）。断言改成断言**动作短语**而不是断言含有某个标识符。

### M2. `tree_root_unresolved` 的整句人话删掉，538 条测试全绿

- **结论**：该诊断的 `message` 无任何失败见证；删掉渲染臂后落到 `_ => arg(args, "detail")`，args 是空的 `BTreeMap::new()`（`task_projection.rs:557`）⇒ 渲染成**空字符串**，读者在报告里看到一句空白。
- **攻击**：删除 `crates/calm-types/src/report_blocks/tasks.rs:248-253` 的整个 `"tree_root_unresolved" => {...}` 臂。
- **实际跑过**：`cargo nextest run -p calm-types -p calm-truth` → **538 tests run: 538 passed**（STILL-GREEN）。
- **对照**：`tree_budget_exhausted` 的臂是有守卫的（`over_share_...` 断言句子内容），所以这是单条缺口不是整类缺口。
- **最小修法**：`unresolvable_root_fails_closed_for_every_declaration:539-544` 里补 `assert!(!diagnostic.message.is_empty())` 且 `contains("root")`；更好的是在 `calm-types` 加一条元测试：对 `TASK_DIAGNOSTIC_CODES` 每一个码渲染一次，断言 `!message.is_empty()`（当前 `diagnostic_code_paths_cover_the_closed_vocabulary:864-868` 只断言了 `path`）。

### M3. fail-closed 被实现成「短路整个读路径」，§6.5 撤回诊断在树断链时从两个读 API 里**消失**

- **结论**：`RootUnresolved` 的早退（`task_projection.rs:588-600`）只为**传入的 declarations** 造裁决，跳过了 `task_projection.rs:848-862` 那段「块已删除但行还在飞 ⇒ 合成撤回裁决」的逻辑。于是一棵树一旦断链，所有**块已被删掉、行仍在 running** 的任务从读 API 里整个不见了 —— 这一侧是 fail-**open**（用户看不见「你的 running 任务的声明被撤了」）。
- **实际跑过**：临时用例 `ch1_repro_unresolved_root_swallows_the_read_path`：
  ```
  CH1 healthy verdicts=1
  CH1 healthy key=k1 status=Some("running") diags=["context_stale_declaration"]
  CH1 broken  verdicts=0            <- 同样的 running 行，断链后整条不见
  ```
- **诚实登记（我自己的一半假设被证伪）**：我原本还怀疑 `status` / `gate_result` 会丢。**不成立** —— `attach_task_read_state` 在早退分支里也调用了，`CH1 broken key=k1 status=Some("running")`。只有「块已删除」的合成裁决这一路丢了。
- **证据**：`crates/calm-truth/src/db/sqlite/task_projection.rs:588-600`（早退）对比 `:844-862`（合成裁决）；读 API 调用点 `crates/calm-truth/src/db/sqlite/read.rs:588`（`include_read_state = true`）。
- **最小修法**：早退分支在返回前跑同一段 `for row in state.inflight.iter().filter(|r| r.origin == "block")` 合成循环（或把 `RootUnresolved` 实现成 `effective_ceiling = 0` + 强制附加 `tree_root_unresolved` 诊断，而不是提前 `return`）。后者顺带让四种形态与主路径共用一套裁决装配。

### M4. `tree_root_unresolved` 四形态之三（成员超深）改成常量 `false` 后全绿

- **结论**：G4 列的四种 fail-closed 形态里，形态③（成员集合中出现超深节点）**零覆盖**。
- **攻击**：`crates/calm-truth/src/db/sqlite/wave_tree.rs:196-198` 的 `let over_deep = members.iter().any(...)` 换成 `let over_deep = false;`。
- **实际跑过**：`-E 'test(wave_tree) or test(tree_budget)'` → **22 tests run: 22 passed**（STILL-GREEN）。
- **可达性**：完全可达 —— root → a → b → c → d（d 在深度 4），而被查询的 wave 是 root 的另一个深度 1 的孩子：向上求根成功（深度 1，合法），向下成员枚举拿到 d（深度 4）⇒ 只有形态③能拦。现在拦不住 ⇒ 一棵局部超深的树会照常分配预算，而分母 N 里混进了本不该合法的节点。
- **最小修法**：加一条用例造上述形状，断言 `wave_tree_term(shallow_sibling) == RootUnresolved`。

---

## MINOR

- **m1. 形态④（成员集合不含发起 wave）疑似不可达 ⇒ 空分支。** 向上走到 root 说明该 wave 距 root ≤ 3，向下枚举允许到 depth 5，必然覆盖到它。`wave_tree.rs:199-205` 的 `index` 分支很可能永远不会取 `None`。零断言 + 疑似不可达 = 建议要么给出可达构造并加断言，要么在注释里降级为「防御性」并说明理由。
- **m2. `tree_cte_queries` 是生产完全不读的测试专用字段**（`task_projection.rs` 只用 `tree.term`）。它数的是 `queries += 1` 语句的条数（`wave_tree.rs:178,193`），不是真正发给 SQLite 的语句数 —— 将来新增第三条递归查询而忘记自增，`a_non_tree_wave_runs_zero_recursive_tree_queries` 不会有反应。作为「零调用」接缝它够用（M9 确实红），但它证明的是「短路那一行还在」，不是「真的没走递归」。
- **m3. 成员枚举不排除已归档 / 已终结的 wave**（`WAVE_TREE_MEMBERS_SQL` 无任何 `archived_at` / `terminal_at` 过滤）。一棵长期存在的树里，做完并归档的子 wave 会**永久**占着一份配额，直接放大 B3 的饿死速度。若这是刻意的（形状 = 全部 `waves` 行，为了 rebuild 序无关），请在 `docs/architecture/985-doc-as-plan.md` §12.1 #19 里明写这条代价 —— 目前只写了「B=32,N=10 ⇒ 3」，没写「N 只增不减」。
- **m4. `share == ceiling` 时归因回落到 `spec_task_ceiling`**（`task_projection.rs:897-899` 的 `.filter(|share| share.share < ceiling)`）。G5 说这是刻意的，但**默认配置恰好落在这个等号上**（B=32、ceiling=32、N=1 ⇒ share=32）—— 也就是说单 wave 树的默认情形永远报旧码、指向 `raise_spec_task_ceiling`，而真正的上界可能是树预算。影响面小（等号时两者同样紧），登记即可。
- **m5. 两个强制点的行谓词不同**：点一 `wave_tree_spec_inventory` 数 `declared_by='spec' AND status NOT IN (done,failed,canceled)`（不看 `origin`，`wave_tree.rs:87-90`），点二的 `occupied` 数 `declared_by=='spec' && origin=='block'` 且只算 `dispatched/running/verifying`（`task_projection.rs:619-623`）。差异落在 fail-closed 一侧（点一更严），但两者被文档并称为「同一条 Σ live_spec ≤ B」，口径应当写清或对齐。

---

## 一、实现方自述的缺口，哪些属实、哪些被低估

1. **M5c「单独变异仍绿」的解释 —— 属实，且解释成立。** 我独立复核：`0072_wave_tree_task_budget.sql:11` 确实没有 `DEFAULT`，所以从 `wave_create_tx` 的列清单里删掉 `tree_task_budget`/`NULL` 后插入值仍是 NULL，观察不到差别。判别力的真正承担者是 `every_created_wave_lands_a_null_tree_task_budget:200-206` 那条 **pragma 断言**（`dflt_value IS NULL`），它是对 migration 的独立正向断言，不与 `wave_create_tx` 共用事实来源 —— 这条是有效的。**冗余没有掩盖别的东西**：今天 `wave_create_tx` 是唯一建 wave 路径（我确认 `mod.rs:116` 只导出这一个）。**唯一被低估的点**：显式写 NULL 那一半在「将来新增第二条建 wave 路径」时才有价值，而没有任何测试会在那时变红（没有 set-equality 式的「所有建 wave 路径」元测试）。建议在注释里把它标成 forward-looking 而不是当前有判别力的防线。
2. **N1（既有 user-only 闸的 403 是恒真）—— 属实，且他们的 M13 加固是真的。** 我实测：删掉 `routes/waves.rs:1221-1223` 里的 `|| p.tree_task_budget.is_some()` ⇒ `wave_projection_policy_patch::tree_task_budget_patch_matches_the_spec_task_ceiling_surface` **FAILED**，而既有的 `non_user_policy_patches_are_forbidden_without_rows_or_events` **仍 PASS**（5 tests: 4 passed, 1 failed）。自述与实测完全一致，#1043 的定性也准确。
3. **「登记制有机器联系」（自述 §2.2 / 变异 M3）—— 被显著高估。** M3 只证明了「从清单里**删**一条会红」，没有证明「**加**一条不登记会红」，而后者才是设计要防的方向。见 B1，实测击穿。
4. **未被自述提及的缺口**：B2（排序子句无守卫）、B3（成员数不受约束）、M1（文案给出无效动作）、M2（空人话）、M3（读路径被短路吞掉）、M4（形态③零覆盖）在 `_985-s6b-mutation-map.md` 与 `_985-s6b-impl-notes.md` 里均无对应条目。

## 二、我自己设计的变异里，哪几条打穿了

| 我的变异 | 目标 | 结果 |
|---|---|---|
| 新增一条**单行**格式的未登记有界 CTE（+ 生产读者 + `pub use`） | `every_bounded_tree_cte_expansion_is_registered` | **STILL-GREEN（打穿）→ B1** |
| 删除 `ORDER BY w.created_at, w.id` | 全部 22 条 | **STILL-GREEN（打穿）→ B2** |
| 对照：`ORDER BY ... DESC` | `shares_over_a_real_tree_sum_to_the_budget` | RED（说明只有方向被钉住） |
| `over_deep` 常量化为 `false` | 全部 22 条 | **STILL-GREEN（打穿）→ M4** |
| 删除 `tree_root_unresolved` 渲染臂 | `-p calm-types -p calm-truth` 538 条 | **STILL-GREEN（打穿）→ M2** |
| 交错（非变异）：`N > B` 的树 + 全树零任务 | —— | 暴露永久饿死 + 文案说谎 → **B3 / M1** |
| 交错（非变异）：断链树里「块已删 + 行在飞」 | —— | 合成撤回裁决消失 → **M3**（同时**证伪**了我关于 `status` 丢失的假设） |
| 删除 user-only 闸里的 `tree_task_budget` | `tree_task_budget_patch_matches_the_spec_task_ceiling_surface` | RED（自述属实，未打穿） |

## 三、可以合入了吗

**NO。**

最小阻塞集（三条，均可在不触碰 v5 裁决的前提下修）：

1. **B1** —— 把登记制改成与源码格式无关（自注册宏，或空白无关的正则），并附一条「新增未登记片段 ⇒ 红」的单违例夹具。
2. **B2** —— 补一条**只测配额顺序**的用例（`created_at` 与插入序相反 + `created_at` 相等按 `id` 决胜），使删除 `ORDER BY` 必红。
3. **B3** —— 强制点一在放行子 wave 前同时校验 `members + 1 <= budget`（新成员 share ≥ 1），并加对应验收；否则本片新增的创建路径自己就能造出永久不可调度的 wave。

MAJOR 四条（M1–M4）建议同轮一并修 —— M1/M2 是纯文案 + 断言（成本极低且直接违反 §12.2 C），M3/M4 各是一处早退分支的补全与一条用例。

---

```
$ git status --short
（无输出，工作树干净；全部变异已 git checkout -- . 复原）
```
