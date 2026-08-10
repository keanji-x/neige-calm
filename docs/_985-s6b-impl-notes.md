# #985 切片 6 PR-B —— 施工笔记（树级预算 + 两个强制点）

依据：`docs/_985-s6-design.md` 第二部分 §8（含 v5 ✅ 裁决框）、`docs/_985-s6b-fork.md`。
变异映射：`docs/_985-s6b-mutation-map.md`。

## 1. 交付清单（对照任务书）

| 交付项 | 落点 |
|---|---|
| `waves.tree_task_budget` | `crates/calm-truth/migrations/0072_wave_tree_task_budget.sql`（`INTEGER NULL`，**无 DB DEFAULT**，原地 additive） |
| 写入面 | `model.rs::WavePatch.tree_task_budget`（double-option）+ `wave.rs::wave_update_tx` 定向单列 UPDATE + `routes/waves.rs` 的 user-only 闸 / `0..=64` 校验 / `patch_has_other_changes` / `projection_policy_changed` + OpenAPI-TS 生成物 |
| 单一真源 | `wave_create_tx` 固定列清单**显式写 NULL**；root-only 守卫落在 `wave_update_tx`（共用 in-tx writer） |
| 强制点一 | `child_wave_adapter.rs::prepare_tx`：库存要求 `inventory < B`，形状还要求创建后 `members + 1 <= B`；任一失败 ⇒ `sub-wave-tree-budget-exhausted` |
| 强制点二 | `task_projection.rs::evaluate_schedulability`：`effective_ceiling = min(spec_task_ceiling, share)` |
| B/N 共用重投影 | `wave_report.rs::tasks_rebuild_tree_tx`：一次枚举预计算全树 share；B PATCH 与 child-wave 创建都在同一写事务调用；裁 pending 后复核 live，in-flight 超额则整笔回滚 |
| 事务上界 | `MAX_TREE_TASK_BUDGET=64`，结合 `N+1<=B` 封顶成员数；递归树查询固定 2 次，成员处理 O(N)，由 `tree_cte_queries` seam 守住 |
| fail-closed | `WaveTreeTerm::RootUnresolved` ⇒ `effective_ceiling=0` + 每条声明追加 `tree_root_unresolved`，但继续走完 withdrawal / 已删块合成 / read-state 主干 |
| 孤根 | 不设语义短路；与树成员走同一路径并得到 `N=1, share=B`。两条有界 CTE 各只有 1 行，仍为 O(1)，新增树项不会再漏出孤根分支 |
| 诊断码 | `tree_budget_exhausted` / `tree_root_unresolved`；Rust recovery-action 契约为单一来源，web 交叉校验“该拧哪个旋钮”，缺参回退与 Rust 同为 share=0 |
| 文档 | `985-doc-as-plan.md` D.4 #7 / §8 / C.2 / C.4 / §12.1 #19 #22 |

## 2. 结构性决定（都能在设计里找到依据）

**2.1 所有有界树 SQL 收进 `calm-truth/src/db/sqlite/wave_tree.rs`。**
原因是硬约束不是审美：`evaluate_schedulability` 住 `calm-truth`，`child-wave` adapter 住
`calm-server`，而 `calm-server` 依赖 `calm-truth`（反向不成立）。要让两边**共用同一个静态
门禁**（设计 §8 ⚠️ 框明令），SQL 只能住在下游 crate。PR-A 的 `MAX_WAVE_TREE_DEPTH` /
`WAVE_ROOT_DEPTH_SQL` 原样搬过去，adapter 侧 `pub use` 回原路径，公开契约不变。

**2.2 修复轮 2 删除登记制，直接守「不存在无界递归 parent-wave CTE」的性质。**
独立集成门从根 `Cargo.toml` 解码并扫描**全部 workspace member** 的生产 Rust / SQL，解码所有
字符串字面量和宏 token 内文本，剥离 SQL 行/块注释后检查：触及 `parent_wave_id` 的递归成员必须
用 alias-qualified depth 上界作 ON/WHERE 的**合取项**；`true OR bound` 两种顺序都拒绝。
它不维护 crate/常量名单，也不依赖顶层 `pub const + concat!` 这一种 AST；内联 mod、包装宏、
static、块表达式和新增 workspace crate 的绕过均逐一变异为 RED。

**2.3 递归只携带 `id` + `depth`，`created_at` 由外层 JOIN 读。**
配额顺序要 `(created_at, id)`，但设计要求向下 CTE 只投影 `id`。做法是
`WITH RECURSIVE down(id, depth)` + 外层 `JOIN waves w ON w.id = d.id ORDER BY w.created_at, w.id`。
去重也在外层（`min(depth) GROUP BY id`），环上不会重复计数。

## 3. 设计缺口 —— 我选了什么、依据是什么（**不静默决定**）

**G1. `tree_task_budget` 改动要不要触发重投影？设计没写。**
修复轮 4 最终裁决：**要，且 B PATCH 与 N 增长都在同一 IMMEDIATE 事务内调用同一个整树例程**。
理由不是用 rebuild 重新裁决历史顺序（fork C7 证伪的仍是那种方案），而是 B 本身是每个成员
确定性 share 的输入；只重投影根会让子 wave 的旧 pending 无限期保留并可直接 claim。例程一次
枚举成员并预计算 share，再把 tree term 传给各投影；没有新增持久载体或跨事务协议。旧 pending
在提交前被裁；若不可删除的 in-flight 仍超过新 share，则 B PATCH / child-wave 创建回滚，而不是
提交退化的树总量上界。每个发生变化的成员仍按自身 wave/cove scope 发 `PlanUpdated`。

**G2. root-only 守卫落 route 还是落 DB？设计只说「第 4/5 项还要加非 root ⇒ 拒绝」。**
选：**落 `wave_update_tx`（共用 in-tx writer）**，route 只保留 user-only 与取值校验。
依据：§7 #17/#20 两条都是「守卫只放 route ⇒ 直调 repo 绕过」被判红的形状；fail-closed
的那一边就是把守卫放在所有入口共用的那一层。REST 侧因此返回 409 而非 400 —— 若评审要
400，改 route 加一次前检即可，DB 守卫不动。

**G3. 强制点一用 `>=` 还是 `>`？设计写 `count >= budget`，照做。**
补一条依据：认领中的父任务本身就是一条非终结 spec 行，会被全树计数数到；用 `>` 会让
`count == budget` 时还能再开一棵子树，越过上界之后才被 schedulability 发现。变异 M7 实测红。

**G3b. 两个强制点的相容性（修复轮 1 补裁决）。**
点一除库存外复用 `can_add_tree_member(B,N) = (N+1 <= B)`；点二继续用 v5 的
`deterministic_share`。纯性质测试遍历 `B,N∈[0,64]`，双向断言
`can_add_tree_member(B,N) == 创建后所有成员 share > 0`，既证明 soundness 也证明边界允许性；
真实 adapter 交错再钉住接线：B=2 的首个 child 完成父任务后，库存已回落但第二个 child 因
成员数被拒。孤根不再另写“可证不生效”谓词，始终计算 N=1 的真实 share；PATCH 回 NULL
仍只准入默认 B=32。

**G4. 「求不到根」包含哪些形态？设计只说「求不到根」。**
选（fail-closed 一侧）：① 向上 CTE 返回 0 行或 >1 行；② 根深度 > `MAX_WAVE_TREE_DEPTH`；
③ 树成员里出现超深节点；④ 成员集合**不含**发起查询的那个 wave。四者都判
`RootUnresolved`。依据：§8 M-B2「一条断链会让整棵子树无约束」；④ 是「我们要按之划分预算的
形状，不是这个 wave 实际所在的形状」，同一条理由。
修复轮 1 给③加了「从中毒树根发起」的生产查询验收；④ 因数据库自然形状不可构造，抽出
`tree_share_from_members` 纯 seam 直接喂一个不含 caller 的成员集。两条各自的单点变异都实测红。

**G5. 两个 bound 同时收紧时报哪个诊断？设计没写。**
最终裁决按实际剩余容量归因：严格较小的一侧报一个；相等时报两个，因为单抬任一侧都无效。
冻结态同样按“解除冻结后的实际绑定项”归因：tree 恒绑定，本地 `ceiling_capacity==0` 时同时点名
ceiling（判断剩余容量而不是裸设置值，所以 `ceiling=3, occupied=3` 也覆盖）。若 64 内不存在能让
目标获得一个空位且让全员 immutable occupancy 装回 share 的 B，则两个诊断都不登记 raise action，
改为说明当前配置下不能靠抬预算解除，并建议等待在飞终结或减少树成员。

## 3.1 修复轮 2 实现收口

- B1：孤根短路从“列恰为 NULL”改成“同源有效 B 可证不更紧”，消除 PATCH 清回默认解除上界。
- B2：删除登记表与 AST 枚举，换成 crate-wide SQL 字符串性质门；SQL 注释不能伪造 depth 谓词。
- M1：库存与成员守卫各用另一守卫明确放行的 fixture，且拒绝后的零写入在同一 tx 内观察。
- M2：ORDER BY 结构门先去 SQL 注释；行为夹具固定 id 顺序与 created_at 顺序相反。
- M3：Rust recovery action 为单一契约，web 做跨文件校验；两边任改一边均有实测红变异。
- MINOR：相等份额归因、双向相容性、migration fixture 集合相等、web 缺参回退均补齐。

## 3.2 修复轮 3 实现收口

- B1：`effective_limit(Option<i64>, default)` 成为 ceiling 与 B 的共同解析函数；shortcut、投影状态、
  `wave_tree_budget` 都不再自行解释裸列。`ceiling=NULL,B=1` 只准 1 条。
- B2：根预算 PATCH 同事务枚举 `WAVE_TREE_MEMBERS_SQL` 并逐成员 rebuild；子 pending 在响应前被裁，
  随后的生产 claim seam 返回 0。
- M-1/M1/M-2：性质门覆盖 calm-truth + calm-server 的生产 Rust 和 crate 内 `.sql`；解析单个 CTE
  括号体与 UNION 递归成员，`RECURSIVE` 可选，只接受 ON/WHERE 中绑定递归 alias 的正序/反序上界。
  各 literal 不再跨文件/跨 const 拼接，仅 literal-only `concat!` 按真实语义合并。
- MINOR：删除 mutation-map 对退休登记门的引用并重写已过期的 crate-wide 结论；web 现在显示
  `root_wave_id`，多树场景能直接指出应修改的根。

## 3.3 修复轮 4 实现收口

- B1：`tasks_rebuild_tree_tx` 成为 B/N 两个触发点的唯一整树重投影例程；承重性质验收在生产
  projection/claim/admission/adapter 路径跑 B=8、B=12 两个构造。删 N 触发调用后精确 RED 为
  `(9>8, 15>12)`。
- M1：成员只枚举一次，每个投影消费预计算 `WaveTreeTerm`；分组 live 后置复核是第二次且最后
  一次递归树查询。`tree_cte_queries` 要求查询数恒为 2；退回逐成员求树后实测 6 并 RED。
  `MAX_TREE_TASK_BUDGET=64` 同时封顶合法 N 与写事务成员循环。
- B2：SQL 门只接受不处于任一可绕过 OR 支路的 alias-qualified depth 合取项；生产谓词两种
  `OR 1=1` 排列均实测 RED。crate 清单删除，改从 workspace manifest 扫所有成员；provider 探针 RED。
- web：从 Rust action 表解析实际 action 后渲染两个 capacity 诊断，断言均为 `Review capacity`；
  web 单边删 tree action 后 56 中 1 failed。

## 3.4 修复轮 5 实现收口

- B1：per-wave ceiling 保留 block-only 占用；tree share 另数 block in-flight 与全部非 block
  非终结 spec 行。升级 legacy 不裁不改；任一成员不可裁占用超过 share 时冻结整树，防止未满
  sibling 继续增长。`K >= B` 时新 block 容量为 0，终结后单调恢复。
- B2：child 创建固化 `B=8/root=5 in-flight/N:1→2` 的真实 adapter 409 序列；整树后置条件
  抽成共同 helper，member overage 走生产验收，total guard 走故意破坏 share/inventory 一致性的
  fail-closed seam。后一分支在正确 `Σshare=B` 下数学上冗余，测试的是内部不一致兜底。
- M1：SQL 门只接受直接合取叶 `alias.depth </<= ?N`（以及既有等价反向写法）；CASE、函数、
  常量 RHS、布尔 postfix 全拒。workspace members 改由 TOML parser 读取，不依赖 manifest 排版。
- MINOR：孤根 shortcut 的相等边界曾改为 `budget > ceiling`，修复轮 6 已将整条 shortcut 删除。

## 3.5 修复轮 6 实现收口

- B1：删除孤根短路与 `WaveTreeTerm::NotInTree`；孤根真实计算 `Share{members:1,share:B}`，legacy
  固定占用因此必经 tree capacity。两份评审构造分别锁住 `33→32 (B=32)` 与 `8→6 (B=6)`。
- B2：诊断归因由剩余容量严格比较决定，平局归 local ceiling；新增“读取动作→执行 PATCH→重新
  投影→准入数增加”的效果性质，错误指向非绑定旋钮时 `2→2`。
- MINOR：tree freeze 文案不再谎称要等当前 wave，而统一指向整组 excess in-flight work；
  workspace member root 缺 `Cargo.toml` 立即 panic，合法 glob 在未实现展开前也 fail-closed。
- SQL 门继续刻意只接受 `alias.depth </<= ?N`（含反向等价式）。常量界、未限定列、匿名 `?`、
  引号标识符虽可合法有界，但此门不做 SQL 名称解析/参数计数；若要引入必须先扩充正反矩阵，
  不能让一个看似有界的 token 误保护别名错误或未绑定递归变量。

## 3.6 修复轮 7 实现收口

- B1：剩余 ceiling capacity 与 tree capacity 平局时，同一拒绝同时携带
  `raise_spec_task_ceiling` 与 `raise_tree_task_budget`，两条人话都明确“必须同时提高”；tree 诊断给出
  `minimum_tree_task_budget`：从 `B+1..=MAX` 中按该成员的 `(created_at,id)` 位置求第一个使 share 增长的
  精确目标；freeze 时再与“所有成员不可裁占用均装回 share”的最小 B 取 max。后序余数成员不再因只做
  `B+1` 而原地不动，既有 overage 也不会只抬到仍冻结的位置。效果性质改成 8 行表，覆盖严格 `<`/`>`、默认孤根 `32/32`、三成员余数内外、两成员零余数
  边界、legacy 平局与 sibling legacy freeze；每行读取全部容量动作、执行后断言同一报告准入严格增加。
- MINOR-1：freeze 诊断携带 `admission_frozen=true`；Rust/web 均改说“某成员不可裁在飞超过 share，
  整树冻结”，不再谎称当前 wave 的 slice 已用尽。
- MINOR-2：删除恒为 2 的 `a_singleton_tree_runs_two_constant_size_recursive_queries`。孤根正确性由默认/
  显式预算与 legacy 占用验收承重；整树入口不得逐成员递归仍由 calm-server 的总查询计数验收承重。
- MINOR-3：删除不存在 API 的 `repair_wave_tree` recovery action。损坏树继续 fail-closed，但诊断如实说需
  operator 修复，不再向普通用户承诺一个不可执行动作。
- MINOR-5：读路径说明补回 PR-B 的 root CTE、member/inventory CTE、root budget 三条 autocommit 读取，
  不再沿用“只剩两处外部读取”的旧版本数说法。

## 3.7 修复轮 8 实现收口

- B1：冻结 minimum 的目标谓词改为
  `deterministic_share(candidate,N,index) > max(current_share, target_tree_occupied)`，再与
  `minimum_budget_to_unfreeze` 取联合可行值。self-overage 不再停在 `share==occupancy` 的零容量点。
- B2：冻结仍由 tree 诊断承载根因；若本地剩余 ceiling capacity 也为 0，同时产生 local 诊断。
  两项 action 都执行后同一报告必须严格多准入。非零 ceiling 被 block in-flight 占满有独立生产接线验收。
- B3：联合搜索在 `MAX_TREE_TASK_BUDGET=64` 内无解时，tree action/minimum 同时缺席；若 local 也绑定，
  local action 也不单独误导。Rust/web 都走明确的“当前配置无法通过抬高预算解除”分支，不再把缺值
  渲染成 `0` 或空串，并给出等待在飞终结/减少树成员两条真实出路。
- 验收：r7 的 8 行手挑表由 504 组合的 SQLite 穷举替换，边界为 `N=1..=3`、`B=0..=6`、
  `index=全部`、`ceiling=0..=5`、`target legacy∈{0,3}`；它跨每个 N 的至少两个余数边界，单次约
  2.5s。修前/后无效
  action 数为 **210 → 0**。`B=64` 普通满载、冻结满载与“无解且 local 同时绑定”另在同模块锁住。
- MINOR-3：保留 `tasks_rebuild_tree_tx` 预计算 term 的 `admission_frozen=false`。该 term 只存在于 B/N
  变更的写事务内；循环后的 `require_tree_budget_postcondition` 会在提交前拒绝任何 member overage，
  因而不会把假值暴露到已提交读路径。若在循环前求真实 freeze，必须新增第三次递归 inventory 查询或
  扩大首个成员查询的载荷，都会破坏该入口由生产后置条件守住的固定 2 次递归查询预算；此处显式记录
  依赖关系，不复制读路径的 freeze 推导。

## 4. 停下来没做的（**不就地扩范围**）

- **N1. 既有 `non_user_policy_patches_are_forbidden_without_rows_or_events` 是恒真断言。**
  M13 的过程实测发现：`X-Calm-Actor: ai:codex` 走 REST 时 `to_actor_id()` 造出空 card id 的
  `AiCodex`，下游另有一条 403，所以「删掉 user-only 闸 ⇒ 仍是 403」。我**只加固了自己的**
  那条断言（改成断言理由字符串），**没有**去改既有的 `spec_task_ceiling` / `automation_policy`
  测试 —— 那是 PR-A 之前就存在的形状，改它要连带审视 REST 的 AI actor 归因路径（另一个载体）。
  **建议单开 issue**：`ai:*` actor 经 REST 的空 card id 403 把所有 user-only 闸的验收变成恒真。
- **N2. 没有新增持久载体。** 整树例程只在现有 PATCH / child-wave 写事务内编排既有投影；
  这不是 fork C7 所否决的“用 rebuild 顺序决定共享 pending 配额”。
- **N3. 没有另加 claim 时的树级共享计数。** B/N 变化后用逐成员 live 后置复核关闭 in-flight
  超额；日常 claim 仍只消费确定性 per-wave share，不引入依赖兄弟投影产物的路径依赖。
- **N4. `tree_task_budget` 没有进 `Wave` / `WaveRow`。** 照 `spec_task_ceiling` /
  `parent_wave_id` 的先例（doc-as-plan §11：`WaveRow` 的显式 SELECT 有 8 处，sqlx 运行时才炸）。
  读它的只有内核 SQL。r3 m-4 仍仅为可观测性缺口；补公开字段会扩 OpenAPI/event replay/8 处显式
  SELECT，超出本轮预算正确性修复。
- **N5. SQL 注释剥离没有抽成生产公共 API。** `wave_tree.rs` 的 ORDER 结构断言与集成性质门都只在
  测试编译期运行，但前者只需固定常量的简单检查，后者需要带引号状态机并扫描外部文件；为消除
  测试辅助代码重复而公开新库 API 得不偿失。r3 m-3 记录保留，性质由各自行为夹具独立购买。
- **N6. 运行时任意 `format!/push_str` 生成 SQL 仍不是静态门可完备证明的对象。** 门覆盖 Rust
  literal、literal-only `concat!`、所有 workspace member 的 `.sql`（含常规 include/query_file/migration 载体）；
  若未来确需动态构造递归树 SQL，应迁到可扫描 `.sql`/完整 literal，而不是削弱门。
- **N7. SQL 门刻意误红 `HAVING` / `IN` / 常量界 / 未限定列 / 匿名 `?` / 引号标识符。** 这是
  安全方向：本树递归 SQL 的唯一允许形状是 ON/WHERE 中 alias-qualified、编号参数化的直接比较
  合取叶。需要其他等价语法时先扩语法白名单和红/绿矩阵，不能靠“语义等价”放宽。
- **N8. `tree_cte_queries` 是回退检测，不是 SQL tracing。** 它能抓循环退回全量入口，不能抓
  未来有人在循环体手写新的递归调用；任意递归 SQL 的终止性另由性质门覆盖。若要购买严格查询
  数，应在 DB 执行层加计数 seam，而不是继续累加调用方字面量。
- **N9. 旧 migration schema fixture 仍是显式最小子集。** 本片只补了生产树查询实际需要且自 0001
  就存在的 `waves.created_at`；把所有旧版本夹具统一改成从 head schema 派生，会改变 migration replay
  所要模拟的历史边界并扩大到全仓迁移治理。当前缺列会 fail-closed 为 `no such column`，不会静默放行；
  本轮不另造一套可能掩盖历史 schema 差异的结构生成器。

## 5. 门（实际数字）

| 门 | 命令 | 结果 |
|---|---|---|
| fmt | `cargo fmt --all --check` | 干净（无输出） |
| clippy | `cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings` | 最终 `Finished dev profile in 1m 14s`，0 warning |
| 测试 | `cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci` | 最终 **3406 tests run: 3406 passed, 89 skipped**（104 binaries，34.202s）。第一轮 **3405/3406** 抓到旧 schema fixture 缺基线 `waves.created_at`，补夹具后定向 **1/1**；第二轮仅 #1046 的既有 `acceptance_19` 偶发红，定向 **1/1** 后最终全量全绿 |
| 生成物 | `web: npm run gen:api` 后 `git diff --exit-code -- src/api/openapi.json src/api/generated.ts src/api/generated-terminal.ts src/api/generated-events.ts src/editor/types/` | 干净，无生成物漂移 |
| web build | `npm run build` | 成功，`built in 727ms`（仅既有 CSS highlight / chunk-size 警告） |
| web test | `npm run test` | **85 files / 1232 tests passed**，`Type Errors no errors` |
| fe lint | `npm run lint` | `no dependency violations found (102 modules, 232 dependencies)` |
| fe build | `npm run build` | 成功，`built in 213ms` |
| fe test | `npm run test`（含 `test:wire` + `test:mock-drift`） | **758 passed / 1 skipped**，wire 与 mock 均无漂移 |

环境：`CARGO_BUILD_JOBS=6`，nextest 取自 `.local-bin`，`NEIGE_CODEX_BIN` 全程未设置，
web/fe 最终全门显式使用 Node `v22.22.0`。

修复轮 6 的复原后定向回归：`calm-truth --lib` **357/357**（其中 wave-tree **26/26**），
全 workspace SQL 性质门 **18/18**，`calm-server` child adapter **12/12**，web report-block
**56/56**。

修复轮 6 最终 Rust/web/fe 全门全绿；web/fe 正式全门均显式使用 Node 22.22.0，且
实现 worktree 内 `node_modules` 存在，vitest 确实执行（不是缺依赖的结构性复核）。

### 5.1 修复轮 7 复原态全门

环境：`CARGO_BUILD_JOBS=6`，nextest 取自 `.local-bin`，`NEIGE_CODEX_BIN` 未设置；web/fe
均在本实现 worktree、显式 Node `v22.22.0` 下执行。

| 门 | 实际结果 |
|---|---|
| `cargo fmt --all --check` + `git diff --check` | 干净 |
| workspace clippy（命令同上） | `Finished dev profile in 1m 22s`，0 warning |
| workspace nextest ci | 第一轮 **3403/3404**，唯一失败为题定既有 #1046 `acceptance_19`（`Failed != Running`）；定向复跑 **1/1 PASS**，第二轮 **3404/3404 PASS，89 skipped**（104 binaries，59.532s） |
| migration replay gate | **2/2 PASS**（37.472s） |
| web 生成物 | `npm run gen:api` 后目标生成文件 `git diff --exit-code` 干净；bindings 49/49 + 15/15 + emit-openapi 1/1 |
| web build | 成功，`built in 785ms`（仅既有 CSS highlight / chunk-size 警告） |
| web test | **85 files / 1235 tests PASS**，Type Errors 0 |
| fe lint | 通过；dependency cruise **102 modules / 232 dependencies**，0 violation |
| fe build | 成功，`built in 211ms` |
| fe test | **758 PASS / 1 skipped**（61 files pass / 1 skipped），wire 与 mock drift 均通过 |

### 5.2 修复轮 8 复原态全门

环境：`CARGO_BUILD_JOBS=6`，nextest 取自 `.local-bin`，`NEIGE_CODEX_BIN` 未设置；web/fe 均在
本实现 worktree、显式 Node `v22.22.0` 下执行。

| 门 | 实际结果 |
|---|---|
| `cargo fmt --all --check` + `git diff --check` | 干净 |
| workspace clippy（命令同上） | 最终 `Finished dev profile in 1m 26s`，0 warning；首轮正确抓到 3 处 `Option` 判空后 `expect`，结构化解包后复跑全绿 |
| workspace nextest ci | **3406/3406 PASS，89 skipped**（104 binaries，72.707s）；本轮未触发 #1046 偶发红 |
| migration replay gate | **2/2 PASS**（42.700s） |
| web 生成物 | `npm run gen:api` 后目标生成文件 diff 干净；bindings **49/49 + 15/15**，emit-openapi **1/1** |
| web build | 成功，`built in 788ms`（仅既有 CSS highlight / chunk-size 警告） |
| web test | 首轮 **1234/1235**，唯一为未改 `DirectoryPicker` 键盘用例；定向 **20/20 PASS**，最终全量 **85 files / 1235 tests PASS**，Type Errors 0 |
| fe lint | 通过；dependency cruise **102 modules / 232 dependencies**，0 violation |
| fe build | 成功，`built in 207ms` |
| fe test | **758 PASS / 1 skipped**（61 files pass / 1 skipped），wire 与 mock drift 均通过 |

fe 首轮 test 的 oracle 抓到本轮 web 文案增行造成 `CAP-REPORT-TASK-023` source range 漂移；把既有
引用从 `task.tsx:99-127` 校准到 `task.tsx:112-170` 后复跑全绿，未增加 source-anchor baseline 债务。

## 6. 已知代价（已登记进 doc-as-plan §12.1 #19）

`share = floor(B/N)` ⇒ `B=32`、`N=10` 时每个 wave 只剩 3 条未结额度：一棵大树里单个 wave
会比它独立存在时更早撞上限。**校准装置 = `tree_budget_exhausted` 的发生率**；频繁触发说明
`tree_task_budget = 32` 定低了 —— **调常数，不要改分配律**（改成共享计数会直接击穿 D.1 #11，
见变异 M1）。
