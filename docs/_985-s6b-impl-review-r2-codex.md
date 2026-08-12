# #985 切片 6 PR-B —— 实现评审 r2（codex）

对象：`c71e4132..511dcd37`。环境：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH`、
`CARGO_BUILD_JOBS=6`、`NEIGE_CODEX_BIN` 未设置。基线定向门：calm-truth 29/29、calm-server 3/3。
v5 的配额分割裁决以 `docs/_985-s6-design.md:770-808` 为前提，本报告不重裁决。

## BLOCKER

### B1. `NULL` 孤根短路把默认树预算一起短掉；PATCH 回默认反而解除上界

- **结论**：孤根 `tree_task_budget=NULL` 时直接返回 `NotInTree`，随后投影只用本 wave ceiling；
  只要把 `spec_task_ceiling` 提到默认树预算 32 以上，`Σ live_spec ≤ B` 即失效。
  这违反 N=1 仍参与公式、只有“可证不收紧”才可短路的定型（`docs/_985-s6-design.md:791-805`）。
- **触发条件**：无父无子的 root，`spec_task_ceiling=64`，预算一直为 NULL；或先显式 PATCH 为
  32，再 PATCH 为 NULL。路由只拒绝负 ceiling，64 是合法输入（`crates/calm-server/src/routes/waves.rs:1274-1280`）。
- **证据**：shortcut 查询只取“是否成树 + budget”，完全不知道 ceiling
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:165-179`）；NULL 被映射为 `NotInTree`
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:187-200`），该项直接采用 ceiling
  （`crates/calm-truth/src/db/sqlite/task_projection.rs:570-583`）。现有 reset 验收只断言 DB 单元格
  变回 NULL，不断言默认 32 仍生效（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:243-268`）。
- **实际跑过的验证**：临时生产链探针
  `db::sqlite::wave_tree_budget_tests::r2_probe_resetting_explicit_default_to_null_keeps_default_budget_effective`
  → **RED**：显式 32 时 32/33 可调度；同一 writer reset 为 NULL 后实际 33，期望 32。
- **最小修法**：shortcut 同一条非递归 SELECT 再读有效 `spec_task_ceiling`；仅当
  `tree_task_budget IS NULL && effective_ceiling <= DEFAULT_TREE_TASK_BUDGET` 才返回 `NotInTree`，
  否则返回 N=1、B=显式值或默认 32 的 `Share`。补上述 PATCH 往返验收。

### B2. B3 的 AST 修复仍只枚举一种 AST 形状，块表达式可绕过登记

- **结论**：门禁不是“枚举每个有界 CTE 展开”，而是只枚举顶层 const 且 initializer **直接**为
  `concat!` 的项目；`const X = { concat!(bounded_wave_...!(), ...) };`、包装宏、函数内/别模块构造
  均不可见。同形的“新增不登记”洞仍在（`crates/calm-truth/src/db/sqlite/wave_tree.rs:318-344`）。
- **触发条件**：新增有界 SQL 时多一层 `Expr::Block`（rustfmt 合法），或不用直接 `concat!`；登记表
  仍只有手写四项（`crates/calm-truth/src/db/sqlite/wave_tree.rs:94-101`），守卫只遍历登记表
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:298-307`）。
- **证据**：匹配器只接受 `syn::Item::Const → syn::Expr::Macro`，且 path 必须恰为 `concat`
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:327-337`）；它不递归访问表达式，也不扫别的模块。
- **实际跑过的验证**：临时新增块初始化的 `R2_UNREGISTERED_BOUNDED_SQL` 并在 `sqlite/mod.rs`
  公开导出、未登记；`db::sqlite::wave_tree::tests::every_bounded_tree_cte_expansion_is_registered`
  → **STILL-GREEN（1/1，零 warning）**。
- **最小修法**：不要从任意 Rust 语法反推名单；用单一声明宏/typed query id 同时生成常量和登记表，
  执行入口只接受已登记项。若暂留 AST 门，至少递归 visit 全文件所有宏调用并加“块包装未登记必红”夹具。

## MAJOR

### M1. 新成员守卫遮蔽库存守卫；变异映射中 M7 的“必红”已经失真

- **结论**：两条生产谓词当前确为合取且在同一事务内（`crates/calm-server/src/operation/child_wave_adapter.rs:159-174`）；
  但新增 shape 守卫让原库存验收在 B=1,N=1 上同时命中两条，`>=` 退化为 `>` 后仍由第二条以同码拒绝。
  `_985-s6b-mutation-map.md:23` 与 `:113-114` 声称 M7 仍红，修复轮后已不成立。
- **触发条件**：库存恰等于 B。现验收只造 B=1,N=1，且仅断言共用错误码/根 id
  （`crates/calm-server/src/operation/child_wave_adapter.rs:777-817`）；shape 守卫也必拒绝 1+1≤1。
  真正能隔离库存的组合应为 B=2,N=1,inventory=2，此时创建后 N=2 仍合法。
- **证据**：库存比较在 `child_wave_adapter.rs:159-166`，成员比较紧随其后在 `:167-174`；
  新验收只购买“库存放行、shape 拒绝”的反向组合（`child_wave_adapter.rs:844-897`）。
- **实际跑过的验证**：把 `inventory >= budget` 单点改为 `>` 后，
  `operation::child_wave_adapter::tests::acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full`
  与 `...::acceptance_tree_budget_never_admits_a_zero_share_member` → **STILL-GREEN（2/2）**。
- **最小修法**：增加 B=2,N=1,inventory=2 的生产 adapter 验收，并断言拒绝文案含
  `unfinished spec task(s)`；这使库存拒绝与成员拒绝各自拥有唯一失败见证。

### M2. B2 的存在性断言可被 SQL 注释满足；行为用例仍共享 SQLite 查询计划

- **结论**：结构门只做字符串 `contains`，不证明该 ORDER BY 是最终结果集的排序
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:310-315`）；两条定向行为用例也没有固定无排序时的
  返回顺序（`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:318-360`）。
- **触发条件**：删除真实 `ORDER BY`，仅留下 SQL 注释 `/* ORDER BY w.created_at, w.id */`；生产查询的
  确定性定义实际消失（原定义位置 `crates/calm-truth/src/db/sqlite/wave_tree.rs:77-85`）。
- **实际跑过的验证**：上述单点变异后，
  `quota_member_sql_keeps_its_total_order_definition`、`quota_remainder_follows_created_at_not_insertion_order`、
  `quota_remainder_breaks_equal_created_at_ties_by_id` → **STILL-GREEN（3/3）**。扩大到 29 条时 28 绿、
  `shares_over_a_real_tree_sum_to_the_budget` 红；该用例用随机 id（`wave_tree_budget_tests.rs:278-301`），
  所以这是概率兜底，不是稳定门禁。
- **最小修法**：行为夹具把 id 固定成与 created_at 顺序相反，令删最终 ORDER BY 必然红；结构门若保留，
  用 SQL parser 验证最外层 ORDER BY 两项与方向，不接受注释/子查询里的同字串。

### M3. Rust 与 web 诊断仍是两份事实源，web 测试不验证“改哪个旋钮”

- **结论**：Rust 渲染器与 web 本地 copy 分别手写同一因果动作
  （`crates/calm-types/src/report_blocks/tasks.rs:238-266`、`web/src/pages/report-blocks/task.tsx:76-79`）；
  API 明称 `message` 由 code+args 渲染且不是第二事实源（`web/src/api/generated.ts:1235-1242`），
  但 web 实际另造了一份。两边当前文案一致，不代表漂移会红。
- **触发条件**：只把 web 正份额动作改成错误的“raise this wave's AI-task limit”，同时保留测试所找的
  “in-progress task in this wave finish”；断言只匹配一个片段（`report-blocks.test.tsx:827-831`）。
- **实际跑过的验证**：该单点变异后，`report-blocks.test.tsx > degraded blocks > gives
  tree_budget_exhausted a human explanation and next action`（share=1 case）→ **STILL-GREEN**；整文件
  **54/54 passed，Type Errors 0**。Rust 未改且其定向基线随 calm-truth 29/29 通过。
- **最小修法**：web 对该诊断直接展示服务端 `message`，或生成跨端 golden fixture；若保留本地 copy，
  测试必须同时钉住“root/top wave”与禁止“this wave ceiling”，并校验 action=`raise_tree_task_budget`。

## MINOR

### m1. “所有 B,N 相容”不变量只证明 soundness，不证明边界允许性

- **结论**：循环虽写了 B=0、N=1、N>B、非整除，但所有拒绝组合都 `continue`，因此任何更保守的
  predicate 都满足断言（`crates/calm-truth/src/db/sqlite/wave_tree.rs:373-386`）；它不是恒真——恒 true
  会红——但名称明显强于购买到的性质。
- **实际跑过的验证**：把 `members+1 <= budget` 收紧为 `<` 后，
  `enforcement_points_are_compatible_for_every_budget_and_member_count` → **STILL-GREEN**；
  `acceptance_tree_budget_refuses_child_creation_when_the_tree_is_full` → **RED**，所以组合套件有兜底。
- **最小修法**：补 `assert_eq!(can_add_tree_member(B,N), N+1<=B)` 的完整真值表，或至少显式钉
  B=0/N=1、B=2/N=1（等号放行）、N>B、B%N≠0 四类边界（公式在 `wave_tree.rs:123-127`）。

## 修复轮 1 的几处，哪几处修出了新洞

- **有新洞**：孤根 NULL shortcut（B1）、AST 枚举（B2）、成员守卫遮蔽旧库存验收（M1）、
  ORDER BY 文本门（M2）、双端诊断双写（M3）；对应修复自述见 `docs/_985-s6b-mutation-map.md:82-90`。
- **未发现新洞**：RootUnresolved 主干、over-deep、caller 缺失、重投影、升级 fixture 的新增验收均有
  独立断言（登记位置 `docs/_985-s6b-mutation-map.md:91-96`）。并发建两个 child 的两次判定不会并行穿透：
  adapter 的 prepare 运行于 `BEGIN IMMEDIATE` 到 commit 的同一 writer 事务
  （`crates/calm-server/src/operation/repo_sqlite.rs:277-325`）。

## 我自己设计的验证里，哪几条发现了问题

- 发现问题：NULL↔显式 32 往返探针（B1）、块 AST 未登记（B2）、`>=`→`>`（M1）、
  删除 ORDER BY 但留注释（M2）、只改 web 为错误旋钮（M3）；证据位置分别为
  `wave_tree.rs:165-200`、`:318-344`、`child_wave_adapter.rs:159-174`、`wave_tree.rs:310-315`、
  `report-blocks.test.tsx:827-831`。
- 未形成阻塞：成员谓词 `<` 虽穿过性质测试，却被 adapter 的等号放行路径稳定抓住
  （`child_wave_adapter.rs:819-840`）；故只列 MINOR。

## 可以合入了吗

**NO。** B1 是当前可达的上界绕过；B2 仍未关闭同形安全门洞。M1-M3 是修复点上的确定性判别力缺口，
应同轮补齐（v5 本身无需改变，依据仍是 `docs/_985-s6-design.md:789-808`）。

```text
$ git status --short
?? docs/_985-s6b-impl-review-r2-codex.md
```
