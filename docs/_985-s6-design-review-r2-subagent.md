# #985 切片 6 设计增量 v2 · 对抗性评审 r2（subagent 通道）

> 对象：`docs/_985-s6-design.md`（v2，PR-A 为重点）。基线 `/tmp/wt6b` @ origin/main `9d30006a`。
> 只读；未跑 cargo。**跑了 sqlite3 做算子实证**（见 BLOCKER-1，可复现的机器证据，不是推理）。
> 结论：**NO，不可施工。** 4 BLOCKER / 9 MAJOR / 5 MINOR。v2 修好了 r1 的多数条目，
> 但**三条最重的修法本身制造了同形状的新空洞**，且都在 v2 新写的段落里（§2.2 的 `UNION`、§3.3 的幂等 harness、§4 的短路）。

---

## BLOCKER

### [BLOCKER-1] `UNION` 根本不是终止的原因；§7 #7 在它自己规定的变异下**必绿**，整条 BLOCKER-B2 的修法归因错误
**攻击**：手工 INSERT `A→B→A`，跑 §2.2 那段 SQL 原文，再把 `UNION` 改回 `UNION ALL`，两次都在毫秒内返回同样的行数。
**证据（实测，非推理）**：`sqlite3 :memory:`，`waves(A→B, B→A)`，跑 §2.2 原文 CTE ——
`UNION` → 6 行立即返回；`UNION ALL` → **6 行立即返回**（变异下结果逐字节相同）；
`UNION` 但删掉 `WHERE up.depth <= ?2` → 10 秒未返回（timeout 124）。
去重是**按整行**去重的，而 CTE 携带 `depth` 列（`docs/_985-s6-design.md:214-220`）：`(A,B,0)` 与 `(A,B,2)` 是不同的行，
`UNION` 一行都去不掉。**唯一的终止装置是 `WHERE up.depth <= ?2`。**
v2 引以为据的生产先例 `crates/calm-truth/src/wave_vcs/gc.rs:271-279` 之所以能靠 `UNION` 终止，
是因为它的 CTE **只有 `hash` 一列**、没有 depth ——「行号对、结论被反用」在 r1 抓过一次，v2 换了个方向再犯一次。
**为什么 v2 的验收抓不到**：§7 #7（`:465`）的变异就是「`UNION` 改回 `UNION ALL`」，断言是「拒绝且 1s 内返回」。
按上表，变异后依然拒绝、依然 1s 内返回 ⇒ **绿**。设计说「后半句是这条修正的全部价值」，而那半句测的是一个不存在的因果。
**建议**：(a) §2.2 把终止性归因改写为 depth 截断（`UNION` 只作无害去重保留）；(b) §7 #7 的变异改成
「删掉 `WHERE up.depth <= ?2`」（实测会挂）；(c) 加静态门禁：`waves` 上携带非 id 列的 `WITH RECURSIVE` 必须带显式 depth 上限。

### [BLOCKER-2] §4 的短路放在错误的层：`kind: terminal, spawn: sub-wave` 直接掉进 terminal 臂，父任务永久 running；#11 的 fixture 必然用 codex ⇒ 绿
**攻击**：父 task 块写 `kind: terminal, spawn: sub-wave`（§1.6 明确承认两者今天正交，且**把裁决推给实现方**）。
子 wave 建成、父任务 running。sweep 的 match 顺序是：
`TaskStatus::Running if task.kind == TaskKind::Terminal` (`crates/calm-server/src/scheduler/mod.rs:1268`) **先于**
`TaskStatus::Running if task_kind_has_running_liveness_deadline(...)` (`:1271`)。
于是 sub-wave 行进 `reconcile_running_terminal` → `worker_card_id` 为 None（§4 裁决 1 明令不 stamp）→
回落 `find_by_kind_and_idempotency("terminal-worker", task.id)` → op kind 是 `child-wave` ⇒ None ⇒
「no resolvable worker card; leaving row」(`:1640-1651`) ⇒ **父任务永远 running、子 wave 无人闭合、树预算永久被占**。
**证据**：`scheduler/mod.rs:1268-1271`、`:1638-1660`；设计 `docs/_985-s6-design.md:345-347`（短路只说「在
`task_kind_has_running_liveness_deadline` 之前」，那个位置在 terminal 臂**之后**）、`:141-144`（§1.6 把 kind 裁决后推）。
**为什么 v2 的验收抓不到**：§7 #11（`:469`）只要求「父任务 running 超 `task_run_timeout_ms` 且子 wave 非终态 ⇒ 不得被终结」，
变异是「去掉 liveness 短路」。terminal 臂根本不看 deadline，所以无论 fixture 用 codex 还是 terminal，
#11 的断言「不得被终结」在 terminal 下**恒真**（它压根不会被终结，只是永远 running）。#11 断言错了方向。
**建议**：(a) §1.6 现在就拍板；(b) 短路写成 sweep Running 分派的**第一个**臂
（`TaskStatus::Running if task.spawn == "sub-wave"`），显式排在 terminal 臂之前；
(c) #11 拆两条：「不得被 liveness 终结」+「**必须**在子 wave 终态后离开 running」，
后者的变异 = 把 sub-wave 臂挪到 terminal 臂之后，fixture **同时覆盖 kind∈{codex, terminal}**。

### [BLOCKER-3] §3.3 的「每次都跑的幂等 harness 步骤」没有关闭窗口，只把它挪到了 `running` flip 之后
**攻击**：`drive_spawn` 只有一个入口能重跑——sweep 的 `TaskStatus::Dispatched => resume_dispatched`
（`scheduler/mod.rs:1264-1265`、`:1596-1630`）。一旦 §4 裁决 1 的专用 `dispatched → running` 写落库，
**`drive_spawn` 此生不再被调用**。而现有代码顺序是 `submit → wait → reconcile_spawn_result(mark_running)`
（`:955-988`）。实现者把 harness submit 放在 `reconcile` 之后（最自然的位置，"提交后启动"），
进程在 running flip 与 harness submit 之间崩溃 ⇒ 子 wave 停在 `Draft`、无 session、
reaper dead-root 因「没有 start-op 行则放过」不收（`session_repo_impl.rs:176-195`）⇒ **BLOCKER-B5 原样复活**。
**证据**：设计 `:284-287`（只说「每次都跑的幂等步骤」，**没有任何顺序约束**）；`:343-344`（专用 running 写）；
`scheduler/mod.rs:940-989, 1264-1265, 1596-1630`。
**为什么 v2 的验收抓不到**：§7 #19（`:477`）的崩溃注入点只写「op 成功后崩溃」。
实现者会把注入点选在 `mark_running` 之前（那时 `resume_dispatched` 确实会补），测试绿；真实窗口在 flip 之后。
**建议**：§3.3 必须明写「**harness submit 严格早于 `dispatched → running` 的 flip**；running flip 是这条幂等步骤的唯一终止条件」，
并在代码注释里锁死。#19 的崩溃注入点**枚举两个**：(a) op 成功后 / harness submit 前，(b) harness submit 后 / running flip 前，
两处都必须重启后子 wave 离开 `Draft`。另加一条负面断言：running flip 之后不存在任何「子 wave 仍在 Draft」的可达状态。

### [BLOCKER-4]（回答 §11 第 2 问）自 FK CASCADE 的**第三条路径**存在：`Repo::wave_delete` 绕过路由层拒删守卫，静默删掉整棵子树
**攻击**：§5.4 的「有 descendant ⇒ Conflict」没有指定层级。`wave_delete_tx` 有两个调用面：
路由 `routes/waves.rs:1418`，以及 **trait 方法 `Repo::wave_delete`**（`crates/calm-server/src/db/mod.rs:554,598-599`
→ `calm-truth/src/db/mod.rs:844` → `session_repo_impl.rs:316`）。守卫若加在路由 handler，
任何走 `repo.wave_delete(id)` 的路径（今天是测试 harness 与 admin/replay 面，明天是任何新 handler）
都会命中 `DELETE FROM waves`（`wave.rs:242`）→ 自 FK `ON DELETE CASCADE` **逐行静默删掉所有 descendant**：
无逐 child `WaveDeleted` 事件、无进程 teardown（terminal / spec harness / workspace lease）、
其它 wave 里的父任务 `tasks.child_wave_id` 变悬空指针（`tasks` 对 `waves` 无 FK，`cove.rs:161` 明说）。
这比 r1 的 NO ACTION 崩溃更坏：**NO ACTION 是响亮的红，CASCADE 是安静的数据丢失。**
**证据**：`db/mod.rs:554,598`、`calm-truth/src/db/sqlite/wave.rs:205,242`、`routes/waves.rs:1418`、
`db/sqlite/cove.rs:161`；设计 `:190-192, 420-423`（全篇只谈「单删父 wave」，未指定守卫层级）。
**为什么 v2 的验收抓不到**：§7 #20（`:478`）的变异是「去掉检查」，测试走路由 ⇒ 守卫放在路由层照样绿。
#21（`:479`）只覆盖 cove 删除。
**建议**：拒删守卫下沉进 **`wave_delete_tx` 本体**（第一条 DB 副作用之前，返回 Conflict），路由只负责错误映射。
#20 增一条：**直接调 `Repo::wave_delete`** 也必须 Conflict；变异 = 把守卫留在路由层。

---

## MAJOR

### [MAJOR-1] 「禁止 reopen 被引用的 wave」没有指定层级，且 reopen 写点有 5 个
`validate_transition` 的 reopen 分支是 user-only（`calm-types/src/wave_lifecycle.rs:236-252`），但落盘写点有：
`routes/waves.rs:1189`、`wave_lifecycle.rs:47-82`（spec 请求）、`:90-129`（内核自动）、
`mcp_server/tools/plan.rs:565`、`bin/replay.rs:452`。设计 `:386-389` 只说「返回 Conflict 并指名父任务 key」，
没说守卫在哪一层。§7 #17（`:475`）的变异「去掉该守卫」在路由层实现下对其它入口全绿。
**建议**：守卫放进 `wave_update_tx` 的 lifecycle 分支（终态→`Planning` 时查 `tasks.child_wave_id` 唯一索引），
#17 增「经 `apply_requested_transition_in_tx` / MCP 路径同样 Conflict」。

### [MAJOR-2] `waves.parent_wave_id` **没有写入载体**；照最省事的路走会让它变成客户端可设，直接绕过深度检查
`wave_create_tx` 是固定 14 列 INSERT（`calm-truth/src/db/sqlite/wave.rs:47-63`），入参是 `NewWave` ——
而 `NewWave` 是 `POST /api/waves` 的 body 且带 `#[schema]`（`calm-truth/src/model.rs:93-118`）。
把 `parent_wave_id` 加进 `NewWave` = **把一个 server-owned 结构指针开放成客户端字段**，
可以直接造深度 10 的树或 `A→B→A` 环（内核的深度检查只在 `child-wave` op 里）。
`purpose` 这个同性质字段的既有先例是**另起一条 raw INSERT**（`routes/today.rs:89`，11 列，绕开 `wave_create_tx`），
而 `wave_create_tx:80` 把 `purpose` 硬写 `None`。设计 §2.1 只写「谁写：只有内核」，§3.5 只给继承矩阵，**没有载体**。
**建议**：显式裁决载体（内核专用 create fn 或 `NewWave` 之外的第二参），并加负面断言：
`POST /api/waves` 带 `parentWaveId` 必须 400/被忽略且 DB 列为 NULL；变异 = 把字段加进 `NewWave`。

### [MAJOR-3] §4 裁决 1 的 `running_deadline_ms = NULL` 会被 sweep **主动回填**
`stamp_missing_running_liveness_deadline`（`scheduler/mod.rs:1318-1341`）对
`kind ∈ {Codex, Claude} && status = Running && running_deadline_ms IS NULL` 的行**补写** `now + task_run_timeout_ms`。
它只吃 `kind`，拿不到 `spawn`。设计 `:345-346` 说短路加「在 `task_kind_has_running_liveness_deadline` 之前」，
但那个函数签名是 `fn(TaskKind) -> bool`（`:277-279`），**承载不了 spawn**。
共 3 个站点需要按 `(spawn, kind)` 判：`:1271` 匹配臂守卫、`:1290` 复核、`:1319` 回填函数。
**建议**：§4 点名这三处；#11 的变异改为「只改其中一处」（单点变异才有判别力）。

### [MAJOR-4] §7 #5 与 §3.2 步骤 0 互相抵消：那条 fixture 证不了它宣称的性质
#5（`:463`）要求「第一次 op insert 后**编辑纳入哈希的字段**，再走真实 `resume_dispatched`，断言 payload hash/child id 保持冻结值」。
但编辑纳入哈希的字段会置 `context_stale_at_ms`，而 §3.2 步骤 0 的 `refuse_if_context_stale`
（`operation/mod.rs:78-95`）会让 `prepare_tx` 直接 Conflict。两种情况：
(a) op 已 `Succeeded` ⇒ `prepare_tx` 根本不重跑 ⇒ 断言**恒真**，任何变异都绿；
(b) op 未 succeeded ⇒ op 失败、父任务 `failed('spawn-failed')`（`scheduler/mod.rs:967-983`）⇒ 断言的对象不存在。
**建议**：拆成两条独立验收——(a) 「编辑纳入哈希字段 + resume ⇒ **fail-closed 拒绝**」；
(b) 「冻结值来源」用 §1.4 已经写好的 seam（同一 IMMEDIATE 事务内改 pending 行、不动 `doc_rev`、不置 stale）证。

### [MAJOR-5] `refuse_if_context_stale` 是**每个 adapter 自己调**的，没有中心强制点，#10 只买了一个 kind
四个 task-bound adapter 各自在 `prepare_tx` 里手写一行（`codex_adapter/mod.rs:773`、`claude_adapter/mod.rs:775`、
`terminal_adapter.rs:583`、`task_verify_adapter.rs:634`），`TASK_BOUND_ADAPTER_KINDS` 只是一个 `[&str; 4]` 常量
（`operation/mod.rs:56-61`）。**名单成员资格与实际调用之间没有任何机器联系。**
设计把「判决强制点从四个变五个」当成核心不变量（`:251-253`），却仍然只给 `child-wave` 加一条测试。
**建议**：加表驱动元测试——遍历 `TASK_BOUND_ADAPTER_KINDS`，每个 kind 构造 stale task 后**驱动真实 adapter**、断言零副作用；变异 = 从任一 adapter 删掉那一行。

### [MAJOR-6] 深度 ≥2 的树在测试里没有生产构造路径，#6 必然靠裸 INSERT
造深度 2 需要「子 wave 的 spec harness 声明一个 `spawn:sub-wave` 任务并被 claim」——完整 agent 回路。
现实里 #6（`:464`）的 0/3/4 三点只能裸 INSERT `waves(parent_wave_id)`，
那正是 §7.1 自己列的「fixture 绕过唯一生产接线」（`:485`）。
后果：`child-wave` op **写错 `parent_wave_id`**（例如写成 root 而不是直接父）在 #6 下全绿。
**建议**：至少一条用例通过**真实 `child-wave` adapter** 造出深度 2，并断言 `parent_wave_id` 等于直接父而非 root；
深度 3/4 的边界再用裸 INSERT 补。

### [MAJOR-7] 悬空 `childWaveId` 的读时语义未定
§5.2（`:376`）明令「行被删 ⇒ 不得清成 NULL」，§6 又把 `childWaveId` 回显进 `BlockVerdict`
（`task_projection.rs:421-428` 的 7 字段子查询 → `:936` DTO → `web/src/pages/report-blocks/task.tsx:119-131`）。
于是稳态下会存在**指向已删 wave 的可点链接**。设计没说 DTO 是否带「已删除」标志，也没说前端渲染什么。
**建议**：DTO 增一个由同一条 SQL 派生的 `childWaveExists`（`EXISTS(SELECT 1 FROM waves ...)`，
#1016 要求并进同一条 SELECT），前端渲染成不可点的 tombstone；#14 增回显断言。

### [MAJOR-8] PR-B 定型留了与 BLOCKER-1 **同形状**的坑
§8（`:498`）对向下 CTE 只规定「`UNION`，不是 `UNION ALL`」——和 §2.2 同一个错。若向下 CTE 携带任何非 id 列
（计数常需 `wave_id`+`status`），环上同样不终止；即使只选 `id` 也应写死上限。不改则 PR-B 原样复发刚修好的 BLOCKER。
**建议**：§8 改写为「只选 `id`，且必须带 `depth <= MAX_WAVE_TREE_DEPTH` 截断」，并共用 PR-A 的静态门禁。

### [MAJOR-9] §7 #21 自己承认「可能间歇」——间歇性红不是门禁
`:479` 写「必红（可能间歇 —— 用多次重复或强制删除序）」。一条概率性红的测试不能作为 CASCADE 的守卫。
**建议**：改成确定性结构断言 `PRAGMA foreign_key_list(waves)` 的 `on_delete = 'CASCADE'`（变异 = 改回 `NO ACTION` ⇒ 立即红），
再加一条行为断言「删 cove 后 `SELECT count(*) FROM waves WHERE cove_id=?` = 0」。

---

## MINOR

1. **migration 形状实测可行，但设计必须写出来**：`ALTER TABLE waves ADD COLUMN parent_wave_id TEXT NULL
   REFERENCES waves(id) ON DELETE CASCADE CHECK(parent_wave_id IS NULL OR parent_wave_id <> id)`
   在 `foreign_keys=ON` 且在事务内**合法**（实测通过，CHECK 与 CASCADE 都生效）——前提是默认值为 NULL
   （SQLite 对带 `REFERENCES` 的 ADD COLUMN 的硬性要求）。**不需要 `0058_` 式整表重建。**
   设计不写这一句，实现者会照 `0058_` 先例重建 `waves` 表（连带 14 列 + 6 个索引 + `idx_waves_one_launchpad` 部分唯一索引）。
2. **`parent_wave_id` 不应进 `Wave`/`WaveRow`**：`WaveRow` 的 14 列显式 SELECT 有 8 处（`wave_lifecycle.rs:141`、
   `wave.rs:95`、`read.rs:123,134,157`、`today.rs:72,78`、`snapshot.rs:41,269`），sqlx 运行时才炸；
   `spec_task_ceiling` 的先例已证明 waves 列可以不进 `WaveRow`。设计要显式写「不进」。
3. **UPSERT 的第四处**：`task_projection.rs:977` 的 SQL 里 `?15` 被 `created_at_ms, updated_at_ms` **复用两次**；
   加 `spawn` 时占位符编号极易错，且 sqlx 不做编译期校验。§1.2 的「三处」应补一句绑定顺序警告。
4. **§7 #3c 不是一条可执行验收**：「把 `claim_task` 改回返回 pre-claim `Task` ⇒ 编译失败」——
   CI 不会跑一个「应当编译失败」的变体。它是好的结构性修法，但登记为验收会让「23 条已验证」失真。改成设计约束条目。
5. **§7 #22 的「结构断言」需指定装置**：若断言写成「`BlockVerdict` 结构体无 `spawn` 字段」，那是编译期事实、恒真；
   要能红，必须断言**序列化后的 DTO JSON 不含 `spawn` key**。

---

## r1 的哪几条 v2 没修好 / 修出了新洞

| r1 编号 | v2 处置 | 裁决 |
|---|---|---|
| codex-B1 stale fence | §3.2 步骤 0 + #10 | **修了方向，留两个新洞**：MAJOR-5（无中心强制/元测试）、MAJOR-4（#5 与它自相矛盾）|
| codex-B2 / sub-B4 worker deadline | §4 三条裁决 | **未修完**：BLOCKER-2（terminal 臂在前）、MAJOR-3（回填站点）|
| codex-B3 pending 自计数 | 整体挪 PR-B（§8 BLOCKER-A3）| 接受 |
| codex-B4 / sub-M4 reopen 非收敛 | §5.2「禁止 reopen 被引用的 wave」| **修了语义，层级未定**：MAJOR-1 |
| codex-M1 gate 事件 | §5.3 eventized 事务 + #13 带 gate | **修好了** |
| codex-M2 / sub-B2 CTE 环 | §2.2 换 `UNION` + depth 截断 | **归因错误、验收失效**：BLOCKER-1（实测） |
| codex-M3 / sub-M1 #11 恒真 | 挪 PR-B（M-A3/M-B1）| 接受 |
| codex-M4 #5 payload 来源 | §1.5 修法 | **新洞**：MAJOR-4 |
| codex-M5 前置条件 3 | §1.4 收窄 `claim_task` API + seam | **修法好**，但 #3c 不可执行（MINOR-4）|
| sub-B1 cove cascade | §2.1 改 `ON DELETE CASCADE` | **修了主路径，开了新路径**：BLOCKER-4 |
| sub-B3 变更检测析取 | §1.2 三段守卫 | **修好了**（占位符编号见 MINOR-3）|
| sub-B5 harness 恢复臂 | §3.3 幂等步骤 | **未修完**：BLOCKER-3（顺序未约束）|
| sub-M2/M3/M5/M6/M7 + MINOR 1–5 | 挪 PR-B / §5.4 / §6 / §1.1 | 接受 |

## §7 的 23 条里，哪几条在我设计的变异下仍然绿

| # | 为什么绿 |
|---|---|
| **#5** | op 已 Succeeded ⇒ `prepare_tx` 不重跑 ⇒ 恒真（MAJOR-4）|
| **#6** | fixture 裸 INSERT ⇒ 「`child-wave` 写错 parent 指针」不可见（MAJOR-6）|
| **#7** | `UNION`↔`UNION ALL` 实测结果相同 ⇒ 变异下逐字节相同（BLOCKER-1）|
| **#10** | 只覆盖一个 kind；「adapter 里删掉那一行」对其余 4 个 kind 全绿（MAJOR-5）|
| **#11** | `kind: terminal` 下「不得被终结」恒真（BLOCKER-2）；单点变异 3 选 2 仍绿（MAJOR-3）|
| **#17** | 守卫放路由层 ⇒ MCP / `apply_requested_transition_in_tx` 入口全绿（MAJOR-1）|
| **#19** | 崩溃注入点选在 running flip 之前 ⇒ 绿（BLOCKER-3）|
| **#20** | 守卫放路由层 ⇒ `Repo::wave_delete` 直调全绿（BLOCKER-4）|
| **#21** | 设计自承「可能间歇」⇒ 不是门禁（MAJOR-9）|
| **#22** | 若写成结构体字段断言则是编译期恒真（MINOR-5）|
| **#3c** | 不是可执行验收（MINOR-4）|

即 23 条里 **11 条**不具判别力。其余我未证伪；#1、#8、#13、#15、#18 是本轮质量最高的五条。

## 可以施工了吗

**NO。**

最小阻塞集（改完这 7 条即可施工，其余可随 PR 一并落）：
**B1** §2.2 归因改写 + #7 变异换成删 depth 截断 + 静态门禁；
**B2** §1.6 现在拍板 `kind`/`spawn`，sub-wave 臂显式排在 terminal 臂之前，#11 拆两条并覆盖两种 kind；
**B3** §3.3 写死「harness submit 严格早于 running flip」，#19 枚举两个崩溃注入点；
**B4** 拒删守卫下沉进 `wave_delete_tx`，#20 增 `Repo::wave_delete` 直调用例；
**M1** reopen 守卫下沉进 `wave_update_tx`，#17 覆盖非路由入口；
**M2** 裁决 `parent_wave_id` 的写入载体 +「REST 不可设」负面断言；
**M3** 点名 3 个 liveness 站点，#11 用单点变异。

**§11 逐问**：(1) 拆片线**站得住**，§0.1 的代价登记也写全了。
(2) 第三条路径**存在**——`Repo::wave_delete`（BLOCKER-4），失败模式是静默数据丢失，比 r1 的崩溃更坏。
(3)「禁止 reopen」的语义对（确实消灭整个交错族），但层级未定（MAJOR-1）；我没找到更小且仍收敛的裁决。
(4) §1.6 **是**在把设计决定推给实现方，且这个决定直接决定 BLOCKER-2 是否可达——必须现在拍板。
(5) 见上表：11 条。
