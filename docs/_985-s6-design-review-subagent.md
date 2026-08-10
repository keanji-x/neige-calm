# #985 切片 6 设计增量 · 对抗性评审（subagent 通道）

> 对象：`docs/_985-s6-design.md`。基线 `/tmp/wt6b` @ origin/main `9d30006a`。
> 只读；未跑 cargo。所有指控带 `文件:行号`。
>
> **我的清单与 §10 的交集/差集**（先独立成形，再对照）：§10.1（不拆片）我给出了一条 §0 声称不存在的
> 自洽切分线，见末节 → **交集，结论相反**；§10.2（强制点二）不反对它存在，但证出其**验收恒真**（M1）
> 且引入 rebuild 序依赖（M8）→ **差集**；§10.3/§10.4 同意现裁决，无补充。
> 我的 B1–B5 / M2 / M3 / M4 / M5 / M7 **全部不在 §10、也不在 §7 的十七条变异里**。

---

## BLOCKER

### [BLOCKER] `waves` 自 FK 会让**删 cove** 在存在 wave 树时直接崩掉，§5.4 只推理了删 wave
**攻击**：cove C 内有 root W 与其子 wave S（`S.parent_wave_id = W.id`）。用户删 cove C。
`cove_delete_tx` 逐 wave 清 vcs/tasks/sessions 后只 `DELETE FROM coves`，**waves 行靠 `cove_id` 的
`ON DELETE CASCADE` 连带删**（`crates/calm-truth/src/db/sqlite/cove.rs:147-189`、
`crates/calm-truth/migrations/0001_init.sql:22`）。级联是逐行的；一旦先删到 W 而 S 行还在，
新增的 `parent_wave_id REFERENCES waves(id)` **NO ACTION 立即检查**就报
`FOREIGN KEY constraint failed`，整个 cove 删除事务回滚。删除顺序由 SQLite 内部决定，
所以这是**顺序相关的间歇性失败**，不是稳定红。
**证据**：`crates/calm-truth/src/db/sqlite/cove.rs:182-188`（只删 coves，不删 waves）；
`crates/calm-truth/migrations/0001_init.sql:22`（waves.cove_id CASCADE）；
`crates/calm-truth/src/db/sqlite/mod.rs:204,214`（`foreign_keys=ON`，FK 真的在执行）；
设计 `docs/_985-s6-design.md:363-373`（§5.4 全篇只谈 `wave_delete_tx`，一个字没提 cove 删除）。
`docs/_985-s6-fork5.md:34` 把 `routes/coves.rs:317-388` 当成「子树删除先例」引用，恰恰漏掉了它自己会被这条 FK 打断。
**为什么现有验收抓不到**：§7 #17 只断言「有 descendant 的 **wave** 拒删」。cove 删除路径不在十七条里，
现有 cove 删除测试的 fixture 里没有树形 wave ⇒ 全绿。
**建议**：自 FK 写成 `ON DELETE CASCADE`（cove 删除已经在 handler 里做完全部进程 teardown，
`routes/coves.rs:342-351`，级联只剩行清理），或改 `DEFERRABLE INITIALLY DEFERRED`；
并在 §7 增一条「cove 内含 wave 树 ⇒ 删 cove 成功且无孤儿」。

### [BLOCKER] 两条递归 CTE 用 `UNION ALL`，环 = 无限递归 + 卡死写者槽；`零行 fail-closed` 不覆盖「无限行」
**攻击**：`waves` 出现 `A.parent=B, B.parent=A`（`CHECK(parent<>id)` 只挡自环，FK 只要求两行都存在，
**挡不住 2-环**）。§2.3 的向上 CTE 从 A 出发：A→B→A→B… `UNION ALL` 不去重 ⇒ 永不收敛。
这条 CTE 跑在 `child-wave` op 的 BEGIN IMMEDIATE 事务里（§3.2 步骤 1），
而 IMMEDIATE 已经**握着全库写者槽**（`crates/calm-truth/src/db/sqlite/infra.rs:10-24`，设计自己在 §4.1 引用）
⇒ 单条中毒数据把整库写入挂死，且 §2.3 的「零行 ⇒ 拒绝」判据永远到不了。
§4.2 的向下 CTE 同形。
**证据**：设计 `docs/_985-s6-design.md:159-166`、`:258-262`（两处均 `UNION ALL`）；
设计 `:155-156` 引用的生产先例 `crates/calm-truth/src/wave_vcs/gc.rs:271-280` **用的是 `UNION`**（去重 ⇒ 环上自然终止），
设计抄了先例的位置却换掉了唯一保证终止的算子。
**为什么现有验收抓不到**：§7 没有任何一条变异构造环；「只有内核在创建时写 parent」是代码约定，
不是持久守卫，`spawn` 之外没有任何东西阻止将来某个 patch 或运维 SQL 造出环。
**建议**：两条 CTE 一律 `UNION`；再给向上 CTE 加 `WHERE up.depth <= MAX_WAVE_TREE_DEPTH + 1` 显式截断，
超过即按 §2.3 的 fail-closed 拒绝。测试：手工 INSERT 一个 2-环，断言创建被拒且**在 1s 内返回**。

### [BLOCKER] 投影 UPSERT 的**变更检测析取**没被设计提到；只改 `spawn` 的编辑会静默不生效
**攻击**：spec 把一个 pending task 块从（无 spawn）改成 `spawn: "sub-wave"`，**其它字段一字不改**。
实现者按设计 §1.2「更新臂带既有 `WHERE tasks.status='pending'`」照做，
在 `SET` 里加了 `spawn=excluded.spawn`——但 UPSERT 的 `WHERE` 后面还挂着一整串
`tasks.kind IS NOT excluded.kind OR tasks.goal IS NOT ... OR ...` 的**逐列变更检测**。
没有 `tasks.spawn IS NOT excluded.spawn` 这一项 ⇒ 整个析取为假 ⇒ UPDATE 命中 0 行 ⇒
列没写、`changed_keys` 不含该 key ⇒ 连 `PlanUpdated` 都不发。
随后 claim 冻结的是旧值 `'in-wave'`，路由走原地执行。**声明改了、执行没改、无任何信号。**
**证据**：`crates/calm-truth/src/db/sqlite/task_projection.rs:976-983`（`ON CONFLICT ... DO UPDATE SET ... WHERE
tasks.status='pending' AND (tasks.origin='block' OR ...) AND (tasks.kind IS NOT excluded.kind OR ... )`）；
设计 `docs/_985-s6-design.md:57` 只写了 status 守卫；
`docs/_985-s6-survey.md:54` 同样把守卫简化成「正是 `WHERE tasks.status='pending'`」——设计继承了清查的这处漏述。
**为什么现有验收抓不到**：§7 #1 的变异是「投影更新臂不写 `spawn`」，粗到必然红；
但只要 fixture 在改 spawn 的同时也改了 goal（最自然的写法），细变异下断言仍绿。
这正是「机制存在、测试会绿、性质为假」。
**建议**：§1.2 显式写出「必须同时进 INSERT 列清单、`DO UPDATE SET`、**以及变更检测析取**」；
§7 #1 改为「**只**改 `spawn`、其余字段逐字节不变」的 fixture，变异改为「只漏析取项」。

### [BLOCKER] 父任务进 `running` 后会被 **worker liveness 超时**杀掉，子 wave 却继续活着
**攻击**：`spawn:"sub-wave"` 与 `kind` 正交——task 块可以同时是 `kind: codex, spawn: sub-wave`
（设计从头到尾没有约束过 sub-wave 任务的 `kind`）。子 wave 建好后走 `reconcile_spawn_result → mark_running`，
`mark_running` **无条件**盖 `running_deadline_ms = now + task_run_timeout_ms`
（`crates/calm-server/src/scheduler/mod.rs:1052-1058`）。sweep 的 Running 臂按 `task.kind` 分支：
codex/claude ⇒ 过期即 `fail_running_liveness_timeout`（`scheduler/mod.rs:1271-1296`、`:277-279`）。
子 wave 的规划-派发-验收动辄跑几小时，超时后：**父任务 failed、子 wave 无人 teardown、
子 wave 的 spec 行永远计入树预算**（§4.1 的谓词按 `wave_id ∈ subtree` 计数，与父任务是否终结无关）
⇒ 树预算被幽灵占用，直到有人手工删 wave。
若换成 `kind: terminal`，走 `TaskStatus::Running if kind == Terminal ⇒ reconcile_running_terminal`
（`scheduler/mod.rs:1268-1270`），它按 `worker_card_id` 找 terminal 行找不到 ⇒ debug 日志 + 原地留行
（`scheduler/mod.rs:1654-1660`），**父任务永远 running**——正是 §5.2 要防的那个形状，只是换了入口。
**证据**：`scheduler/mod.rs:1045-1058`、`:1265-1298`、`:277-279`、`:1638-1660`；
设计 `docs/_985-s6-design.md:329-339`（§5.2 映射表只覆盖「子 wave 状态 ⇒ 父任务」，
完全没有「父任务自身的 sweep 臂会先动手」这一维）。
**为什么现有验收抓不到**：§7 #12/#13/#14 全部构造「子 wave 先到终态」，没有一条让
**时钟先走到 `task_run_timeout_ms`**；#14 的「绕总线直接改 DB」也只验 sweep 会闭合，不验 sweep 会先杀。
**建议**：(a) 明令 sub-wave 任务的 `kind` 语义（要么新增 `TaskKind::SubWave`，要么在
`task_kind_has_running_liveness_deadline` 前加 `task.spawn != 'sub-wave'` 的短路），
(b) §5 明写父任务在子 wave 创建后落在哪个 status，(c) §7 增：「父任务 running 超过
`task_run_timeout_ms` 且子 wave 仍非终态 ⇒ 父任务**不得**被 liveness 超时终结」，变异 = 去掉该短路。

### [BLOCKER] 「提交后启动子 wave harness」没有恢复臂，崩在这一步 ⇒ 子 wave 永久 inert、父任务永久 running
**攻击**：`child-wave` op 的 `prepare_tx` 提交成功（子 wave 骨架 + `child_wave_id` 已落库），
进程在提交后、`spec-harness-start` submit 之前崩溃。重启后：
`resume_dispatched → drive_spawn → submit`（`(kind,idem)` 命中已 **Succeeded** 的 op）→ `wait` 立即返回
→ `reconcile_spawn_result → mark_running`（0 行，已 running）⇒ **循环闭合，harness 永远不启动**。
子 wave 停在 `Draft`、无任何 session。唯一的兜底 reaper dead-root 扫描要求
「该 wave 的**最近一条** `spec-harness-start` op 处于 `phase='failed'`；**没有 start-op 行则信号非正 ⇒ 放过**」
（`crates/calm-server/src/db/../session_repo_impl.rs:176-195`）——本场景恰恰是**一条 start-op 都没有**，
所以 dead-root 扫描明确不收。父任务 `running` + 树预算被占，永久。
**证据**：设计 `docs/_985-s6-design.md:205-206`（「提交后启动子 wave 的 spec harness」，无恢复语句）；
`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:176-195`；
`docs/_985-s6-forks.md:29` 在内联方案下点了这个风险（「agent 可能永久 inert」），
但设计改选 operation 方案后把这句一起丢了——窗口并没有消失，只是换了位置。
**为什么现有验收抓不到**：§7 #5 只验「重复 submit ⇒ 同一 child」，不验 child **活着**。
没有任何一条断言子 wave 最终离开 `Draft`。
**建议**：把 spec-harness-start 的 submit 做成 `drive_spawn` 里**每次都跑**的幂等步骤
（照抄 `routes/today.rs:253-268` 的「事务外幂等 harness operation」形状），
或让 `child-wave` op 多一个 phase 负责它。§7 增：「op 成功后崩溃 ⇒ 重启后子 wave 仍会拿到 harness」。

---

## MAJOR

### [MAJOR] §7 #11「非树 wave 零行为变化」在变异下**仍然绿** —— 恒真断言
**攻击**：变异 = 让短路失效、总是跑树 CTE。对一个非树 wave，树项算出的是「它自己这棵单点树里
`declared_by='spec'` 的非终结行数」对 `tree_task_budget=32`。除非 fixture 恰好造满 32 行，
两项都过，**准入集合与诊断逐字节不变** ⇒ 断言全绿。唯一变的是执行了几条 SQL，
而设计没有给出任何观测语句数的装置。
**证据**：设计 `docs/_985-s6-design.md:296-298`（「这个短路本身要有正面测试（断言非树 wave 的行为逐字节不变），
否则『零成本』是一句没有载体的话」——载体恰恰仍然没有）；
`crates/calm-truth/src/db/sqlite/task_projection.rs:518-540`（现有 ceiling 项的形状）。
**为什么现有验收抓不到**：#11 是它自己。
**建议**：断言改成「非树 wave 的 `evaluate_schedulability` **不执行**任何 `waves` 递归查询」——
需要一个可计数的接缝（如 sqlx 语句计数或把树项抽成一个可注入的、测试里断言 0 次调用的函数）。
没有这个接缝就删掉 #11，别留一条恒真断言充数。

### [MAJOR] §4.2 的树项在「求不到根」时是 fail-open 还是 fail-closed，设计没说
**攻击**：§2.3 的「零行 ⇒ 拒绝」只写在**创建路径**（§3.2 步骤 1）。§4.2 复用同一对 CTE 做准入，
但没有任何一句规定「up-CTE 零行时树项如何取值」。实现者最自然的写法是
`if let Some(root) = ... else { /* 没有树，跳过树项 */ }` ⇒ **一条断链把整棵子树的预算变成无约束**。
这和 §2.1 自己列的 fail-open 形状（「孤儿指针 ⇒ 求根 CTE 静默截断 ⇒ 每棵子树各拿一份完整预算」）是同一个，
只是发生在第二个强制点。
**证据**：设计 `docs/_985-s6-design.md:168-170` vs `:280-302`（后者无对应语句）；`:131-133`（自己承认的形状）。
**为什么现有验收抓不到**：§7 #10 只验「树项存在」，没有断链构造。
**建议**：§4.2 显式写「up-CTE 零行 ⇒ 该 wave 的所有声明一律 **不可调度**，诊断码
`tree_root_unresolved`」；§7 增变异「把零行分支改成跳过树项」。

### [MAJOR] `tree_task_budget` 没有任何写入面，§12.1 的「校准装置」无法落地，测试只能绕生产路径造行
**攻击**：C.2 说该列「建 wave 时」写，但 `wave_create_tx` 是固定列清单、根本不命名它
（`crates/calm-truth/src/db/sqlite/wave.rs:47-63`，14 列，无新列），`WavePatch` 也不含它
（`crates/calm-truth/src/model.rs:147-184`，设计未提扩展）。于是生产上**每个 root 恒为 DEFAULT 32，
永远无法调**。§12.1 却写「常数 32 仍是猜的，校准装置是子 wave 创建拒绝率」——没有旋钮，校准无处可去。
连带后果更重：§7 #10 要构造「树预算耗尽」就必须造 32 条 spec 行，或**用裸 SQL 改列**——
后者正是「fixture 绕过生产创建路径」的形状。
**证据**：`wave.rs:47-63`；设计 `docs/_985-s6-design.md:129`、`:475`。
**建议**：要么给 `NewWave`/`WavePatch` 加该字段并进 §9 的 C.2 四格（写者从「建 wave 时」变成真的存在），
要么把 §12.1 那句改成「本片不可校准，旋钮登记后推」，别留一个指向不存在装置的承诺。

### [MAJOR] 子 wave 被 user **reopen** 回 Planning：父任务已终结，闭合是一次性的，此后永久失联
**攻击**：子 wave `Done` ⇒ 父任务 `done`（或 gate 通过后 `done`）。用户随后 reopen 子 wave
（终态 → `Planning`，user-only，合法：`crates/calm-types/src/wave_lifecycle.rs:241-252`）。
子 wave 重新活起来、继续产 spec 行、继续吃树预算；父任务是终态，
§5.3 的 sweep 臂只枚举 `status IN ('dispatched','running')` ⇒ **永远不再看这一行**。
子 wave 之后 `Failed` 也不会回写。反向同理：`Failed→父 failed→reopen→最终 Done`，父任务停在 failed。
**证据**：`crates/calm-types/src/wave_lifecycle.rs:236-253`；设计 `docs/_985-s6-design.md:333-339`
（§5.2 的表只有五行，无 reopen 行）、`:356-358`（sweep 谓词）。
**为什么现有验收抓不到**：§7 #12/#13 都是「一次终态 ⇒ 一次映射」，没有第二次转移。
**建议**：§5.2 加一行「子 wave 从终态 reopen ⇒ 父任务已终结 ⇒ **no-op，并在读时回显里标注
`childWaveId` 指向的 wave 已重开**」，或明确禁止 reopen 一个被 `tasks.child_wave_id` 引用的 wave
（唯一索引已经让这个反查成立）。至少写进 §12.1，别留空白。

### [MAJOR] `waves.parent_wave_id` 无索引 —— 短路条件与子树 CTE 都是全表扫，「零成本」不成立
**攻击**：§4.2 的短路是 `NOT EXISTS (SELECT 1 FROM waves WHERE parent_wave_id = <this>)`。
SQLite 的 FK **不为子列自动建索引**（只要求父列唯一）。设计 §2.1 只声明了列 + CHECK，没声明索引。
于是每次 `evaluate_schedulability` 都全扫 `waves`——而它不只跑在写路径，
**读路径每次打开报告页都跑一次**（`task_projection.rs:518-524` 的 doc 明写读路径以 autocommit 连接调用它，
`read.rs::task_diagnostics`）。§4.2 断言的「对既有报告写路径零新增查询」在存量 100% 非树时也是假的：
短路条件本身就是那条新增查询。
**证据**：设计 `docs/_985-s6-design.md:128`（列声明，无索引）、`:293-296`；
`crates/calm-truth/src/db/sqlite/task_projection.rs:459-464, 518-525`。
**建议**：migration 加 `CREATE INDEX ... ON waves(parent_wave_id) WHERE parent_wave_id IS NOT NULL`；
把 §4.2 的措辞从「零新增查询」改成「一条走索引的 NOT EXISTS」。

### [MAJOR] 树项让 task 投影依赖**兄弟 wave 的行**，rebuild 不再是本 wave 文档的纯函数
**攻击**：`evaluate_schedulability` 今天的输入全部来自本 wave（`wave_projection_state` 的单条 SQL
`WHERE t.wave_id = w.id`）。加上树项后，wave C 的准入结果取决于同树的 wave D 当前有多少非终结行。
rebuild / replay 逐 wave 重放时，**先 rebuild C 还是先 rebuild D 会产出不同的准入集合**。
C.2 却把 `parent_wave_id` 的 rebuild 格写成「结构真源，非文档函数」，没有回答投影的 rebuild 语义。
**证据**：`crates/calm-truth/src/db/sqlite/task_projection.rs:408-440`（现有输入范围）；
设计 `docs/_985-s6-design.md:128-129`（C.2 四格）、`:280-302`（树项）。
**建议**：§4.2 补一段 rebuild 语义：要么 rebuild 时跳过树项（并说明为什么不破坏 D.4 #7），
要么规定整棵树必须按固定序（root-first, DFS）整体 rebuild。§7 增一条「同一棵树两种 rebuild 序 ⇒ 同一结果」。

### [MAJOR] 父 wave 被 cancel 时子 wave 无人管
**攻击**：用户 cancel 父 wave（非终态 → Canceled 合法，`wave_lifecycle.rs:222-233`）。
父 wave 的 task 行不会被终结（cancel 只改 wave lifecycle），子 wave 继续跑、继续吃树预算；
父 wave 又因为「有 descendant」而**删不掉**（§5.4）。用户拿不到任何收场路径。
**证据**：`crates/calm-types/src/wave_lifecycle.rs:219-233`；设计 `docs/_985-s6-design.md:369`。
**建议**：§5.2 增一行「父 wave 进终态 ⇒ 其 sub-wave 父任务的子 wave 如何处理」，
最小实现是登记进 §12.1 并在 UI 拒删时给出「先处理子 wave `<id>`」的可执行提示。

---

## MINOR

- **[MINOR] §6 的 `BlockVerdict` 字段清单不全。** 设计写 `BlockVerdict{status, gate_result, worker_card_id}`
  （`docs/_985-s6-design.md:382`），实际还有 `withdrawal`（`task_projection.rs:936`），
  且承载 SQL 取的是 7 个字段（`task_projection.rs:421-428`）。加 `childWaveId` 的连带面比设计列的宽。
- **[MINOR] `spawn` 在 manifest plan template 里有一条 set-equality 元测试挡着。**
  `crates/calm-server/src/mcp_server/tools/plan.rs:918-933` 断言 `spawn ∈ template_exclusions`，
  即 workflow manifest **不得**声明 sub-wave。这是本片让 `spawn` 通电后的一条真实策略约束，
  §9 的文档修订清单没有登记它。
- **[MINOR] §7 #4 的变异不可实施。** 「让它落 NULL」在 `NOT NULL DEFAULT 'in-wave'` 下是约束错误而非错值；
  可实施的变异只有空串或写死默认值。措辞需改，否则「变异已验证」是假的。
- **[MINOR] §5.2 没规定 `SuccessReportFlip::None` 怎么办**（`crates/calm-truth/src/db/sqlite/task.rs:528-542`
  三种情况返回 `None`）：闭合是重试型 sweep，必须说明是「已被别人处理」还是「下次重试」。
- **[MINOR] 命名漂移。** §2.3 叫 `parent_depth`，`docs/_985-s6-forks.md:79` 叫 `current_depth`；会被再 +1 一次。

---

## 我抽查了前置清查的哪些行号，哪些对不上

| # | 被抽查断言 | 实测 | 判定 |
|---|---|---|---|
| 1 | `_985-s6-survey.md:54`「投影 INSERT/UPSERT 在 `task_projection.rs:977-985`，更新守卫**正是** `WHERE tasks.status='pending'`」 | SQL 确在 `:976-983`，但完整守卫是 `status='pending' AND (origin='block' OR origin='legacy') AND (`逐列变更检测析取`)` | **对不上（漏述）**。设计 `:57` 原样继承 ⇒ 直接导致 BLOCKER-3 |
| 2 | 设计 `:155-156`「`WITH RECURSIVE` 已有生产先例：`gc.rs:266-283`」 | CTE 实体在 `gc.rs:271-280`，行号范围成立；但先例用 **`UNION`**，设计两条 CTE 用 `UNION ALL` | **行号对、结论被反用** ⇒ BLOCKER-2 |
| 3 | 设计 `:145-146` / `_985-s6-forks.md:69`「`wave_create_tx` 的 INSERT 是固定列清单（`wave.rs:47-63`）」 | `wave.rs:47-63` 精确命中，14 列，确无新列 | **对得上** |
| 4 | `_985-s6-fork5.md:7-8`「终态可由 user reopen 到 Planning，见 `wave_lifecycle.rs:182-307`」 | `validate_transition` 恰好 182-307；reopen 分支 241-252，user-only | **对得上**，但设计 §5.2 没消费这条 ⇒ MAJOR-4 |
| 5 | `_985-s6-fork5.md:32` /设计 `:365-366`「`wave_delete_tx` 显式删 …，cards 由 FK cascade（`wave.rs:205-252`）」 | `wave_delete_tx` 在 205-253，逐条属实 | **对得上**，但两份文档都没推演 **cove** 删除的级联 ⇒ BLOCKER-1 |
| 6 | 设计 `:41`「根哈希排除集已含 `spawn`（`task_context.rs:36-55`）」 | 纳入集 36-45、排除集 47-55，`spawn` 在排除集第 4 项 | **对得上** |
| 7 | 设计 `:36-37`「写入校验已接受 `in-wave\|sub-wave`（`kinds.rs:252-255`）」 | 精确命中 252-256 | **对得上** |
| 8 | 设计 `:337`「既有 canceled 只允许 `pending→canceled`（`task.rs:158-171`）」 | `task_cancel_tx` 在 161-172（doc 注释起于 158），守卫 `status='pending'` 属实 | **对得上** |
| 9 | 设计 `:380-381`「`attach_task_read_state` 在 `wave_projection_state` 的单条 SELECT 里附加状态列（`task_projection.rs:154-168, 413-455`）」 | SQL 在 408-440，`task_read_state_json` 子查询在 421-428；`:413-455` 涵盖 | **对得上**，字段清单不全见 MINOR-1 |
| 10 | 设计 `:134`「`worker_card_id` 今天是裸 TEXT（`0058:18-24`）」 | `worker_card_id TEXT NULL` 在 `0058_tasks_kind_claude.sql:24` | **对得上** |

结论：三份清查的**行号基本可信**（10 条抽查中 9 条精确），
唯一实质性偏差是 #1 的**守卫漏述**，而它恰好是本片最容易静默失败的那个写点。
#2 属于「行号对但把先例抄反了」，是设计层的错，不是清查的错。

---

## 本片应该拆吗

**能拆，而且 §0 的「找不到自洽切分线」是错的。** §0 的论证是
「『创建而不闭合』违反 D.3；『预算而无子 wave』是纯死代码」——这两条都成立，
但它只考察了「沿**载体**切」，没考察「沿**约束**切」。

**切分线：把树的两个上界拆开，深度上限留 PR-A，树预算整体挪 PR-B。**

- **PR-A（~900 行）**：`tasks.spawn` 冻结列 + `child-wave` operation + `waves.parent_wave_id`（含 FK/索引/CHECK）
  + **深度上限 `MAX_WAVE_TREE_DEPTH=3`** + `tasks.child_wave_id` + 父任务闭合（live + sweep）+ 读时回显。
  自洽性：创建**有**闭合（不违反 D.3）；树**有**上界——深度 3 是一个真实、可判定、单点强制的界，
  不是死代码。D.4 #7 在 PR-A 里明确登记为「尚未成立，PR-B 交付」，而不是留一个 32 倍失真的假不变量。
- **PR-B（~400–500 行）**：`waves.tree_task_budget`（含写入面）+ 强制点一 + 强制点二 + 短路 + 诊断码。

**为什么这条线比不拆好**：本评审的 7 条 MAJOR 里有 4 条（M1 恒真断言 / M2 fail-open / M5 索引与读路径成本 /
M8 rebuild 序依赖）**全部长在树预算上**，而 BLOCKER 里有 3 条（B1/B4/B5）长在创建-闭合上。
这是两簇互不相干的风险，混在一片里意味着任何一簇返工都要重跑另一簇的全部门禁。
拆开后 PR-B 是一个纯准入-谓词 PR，评审面小到可以逐条对 §7 #9/#10/#11 做真变异验证——
而这三条恰恰是现在最可能空转的三条。

**唯一的代价**：PR-A 落地后到 PR-B 合并前，树内 spec 非终结行的真实上界是
`Σ per-wave ceiling ≈ 32 × (1+3 层扇出)`。这个数必须**写进 PR-A 的 §12.1 并写进 D.4 #7 的旁注**，
不能省——省了就等于 §0 批判的那种「名义不变量为假」。设计已经证明自己知道怎么写这句话（§4.3），
把它提前一片写而已。
