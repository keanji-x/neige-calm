# 切片 6 设计增量 v4：`spawn: "sub-wave"` + 深度上限（PR-A）/ 树级预算（PR-B）

> 本文是 `docs/architecture/985-doc-as-plan.md` 的**增量**。定稿后并入权威文档，随 PR 提交。
> 证据链：`_985-s6-survey.md`（施工清查）、`_985-s6-forks.md`（岔口 1–4）、`_985-s6-fork5.md`（岔口 5）、
> `_985-s6-design-review-codex.md` + `_985-s6-design-review-subagent.md`（v1 双通道评审，8 BLOCKER / 11 MAJOR）。
> 所有「今天是这样」的断言取自 `origin/main` @ `9d30006a`。

## v1 → v2 的变更总览

v1 双通道评审判**拒绝按现设计施工**，且两通道的清单**几乎不相交**（交集只有「是否拆片」）。
v2 接受全部 8 条 BLOCKER 与 11 条 MAJOR，并**推翻 v1 §0 的「找不到自洽切分线」**。

## v2 → v3 的变更总览

r2 双通道再次判 **NO**（codex 3 条最小阻塞集 / subagent 7 条）。v3 的关键变化：

**① 两通道在自 FK 语义上直接冲突，已用 sqlite3 实证裁决**（三条构造，见 §2.1）：

| 构造 | 实测 |
|---|---|
| `NO ACTION` 自 FK + 同 cove 的 wave 树 + 单条 `DELETE FROM coves` | **成功**，0 行残留 |
| 同上但自 FK 改 `RESTRICT` | **失败** |
| `NO ACTION` + **跨 cove** 子 wave 指向被删 cove 内的父 | **失败** |

⇒ **r1 的 BLOCKER-B1 不成立**（它把 `NO ACTION` 当成了 `RESTRICT`；`NO ACTION` 在**语句结束时**检查，
而删 cove 是单条 `DELETE FROM coves`）。v2 改 `CASCADE` 是在修一个不存在的问题，
代价是开出 r2-B 的 BLOCKER-4。**v3 回到 `NO ACTION`，守卫下沉进 `wave_delete_tx`。**
**真正的危险是跨 cove 父子链** —— 两个通道都没提到，v3 新增绊线（§2.1）。

**② `UNION` 的去环主张被实证推翻**（两通道一致 + sqlite3 实测）：带 `depth` 列的 CTE 按整行去重，
`(A,B,0)` 与 `(A,B,2)` 是不同行，`UNION` 与 `UNION ALL` 行为完全相同。
**唯一终止装置是 depth 截断。** §7 的变异随之改写。

**③ `kind`/`spawn` 现在定型，不推给实现方**（§1.6）。r2-A 找到了裁决依据：
spec harness 全链硬编码 `AgentProvider::Codex`。这同时从源头消灭 r2-B 的 BLOCKER-2。

**④ 三条守卫全部下沉到 DB 收口**（拒删 / 禁止 reopen / descendant）——
「守卫放路由层」是 v2 重复犯的同一个错，被 `Repo::wave_delete` 与
`RepoSyncDomainRaw::wave_update` 两条 raw 路径分别打穿。

**⑤ `Done` ≠ 子 wave 静止**（§5.2）—— 新增 quiescence 前置条件。

**⑥ §7 从 23 条重写**：r2 判定其中 11 条不具判别力。v3 逐条替换变异，
并删掉两条不可执行的（#3c 编译失败、#21 间歇性红）。

## v3 → v4 的变更总览

r3 两通道**独立收敛到同一条 BLOCKER**，A 另加一条。两边均判「修完即 YES」。

| # | 来源 | 内容 | v4 处置 |
|---|---|---|---|
| B1 | **两通道一致** | **quiescence 在「子 wave `Done` + 残留 `pending`」下是确定性活锁** | §5.2 改：quiescence **只数 in-flight 三态**；残留 pending ⇒ 父任务 `failed('child-wave-incomplete')` |
| B2 | r3-A | **`child-wave` / bootstrap operation 自身 `Failed`/`Stuck` 没有父任务映射** | §3.3 新增 outcome 表 |
| M1 | r3-A | #5 的「报告与冻结行分歧」seam 仍未定义，可降级成 builder 单测 | §7 #5 写实 seam |
| M1' | r3-B | #11 要求「三站点各单点变异」**不可满足**（`:1290`/`:1319` 嵌在 `:1271` 臂内，臂序修好后是死代码）| §4 改为「一处行为变异 + 一处结构断言」 |
| M2' | r3-B vs r3-A | B 判 #10 恒绿；**A 查代码否掉了 B** —— 四个 adapter 的 stale 检查全在第一个 DB 副作用之前 | 保留 #10，补「每 kind 各配合法 fixture + 断言**具体的** stale 错误变体」 |
| MINOR | 两通道 | 标题写 v2；§7 实际 30 条而正文写 28；§10 清单仍写 `ON DELETE CASCADE` | 全部改正 |

**r3 坐实的两条**：`cove_delete_tx` 确实没有逐 wave 删除（两通道各自读码 + 各自跑 sqlite3 复现三条构造）
⇒ §2.1 的 `NO ACTION` 裁决成立；跨 cove 父子边今天确实只有 `child-wave` adapter 一条写路径
（`WavePatch` 无 `cove_id`，全仓无 `UPDATE waves SET cove_id`）。

**r3 排除的一条**：第二层 sub-wave **不会**与第一层 idem 碰撞 ——
task id 是 `wave_id:key`，且 operation 唯一键还带 kind。

---

## 0. 拆片：沿约束切，不沿载体切

**v1 错在只考察了「沿载体切」。** 沿约束切存在一条自洽线：

| 片 | 内容 | 行数 |
|---|---|---|
| **PR-A** | `tasks.spawn` 冻结列 + `child-wave` operation + `waves.parent_wave_id`（FK/索引/CHECK）+ **深度上限 `MAX_WAVE_TREE_DEPTH = 3`** + `tasks.child_wave_id` + 父任务闭合（live + sweep）+ 读时回显 | ~900 |
| **PR-B** | `waves.tree_task_budget`（含写入面）+ 强制点一（创建准入）+ 强制点二（`evaluate_schedulability` 树项）+ 短路 + 诊断码 | ~450 |

**PR-A 为什么自洽**：创建**有**闭合（不违反 D.3）；树**有**上界 —— 深度 3 是一个真实、
可判定、单点强制的界，不是死代码。

**为什么这条线比不拆好**（评审的证据）：v1 的 7 条 MAJOR 里 4 条全长在树预算上，
5 条 BLOCKER 里 3 条长在创建-闭合上。两簇互不相干的风险混在一片里，
任何一簇返工都要重跑另一簇的全部门禁。

**不采纳 codex 通道的「6a inert plumbing / 6b activation」线**：6a 是纯死代码
（`spawn` 列无消费者），且**承重断言 3（claim 后改 `spawn` 不改变路由）在两条路由不可区分时根本测不了**。

### 0.1 PR-A 必须显式登记的代价

> **PR-A 合入后到 PR-B 合入前，D.4 #7 的树上界「尚未成立」。**
> 真实上界是 `Σ per-wave spec_task_ceiling`（每个 wave 的 ceiling 独立生效）。
> 这句话必须同时写进 PR-A 的 §12.1 与附录 D.4 #7 的旁注。
> **不写就等于本设计一贯批判的「名义不变量为假」。**

---

# 第一部分：PR-A

## 1. `spawn` 通电（前置裁决的四条交付项）

`spawn` **保持在 §5.2 哈希排除集**，理由从「无执行消费者」换成 **「claim-frozen 路由选择器」**。

### 1.1 今天的实证起点

- 写入校验已接受 `"in-wave" | "sub-wave"`（`crates/calm-types/src/report_blocks/kinds.rs:252-256`），
  `spawn` 已在 `TASK_FIELDS`（`kinds.rs:119-137`）；
- `TaskDeclaration` **没有该字段**（`crates/calm-types/src/report_blocks/tasks.rs:97-119`）
  ⇒ **今天一个合法的 `spawn` 值被静默丢弃**；
- 根哈希排除集已含 `spawn`（`crates/calm-server/src/task_context.rs:47-55`），有测试守住（`:840-865`）；
- **manifest plan template 有一条 set-equality 元测试断言 `spawn ∈ template_exclusions`**
  （`crates/calm-server/src/mcp_server/tools/plan.rs:918-933`）⇒ workflow manifest **不得**声明 sub-wave。
  这是一条真实策略约束，登记进 §9 文档修订清单。（评审 MINOR-2）

### 1.2 前置条件 1 —— claim 前规范化 + 投影

**载体**：`tasks.spawn TEXT NOT NULL DEFAULT 'in-wave'`（migration `0071_`）。

**规范化发生在投影入库那一步，不在哈希层** —— canonical 投影折叠「缺席 / null」但
**不折叠默认值**（`task_context.rs:726` + `calm-types/src/report_blocks/fence.rs:79`）。

> ### ⚠️ BLOCKER-B3 修正：UPSERT 有三段守卫，不是一段
>
> v1 与 `_985-s6-survey.md:54` 都把投影 UPSERT 的守卫简化成「`WHERE tasks.status='pending'`」。
> **真实形状是三段合取**（`crates/calm-truth/src/db/sqlite/task_projection.rs:976-983`）：
>
> ```
> ON CONFLICT ... DO UPDATE SET ...
>  WHERE tasks.status = 'pending'
>    AND (tasks.origin = 'block' OR ...)
>    AND (tasks.kind IS NOT excluded.kind OR tasks.goal IS NOT excluded.goal OR ...)   ← 逐列变更检测析取
> ```
>
> **加 `spawn` 必须同时改三处**：INSERT 列清单、`DO UPDATE SET`、**以及变更检测析取**。
> 漏掉析取项的后果：spec 把一个 pending 块从「无 `spawn`」改成 `spawn:"sub-wave"`
> 而其余字段一字不改 ⇒ 析取为假 ⇒ UPDATE 命中 0 行 ⇒ 列不写、`changed_keys` 不含该 key
> ⇒ **连 `PlanUpdated` 都不发**。claim 冻结旧值 `'in-wave'`，原地执行。
> **声明改了、执行没改、零信号。**

四格：

| 格 | 内容 |
|---|---|
| 载体 | `tasks.spawn`，进 `Task` 与 `TASK_COLUMNS`（照抄 `declared_by`，**不**照抄 `decl_ready`）|
| 谁写 | `project_tasks_tx` 的 INSERT/UPSERT（三段守卫全改）|
| 谁读 | `drive_spawn` 的路由分支；`resume_dispatched` 经 `tasks_nonterminal` 读到的同一行 |
| rebuild | 当前文档的纯函数 |
| backfill | 存量全 `'in-wave'`，**显式写出**（照抄 `0068_` 形状）|

**为什么必须进 `TASK_COLUMNS`/`Task`**：恢复路径的输入就是 `tasks_nonterminal` 解码出的完整 `Task`
（`read.rs:423-429` → `scheduler/mod.rs:1624-1631`）。做窄 reader 等于给
「恢复只信冻结行」这条前置条件加一个新的失败面。

**连带面**：完整 `Task` 解码点恰好 5 处 —— `task.rs:33`、`task.rs:131`、
`read.rs:402`、`read.rs:413`、`read.rs:423`。**sqlx 在运行时才失败，不是编译期。**

### 1.3 前置条件 2 —— 栅栏（今天已成立，只加验收）

`scheduler/mod.rs:741-769`，按 `doc_rev` 判，与字段是否进哈希无关。

### 1.4 前置条件 3 —— claim 成功后同事务重读冻结行

> ### ⚠️ MAJOR-A5 修正：这条 v1 **只在文档里成立**
>
> 今天确实重读（`scheduler/mod.rs:803-812`、`:920-925`），但 v1 的 §7 里
> **没有任何一条变异能让它改坏后变红**：保留 `task_get_tx` 供日志、把 `drive_spawn`
> 的入参改回 claim 前的 `task`，四条路由测试全绿 —— 因为生产的报告编辑会升 `doc_rev`、
> 先被栅栏挡成 race-lost，制造不出「pre-read 与 tx re-read 不同但栅栏仍通过」的差异。
>
> **修法（结构性，不靠测试）**：收窄 `claim_task` 的 API，
> **让 claim 前的 `Task` 快照在 claim 成功后不再可达** —— 能让编译器强制的，别写测试去扫。
> 另配一条 seam 测试：在同一 IMMEDIATE 事务内改 pending 行的 `spawn`（不经文档、不动 `doc_rev`），
> 断言实际 op payload 取的是 tx 内重读值。

### 1.5 前置条件 4 —— 三条路由只按冻结行

| 路径 | 本片 |
|---|---|
| 即时派发 | `drive_spawn` 内按 `task.spawn` 分流；**不得**新增任何报告读 |
| 崩溃恢复 | `resume_dispatched` 走同一个 `drive_spawn`；今天确实不重读报告（`:1596-1631`）|
| 子 wave 幂等创建 | `child-wave` op 的 payload 由**冻结行**的纯函数生成 |

> ### ⚠️ MAJOR-A6 修正：`§7 #5` 证明不了「payload 来自冻结行」
>
> v1 的唯一观察是「重复 submit ⇒ 同一 child」。变异「让 payload 重读当前报告」在
> **未编辑报告**的 fixture 下两次 hash 相同、同一 child ⇒ **绿**。
> **修法**：在第一次 op insert 后、prepare/recovery 前**编辑纳入哈希的字段**，
> 再走真实 `resume_dispatched`，断言 op payload hash / child seed / child id 全部保持第一次冻结值；
> **禁止用 payload builder 计算 expected**（那是与被测代码同源）。

### 1.6 `kind` 与 `spawn` 的关系（**v3 定型**）

> ### ⚠️ v2 把这条推给实现方是错的，且已有具体后果
>
> v2 写「实现前从代码裁决 + fail-closed 默认」。r2 两个通道都判这是**把设计决定推给实现方**：
> - r2-A：#23 的 expected 取决于被测实现自己先选哪条语义 ⇒
>   **断言与被测代码共用同一事实来源**，静默忽略可以被包装成「裁决为可选」而全绿；
> - r2-B：正是这条延后让 BLOCKER-2 可达 —— `kind: terminal, spawn: sub-wave` 的父任务
>   会掉进 sweep 的 `TaskStatus::Running if kind == Terminal` 臂（`scheduler/mod.rs:1268`）。

**裁决依据（当前代码已经给出答案）**：spec harness 全链硬编码 `AgentProvider::Codex` ——
session row、thread attribution、MCP identity 三处
（`crates/calm-server/src/operation/spec_harness_start_adapter.rs:381-383, 589-594, 627-633`）。
**子 wave 的 spec agent 今天不可选。**

**定型**：

> PR-A 下，`spawn: "sub-wave"` **只允许与默认 `kind`（codex）组合**。
> `claude` / `terminal` 与 `sub-wave` 的组合在 **task 块公共写口校验处拒绝**
> （`crates/calm-types/src/report_blocks/kinds.rs` 的 task 校验，与既有
> `spawn` 值域校验同处），**禁止静默忽略**。
> 未来引入可选 spec provider 时另开设计，那时这条限制随之放宽。

**这条同时消灭 r2-B 的 BLOCKER-2**：`terminal` 永远不会与 `sub-wave` 共存，
sweep 的 terminal 臂够不到 sub-wave 父任务。**但 §4 的臂序仍然要改** ——
不能把安全性建立在「校验挡住了」这一条上（存量行、未来放宽都会绕过它）。

### 1.7 §5.2 与附录 D.1 3d 的旁注改写

§5.2 现文「**`spawn` 的排除是版本化的，不是永久的**：它今天无执行消费者，
切片 6 让它获得执行语义之前必须重新裁决」**改为**：

> **`spawn` 的排除理由是「claim-frozen 路由选择器」，不是「无执行消费者」**（后者自切片 6 起失效）。
> 它有执行语义，但那个语义**只读冻结行**：claim 前规范化并投影进 `tasks.spawn`
> （三段守卫全改）、解析→claim 之间的编辑被 `docRev` 栅栏挡成 race-lost、
> claim 成功后同事务重读冻结行（**且 claim 前快照在 API 上不可达**）、
> 即时派发 / 崩溃恢复 / 子 wave 幂等创建一律只按该行路由。
> **这四条任何一条不成立，排除结论立即失效。**
>
> **「版本化排除」在代码里不存在** —— `TaskContextRef` 无 hash schema 或版本位
> （`calm-types/src/event.rs:36`），裸 `Vec<TaskContextRef>` 进 `claim_context_json`
> 与 `TaskContextFrozen`，两处都无集合版本（`task.rs:230`、`event.rs:839`）。
> 它只是设计词汇。**本裁决选保持排除，正是因为它不需要引入这套东西。**

附录 D.1 3d 的 `spawn ⇒ 不判` 那一格**保持不变**，旁注改为：
「`spawn` 自切片 6 起有执行语义；不判的理由是它已在 claim 时冻结进 `tasks.spawn`，
claim 后的文档值不再被任何路由读取 —— 见 §5.2 的四条前置条件。」

---

## 2. 树结构与深度（PR-A 的上界）

### 2.1 载体

| 列 | 类型 | 谁写 | rebuild | backfill |
|---|---|---|---|---|
| `waves.parent_wave_id` | `TEXT NULL REFERENCES waves(id)`（**`NO ACTION`，即默认**）+ `CHECK(parent_wave_id IS NULL OR parent_wave_id <> id)` | 只有内核（子 wave 创建）| 结构真源，非文档函数 | 存量全 `NULL` |
| 索引 | `CREATE INDEX ... ON waves(parent_wave_id) WHERE parent_wave_id IS NOT NULL` | —— | —— | —— |

> ### ⚠️ v3 裁决：回到 `NO ACTION`。r1 的 BLOCKER-B1 **不成立**，v2 的 `CASCADE` 修的是不存在的问题
>
> **r1-B 的论证**：「级联是逐行的，先删到父 wave 而子 wave 行还在 ⇒ NO ACTION 立即报错
> ⇒ 整个 cove 删除回滚 ⇒ 顺序相关的间歇性失败」。
> **r2-A 反驳**：那是 `RESTRICT` 语义，不是 `NO ACTION`。仓库需要「立即强迫 teardown」时
> 明确选的就是 `RESTRICT`（`crates/calm-truth/migrations/0011_terminals_card_id_restrict.sql:11-16`）。
>
> **实证裁决**（sqlite3，三条构造）：
>
> | 构造 | 实测 |
> |---|---|
> | `NO ACTION` + 同 cove 的 `w_root ← w_child` + 单条 `DELETE FROM coves` | **成功**，`waves` 剩 0 行 |
> | 同上，自 FK 改 `ON DELETE RESTRICT` | **`FOREIGN KEY constraint failed`** |
> | `NO ACTION` + **跨 cove**：`w_cross`(c2) → `w_root`(c1)，删 c1 | **`FOREIGN KEY constraint failed`** |
>
> **结论**：`NO ACTION` 在**语句结束时**检查；`cove_delete_tx` 的删除是**单条**
> `DELETE FROM coves`（`crates/calm-truth/src/db/sqlite/cove.rs:182-188`），
> 同 cove 的 waves 在该语句内经 `cove_id CASCADE`（`migrations/0001_init.sql:22`）全部消失，
> 语句结束时已无悬垂引用。**r1 的 BLOCKER-B1 是把 `NO ACTION` 读成了 `RESTRICT`。**
>
> **v2 的 `CASCADE` 必须撤回**：它开出 r2-B 的 BLOCKER-4 ——
> `Repo::wave_delete`（`db/mod.rs:554,598` → `wave.rs:242`）绕过路由层守卫，
> 自 FK 静默删掉整棵子树：无逐 child `WaveDeleted`、无进程 teardown、
> 其它 wave 的 `child_wave_id` 悬空。**响亮的红换成了安静的数据丢失。**

> ### ⚠️ v3 新增（两个通道都没提到）：真正的危险是**跨 cove 父子链**
>
> 上表第三行证明：只要存在一条 `child.cove_id ≠ parent.cove_id` 的边，
> 删 parent 所在的 cove 就会 `FOREIGN KEY constraint failed`，**整个 cove 删除回滚** ——
> 这才是 r1-B 想描述、但归错因的那个故障。
>
> 它今天不会发生，唯一原因是 §3.5 让子 wave **继承** `cove_id`，而内核是
> `parent_wave_id` 的唯一写者。**这是一条「安全性依赖当前事实」** ⇒ 按项目纪律必须立绊线：
>
> 1. **不变量测试**：全表扫 `waves`，断言每一条 `parent_wave_id IS NOT NULL` 的行满足
>    `child.cove_id = parent.cove_id`；
> 2. **反向构造测试**：手工造一条跨 cove 边，断言删 cove **失败** ——
>    把「为什么必须同 cove」钉成可执行事实，而不是注释；
> 3. `child-wave` adapter 里 `cove_id` 的复制点写明「跨 cove 会打断 cove 删除，见测试 X」。

**索引是必需的（评审 MAJOR-B5）**：SQLite 的 FK **不为子列自动建索引**。
无索引则每次求根/求子都全扫 `waves`。

### 2.2 深度

**定义**：根深度 = 0；子 = 父 + 1。`MAX_WAVE_TREE_DEPTH = 3` ⇒ 允许 0..=3，第 4 层被拒。
判据：`parent_depth >= MAX_WAVE_TREE_DEPTH ⇒ 拒绝`。

> ### ⚠️ 环的真实防护：**只有 depth 截断**。`UNION` 不去环 —— v2 的归因是错的
>
> **v1 的洞是真的**：`UNION ALL` + `A→B→A` ⇒ 无限递归，且这条 CTE 跑在握着全库写者槽的
> BEGIN IMMEDIATE 事务里（`db/sqlite/infra.rs:10-24`）⇒ 一条中毒数据挂死整库写入。
> `CHECK(parent<>id)` 只挡自环，FK 只要求两行都存在，挡不住 `A→B→A`。
>
> **但 v2 的修法归因错了**，r2 两个通道独立指出、并经 sqlite3 实测：
> 带 `depth` 列的 CTE **按整行去重**，`(A,B,0)` 与 `(A,B,2)` 是不同的行 ⇒
> **`UNION` 与 `UNION ALL` 行为完全相同**（实测：都返回 6 行、都毫秒级；
> 删掉 `WHERE up.depth <= ?2` 后 `UNION` 十秒不返回）。
> v2 照抄的先例 `wave_vcs/gc.rs:271-280` 能靠 `UNION` 终止，
> **是因为它的 CTE 只投影 `hash` 一列**。
>
> ⇒ **唯一的终止保证是 depth 截断。** v2 的 §7 #7（「`UNION` 改回 `UNION ALL` 必红」）
> 在它自己规定的变异下**必绿**。

**求根 + 求深度（一次向上 CTE）**：

```sql
WITH RECURSIVE up(id, parent_wave_id, depth) AS (
  SELECT id, parent_wave_id, 0 FROM waves WHERE id = ?1
  UNION ALL
  SELECT w.id, w.parent_wave_id, up.depth + 1
    FROM waves w JOIN up ON w.id = up.parent_wave_id
   WHERE up.depth <= ?2          -- MAX_WAVE_TREE_DEPTH + 1。★唯一的终止保证★
)
SELECT id AS root_id, depth AS parent_depth FROM up WHERE parent_wave_id IS NULL;
```

**算子写 `UNION ALL` 而不是 `UNION`** —— 因为 `UNION` 在这里买不到任何东西，
写 `UNION` 会让后来者以为环由它挡住。截断条件旁必须有 `★` 注释说明它是唯一保证。

**三态判定，全部 fail-closed**：
- 命中一行 ⇒ 正常；
- **零行**（断链 / 环 / 触到截断）⇒ **拒绝**；
- 多行 ⇒ 结构上不可能，若出现 ⇒ 拒绝。

**环与截断必须给不同的诊断**（否则「拒绝」掩盖了「数据已中毒」这条运维信号）：
外层再跑一次带 `visited` 的判定，或对被拒的根路径做一次有界的环检测，
产出 `sub-wave-tree-cycle` 与 `sub-wave-depth-exceeded` 两个不同理由。

**测试**（v2 的变异作废，改这两条）：
- 手工 INSERT 2-环 ⇒ 创建被拒、**耗时 < 1s**、理由是 `sub-wave-tree-cycle`；
  **变异 = 删掉 `WHERE up.depth <= ?2`**（不是换算子）⇒ 必须挂死/超时 ⇒ 红。
- **静态门禁**：源码断言这条 CTE 文本含截断子句 —— 因为「挂死」这种红在 CI 上表现为超时，
  判别力弱且慢。

---

## 3. 子 wave 创建：`child-wave` operation

### 3.1 为什么是 operation（v1 论证保留）

`(kind, idempotency_key)` partial unique（`0042_operations_parked.sql:96-98`）允许同一
`task.id` 同时挂 `codex-worker` 与 `child-wave` 两条 op；`submit` 先查重、hash 相同则 drive 原 op
（`operation/driver.rs:110-128`）；`prepare_tx` 是 BEGIN IMMEDIATE（`operation/repo_sqlite.rs:277-325`）。
claim→submit 的窗口由 `resume_dispatched` 补齐。内联方案要重造两套幂等与恢复载体。

### 3.2 `prepare_tx` 内的原子序列（修正版）

> ### ⚠️ BLOCKER-A1 修正：这是**第五个判决强制点**，必须显式登记
>
> v1 漏了 `refuse_if_context_stale`。攻击：claim 一条 `sub-wave` 任务 → 编辑纳入哈希的
> `goal`/`context` → `context_stale_at_ms` 已持久 → **子 wave 照建**，在一份已判失效的规格上起活。
> 而 registry 集合测试只证明「被分类」，错分进 `NON_TASK_BOUND_ADAPTER_KINDS` 仍绿
> （`crates/calm-server/src/operation/mod.rs:54-75`、`tests/scheduler.rs:2374-2399`）。
>
> **更重的一层**：§5.2 原文写着「判决强制点只有 §5.6 的四个 `prepare_tx`，**没有第五条**」。
> **本片新增 adapter 就是在改这条不变量。** 必须在 §5.6 与 §5.2 两处同时改成「五个」，
> 并说明第五个是 `child-wave`。不改 = 文档在指导后来者相信有一条不存在的封闭性。

同一个 IMMEDIATE 事务：

0. **`refuse_if_context_stale(payload.task_id)`** —— 在**第一条 DB 副作用之前**。
   `child-wave` 进 `TASK_BOUND_ADAPTER_KINDS`。
1. 求根 + 深度（§2.2 的向上 CTE）；三态 fail-closed。
2. 深度检查：`parent_depth >= MAX_WAVE_TREE_DEPTH` ⇒ 拒绝。
3. *(PR-B：树预算检查)*
4. INSERT 子 wave（`parent_wave_id = 父`，lifecycle `Draft`）。
5. 建 spec / report 两张 cards + layout overlay（照抄 `routes/waves.rs:679-680, 720-756, 794-807`）。
6. **stamp `tasks.child_wave_id`**（guarded COALESCE，见 §5.1）。
7. 事务内 `append_decision_events_in_tx`，`BroadcastEnvelope` 进 `TxOutput.post_commit_events`
   （`operation/codex_adapter/mod.rs:370-410`）。

### 3.3 提交后的 harness 启动**必须有恢复臂**

> ### ⚠️ BLOCKER-B5 修正
>
> v1 写「提交后启动子 wave 的 spec harness」，无恢复语句。攻击：`prepare_tx` 提交成功
> （骨架 + `child_wave_id` 已落库），进程在 submit `spec-harness-start` 之前崩溃。重启后
> `resume_dispatched → drive_spawn → submit`（`(kind,idem)` 命中已 **Succeeded** 的 op）
> → `wait` 立即返回 → `mark_running`（0 行，已 running）⇒ **循环闭合，harness 永远不启动**。
> 子 wave 停在 `Draft`、无 session。唯一兜底 reaper dead-root 扫描要求
> 「最近一条 `spec-harness-start` op 处于 `phase='failed'`；**没有 start-op 行则放过**」
> （`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:176-195`）—— 本场景恰恰一条都没有。
> **子 wave 永久 inert、父任务永久 running、树预算永久被占。**
>
> `_985-s6-forks.md:29` 在**内联方案**下点过这个风险，v1 改选 operation 方案后把它一起丢了 ——
> **窗口没有消失，只是换了位置。**

**修法**：spec-harness-start 的 submit 做成 `drive_spawn` 里**每次都跑的幂等步骤**
（照抄 `routes/today.rs:253-268` 的「事务外幂等 harness operation」形状），
而不是 child 创建的一次性尾巴。

> ### ⚠️ r2 两通道各加一条，v2 的修法**都没堵住**
>
> **① 顺序未约束（r2-B BLOCKER-3）**：`drive_spawn` 只在行是 `dispatched` 时被
> `resume_dispatched` 重进。若 harness submit 排在 `dispatched → running` flip **之后**，
> 窗口只是从「op 成功后」挪到了「running flip 之后」，一行没少。
> ⇒ **写死：harness submit 严格早于 running flip。** flip 是这条链的提交点。
>
> **② 没有 exactly-once oracle（r2-A MAJOR）**：v2 的验收只要求「拿到 harness / 离开 `Draft`」。
> 实现若误抄普通 wave create 的 `idempotency_key: None`（`routes/waves.rs:868-883`），
> 会建**第二条** start-op / 第二个 thread；adapter 的 replace 逻辑最终仍留下一个活 harness，
> child 仍离开 `Draft` ⇒ **全绿**。「至少一个活着」证明不了「没有重复起」。
> ⇒ **写死稳定 idem key**：`child-wave:<child_id>:bootstrap`，payload 逐字节稳定
> （today 先例用的就是稳定 key：`routes/today.rs:288-303`）。

**验收（改写）**：崩溃注入点**枚举两个**（op 成功后 / running flip 前后各一），
每个都断言：子 wave 最终离开 `Draft` **且** 恰好一条 start-op、一次 thread mint、无 superseded runtime。

> ### ⚠️ v4 新增（r3-A BLOCKER-2）：两级 operation 的 `Failed` / `Stuck` **没有父任务映射**
>
> v3 只规定了 bootstrap 的重试、顺序与 exactly-once，**全部落在成功时间轴上**。
> **攻击**：`child-wave` op 已成功、Draft child 已落库；稳定 idem 的 bootstrap 返回 `Stuck`。
> 每次 resume 都命中同一条终态 op，harness 不存在，running flip 没有成功前提；
> 而 dead-root 扫描只认最新 start-op `phase='failed'`、**明确放过其它终态**
> （`session_repo_impl.rs:177-195, 211-225`）⇒ Stuck child 与 dispatched/running parent 永久停住。
> `OperationOutcome` 把 `Failed` 与 `Stuck` 分成两臂（`operation/mod.rs:531-547`），
> 而既有普通 spawn **两臂都送进 `fail_spawn`**（`scheduler/mod.rs:1031-1036`）—— v3 一臂都没消费。
>
> **v4 裁决：写死两级 outcome 表。**
>
> | operation | outcome | 父任务 | child skeleton |
> |---|---|---|---|
> | `child-wave` | `Failed` / `Stuck` | eventized guarded `failed('child-wave-create-failed'/'-stuck')` | 尚未创建（`prepare_tx` 回滚）|
> | bootstrap（`spec-harness-start`）| `Failed` / `Stuck` | eventized guarded `failed('child-wave-bootstrap-failed'/'-stuck')` | **已存在**：置 `Failed` lifecycle，使其可被人删除 / 不再计入静止判定 |
>
> **验收**：`Failed` 与 `Stuck` **各注入一次**（两级 × 两臂 = 4 例），
> 断言父状态、理由字符串、事件，**以及重启后幂等**（不得反复重试同一终态 op）。
> 变异 = 只消费 `Failed` 臂、漏掉 `Stuck`（v3 的形状）。

### 3.4 播种

用现成 `SpecHarnessStartOperationPayload.goal → Observation::WaveGoal`
（`spec_harness_start_adapter.rs:191-207, 323-368`、`harness/mod.rs:482-487`），
把**冻结行**的 `goal / acceptance / context / cwd` 渲染成稳定文本。

**明确不用 `workflow_input`**：无 `workflow_id` 必拒、descriptor 无 `input_schema` 必拒
（`routes/waves.rs:573-614`、`plugin_host/workflow_input.rs:238-283`），
且 plugin 失信时 binding **降级 vanilla prompt 并丢弃 input**
（`spec_harness_start_adapter.rs:170-179`）—— 静默不注入。

### 3.5 继承矩阵（每一行都要有断言，尤其「不继承」的）

| 字段 | 子 wave | 依据 |
|---|---|---|
| `cove_id` / `cwd` | **继承** | workspace 身份与运行目录（`calm-types/src/model.rs:370-404`）|
| `parent_wave_id` | 父 id | —— |
| `lifecycle` | `Draft` | `wave.rs:33-50, 70-84` |
| `workflow_id` / `workflow_input` | **不继承** | 否则子 wave 再吃一遍父 workflow 的 plan/gates |
| `purpose` | **不继承** | server-owned structural marker（`calm-types/src/model.rs:408-410`）|
| `theme` | 不适用 | `wave_create_tx` 不消费 `NewWave.theme`（`wave.rs:47-84`）；占位用 `default_dark`（`routes/today.rs:172-187`）|
| archive / pin / terminal | **不继承** | 同上 |
| *(PR-B)* `tree_task_budget` | 显式 `NULL` | 单一真源 |

**不得把 task 自己声明的 `cwd` 提升为子 wave 的 cwd** —— 那个值没过 cove-folder ownership 复核
（`routes/waves.rs:423-521`）。

### 3.6 拒绝路径：fail-closed，禁止静默降级

深度耗尽 ⇒ 父任务 fail-closed 终结 + 可读理由 `sub-wave-depth-exceeded`
（PR-B 另加 `sub-wave-tree-budget-exhausted`）。
**明令禁止静默降级为 `in-wave`** —— 那是不留痕地改变执行语义，
且让「上界生效」与「上界未生效」在观测上不可区分。

---

## 4. 父任务的 status 与 liveness（v1 完全没有这一维）

> ### ⚠️ BLOCKER-A2 / B4 修正：2 小时 worker deadline 会杀掉合法运行的子 wave
>
> `reconcile_spawn_result → mark_running` **无条件**盖
> `running_deadline_ms = now + task_run_timeout_ms`（默认 7200s：`scheduler/mod.rs:88, 491-505, 1045-1058`；
> 底层 `task.rs:281-302`）。sweep 的 Running 臂按 `kind` 分支：codex/claude 过期即
> `fail_running_liveness_timeout`（`scheduler/mod.rs:1271-1296`、`:277-279`）。
> 子 wave 动辄跑几小时 ⇒ **父任务 failed、子 wave 无人 teardown、其 spec 行永远计入树预算**
> （PR-B 的谓词按 `wave_id ∈ subtree` 计数，与父任务是否终结无关）⇒ 树预算被幽灵占用。
>
> 换 `kind: terminal` 也不安全：走 `reconcile_running_terminal`，按 `worker_card_id`
> 找不到 terminal 行 ⇒ debug 日志 + 原地留行（`scheduler/mod.rs:1638-1660`）⇒ **父任务永远 running**。
>
> **所有快测试都会绿** —— 没有一条有「子 wave 非终态跨过 `task_run_timeout_ms`」的时间轴。

**裁决**：

1. sub-wave 任务的成功 reconcile 走**专用**的 `dispatched → running` 写：
   `running_deadline_ms = NULL`，**且不得把 child 的 result id 当 `worker_card_id`**。
2. **liveness 判定要改的是三个站点，不是一个**（r2-B MAJOR-3）——
   `task_kind_has_running_liveness_deadline` 的签名是 `fn(TaskKind) -> bool`
   （`scheduler/mod.rs:277-279`），**承载不了 `spawn`**。三处必须按 `(spawn, kind)` 判：
   - `scheduler/mod.rs:1271` 匹配臂守卫
   - `scheduler/mod.rs:1290` 复核
   - `scheduler/mod.rs:1319` **回填函数 `stamp_missing_running_liveness_deadline`** ——
     这一处最隐蔽：它会主动给 `running_deadline_ms IS NULL` 的行**补盖**一个未来 deadline。
3. **sweep 的 `spawn='sub-wave'` 臂必须显式排在 terminal 臂与 kind 超时臂之前**
   （`scheduler/mod.rs:1268` 的 `Running if kind == Terminal` 今天在前）。
   **不依赖 §1.6 的写口校验来保证 terminal 不会出现** —— 存量行与未来放宽都会绕过它。

> ### ⚠️ r2-A MAJOR：v2 的 #11「有时钟推进的时间轴」**只跑一轮时删掉短路仍然绿**
>
> 生产 sweep 是**先补盖再判断**（`scheduler/mod.rs:1271-1296`，补盖值 `now + timeout`，`:1318-1347`）。
> 删掉短路后，第一次 sweep 只会从**当前时刻**盖一个未来 deadline，**不会 fail**；
> 要第二次推进 + 第二次 sweep 才杀父任务。

**验收（v4 改写）**：
- **两轮** `advance(timeout + 1)` + sweep，**每轮都断言**父任务仍 `running` 且
  `running_deadline_ms IS NULL`；
- **变异只有一处是行为变异**（r3-B 修正 v3）：`:1290` 与 `:1319` 都**嵌在 `:1271` 那条臂内部**，
  臂序修好后它们对 sub-wave 行是**死代码** ——「三个站点各一条会红的用例」**不可满足**，
  v3 那句要求本身是错的。
  ⇒ 行为变异打 `:1271` 的臂守卫/臂序；`:1290` 与 `:1319` 改为**结构断言**
  （源码序：sub-wave 臂在 terminal 臂与 kind 超时臂之前），变异 = 调换臂序。

---

## 5. 父任务闭合

### 5.1 载体

| 列 | 类型 | 谁写 | 谁读 |
|---|---|---|---|
| `tasks.child_wave_id` | `TEXT NULL` + `UNIQUE INDEX ... WHERE child_wave_id IS NOT NULL` | `child-wave` op 的 `prepare_tx`，guarded COALESCE stamp（照抄 `task.rs:275-302`）| 闭合逻辑（窄 reader）、读时回显、reopen 守卫 |

唯一索引固定「一个子 wave 只属于一条父任务」，并承担按 child id 的反查
（现有索引以 `wave_id` 开头，`0058:49-54`，承担不了）。

**该列 claim 后才写** ⇒ #1030 第三条缺口的豁免继续成立。
**`spawn` 是 claim 前写的列 ⇒ 明令不得进读时回显**，否则该豁免当场失效。
两条写进代码注释。

### 5.2 映射：子 wave 状态 ⇒ 父任务

| 子 wave | 父任务 | 依据 |
|---|---|---|
| `Done` **且子 wave 无 in-flight 任务、也无残留 `pending`** | `task_report_success_from_worker_tx(TaskReporter::Kernel)`：**有 gate ⇒ `verifying`，无 gate ⇒ `done`**（`task.rs:417-541`）| 父 task 块的 `gate` 是独立的机器验收合同 |
| `Done` **但子 wave 仍有 in-flight 任务**（`dispatched`/`running`/`verifying`）| **no-op + 诊断**，等下一轮 | 三态各有自然推进写者，必然离开 |
| `Done` **且子 wave 只剩 `pending`** | **`failed('child-wave-incomplete')`**，理由带残留行数 | 见下 |
| `Failed` | `failed('child-wave-failed')` | —— |
| `Canceled` | `failed('child-wave-canceled')`，**不是 canceled** | 既有 canceled 只允许 `pending→canceled`（`task.rs:161-172`）|
| 行不存在（被删）| `failed('child-wave-deleted')` | **不得清成 NULL** —— 会把「从未建成」与「建成后被删」混为一谈 |
| 非终态 / archived | **no-op** | archive 是正交可见性字段（`calm-types/src/model.rs:275-279`）|
| `SuccessReportFlip::None` | **视为「已被别人处理」，no-op 不重试** | 三种 None 情况见 `task.rs:528-542`（评审 MINOR-4）|

> ### ⚠️ v3 新增（r2-A BLOCKER）：`Done` 不代表子 wave 已静止，父 gate 会与子 gate 并发写同一 cwd
>
> **攻击**：子 wave 有两个 gated task。第一个 gate 结束把 lifecycle 推到 `Reviewing`，
> 第二个 gate 仍在跑。spec 此时**合法**写 `Reviewing → Done`
> （`crates/calm-types/src/wave_lifecycle.rs:279-286`）⇒ 父闭合立即把父 task 推到 `verifying`
> 并启动父 gate。父 task 未声明 `cwd` 时父 gate 落**父 wave 的 cwd**
> （`operation/task_verify_adapter.rs:668-681`），而子 wave 继承同一 cwd（§3.5），
> 子 task 的 gate 也回退到该 cwd ⇒ **两个 shell gate 并发改同一个目录**。
> 更基本的一层：父任务已判「成功」，而子 wave 仍有非终结任务。
>
> **v2 的 §7.1 反而帮了倒忙**：它强制父 fixture 带**非默认 `cwd`** 以增加判别力，
> 恰好**绕开了两个 gate 都回退到 wave cwd 的生产分支**。
> 这是「为提高判别力加的约束，把真正危险的分支排除掉了」——一种新形状，记进 §7.1。
>
> **裁决**：`Done → 父成功` 的映射增加 **quiescence 前置条件**，在**同一个 IMMEDIATE 事务内**判定。
> **验收必须造「双 gated task、一条仍 parked」的交错，且父 task 与 gate 的 `cwd` 均缺席。**

> ### ⚠️ v4（r3 两通道独立收敛的唯一 BLOCKER）：quiescence 计入 `pending` 是**确定性活锁**
>
> **攻击**：子 wave 留一条 `pending` 任务，再合法 `Reviewing → Done`
> （FSM 不检查 task：`calm-types/src/wave_lifecycle.rs:279-286`）。此后：
> - `Done` 是终态，`lifecycle_allows_scheduling` 挡住新 claim
>   （`scheduler/mod.rs:147-155`；pending claim 排在该 gate **之后**：`:552-585`）；
> - **全仓没有任何「wave 进终态时清理残留 pending」的写点**
>   （`task.rs:161-171` 只有显式 cancel，且只允许 `pending→canceled`）；
> - ⇒ 该行永不终结 ⇒ quiescence 永不成立 ⇒ 父任务永久 `running` 且
>   `running_deadline_ms IS NULL`（**§4 亲手删掉了唯一的收割器**）
>   ⇒ 树预算幽灵占用 ⇒ 父 wave 被 descendant 守卫锁死。
>
> **v3 的 §7 没有一条买「子 wave 静止后必须闭合」** —— 只买了「不许早闭合」。
> **修一个安全性质时只写了安全侧的验收，活性侧一条没有。**（本设计第四次同形状。）
>
> **v4 裁决（两段）**：
>
> 1. **quiescence 只数 in-flight 三态**（`dispatched` / `running` / `verifying`），**不数 `pending`**。
>    这三态各有自然推进写者（`resume_dispatched` / worker+deadline / `drive_gate` ——
>    且 `drive_gate` 排在 lifecycle gate **之前**，Done 的 wave 里 `verifying` 仍会推进），
>    **必然离开**；`pending` 是唯一没有写者的状态，把它计入即活锁。
> 2. **残留 `pending` 不静默放过**：子 wave `Done` 且只剩 `pending` ⇒
>    父任务 **`failed('child-wave-incomplete')`**，理由带残留行数。
>    子 wave 自称完成而已声明的工作从未执行 —— 判成功是静默丢工作，判失败是可诊断的事实。
>
> **验收必须成对（安全 + 活性）**：
> - 安全侧：`Done` + 一条 `verifying` ⇒ **no-op**，父 gate 不启动；
> - **活性侧（v3 完全没有）**：该 `verifying` 终结后 ⇒ 父任务**最终闭合**。
>   变异 = 把 `pending` 加回 quiescence 集合 ⇒ 用 `Done` + 残留 `pending` 的 fixture **必红**。
> - 残留 pending 侧：`Done` + 只剩 `pending` ⇒ 父 `failed('child-wave-incomplete')`，
>   `origin='block'` 与 `legacy` **各一例**。

> ### ⚠️ BLOCKER-A4 / MAJOR-B4 修正：子 wave 终态**不单调**
>
> 终态可由 user reopen 回 `Planning`（`calm-types/src/wave_lifecycle.rs:241-252`）。
> v1 下同一份最终 DB 状态按事件顺序不同会得出不同的父任务结论；
> 且父任务终结后 sweep 只枚举 `dispatched/running` ⇒ **永久失联**。
>
> **裁决（fail-closed，无交错）**：
> **被 `tasks.child_wave_id` 引用的 wave 一律禁止 reopen**，返回 Conflict
> 并指名父任务 key。唯一索引让这个反查是 O(1)。
> 这消灭整个 reopen 交错族，不需要给 tasks 加 reopen 状态机。
>
> **v3：守卫的落层必须写死（r2 两通道同时指出）。** v2 只写了产品裁决、没指定函数落点，
> 于是 REST route 加反查即可让验收全绿，而
> `RepoSyncDomainRaw::wave_update`（`session_repo_impl.rs:309-313`）
> 仍直达机械的 `wave_update_tx` —— 后者明说「校验在调用者、自己机械写」
> （`crates/calm-truth/src/db/sqlite/wave.rs:115-147`）。
> ⇒ **守卫放进所有 lifecycle writer 共用的 in-tx helper**，不是 route。
> 验收必须**覆盖非路由入口**（raw repo 直调），route 那条只是快失败。
>
> **另一半**：child 的 lifecycle 读取必须**放进与父任务 flip 相同的 IMMEDIATE 事务**，
> 且 UPDATE 的 SQL guard 要复核 child 当前 lifecycle 与存在性 ——
> 不能「事务外读到 Done、事务内只 guard 父状态就写 success」。

### 5.3 双路径：live 快路 + sweep 兜底，共用一个 guarded 函数

**只挂事件会失活**：EventBus 是 bounded broadcast，无订阅者时静默成功
（`calm-truth/src/event_bus.rs:108-129`）；dispatcher lag 明说丢事件（`dispatcher/mod.rs:806-830`）；
重启不 replay。可靠性来自 boot sweep 与周期 sweep（`scheduler/mod.rs:1183-1200`）。

- **live 快路**：dispatcher 已订阅 `wave.lifecycle_changed` / `wave.deleted`
  （`dispatcher/mod.rs:743-782`），但 lifecycle arm 今天只 `poke(id)`、不看 `to`（`:1010-1024`）。
- **sweep 兜底**：新增 arm，枚举 `status IN ('dispatched','running') AND child_wave_id IS NOT NULL`，
  照抄 `reconcile_running_terminal` 的形状（`scheduler/mod.rs:1634-1698`）。
- **两条路共用 `reconcile_child_wave_task`**。**事件只负责低延迟，DB 当前态是裁决真源。**

> ### ⚠️ MAJOR-A1 修正：闭合必须是 **eventized** 事务
>
> v1 只点名了 DB flip 函数。但 `task_report_success_from_worker_tx` 本身只做 UPDATE、
> **不产事件**（`task.rs:523-541`），而 gate 快路依赖 `TaskCompleted` 去 poke
> （`dispatcher/mod.rs:975-1005` → `scheduler/mod.rs:552-570`）。
> 不发事件 ⇒ 有 gate 的父任务进 `verifying` 后**只能等下一轮 300s sweep**，spec 也收不到通知。
>
> **修法**：`reconcile_child_wave_task` 是「guarded flip + `TaskCompleted`/`TaskFailed`
> + 必要 lifecycle events」的 eventized 事务。
> **验收不得只断言父状态到 `verifying`** —— 必须断言 gate attempt 实际启动/终结。

### 5.4 删除与取消

- **有 descendant 的 wave 拒删（Conflict）**，不做递归 leaf-first 删除
  （递归删除会漏 descendant 的进程 teardown、无逐 child `WaveDeleted`、
  可能被 terminal→card 的 RESTRICT 中止：`routes/waves.rs:1324-1342`、`0045_worker_sessions.sql:1-7`）。
  递归子树删除按 cove 删除先例（`routes/coves.rs:317-388`）后推，登记 §12.1。

  > **v3：这条守卫必须下沉进 `wave_delete_tx`，不能只放 route（r2 两通道同时指出）。**
  > `Repo::wave_delete`（`db/mod.rs:554,598` → `wave.rs:242`）是第三条入口，绕过 route。
  > route 层可保留一次早拒绝以给出好文案，但**唯一的正确性载体是 tx 内那一条**。
  > 验收要**分别测三个入口**：route / `Repo::wave_delete` 直调 / cove 删除。

- **删 cove 不受上一条影响**：它是单条 `DELETE FROM coves`，同 cove 的 waves 经
  `cove_id CASCADE` 在该语句内整体消失，`NO ACTION` 自 FK 在语句结束时已无悬垂引用（§2.1 实证）。
  **descendant 守卫不得挡住 cove 删除** —— 它挂在 `wave_delete_tx` 上，而 cove 删除不走那条路径。
- **父 wave 被 cancel（评审 MAJOR-B7）**：cancel 只改 wave lifecycle，不终结其 task 行；
  子 wave 继续跑，父 wave 又因有 descendant 删不掉。
  **恢复路径是存在的且必须写进拒删文案**：用户先删/取消**子 wave 本身**
  ⇒ 父任务落 `failed('child-wave-deleted')` ⇒ 父 wave 无 descendant ⇒ 可删。
  拒删错误信息必须指名子 wave id。登记 §12.1。

---

## 6. 读时回显：`childWaveId`

`attach_task_read_state`（**不是 `attach_task_read_state_tx`，那个名字不存在**）
在 `wave_projection_state` 的单条 SELECT 里附加状态列
（`task_projection.rs:408-440`，`task_read_state_json` 子查询在 `:421-428`，**取 7 个字段**），
写进 DTO `BlockVerdict{status, gate_result, worker_card_id, withdrawal, ...}`
（`:936` 有 `withdrawal`，v1 的字段清单不全 —— 评审 MINOR-1），
经 `task_diagnostics(include_read_state=true)` → `WaveReportReadResponse.task_diagnostics`
（`wave_report_read.rs:91-103`、`read.rs:558-589`、`routes/waves.rs:1543-1581`）
到前端按 block id 配回（`web/src/pages/WaveReportPage.tsx:467-478`）
并在 `ReportTaskBlock` 渲染（`web/src/pages/report-blocks/task.tsx:119-131`）。

**`#1016` 约束**：必须并进 `wave_projection_state` 的**同一条 SQL**，
不恢复 deferred 事务、allowlist 保持为空。

---

## 7. PR-A 的验收与变异映射

> **「关键断言必须先证明它会红」是交付项。没有「改坏什么 → 哪个测试红了」的映射视为未完成。**

**v3 说明**：r2 判定 v2 的 23 条里 **11 条不具判别力**（#3c #5 #6 #7 #10 #11 #17 #19 #20 #21 #22）。
下表逐条替换其变异，并**删掉两条不可执行的**：v2 #3c（「应当编译失败」CI 不会跑）
与 v2 #21 的间歇性红。二者改为设计约束条目与确定性结构断言。

| # | 断言 | 变异（改坏什么）| 必须红 / 判别力说明 |
|---|---|---|---|
| 1 | **只**改 `spawn`（其余字段逐字节不变）⇒ 列被写、`PlanUpdated` 发出、随后路由改变 | **只漏 UPSERT 的变更检测析取项**（保留 SET）| **fixture 禁止同时改 goal**。绑定占位符注意：`task_projection.rs:977` 的 `?15` 被 `created_at_ms`/`updated_at_ms` 复用两次，sqlx 无编译期校验 |
| 2 | 解析后、claim 前改 `spawn` ⇒ race-lost | 去掉 `doc_rev` 栅栏 | 行仍 pending、无 `TaskContextFrozen` |
| 3a | **（承重）** claim 后改 `spawn` ⇒ 不判 material，**即时派发**路由不变 | 即时派发改成重读当前报告块 | 必红 |
| 3b | **（承重）** 同上，**重启后**路由不变 | 恢复路径改成重读当前报告块 | 必红 |
| 3c | op payload 取 tx 内重读值 | 把 `drive_spawn` 入参换回 claim 前快照 | seam：同一 IMMEDIATE 事务内改 pending 行的 `spawn`，**不动 `doc_rev`、不置 stale** |
| 4 | 缺席、显式 `"in-wave"`、显式 `null` **三者**得到同一冻结行值 | 只把**显式 `null`** 规范化错 | v2 只比前两者 ⇒ 该变异下恒绿（r2-A）。`NOT NULL` 下变异用空串或写死默认值，不是 NULL |
| 5 | child payload/seed **逐字段**来自冻结行 | 只重读 `goal`/`acceptance`/`context`/`cwd` **中的一个** | **v4 写实 seam（r3-A MAJOR-1）**：v3 只说「表驱动四次差分」，没说怎么造出「报告 ≠ 冻结行**且不 stale**」—— 四个字段全在哈希纳入集内，经文档编辑必然置 stale，于是 #5 可被降级成 builder 单测。**唯一可行的 seam 是 DB 直写**：冻结 `tasks` 行四个字段写 sentinel-A、当前报告保持 sentinel-B、`context_stale_at_ms IS NULL`，**驱动真实 child adapter 到持久化的 child seed**，expected 手写。四字段逐一变异分别红。**expected 禁止调用 payload/seed builder** |
| 5b | 编辑纳入哈希字段 + resume ⇒ **fail-closed 拒绝** | 去掉 §3.2 步骤 0 | 与 #5 拆开 —— 合在一条里两者互相抵消（r2-B MAJOR-4）。**#5 与 #5b 必须是不同的构造**：#5 走 DB seam（不 stale），#5b 走真实文档编辑（stale）|
| 6 | 深度 0/1/2/3 允许，第 4 层被拒；**且 `parent_wave_id` 等于直接父而非 root** | 判据写 `>` 而非 `>=`；`child-wave` op 把 `parent_wave_id` 写成 root | **至少一条用例经真实 `child-wave` adapter 造出深度 2**，其余边界再用裸 INSERT。v2 全靠裸 INSERT ⇒ 写错父指针全绿（r2-B MAJOR-6）|
| 7 | 2-环 ⇒ 拒绝、耗时 < 1s、理由 `sub-wave-tree-cycle` | **删掉 `WHERE up.depth <= ?2` 截断**（不是换算子）| `UNION`↔`UNION ALL` 实测同行为 ⇒ v2 的变异恒绿。另加**静态门禁**：源码断言 CTE 文本含截断子句（挂死在 CI 上表现为超时，判别力弱且慢）|
| 8 | 求根断链 / 触截断 ⇒ 拒绝 | 零行分支改成「当作根」 | fail-closed 断言 |
| 9 | 深度耗尽 ⇒ 父任务 fail-closed 终结 + 理由字符串 | 改成静默降级为 `in-wave` | 必红 |
| 10 | **表驱动**：遍历 `TASK_BOUND_ADAPTER_KINDS` 的**每一个** kind，用**该 kind 自己的合法 payload/fixture**（否则 adapter 会因别的前置失败）构造 stale task，**驱动真实 adapter**，断言零副作用**且错误是具体的 stale 变体**（不是「任意 Err」）| **从任一 adapter 删掉那一行 `refuse_if_context_stale`** | v2 只买了 `child-wave` 一个 kind。四个 adapter 各自手写一行，而 `TASK_BOUND_ADAPTER_KINDS` 只是 `[&str; 4]` 常量（`operation/mod.rs:56-61`）——**名单成员资格与实际调用之间没有机器联系**（r2-B MAJOR-5）。**r3-B 曾判此条恒绿、被 r3-A 用代码否掉**：四个 adapter 的 stale 检查确实都在第一个 DB 副作用之前（`codex_adapter/mod.rs:766-780`、`claude_adapter/mod.rs:768-780`、`terminal_adapter.rs:576-595`、`task_verify_adapter.rs:627-665`），所以可做实 —— 但**必须逐 kind 配合法 fixture + 断言具体错误变体**，否则就退化成 r3-B 说的那种恒绿 |
| 11 | **两轮** `advance(timeout+1)` + sweep，**每轮都**断言父仍 `running` 且 `running_deadline_ms IS NULL` | **三个站点各删一处**（`scheduler/mod.rs:1271` / `:1290` / `:1319` 回填函数）| v2 只跑一轮 ⇒ 首轮只补盖未来 deadline、不 fail ⇒ 恒绿（r2-A）。变异必须单点，三选二仍绿没有判别力（r2-B MAJOR-3）|
| 12 | sub-wave 任务不 stamp `worker_card_id`、`running_deadline_ms IS NULL` | 复用 `mark_running` | 两列各一条断言 |
| 13 | 子 wave `Done` **且已静止** ⇒ 按 gate 分流，且 gate attempt 实际启动/终结 | 无条件写 `done`；或不发 `TaskCompleted` | **父 task 必须带 gate**（5a 的 F2 教训）|
| 13b | **（安全侧）** 子 wave `Done` 但仍有 `verifying` 任务 ⇒ 父任务 **no-op**，父 gate **不启动** | 去掉 quiescence 前置条件 | 「双 gated task、一条仍 parked」交错，**父 task 与 gate 的 `cwd` 均缺席**（r2-A BLOCKER）|
| 13c | **（活性侧，v4 新增）** 上述 `verifying` 终结后 ⇒ 父任务**最终闭合** | **把 `pending` 加回 quiescence 集合** | 用「`Done` + 只剩 `pending`」的 fixture ⇒ 必红。v3 只买了「不许早闭合」、没买「最终必须闭合」，两通道独立判为唯一 BLOCKER |
| 13d | 子 wave `Done` 且**只剩 `pending`** ⇒ 父 `failed('child-wave-incomplete')`，理由带残留行数 | 判成功（静默丢工作）/ 判 no-op（活锁）| `origin='block'` 与 `legacy` **各一例** |
| 13e | `child-wave` 与 bootstrap 两级 op 的 `Failed` / `Stuck` **四臂**各有父任务映射 + 重启幂等 | 只消费 `Failed`、漏掉 `Stuck`（v3 的形状）| 四例。`OperationOutcome` 分两臂（`operation/mod.rs:531-547`），既有 spawn 两臂都送 `fail_spawn`（`scheduler/mod.rs:1031-1036`）|
| 14 | 子 wave `Failed`/`Canceled`/**被删** ⇒ 父任务 `failed` + 各自理由 | 被删臂清成 NULL | 三个理由字符串各一条 |
| 14b | `childWaveId` 指向已删 wave ⇒ 回显带「已删除」标志，前端渲染不可点 tombstone | 不带该标志 | 稳态下确实存在指向已删 wave 的链接（§5.2 明令不清 NULL）。标志由**同一条 SQL** 派生（#1016）|
| 15 | 事件丢失下 sweep 仍闭合 | 只留 live 订阅、删 sweep arm | **绕总线直接改 DB**（D.4 4b 形状）|
| 16 | live 与 sweep 给出同一结论 | 两条路走不同写函数 | 同构断言 |
| 17 | 被 `child_wave_id` 引用的 wave 禁止 reopen，**经 raw lifecycle writer 直调同样被拒** | 守卫只放 route | v2 的 Conflict 测公开 PATCH 即绿（r2 两通道）。守卫落在共用 in-tx helper |
| 18 | child lifecycle 读与父 flip **同事务**且 SQL guard 复核 child 现态 | 事务外读、只 guard 父状态 | 交错：读到 Done 后 child 被删 ⇒ 不得写 success |
| 19 | **两个崩溃注入点**（op 成功后 / running flip 前后）⇒ 子 wave 离开 `Draft` **且恰好一条 start-op、一次 thread mint、无 superseded runtime** | harness submit 排在 running flip **之后**；或 idem key 用 `None` | v2 只验「拿到 harness」⇒ 重复起 harness 全绿（r2-A）。idem key 写死 `child-wave:<child_id>:bootstrap` |
| 20 | 有 descendant 的 wave 拒删，文案指名子 wave id；**三个入口各一条**：route / `Repo::wave_delete` 直调 / cove 删除 | 守卫只放 route | v2 只测公开单-wave 删除（r2 两通道）|
| 21 | **结构断言**：`PRAGMA foreign_key_list(waves)` 的 `on_delete` **是 `NO ACTION`** | 改成 `CASCADE` 或 `RESTRICT` | 确定性红。v2 的「可能间歇」不是门禁（r2-B MAJOR-9）|
| 21b | cove 内含 wave 树 ⇒ 删 cove 成功、`waves` 无残留 | 自 FK 改 `RESTRICT` | 行为断言，与 #21 成对 |
| 21c | **绊线**：全表断言每条 `parent_wave_id IS NOT NULL` 的行满足 `child.cove_id = parent.cove_id`；且手工造跨 cove 边 ⇒ 删 cove **失败** | `child-wave` adapter 不复制 `cove_id` | v3 新增，两个通道都没提（§2.1 实证）|
| 22 | **序列化后的 DTO JSON 不含 `spawn` key** | 把 `spawn` 加进 `attach_task_read_state` | 若写成「结构体无该字段」是编译期恒真、不会红（r2-B MINOR-5）|
| 23 | `spawn:"sub-wave"` + `kind ∈ {claude, terminal}` ⇒ **公共写口拒绝** | 静默忽略 `kind` | v2 的「按裁决处理」让 expected 跟随实现 ⇒ 恒绿（r2-A）。§1.6 已定型，oracle 确定 |

**登记为设计约束（不是验收）**：`claim_task` 的 API 收窄使 claim 前快照在成功路径上不可达。
这是结构性修法（编译器强制），但「应当编译失败」不是 CI 会跑的东西 ——
写进 §1.4 的约束条目，**不计入「已验证的变异」计数**。

### 7.1 必须避开的、5a/5b 已交学费的形状

- **fixture 与生产共用同一个错误事实来源** —— expected 值一律手工写，禁止用被测的构造函数算。
- **「真实链路」只喂最简单那一项** —— sub-wave 用例要覆盖**带 gate、带 `depends_on`** 的父 task。
- **⚠️ 新形状（v3 记入）：为提高判别力加的约束，可能把真正危险的分支排除掉。**
  v2 的本节写「父 fixture 必须带**非默认 `cwd`**」，恰好绕开了「父 gate 与子 gate 都回退到
  wave cwd」这个生产分支 —— 那正是 r2-A 那条 BLOCKER 所在。
  ⇒ **`cwd` 要两种都测**：显式非默认的一条，**和缺席的一条**。
  凡是「fixture 一律取非默认值」的约束，都要反问一次「默认值那条分支谁在测」。
- **给 fixture 加数据但不加断言** —— 继承矩阵每一行都要断言，尤其**不继承**的那几行。
- **元测试两边同源恒真** —— 集合相等元测试引用同一个 `TASK_FIELDS` 常量。
- **名单式常量与实际调用之间没有机器联系**（#10 的教训）—— 凡是「某某在名单里」的不变量，
  验收必须**遍历名单驱动真实实现**，而不是断言名单内容。
- **变异证据有保质期。** 后续改动可能让原先的「必红」被更早的新守卫遮蔽，CI 不会提示这份
  证据已经失真。改守卫、改分支顺序或替换测试接缝时，必须重跑受影响的旧变异并修订映射；
  不能把历史上红过一次当作永久有效。
- **打实例不等于打类。** 若要守的是「不存在无界递归 CTE」，就直接扫描并拒绝这个性质，
  不要登记当前已知的常量名或枚举一种 AST initializer；新增写法不应要求人先想起更新名单。
- **跑完整门看 SLOW 标记** —— 递归 CTE 是新的复杂度来源。

---

# 第二部分：PR-B（树级预算）—— 本文只定型，不在 PR-A 交付

## 8. 两个强制点

**强制点一**：`child-wave` op 的 `prepare_tx` 第 3 步，向下 CTE 计数，
`count >= budget` ⇒ 拒绝 `sub-wave-tree-budget-exhausted`。

> ### ⚠️ v3：v2 在这里留了与 §2.2 **同形状**的坑（r2 两通道同时指出）
>
> v2 只写「向下 CTE（`UNION`，不是 `UNION ALL`）」—— 和刚被实证推翻的 §2.2 是同一个错。
> 若向下 CTE 携带任何非 id 列（计数常需 `wave_id` / `status`），环上同样不终止；
> 即使只投影 `id`，也**必须**写死上限。
> ⇒ **定型**：向下 CTE **只投影 `id`**，**且必须带 `depth <= MAX_WAVE_TREE_DEPTH` 硬截断**，
> 并**共用 PR-A 的静态门禁**（源码断言截断子句存在）。
> PR-B 的验收要单独登记「向下 2-环」与「超深中毒数据」的**耗时**测试 ——
> PR-A 的 §7 全部只打向上 CTE。

**强制点二**：`evaluate_schedulability` 加树项。**只有强制点一时 D.4 #7 为假**
（真实上界 Σ per-wave ceiling）。

> ### ✅ v5 裁决（PR-B 施工前）：树项形状 = **确定性配额分割**，不是「数兄弟的行」
>
> 设计原文把这里留成「必须裁决：rebuild 跳过树项，或整树 root-first DFS 重建」。
> 施工前的定向调研（`docs/_985-s6b-fork.md`）**证伪了这两个选项，并给出第三条**。
>
> **甲（只数兄弟 in-flight，不数兄弟 pending）—— 恒真门，不做。**
> 精确退化上界 `Σ_v min(spec_task_ceiling_v, B)`；默认 `ceiling = B = 32`
> （`task_projection.rs:17`、`985-doc-as-plan.md:1179`）⇒ **收窄量为 0**。
> 且 `N_open ≤ B` 已被强制点一独立封顶（每个未闭合子 wave 恰对应一条仍
> `dispatched/running` 的父任务行，必被全树非终结计数数到）⇒ 强制点一单独已给
> `L ≤ B·ceiling`，甲把它「改进」到同一个数。**典型 fake gate 形状。**
>
> **乙（数兄弟 pending + 整树 root-first DFS 重建）—— 救不了 D.1 #11，不做。**
> 共享预算的先到先得分配是**路径依赖**的：兄弟 C、D 各声明 2 条、`B=2`、双方 0 存量时，
> 增量结果由人的编辑顺序决定（先写 C ⇒ C 得 2、D 得 0）。
> 整树 rebuild 只能按确定性顺序得到**规范**不动点，与增量历史留下的不动点在一半情形下不同；
> 而 rebuild **无从得知「谁先来」**（pending 是产物，不能当输入 —— 当 tie-break 就是 BLOCKER-A3 的同一个坑）。
> ⇒ 乙 = **明确牺牲 D.1 #11**，整树 rebuild 只是把不确定变确定。
>
> **丙-2（确定性配额分割）—— 采纳。**
>
> ```
> effective_ceiling(W) = min(spec_task_ceiling(W), share(W, T))
> share(W, T)          = floor(B / N) ，余数按 (created_at, id) 升序分给前 r 个 wave 各 +1
> 其中 T = W 所在的树，N = |T|，B = 树根的 tree_task_budget（NULL ⇒ 默认 32），Σ share = B
> ```
>
> **为什么它同时满足两边**：三个输入 —— 本 wave 文档、本 wave 在飞、**树的形状（`waves` 行）**
> —— **没有一个是投影的产物** ⇒ rebuild 序无关，**D.1 #11 原样成立**；
> 而上界 `Σ_v live_spec(v) ≤ Σ_v share_v = B` —— **这才是 D.4 #7 想要的那条**。
>
> **两个强制点的相容性（修复轮 1 补齐的设计缺口）**：点一除了
> `inventory < B`，还必须满足**创建后** `N + 1 <= B`。因此凡是点一放行的新树形，点二
> 对其中每个成员都给出 `share >= 1`；不得再出现「创建刚放行、投影立刻给新成员零份额」的
> 组合死锁。反方向的边界也写死：`N=1` 仍是树公式的一部分；孤根只有在**有效 B**
> （根列值，`NULL` ⇒ 内核默认，且与点一共用同一个读取函数）`>= spec_task_ceiling`、可证树项
> 不会更紧时才允许零递归短路；否则必须返回 `members=1, share=B`。因此 ceiling=40、B=NULL
> 仍按默认 B=32 只准入 32，PATCH 回 NULL 不能解除上界。若人事后把 B 调低到现有 N 以下，
> 既有树可暂时出现零份额（与调低 ceiling 的既有退化同形），但点一拒绝继续增员；恢复动作
> 是提高根预算或删除多余子 wave，而不是等待别的 wave 的任务完成。
>
> **`external_occupied` 补丁随之取消**（它是 BLOCKER-A3 的修法，丙-2 下不再需要 ——
> 份额完全不依赖 pending，因此没有「自己的输出计入自己的占用」那个回路）。
>
> **唯一的新语义**：树变大 ⇒ 份额缩小 ⇒ 超额 pending 在其下次投影按既有顺序被裁。
> 这与「人把 ceiling 调低到当时在飞数以下」**完全同形**，而那条退化语义文档已批准写死
> （`985-doc-as-plan.md:1194-1196`）⇒ **复用既有语义，不是新风险**。
> 关键性质：**份额不随 pending 变化**，所以裁掉超额 pending 不会反过来改变份额 ⇒ **不振荡**。
>
> **已知代价**：树深/宽时份额偏紧（N=10 ⇒ 每 wave 3 条）。属常数标定问题，
> 与其余常数一并按可观测量标定（`985-doc-as-plan.md:1198-1204`）。登记进 §12.1。
>
> **查询面**：解根复用 PR-A 的 `WAVE_ROOT_DEPTH_SQL`；向下 CTE **只数 wave 不数 task**
> （走 `idx_waves_parent_wave_id`，`0071_sub_wave_tree.sql:7-8`），比乙更轻。
>
> **D.4 #7 的最终表述**（PR-B 合入时改成这条，去掉 PR-A 留下的「尚未成立」旁注）：
>
> > 树内 `declared_by='spec'` 的非终结行 ≤ `tree_task_budget`。
> > 由**两个强制点**共同保证：子 wave 创建准入（全树非终结计数）与
> > `evaluate_schedulability` 的**确定性配额分割**（`min(ceiling, share)`，`Σ share = B`）。
> > **配额分割而非共享计数**是刻意的：它使树项只依赖树的形状而非投影产物，
> > 从而 D.1 #11「rebuild ≡ 增量差分」得以保持 —— 共享计数在这一点上做不到（先到先得是路径依赖的）。

> ### ⚠️ BLOCKER-A3（**v5 后已由丙-2 从根上消除，保留作为记录**）：树项直接数 pending 会让投影**非幂等**
>
> 树计数含 pending，但 pending 行是投影的**输出**、不是占用输入
> （`task_projection.rs:777-796`）。于是一次无关的 prose 编辑 ⇒ 树项把本 wave 自己的
> pending 行数进 occupied ⇒ capacity=0 ⇒ 声明变 unschedulable ⇒
> **删除阶段删掉它**（`:839-876, 939-945`）⇒ 下一次写 count=0 又重建。
> **预算从未超，任务却在振荡消失。**
>
> **修法**：`external_occupied = 全树非终结 − 本 wave 的 pending`，
> 再把本报告的 clean spec declarations 按稳定顺序重新准入。
> **验收**：「同一文档投影两次 ⇒ 结果与事件完全相同」+「删 A 同事务把额度释放给 B」。

**其余必须在 PR-B 解决的（评审 MAJOR）**：

- **M-B2**：`evaluate_schedulability` 里求不到根时**必须 fail-closed**
  （诊断码 `tree_root_unresolved`），不能「没有树就跳过树项」—— 一条断链会让整棵子树无约束。
- **M-B3**：`tree_task_budget` **必须有写入面**（`NewWave` / `WavePatch`），
  否则每个 root 恒为 `DEFAULT 32`、永远无法调，§12.1 的「校准装置」无处落地，
  且测试只能用裸 SQL 改列 —— 那是「fixture 绕过生产创建路径」的形状。
- **M-A3 / M-B1**：`§7 #11`「非树 wave 逐字节不变」是**恒真断言**，
  证明不了「零新增查询」。要么给树项一个可计数的接缝（语句计数 / 可注入函数断言 0 次调用），
  要么删掉这条，别留恒真断言充数。
- **M-B8**（**v5 已裁决**：丙-2 确定性配额分割）：原问题是「树项让本 wave 的准入依赖兄弟 wave 的行
  ⇒ rebuild 不再是本 wave 文档的纯函数」。丙-2 让树项**只依赖树的形状**（`waves` 行，非投影产物），
  问题从根上消失。**验收仍要保留**：「同一棵树两种 rebuild 序 ⇒ 同一结果」，
  且**变异 = 把 `share` 改成依赖兄弟 pending 的共享计数 ⇒ 该验收必红**。
- 单一真源：`tree_task_budget` 只在树根有意义，子 wave 建时**显式写 NULL**
  （`wave_create_tx` 是固定列清单 `wave.rs:47-63`，不写就吃 `DEFAULT 32` ——
  每个子 wave 都拿到自己的 32）。**正面断言该列为 NULL**，不要断言「预算生效」（值相等时恒真）。

---

## 9. 门（CI 原样命令）

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings
cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci   # nextest 在 .local-bin/
web: npm run gen:api  然后  git diff --exit-code -- web/src/api/openapi.json web/src/api/generated.ts \
     web/src/api/generated-terminal.ts web/src/api/generated-events.ts web/src/editor/types/
web: npm run build && npm run test
fe:  npm run lint && npm run build && npm run test        # 已链 test:wire + test:mock-drift
     漂移则跑 npm run mock:generate 并提交产物
```

PR-A 必然产生 OpenAPI / TS / Zod 生成物差异（`childWaveId`），**必须提交产物**。

---

## 10. 文档修订清单（随 PR-A 提交）

| 位置 | 改什么 |
|---|---|
| §5.2 | `spawn` 排除的旁注整段改写（§1.7）；「判决强制点只有四个」⇒ **五个** |
| §5.6 | 判决强制点 4 ⇒ 5，第五个是 `child-wave` 的 `prepare_tx` |
| §3.5 | `spawn:"sub-wave"` 已落地；指向继承矩阵与拒绝路径；`kind` 语义（§1.6 裁决结果）|
| §8 | 第三层标注「PR-B 交付」；补两个强制点与谓词差异 |
| §11 切片 6 | 拆成 PR-A / PR-B；删「父块的 `neige://wave/<child>` 反链」，改为 `child_wave_id` + 读时回显 |
| 附录 C.1 | 新增 `spawn`、`child_wave_id` 两行 |
| 附录 C.2 | `parent_wave_id` 四格（**`NO ACTION`，即默认** + partial 索引 + `CHECK(parent<>id)`；**不进 `Wave`/`WaveRow`**；**不进 `NewWave`**）；`tree_task_budget` 标 PR-B |
| 附录 C.4 | `MAX_WAVE_TREE_DEPTH` 耗尽行为 = 父任务 fail-closed 终结；`tree_task_budget` 标 PR-B |
| 附录 D.1 3d | `spawn ⇒ 不判` 的旁注改写（§1.7）|
| 附录 D.2 切片 6 | 五条 ⇒ 二十三条（§7），并标注哪些属 PR-B |
| **附录 D.4 #7** | **旁注：树上界在 PR-A 后尚未成立，真实上界 Σ per-wave ceiling；由 PR-B 的两个强制点交付** |
| §12.1 | 登记：递归 leaf-first 子树删除后推（恢复路径 = 先删子 wave）；父 wave cancel 的收场；`MAX_WAVE_TREE_DEPTH=3` 仍是猜的 |
| plan template | 登记 `spawn ∈ template_exclusions` 这条策略约束（`plan.rs:918-933`）|

---

## 11. migration `0071_` 的形状（两个通道各自实证，结论一致）

**不需要 `0058_` 式整表重建。** 三条都可原地 additive：

```sql
ALTER TABLE waves ADD COLUMN parent_wave_id TEXT NULL REFERENCES waves(id)
  CHECK (parent_wave_id IS NULL OR parent_wave_id <> id);          -- NO ACTION（默认）
CREATE INDEX ... ON waves(parent_wave_id) WHERE parent_wave_id IS NOT NULL;
ALTER TABLE tasks ADD COLUMN spawn TEXT NOT NULL DEFAULT 'in-wave';
ALTER TABLE tasks ADD COLUMN child_wave_id TEXT NULL;
CREATE UNIQUE INDEX ... ON tasks(child_wave_id) WHERE child_wave_id IS NOT NULL;
```

- 带 `REFERENCES` 的 `ADD COLUMN` 在 `foreign_keys=ON` 且事务内**合法**，
  硬性前提是默认值为 NULL；原地加 `CHECK` 的先例见 `migrations/0067_task_context_freeze.sql:2-6`；
  仓库明确记载「NOT NULL + REFERENCES」不能一步加（`0054_worker_sessions_card_id.sql:1-13`）。
- **`0058_` 之所以重建整表，是因为要修改既有的 `kind CHECK`**（`0058_tasks_kind_claude.sql:1-7`），
  不适用于本片；且被引用表重建会触发 ON DELETE actions（`0054_` 的警告）。

> **必须把「禁止整表重建」写进 migration 方案。** 不写，实现者会照 `0058_` 先例
> 去重建 `waves`（14 列 + 6 个索引 + `idx_waves_one_launchpad` 部分唯一索引）。

**`parent_wave_id` 不进 `Wave` / `WaveRow`**：`WaveRow` 的 14 列显式 SELECT 有 8 处
（`wave_lifecycle.rs:141`、`wave.rs:95`、`read.rs:123,134,157`、`today.rs:72,78`、`snapshot.rs:41,269`），
sqlx 运行时才炸；`spec_task_ceiling` 的先例已证明 waves 列可以不进 `WaveRow`。**设计显式写「不进」。**

**`parent_wave_id` 的写入载体**（r2-B MAJOR-2）：**不进 `NewWave`** ——
`NewWave` 由 REST body 构造，塞进去就变成客户端可设的树指针。
由 `child-wave` adapter 在 INSERT 后以定向单列 UPDATE 写，或走一个内核专用的建 wave 入口。
**验收要有「REST 创建请求带 `parent_wave_id` ⇒ 被忽略/拒绝」的负面断言。**

---

## 12. 评审状态

三轮双通道设计评审，逐轮收敛：

| 轮 | codex 通道 | subagent 通道 | 结论 |
|---|---|---|---|
| r1 | 4 BLOCKER / 5 MAJOR | 5 BLOCKER / 7 MAJOR / 5 MINOR | NO。**两通道清单几乎不相交**（交集只有「是否拆片」）|
| r2 | 3 BLOCKER + 5 MAJOR（最小阻塞集 3）| 4 BLOCKER / 9 MAJOR（最小阻塞集 7）| NO。**两通道在自 FK 语义上直接冲突** ⇒ 用 sqlite3 实证裁决 |
| r3 | 2 BLOCKER / 1 MAJOR | 1 BLOCKER / 6 MAJOR | NO，但**两通道独立收敛到同一条**；两边均判「修完即 YES」|

**v4 已清空 r3 的最小阻塞集**（B1 quiescence 活锁、B2 两级 outcome 表、M1 的 #5 seam、
M1' 的 #11 不可满足要求、MINOR 的三处自相矛盾）。

r3 两通道各自坐实、可不再复议的：
1. `cove_delete_tx` 无逐 wave 删除 ⇒ §2.1 的 `NO ACTION` 成立（两通道各自读码 + 各自跑 sqlite3）；
2. 跨 cove 父子边今天只有 `child-wave` adapter 一条写路径；
3. 第二层 sub-wave 不与第一层 idem 碰撞（task id 是 `wave_id:key`，唯一键还带 kind）。

**§7 现有 33 个编号行**（含 3a/3b/3c、5b、13b–13e、14b、21b、21c）。

**未在本片解决、已登记的**：一个活着但永不终态的 child spec 可让无 deadline 的父任务长期 `running`。
这是本设计选择的**人工 cancel / delete 恢复语义**（§5.4 给出了完整恢复路径），
不是「系统已失去任何自然进展写者」的那类永久等待 —— 后两类已由 v4 的 B1/B2 消灭。登记进 §12.1。
