# 文档即计划：`task` 块为声明真源，plan 表降为投影（issue #985）

本文回答四个问题：**声明该住在哪**（判据 + 声明/状态分栏）、**`task`
块长什么样**（kind 归属、payload、身份、写口）、**投影与冲突怎么办**
（同事务投影、引用闭包 stale、三级阶梯）、**怎么切、怎么验**（切片 +
可证伪断言）。

伴生文档：`docs/architecture/644-plan-then-schedule.md`（`tasks` 表 /
scheduler / gate 的现状事实源）、`docs/architecture/955-kernel-app-boundary.md`
（§1.1 判据、§4 单写者原则）、`docs/architecture/653-parked-operations.md`
（parked 原语的真实能力边界）、`docs/architecture/terminology-glossary.md`。

> **修订状态：r1 双通道评审已折叠（31 条发现，接受 27 / 驳回 4，逐条见 §14）。**
> r1 有四处修订**改变了设计方向**，不是加注解：
> ① `declared_by` 从投影列**迁进块 payload**（否则 rebuild 重建不出，§8 树预算失守，§4.4）；
> ② 冻结集从 `(block_id, rev)` 改为 **`(wave_id, block_id, rev, content_hash)`**
> （块 id 会回收、rev 是饱和加，两个独立反例都能静默漏报，§5.1）；
> ③ `automation_policy` 默认**从 `declare-and-wait` 翻转为 `auto-declare`**
> （裁决 issue 自身的不自洽：第二条评审已撤回"人逐条放行"，§6.6）；
> ④ 切片计划**整体重切**为"惰性两片 + 带齐护栏的一片"，因为原切片 1 会把一个
> 半截机制合进 main（§12）。
> 另新增 §3.7（`guard_task_declarations`）——它来自本轮自查发现的一条事实：
> `calm.report.write_markdown` 不受 stomp guard 约束，所以任何"块级工具入口校验"
> 都可被绕过（§0.2(a′)）。
>
> **r2 双通道评审已折叠（21 条发现，全部接受；3 条建议的*补救手段*被驳回并换成
> 更便宜/更保守的方案，逐条见 §14）。** r2 有四处修订**再次改变了设计方向**：
> ⑤ **声明消失时不再 `pending → canceled`，而是"守卫式删除该 pending 行"**
>   （`canceled` 是非 pending，会被 §4.2 规则 2 永久吸收该 `key`，人删一次任务
>   就再也无法重提；同一处修订顺带让 §11.1 的 rebuild≡增量真正成立，§2/§4.2/§11.1）；
> ⑥ **`guard_task_declarations` 一分为二**：`normalize_report_op`（应用**前**的
>   op 改写器，物化墓碑）+ `guard_task_declarations`（前后态快照上的校验器）——
>   `-> Result<(), _>` 的校验器无法执行"删除→墓碑"这个**变更**（§3.7）；
> ⑦ **第 1 级机械检测改为"按 `dst_wave_id` 拉全部冻结引用再逐条重解析"**——
>   `WaveReportEdited` 只带扁平 markdown（`wave_report.rs:566-579`），"从事件算出
>   变更块 id"不可实现；而 `reassign_ids` 只返回**存活**切片（`align.rs:152-168`），
>   被删除的被引用块**根本不会出现在任何 id 集合里**（第三个静默漏报洞，§5.3）；
> ⑧ **切片 3 一分为二**（3a 冻结/索引/检测，仍然惰性；3b 投影/rebuild/迁移），
>   并把 `waves.automation_policy` 与 spec 声明的**单 wave 未结存量上限**从切片 6
>   前移进 3b —— `task_budget` 是**并发容量**不是存量上限（`scheduler/mod.rs:164`），
>   一个失控 spec 可以声明无界 pending 行串行排到天荒地老（§12）。
> 另有两条边界被补上：**引用闭包限制在同 cove（+ system cove）**（否则裁决可以
> 把 cove B 的块内容递给 cove A 的 spec，§5.1），以及 **task 块自身进入它自己的
> 冻结闭包**（否则改一个 in-flight task 的 `goal` 不触发任何失效判定 —— 而那正是
> issue 点名要解决的故障，§5.1）。
>
> **r3 双通道评审已折叠（19 条发现，事实全部成立、全部接受；驳回 2 条建议的
> *补救手段*，并更正 3 处行号，逐条见 §14）。**
> r3 有四处修订**再次改变了设计方向**：
> ⑨ **墓碑的权属载体从 `declared_by` 拆出为独立的 `tombstoned_by`**——r2 的形状
>   让「人删 spec 声明的任务」这条**默认路径必然 400**（规则 4 原位改写
>   `declared_by:"user"`，规则 2 又禁止改它；两个通道交叉命中），且合成的墓碑
>   payload 缺 `kind` 会被 `validate_payload` 拒掉自己（§3.2/§3.7/§6.1）；
> ⑩ **第 1 级机械检测对所有 `EditAuthor` 无条件运行**——`event_warrants_spec_push`
>   （`dispatcher/mod.rs:63`，`WaveReportEdited` 分支 `:95-97`，用于 `:989`）
>   只放行 `User | Plugin`，**spec 自己的编辑被整条丢弃**，而"spec 改了被引用的
>   方案块 / 改了 in-flight task 自己的 goal"恰是最常见的变更源。这是**第四个
>   静默漏报路径**；同一处补上 wave/cove 删除不发 `WaveReportEdited` 的第五个洞（§5.3）；
> ⑪ **`declare-and-wait` 换成人可写、spec 不可写的独立放行位 `released_by_user`**
>   ——r2 的写法（"人把 `ready` 改成 `true`"）是空操作（`ready` 本来就是 `true`），
>   判据 `declared_by` 又被规则 2 冻住 ⇒ 高后果 wave 里 spec 的任务**永远无法被放行**；
>   并补上两列的 PATCH/OpenAPI 写面（照 #644 `WavePatch.task_budget` 的定向单列
>   UPDATE 形状，`wave.rs:168-187` + `routes/waves.rs:864-887`，§6.6）；
> ⑫ **"诊断非空"与"声明消失"合并为同一条删除规则**——r2 的"不 insert / 不 update"
>   对**已经是 `pending` 的行**不成立（不写 ≠ 删除），那行会带着旧声明被
>   `compute_ready` 交出去；且 §11.1(1) 的谓词对"ready:true 但被诊断"未定义，
>   rebuild ≡ 增量当场再破（§4.2 规则 1/4、§11.1）。
>
> **r4 双通道评审已折叠（通道 A 6 条、通道 B 1 条，外加 ⑨–⑫ 的确认；事实全部
> 成立、全部接受；驳回 1 条建议的*补救手段*，逐条见 §14）。** r4 有两处修订
> **改变了设计方向**：
> ⑬ **第 1 级的正确性载体从"事件到达时重解析"改为 fail-closed 全量 sweep**
>   ——通道 A 找到的**第六个静默漏报路径**，而且它与前五个**不在同一维**：前五个
>   都在"谁能改内容"这一维上（通道 A 的枚举表已逐条证明那一维现在确实是关上的，
>   §5.3 已收录该表），第六个在**"改了之后凭什么保证有人去看"**这一维上，
>   而那一维从 r1 起从未被审过。事实是逐字的：`scheduler/mod.rs:24-27` 自己写着
>   "The bus is lossy, so liveness is backstopped by `Scheduler::sweep_all`"；
>   `dispatcher/mod.rs:784-806` 对 `RecvError::Lagged(n)` 只 warn + `sweep_all`
>   （那是**调度活性**兜底，对"某个 in-flight 任务的冻结集是否还与文档一致"
>   一无所知）；每条 envelope 是 `tokio::spawn` fire-and-forget（`:777-782`），
>   进程在 commit 之后、handler 完成之前退出就丢；唯一的跨重启补投
>   `replay_harness_events_since`（`harness/mod.rs:203-228`）按 **spec card** 展开
>   且只查该 wave 自己的事件，对 §5.1 存在理由所在的**跨 wave 引用结构性失明**。
>   ⇒ §11.2 不变量 4 在一次 `Lagged` 或一次重启之后**当场为假**。裁决：
>   **boot 时 + 周期性重扫全部 in-flight 冻结元组，任何无法验证的元组一律
>   `material`；事件路径降级为延迟优化，不再是正确性载体**（§5.3）。
>   **连带简化**：前五个洞、以及通道 B 本轮的 MAJOR（`WaveDeleted`/`CoveDeleted`
>   没有承载受影响 task 集合的载体，`event.rs:419-425` 只带 id）**全部退化为
>   优化**；删除事务内"先读后删算受影响集合"因此被**整条删掉**，两个删除事件
>   **不加任何字段**（省下一次 Tier-A wire 变更）。
> ⑭ **诊断读端从"纯函数"改为"读事务内的派生调用"**——§11.1(1) 的
>   "「可调度」是当前文档的纯函数"在**它自己那句话里**就是假的（谓词含
>   `automation_policy`，那是 `waves` 的**列**），且四类实际决定可调度性的诊断
>   （跨 cove 引用、`unknown_deps`、`spec_task_ceiling`、`declare-and-wait` 放行）
>   **造不出**于 `project_task_declarations(blocks)` 这个明写"不读 DB"的签名。
>   后果是**行被删掉而原因在 UI 上渲染不出来**——一种静默降级，同时违反
>   §4「永不拒绝合并，只降级 + 可见待办」、§5.1、§6.6 与 §8(A)。裁决：读端与
>   `project_tasks_tx` **共用同一个** `evaluate_schedulability_tx`，谓词重述为
>   "**当前文档 + wave 策略列 + 同 wave 既有行**的函数"（§4.2、§11.1）。
>
> **r5 双通道评审已折叠（通道 A 9 条、通道 B 3 条；事实全部成立、全部接受；
> 驳回 1 条建议的*补救手段*，逐条见 §14）。** r5 有四处修订**改变了设计方向**：
> ⑮ **第三个维度：「有人看过之后，凭什么执行那个判决」——它此前是空的**
>   （通道 A 本轮最重要的一条，BLOCKER-adjacent）。维度 ① 是"谁能改内容"
>   （枚举 + `content_hash`，§5.3.2 已关上），② 是"谁保证有人去看"
>   （r4 的 sweep 已关上）；③ 是**执行**，而它此前**没有任何持久载体**：
>   没有列、没有派生查询；`TaskDispatched` 只在 claim 事务里发射
>   （`scheduler/mod.rs:692`，全仓唯一发射点）而被 claim 的行**永不回到 `pending`**
>   （`task.rs:50` 是唯一 INSERT，`:163,:229,:255,:415,:468,:553,:597` 全是前向
>   跃迁）⇒ **§11.2 不变量 5 对 sweep 覆盖的那些行是空洞的**。真正会拿着过期
>   闭包重新开始工作的是**崩溃恢复的重驱动**：`sweep_reconcile` 的 `Dispatched`
>   分支（`scheduler/mod.rs:1067`）→ `resume_dispatched`（`:1397`）→ `drive_spawn`
>   （`:761`），**重新 spawn 且不发 `TaskDispatched`**。而顺序在 §11.2 不变量
>   4b(b) 测试的那个确切场景上就是错的：上下文 sweep 挂在 `sweep_boot`
>   （`:1015`）之后，而 `sweep_boot` → `sweep_reconcile` 早已把每条 dispatched 行
>   重新拉起来了。**重启正是 sweep 存在的理由，而在那条路径上过期的 worker 先被
>   拉起来。** 裁决三件事（§5.3.3 新增）：**(i)** 持久载体
>   `tasks.context_stale_at_ms`（`TaskContextAdvanced{material}` 的投影列）；
>   **(ii)** `resume_dispatched` 内的两道前置检查（上下文 boot 门 + 载体），
>   使**顺序无关紧要**——**这一支已被 r6 的 ⑲ 取代**，因为 `resume_dispatched`
>   不是唯一会起活的东西（载体 (i) 保留）；**(iii)** 尚未开始的工作用**既有**的 `fail_spawn`
>   （`scheduler/mod.rs:891`）终结——理由是那段代码自己写着的（`:789-792`：
>   留着 `dispatched` 会 "pinning the wave budget forever"）。不变量 4/5 随之重述。
> ⑯ **`spec_task_ceiling` 的谓词从 `count(*)` 改为对声明集合的确定性准入**
>   （两个通道各自从"幂等性"与"诊断可渲染性"两个角度命中同一个缺陷）。
>   共用一份实现只能让三个调用点算出同一个**函数**，不能让那个函数**幂等**——
>   而 `count(*)` 会把**正在被重新求值的那些行**也数进去，于是 rebuild ≢ 增量、
>   且普通编辑之间会抖动（§4.2 规则 3″ 给出 ceiling=2 的具体序列）。
> ⑰ **§13.22「人的否决可被换 key 绕过」从"如实记风险"升级为"采纳机制"**
>   （通道 A MAJOR / 通道 B BLOCKER）。r4 的前提「唯一便宜的候选是相似度启发式」
>   **是假的**：设计**已经**在为一条非启发式、与 `key` 无关的机制付钱——
>   §6.6 的 `automation_policy` + `released_by_user`。裁决：**一条未清除的
>   `tombstoned_by:"user"` 墓碑把该 wave 对 spec 声明的任务翻成
>   `declare-and-wait`**（§6.1、§6.6）。零相似度判断、对任何 key 都终止，
>   "人的不作为是吸收态"正是 §6.1 要求的终止性质。代价与 UX 后果如实定价。
> ⑱ **首次部署上的必然批量误判被堵上**（见下）。
>
> **r6 双通道评审已折叠（通道 A 8 条、通道 B 2 条；0 BLOCKER；事实全部成立、
> 全部接受，逐条见 §14）。r6 是最后一轮改变设计方向的评审；r7 是收尾轮
> （见下）。** 两处修订**改变了设计方向**：
> ⑲ **维度 ③ 的强制点从"一个调用点"换成"一个漏斗"**（两个通道从三个角度命中
>   同一个形状）。r5 让 `resume_dispatched` 读那个载体，并宣称"任何调用方重排
>   都不能绕过它"——**而 `resume_dispatched` 根本不是唯一会起活的东西**：
>   (1) operation 的**开机恢复**（`operation/driver.rs:1010-1024` →
>   `:1043-1055` → `drive_one`）是第二条独立的 spawn 入口，且从不读该载体；
>   (2) b1 的判据"有 operation 行 ⇒ 工作已开始"**恰好在它要处理的崩溃窗口上
>   误判**——`submit` 是先插行（`Phase::Pending`）再 drive
>   （`operation/driver.rs:105-123`）；(3) `drive_gate_inner` 的 submit 分支
>   （`scheduler/mod.rs:1541-1581`）会在判决之后**首次**启动一条用过期
>   `gate_json` 构造的真实 shell 命令。裁决**不是三个豁免口，而是一条规则**：
>   **过期判决禁止该 task 上任何 operation *开始*，强制点是适配器的
>   `prepare_tx`** —— 每个 op 在任何副作用之前、在事务内必经它（至少一次，
>   r7 改述；越过 `TxCommitted` 后不再经过），
>   而 `submit` 与开机恢复都通向它。于是 b1/b2 不再是要写的谓词，
>   **而是 phase 阶梯本身**。不变量 5/5b 随之重述（gate 首启补进构造），
>   §6.5 对"产出照常过 gate"的承诺被**明确收窄并定价**（§5.3.3、§13.4/23/24）。
> ⑳ **`spec_task_ceiling` 的排除集从"诊断为空的在声明 key"改为"pending 行"**
>   （两个通道又一次从两个相反方向命中同一处）。r5 的 `D` 把**带诊断的在声明
>   key** 留在了 `occupied` 里（那条 pending 行马上会被删 ⇒ 不幂等仍在，通道 B），
>   又把**仍被声明的在飞行**排除在 `occupied` 外（把新块插到在飞任务块之上即可
>   多拿容量 ⇒ 不变量 7b 当场为假，通道 A）。新判据一句话：
>   **`pending` 行永远是输出，在飞行永远是输入**；7b 随之改述为它真正能保证的
>   那条（含"ceiling 被调低"时的退化形态）（§4.2 规则 3″、§8(A)、§11.2 7b）。
>
> r6 另有三处**补齐而非改向**：(a) `automation_policy` / `spec_task_ceiling`
> 加 **user-only actor 闸**——`X-Calm-Actor` 是自述的（`actor.rs:28-33`），
> 逐块守住 `released_by_user` 却留一个 wave 级总开关不守等于没守（§6.6、
> §11.2 不变量 8(f)）；(b) **切片 1 拒 `declared_by:"user"`**，堵上切片 1→2
> 之间那个"自述、随后被永久冻住"的伪造窗口（§12 切片 1/2）；
> (c) **墓碑诊断补进 §4.2 规则 3′ 的谓词枚举**（不变量 6 压在它上面，
> 而谓词自己的唯一枚举里没有它）。另**更正 §3.1 的安全边界引用**：
> `manifest.rs:491-499` 讲的是 **card** kind；真正支撑结论的是"报告块 kind 是
> 封闭常量 `DATA_KINDS`（`kinds.rs:45`）、根本没有 plugin 注册面"。
>
> **r7 双通道评审已折叠——这是收尾轮，文档到此定稿。**
> **通道 B：APPROVE，零发现。** 通道 A：1 MAJOR + 4 MINOR，**无一阻塞、
> 无一改变设计方向**（①–⑳ 到 r6 为止，r7 不新增），**零驳回**。五条同属一类：
> 设计已在别处做对，而某一处的措辞 / 规范 / 机制载体没跟上——
> (1) **§6.5 说反了一个事实并据此写用户可见文案**：它把"删块 / 立墓碑"当成 ⑲
>   的**例外**，而按 §5.1（task 块是它自己冻结闭包的**根**）、§5.3.1（判据
>   逐字含「**块没了**」）、§4.2 规则 2(ii)，删块是 ⑲ 的**范式情形**——
>   必然判 `material`、必然写 `context_stale_at_ms`、尚未开始的 gate 必然被拒。
>   §6.5 已重写，诊断文案改为与 §5.3.3 / §11.2 不变量 5(b) 逐字一致；
> (2) **`unknown_deps` 的入参在 §4.2 与 §11.1 有两个规范** ⇒ 新增**规则 3‴**
>   把 ⑳ 的"在飞行才是输入"写进 §4.2，并定价它的正确后果（含 `origin='legacy'`
>   存量 pending 行会开始产生诊断，专列进切片 3b 的迁移验收）；
> (3) **不变量 5b 没有可断言的载体**（boot funnel 是 `main()` 的直线代码，
>   运行期不可调用）⇒ 点名项目**已有**的源码序机制
>   （`lib.rs:611-705` 的 `mod boot_order_tests`，`include_str!("main.rs")`
>   + 偏移断言）作为 CI 载体，加上既有的 seam 行为测试，两半缺一不可；
> (4) **§5.3.3 事实 2「只经一次」过强**（`prepare_tx_and_advance` 的 phase
>   UPDATE 守卫在 `lease_owner`，0 行时回滚留在 `Pending`，
>   `operation/repo_sqlite.rs:321-323`）⇒ 改述为「**至少一次，且必在任何
>   副作用之前**；phase 只前进不后退 ⇒ 越过 `TxCommitted` 后不再经过」，
>   承重方向不变（只读、fail-closed、副作用同事务回滚）；
> (5) **切片 3a 的 `TASK_COLUMNS` 仍带 r5 的过期理由** ⇒ 降级为建议，
>   与 §5.3.3(1) 对齐；`declared_by`/`origin` 的同一条纪律在切片 3b 保持"必须"。
> **收敛记录（七轮、BLOCKER 10 → 4 → 3 → 1 → 1 → 0 → 0、三个维度、
> 以及本次评审最可复用的那条自查规则）见 §14 开头。**
>
> ⑱ 的原文：`claim_context_json` 是 `TEXT NULL`
>   新列，而切片 3a 同片既加列又上 sweep ⇒ 部署那一刻所有在飞行都是"缺失"
>   而非"空" ⇒ 首次开机 sweep 把**升级期间每一个在飞任务**判 `material`；
>   叠加 ⑮ 之后那是一次跨升级的**必然 Stuck-ops 事件**。修法是同一个 migration
>   里的一行 backfill（§9 第 6 项）。

> **本文的第一职责是把 issue #985 的事实基线校正到 HEAD。** issue 正文与
> 两条评审共引用了约 20 处 `file:line`，其中 6 处不成立或已过时，另有 4 条
> 机制被当成"已有"而实际不存在。§0 逐条列出；后文的所有裁决建立在校正后的
> 事实上，不建立在 issue 的原文上。

---

## 0. 事实核对（以 HEAD 代码为准）

### 0.1 issue / 评审的引用逐条校验

| # | issue / 评审的表述 | 判定 | 校正后的事实 |
|---|---|---|---|
| 1 | `mcp_server/tools/plan.rs:12,67` —— `calm.plan.upsert` 是 Spec-only | ✅ 成立（**但 in-tx 硬闸的性质要更正**） | 工具名常量 `plan.rs:67`；MCP 软闸 `require_role(Spec)` `plan.rs:689`；descriptor `visible_to_roles: &[CardRole::Spec]` `plan.rs:680`。**`Event::PlanUpdated` 的 in-tx 硬闸不是 Spec-only**：`crates/calm-truth/src/role_gate.rs:257-278` 放行 `ActorId::User` / `Kernel` / `KernelDispatcher` / `Plugin(_)`，只对 `AiSpec` 要求 role==Spec、并整体拒绝 `AiCodex` / `AiClaude`。准确表述是**「worker-AI 排除 + MCP 侧 Spec 软闸」**。这对本设计是**有利事实**：§3.4 的人用写口发 `PlanUpdated` 无需放宽任何闸 |
| 2 | `plugin_host/manifest.rs:225-247` —— `WorkflowDescriptor` 五个字段，其中三个是 prompt-only | ✅ 成立（需精确化） | 结构体 `manifest.rs:225-248`，除 `id` 外**五**个字段。**prompt-only 三个**：`plan_template`（`manifest.rs:228`，仅被序列化进 spec 系统提示 `spec_harness_start_adapter.rs:229-235`，**从不写 `tasks` 行**）、`gates`（`manifest.rs:229-236`，注释明写 NEVER executed）、`spec_instructions`（`manifest.rs:238`，渲染于 `spec_harness_start_adapter.rs:222-228`）。**可执行两个**：`card_kinds`（`manifest.rs:240`，kind 命名空间注册）、`input_schema`（`manifest.rs:247`，#891 的 `workflow_input` 校验）。issue 的"五分之三"成立 |
| 3 | `wave_report.rs:759` —— `persist_report_with_shadow` | ❌ **行号错** | 该文件共 754 行。函数在 `crates/calm-server/src/wave_report.rs:404`；薄封装 `persist_report` 在 `:367` |
| 4 | `report_blocks/align.rs:25-27` 的 rev 单调性 | ✅ 成立，**但路径错** | 文件在 `crates/calm-types/src/report_blocks/align.rs`（不在 calm-server）。rev 语义 `align.rs:24-27`：匹配且规范化相同 → rev 不变；匹配且内容变 → `rev+1`；全新切片 → `rev=1` |
| 5 | `wave_report_blocks.rs:12-19,348-353` —— 块级 `if_rev` 活着且强制 | ✅ 成立 | `RPC_REV_CONFLICT = -32001` `wave_report_blocks.rs:57`；`upsert` 带 `id` 时必填 `:348-357`；`delete` 必填 `:480-486`；**`move` 的 `if_rev` 是可选的** `:428`，且 move 不升 rev（`:13` 注释 "rev untouched"）——评审第 4 条"move 不升 rev 在这里恰好是对的"成立 |
| 6 | `mcp_server/tools/wave_report.rs:534` —— 整文档 `Replace` 无 `if_rev` | ⚠️ **已过时** | 在本 worktree HEAD 仍成立（`tools/wave_report.rs:534`）。但 #979 已在兄弟 worktree `.claude/worktrees/979-if-rev` 落地（commit `be430816`），形状见 §0.3 |
| 7 | `routes/waves.rs` 全文无 `if_rev` | ⚠️ **已过时** | HEAD 成立（`grep if_rev crates/calm-server/src/routes/waves.rs` 零命中）；#979 已给 `UpdateWaveReportBody` 加 `if_rev` |
| 8 | `scheduler/mod.rs:76,80,320`、`compute_ready` 在 `:164` | 半对 | `compute_ready` **正是 `scheduler/mod.rs:164`** ✅；`DEFAULT_WAVE_TASK_BUDGET: i64 = 1` 在 **`:80`** ✅（`:76` 是它的文档注释首行）；`:320` 是注释行，dispatcher 全局信号量字段是 `scheduler/mod.rs:318`，其容量由 dispatcher 决定：`DEFAULT_PERMITS = 8`（`dispatcher/mod.rs:55`），构造于 `dispatcher/mod.rs:666` |
| 9 | `role_gate.rs` 的既有 kernel-only 门禁 | ⚠️ 名称比实际语义更窄 | 全部在 **`crates/calm-truth/src/role_gate.rs`**；**仓库既有所谓 kernel-only 的实际语义是“非 AI、非 Plugin”，`User` 也被允许**，因此不能把名称当成严格内核权限证明。`TaskDispatched` / `TaskGateResult` 保持该既有约定；本片新增的 `TaskContextFrozen` / `TaskContextAdvanced` 因伪造冻结集或判决会直接击穿安全机制，作为例外采用严格 `Kernel | KernelDispatcher`，明确拒绝 `User`。**`crates/calm-server/src/role_gate.rs` 是一行再导出**，本文凡引 `role_gate.rs` 一律指 calm-truth 那个 |
| 10 | `calm-types/src/report_links.rs` —— `neige://` 块级引用，代码块内会被忽略 | ✅ 成立（**可见性需修**） | `parse_destination` `report_links.rs:138-150`；`is_block_id` `:152-159`（**恰好 `b_` + 4 位小写十六进制**）；代码块/行内代码忽略由 pulldown-cmark 事件过滤保证（测试 `:183-192`）。**两者今天都是私有 `fn`，只有 `scan_links`（`:63`）是 `pub`** —— §3.2 规则 5 与 §5.1 要直接调用它们，切片 1 必须把这两个函数提升为 `pub`（NEW，两行） |
| 11 | 「wave_vcs GC 默认每 wave 只留 50 提交」 | ✅ 成立 | `DEFAULT_WAVE_HISTORY_PRUNE_KEEP: usize = 50`，`crates/calm-truth/src/wave_vcs/gc.rs:17`；6 小时一轮 `:15`；默认**启用**（env 未设走默认值，`:129-145`；仅 `NEIGE_WAVE_PRUNE_INTERVAL_SECS=0` 关闭；CLI 同默认 `neige-cli/src/main.rs:1027`）。**精确化：50 是下限不是上限**——活跃会话的 diff 端点被额外保护，删除阈值取全部受保护提交的最早 `created_at`（`gc.rs:24-28,71-93`） |
| 12 | 「`doc_heads` 单向不可回溯」 | ❌ **指向了不存在的东西** | 没有 `doc_heads` 表。`base_doc_heads` 是已 DROP 的 `proposals` 表的列（`migrations/0065_proposals.sql:15`，被 `0066_drop_proposals.sql` 删除）；`ReportDoc::doc_heads()`（`wave_report_doc.rs:151`）是撤回 ④ 通道后的只读残留。**§10 的限定结论仍成立，但理由要换**：报告的历史态只存在于 `wave_vcs` 提交链（keep=50 修剪）里，automerge 文档本身以**单份当前态 BLOB** 存在 `cards.body_crdt`，无历史保留 → 任意时点的 plan 无法重建 |
| 13 | 「模板 = fork 一份报告 …… fork 必须重铸块 id」 | ⚠️ 前提需修正 | 块 id **不在文本里**：非 prose 块的 flat 表示是 ```` ```neige-block <kind> ```` fence，fence 模块文档明写 "carries **no id and no rev** (design D9)"（`report_blocks/fence.rs:6-8`）。id 由内核在对齐时铸造（`align.rs:358`，`b_{:04x}` 内容哈希 + 冲突探针）。**所以"fork 必须重铸 id"这个*要求*不成立**（写方本来就指定不了 id）。**但"fork 会重铸 id"这个*描述*同样不成立**（r2 修正）：§7.3 的裁决是 fork 自己填 `payload.blocks`，于是 `from_payload` 的对齐把源 id **原样保留**（`wave_report_doc.rs:101-114`，测试 `:897-914`），**零 `mint_id` 调用**。全文以 §7.3 为准：**fork 保留 id，不重铸**。真正的缺口是 **`neige://` 引用里的 `wave/<id>` 那一半的重写**（§7.3） |
| 14 | Q3「正在跑的必须走 #653 parked/compensating」 | ❌ **该机制不存在** | parked 原语本身**是活的**（`operation/mod.rs:254,285,306,513-532,613-620`；驱动 `operation/driver.rs:455,546,1025,1095`；migration `0042_operations_parked.sql`；注意 `docs/architecture/653-parked-operations.md:3-4` 的 "No code in this PR" 已过时）。但它只有**两个生产者**——gate runner（`task_verify_adapter.rs:1018`）与 forge wrapper（`forge_action_adapter/mod.rs:1511`），**都不是"取消一个正在跑的 task"**。`calm.plan.cancel` 对 in-flight 任务**明确拒绝**（`plan.rs:985-994`，`-32409`，注释 "interrupting running tasks is out of scope (#644)"）。取消 in-flight 的补偿路径必须从零设计或继续不做（本文选后者，§6.5） |
| 15 | 评审「既有先例：git-forge 的 `merge_policy: hold-for-ratify \| auto-merge`」 | ⚠️ **形状成立，机制不成立** | 它只是 `plugins/git-forge/manifest.json:291-295` 里 workflow `input_schema` 的一个 JSON-Schema enum（默认 `hold-for-ratify`），内核只做**形状校验**（`plugin_host/workflow_input.rs:242`）。**语义完全由 `spec_instructions` 的散文强制**（`manifest.json:400`，以及 merge 步骤的 `goal`/`acceptance_criteria` `:382,387`）——内核不会因为 `merge_policy` 拦住 `gh.pr.merge`，代码里根本没有 `enum MergePolicy`。更强的证据：**内核从不应用 schema 的 `default`**（`routes/waves.rs:503-505` 明写"值原样持久化"），所以"缺省 = hold-for-ratify"这条本身也是 agent 按散文自觉执行的。可以照抄"两档 + 默认等人"的形状，但**不能照抄实现，因为没有实现** |
| 16 | §3.1「与 #976 活数据块同构，两者应共用一套机制」 | ⚠️ 无法核对 | 代码里没有活数据块：`DATA_KINDS = [chart.candles, table, app]`（`report_blocks/kinds.rs:44`），全部是**内联静态数据**（`kinds.rs:15-17` 明写"内核没有行情源，agent 自己抓了写进来"）。#976 尚未落地，本文不对它作机制承诺（§13） |

### 0.2 issue 完全没提、但会决定成败的四条既有事实

**(a) 人今天能"新建"非 prose 块，但不能改、不能删。**（r1 修正：初稿写的
"人根本写不了非 prose 块"**说过头了**，逐字读 guard 源码后更正。）人的唯一报告写口是
`POST /api/waves/{id}/report`（路由 `routes/waves.rs:89`，处理器 `:1207`），
它走整文档 `Replace`。`apply_report_op` 对 `Replace` 依次跑两道闸
（`wave_report.rs:167-171`）：

- `validate_body_fences`（`wave_report_guard.rs:28`）—— 校验 body 里**每一个**
  fence 的 payload；**新出现的 fence 只要合法就放行**；
- `guard_non_prose_stomp`（`wave_report_guard.rs:58-80`）—— 只遍历
  **`current` 里已存在的**非 prose 块，要求对齐后 id/kind/canonical fence 三者
  逐字节保持；**它对"新增一个 fence"零约束**。

所以准确的缺口是：**人可以在整文档写里"新写下一个 `task` fence"，但一旦它落进
文档，人就再也改不动、删不掉它**（改/删都会撞 stomp guard）。这依然让 issue 的
"人删 AI 的任务"叙事无处落脚，仍是第一优先机制缺口（§3.4）——但理由要说准。

**(a′) `WriteMarkdown` 完全不受 stomp guard 约束。** `apply_report_op`
只对 `Replace` 调 `guard_non_prose_stomp`；`WriteMarkdown` 分支
（`wave_report.rs:174-184`）只跑 `validate_body_fences`。而
`calm.report.write_markdown` 是 **Spec 的工具**。结论：**今天 spec 已经可以用
一次 `write_markdown` 任意新建 / 改写 / 删除任何非 prose 块**。这条对本设计是
决定性的——它意味着 §6.1「AI 不得删人声明的任务」与 §4.4 的归因**都不能靠块级
工具的入口校验来守**，必须守在 `apply_report_op` 这个所有写路径的**唯一收口**上
（§3.7 的 `guard_task_declarations`）。

**(b) `task` 块里的 `neige://` 引用今天产生不了反链。**
`report_backlinks.rs:177-180` 只扫描 `block.kind == KIND_PROSE` 的块。
§3.5「父块用 `neige://wave/<id>` 反链指向子 wave」与评审第 3 条的引用闭包，
都依赖对**非 prose 块的声明字段**做链接扫描——需要新建（§5.2）。

**(c) 反链是读时全 cove 扫描，无索引。** `backlinks_for_wave`
（`report_backlinks.rs:106`）读 cove 内**全部** wave-report 卡再逐块扫描
（`:122-215`），并且只在 cove 内解析——跨 cove 引用不产生反链。引用闭包的
stale 检测若照此实现，代价是 O(cove 内报告总字节)。§5.3 给出替代路径。

**(d) `tasks` 表的唯一声明写者确实只有 `calm.plan.upsert`。**
穷举结果（`tasks` 表的全部 SQL 与调用点，见 §0.4）：写声明列的只有
`task_insert_tx`（`db/sqlite/task.rs:48`）与 `task_update_pending_tx`（`:98`），
二者的**生产调用点只有 `plan.rs:803` 与 `plan.rs:807`**。其余全部写入只改状态
列（scheduler / decision_sink / reaper / task_verify_adapter）。**没有 REST 端点、
没有 web 调用者、没有 plugin 写口、没有 admin CLI 写口。** 这让 Q2 变成一个
干净的问题（§10 Q2）。

**(e) `WaveReportEdited` 的推送只到"被编辑的那个 wave"的 harness。**
dispatcher 的分支 `Event::WaveReportEdited { author, wave_id, .. } =>` 调
`self.observe_harness(wave_id.clone(), …)`（`dispatcher/mod.rs:983-998`），
observation 构造在 `:1308-1312`；谓词 `event_warrants_spec_push`
（函数 `dispatcher/mod.rs:63`，`WaveReportEdited` 分支 `:95-97`）只放行
`EditAuthor::User | Plugin`。**没有任何"被编辑的块 → 引用它的任务"
的反向查找**，也没有跨 wave 路由。§5.1 明确允许跨 wave 引用，所以三级阶梯的
第 1 级必须自带反向索引与路由（§5.3），不能靠这条现成通道。

**(e′) 而且这条谓词会把 spec 自己的编辑整条丢掉**（r3 通道 A MAJOR，成立，
改设计方向）。`:989` 的分支体是 `if event_warrants_spec_push(…) { observe_harness(…) }
else { tracing::trace!(…) }`——`EditAuthor::Spec`（以及 `Kernel`）走 `else`，
**什么都不做**。这对第 2 级推送是对的（spec 自己写的东西再推回去会成环，
`:90-94` 的注释明写这个理由），但**对第 1 级机械检测是致命的**：
"spec 改了被引用的方案块"「spec 改了一个 in-flight task 自己的 `goal`」
是本设计要检出的**最常见**变更源。裁决写在 §5.3：**第 1 级对所有 author 无条件
运行，该谓词只管第 2 级。**

**(f′) wave / cove 的删除不发 `WaveReportEdited`。** `wave_delete_tx`
（`crates/calm-truth/src/db/sqlite/wave.rs:191-234`，含 `DELETE FROM tasks
WHERE wave_id` 于 `:207`）只随 `Event::WaveDeleted`
（`routes/waves.rs:1050`）落地；cove 删除走 `cove_delete_tx`
（`crates/calm-truth/src/db/sqlite/cove.rs:147` 起，逐 wave 删 tasks 于 `:162`），
`replay.rs:363` 的 fixture 重置同样直接 `DELETE FROM tasks`。**三条路径都不产生
任何报告编辑事件**，所以"被引用块随整个 wave/cove 消失"不会触发任何重解析——
§5.3 已据此补上 `WaveDeleted` / `CoveDeleted` 触发与索引清理。

**(f) 块 id 会被回收，且 rev 的自增是 `saturating_add`。**
（r2 修正：机制的**时序**说错了，结论不变。）`reassign_ids` 用**传入的全部
`old_blocks`**（= 整份前态快照，含即将消失的那个块）预置 `used`（`align.rs:151`），
`mint_id`（`:352-364`）只对 `used` 探冲突。所以被删块的 id **不是在同一次写里**
立刻可用，而是在**下一次对齐时**被释放——那时它已不在 `old_blocks` 里。
`ReportDoc::update` 的文档注释明写 "vanished blocks are deleted"
（`wave_report_doc.rs:198`）。而全新切片的 rev 恒为 1（`align.rs:167`）。
**"删块 → 下一次编辑 → 一个不相干的新块铸出同一个 `(id, rev=1)`"是完全可达的
两步序列**，所以"冻结 `(block_id, rev)` 即可不漏报"依然被否掉。
于是 `(b_1f3a, rev=1)` 可以对应**两个毫不相干的块**。
另外两处 rev 自增都是 `saturating_add(1)`（`align.rs:163`、
`wave_report_doc.rs:474`）：在 `u32::MAX` 处内容变而 rev 不变。
**这两条一起否掉了"冻结 `(block_id, rev)` 即可不漏报"**，§5.1 已据此改为
冻结 `(block_id, rev, content_hash)`。

**(g) 事件总线是有损的，而且内核自己是这么设计的**（NEW，r4 通道 A BLOCKER，
成立，改设计方向 ⑬）。这是本设计**唯一一条**不在"谁能改内容"那一维上的
事实，也是前三轮从未审过的一维。逐字复核：

- `crates/calm-server/src/scheduler/mod.rs:24-27` 的模块文档写着
  "**The bus is lossy**, so liveness is backstopped by
  [`Scheduler::sweep_all`] — run at boot (after operation recovery),
  on `RecvError::Lagged`, and on a slow periodic reconcile tick
  (`NEIGE_SCHEDULER_RECONCILE_SECS`, default 300)"；`sweep_all` 本体在
  `scheduler/mod.rs:991`，boot 门在 `:1015`（`sweep_boot` 完成前 `sweep_all` no-op）。
- `crates/calm-server/src/dispatcher/mod.rs:784-806`：`rx.recv()` 遇
  `RecvError::Lagged(n)` 只 `tracing::warn!` + `scheduler.sweep_all()`
  （`:802`）。**`sweep_all` 是调度活性的兜底**（重扫 pending 行），它对
  "某个 in-flight 任务的冻结集是否还与当前文档一致"**一无所知**。丢掉的 `n` 条
  `WaveReportEdited` 对第 1 级机械检测就是**永久丢失**。
- 同处 `:777-782`：每条 envelope 走 `tokio::spawn(inner.handle_envelope(envelope))`，
  注释逐字写着 "Per-event spawn is **fire-and-forget**"。进程在事件 commit 之后、
  handler 完成之前退出（重启 / shutdown / panic），该次检测**无人接手**。
- 唯一的跨重启 `wave.report_edited` 补投是
  `crates/calm-server/src/harness/mod.rs:203-228` 的
  `replay_harness_events_since`：它 (i) 按 **spec card** 展开，
  (ii) 只查 `events_for_wave(wave_id, …)`——**harness 自己那个 wave 的事件**，
  (iii) 喂的是第 2 级 harness 推送。而 §5.1 存在的**全部理由**就是跨 wave 引用
  （dst wave X 的编辑要唤醒 wave W 的任务），这条补投**结构性地**看不到它。
- `WaveDeleted` / `CoveDeleted` 更糟：r3 的写法要求受影响集合"由删除事务在事务内
  算出、不能由订阅者事后反查"，于是一条丢掉的 envelope **按设计不可恢复**
  ——dst wave 已删、索引行已清，没有任何东西能重建它。

**结论**：把第 1 级的正确性挂在这条总线上，等于把一条全称断言
（"不允许漏报"）挂在一个自承 lossy 的边沿触发通道上。§5.3 已据此把正确性载体
改成 fail-closed 全量 sweep，事件路径降级为延迟优化。

**(h) 「不再派发」这个说法在代码里没有着落：真正会重新开始工作的是崩溃恢复的
重驱动，而它不发 `TaskDispatched`**（NEW，r5 通道 A，成立，改设计方向 ⑮）。
这是维度 ③（"有人看过之后凭什么执行那个判决"）的事实基础，逐字复核：

- **`TaskDispatched` 全仓只有一个发射点**：`scheduler/mod.rs:692`，在 claim
  事务内、`task_claim_pending_tx` 返回 1 行之后构造
  （`grep -n TaskDispatched crates/calm-server/src/scheduler/mod.rs` 只有 `:39`
  的模块注释、`:596` 的函数注释与 `:692` 这一处）。
- **被 claim 的行永不回到 `pending`**：`crates/calm-truth/src/db/sqlite/task.rs`
  里 `:50` 是唯一的 `INSERT`（`status='pending'`），其余全部是前向跃迁
  （`:163` canceled、`:229` dispatched、`:255` running、`:415` done、
  `:468` verifying、`:553` done|failed、`:597` failed），**没有任何一条 SQL 把
  `status` 写回 `'pending'`**。
- ⇒ **对一条已经离开 `pending` 的行，"不得再产生新的 `TaskDispatched`"是一条
  恒真的空话**——它在机制上根本不可能发生，所以它挡不住任何东西。
- **真正的重驱动路径**：`sweep_reconcile`（`scheduler/mod.rs:1041`）的
  `TaskStatus::Dispatched` 分支（`:1067`）→ `resume_dispatched`（`:1397`）→
  `drive_spawn`（`:761`）。`drive_spawn` 走 `runtime.submit(...)`（幂等键 =
  `task.id`）+ `wait()` + `reconcile_spawn_result`，**全程不发任何
  `TaskDispatched`**。它的 doc comment（`:1391-1396`）自己写明这条分支的语义是
  "the claim landed but the spawn outcome was never reconciled（crash between
  claim and op insert…）"。
- **而 boot 的顺序恰好是反的**：`sweep_boot`（`:1015`）第一件事就是
  `sweep_reconcile()`（`:1016`），也就是**先**把每条 dispatched 行重新拉起来；
  r4 把上下文 sweep 挂在"`sweep_boot` 之后"。**重启正是 sweep 被引入所要解决的
  场景，而在这条路径上过期的 worker 先被拉起来。**

- **它也不是唯一的重驱动路径（r6 补，两个通道交叉命中）**。另外三条同样会在
  过期闭包上**开始**工作，而 r5 的强制点（放在 `resume_dispatched` 里）
  一条都盖不住：
  1. **operation 的开机恢复**：`plan_recovery_for`（`operation/driver.rs:1010-1024`）
     把 `Pending | TxCommitted | AppServerInteract | SpawnStarted |
     SpawnSucceeded` 一律映射为 `Recover`，`apply_recovery_item`（`:1043-1055`）
     直接 `drive_one` ⇒ **它真的会 spawn**，且全程不读 `tasks`；
  2. **`submit` 的 `Pending` 窗口**：`OperationRuntime::submit`
     （`operation/driver.rs:105-123`）**先** `insert_operation` **再** `drive()`，
     phase 阶梯（`crates/calm-truth/migrations/0042_operations_parked.sql:13-24`）
     在任何 spawn 之前还有 `pending`/`tx_committed` ⇒ **"有 operation 行"
     ≠ "工作已开始"**；
  3. **gate 的首次启动**：`drive_gate_inner`（`scheduler/mod.rs:1541-1581`）
     分支 2 会提交一个 `task-verify` op，用该行已冻结的 `gate_json` 跑真实
     shell 命令（`operation/task_verify_adapter.rs:660-665`）。
- **四条路径的公共必经点只有一个**：它们全都要经过某个 operation 的
  `prepare_tx`（`operation/driver.rs:388-393`，只在 `Phase::Pending` 上跑、
  在任何副作用之前、在事务内）。

结论：维度 ③ 需要**一个持久载体 + 一处站在漏斗上的强制点**，而不是一条针对
`TaskDispatched` 的措辞、也不是逐个调用点的 if。§5.3.3 给出裁决。

### 0.3 #979 的实际落地形状（读自 `.claude/worktrees/979-if-rev`，commit `be430816`）

它**不是**块级 rev 的推广，而是**新增了一个文档级 rev**：

- `ReportDoc` 根上新增 `doc_rev`（Uint，`wave_report_doc.rs` 新增
  `doc_rev()` / `increment_doc_rev()`），旧文档缺该字段 → 读作 0；
- 每次成功 persist 后自增（在同一事务内）；
- `calm.report.write` / `calm.report.edit` / `write_markdown` 的
  `if_rev` **变为必填**，冲突映射到既有的 `RPC_REV_CONFLICT = -32001`；
- REST `UpdateWaveReportBody` 新增必填 `ifRev`，冲突 → 409；
- `WaveReportPayload::SCHEMA_VERSION` 2 → 3，新增 `docRev` 字段（Tier-A 全流程：
  openapi.json / zod / generated-events 同步）。

**对本设计的影响**：评审第 4 条（#979 不阻塞本设计）**结论仍成立，但理由要更正**
（r2）——claim 失效检测的权威判据是冻结集里的 **`content_hash`**（§5.1；块级
`rev` 只是便宜的先判与给人看的诊断，它在 id 回收与 `saturating_add` 两处都会漏报），
与文档级 `doc_rev` 正交。但有两条要吸收：
1. 本设计新增的任何写口（§3.4 的人用块级 REST）**必须从第一天就带 OCC**，
   否则重开 #979 刚堵上的洞。**但守的东西分两种**（r1 后修正）：
   update/delete 守 `if_block_rev`，create / positional insert / move 守
   **`if_doc_rev`**——被守的块还不存在、或被改的是 `order` 而不是块内容，
   块级 rev 在那里无物可守（§3.4）。**于是本设计对 #979 的依赖比初稿说的强**：
   `if_doc_rev` 就是 #979 引入的 `doc_rev`。
2. §5 的冻结集只冻**块级**的 `(block_id, rev, content_hash)`，**不冻 `doc_rev`**
   ——文档级 rev 每次写都变，用它冻结等于让任何无关编辑都触发 stale（正是 OCC
   长事务误中止的经典病）。

### 0.4 `tasks` 表写者全景（Q2 的事实基础）

| 写入 | 位置 | 性质 |
|---|---|---|
| `task_insert_tx` / `task_update_pending_tx` | `db/sqlite/task.rs:48,98` ← **仅** `plan.rs:803,807` | **声明** |
| `task_cancel_tx` | `task.rs:160` ← `plan.rs:1032` | 声明（撤销）+ 状态 |
| `task_claim_pending_tx` | `task.rs:222` ← `scheduler/mod.rs:641` | 状态 |
| `task_mark_running_tx` | `task.rs:246` ← `scheduler/mod.rs:873` | 状态 |
| `task_stamp_missing_running_deadline_tx` | `task.rs:273` ← `scheduler/mod.rs:1133` | 状态 |
| `task_report_success_from_worker_tx`（→ done / verifying） | `task.rs:405,458,493` ← `decision_sink.rs:162`、`scheduler/mod.rs:1877` | 状态 |
| `task_fail_from_worker_tx` | `task.rs:586` ← `decision_sink.rs:181`、`scheduler/mod.rs:912,1222,1909`、`reaper/mod.rs:559` | 状态 |
| `task_gate_attempt_bump_tx` / `task_apply_gate_result_tx` | `task.rs:514,541` ← `operation/task_verify_adapter.rs:689,197` | 状态 |
| 裸 `UPDATE tasks SET gate_pid…` | `operation/task_verify_adapter.rs:887` | 状态 |
| `DELETE FROM tasks WHERE wave_id` | `db/sqlite/wave.rs:207`、`db/sqlite/cove.rs:162`、`replay.rs:363` | 级联删除 / replay 重置 |

MCP 面共三个 plan 工具（注册 `plan.rs:77-81`）：`upsert` / `cancel` / `list`。
无 `plan.get` / `plan.delete`。退役的 `calm.task.dispatch` 已是**零写入的
隐藏 shim**（`mcp_server/tools/emit.rs:85-124`，`visible_to_roles: &[]` 于 `:107`，
返回迁移指引）——这是 Q2 直接可抄的先例。

---

## 1. 边界判据：这个东西最终会不会沉淀进记录

采纳评审第 1 条，**替换**讨论中一度使用的「控制面 vs 产出物」二分。后者是
结构性的，而现实是时间性的：同一个块在未决时是控制面，落定后是记录。

> **判据：这个东西最终会不会收敛进「我们做了什么 / 我们当时决定了什么」的记录？
> 会 → 进文档（CRDT）。不会 → 不进文档。**

它事后解释了三个**独立做出**的决定：

| | 会收敛吗 | 该进文档吗 | 实际决定 |
|---|---|---|---|
| 提案队列（pending/accept/reject） | 不会——裁决过程的噪音 | 否 | #973/#978 撤回 ④ ✓（`955-kernel-app-boundary.md` §3.2 / D5） |
| `task` 块 | 会——完成后即"我们做了什么" | 是 | 本设计 ✓ |
| 活数据 | `source` 声明收敛，跳动的数值不收敛 | 声明进、数据不进 | #976（未落地，§13） |
| 「人否决过这件事」（墓碑） | 会——"我们决定不做"是终局结论 | 是 | §6.1 ✓ |
| pending/claimed/租约/尝试次数 | 不会 | 否 | §2 ✓ |

它同时化解了与 #330（"要产出与证据，不是协作文档平台"）的表面冲突：#330 反对的是
**永不收敛**的协作物（讨论串、指派、在线状态），不是过程性内容本身。

这条判据**不替代** `955-kernel-app-boundary.md` §1.1 的三条内核/app 判据——那三条
回答"能力放哪个平面"，本条回答"事实放哪个载体"，两轴正交。

---

## 2. 声明 / 状态必须分开（承重墙，逐字保留 issue §2 的立论）

| | 内容 | 唯一真源 | 写者 |
|---|---|---|---|
| **声明** | 任务是什么、依赖、验收标准、gate 该查什么、是否就绪、**是谁提出要做它**（§4.4）、人是否否决过 | **文档（CRDT）** | spec + 人 |
| **状态** | pending/dispatched/running/verifying/done/failed/canceled、尝试次数、租约（`running_deadline_ms`）、gate 结果、幂等键、**claim 时冻结的引用闭包**（§5.3）、**"这个 claim 的上下文已被判失效"**（§5.3.3 的 `context_stale_at_ms`，`TaskContextAdvanced` 的投影） | **事件日志** | 只有内核 |

**这堵墙的精确形状**（r1 评审两个通道同时指出初稿在这里自相矛盾——§6.1 的墓碑
投影会调 `task_cancel_tx`，而那个函数写 `status`）。正确的表述是三句话，不是一句：

1. **状态的取值绝不进文档。** 文档里没有 `status` 字段，没有 attempt，没有租约。
   这一条无例外。
2. **状态的写者仍然只有内核。** 投影函数是内核代码，它在内核的事务里写。
   "只有内核写状态"从未被破坏。
3. **但行的*存在性*由声明决定——恰好两处，并且只有这两处**（r2 改写，
   见下面「为什么不是 `canceled`」）：
   - 新声明落地 → 该行以 `status='pending'` 创建；
   - 声明消失 / 被墓碑覆盖 / `ready` 被撤回，**且该行仍是 `pending`**（=
     从未派发过）→ **该行被守卫式删除**：
     `DELETE FROM tasks WHERE id = ?1 AND status = 'pending'`
     （NEW `task_delete_pending_tx`，与 `task_cancel_tx`
     `crates/calm-truth/src/db/sqlite/task.rs:160` 同一个单赢家 `WHERE` 形状）。
     非 `pending` 的行一律不碰（§6.5）。

   于是**声明面写的唯一 `status` 取值就是创建时的 `'pending'`**；之后每一次
   状态跃迁都仍然只由执行面（scheduler / decision_sink / gate runner）写。
   墙因此比 r1 的形状更干净，不是更脏。

**为什么不是 `pending → canceled`（r2 双通道 BLOCKER，改设计方向）。**
r1 写的是复用 `task_cancel_tx` 把行翻成 `canceled`。逐字读源码后这条**必须推翻**：
`task_cancel_tx` 留下的是 `status='canceled'`（`task.rs:160-170`），而 `canceled`
是**非 pending**，于是 §4.2 规则 2（非 pending 行的声明变更 → 只出 stale 诊断、
永不写入）会让这个 `(wave_id, key)` **被永久吸收**——人删一次任务、`canceled`
行留下，此后**任何**同 `key` 的重新声明都只会得到一条陈旧诊断，永远不再变成
pending 行。而 §6.1 明确承诺"删掉墓碑块 = 撤回否决，此后 AI 可以合法重提"，
§3.7 规则 4 又让**每一次人的删除**都走这条路径。也就是说：**默认路径上，
墓碑不是终止的，是吸收的。** MCP 侧有同样的先例（`plan.rs:451-463` 对非
`Pending` 行一律 `return Err`），只是那条路径上没有"撤回否决"的承诺。

删除而不是 canceled，三条理由：

1. **一个从未派发过的 `pending` 行不含任何执行史**——没有 worker 卡、没有 gate、
   没有 operation（`tasks.id` 作为幂等键从未被消费，`plan.rs:580` →
   `dispatcher/mod.rs:180`）。删掉它删不掉任何证据（#330 的顾虑不适用）。
2. **"我们决定不做这件事"的记录仍在**，而且在正确的载体上：**墓碑块住在文档里**
   （§1 判据、§6.1），不在 `tasks` 表里。用一条 `canceled` 投影行去承担这份记录，
   正是 r1 那个洞的来源。
3. **它让 §11.1 的 rebuild ≡ 增量投影第一次真的成立**：增量删、rebuild 也删，
   同一份文档只有一种终态。r1 的形状是增量留 `canceled`、rebuild 直接删——
   两个通道都指出那不是等价，只是把等价重述了一遍。

`task_cancel_tx` **仍然保留**：`calm.plan.cancel`（§10 Q2 保留它）与
`origin='legacy'` 行的取消语义不受本裁决影响。本设计的声明路径不再调用它。

**状态绝不能进文档**，四条理由，第 3 条是决定性的：

1. 内核会成为报告的第二个写者——#973 / #978 刚拆掉的东西
   （`955-kernel-app-boundary.md` §4）；
2. 每次状态跳变要 load/save 整份 automerge BLOB（`cards.body_crdt`，
   `wave_report.rs:488-566` 的完整 load→project→apply→project→save 流程）再发
   两个事件，文档退化成状态机日志；
3. **CRDT 的合并语义与状态机语义不兼容。** automerge 的设计目标是「永不冲突、
   总能合并」；状态机要的是「带前置条件的串行化事务」。项目里每一处状态跳变都是
   **带 WHERE 前置条件的单赢家 UPDATE**（`task.rs:228` `WHERE status='pending'`、
   `:254` `WHERE status='dispatched'`、`:552` `WHERE status='verifying' AND
   gate_attempt=?`）——这些前置条件在 CRDT 里没有对应物。并发编辑合并后会出现
   「已完成的任务复活」「同一任务被认领两次」，而 **CRDT 不会报错，它会高兴地合并**。
   这不是实现难度，是语义冲突；
4. operations saga 需要**提交时刻就 durable 的幂等键**（`tasks.id =
   "{wave_id}:{key}"`，`plan.rs:580`，被 dispatcher 当作幂等键直接查
   `dispatcher/mod.rs:180`），那东西不能住在一份会合并的文档里。

内核对 `task.dispatched` / `task.gate_result` 的 kernel-only 硬闸
（`role_gate.rs:289-360`）正是状态面已有的边界，**本设计不动它**。

**精确表述**：文档是声明的唯一真源；状态的唯一真源是事件日志；**`tasks` 表是
两者的可重建投影**。这与项目既有取向一致——`cards.payload` 是 CRDT 的投影
（`wave_report.rs:541-556`），刚被 DROP 的 `proposals` 表也是事件的投影。

---

## 3. `task` 块

### 3.1 kind 归属：内核拥有，plugin 不得定义（安全边界）

`task` 加入 `DATA_KINDS`（`crates/calm-types/src/report_blocks/kinds.rs:45`），
payload 校验加入 `validate_payload`（`kinds.rs:55-91`）。理由不是风格：plugin
若能定义一个"看起来像 task"的块，就等于拿到了调度器的间接驱动权，绕过全部
spawn 门禁（`955-kernel-app-boundary.md` §3.2 第一行）。

**这条安全边界立在哪（r6 通道 A MINOR，更正引用）。** r5 及以前写的是"与
plugin 的 `card_kinds`（`manifest.rs:240`，禁撞内建）完全隔离"——**所引证据讲的
是 card kind，不是 block kind**：那条冲突检查（`crates/calm-server/src/plugin_host/manifest.rs:491-499`）
是 `CardKindRegistry::builtins().claims_kind(...)`，属于 **card** kind 命名空间，
与报告块 kind 无关。结论不变，但真正支撑它的事实**更强**：

> **报告块 kind 是一个封闭常量 `DATA_KINDS`（`kinds.rs:45`），
> 没有任何 plugin 注册面**——manifest 里根本不存在"block kind"这个字段；
> 且 `validate_payload` 对**未知 kind 直接报错**
> （`kinds.rs:67-70`：`other => errors.push("unknown block kind …")`）。

也就是说：plugin 不是"被禁止撞车"，而是**根本没有那张桌子**。
边界从此立在这条事实上，不再立在一条讲 card kind 的引用上。

同理，评审第 2 条补充的载体裁决成立：**就绪标记必须是 `task` kind 自己的字段，
不能是"任意块加一个 `ready: true` 字段"**——后者等于让任何 prose 块都能驱动调度器。

**代价（如实记）**：`task` 成为非 prose kind，立刻继承三条既有事实——
(i) `guard_non_prose_stomp` 会拒绝任何试图**改动或删除**它的整文档 `Replace`
（§0.2(a)；新建不受阻）；(ii) `calm.report.write_markdown` **完全不受**该
guard 约束，spec 可以借它任意改删（§0.2(a′)）；(iii) `report_backlinks`
不扫描它（§0.2(b)）。(i)+(ii) 决定了收口必须放在 `apply_report_op`（§3.7），
(iii) 在切片 2 解决。

### 3.2 payload schema（NEW，严格校验）

```jsonc
// ```neige-block task
{
  "key": "impl-parser",             // 必填。任务身份，见 §3.3。
                                    // 语法与 tasks.key 完全一致：
                                    // ^[a-z0-9][a-z0-9._-]{0,63}$（plan.rs:165-176）
  "kind": "codex",                  // 必填。codex | claude | terminal
                                    // （与 TaskKind 同集合，plan.rs:213-222）
  "goal": "把 parser 拆成三个模块",   // 必填，trim 后非空
  "acceptance": "…",                // 可选。自然语言验收标准
  "gate": {                         // 可选。形状 == GateInput（plan.rs:119-134）
    "cwd": "/abs/path",
    "timeout_secs": 1800,
    "steps": [{ "name": "test", "cmd": "cargo test" }]
  },
  "no_gate_reason": "…",            // 可选。rule 6 逃生口（plan.rs:247-259）
  "depends_on": ["design"],         // 可选。同 wave 内的 key 集合
  "priority": 0,                    // 可选，默认 0
  "cwd": "/abs/path",               // 可选，绝对路径
  "context": { },                   // 可选，任意 JSON，透传给 worker
  "refs": ["neige://wave/w1#b_1f3a"],// 可选。显式引用闭包，见 §5.1
  "ready": true,                    // 必填。就绪标记，见 §4.2 / §10 Q6
  "declared_by": "spec",            // 必填。spec | user。**声明**来源，见 §4.4。
                                    // 写时由 guard_task_declarations 强制
                                    // 等于本次编辑的 EditAuthor（§3.7），
                                    // 之后**永不可改**（含转墓碑时，r3 ⑨）。
  "released_by_user": false,        // 可选，默认 false。**放行位**，见 §6.6。
                                    // 只有 EditAuthor::User 能写/改它；
                                    // 仅在 automation_policy = declare-and-wait
                                    // 时有语义。墓碑块上必须缺席。
  "spawn": "in-wave",               // 可选。in-wave（默认）| sub-wave，见 §10 §3.5
  "tombstone": null,                // 可选。见 §6.1；非空时 payload 形状见规则 7
  "tombstoned_by": null             // 墓碑块必填、非墓碑块必须缺席。spec | user。
                                    // **墓碑的权属**，与 declared_by 正交（r3 ⑨）：
                                    // 写时强制等于本次编辑的 EditAuthor，之后不可改。
}
```

**严格校验规则**（全部在 `validate_payload` 内，逐字段路径报错，与既有 kind
一致 `kinds.rs:55-92`）：

1. `deny_unknown_fields` —— 未知字段直接拒（`reject_unknown` `kinds.rs:94` 的
   既有做法）；
2. `key` 过 `key_is_valid`（复用 `plan.rs:165`，提升到 calm-types 以便两端共用）；
3. `kind ∈ {codex, claude, terminal}`；
4. `gate` 过 `validate_gate_shape`（复用 `plan.rs:352`，同样提升到 calm-types）；
5. `refs[]` 每项过 `report_links::parse_destination`（`report_links.rs:138`），
   且**必须带 `#b_xxxx` 片段**（`is_block_id` `:152`）——见 §5.1 的位置式引用禁令。
   **两个函数今天都是私有的，切片 1 需把它们提升为 `pub`**（§0.1 #10，NEW，两行）；
6. 尺寸沿用既有上限：`MAX_STRING_CHARS = 2048`、`MAX_CANONICAL_BYTES = 256 KiB`
   （`kinds.rs:39,42`）；
7. **墓碑形状是一个封闭集合**（r3 通道 A BLOCKER，成立）。r2 只豁免了
   `goal / gate / depends_on / ready`，而 `kind` 在本节是**必填**——于是
   §3.7 规则 4 合成的墓碑 payload 与 §6.1 的示例**都会被 `validate_payload`
   自己拒掉**（改写器的产物过不了同一次 persist 里的校验器）。裁决：
   > `tombstone` 非空时，payload **有且只有**四个字段：
   > `key`（必填）、`tombstone`（必填）、`declared_by`（必填，承接原块的值）、
   > `tombstoned_by`（必填）。**其余字段一律必须缺席**——包括 `kind`、
   > `goal`、`acceptance`、`gate`、`no_gate_reason`、`depends_on`、`priority`、
   > `cwd`、`context`、`refs`、`ready`、`spawn`、`released_by_user`。

   反过来：`tombstone` 为空/缺席时，`tombstoned_by` **必须缺席**。
   于是"墓碑"与"任务"是两个互斥的封闭形状，改写器的产物是**唯一**的规范形态，
   §6.1 的示例按此改写。
8. `declared_by ∈ {spec, user}`、`tombstoned_by ∈ {spec, user}`。
   **两者的正确性都不由 `validate_payload` 保证**——纯函数看不到写者是谁；
   由 §3.7 的 `guard_task_declarations` 在持久化事务里强制。
   `released_by_user` 同理（§3.7 规则 5）。
   **切片 1 期间的临时收紧（r6）**：`guard_task_declarations` 要到切片 2 才存在，
   而切片 2 的规则 2 会把值永久冻住 ⇒ 切片 1 的 `validate_payload`
   **只接受 `declared_by: "spec"`**，切片 2 在落地规则 1 的同一个 PR 里
   放宽为 `{spec, user}`。理由与代价见 §12 切片 1。

**注意 `depends_on` 的跨文档限制**：依赖只能命名**同一份报告内**的 `key`。
跨 wave 依赖用 `refs` 表达（引用闭包 → stale），不进调度图——`compute_ready`
（`scheduler/mod.rs:164-191`）与 `resolve_plan_batch` 的环检测
（`plan.rs:472-482`）都是 per-wave 的，跨 wave 依赖会把环检测变成跨文档的
分布式问题，收益不抵复杂度。

### 3.3 任务身份 = payload 里的 `key`，**不是** block id（Q3 裁决）

issue Q3 的前提（"以块 id 作为 task 身份"）**不成立**，两条硬事实：

- **block id 在整文档重写下不稳定。** 它由内核在对齐时铸造：`align.rs:358`
  `format!("b_{:04x}", (hash as u16).wrapping_add(candidate))`，复用靠 LCS 锚点 +
  gap 相似度**启发式**（`align.rs:20-34`）。一次大幅重写完全可能让同一个任务的
  块换 id。用它当身份 = 用启发式当主键。
- **block id 不在文本里。** fence 不携带 id / rev（`fence.rs:6-8`，design D9）。
  这是好事（文本复制不会复制 id，Q3 担心的重复 id 不会发生），但也意味着
  **写方无法指定 id**，人/agent 都无法用 id 表达"这还是原来那个任务"。

裁决：**`key` 是身份，block id 是引用锚。** `key` 直接对上既有的
`UNIQUE (wave_id, key)`（`migrations/0041_tasks.sql:28`）与 `tasks.id =
"{wave_id}:{key}"`（`plan.rs:580`，同时是 operation 幂等键）。

由此产生的两个后果，都可机械处理：

- **复制粘贴会产生重复 `key`**（因为 `key` 在文本里）。这是**可检测的**：
  投影时 batch 内重复 key 直接命中既有 rule 1（`plan.rs:417-422`）。
  按 §4 的原则，它不是错误，而是渲染成文档里可见的待办（"两个任务用了同一个
  key `impl-parser`，请改其中一个"），两个块都不进入可调度状态。
- **改 `key` = 删旧任务 + 建新任务**。这是正确的语义（改了身份就是换了任务），
  但要在 UI 上说清楚。

### 3.4 人的写口：块级 REST（NEW，机制缺口）

§0.2(a) 的结论是：不补写口，本设计的人机协作叙事全部落空。

裁决：**新增块级 REST，形状与 `calm.report.blocks.*` 一一对应**：

```
POST   /api/waves/{id}/report/blocks            { kind, markdown?|payload?, ifDocRev, position? }
PATCH  /api/waves/{id}/report/blocks/{block_id} { kind, markdown?|payload?, ifBlockRev }
DELETE /api/waves/{id}/report/blocks/{block_id} { ifBlockRev }
POST   /api/waves/{id}/report/blocks/{block_id}/move { toIndex, ifDocRev }
```

**两种 rev，各守各的（r1 通道 B MAJOR，成立）。** 初稿写 "`if_rev` 必填从第一天起"，
但**创建与位置性操作根本没有块级 rev 可守**——被守的块还不存在，或被改的是
`order` 列表而不是块本身。现状印证：`calm.report.blocks.upsert` 的 `if_rev`
只在带 `id` 时必填（`wave_report_blocks.rs:348-357`），不带 `id`（=创建）时
**完全没有并发保护**；`move` 的 `if_rev` 是可选的且不升 rev
（`:428`，`wave_report_doc.rs` 的 `move_block` 注释 "rev unchanged"）。
于是裁决：

| 操作 | 守什么 | 语义 |
|---|---|---|
| create（无 id）| **`if_doc_rev` 必填**（NEW）| 防"两个人同时往同一份文档追加块" |
| positional insert / move | **`if_doc_rev` 必填**（NEW）| `order` 是文档级结构 |
| update（有 id）| `if_block_rev` 必填（既有语义）| 块内容的 OCC |
| delete | `if_block_rev` 必填（既有语义，`:480-486`）| 块内容的 OCC |

`if_doc_rev` 直接复用 #979 落地的文档级 `doc_rev`（§0.3）——**这是本设计与 #979
唯一的真实耦合**，比初稿说的更强：初稿以为块级 rev 够用。同一裁决**同时适用于
MCP 侧的 `calm.report.blocks.*`**（今天的 create/move 无保护是一个既有洞，本设计
不该在它上面盖新楼）。

- 走同一个 `persist_report_with_shadow`（`wave_report.rs:404`）+ 同一组
  `ReportDocOp`，`author = EditAuthor::User`，`actor = ActorId::User`；
- 复用整文档路径已有的 `X-Calm-Actor: user` 严格闸（`routes/waves.rs:1245-1251`）；
- 冲突 → 409，复用 `RPC_REV_CONFLICT` 的语义（REST 侧映射为 409，与 #979 一致）。

**`task` 块的 DELETE 有特殊语义（§6.1 的死循环防线）**：`EditAuthor::User`
删除一个非墓碑的 `task` 块时，该 op 在**同一次编辑内**把块**替换为同 `key` 的
墓碑块**（原位置），而不是让块消失——执行者是 §3.7 的 `normalize_report_op`
（应用**前**的 op 改写器），不是校验器。删除墓碑块本身则是普通删除
（=人撤回否决）。理由见 §6.1。

**这不是给 `task` 开的特权口**——它对任何块 kind 都成立，只是本设计是它的第一个
真实消费者。它同时把整文档 `Replace` 从"人的唯一写口"降级为"粗粒度逃生口"，
`guard_non_prose_stomp` 因此可以保持原样不放松（这很重要：放松它等于让一次
无关的整文档重写有机会静默删掉一个正在执行的任务）。

### 3.5 执行粒度：默认 **不** 开子 wave（推翻 issue §3.5）

评审第 6 条说"一个 task → 一个子 wave 不是必然推论"，正确；并指出成本从未计算。
**现在算出来了**（全部读自代码）：

**一个 wave 的固定成本**（创建事务 `routes/waves.rs:587-690`，入口 `create_wave`
`:319` → `create_wave_with_spec_harness` `:567`）：

| 项 | 数量 | 依据 |
|---|---|---|
| `waves` 行 | 1 | `wave_create_tx` `db/sqlite/wave.rs:13`，INSERT 于 `:47-51` |
| `cards` 行 | **2**（spec 卡 kind=`codex`/role=Spec；wave-report 卡 role=ReportCard） | `routes/waves.rs:613-645` |
| `overlays` 行 | 1（kernel / view / layout） | `routes/waves.rs:661-674` |
| `cove_folders` 行 | 0 或 1 | `routes/waves.rs:598-607` |
| 创建时事件 | **4**（`WaveUpdated` + `CardAdded`×2 + `OverlaySet`） | `routes/waves.rs:676-688` |
| automerge 文档 | 1（`cards.body_crdt`，**wave-report 卡专属**，首次 persist 时惰性建立） | `migrations/0019_cards_body_crdt.sql:6-8,38`；`wave_report.rs:498-513` |
| 每次报告写的事件 | **2**（`CardUpdated` + `WaveReportEdited`，**无条件成对**，内容相同也发） | `wave_report.rs:560-585`、契约见 `:354-360` |
| `wave_vcs` | **每个 wave-scoped 事件批 = 1 个 commit** + 内容寻址对象；每 wave 一行 `wave_vcs_refs` | `db/sqlite/events.rs:138`、`wave_vcs/commit.rs:76-108`（仅 `WaveDeleted` 与排除名单不提交）；`migrations/0039_wave_vcs.sql:7-31` |
| spec harness 的伴生行 | `operations` ×1、`worker_sessions` ×1（`SharedSpec`/Codex）、`card_mcp_tokens` ×1，之后 `harness_items` 随 transcript 增长 | `spec_harness_start_adapter.rs:389-391,498,571`；`migrations/0045_worker_sessions.sql`、`0031_harness_items.sql` |
| **活的 spec agent** | **1 个常驻 in-process Tokio 任务**（`SpecHarness::run`，登记进 `HarnessRegistry`）+ **共享 codex daemon 上的 1 条 thread**（**不是**独立进程） | `spec_harness_start_adapter.rs:726,733,748`；`docs/architecture/410-shared-codex-daemon.md:7-24` |

一个 create → 一次报告写 → 完成的最简 wave ≈ **9–11 个事件、4–5 个 wave_vcs
commit、2 张卡、1 个 overlay、1 份 CRDT 文档**，外加上表的 saga/session/token 行。

对比 **在本 wave 内跑一个 task 的成本**：1 个 `tasks` 行 + 1 张 worker 卡 +
`TaskDispatched` / `TaskGateResult` 若干事件——**没有第二个 spec harness，
没有第二份 CRDT 文档，没有第二条 vcs 链**。

**一处必须说准的更正**：子 wave **不会**多开一个 OS 进程——自 #410 起全内核只有
**一个** `codex app-server`（`shared_codex_appserver.rs`，`410-shared-codex-daemon.md:19-24`
明写无 per-wave 回退路径）。所以子 wave 的边际成本不是进程，而是
**一条独立的 LLM 会话上下文（token 与延迟）+ 一个常驻 Tokio 任务 + 一整套
CRDT/vcs/session/token 行**。这仍然是数量级差异，但理由要说对。

而 issue 说它是"现有绑定模型的必然推论"——不是：绑定模型说的是
**一个 wave 一个 workflow**（`955-kernel-app-boundary.md` §3.3(a)），而本设计
恰恰把 workflow 拆成了模板（文档，可拼），单绑定的约束力因此下降。

裁决：**默认 `spawn: "in-wave"`（沿用今天的 scheduler → worker card 路径，
零改动）。`spawn: "sub-wave"` 是显式选项**，受 §8 的树级预算与深度上限约束。
可证伪：若实测中 in-wave 执行导致父报告被 worker 产出淹没到无法阅读，
或 per-wave `task_budget` 默认 1（`scheduler/mod.rs:80`）成为吞吐瓶颈且无法通过
调 budget 解决，则该裁决被推翻。

### 3.6 状态回显：读时合成，不进文档

`task` 块渲染时，前端/读路径把 `tasks` 行的状态（status / status_detail /
gate_result / worker_card_id）贴在块上。这是**读时**行为，不产生文档写、不产生
第三个写者（§7 非目标）。

issue §3.1 说这与 #976 活数据块"同构、应共用一套机制"——§0.1 #16 已说明 #976
不在代码里，**本文不对共用机制作承诺**，只作一条弱约束：`task` 的状态回显
**不得引入任何新的读时数据源抽象**，就是投影表的直读，这样 #976 落地时无论
选什么形状都不会与它冲突。

### 3.7 `guard_task_declarations`：所有写路径的唯一收口（NEW，r1 新增）

§0.2(a′) 的事实——**`write_markdown` 不受 stomp guard 约束，spec 今天就能任意
新建/改写/删除任何非 prose 块**——把两条本设计最重要的规则从"入口校验"逼成了
"收口校验"：

- §4.4 的归因（`declared_by` 必须等于本次编辑的 `EditAuthor`）；
- §6.1 的不对称（AI 不得删/改人声明的任务）。

如果只在 `calm.report.blocks.upsert` 里校验，spec 换一个 `write_markdown` 就绕过了。

裁决：**收口是两个函数，不是一个**（r2 双通道 BLOCKER，成立，改设计方向）。
r1 写的是单个 `guard_task_declarations(before, after, author) -> Result<(), CalmError>`
并让它同时承担规则 4 的"删除 → 原位墓碑"。**这做不到**：返回 `Result<(), _>`
的东西是**校验器**，只能接受/拒绝；而规则 4 是一次**变更**（把一个删除改写成一次
upsert）。而且 `apply_report_op` 的每个分支在被调用时**已经把 `doc` 改完了**
（`wave_report.rs:131-215`：`Replace` 在 `:171` 就 `doc.update(...)`），
校验器在那之后拿到的是既成事实。所以拆成两步：

```rust
/// 应用**前**的 op 改写器。唯一职责：把人的 `DeleteBlock`（目标是非墓碑
/// `task` 块）改写成同 `key` 的墓碑 upsert。其余 op 原样返回。
/// 需要读 doc（判断目标块的 kind / payload / rev），但不改 doc。
fn normalize_report_op(
    doc: &ReportDoc,
    op: ReportDocOp,
    author: EditAuthor,
) -> Result<ReportDocOp, CalmError>;                   // NEW

/// 应用**后**的校验器，跑在前后态块快照上。只接受/拒绝。
fn guard_task_declarations(
    before: &[ReportBlock],
    after: &[ReportBlock],
    author: EditAuthor,
) -> Result<(), CalmError>;                            // NEW
```

**接线**：`apply_report_op`（`wave_report.rs:132`）新增 `author: EditAuthor` 参数
（NEW，一处签名改动），函数体变成
`let op = normalize_report_op(doc, op, author)?;` → `let before =
doc.blocks_snapshot()?;` → 既有的 match 分支 → `let after = doc.blocks_snapshot()?;`
→ `guard_task_declarations(&before, &after, author)?`。**对每一个 op 变体都跑**，
`Replace` / `WriteMarkdown` / `UpsertBlock` / `DeleteBlock` / `MoveBlock` 无一例外。
这是所有写路径的唯一收口（`persist_report_with_shadow` 内、事务里、能看到 CRDT
真相），拒绝即整事务回滚、不发事件——与既有两道 guard 完全同构。
（代价如实记：每个 op 多两次 `blocks_snapshot()`，与 `guard_non_prose_stomp`
今天已经在做的那次同数量级；`Replace` 路径上可与 stomp guard 共用一次快照。）

**规则**（违反 → `CalmError::BadRequest`，MCP 映射 `-32602`、REST 映射 400）：

| # | 落在哪 | 规则 | 为什么 |
|---|---|---|---|
| 1 | 校验器 | 新出现的 `task` 块，其 `declared_by`（墓碑块则是 `tombstoned_by`，两者在新块上同时受约束）**必须**等于本次编辑的 `EditAuthor`：`Spec`→`"spec"`、`User`→`"user"`；**其它任何 author（`Kernel` / `Plugin`）新建 `task` 块 → 一律拒绝**（fail closed）| 否则 AI 写 `declared_by: "user"` 就逃掉了 §8 的树预算。`Kernel`/`Plugin` 今天没有生产发射点（`event.rs:161,168` 注释 "no emitter today"；§4.4 已核实），但 guard 不能在其中之一回归时行为未定义 |
| 2 | 校验器 | 已存在的 `task` 块，`declared_by` **不可改**——**含"任务块转为同 key 墓碑"这一次转变**（r3 ⑨：墓碑承接原块的 `declared_by`）| 归因是不可变的声明属性：「谁提出要做这件事」不因「谁否决了它」而改变 |
| 2b | 校验器 | 已存在的墓碑块，`tombstoned_by` **不可改**；且墓碑块**不得原位改回非墓碑**（撤回 = 删除该墓碑块，§6.1）| 否则 spec 一次原位改写就能把人的否决变成自己的任务，绕过规则 3 |
| 3 | 校验器 | **`author != User`** 时，**不得删除、也不得修改**任何满足 `declared_by == "user"` **或** `tombstoned_by == "user"` 的 `task` 块 | §6.1 的不对称，机制化。**第二个析取项是 r3 ⑨ 必需的补丁**：人否决 spec 声明的任务后，墓碑的 `declared_by` 仍是 `"spec"`，只靠第一个析取项，spec 可以直接删掉那块墓碑再重提——死循环原地复活。**主语在 r4 从 `author == Spec` 改成 `author != User`**（通道 A MINOR，成立）：规则 1 对 `Kernel`/`Plugin` 在**新建**方向已明确 fail closed 并写明理由"guard 不能在其中之一回归时行为未定义"，同一理由在**删除/改写**方向此前没有被应用 ⇒ `EditAuthor::Kernel`（`wave_report.rs:330-331` 逐字确认它今天 "has no caller today and is reserved for future server-internal rewrites"）或 Plugin 通道（#978 已撤回，`532eed6c`）一旦回归，删除人的墓碑就是**未定义行为**。改成 `!= User` 后两条规则同一形状 |
| 4 | **改写器** | `author == User` 且 op 是 `DeleteBlock{id}`、目标是**非墓碑** `task` 块 → 改写为 `UpsertBlock{ id, kind:"task", content: {key: <原块的 key>, tombstone:{reason:null}, declared_by: <原块的 declared_by，原样承接>, tombstoned_by:"user"}, if_rev }`（原位，`if_rev` 从原 op 承接）| §6.1 的死循环防线必须落在**默认路径**上。这个 payload 是 §3.2 规则 7 的封闭形态，**过得了同一次 persist 里的 `validate_payload`**（r2 的形状过不了：缺必填的 `kind`，且改写 `declared_by` 会撞规则 2）|
| 4′ | 校验器 | `author == User` 时，`before` 里任何**非墓碑** `task` 块若在 `after` 中消失，则 `after` 里**必须**存在同 `key`、且 `tombstoned_by == "user"` 的墓碑块；否则拒绝，错误文案指向块级 DELETE 端点 | 规则 4 只覆盖块级 DELETE；整文档路径必须 fail closed 而不是静默放行 |
| 5 | 校验器 | `released_by_user`（§6.6）**只有 `author == User` 能写入或改变**。`author == Spec`（及其它任何 author）的写：新块上该字段必须缺席或 `false`；已存在块上该字段必须与 `before` **逐字节相同**，否则拒绝 | ⑪ 的放行位必须是**人可写、spec 不可写**的独立载体。`declared_by` 做不了这件事（规则 2 冻住它），`ready` 也做不了（spec 写的块上它本来就是 `true`，"人改成 true"是空操作）|

**为什么墓碑权属要独立成 `tombstoned_by`（r3 两个通道交叉命中的 BLOCKER，改设计方向 ⑨）。**
r2 的形状在**默认路径上必然 400**：被删的块通常是 `declared_by:"spec"`
（issue 点名的场景就是"人删 AI 声明的任务"），规则 4 原位写入
`declared_by:"user"`，规则 2 随后看到"已存在的块的 `declared_by` 变了"→
`BadRequest`。**人的每一次删除都被拒绝**，而 §6.1 的整条死循环防线全建在这条
路径上。三个候选形状，逐条比：

| 形状 | 做法 | 判定 |
|---|---|---|
| (a) 规则 2 开一个豁免 | 允许"非墓碑 task → 同 key 墓碑"这一种 `declared_by` 转变 | **驳回**：把一条全称不变量（归因不可变）降级成带例外的条件规则，此后每一处读 `declared_by` 的代码都要问"它是原始归因还是被墓碑改过的"；§8 的预算与 §4.4 的"谁提出要做这件事"语义同时被污染 |
| (b) 规则 4 改成 Delete + Insert | 墓碑成为"新出现的块"，落在规则 1 上 | **驳回**：`normalize_report_op` 返回**单个** `ReportDocOp`，拆成两个 op 要么新增复合 op、要么两次 persist（后者破 §3.7 的"不产生额外持久化往返"判据）；更根本的是"新块 vs 已存在块"的判定依赖对齐器的 id 铸造（`align.rs:352-364` 的内容哈希 + 冲突探针），把一条安全规则挂在启发式对齐上，正是 §3.3 已经拒绝过的做法 |
| **(c) 独立的 `tombstoned_by`** | `declared_by` 原样承接，新增不可变的 `tombstoned_by` = 本次 `EditAuthor`；防线改判 `tombstoned_by` | **采纳**（通道 B 的建议）。规则 2 保持**无例外**的全称形式；两个问题（谁提出 / 谁否决）由两个正交字段各自回答，与 §4.4"是谁提出要做这件事"与"最后谁改了措辞"的既有切分同构；代价是一个字段 + 规则 3 多一个析取项 |

采纳 (c) 之后，全文凡以"墓碑是谁立的"为判据处（§6.1 的终局性、§11.2 不变量 6、
§3.7 规则 3）**一律读 `tombstoned_by`**，凡以"任务是谁提的"为判据处
（§8 的预算、§4.4、§6.2）**一律读 `declared_by`**。两者不再互相顶替。

**整文档路径逐条说清楚**（r2 通道 A/B 共同要求）：

| 写路径 | 谁在用 | 人删一个 `task` 块会怎样 |
|---|---|---|
| `ReportDocOp::Replace` | **人的整文档编辑器**（`routes/waves.rs:1207`）、`calm.report.write/edit` | **400，不是墓碑。** `guard_non_prose_stomp`（`wave_report_guard.rs:58-80`，逐字复核）遍历 `current` 里每个非 prose 块，要求对齐后 id/kind/canonical fence 三者逐字节保持——删掉一个 `task` fence 直接撞它。**所以规则 4 在 `Replace` 上不可达**，规则 4′ 也够不到（stomp guard 先拒）。**UI 必须用块级 DELETE 端点删 `task` 块**；stomp guard 的错误文案要增补一句指路（NEW，一行） |
| `ReportDocOp::WriteMarkdown` | **只有 Spec**（`calm.report.write_markdown`）| 人到不了这条路径。spec 删自己的 `task` 块 = 普通撤回（不物化墓碑——§6.1 已裁定 spec 墓碑本来就不是防线）；spec 删 `declared_by:"user"` **或** `tombstoned_by:"user"` 的块 → 规则 3 拒。若将来 `WriteMarkdown` 对 `User` 开放，规则 4′ 会把"整文档写里凭空少了一个人的 task 块"拒掉 |
| `UpsertBlock` / `DeleteBlock` / `MoveBlock` | 块级 MCP 工具 + §3.4 的块级 REST | `DeleteBlock` 是**唯一**会物化墓碑的路径（规则 4） |

**这条如实记的后果**：`task` 块一旦落进文档，人在 markdown 编辑器里删不掉它，
只能在块 UI 上删。这不是新增的限制（§0.2(a) 已证今天就是如此），但本设计**依赖**
它——放松 stomp guard 等于让一次无关的整文档重写有机会静默删掉一个正在执行的任务。

**规则 4 会不会让内核变成文档写者（破 §7 非目标）？** 不会，且区别是实质的：
内核不是**自发**地写，而是把**用户自己的一次 op** 展开成两步，`author` 仍是
`EditAuthor::User`，`actor` 仍是 `ActorId::User`，事件仍是那一对
`CardUpdated` + `WaveReportEdited`。这与"内核订阅事件后另起一次写"（#973/#978
拆掉的那种）在因果上完全不同。**判据**：内核在文档里写的每一个字节，都必须能
归因到本次人/AI 编辑，且不产生额外的持久化往返。规则 4 满足；proposal 通道不满足。

**代价（如实记）**：这两个函数让 `apply_report_op` 从"对文档形状的校验"变成
"op 改写 + 对文档形状 + 写者身份的校验"。这是本设计给既有写路径引入的最重的
一处改动，风险记在 §13.3。

---

## 4. 投影：契约与重建

### 4.1 同事务投影（沿用 issue §3.2，成立）

`persist_report_with_shadow`（`wave_report.rs:404`）今天已在**同一个事务**里做：
load CRDT → project 前态 → apply op（`if_rev` 在此校验，调用点 `:527`）→
project 后态 → 写 `cards.payload` + `cards.body_crdt`
（`card_update_with_crdt_tx`，`:565`）→ 发 `CardUpdated` + `WaveReportEdited`（`:576-584`）。

**扩展点**：在 **`:542`**（`projected_payload.blocks = Some(doc.blocks_snapshot()…)`，
即拿到块快照的那一行）与 `:565`（卡写入）之间插入 `project_tasks_tx`。
（r1 修正：初稿写 `:556`，那一行落在 `CardPatch` 构造中间；`:550` 才是
`let patch = CardPatch {`。区间是 **542–565**。）理由与 issue 一致——异步 reconcile 会引入一整类
"文档改了但 plan 没跟上时调度器读到旧值"的 bug，同事务直接消灭；且这条路径
**已经**在同事务里做投影（`cards.payload` 就是 CRDT 的投影），这是同构扩展不是新机制。

### 4.2 投影函数契约（NEW）

```rust
/// 纯函数：从报告的块快照算出这份文档声明的任务集合 + **块局部**诊断。
/// 无副作用、不读 DB —— 这样它能被单测穷举，也能被 rebuild 复用。
/// **它只回答"这份文档声明了什么"，回答不了"这条能不能排"**（r4 ⑭）。
fn project_task_declarations(
    blocks: &[ReportBlock],
) -> (Vec<TaskDeclaration>, Vec<Vec<Diagnostic>>);   // NEW

/// **唯一的可调度判定**（NEW，r4 通道 A MAJOR，改设计方向 ⑭）。
/// 在一个事务（读或写皆可）内，把纯函数的块局部诊断与四类**需要 DB**的诊断
/// 合并，给出每个块的完整诊断与可调度性。写路径（`project_tasks_tx`）与
/// 读路径（`taskDiagnostics`）**调的是这同一个函数**，不是两份实现。
///
/// 需要 DB 的四类（逐条已核实，见规则 3′）：跨 cove 引用、`unknown_deps`
/// （第二个入参是从该 wave 的*在飞*行派生出的已知 task key 列表——
/// `status IN ('dispatched','running','verifying')`，见规则 3‴）、
/// `spec_task_ceiling` 超限（**不是**裸 `count(*)`：对声明集合的确定性准入，
/// 规则 3″）、`declare-and-wait` 未放行（`effective_policy` =
/// `waves.automation_policy` 列 + 文档里的 `tombstoned_by:"user"` 墓碑，§6.6）。
async fn evaluate_schedulability_tx(
    tx: &mut Transaction<'_, Sqlite>,   // 读事务亦可
    wave_id: &WaveId,
    decls: &[TaskDeclaration],
    block_local_diags: &[Vec<Diagnostic>],
) -> Result<Vec<BlockVerdict>>;                      // NEW
// BlockVerdict = { diagnostics: Vec<Diagnostic>, schedulable: bool }

/// 事务内 upsert：把声明落到 tasks 表。声明列全量覆盖，状态列一律不碰。
async fn project_tasks_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    declarations: &[TaskDeclaration],
    author: EditAuthor,
) -> Result<TaskProjectionOutcome>;                  // NEW

/// 重建：从当前文档重放一遍投影。**语义 = "在同一份文档上重跑一次增量投影"**，
/// 不是 "把 tasks 表重置成 ready 声明集合"（见 §11.1 的修正）。
/// 先例：`proposals_rebuild_tx`
/// （已随 #978 的 ④ 撤回删除，commit `532eed6c`；最后含它的树是
///  `532eed6c^` = `6d8e3591`，定义在
///  `crates/calm-truth/src/db/sqlite/proposal.rs:179`，签名
///  `async fn proposals_rebuild_tx(tx) -> Result<()>`，形状是
///  「DELETE 投影表 → 按 id 升序重放真源 → 逐条 apply → 损坏即 fail-loud」。
///  本函数镜像它，唯一差异是真源从事件日志换成文档）。
async fn tasks_rebuild_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<RebuildDiff>;                            // NEW
```

**投影的七条硬规则**（r1 后重写：规则 1 与 3 初稿都不成立）：

1. **写声明列 + 恰好两处由声明决定的行存在性变化**（§2 的形状，r2 改写）。
   声明列：`kind / goal / context_json / acceptance_criteria / cwd /
   depends_on_json / priority / gate_json / declared_by / origin`。
   **绝不触碰**：`status_detail / worker_card_id / gate_result_json /
   gate_attempt / gate_pid* / running_deadline_ms / finished_at_ms`。
   `status` **只被写一次**：新行创建为 `pending`。除此之外声明面**从不写
   `status`**；它只会**删除**一个仍是 `pending` 的行。**触发删除的谓词只有一个**
   （r3 ⑫ 合并，见规则 4）：**该 key 在当前文档里不再被声明为"可调度"**
   —— 即不满足「存在非墓碑 `task` 块 ∧ `ready == true` ∧ 该块诊断为空
   ∧（`declare-and-wait` 时）放行位已置」。**这个谓词由
   `evaluate_schedulability_tx` 唯一实现**（规则 3′、r4 ⑭），
   增量、rebuild、读端诊断三条路径调的是同一份代码。它涵盖四种情形：
   块被删除 / 被墓碑覆盖 / `ready` 从 `true` 撤回 / **该块新产生了诊断**。
   执行：`task_delete_pending_tx`（NEW，`DELETE … WHERE id=?1 AND status='pending'`，
   单赢家守卫）。行已非 `pending` → 一律不碰（§6.5）。
   **0 行返回不是错误**：它意味着这一行在读投影与删除之间被 scheduler 认领了
   （`task_claim_pending_tx` 同样 `WHERE status='pending'`，`task.rs:222`
   ← `scheduler/mod.rs:641`）；按 §6.5 处理——不改状态、产出"正在执行，
   无法立即撤回"的诊断。这与 `calm.plan.cancel` 对 0 行的消歧同构
   （`task_get_tx` 复读，`task.rs:130-131`）。
   *（r1 用的是 `task_cancel_tx`（`pending → canceled`）。r2 两个通道同时证明
   那会让 `key` 被 `canceled` 行永久吸收、并让 rebuild 与增量分叉；理由与三条
   论证见 §2。`ready` 撤回也走同一条删除规则——否则 rebuild（它只看当前 ready
   声明集合）与增量路径会在这一点上再次分叉。）*
2. **非 pending 行的声明变更 → stale 诊断 + 失效裁决，不写入、不报错。**
   `calm.plan.upsert` 在这里 `return Err("task X already dispatched; insert a
   new task instead")`（`plan.rs:451-463`，逐字复核）。**文档路径不能这么做**
   ——那会让一次文档编辑被整体拒绝，违反"永不拒绝合并"。
   **这是本设计与 `calm.plan.upsert` 唯一的行为分歧，且方向是更严不是更松**：
   MCP 路径拒绝整批（什么都没写），文档路径接受文档、拒绝那一行的声明更新
   （那一行同样什么都没写）。两边**落到 `tasks` 表的效果完全一致**。
   **但"只出一条诊断"不够**（r2 通道 A MAJOR，成立）：issue 点名的阻塞需求是
   「已被认领的 task，其声明变更必须让 claim 失效」。诊断是给人看的，不改变
   worker 的行为。所以规则 2 的完整形状是**两件事同时发生**：
   (i) 声明列不写 + 产出 stale 诊断；
   (ii) **因为 task 块自身在它自己的冻结闭包里**（§5.1），这次编辑必然被第 1 级
   机械检测捕获 → 走 §5.3 的裁决 → 判 `material` 则该 task 不再产生新的
   `TaskDispatched`（§11.2 不变量 5）。**规则 2 与 §5.3 是同一件事的两半**，
   不是两条独立机制。
2b. **`key` 的复活规则（r2 通道 A BLOCKER 的正面表述）。** 一个
   `(wave_id, key)` 在 `tasks` 表里**没有行**时，投影对该 key 的新声明一律按
   "新行创建"处理，与它历史上是否存在过无关。规则 1 的删除保证了"从未派发过的
   任务被撤回后不留残骸"，于是「删任务 → 墓碑 → 删墓碑 → AI 重提」这条循环
   **真的**终止在"空"上：删任务 → 墓碑块 + 行被删；删墓碑 → 文档空、表空；
   重提 → 干净地建一条新 pending 行。
   **已派发过的 key 不复活**：`dispatched` 及之后的行留在表里（§11.1 的代价段），
   同 key 的新声明命中规则 2 的 stale 路径。这是对的——那个 `tasks.id` 是
   operation 幂等键（`plan.rs:580` → `dispatcher/mod.rs:180`），复用它等于让
   两段互不相干的执行史共享一个幂等键。UI 的说法：「`impl-parser` 已经跑过了，
   要重做请换一个 key」——与 `calm.plan.upsert` 今天的文案一致。
3. **规则复用的诚实表述**（r1 通道 A MAJOR，成立）。初稿写"校验沿用
   `resolve_plan_batch`，一条都不新写"——**做不到**：
   `resolve_plan_batch`（`plan.rs:412-484`）的签名是
   `fn(&[Task], &[NormalizedTask]) -> Result<Vec<PlanOutcome>, String>`，
   四类违规各自 `return Err(...)`、**整批失败**；而投影要的是**逐块诊断**。
   两者不可能是同一个函数。真实做法：

   - 把四条规则各自抽成 **calm-types 里的纯谓词**（NEW）：
     `dup_keys(&[Decl]) -> Vec<Key>`、
     `unknown_deps(&[Decl], known_task_keys: &[String]) -> Vec<(Key, Dep)>`
     （第二个入参是从**在飞行**派生出的已知 key 列表，规范约束见规则 3‴；
     不能写成 `&[Task]`，因为 `Task` 定义在 `calm-truth`，底层
     `calm-types` 依赖它会把 crate DAG 反过来）、
     `find_cycle(&BTreeMap<..>) -> Option<Vec<String>>`（从 `plan.rs:524` **移动**过去）、
     `gate_rule_violations(&[Decl], require_gates: bool) -> Vec<Key>`；
   - **`resolve_plan_batch` 重构为调用同一批谓词**，只保留"首个违规 → `Err`"的
     聚合逻辑。于是每条规则**确有唯一实现**，差的只是聚合器；
   - **等价性测试（这条断言的机制保证）**：属性测试，对随机生成的批次 `B`，
     断言
     `resolve_plan_batch(existing, B).is_err()`
     `⟺ project_task_declarations(blocks_of(B)) 的诊断集合非空`，
     **规则 2 那一类除外**（唯一已知且已论证的分歧，见上）。
     这条测试是"投影不得比 `calm.plan.upsert` 松"从散文变成机制的地方。
3′. **谓词分两层，读端与写端共用第二层**（NEW，r4 通道 A MAJOR，成立，
   改设计方向 ⑭）。上面那批谓词里**只有一部分是纯的**，而 r3 的文本把
   "可调度"整体说成了"当前文档的纯函数"——那句话**在它自己内部就是假的**：

   | 诊断 | 需要什么 | 落在哪 |
   |---|---|---|
   | payload schema / `goal`·`acceptance` 非空 / `gate_rule_violations` | 只需本块 | 纯函数 `project_task_declarations` |
   | `dup_keys` / `find_cycle` | 只需本文档的全部块 | 纯函数 |
   | **`tombstoned_keys`：同 `key` 存在未清除的墓碑块**（NEW，r6 通道 A MINOR）| 只需本文档的全部块 | 纯函数 |
   | `unknown_deps` | 从**该 wave 的*在飞*行**派生出的已知 task key 列表 `&[String]`（`status IN ('dispatched','running','verifying')`；**不是**全部非终结行——规范约束见规则 3‴，r7）| `evaluate_schedulability_tx` |
   | 引用越 cove | **DB**（§5.1 明写"执行点在 `project_tasks_tx`，纯 `validate_payload` 看不到 cove"） | 同上 |
   | `spec_task_ceiling` 超限 | **一次 `SELECT count(*)`（只数**在飞行**）+ 对声明集合的确定性准入**（规则 3″、§8(A)；**不是**裸 `count(*)`，也**不数 `pending` 行**）| 同上 |
   | `declare-and-wait` 未放行 | **`effective_policy(wave)`** = `waves.automation_policy` 列 + 文档里有无 `tombstoned_by:"user"` 的墓碑（§6.6 的派生规则，r5 ⑰；两个输入这里本来都有） | 同上 |

   **墓碑那一行是 r6 补的（通道 A MINOR，成立）**：§11.2 不变量 6（死循环的机械
   防线）整条压在"同 `key` 存在未清除墓碑 ⇒ 该声明不可调度"上，§6.1 也以它为
   机制面；而 r5 的这张表——**谓词的唯一枚举**——里没有它。§12 切片 3b 的交付
   清单确实列了它（"墓碑投影 + 同 key 重声明的拒绝"），所以这不是空洞断言，
   但**谓词自己的枚举漏掉了不变量 6 所依赖的那一项**。它与 `dup_keys` 同类：
   **整文档的纯谓词**（墓碑块与任务块都在同一份块快照里），因此落在
   `project_task_declarations` 这一层，不需要 DB。

   **后果如果不修**：§4.2 规则 1/4 会**删掉一条 pending 行**，而读端
   `taskDiagnostics`（当时定义为"按需调用纯函数"）**渲染不出原因**——
   行没了、原因看不见。这直接违反 §5.1 的"不是静默丢弃"、§8(A) 的
   "不落 pending 行 + 产出诊断"、§6.6 的"本 wave 要求人确认后才排队"，
   以及 §4 的"永不拒绝合并，只降级 + **可见**待办"。裁决：
   **读端是一次读事务内的派生调用**（`project_task_declarations` +
   `evaluate_schedulability_tx`，与写路径同一实现），不是纯函数调用。
   代价：`GET .../report` 多一个只读事务与一次 `count(*)`——与该端点今天
   已经在做的读同数量级，仍然零存储零缓存零事件（规则 7 不变）。
3″. **`spec_task_ceiling` 必须是对声明集合的确定性准入，不能是 `count(*)`**
   （NEW，r5 通道 A MAJOR + 通道 B MAJOR 交叉命中，成立，改设计方向 ⑯）。
   r4 用"三条路径调同一份代码"来保证不分叉。**那句话只证明了三个调用点算出
   同一个*函数*，没有证明那个函数*幂等*** —— 而 §8(A) 指定的谓词
   （`SELECT count(*) … WHERE wave_id=? AND declared_by='spec' AND
   origin='block' AND status NOT IN (…)`）**把正在被重新求值的那些行也数了
   进去**。具体化到 `spec_task_ceiling = 2`：

   | 步 | 发生什么 | `count(*)` | 结果 |
   |---|---|---|---|
   | 1 | 增量投影落地 `k1` | 0 → 落行 | {k1} pending |
   | 2 | 增量投影落地 `k2` | 1 → 落行 | {k1,k2} pending |
   | 3 | 同一份文档上再声明 `k3`（或任何一次 `tasks_rebuild_tx`／读端求值）| **2 ≥ ceiling** ⇒ **k1、k2、k3 一律超限** | 三者皆不可调度 |
   | 4 | §4.2 规则 1 对"不可调度"执行守卫式删除 | —— | k1、k2 的 pending 行**被删掉** |
   | 5 | 下一次编辑重新求值 | 0 | k1、k2 **又落地** |

   这既是 **rebuild ≢ 增量**（破 §11.1 的等号与 §11.2 不变量 11），也是普通编辑
   之间的**抖动**；顺序依赖是同一枚硬币的另一面（哪些声明能落地成了编辑历史的
   函数，而 rebuild 看不见历史）。通道 B 从**诊断可渲染性**的角度命中同一处：
   写事务删行之后，读事务看到的 DB 状态已经不同，于是"调同一个函数"仍然可能
   渲染不出那条 ceiling 诊断。

   **r5 的裁决方向是对的，但它的排除集选错了，两个通道在 r6 又从两个方向
   各命中一次（⑳）。** r5 写的是：`D` = 当前文档里 `declared_by == "spec"`、
   `origin` 将为 `'block'`、且**其余所有诊断均为空**的 key 集合；
   `occupied` = 该 wave 内非终结、**`key ∉ D`** 的行数。两个反例：

   - **（通道 B）"其余诊断均为空"这个限定把幂等性又漏了回去。** 一个**当前仍被
     声明、但带别的诊断**的 key 不在 `D` 里 ⇒ 它那条 `pending` 行被数进
     `occupied`，可能因此拒掉一条本来合法的声明；而**同一次求值**里规则 1/4
     又会把那条 pending 行**删掉** ⇒ 下一次求值的 `occupied` 少 1，答案不同。
     这正是 ⑯ 要消灭的那个形状，只是搬了个家。
   - **（通道 A）反向的漏洞：在飞的行被排除出计数。** 一个仍被声明、诊断为空的
     **`dispatched`** 行**在 `D` 里** ⇒ 不计入 `occupied`。构造：`ceiling = 1`、
     `k1` 已 dispatched 且仍被声明，把 `k2` 的块插到 `k1` **之上**（准入按块序）
     ⇒ `occupied = 0`、`capacity = 1`、`k2` 准入 ⇒ 该 wave 出现 **2** 条非终结
     的 spec/block 行，**§11.2 不变量 7b 当场为假**。超额被该 wave 的
     `task_budget` 封顶（in-flight ≤ budget，`compute_ready`
     `scheduler/mod.rs:164-191`），所以它是**上界/措辞缺陷而不是失控**——但 7b
     是切片 3b 的验收测试，照 r5 的写法会红。

   **裁决（r6，⑳）：不按"诊断"分集合，按"这一行是不是本次求值的产物"分。**
   这条判据是结构性的，而且它同时消灭上面两个反例：

   > **`pending` 行永远是输出，在飞行永远是输入。**

   逐条展开——注意这里**没有 `D` 了**，排除集就是"全部 `pending` 行"：

   1. **`occupied` = 该 wave 内 `declared_by='spec' AND origin='block'` 且
      `status IN ('dispatched','running','verifying')` 的行数。**
      不看它的 `key` 是否仍被声明、也不看它有没有诊断——这些行**不是本次求值
      能改变的东西**（规则 1 的守卫式删除只赢 `pending`，`task.rs:222` 的
      同一条 `WHERE status='pending'`），所以它们是纯输入。
   2. `capacity = max(ceiling − occupied, 0)`；
   3. 令 `A` = 当前文档里 `declared_by == "spec"`、`origin` 将为 `'block'`、
      **且其余所有诊断均为空**的 key 集合（"只差 ceiling 这一关"的候选）。
      把 `A` 按**块在文档中的顺序、同序时按 `key` 升序**排列，取前 `capacity`
      个准入（两条路径拿到的是同一份块快照，所以这个顺序对增量、rebuild、
      读端三者逐位相同）；
   4. 其余的 key 得到 ceiling 诊断 + 不可调度。

   **为什么"pending 行不计数"是安全的而不是放水**：本次求值之后，该 wave 内
   **所有**非终结的 spec/block 行只有两种来源——(a) 第 1 步数过的在飞行
   （本次不动它们），(b) 第 3 步准入的那些（≤ `capacity`）。**其余每一条
   pending 行都必然在本次事务里被删掉**：key 不再被声明 → 规则 1 删；仍被声明
   但有诊断 → 规则 4 删；仍被声明、诊断为空但未获准入 → 拿到 ceiling 诊断，
   同样走规则 1/4 删。没有第四种。所以把它们计入 `occupied` 就是"数一批马上要
   消失的行"——那正是 ⑯ 与本轮通道 B 指出的不幂等来源。

   **性质**（这四条就是它相对 `count(*)` 与相对 r5 写法的全部价值）：
   (i) **幂等**——输入 =（文档块快照，该 wave 的在飞行集合），两者都不含本次
   求值正在创建/删除的任何东西，固定文档上重复求值逐位相同；
   (ii) **rebuild ≡ 增量**——`tasks_rebuild_tx` 不动在飞行（§11.1(3)"所有存活行
   的全部状态列逐字节不变"），所以两条路径的输入相同；
   (iii) **诊断稳定可渲染**——读端用同一构造算出同一份准入表，被拒的 key 与原因
   在删行之后仍然渲染得出来；
   (iv) **上界成立且可陈述**——见下。

   **上界的精确形式（§11.2 不变量 7b 随之重述）。** 一次求值之后：
   `非终结行数 = occupied + |准入| ≤ occupied + max(ceiling − occupied, 0)
   = max(ceiling, occupied)`。而 `occupied` 只在"某条被准入的 pending 行被
   claim"时增长 1，同时 `pending` 侧减 1 ⇒ **总数不变**。所以：

   > **在该 wave 的 `spec_task_ceiling` 从未被调低的前提下，该 wave 内
   > `declared_by='spec' AND origin='block'` 的非终结行数恒 ≤ `spec_task_ceiling`。**
   > 若人把 ceiling 调低到当时的在飞行数以下，上界暂时退化为**调低那一刻的
   > 在飞行数**，并随这些行终结单调收敛回新 ceiling——**期间不会准入任何新行**
   > （`capacity = 0`）。

   这是**能被证明的那条**，而不是 r5 那条无条件全称句。§11.2 不变量 11 的生成器
   必须覆盖"声明数跨过 ceiling"的编辑序列 **以及"在飞行仍被声明时新块插到它
   之上"这个构造**；§12 切片 3b 的验收补一条**删行之后 ceiling 诊断仍可读**的
   断言，以及一条**通道 A 反例的回归测试**。
3‴. **`unknown_deps` 的第二个输入规范地只含*在飞*行**（NEW，r7 通道 A MINOR，
   成立）。这条**不是新决定**——§11.1(1) 在 r6 的 ⑳ 里已经把可调度谓词的第三个
   输入收窄为「同 wave 的**在飞** tasks 行」，理由是「`pending` 行是这个函数的
   **输出**，不能同时是它的输入」。但 §4.2 这一侧（上面的契约签名注释与规则 3′
   的表）当时仍写着裸 `&[Task]`，于是**同一个谓词在本文的两处有两个不同的
   规范**。规则 3‴ 把 §11.1 的那条收窄写进 §4.2，让它只有一处定义：

   > **`unknown_deps` 的第二个入参 = 从该 wave 内
   > `status IN ('dispatched','running','verifying')` 的行派生出的 key 列表。
   > `pending` 行的 key 一律不在其中；终结行本来也不在。**

   **不修会怎样（⑳ 的失败类在另一个诊断上原样重现）**：ceiling = ∞、
   声明 `k1` 与 `k2`（`depends_on: [k1]`），两者都已落 `pending` 行；
   一次编辑删掉 `k1` 的块。**同一个事务里**：
   - 若第二个入参是"全部非终结行"，求值 E1 仍看得见 `k1` 那条 `pending` 行
     ⇒ `k2` **无诊断、存活**；而同一事务按规则 1 把 `k1` 的 pending 行**删掉**；
   - 在**同一份文档**上的下一次求值 E2（或一次 `tasks_rebuild_tx`、或读端
     `taskDiagnostics`）看到的是 `k1` 无行 ⇒ `k2` 得 `unknown_deps` ⇒ 被删。

   ⇒ **rebuild ≢ 增量**（破 §11.1 的等号与 §11.2 不变量 11），且读端渲染出的
   诊断与写路径刚刚做过的决定**互相矛盾**。收窄之后两次求值的输入相同，
   等号恢复——与规则 3″ 的 `occupied` 是**同一条判据的同一次应用**：
   **`pending` 行永远是输出，在飞行永远是输入。**

   **正确的后果如实定价（这是收窄真正的代价，不是副作用）**：
   一条 `depends_on` 指向一个**当前文档没有声明、但在 `tasks` 表里有 `pending`
   行**的 key，**从此得到 `unknown_deps` 诊断**（此前它静默通过）。两类：
   - 该 key 的块刚被删 / 被墓碑覆盖 / `ready` 撤回 ⇒ 那条 pending 行本来就在
     本次事务里要被删——**诊断说的是真话**，此前的"通过"才是错的；
   - **`origin='legacy'` 的 pending 行**（§9 的存量，尚未被物化成块）
     ⇒ 一条指向它的 `depends_on` 现在拿到诊断。这是**保守的那一侧**且与
     §9 的物化顺序一致：legacy 行没有块，就不该被文档里的声明依赖；
     物化（§12 切片 7）之后它成为正常声明，诊断自然消失。
     **§12 切片 3b 的迁移验收必须专列这一条**，免得存量库在上线当天出现
     一批"突然多出来的诊断"而无人预期。
   *（依赖**已派发**的任务照旧成立——`dispatched/running/verifying` 正是
   在飞行，本来就在入参里；依赖一个**已终结**的任务此前就是 `unknown_deps`，
   这条不改变它。）*
4. **永不拒绝合并，只降级**（issue §4，成立）：任一诊断非空的块，其任务
   **不进入可调度状态**。`ready != true` 同理。诊断**不写进文档**，见规则 7。
   **"不进入可调度状态"必须包含"撤掉已经在那里的 pending 行"**
   （r3 通道 A MAJOR，成立，改设计方向 ⑫）。r2 写的是"不 insert / 不 update"，
   而**不写 ≠ 删除**：序列「声明 A（ready，落 pending 行，因 `task_budget=1`
   尚未派发）→ 编辑引入环 / 复制粘贴出同 key / 把 `refs` 改成跨 cove」之后，
   A 行仍是 `pending`，`compute_ready`（`scheduler/mod.rs:164-191`）照旧把它
   交出去，worker 按**已知有问题的旧声明**跑。裁决：**诊断非空的 key 与
   "声明消失"走同一条删除**（规则 1 的统一谓词），于是：
   - 增量路径与 rebuild 用**同一个**可调度谓词，§11.1 的等号不再对
     "ready:true 但被诊断"未定义；
   - 代价如实记：一次瞬时的编辑失误（例如粘贴出重复 key）会**删掉一条从未派发
     的 pending 行**；改回来之后它按规则 2b 干净重建。这是保守的那一侧，
     而且删的是一行**不含任何执行史**的行（§2 的三条理由）。
   - 已 `dispatched` 及之后的行不受影响（规则 1 的守卫），它们走规则 2 的
     stale 路径 + §5.3 的失效裁决。
5. **`ready` 门**：`ready: true` 且诊断为空（且 `declare-and-wait` 时放行位已置，
   §6.6）→ 行以 `status='pending'` 落地 → `compute_ready`
   （`scheduler/mod.rs:164`）可见。否则不落行；已有的 `pending` 行按规则 1 删除。
6. **原子性**：投影与报告写在同一事务；投影失败（例如 SQL 层错误）整体回滚，
   报告写也不落。**诊断不是失败**——诊断走渲染，不走回滚。
7. **诊断是读时派生，没有存储**（r1 通道 B MAJOR，成立）。初稿反复说
   "渲染成文档里可见的待办"，但从没说它存在哪——而**存进报告就等于内核成了
   文档写者**（破 §2/§7）。裁决：

   - **读端按需派生**（r4 ⑭ 修正：**不是**纯函数调用）：
     `GET /api/waves/{id}/report` 的响应新增只读字段 `taskDiagnostics`
     （Tier-A：openapi.json + zod），`calm.report.read` 同步新增同名字段；
     其值由**一次读事务内**的 `project_task_declarations` +
     `evaluate_schedulability_tx` 算出——与写路径**同一实现**（规则 3′）。
     若读端只跑纯函数，四类需要 DB 的诊断就渲染不出来，而它们恰恰是
     §4.2 规则 1 会据以**删行**的那四类；
   - **零存储、零缓存、零事件**。代价是 O(块数) 的纯 CPU + 一次读事务
     （一条 `waves` 单列读、一条 `tasks` 行读、一条 `count(*)`），
     与该端点今天已经在做的读同数量级；
   - 事务里那次调用只用来决定"哪些行落地"，其诊断输出**丢弃**，不持久化。

   所以"文档里可见的待办"的准确说法是：**读端在文档视图上渲染的派生标注**，
   与 `backlinks_for_wave`（`report_backlinks.rs:106`）的读时派生同构。全文
   凡出现"渲染成文档里可见的待办"，一律按此理解。

### 4.3 事件

- 声明变更时追加 **`Event::PlanUpdated { wave_id, changed_keys, agent_message }`**
  （既有，`event.rs`；发射先例 `plan.rs:843-851`），actor 随文档写者
  （Spec 或 User），scope = Wave。**不新增事件 kind。**
- **`changed_keys` 的语义（NEW，r3 两个通道交叉命中，此前未定义）**：
  > `changed_keys` = **插入的 key ∪ 声明列被更新的 key ∪ 被删除的 key**，
  > 排序 + 去重。"被删除"包含规则 1 的全部四种情形（块删除 / 墓碑覆盖 /
  > `ready` 撤回 / 诊断产生）。仅产生 stale 诊断而**未写任何行**的 key
  > **不进** `changed_keys`（那不是 plan 的变更）。

  **必须包含删除**：否则一次纯撤回编辑可能一个 `PlanUpdated` 都不发，
  而 dispatcher 的 `Event::PlanUpdated { wave_id, .. } => scheduler.poke(wave_id)`
  （`dispatcher/mod.rs:968`，逐字复核）是**key 无关**的——它只认 wave，
  所以把删除算进去对 dispatcher 侧零风险，漏掉它才会丢一次 poke。
- **Tier-A 文档更新（必做项，r3 通道 B MAJOR）**：`Event::PlanUpdated` 今天的
  doc comment 把 `changed_keys` 定义为"被 `calm.plan.upsert` / `calm.plan.cancel`
  created/updated/canceled 的 key"（`crates/calm-types/src/event.rs:764-781`），
  并写着"Spec-only: 该事件的 in-tx role gate 拒绝任何 AI worker actor"。
  两句都要改：新增文档写路径这个生产者、把 `deleted` 写进语义、并按 §0.1 #1 的
  更正把"Spec-only"改述为"worker-AI 排除"。文档字符串是 Tier-A 契约的一部分，
  切片 3b 必须带上它。
- `changed_keys` 为空则不发（复用 `plan.rs:842` 的抑制，避免给 plan 订阅者
  造成虚假唤醒）。**"墓碑物化"这一次编辑本身会发一条含该 key 的 `PlanUpdated`**
  （行被删除 ⇒ key 进 `changed_keys`），§11.2 不变量 6 已据此改写为
  "墓碑生效**之后**（不含物化那一次编辑）"——否则 E2E 一写就红。
- 于是一次报告写最多产生 3 个事件：`CardUpdated` + `WaveReportEdited` +
  `PlanUpdated`。顺序：前两个保持既有次序（`wave_report.rs:580-585`），
  `PlanUpdated` 追加在后——**订阅者是 dispatcher，不是 scheduler**
  （r1 通道 A MINOR，成立）：`dispatcher/mod.rs:137` 过滤、
  `:968` 的 `Event::PlanUpdated { wave_id, .. } => self.scheduler.poke(wave_id)`、
  `:1449`；`grep PlanUpdated crates/calm-server/src/scheduler/` 零命中。
  dispatcher 收到后**戳** scheduler，所以顺序结论不变：`PlanUpdated` 必须在
  卡写入可见之后才被看到。
- **role_gate 无需改动**：`Event::PlanUpdated` 的 in-tx 闸放行 `ActorId::User`
  （`crates/calm-truth/src/role_gate.rs:257-278`，§0.1 #1 的更正），
  所以人经 §3.4 的写口触发的 `PlanUpdated` 天然合法。

### 4.4 归因（评审第 2 条第 4 项）

`EditAuthor`（`calm-types/src/event.rs:153`，四个无字段变体 Spec `:156` /
User `:158` / Kernel `:161` / Plugin `:168`）是**每次编辑**的属性，不是每个块的
属性——文档本身不携带"这个 task 块是谁声明的"。生产发射点只有两个：
`EditAuthor::Spec`（`decision_sink.rs:436`，覆盖全部 `calm.report.*` 工具）与
`EditAuthor::User`（`routes/waves.rs:1274`，REST）。`Kernel` 与 `Plugin` **今天
没有生产发射点**（`Kernel` 只出现在测试里）。

**初稿的裁决（"新增 `tasks.declared_by` 列，由投影在创建行时写入 `EditAuthor`，
文档不携带"）被 r1 的两个通道独立推翻，且推翻得对。** 逐条复核：

- `EditAuthor` 是**每次编辑**的属性（`calm-types/src/event.rs:153`），不是每个块的属性；
- 若 `declared_by` 只活在 `tasks` 行里，那么 `tasks_rebuild_tx`（真源=当前文档）
  **重建不出它**——文档里没有这个信息；
- 而 §8 的树级预算（唯一约束 AI 递归展开的机制）**完全建立在这一列上**。
  于是任何一次 rebuild 都会把全部归因洗成"触发 rebuild 的那个人"或默认值，
  预算随之失守。§0.4 里 `DELETE FROM tasks WHERE wave_id`
  （`db/sqlite/wave.rs:207`、`cove.rs:162`）与 replay 重置都会触发这条路径。

**这是承重墙在"声明既不住文档也不住事件日志"方向上的失效——必须改方向，
不能加注意事项。**

**新裁决：`declared_by` 是 `task` 块 payload 的必填字段（§3.2），住在文档里。**
这不是妥协，而是 §1 判据的直接结论：**"是谁提出要做这件事"当然会收敛进
「我们当时决定了什么」的记录**——它是声明，不是状态。初稿把它当成"归因元数据"
才产生了那个死角。

三个后果，全部是好的：

1. **rebuild 重新成为文档的纯函数。** `tasks_rebuild_tx` 从块 payload 直接读
   `declared_by`，§11.1 的断言不再有洞。
2. **AI 不能伪造归因。** §3.7 规则 1 在持久化事务里强制
   `declared_by == EditAuthor`，且规则 2 禁止事后改写。纯函数
   `validate_payload` 做不到这件事（它看不见写者），所以校验点必须是 guard——
   这正是 §3.7 存在的原因。
3. **§6.1 的不对称第一次有了机制。** 块自己携带 `declared_by: "user"`，
   §3.7 规则 3 才可能在**所有**写路径上拒绝 spec 删改它。初稿只有投影表列的
   时候，spec 一次 `write_markdown` 就能删掉人的任务块，而投影只会以为
   "人自己删了"。

`tasks.declared_by TEXT NOT NULL DEFAULT 'spec'`（NEW 列）**仍然保留**，但
它的性质变了：**它是块字段的投影副本**，存在的理由是让 §8 的预算能用一条
SQL 聚合查出来（`WHERE declared_by='spec' AND status NOT IN (…)`），
而不是让树预算去 load 每份 CRDT 文档。真源是块。

**如实记的弱点**：如果一个人手改了 AI 声明的任务的 goal，`declared_by` 仍是
`spec`。这是对的——"是谁提出要做这件事"与"最后谁改了措辞"是两回事，
后者由事件日志的 `WaveReportEdited` 链回答。

**r3 的推论：同一条切分要求墓碑权属独立成字段。** "是谁提出要做这件事"
（`declared_by`）与**"是谁否决了它"**（`tombstoned_by`，§3.2/§6.1）同样是两回事；
让墓碑改写 `declared_by` 会把两个问题挤进一个字段，并直接与"归因不可变"
（§3.7 规则 2）冲突——那正是 r3 两个通道交叉命中的 BLOCKER（§3.7 的 ⑨ 决策表）。
第三个权属位 `released_by_user`（"是谁放行的"，§6.6）遵循同一条原则：
**一个问题一个字段，每个字段各由 §3.7 的一条规则守。**

---

## 5. 冲突与 stale

### 5.1 stale 的作用域是**引用闭包**，不是单块（评审第 3 条）

`task` 块的 `goal` / `acceptance` 本身就是 prompt，这不构成重复。但上下文应当
**引用**而非复述。若 `key=impl` 的块写着"按 `b_1f3a` 的方案实现"，
**有人改了 `b_1f3a` 时该 task 块自己的 rev 不变**——基于单块 rev 的失效检查会
完全漏掉，agent 继续按旧方案跑。

**两条硬规定**：

- **引用必须是 id 式的**：`neige://wave/<id>#b_xxxx`。位置式表述（"按上面第 3 节"）
  **禁止**——位置漂移让声明静默变义而 rev 检测不到。机制上：`refs[]` 字段的
  每一项都过 `parse_destination`（`report_links.rs:138`）且**必须解析出
  `dst_block_id`**（`is_block_id` `:152`），否则校验失败 → 该块不可调度。
- **`goal` / `acceptance` 正文里的 `neige://…#b_xxxx` 也进闭包**，用同一个
  `scan_links`（`report_links.rs:63`）。不带块片段的整 wave 链接（`neige://wave/w1`）
  **不进闭包**——它没有可比对的 rev，进了只会产生永远无法收敛的 stale。

**`move_block` 不构成漏报，且理由就是上面第一条**（NEW，r4 通道 A 建议明写，
成立）：`move` 既不动 rev 也不动内容（`wave_report_blocks.rs:12-19` 的表格逐字
写着 "Reorder; **rev untouched**"），因此它不改变任何被引用块的 `content_hash`；
而**位置式引用已被上面第一条硬规定禁止**并由 `is_block_id`
（`report_links.rs:152`）机制强制，所以"块挪了位置 ⇒ 引用悄悄指向了别的东西"
这条路径**在本设计里根本不存在**。这条以前只是隐含成立，现在明写出来。

**闭包的根是 task 块自己**（NEW，r2 通道 A MAJOR，成立且是 issue 点名的需求）。
r1 把闭包定义成"task 块**引用**的块"，于是**编辑一个 in-flight 任务自己的
`goal` / `acceptance` / `gate` 不触发任何失效判定**——没有 `TaskContextAdvanced`、
不判 `material`、不停止派发，worker 照旧按旧声明跑，而 §4.2 规则 2 只产出一条
给人看的诊断。这恰好是 issue §5 列为阻塞的那条需求（「已被认领的 task，
其声明变更必须让 claim 失效」）落空。裁决：

> **冻结集必然包含 task 块自身的 `(wave_id, block_id, rev, content_hash)`**，
> 它是闭包遍历的根节点（深度 0，计入 `MAX_REF_NODES`）。

后果：改 in-flight 任务的 `goal` → 内容哈希变 → 第 1 级检测命中 → 第 2 级裁决
（"你认领的任务，它自己的声明从 X 变成了 Y"）→ 判 `material` 则不再派发 + 诊断。
**改 `priority` / `context` 这类不进 prompt 的字段也会命中**（哈希是整个 canonical
fence 的），这是保守的那一侧，由第 2 级裁决降误报。
**`origin='legacy'` 的行没有块**，其冻结集就是空集——见 §11.2 不变量 3 的
"空 ≠ 缺失"。

**闭包不得跨 cove**（NEW，r2 通道 A MAJOR，成立）。r1 允许 `refs[]` 指向任意
wave，而第 2 级裁决会把「`b_1f3a` 从 X 变成了 Y」的**块内容**递给任务所在 wave
的 spec。项目里每一处跨 wave 的读时派生都是**刻意 cove 内**的：
`backlinks_for_wave` 先读 `target_wave.cove_id` 再
`wave_report_cards_by_cove(...)`（`report_backlinks.rs:106-130`，逐字复核），
§7.2 也显式防了 fork 的跨 cove 泄漏。裁决：

- **引用闭包只在同 cove（+ system cove）内解析**，与反链同一条边界；
- 执行点在 `project_tasks_tx`（它有 DB，纯函数 `validate_payload` 看不到 cove）：
  某条 `refs[]` / 正文链接指向本 cove 与 system cove**之外**的 wave →
  该块**不可调度** + 诊断「引用越过了 cove 边界」。不是静默丢弃——静默丢弃会让
  一条本该被检测的引用悄悄退出闭包；
- claim 时的闭包解析再做一次同样的过滤（fail-closed 的第二道），
  越界节点按"解析不到"处理，即 `material`。

**冻结集** = 闭包的 **`(wave_id, block_id, rev, content_hash)`** 四元组集合。

初稿写的是 `(block_id, rev)`、明确**排除**内容哈希，依据是"`align.rs:24-27`
的规范化相同 → rev 不变，所以 rev 相同即语义未变"。**r1 的两个通道各自找到一个
致命反例，两个都成立，两个都足以单独击穿"不允许漏报"**：

- **块 id 会被回收（通道 A）。** 被引用的不变量说的是"一个**存活且匹配上**的块，
  规范文本相同则 rev 不变"，它**不蕴含**逆命题"`(id, rev)` 相同 ⇒ 同一个块"。
  `reassign_ids` 只用存活块的 id 预置 `used`（`align.rs:151`），`mint_id`
  （`:352-364`）只对 `used` 探冲突；被删块的 id 立即释放
  （`wave_report_doc.rs:198` "vanished blocks are deleted"），
  而全新切片 `rev = 1`（`align.rs:167`）。于是：冻结 `(b_1f3a, 1)` →
  一次整文档重写删掉它 → 一个**毫不相干**的新块铸出 `b_1f3a`, `rev = 1` →
  机械检测比对相等，报告"没变"。**而 `rev = 1` 恰恰是最常见的冻结值**
  （紧挨着任务写下的方案块）。
- **rev 自增是饱和的（通道 B）。** `align.rs:163` 与 `wave_report_doc.rs:474`
  都用 `saturating_add(1)`：在 `u32::MAX` 处内容变而 rev 不变。
  （发生概率极低，但"不允许漏报"是**全称**断言，一个反例就够。）

**裁决：冻结 `content_hash`。** `content_hash = sha256(canonical_flat_text)`，
其中 canonical flat text **已经**被算出来了——prose 走 `markdown` 原文、
非 prose 走 `render_fence`，即 `flat_text`（`report_blocks/mod.rs:234`）/
`comparable_flat`（`align.rs:190`），对齐器每次写都要算它。**这一份哈希的
计算是白拿的**，唯一存储成本是每个引用 32 字节。加上它之后，上面两个反例
（id 回收、rev 饱和）都被关闭：它们靠的是**身份坐标复用**，而哈希只看内容。

**断言的强度要说准**（r2 通道 B MAJOR，成立）。r1 写的是"不同内容**必然**不同
哈希"——**那是假的**，定长哈希不可能是单射。准确表述：

> **`content_hash` 提供的是抗碰撞检测，不是不可能性证明。** "不允许漏报"这条
> 断言的精确形式是：**除去 SHA-256 的碰撞，任何内容变更都会被检出。**
> 与之相对，r1 的 `(block_id, rev)` 方案漏报的是**构造性的、日常发生的**两类
> 序列（删块后 id 回收、`saturating_add` 饱和），不是密码学残余概率。
> 两者不在一个量级上，但断言不能写成全称。

**为什么不"冻结规范字节本身"**（通道 B 的备选，驳回）：块的 canonical bytes
上限是 `MAX_CANONICAL_BYTES = 256 KiB`（`kinds.rs:42`），乘以
`MAX_REF_NODES = 64` 就是每个 claim 最多 16 MiB——而冻结集要进
`Event::TaskContextFrozen` 的 payload（§5.3），那会把一个核心事件变成 BLOB 载体。
32 字节 × 64 = 2 KiB 是可接受的；16 MiB 不是。
**为什么不引入"块化身 id"**（通道 B 的另一个备选，驳回）：那要求块在铸造时就带
一个持久唯一标识，等于给 `align.rs` 的 id 机制加一列并改所有写路径——
用一个 32 字节的派生值就能达到同等检测强度时，不值得动身份层。

**`rev` 为什么还留着**：它是便宜的先判（绝大多数情况 rev 不同即可判定），
且是给人看的诊断信息（"从 rev 3 变成 rev 5"）。**但判定的权威是 `content_hash`。**
本文其余各处凡说"rev 单调 ⇒ 可检测"，一律更正为"**内容哈希不等 ⇒ 可检测**"。

**闭包深度：传递闭包 + 双预算 + 耗尽即 fail-closed**（推翻初稿的"1 层"）。

初稿写"深度 1 层"，同时 §5.3 第 1 级写"不允许漏报"。r1 通道 A 指出这两条
**不相容**，且理由是设计自己造成的：§5.1 强制"引用而非复述"、§5.2/§7 都在推
agent 走引用链，那么"方案块 `b_1f3a` 自己再链到接口契约块 `b_2000`"正是本设计
**鼓励**的形状；改 `b_2000` 在深度 1 下不可检测，而 fail-closed 纪律永远不会
触发——**因为压根没观测到变化**。"唯一被论证过的真实场景"讲的是迄今观测到什么，
不是机制允许什么。这个反驳成立。

裁决：

- **传递展开**，`MAX_REF_DEPTH = 3`、`MAX_REF_NODES = 64`（NEW 常数）；
- **任一预算耗尽 → 该任务被标记为 `closure_truncated`**，此后它闭包内**任何**
  wave 的**任何**编辑一律按 `material` 处理（fail-closed），并产出诊断
  "引用链过深/过宽，无法精确判定失效；请把上下文收敛进更少的块"；
- 预算耗尽**不阻止任务派发**——它只是把该任务降级到最保守的失效判定。
  这保住了"永不拒绝合并"，也保住了"不允许漏报"。

可证伪：若 `closure_truncated` 的比例在真实使用中高到让第 2 级 LLM 裁决成为
主要成本项，说明预算太小或"引用而非复述"这条指导本身有问题（§13.3 的可观测量
覆盖它）。

**闭包展开在 claim 事务*之外*做**（NEW，r2 通道 A MINOR，成立）。r1 把冻结写在
`task_claim_pending_tx`（`task.rs:222`）"的同一事务里"，而那是**调度器的写事务**
（`scheduler/mod.rs:641`）；在里面做最多 64 个节点、跨任意 wave 的传递展开
（每个节点一次 `cards.payload.blocks` 行读）是一笔**没有被预算过**的持锁开销。
r1 的"哈希是白拿的"只对**哈希**成立，对**遍历**不成立。裁决：

1. **解析在 claim 之前**：scheduler 选出候选任务后、开启 claim 事务前，用普通读
   连接展开闭包，得到冻结集；
2. **事务内只写**：`task_claim_pending_tx` 的同一事务里写
   `tasks.claim_context_json` + `task_ref_index` 行 + 发 `TaskContextFrozen`。
   事务内**不做**任何跨 wave 读；
3. **中间窗口是安全的，且不需要复验**：若某个被引用块在"解析"与"claim"之间被改，
   冻结下来的就是**旧**哈希，而当前文档是新内容——下一次（或紧接着的那次）
   第 1 级检测重解析时两者不等 ⇒ 判 `material`。**窗口内的竞态只会让系统更保守，
   不会漏报**，这正是 fail-closed 想要的方向。

代价（`MAX_REF_NODES` 次行读 / claim）记进 §13。

### 5.2 反链扫描必须覆盖 `task` 块（机制缺口）

`report_backlinks.rs:177-180` 的 `filter(kind == KIND_PROSE)` 必须放宽为
"prose + 内核声明为可扫描的 kind"，且扫描的是**该 kind 声明的文本字段**
（对 `task` 是 `goal` / `acceptance`），不是 fence 的 canonical JSON——
否则 JSON 转义会让 markdown 链接语法解析失败。这是切片 2 的必做项。

### 5.3 三级阶梯（评审第 5 条，逐条落地）

> **本节最重要的一条（r4 ⑬）：第 1 级的正确性载体是 fail-closed 全量 sweep，
> 事件路径是延迟优化。** 下面"按 `dst_wave_id` 查索引再重解析"的全部内容仍然
> 保留、仍然是常态路径，但它**不再承载**"不允许漏报"这条全称断言——
> 承载它的是 §5.3.1 的 sweep。理由见 §0.2(g)：总线自承 lossy、每条 envelope
> 是 fire-and-forget、唯一的跨重启补投对跨 wave 引用结构性失明。

1. **机械检测**（便宜、必须保守、**不允许漏报**）：claim 时冻结闭包的
   `(wave_id, block_id, rev, content_hash)`；每次 `WaveReportEdited` 后比对，
   **并由周期性 sweep 兜底**（§5.3.1）。
   比对**不走 `backlinks_for_wave`**（§0.2(c)：那是 O(cove) 全扫描）。
   **但"直接按 id 查那几个块"回答的是错的问题**（r1 两个通道独立指出）：
   事件告诉你**哪个 wave 的报告变了**，没告诉你**哪些任务冻结了那个 wave 里的块**；
   而 §5.1 明确允许跨 wave 引用，被编辑的 wave 常常不是任务所在的 wave。
   缺的是**反向索引**，见下面"反向索引与路由"。
   **而且第 1 级必须对所有 `EditAuthor` 无条件运行**（NEW，r3 通道 A MAJOR，
   本轮找到的**第四个静默漏报路径**）：dispatcher 的 `WaveReportEdited` 分支
   （`dispatcher/mod.rs:989`）在做任何事之前先过 `event_warrants_spec_push`
   （函数 `:63`，`WaveReportEdited` 分支 `:95-97`），它只放行
   `EditAuthor::User | Plugin`，`Spec` / `Kernel` 走 `else` 分支**只打一条
   trace**（§0.2(e′) 已逐字复核）。而"spec 改了被引用的方案块"「spec 改了一个
   in-flight task 自己的 `goal`」正是最常见的变更源——把第 1 级挂在这条谓词后面
   等于把主要场景整条丢掉。裁决：
   > **第 1 级机械检测是 `WaveReportEdited` 分支里独立的、前置的一步，
   > 对任何 author 无条件运行；`event_warrants_spec_push` 只决定第 2 级的
   > harness 推送。** 两者在同一个分支里顺序执行，互不为前提。

   这与该谓词存在的理由不冲突：它防的是"把 spec 自己写的东西推回给它"造成的
   自激环（`dispatcher/mod.rs:90-94` 的注释），而第 1 级不推送任何东西，
   它只做索引查找与哈希比对。
2. **spec 语义裁决**（贵、精确）：把 diff 交给 spec——"你认领的任务依赖
   `b_1f3a`，它从 X 变成了 Y，这是否使工作失效？"
   载体在，但**不能直接用**：dispatcher 今天的
   `Event::WaveReportEdited` 分支只把 observation 推给
   **被编辑的那个 wave** 的 harness（`dispatcher/mod.rs:983-998` 调
   `observe_harness(wave_id, …)` 于 `:990`，observation 构造 `:1308-1312`，
   谓词 `event_warrants_spec_push` `:95-97` 允许 `User | Plugin`）。
   跨 wave 引用时，该推给的是**任务所在 wave** 的 spec，不是被编辑 wave 的。
   所以新增的不只是"附上 claim 上下文"，还有**按反向索引扇出路由**（见下）。
3. **人**：判定为实质变更时，**不自动重做**（agent 产出花了 token 和时间），
   而是变成 §4.2 规则 4 的可见待办。#830 已定 worker 层无 human-in-loop，
   这是唯一合适的归宿。

**分工必须干净**：**agent 永远不是"发现"变化的那一环，只是"判断"变化重不重要的
那一环。** 让 agent 决定要不要去看，保证就没了。

**三条约束**：

- **fail-closed**：判不准就算实质变更。LLM 做裁判倾向说"看着没问题，继续"，
  而代价不对称——在实质变更的规格上继续跑，产出的是**自信的错**。机制上：
  裁决工具的返回值只有 `{ verdict: "material" | "immaterial", rationale }`，
  缺席 / 解析失败 / 超时一律按 `material` 处理（与
  `feedback_fail_closed_fence_semantics` 的既有纪律一致）。
- **判定落事件**：新增 **`Event::TaskContextAdvanced { wave_id, task_key,
  from_revs, to_revs, verdict, rationale }`（NEW，Tier-A 全流程：goldens
  min/full + zod + invalidationPolicies + event-version 说明）**。
  否则无法排查"为什么 agent 拿着过期上下文产出了东西"。
  actor = `ActorId::Kernel`（内核记录的是裁决**结果**，spec 的判断内容在 payload 里）；
  role_gate 加一条 kernel-only 条款，与 `TaskGateResult`（`role_gate.rs:329-360`）同构。
  **落在切片 3a，不是切片 4**（r5，见 §12）：sweep 是 3a 的正确性载体，
  而一个判决无处可记的 sweep 也就无处可执行。判 `material` 的那一条
  **同事务写 `tasks.context_stale_at_ms`**（§5.3.3 的持久载体）。
- **冻结点可推进**：判非实质后冻结点推进到新的 `(rev, content_hash)`，否则
  同一处变更反复触发。所以"冻结"不是一次性快照，而是**可推进的 claim 上下文**，
  每次推进带一条 `TaskContextAdvanced`，并同事务更新 `task_ref_index`。
  **反过来，判 `material` 不推进冻结点**——因此那一侧需要一条**独立**的
  once-per-condition 守卫（`context_stale_at_ms`，§5.3.1），
  否则电平触发的 sweep 会每轮重发一次（r5 通道 A MINOR）。

**裁决与 gate 的时序：不做栅栏，如实承认**（r1 通道 B BLOCKER 的后半段）。
通道 B 要求"一个原子状态跃迁，在裁决出结果前阻止 gate 落定"。**本设计不做**，
因为它需要往任务状态机里加一个新状态并让 gate runner 在上面阻塞——那正是
§0.1 #14 已证不存在、§6.5 已明确不造的那条"中断在跑的东西"的机制。
后果如实记：**一个已经在跑的 worker，其 gate 可能在裁决返回之前就落定**。
缓解不是栅栏，而是三条已有机制的组合：

1. 判 `material` 后**任何 operation 都不得再*开始***（§11.2 不变量 5，
   **r6 重述**）——失效的上下文不会被用来开始新工作。
   *（**r4 的写法「不得再产生新的 `TaskDispatched`」是空洞的**，r5 通道 A
   BLOCKER-adjacent：`TaskDispatched` 只在 claim 事务里发射
   （`scheduler/mod.rs:692`）而被 claim 的行永不回 `pending`，所以那句话在它
   要保护的那些行上恒真。**r5 的重述又选错了强制点**（r6 两个通道）：
   `resume_dispatched` 不是唯一会起活的东西——operation 的开机恢复与
   `drive_gate_inner` 的 submit 分支同样会。载体与**那一条**规则见 §5.3.3。）*
2. 诊断把"这个任务是在过期上下文上完成的"渲染在文档视图上，人可见；
3. **已经开始的 operation 照常跑完并汇报**——与 §6.5 的立场一致
   （**跑起来的东西跑完**）。**r6 收窄了这一条，必须读作它的字面意思**：
   "跑完"指的是**那一个已越过 `prepare_tx` 的 operation**。判 material 之后
   **不会再有新的 gate 执行被启动**——一个已完成的 worker，若它的 gate 尚未
   开始，那次 gate 会被拒（§5.3.3 的同一条规则），该行落 `failed` 且
   `gate_result.log_tail` 含 `context-stale`。理由：`gate_json` 同样是那份
   过期声明的一部分（它就住在 task 块里），在过期声明上跑一条真实 shell 命令
   并把结果当成"通过"，是本设计最不该给的那种自信。**代价见 §6.5 / §13.4。**

§11.2 的不变量 3 因此**去掉了"早于 `TaskGateResult`"这半句**（初稿写了，
但没有任何机制保证它）。风险记在 §13。

**反向索引与路由（NEW，r1 补齐的机制缺口）**

- **新表 `task_ref_index`**（NEW migration）：
  `(dst_wave_id TEXT, dst_block_id TEXT, task_id TEXT, PRIMARY KEY(dst_wave_id,
  dst_block_id, task_id))` + `INDEX (dst_wave_id, dst_block_id)`。
  在 `task_claim_pending_tx`（`task.rs:222`）的**同一事务**里按冻结集写入。
  它和 `tasks.claim_context_json` 一样是**状态的投影**（真源见下），
  可从 `TaskContextFrozen` 事件完整重建。
- **清理必须收敛成一个原语，并枚举全部生产者**（NEW，r3 通道 B MAJOR + 通道 A
  MAJOR 的后半，成立）。r2 只写了"任务终结时同事务删除"，而终结**不是一条路径**：

  ```rust
  /// 唯一的索引清理原语。所有终结/消失路径调它，不各写各的 DELETE。
  async fn task_ref_index_clear_tx(
      tx: &mut Transaction<'_, Sqlite>,
      task_id: &str,
  ) -> Result<()>;                                   // NEW
  /// wave/cove 删除与 fixture 重置用的表级形态。
  async fn task_ref_index_clear_by_wave_tx(
      tx: &mut Transaction<'_, Sqlite>,
      wave_id: &str,   // 既清 task 侧（task 属于该 wave）也清 dst 侧
  ) -> Result<()>;                                   // NEW
  ```

  **生产者全清单**（逐条已核对，行号为 HEAD）：

  | 终结/消失路径 | 位置 | 清理形态 |
  |---|---|---|
  | `task_cancel_tx`（`pending → canceled`）| `crates/calm-truth/src/db/sqlite/task.rs:160` | 返回 1 行时 `clear_tx` |
  | `task_complete_from_worker_tx`（→ `done`）| `task.rs:405` | 同上 |
  | `task_report_success_from_worker_tx`（→ `done`/`verifying`）| `task.rs:493` | **仅在落到 `done` 时**清；`verifying` 仍是 in-flight |
  | `task_apply_gate_result_tx`（`verifying → done\|failed`）| `task.rs:541` | 返回 1 行时 `clear_tx` |
  | `task_fail_from_worker_tx`（→ `failed`）| `task.rs:586`（调用点含 `decision_sink.rs:181`、`scheduler/mod.rs:912,1222,1909`、`reaper/mod.rs:559`）| 返回 1 行时 `clear_tx` |
  | `task_delete_pending_tx`（NEW，§2 的声明面删除）| NEW | 返回 1 行时 `clear_tx`（正常情况下 pending 行没有索引行，但删除必须幂等） |
  | wave 删除 | `db/sqlite/wave.rs:191-234`（`DELETE FROM tasks` 于 `:207`）| `clear_by_wave_tx` |
  | cove 删除 | `db/sqlite/cove.rs:147` 起（逐 wave `DELETE FROM tasks` 于 `:162`）| 对每个 wave 调 `clear_by_wave_tx` |
  | fixture 重置 | `crates/calm-server/src/replay.rs:363` 的表清单 | 加一条 `DELETE FROM task_ref_index` |

  **但正确性不依赖这张清单是否穷尽**（这是设计上的关键选择）：**索引的读端
  一律与 `tasks` 内联接并过滤 in-flight** ——
  `… JOIN tasks t ON t.id = task_ref_index.task_id
  WHERE t.status IN ('dispatched','running','verifying')`。于是漏掉一个清理点
  只是**代价 bug**（残留行被读时跳过），不是**正确性 bug**（不会让已终结的任务
  被反复重解析/裁决）。清单负责代价，联接负责正确性。
  **r4 补一句更强的**：sweep（§5.3.1）**根本不从这张索引出发**——它从 `tasks`
  自己枚举 in-flight 行，所以"索引行被漏清"与"索引行被过早清掉"两个方向
  **都不影响正确性**。索引至此纯粹是事件路径的加速结构。
- **不变量**（§11.2 新增 12）：不存在 `task_ref_index` 行，其 `task_id` 指向
  一个已终结（`done/failed/canceled`）或**不存在**的 `tasks` 行。
- **检测路径**（r2 改写，两个通道各自证伪了 r1 的写法）：
  `WaveReportEdited{wave_id}` → **只按 `dst_wave_id` 查 `task_ref_index`**
  （主键 `(dst_wave_id, dst_block_id, task_id)` 覆盖这个前缀）→ 得到"冻结了这个
  wave 里任意块"的全部 `task_id` → 对每个这样的任务，**把它的整份冻结集
  （`tasks.claim_context_json`，真源 `TaskContextFrozen`）逐条对着当前快照
  重新解析**：
  - 解析得到的块的 `content_hash` 与冻结值相等 → 该引用未变；
  - 不相等 → `material` 候选，送第 2 级；
  - **解析不到（块已被删除、id 已失效、越 cove）→ 直接 `material`**（fail-closed）。

  **为什么不能"从事件算出变更块 id"**（r1 的写法，两条独立的证伪）：
  1. `Event::WaveReportEdited` 只携带 `summary_before/after` 与
     `body_before/after` 的**扁平 markdown 文本**（`wave_report.rs:566-579`，
     逐字复核）——**没有块 id、没有 rev、没有前态块集合**；而 fence 明确不携带
     id/rev（`fence.rs:6-8`），文档里的真实 id 来自对齐/hint 而不是对 body 的
     重新哈希（`wave_report_doc.rs:98-112`、`align.rs:38-47`）。在事件体上重跑
     一次对齐**恢复不出**权威 id。
  2. 更致命的是**即使能恢复也不够**：`reassign_ids` 只返回**存活**切片
     （`align.rs:152-168`，它 `map` 的是 `new_slices`）。一个**被删除**的被引用块
     在任何"变更块 id 集合"里**都不会出现**，于是索引查找落空、什么都不触发——
     "删掉一个 in-flight 任务所依赖的方案块"会静默漏报。这是「不允许漏报」上的
     **第三个洞**（前两个是 id 回收与 rev 饱和，§5.1）。按 `dst_wave_id` 拉全集
     再重解析的写法**结构性地**免疫它：删除表现为"解析不到"，而不是"不在集合里"。

  **`WaveReportEdited` 不是唯一的触发源**（r3 通道 A MAJOR，成立；**r4 重写了
  它的补救手段**）。"解析不到 ⇒ 直接 `material`，因而结构性免疫删除"这条
  **只在有事件触发重解析时成立**；而删除**整个 wave / cove** 一个
  `WaveReportEdited` 都不发（§0.2(f′)：`wave_delete_tx` `wave.rs:191-234` +
  `Event::WaveDeleted` `routes/waves.rs:1050`；`cove_delete_tx` `cove.rs:147-` +
  `Event::CoveDeleted` `crates/calm-types/src/event.rs:419-420`，
  `Event::WaveDeleted` 的定义在 `:424-425`）⇒ 事件驱动的索引查询永不被触发。

  **r3 的补救是"删除事务内先读后删、把受影响 `task_id` 集合随事件落地"。
  r4 把它整条删掉**，理由有两条，第二条是决定性的：

  1. **它没有载体，而造一个载体太贵**（r4 通道 B MAJOR，成立）。
     `Event::WaveDeleted { id, cove_id }`（`crates/calm-types/src/event.rs:425`）
     与 `Event::CoveDeleted { id }`（`:420`）**都不携带任何 task 集合**；
     dispatcher 只看得到 envelope，看不到"随事务返回给调用者"的那个值。
     给这两个事件加字段是一次 Tier-A wire 变更（goldens min/full、zod、
     `invalidationPolicies`、ts-rs、event-version），而 §5.3 末尾刚以同样的
     Tier-A 代价为由**驳回**了"给 `WaveReportEdited` 加变更块 id"——
     同一条纪律下这次必须被同样定价。
  2. **有了 sweep 之后它不再是正确性依赖**（r4 通道 A BLOCKER 的连带简化）。
     dst wave 被删 ⇒ in-flight 任务冻结集里那个元组**解析不到** ⇒ 下一次 sweep
     一定判 `material`。r3 之所以要求"事务内先读"，正是因为当时**只有事件路径**
     能触发重解析；sweep 不需要任何人告诉它谁受影响。

  于是删除路径的落地形态简化为：

  - `wave_delete_tx` / `cove_delete_tx` 照常调 `task_ref_index_clear_by_wave_tx`
    （只清索引，**不再先读**），**两个事件不加任何字段**；
  - dispatcher 的 `WaveDeleted` / `CoveDeleted` 分支（NEW，但很薄）只做一件事：
    **立刻触发一次 sweep**（§5.3.1；sweep 是幂等的、有预算的，重复触发无害）。
    这纯粹是**降低延迟**——即使这条 envelope 丢了，周期性 sweep 仍会在下一轮
    抓到它。
  - 这条路径因此**不需要**「不属于被删 wave 的 task_id」这种事务内计算，
    §12 切片 3a 的完成定义里也不再有对应的 Tier-A 连带面。

  **代价的真实上界**（r3 通道 A MAJOR，r2 写错了）：`DEFAULT_PERMITS = 8`
  （`dispatcher/mod.rs:55`，注释与 `:475` 都写明它是 **global concurrent-spawn cap**）
  是**并发 spawn 的上限，不是任务生命周期的持有量**——一个任务 spawn 之后就把
  permit 还回去了，`dispatched/running/verifying` 的行数与它无关。
  in-flight 的真实上界是 **Σ 各 wave 的 `task_budget`**（`compute_ready` 在
  **单 wave 内**算 `capacity = budget - running_cost` 后 `take`，
  `scheduler/mod.rs:164-191`），而 wave 数**无界**。所以：

  > 一次触发的重解析上界 = （冻结了该 wave 的 in-flight 任务数）× `MAX_REF_NODES`
  > ≤ (Σ per-wave `task_budget`) × 64 —— **没有常数封顶**。

  这正是"一个 wave 被很多 claim 引用"的场景。裁决：**给第 1 级按 `dst_wave_id`
  的扇出加一个显式上限 `MAX_RERESOLVE_FANOUT = 64`（NEW 常数）**，
  超出部分**不做重解析，直接按 `material` 处理**——与 §5.1 的闭包预算耗尽、
  `MAX_ADJUDICATION_FANOUT` 同一条 fail-closed 纪律，因此**不引入漏报**，
  只在极端扇出时增加误报。§13.14 已按此改写。

  **驳回通道 B 建议的"把变更块 id 塞进 `WaveReportEdited` 或新事件"**：
  那是给一个所有报告写都会发的核心事件加 Tier-A 字段，用来换一条这里根本不需要
  的信息（重解析已经拿到权威答案），而且它对"块被删除"这一类**仍然**要靠重解析兜底。
- **路由**：对每个受影响的 `task_id`，把裁决 observation 推给
  **`task.wave_id` 的 harness**（不是被编辑 wave 的）。这需要 dispatcher 的
  `WaveReportEdited` 分支从"单 wave observe"改成"按索引扇出 observe"（NEW）。
- **扇出上界**：一次编辑最多唤醒 `MAX_ADJUDICATION_FANOUT = 16` 个 wave 的 spec；
  超出 → 剩余任务**不做第 2 级裁决，直接按 `material` 处理**（fail-closed，
  与 §5.1 的预算耗尽同一纪律）。

**冻结集的真源是事件，不是列**（r1 两个通道独立指出）。初稿把冻结集只放在
`tasks.claim_context_json`：它**在文档里没有、在事件日志里也没有**，
而 §2 明说 `tasks` 是"两者的可重建投影"——这一列两边都重建不出来。
任何一次 `tasks` 清除（`db/sqlite/wave.rs:207`、`cove.rs:162`）或 rebuild
都会丢掉每个 in-flight claim 的安全锚，而届时 fail-closed 规则**无物可比**。

裁决：**新增 `Event::TaskContextFrozen { wave_id, task_key, idempotency_key,
refs: [{ wave_id, block_id, rev, content_hash }], truncated: bool }`
（NEW，kernel-only，Tier-A 全流程）**，在 claim 事务内与 `TaskDispatched`
一起发射。于是：

- 真源 = 事件日志（符合 §2：冻结集是状态）；
- `tasks.claim_context_json TEXT NULL`（NEW 列）与 `task_ref_index`
  都降为**该事件的投影**，可被重建；
- **上下文缺失时的行为**：查不到某个 in-flight 任务的冻结集 →
  按 `material` 处理（fail-closed），并产出诊断。

为什么不是给既有 `TaskDispatched` 加字段：那是同等的 Tier-A 代价，却把一个
可选的安全机制焊进了一个所有路径都在发的核心事件；独立事件还能在
`truncated: true` 时单独告警。

**「编辑稀疏」是个假设**：加可观测量（每 wave 的机械检测次数 / 送裁决次数 /
判 material 次数），让它可被数据推翻。

#### 5.3.1 第 1 级的正确性载体：fail-closed 全量 sweep（NEW，r4 ⑬）

上面整条事件路径是**延迟优化**。承载"不允许漏报"的是这一条：

> **在 boot（跟在 `Scheduler::sweep_boot` 之后）与每个周期性 reconcile tick 上，
> 重扫全部 in-flight 任务的冻结集，逐元组重解析；任何一个元组
> ——因为内容变了、块没了、wave/cove 没了、越 cove、冻结集本身缺失——
> 只要不能被验证为"与冻结值逐字节相同"，该任务一律判 `material`。**

落地形态（全部复用既有形状，不新造机制）：

- **枚举源是 `tasks`，不是索引**：
  `SELECT id, wave_id, claim_context_json FROM tasks
   WHERE status IN ('dispatched','running','verifying')
     AND context_stale_at_ms IS NULL`。
  这比通道 A 建议的"`task_ref_index JOIN tasks`，去掉 `dst_wave_id` 前缀过滤"
  **更强一点**，采纳这一版：从 `tasks` 出发时，`task_ref_index` 的清理是否
  完备、是否被 wave 删除提前清空，都不能让一个 in-flight 任务从 sweep 里掉出去。
  （通道 A 的版本对"索引行被提前清掉"这一种情况仍然敏感——而 wave 删除
  正好就是那种情况。）
- **`AND context_stale_at_ms IS NULL` 是 once-per-condition 守卫，不是优化**
  （NEW，r5 通道 A MINOR，成立）。事件路径是**边沿触发**，而 sweep 是
  **电平触发在一个按构造会持续存在的条件上**：判 `material` **不推进冻结点**
  （§5.3 末段"冻结点可推进"只在判 immaterial 时推进），于是没有守卫时，
  每个 reconcile tick、对每个已判 material 的在飞任务、在其**剩余生命周期内**，
  sweep 都会重新检出、**重新发一条 `TaskContextAdvanced`**、并且从切片 4 起
  **在同一个 diff 上重新调用一次 LLM 裁决**。而 `TaskContextAdvanced`
  **不可裁剪**（`events_prune` 是白名单式的，可裁剪 kind 只有
  `claude.hook`/`codex.hook`/`harness.phase.changed`/`harness.item.added`/
  `overlay.set`，`crates/calm-truth/src/events_prune.rs:95-101`）——重复项会
  淹没"判 material 次数"这个可观测量，并击穿切片 4 建立在"编辑稀疏"上的成本模型。
  **守卫的形状**：已有 `context_stale_at_ms`（§5.3.3 的持久载体）的任务
  **不再被 sweep 枚举**——不重复裁决、不重复发射，直到它的冻结点推进
  （推进只可能发生在判 immaterial 的路径上，那条路径不会写这一列）。
  于是每个 `(task, 冻结点)` 至多产生一条 material 的 `TaskContextAdvanced`。
- **冻结集缺失 → `material`**：与 §5.3 末段"上下文缺失时的行为"是同一条规则，
  不是新规则。`refs: []` 的 legacy 行是"空"不是"缺失"（§11.2 不变量 3），
  sweep 对它 0 次解析、直接通过。
- **判定与事件与事件路径完全一致**：命中 → 送第 2 级裁决（切片 4 之前直接判
  `material`）→ `TaskContextAdvanced` + **同事务写 `tasks.context_stale_at_ms`**
  （§5.3.3）。**sweep 不引入任何新事件 kind**；但 **`TaskContextAdvanced` 本身
  从切片 4 前移进切片 3a**（r5，见 §12）——没有它，切片 3a 交付的是一个
  **判决无处可记、因而无法被执行**的 sweep，那正是 §12 声称要防止的半截机制。
- **挂点与节奏**：与 `Scheduler::sweep_all` 同一批调用点——boot、
  `RecvError::Lagged` 分支（`dispatcher/mod.rs:784-806`，那里已经在 `spawn`
  一次 `sweep_all`）、以及既有的 reconcile tick
  （`NEIGE_SCHEDULER_RECONCILE_SECS`，默认 300 秒，`scheduler/mod.rs:24-27`、
  `:83`、`:432`）。**不新增任何 `NEIGE_*` 环境旋钮**——复用既有的那一个，
  节奏与调度活性兜底同频。
  **boot 上的挂点：r5 更正了一次，r6 又前移了一格。** r4 写的是"跟在
  `sweep_boot` 之后"，而 `sweep_boot`（`scheduler/mod.rs:1015`）的**第一件事**
  就是 `sweep_reconcile()`（`:1016`），它的 `Dispatched` 分支（`:1067`）已经把
  每条 dispatched 行重新拉起来了（§0.2(h)）——**重启正是 sweep 存在的理由，
  而在这条路径上过期的 worker 先被拉起来**。r5 因此把它排到 operation 恢复
  **之后**、`sweep_boot` 之前，并宣称"顺序只影响恢复延迟"。
  **r6 证明那句话不对**（通道 A MAJOR）：operation 的开机恢复
  （`plan_recovery_for` `operation/driver.rs:1010-1024` → `apply_recovery_item`
  `:1043-1055` → `drive_one`）**本身就是一条 spawn 入口**。裁决：

  1. **boot 顺序（r6 前移）**：上下文 sweep 排在 **operation 恢复之前**、
     因而也在 `sweep_boot` 之前，是 boot funnel 里与 task 相关的**第一件事**。
     它只读 `cards.payload.blocks` 与 `tasks`、不重驱动任何东西，因此不受
     `sweep_all` 的 boot 门那条理由约束（`scheduler/mod.rs:985-990` 的理由是
     "re-drive dispatched rows against unrecovered operation rows"），
     排在最前面是免费的。
  2. **正确性的主承载者不是顺序，而是 §5.3.3 的那一条规则**：过期判决在
     **operation 适配器的 `prepare_tx`** 里被强制，那是所有起活路径的必经漏斗
     （worker 与 gate、live 与恢复，全都经过）。**判决只要已经落库，顺序就
     无关紧要。**
  3. **顺序承载的只剩一种情况，如实记**：如果"使闭包过期的那次编辑"发生在
     内核**停机期间**，重启时判决还没算出来，谁都读不到它——此时唯一的保护就是
     "sweep 先跑"。这是本设计里**唯一**一处 boot 顺序承载正确性，
     所以 §11.2 不变量 5b 被重述为**对 boot 顺序本身的断言**：
     将来谁重排 boot funnel，CI 会红，而不是静默失去这一层。
- **顺手清索引**（NEW，r5 通道 A MINOR，采纳 §13.20 自己的建议）：每轮 sweep
  末尾执行一条
  `DELETE FROM task_ref_index WHERE task_id NOT IN
   (SELECT id FROM tasks WHERE status IN ('dispatched','running','verifying'))`。
  **一条 SQL、无扇出**，于是 §11.2 不变量 12 可以保留**全称**形式（断言点加
  "一轮 sweep 完成之后"），而不必被限定到它枚举的那几个点。它仍然只是代价治理
  ——正确性由读端的 JOIN 过滤承担（§5.3），这一点不变。
- **代价上界**（记进 §13.21）：一轮 sweep = （in-flight 行数）× `MAX_REF_NODES`
  次 `cards.payload.blocks` 行读，即 (Σ per-wave `task_budget`) × 64，
  与一次极端的事件驱动重解析同量级，但**每 300 秒才发生一次**、且在普通读连接上
  做（不持任何写锁），与 §5.1 末段"解析在 claim 之外"同一形状。
  sweep 内同样受 `MAX_RERESOLVE_FANOUT` 之外的一条上限约束：**单轮 sweep 的
  重解析预算 `MAX_SWEEP_NODES = 4096`（NEW 常数）**，用满即**把本轮剩余未验证的
  任务全部判 `material`**（fail-closed，与闭包预算耗尽、扇出触顶同一纪律），
  并记一条告警——不是"下一轮再说"，因为那会让上界重新变成"没有上界"。
- **可观测量**：每轮 sweep 的耗时、验证元组数、命中数、预算触顶次数。
  若预算频繁触顶，说明 in-flight 规模已经超出本设计的假设（§13.21 的证伪装置）。
- **必须再加一个正向健康信号**（NEW，r5 通道 A MINOR，成立）。上面四个全是
  **每轮指标**——也就是说**恰恰在 sweep 停摆时它们缺席，而缺席不是告警**。
  而 sweep 是本设计"不允许漏报"的唯一承载者，它的停摆方式是真实的：
  (i) 既有 sweep 的形状是"DB 出错 → `tracing::warn!` + 跳过本轮"
  （`scheduler/mod.rs:1050-1055`，`sweep_parked` 那条写着 "next tick retries"；
  `tasks_nonterminal()` 失败则直接 `return`），**反复失败会静默降级这条保证**，
  而不只是延迟一个周期；(ii) reconcile tick 是一个 `tokio::spawn` 里的裸
  `loop`（`dispatcher/mod.rs:814-829`），**其中一次 panic 会静默终结该进程余生的
  所有 sweep**。裁决：
  > 上报 **`context_sweep_last_success_age_seconds`（正向 gauge：距上一次
  > *完整跑完* 的 sweep 的时长）** 与 **`context_sweep_consecutive_failures`
  > （连续失败计数）**。前者对 (i)(ii) 两种停摆**同时**有效，因为它在无人跑
  > sweep 时会无界增长；告警阈值取 `3 × NEIGE_SCHEDULER_RECONCILE_SECS`。

  这一条与 §13.21 成对：那里如实记下"反复的 DB 失败会**静默**降级该保证"。

**这条 sweep 的意义超出它自身。** 一旦它在位：

- 前五个静默漏报洞（id 回收、rev 饱和、`reassign_ids` 只返存活切片、
  `event_warrants_spec_push` 丢 Spec、wave/cove 删除不发事件）**全部从正确性
  依赖退化为优化**——它们决定的是"多快发现"，不再是"会不会发现"；
- 上面刚删掉的"删除事务内先读后删 + 事件加字段"同理；
- §11.2 不变量 4 因此**必须**重述（它在一次 `Lagged` 或一次重启后当场为假），
  改成"sweep 完成之后"的形式，见 §11.2。

**什么仍然不被 sweep 覆盖**（如实记）：sweep 只保证"in-flight 任务的冻结上下文
被重新验证"。它不缩短"从变更发生到被发现"的窗口（那是事件路径的职责，上界是
一个 reconcile 周期），也不改变 §5.3 末段那条"裁决与 gate 之间没有栅栏"——
一个 worker 的 gate 仍可能在 sweep 发现问题之前落定（§13.4）。

#### 5.3.2 「失效检测」的三个维度，以及第一个维度的穷举证据

这条断言横跨三个正交的维度，**每一轮只审出一个**：

| 维度 | 问题 | 状态 |
|---|---|---|
| ① 内容 | 一个冻结的 `(wave_id, block_id, rev, content_hash)` 所指内容，**能不能**被改变 / 变得不可达 / 变义？ | **穷举完毕、全部关闭**（下表，r4 通道 A 逐条查证） |
| ② 观测 | 它变了之后，**凭什么保证有人去看**？ | r1–r3 从未审过；r4 的第六个洞在这里，由 §5.3.1 的 sweep 关闭 |
| ③ 执行 | 有人看过、判了 `material` 之后，**凭什么执行那个判决**？ | r1–r4 从未审过；**r5 发现这一维此前是空的**（§0.2(h)：没有持久载体、不变量 5 空洞、崩溃恢复的重驱动不受任何约束）并给出载体 + 一个强制点；**r6 证明那个强制点不在漏斗上**（还有 operation 开机恢复与 gate 首启两条入口），把它换成**一条规则、一个漏斗**：过期判决在 operation 适配器的 `prepare_tx` 里被强制（§5.3.3）。**关闭** |

**三维的性质各不相同，补救手段也不能互换**：① 枚举的是**写路径**（有限、
静态可穷举），所以枚举能承载结论；② 枚举的是**运行时投递**（没有封闭性），
只能靠 fail-closed 的重扫；③ 既不是枚举也不是重扫的问题——**判决必须落在一个
执行路径读得到的持久载体上，且强制点必须站在所有执行路径的必经漏斗上**
（r6 加的后半句：r5 点名了一个读者，但那个读者只是一个调用点，不是漏斗，
于是 operation 开机恢复与 gate 首启两条路径从它旁边走过去了）。
三次里有两次（②③）的失败模式相同：文档写了一句正确的话，而代码里没有任何
东西读它——③ 还多一层：**代码里有东西读它，但读得不够靠前**。

维度 ①（r4 通道 A 的枚举，已逐条对 HEAD 复核；行号按 `02ef95d5`）：

| 通道 | 判定 | 证据 |
|---|---|---|
| 绕过 `apply_report_op` 写 `cards.payload` | **不可能** | `card_update_tx`（`crates/calm-truth/src/db/sqlite/card.rs:215-229`）对 `kind='wave-report'` 的 payload 写、以及任何进出 `wave-report` 的 kind 迁移**一律 400**；`card_update_with_crdt_tx` 自己也只接受 wave-report 卡、且拒绝 kind 变更（`card.rs:249-266`），其唯一生产调用者是 `wave_report.rs:565`（`use` 在 `:37`） |
| 报告卡被删除 / 被替换 | **不可能** | report 卡以 `deletable = 0` 创建（`migrations/0014_wave_report_card.sql:31`），`routes/cards.rs:1378-1387` 拒绝 REST 删除；plugin 通道也到不了：`callbacks.rs:587` 先过 `perms.can_card_delete(&card.kind, plugin_id)`，而它按 kind 归属判定（`plugin_host/perms.rs:18`），`wave-report` 非 plugin 所有 |
| wave 在 cove 间移动（会静默改变闭包的 cove 边界） | **不存在该路径** | `WavePatch`（`crates/calm-truth/src/model.rs:147-174`）无 `cove_id` 字段；`routes/waves.rs:818-820` 注释逐字写着 "Wave rows are immutable wrt their parent cove" |
| `wave_vcs` restore / revert | **不存在** | `wave_vcs/` 的公开面是 `put_blob`/`head`/`tree_at`/`commit*`/`snapshot*`/`diff*`/`cat_at`/`log`/`prune*`/`sweep*`（`store.rs:13,21,30`、`commit.rs:13,38,57,76`、`read.rs:21,36,53,94`、`snapshot.rs:22,85`、`gc.rs:29,109,150,208,229`）——**没有任何回写 live 状态的路径**；唯一的 `UPDATE cards`（`delta.rs:548`）在 `#[cfg(test)]` 内 |
| 并发 session 的 CRDT merge 复活前墓碑态 | **今天不存在**（未来需复审） | automerge doc 全程 load→mutate→save 在**同一个写事务**内（`wave_report.rs:487-565`）；`wave_report_doc.rs:23` 写的是 "future concurrent merges"，全仓唯一的 `.merge(` 在单测 `wave_report_doc.rs:1194`。**若将来开启 sync，本行必须重审** |
| fence 规范化把变更抹平 | **不会** | `flat_text`（`report_blocks/mod.rs:234-245`）prose 取 `markdown` 原文，非 prose 走 `render_fence`（`fence.rs:75`）→ `canonical_json`（`fence.rs:84`），后者对 payload 是**全量**遍历（只排序 key、定格缩进），不丢字段 ⇒ `content_hash` 相对存储 payload 无损 |
| 内容相等的写被抑制、不发事件 | **不会** | `wave_report.rs:354-360` 逐字保证 "**Both events fire on every call, including content-equal writes**"，`WaveReportEdited` 无条件发射（`:566-579`、`:580-585`） |
| `move_block`（rev 与内容都不动） | **不构成漏报** | 见 §5.1 新增的那一段（`wave_report_blocks.rs:12-19` "rev untouched" + 位置式引用被禁） |
| 迁移 / backfill 改写 report payload | **未发现** | 全部 migration 里对 `cards` 的 `UPDATE` 只碰 `role`/`deletable`/`session_id`（`0013:26`、`0037:4`、`0050:21`、`0055:102`），**没有一条写 `payload`** ；§9 的物化工具走普通报告写路径（因而发事件） |
| fixture 重置 | 已覆盖 | `replay.rs:363` 的表清单加 `task_ref_index`（§9 末段） |

**这张表是"内容维已经关上"的证据，也是"枚举不能承载全称断言"的证据**：
三轮里每一轮都从触发器枚举里掉出一个洞。维度 ① 之所以现在可以靠枚举结论，
是因为它枚举的是**写路径**（有限、可静态穷举、且都汇于 `apply_report_op`）；
维度 ② 枚举的是**运行时投递**，它没有这种封闭性——所以那一维只能靠
fail-closed 的重扫，不能靠枚举。

#### 5.3.3 维度 ③：判决的持久载体与它的强制点（NEW，r5 ⑮）

r4 之前，"判 `material` 之后会发生什么"整个是**措辞**：§5.3 的缓解 1 与
§11.2 不变量 5 写的是「不得再产生新的 `TaskDispatched`」。§0.2(h) 已逐字证明
那句话**恒真且无用**——`TaskDispatched` 只在 claim 事务里发射
（`scheduler/mod.rs:692`），而被 claim 的行永不回到 `pending`
（`task.rs` 无任何写回 `'pending'` 的 SQL）。真正会拿着过期闭包**重新开始
工作**的是崩溃恢复的重驱动（`:1067` → `:1397` → `:761`），它一个
`TaskDispatched` 都不发。

**三件事，缺一不可：**

**(1) 持久载体：`tasks.context_stale_at_ms INTEGER NULL`（NEW 列）。**

- 语义：该任务**最近一次被判 `material` 的时刻**；`NULL` = 从未被判 material。
- 它是**状态**，不是声明——按 §2 的墙，写者只有内核，真源是事件日志：
  它是 **`Event::TaskContextAdvanced{verdict: "material"}` 的投影列**，
  与 `claim_context_json`（`TaskContextFrozen` 的投影）、`task_ref_index`
  同一性质，可从事件日志完整重建。文档里没有它，`tasks_rebuild_tx` 不碰它
  （§11.1(3)："所有存活行的全部状态列逐字节不变"已经涵盖它）。
- 写入点：与 `TaskContextAdvanced` **同一事务**（无论判定来自 sweep 还是
  事件路径），单赢家形状
  `UPDATE tasks SET context_stale_at_ms = ?1
   WHERE id = ?2 AND status IN ('dispatched','running','verifying')`。
  判 `immaterial` 时**不写它**（冻结点推进走既有路径）。
- **连带面（r6 修正为它现在真实的强度）**：新增列**建议**同时进
  `TASK_COLUMNS`（`crates/calm-truth/src/db/sqlite/task.rs:19`，被 `:33`
  `tasks_by_wave_tx`、`:131` `task_get_tx` 与 `read.rs:229,240,250` 的池读共用）
  与 `Task` 的 `FromRow`，理由是读端诊断与可观测量都会要它；
  **但它不再是正确性耦合**——r5 的强制点在 `resume_dispatched`（读
  `tasks_nonterminal()` 返回的 `Task`），漏改则拿不到那一列；
  **r6 把强制点换成 `refuse_if_context_stale(tx, task_id)` 的一条定向 SQL 读**
  （§5.3.3(2)），它不经过 `TASK_COLUMNS`。**这是新形状顺带削掉的一个
  运行期失败面**，如实记在这里而不是假装它还在。
  （切片 3b 的 `declared_by`/`origin` 仍然受那条纪律约束——投影**确实**读
  `Task`。）

**(2) 强制点：一条规则，落在所有"起活"路径都必经的那一个漏斗上（r6 重写，⑲）。**

**r5 的强制点选错了位置。** r5 把两道前置检查放进 `resume_dispatched`
（`scheduler/mod.rs:1397`），并宣称"任何未来的调用方重排都不能绕过它"。
r6 两个通道各自证明 **`resume_dispatched` 根本不是唯一会起活的东西**——
三条发现，同一个形状：

| # | 另一条起活入口 | 逐字证据 | 为什么 r5 挡不住 |
|---|---|---|---|
| 1 | **operation 的开机恢复** | `plan_recovery_for`（`crates/calm-server/src/operation/driver.rs:1010-1024`）把 `Pending \| TxCommitted \| AppServerInteract \| SpawnStarted \| SpawnSucceeded` 一律映射为 `RecoveryItem::Recover`；`apply_recovery_item`（`:1043-1055`）`claim_operation_for_recovery` 之后直接 `drive_one` ——**它真的会 spawn** | 这条路径上**没有任何东西读 `tasks`**，更不读 `context_stale_at_ms`；而 §5.3.1 又把上下文 sweep 排在 operation 恢复**之后** ⇒ 在不变量 5 自己的标准构造（dispatched + material → `kill -9` → 重启）上，过期 worker 由恢复漏斗**先**被拉起来 |
| 2 | **b1 谓词误判它自己要处理的那个崩溃窗口** | `OperationRuntime::submit`（`operation/driver.rs:105-123`）**先** `insert_operation` **再** `drive()`；op 以 `Phase::Pending` 创建，phase 阶梯（`crates/calm-truth/migrations/0042_operations_parked.sql:13-24`）在任何 spawn 之前还有 `pending` / `tx_committed` | "有 operation 行" ≠ "工作已开始"。崩溃发生在 insert 与 spawn 之间 ⇒ 留下一条无副作用的 `pending` op ⇒ **b1 命中** ⇒ `drive_spawn` → `submit` 去重 → `drive()` → **worker 在过期闭包上被首次启动**。两个通道独立命中 |
| 3 | **gate 在判决之后首次启动** | `sweep_reconcile` 的 `Verifying` 分支（`scheduler/mod.rs:1107-1112`）spawn `drive_gate`；`drive_gate_inner`（`:1541-1581`）分支 2（"op 缺失或 `gate_attempt == 0`"）**提交**一个 `task-verify` op，它会用该行**已冻结、已过期**的 `tasks.gate_json` 构造并运行真实 shell 命令（`operation/task_verify_adapter.rs:660-665`）| 这是 b2 的情形逐字重现（"从未开始的工作"），却被给了相反的待遇。§5.3 末段与 §13.4 的豁免论证的是一个**已经在跑**的 gate，从未覆盖"判决之后、重启之后**首次**启动的 gate" |

**三条不修成三个豁免口。** r5 自己立的纪律是"点名哪一行代码读哪一个持久值"，
而不是"在每一个想得起来的调用点各加一个 if"——后者只会在下一轮长出第四条
入口。裁决是找**那个所有起活路径都必经的漏斗**，并且它已经存在：

> **规则（本设计里关于维度 ③ 的唯一一条）：**
> **一个非空的 `tasks.context_stale_at_ms` 禁止该 task 上的任何 operation
> *开始*；它从不打断一个*已经开始*的 operation。强制点是 operation 适配器的
> `prepare_tx` ——每个 operation 在**任何副作用之前**、**在事务内**必经它
> （至少一次；越过 `TxCommitted` 之后不再经过，见下面事实 2 的 r7 改述）。**

**为什么 `prepare_tx` 是那个漏斗（三条逐字事实）：**

1. **没有 operation 就没有工作。** 会为一条 `tasks` 行起活的 op kind 是一个
   **封闭集合**：`build_worker_payload`（`scheduler/mod.rs:197-253`）枚举的
   `codex-worker` / `claude-worker` / `terminal-worker`，加上 gate 的
   `TASK_VERIFY_KIND`。四者的 op 都携带 task 身份——三个 worker kind 的
   `idempotency_key` 就是 `task.id`（`drive_spawn` 逐字设置，`scheduler/mod.rs:772-780`），
   `task-verify` 的 payload 带 `task_id`。旧的第五条（`calm.task.dispatch`）
   **已于 #644 退役**，今天只剩一个直接报错的兼容 shim
   （`mcp_server/tools/emit.rs:88-118`：`"calm.task.dispatch was retired (#644);
   no task was dispatched"`）。
2. **每个 operation 必经 `prepare_tx` —— *至少*一次，且必在任何副作用之前；
   phase 只前进不后退 ⇒ 越过 `TxCommitted` 之后不再经过。**
   `drive_one` 的 `Phase::Pending` 分支（`operation/driver.rs:388-393`）唯一的
   动作就是 `prepare_tx_and_advance`。**`submit` 与 `drive_one` 都通向它，
   开机恢复的 `drive_one` 也通向它**——发现 1 与发现 2 因此被同一句话覆盖。
   **"只经一次"是过强的说法（r7 通道 A MINOR，成立，改述）**：
   `prepare_tx_and_advance`（`crates/calm-server/src/operation/repo_sqlite.rs:277-330`）
   在事务内先跑 `adapter.prepare_tx`，再把 phase UPDATE **守卫在 `lease_owner`**
   上；`rows_affected() == 0` 时**回滚并返回 `Ok(None)`**（`:321-323`），
   op 留在 `Pending`，另一个 driver 会**再跑一次** `prepare_tx`。
   **这不削弱本规则所依赖的方向**，三条都成立：
   (i) 我们要加的那一步是**只读检查**且 **fail-closed**，重复执行无副作用；
   (ii) 它**必在任何副作用之前**——同一事务回滚时 `prepare_tx` 自己的写
   （工作区租约、worker 卡等）也一并回滚，所以"多跑一次"不会留下半成品；
   (iii) phase 只前进不后退 ⇒ 一旦 `TxCommitted`，再也不经过这个准入点。
   规则要的是**"任何工作开始前必定被检查一次"**，不是"恰好被检查一次"。
3. **`prepare_tx` 拿得到事务、也拿得到 task 行，而且已经在做同类检查。**
   签名是 `prepare_tx(&mut Tx<'tx>, &Value, &Operation)`（`operation/mod.rs:585-590`）；
   `task_verify_adapter::prepare_tx`（`task_verify_adapter.rs:627`）**今天就在
   同一处**做 `task_get_tx` 然后 `if task.status != TaskStatus::Verifying →
   CalmError::Conflict`（`:651-658`）。我们要加的是**紧邻它的第二个合取项**。

**落地形态：一个共享的读者，四个点名的调用点。**

- **NEW（~10 行）**：`refuse_if_context_stale(tx, task_id) -> Result<()>` ——
  读 `tasks.context_stale_at_ms`，非空即
  `Err(CalmError::Conflict("context-stale: frozen closure no longer matches the document"))`。
- **四个调用点**（本设计"点名读者"的全部内容，每处一行，放在各自 `prepare_tx`
  的**最前面**）：`CodexWorkerAdapter` / `ClaudeWorkerAdapter` /
  `TerminalWorkerAdapter` / `TaskVerifyAdapter` 的 `prepare_tx`。
  **task_id 从哪来（写死，免得实现时猜）**：三个 worker adapter 用
  `op.idempotency_key`（`drive_spawn` 逐字把它设成 `task.id`，
  `scheduler/mod.rs:772-780`；它们的 payload 里那个同名字段是同一个值）；
  `TaskVerifyAdapter` 用 `payload.task_id`（它已经在解析了，`:633`）。
  **`op.idempotency_key` 为 `None` 时 fail closed**——一个 task 绑定的 worker op
  按构造不可能没有它，缺失即是不该发生的形状，按 `Conflict` 拒。
  非 task 绑定的适配器（会话卡、forge、harness 启停…）**一律不碰**。

**下游收敛完全走既有路径，不新增任何分支**（这是选 `prepare_tx` 而不是选
scheduler 的第二个理由）：

- `CalmError::Conflict` 是 `client_failure_parts` 认的**永久性客户端失败**
  （`operation/driver.rs:1180-1191`）⇒ `drive_one` 在 `Pending` 处
  `mark_failed`，**op 终结、无副作用**。
- **worker 侧**：`drive_spawn` 的 `wait()` 拿到 `Failed` →
  `reconcile_spawn_result`（`scheduler/mod.rs:812`）→ **既有的
  `fail_spawn`**（`:891`）⇒ 守卫式 `dispatched/running → failed('spawn-failed')`
  + kernel `Event::TaskFailed{reason: "worker spawn failed: context-stale: …"}`。
  **r5 的 b2 动作原封不动地发生了，但不需要 scheduler 里的任何 if。**
  开机恢复路径下 op 先被标 Failed，随后第一次 `resume_dispatched` →
  `drive_spawn` → `submit` 去重命中该终结 op → `wait()` → 同一条
  `fail_spawn` ⇒ 行不会长期停在 `dispatched` 占住 `task_budget`。
- **gate 侧**：拒绝发生在 `prepare_tx` 的 `gate_attempt` 自增**之前**，
  这正是 `reconcile_gate_outcome` 里**既有的 pre-bump 失败臂**所处理的情形
  （`scheduler/mod.rs:1679-1699` 的注释逐字写着 "A client error in `prepare_tx`
  BEFORE the guarded bump (wave row gone → Conflict) terminal-fails op `#gN`
  while the row stays `verifying@N-1`… Flip the row at its pre-bump attempt
  instead"）⇒ 行落 `failed`，`GateVerdict.status_detail = "gate-infra"`、
  `log_tail` 逐字保留 `context-stale: …`。**本设计不新增 gate 原因枚举值**
  （那是一次 wire 面变更），代价记在 §13.4。

**"已经开始"因此不再是一个我们要写的谓词，而是结构性的。**
`prepare_tx` 只在 `Phase::Pending` 上跑：

| op 所处 phase | 语义 | 本规则的效果 |
|---|---|---|
| 尚不存在 / `Pending` | **这份工作还没开始**（`prepare_tx` 是它的准入点，副作用一律在其后） | 拒绝 → op `failed` → 行落 `failed('context-stale')` |
| `TxCommitted` 及之后 | **这份工作已经开始**（`prepare_tx` 已提交，工作区租约 / worker 卡等副作用已落库） | 不再经过强制点 ⇒ 照常跑完、照常对账（§6.5） |

于是 r5 的 b1/b2 **不再需要被判定**——它退化成"这个 op 是否已越过它自己的
准入点"，由 phase 阶梯本身回答。发现 2 指出的误判**在新形状下不可能发生**：
崩溃留下的那条 `pending` op 重新 drive 时正好落在第一行。

**`resume_dispatched` 里剩下什么：只剩上下文 boot 门（原来的检查 a）。**

| # | 条件 | 动作 |
|---|---|---|
| a | **上下文 boot 门未开**（`context_sweep_boot_done == false`）| **原样留下该行**、`tracing::debug!` 后返回。不丢任何东西——行的 `status` 未变，下一次 tick 的 `sweep_all` 会重来（这正是既有 boot 门自己的论证，`scheduler/mod.rs:985-990` 逐字："Until the gate opens this is a no-op; nothing is lost"）|

`context_sweep_boot_done` 是一个 `AtomicBool`，形状**逐字照抄**既有的
`boot_sweep_done`（字段 + `scheduler/mod.rs:992` 的读、`:1015` 的置位、
`:1026` 的测试读口、`:1031` 的测试 seam）。**不新增任何环境旋钮。**
原来的检查 b **整条删除**——它的职责已经被上面那一条规则接管，
而且接管得更彻底（它同时覆盖恢复漏斗与 gate，那两条 `resume_dispatched`
永远够不着）。

**这条规则的边界，逐条定价（不留隐含）：**

1. **判决尚未算出来时，没有人读得到它**——这是唯一残余的窗口，只出现在
   **boot**：如果"使闭包过期的那次编辑"发生在内核**停机期间**，重启时
   `context_stale_at_ms` 还是 `NULL`，恢复漏斗读到的就是"未过期"。
   两条便宜的缓解，**两条都做**：
   (i) **boot 顺序改为：上下文 sweep 排在 operation 恢复之前**（§5.3.1 相应
   改写；它只读 `cards.payload.blocks` 与 `tasks`，不重驱动任何东西，因此不受
   `sweep_all` boot 门那条理由约束）；
   (ii) `resume_dispatched` 的检查 a 把 scheduler 自己的漏斗也关到 sweep 之后。
   **如实记**：这是本设计里**唯一**一处 boot 顺序承载正确性（而且只对
   "停机期间发生的编辑"这一种情况）。因此 §11.2 不变量 5b 被重述为**对 boot
   顺序本身的断言**——将来谁重排 boot funnel，CI 会红，而不是静默失去保护。
   **判决已经落库的那种情况（含不变量 5 的标准构造：判 material 之后才
   `kill -9`）与顺序无关**——那正是上面这条规则的价值。
2. **`prepare_tx` 提交之后、spawn 之前的那一小段**，本规则够不着：op 已在
   `TxCommitted`，按定义"已经开始"。正常情况下这两步在同一次 `drive_one` 里
   相隔微秒；要观察到它，必须**恰好在这中间崩溃**，**并且**判决在停机期间落库。
   代价与 §6.5/§13.4 的"不打断已开始的工作"是同一条，记在 §13.24。
3. **gate 也被拒了，这是对 §6.5 承诺的收窄，必须明说**：判 material 之后
   **不再有新的 gate 执行**。§6.5 已按此改写并定价。

**(3) 不变量重述（§11.2 不变量 4/5/5b）。** 旧的不变量 5 是空洞的；r5 的重述
挡不住恢复漏斗与 gate；r6 的形状**直接说出上面那条规则**，因而是可机械检验的：

> **不变量 5（r6 重述）**：一条 `TaskContextAdvanced{verdict: material}`
> **提交之后**，该 task 上**任何进入 `prepare_tx` 的 operation 一律被拒**
> （`codex-/claude-/terminal-worker` 与 `task-verify` 四个 kind），
> 因而**不产生任何新的 worker、也不产生任何新的 gate 执行**；
> 已越过 `prepare_tx` 的 operation 照常跑完（§6.5）。
> **构造（这条不变量唯一有意义的测法是崩溃重启）**：dispatched 行 + material
> 判定 → `kill -9` → 重启 → boot 之后**不得**出现该 task 的新 worker 卡；
> 该 task 的 worker op 必须终结在 `failed`、其 `last_error` 含 `context-stale`；
> `tasks` 行必须落到 `failed`（`spawn-failed` + `TaskFailed.reason` 含
> `context-stale`），**不得**长期停在 `dispatched` 占住 `task_budget`。
> **第二个构造（gate 首启，r6 新增）**：`verifying` 行 + `gate_attempt = 0`
> + material 判定 → `kill -9` → 重启 → **不得**有任何 gate shell 命令被执行；
> 该行落 `failed`，`gate_result.log_tail` 含 `context-stale`。
> **第三个构造（r5 的 b1，验证我们没有过度收紧）**：op 已在 `SpawnStarted`
> + material 判定 → 重启 → 该 op **照常**被恢复驱动到终结、worker 照常汇报。

> **不变量 5b（r6 重述）**：**上下文 sweep 的 boot 轮在 operation 恢复漏斗
> 与 `sweep_boot` 之前完成。** 构造：用测试 seam 卡住上下文 sweep → 跑 boot →
> 断言 (i) 没有任何 operation 被恢复驱动、(ii) 没有任何 dispatched 行被
> `resume_dispatched` 重驱动；放开 → 两者照常发生、行不丢。
> *（r5 的写法只断言了 (ii)，而 (i) 正是 r6 发现 1 的那条路径。这条现在
> **就是**"停机期间的编辑"那个残余窗口的机制保证，所以它必须以 boot 顺序为
> 断言对象——顺序在这一处是承重的，写清楚比假装无关更安全。）*

**这一维为什么会连着两轮不闭合**（元教训，与 §14 的 r4/r5 元教训成对）：
r5 已经做对了一半——它点名了载体，也点名了一个读者。**它错在把"读者"当成
一个可以逐点补充的清单**：`resume_dispatched` 是当时想得起来的那个起活入口，
于是规则被写成了那个入口的性质，而不是"起活"这件事的性质。
新的自查规则比 r5 的更强一格：**凡出现"此后不得再 X"，不但要点名读者，
还要证明那个读者站在所有 X 的必经之路上**——如果做不到这个证明，
说明选的是调用点，不是漏斗。

### 5.4 #979 不阻塞本设计（评审第 4 条，确认）

- OCC 只要求"变更**事后可检测**"，不要求写方出示 `if_rev`；
- 事后可检测由 **冻结集里的 `content_hash`** 保证（§5.1 的修正——**不是**块级
  rev 单调性，那条在 id 回收与 `saturating_add` 两处都会漏报）。
  canonical flat text 由所有写路径共用的对齐器算出（`flat_text` /
  `comparable_flat`），所以整文档写路径同样被覆盖；
- 因此 #979 管的是 **writer-vs-writer 的丢更新**（两个整文档写互相静默覆盖），
  claim 的失效检测不需要它。

**结论不变，但耦合比初稿说的强。** 初稿写"唯一的耦合是块级 REST 必须带
`if_rev`"；§3.4 已修正为：**create / positional / move 需要 `if_doc_rev`，
而 `doc_rev` 正是 #979 引入的东西**。所以：

- 切片 1（纯 schema/校验，不落行）与切片 2（人的写口）**不阻塞**；
- 切片 2 的 create/move 端点**必须在 #979 合入后**才能带上 `if_doc_rev`。
  #979 已在兄弟 worktree 落地（§0.3），实践上不构成阻塞，但依赖关系要写实。

---

## 6. 人 / AI 的不对称

**机制上不该有区别**——同一个块 kind、同一个 `ready` 标记、同一条投影、
同一道就绪门。造两套机制是错的。语义上四处不对称，三处需要机制。

### 6.1 撤销权不对称 —— 墓碑（必须有机制，否则死循环）

人删掉 AI 声明的任务 → 正常且终局；AI 不得删人声明的任务。

**关键的坑**：人删掉 AI 声明的任务后，**AI 下次唤醒不能把它重新声明一遍**——
否则人删一次、AI 建一次，无限循环，**而且每一轮都合法**。这类缺陷在设计阶段
极难被看见，因为每一步单独看都对。

**初稿把这个机制挂在了错的路径上**（r1 通道 A BLOCKER，成立）：初稿的墓碑只在
人**主动写下**一个 `tombstone` 块时才触发，而 issue 点名的那个故障走的是
**删除**路径——人在 UI 上删掉那个块。§3.4 的 `DELETE .../blocks/{block_id}`
不产生任何墓碑，于是**默认路径上死循环完全没有缓解**，设计关掉的是另一条更麻烦
的路径（人特意去写一块墓碑）。

修复见 §3.7 规则 4：**`EditAuthor::User` 删除一个非墓碑 `task` 块 →
同一 op 内原位替换为该 `key` 的墓碑块**（执行者是 `normalize_report_op`）。
人做的还是"删除"，机制得到的是墓碑。
`reason` 缺省为空（UI 可以顺手问一句，但不强制——强制填理由就是在删除路径上加摩擦）。
**删除墓碑块本身是普通删除**，语义是"我撤回这次否决"，此后 AI 可以合法重提。

**"终止"这个词在 r1 里是错的**（r2 通道 A BLOCKER，成立）。r1 的投影把
`pending` 行翻成 `canceled`（`task_cancel_tx`），而 `canceled` 是**非 pending**，
§4.2 规则 2 对非 pending 行的声明变更只出诊断、永不写入 ⇒ 那个 `key` 被
**永久吸收**：删墓碑之后 AI 重提，得到的不是新任务，是一条永远的陈旧诊断。
**墓碑不是终止的，是吸收的**，而且这是默认路径（§3.7 规则 4 让每一次人的删除
都走它）。修复是 §2/§4.2 规则 1 的方向修订：**声明消失时守卫式删除那个从未派发过
的 `pending` 行**（`task_delete_pending_tx`），而不是把它翻成 `canceled`。
于是这条循环**真的**终止在"空"上，且增量路径与 rebuild 给出同一个终态：

| 步骤 | 文档 | `tasks` 行 | AI 能重提吗 |
|---|---|---|---|
| AI 声明 `impl-parser` | `task` 块，`ready:true` | `pending` | —— |
| 人删掉它 | 同 `key` 的**墓碑块** | **行被删除** | 否（墓碑块在，投影拒绝同 key 重声明）|
| 人删掉墓碑 | 空 | 空 | **是**（§4.2 规则 2b 的复活规则）|
| （若删除时已 dispatched）| 墓碑块 | 行保留、状态不动（§6.5）| 否——已派发过的 key 不复活，见 §4.2 规则 2b |

机制：**墓碑是一个 `task` 块，`tombstone` 非空，payload 是 §3.2 规则 7 的
封闭四字段形态**（r3 通道 A BLOCKER：r2 的示例既缺必填的 `declared_by`、
也缺 `kind`，而规则 7 当时没豁免 `kind` ⇒ 改写器造出来的墓碑会被同一次 persist
里的 `validate_payload` 拒掉）：

```jsonc
// 人否决一条 spec 声明的任务后，规则 4 原位改写出的规范形态：
{
  "key": "impl-parser",
  "tombstone": { "reason": "这条不做了，先只改 lexer" },
  "declared_by": "spec",      // 原样承接：是 spec 提出要做它的
  "tombstoned_by": "user"     // 是人否决的 —— 终局性的判据（r3 ⑨）
}
```

- 它是**声明层**信息，会沉淀进记录（"这件事我们决定不做"），按 §1 的判据进文档、
  不进事件日志；
- 投影时：`tombstone` 非空 → **不落 `tasks` 行**；若同 `key` 已有 `pending` 行 →
  **守卫式删除该行**（`task_delete_pending_tx`，同一事务内；r2 改：r1 写的是
  `task_cancel_tx` 翻成 `canceled`，见上）；若已 in-flight → §6.5；
- **同 `key` 的重新声明在投影层直接拒绝**（诊断："`impl-parser` 已被人否决，
  理由：…；要重开请换一个 key 并说明为什么"）。这一半是机制：墓碑块按 `key`
  落在文档里，投影按 `(wave_id, key)` 判定，§3.7 规则 2b/3 挡住改写与删除。
- **这条防线是 `key` 作用域的，而 `key` 由 spec 自选 ⇒ 换一个 key 就整条绕过；
  r5 为它采纳了一条与 `key` 无关的机制**（r4 通道 A MAJOR 提出事实并被如实记录，
  **r5 通道 A MAJOR / 通道 B BLOCKER 推翻了 r4 的"不造机制"裁决**，改设计方向 ⑰）。
  事实部分照旧成立，逐条核实过：墓碑按
  `key` 落块、§3.7 规则 3 按 `tombstoned_by == "user"` 保护**那一块**、
  §4.2 规则 2b 的复活规则按 `(wave_id, key)` 判定、§11.2 不变量 6 按 `key` 断言
  ——换 key 之后**没有一条命中**。于是 §6.1 开头点名的那条循环
  （"人删一次、AI 建一次，无限循环，而且每一轮都合法"）**在换 key 下原样复活**；
  `spec_task_ceiling` 也挡不住它（被删的 pending 行不留行，未结存量恒不增长，
  §13.18 已自承它挡不住细水长流）。**上一版把"spec 的系统提示里必须包含本 wave
  的墓碑清单"写成这一层的补救——那是 prompt，而本节自己刚判过"prompt 不是机制"。
  提示词保留（它有用），但不得被当作机制。**

  **r4 的"没有可用机制"是错的，r5 采纳机制**（⑰）。r4 只比较了一个候选
  （墓碑携带 `content_hash` / goal 指纹 + 相似度诊断）并正确地拒绝了它——
  相似度阈值是启发式，把安全判定挂在启发式上正是 §3.3 与 §3.7 ⑨ 决策表两次
  拒绝过的做法。**但"唯一便宜的候选是启发式"这个前提是假的**：本设计**已经**
  在为一条非启发式、**与 `key` 完全无关**的机制付钱——§6.6 的
  `automation_policy` + `released_by_user`（同在切片 3b）。裁决：

  > **一条未清除的 `tombstoned_by:"user"` 墓碑，使该 wave 对
  > `declared_by:"spec"` 的任务按 `declare-and-wait` 处理。**

  形式化（它是 §6.6 的**生效策略**，纯粹派生，见那一节）：

  ```
  effective_policy(wave) =
      waves.automation_policy                      若该列非 NULL（人显式设过）
      'declare-and-wait'                           否则，若文档里存在
                                                   tombstoned_by == "user" 的墓碑块
      'auto-declare'                               否则
  ```

  **它为什么终止那条循环**：循环的每一轮都需要"spec 声明 → 自动排队"。
  一次人的否决之后，**任何 key**（包括 spec 新造的 key）都必须等到人写下
  `released_by_user: true` 才落 pending 行——**人的不作为是吸收态**，
  这正是 §6.1 开头要求的终止性质。spec 仍然可以**提议**（§6.1 从不禁止提议，
  它禁止的是"绕过人的否决自动跑起来"），零相似度判断、零 key 依赖。

  **为什么它不是又一条启发式**：判据是文档里一个**精确的、机制强制的**事实
  （`tombstoned_by == "user"`，由 §3.7 规则 1/2b/3 保证 spec 写不了也删不掉），
  不是对"这是不是同一件事"的猜测。它**故意不去判断是不是同一件事**——
  它换了一个更弱但可判定的问题：**"这个 wave 里，人最近一次表达的是否决吗？"**

  **代价与 UX 后果，逐条定价（不粉饰）**：

  | 项 | 后果 | 判断 |
  |---|---|---|
  | 载体 | **零新列、零新字段**——`automation_policy` 从 `NOT NULL DEFAULT 'auto-declare'` 改为 **`TEXT NULL`**（NULL = 内核默认），与 `spec_task_ceiling INTEGER NULL` 及 `WavePatch` 的 double-option（`Some(None)` = 清回默认）**本来就是同一形状**（§6.6） | 采纳的边际成本≈0，全部在切片 3b 之内 |
  | 粒度 | **一次否决改变整个 wave 的自动化姿态**（对 spec 声明的任务），不只是被否决的那一条 | 不成比例吗？——一次人的否决是这个 wave 里**最强的一次人类信号**，把该 wave 的 spec 任务从"自动跑"降为"逐条放行"是**保守的那一侧**，且这正是 §6.6 为 `declare-and-wait` 设计的那一档。**如实记**：这是本设计里"一处否决、全 wave 收紧"的唯一一处 |
  | 已有的 pending 行 | 翻档之后，该 wave 内 `declared_by:'spec'` 且未放行的 **pending 行会因"不可调度"被 §4.2 规则 1 守卫式删除** | **如实记的真实代价**。它们是**从未派发过、不含任何执行史**的行（§2 的三条理由），人打开放行位即回来；in-flight 行**不受影响**（§6.5）。这与 §4.2 规则 4 "诊断非空即删 pending 行"是同一条纪律，不是新的一类后果 |
  | 可发现性 | 人必须能看出"为什么我的任务不排队了" | **机制上必须成对**：这一档的诊断文案专列一条——「本 wave 有一条未清除的人工否决（`key`: X），spec 声明的任务需要逐条放行；清除办法见下」——经既有的 `taskDiagnostics` 读端渲染（§4.2 规则 7）。**没有这条诊断就是静默降级**，与 r4 ⑭ 判过的那类问题同型 |
  | 人怎么清除 | 两条，都已经是既有机制 | (i) **删掉那块墓碑**（= §6.1 已定义的"撤回否决"）⇒ 派生条件消失，回到 `auto-declare`；(ii) **显式 PATCH `automation_policy = 'auto-declare'`**（§6.6 的写面）⇒ 显式设置**压过**派生值，于是人可以"保留否决记录，但恢复自动化"。两者语义不同且都需要，正是 `TEXT NULL` 三态的用处 |
  | 不做的事 | **不做"有界时间窗"**（通道 A 提到的备选） | 驳回：时间窗一到循环即恢复，**吸收态没了**；而且窗口需要一个时间戳 ⇒ 不是当前文档的函数 ⇒ rebuild 重建不出（与 §8(A) 驳回累计配额同一条理由） |

  **必须在切片 3b 之前定、但不阻塞切片 1–3a**：墓碑投影与策略列都落在 3b。
  §12 已据此写明。残余风险（这条机制**不**阻止 spec 反复提议、只阻止自动跑起来；
  以及"人一直不放行"与"人没看见"在机制上不可区分）记在 §13.22。
- 谁能立墓碑：**`tombstoned_by: "user"` 的墓碑是终局的**；
  `tombstoned_by: "spec"` 的墓碑视为普通的"取消自己的任务"，
  人可以直接删掉那个块把它撤回。**这是单写者原则在声明面的复现：
  人的声明是覆写级，AI 的声明是作者级。**
  （**判据在 r3 换成了 `tombstoned_by`**，理由见 §3.7 的 ⑨ 决策表：用
  `declared_by` 当判据时，"人删 spec 声明的任务"这条默认路径必然 400。
  载体仍然是**块自己携带的字段**（§4.4），不是"哪次编辑写的"——后者事后查不到，
  也 rebuild 不出来。spec 不能把一块人的墓碑改成 spec 的：§3.7 规则 2b 禁止改
  `tombstoned_by`、禁止原位改回非墓碑，规则 3 禁止 spec 删/改
  `tombstoned_by == "user"` 的块。）
- **因此 §11.2 的不变量 6 只对 `tombstoned_by: "user"` 的墓碑成立**——
  spec 墓碑可撤回，撤回后同 `key` 合法重现，它根本不是一条防线（r1 通道 A
  MAJOR，成立；不变量已限定）。
- **审计契约（NEW，r3 通道 B MINOR，成立）**：`pending` 行被守卫式删除后，
  `tasks` 表**不再保留**"曾经存在过一条从未派发的声明"这件事——
  既有的 `task_cancel_tx` 会留下 `status='canceled'` + `finished_at_ms`
  （`task.rs:157-170`），本设计的删除不会。**这是刻意的，且记录并没有丢**：
  > 该事实的持久记录是 **`WaveReportEdited`（携带 `body_before/after` 全文，
  > `crates/calm-types/src/event.rs:541` 一带的 payload 定义）+ 同事务的
  > `PlanUpdated{changed_keys 含该 key}`**，外加文档里那块墓碑本身。

  这两条事件**不会被裁剪**：`events_prune` 是**白名单**式的，可裁剪 kind 只有
  `claude.hook` / `codex.hook` / `harness.phase.changed` / `harness.item.added` /
  `overlay.set`（`crates/calm-truth/src/events_prune.rs:96-100`，逐字复核）。
  所以正确的表述是"审计留在事件日志与文档里"，**不能**说成"`tasks` 表保留了
  撤销历史"——它不保留。

### 6.2 预算不对称（把 §8 的缺口变具体）

人声明的任务有天然上限（人的打字速度）；AI 声明的没有，且会递归。
**树级预算与深度上限只约束 `declared_by = 'spec'` 的子树**，人声明的不受限。
既挡住失控展开，又不会在人明确要做二十件事时碍事。详见 §8。

### 6.3 未就绪时的处理不对称

- 人声明 + 缺验收标准 → **常态**，AI 的活就是补完（人本来只该写意图）。
  投影产出诊断"这条还排不了：缺验收标准"，spec 被 `WaveReportEdited{author:User}`
  唤醒（`dispatcher/mod.rs:95-97` 已有），补齐后打 `ready`。
- AI 声明 + 缺验收标准 → **不该发生**，是 AI 自身输出问题。机制上：
  **`author == EditAuthor::Spec` 且写入的 `task` 块 `ready: true` 而**块局部**
  校验不过 → `-32602` 当场拒**（写不进去，与 CRDT 合并语义无关，
  因为这是写入前的校验）。若 `ready: false` → 允许写入，作为草稿。
  **执行点是 §3.7 的 `guard_task_declarations`，不是块级工具的入口**——
  §0.2(a′) 已证 spec 可以用 `write_markdown` 绕过任何块级工具的入口校验。
  这是本设计里"同一道门必须守在收口"的第三个实例（前两个是归因与删改不对称）。

  **"校验不过"的范围必须限定为块局部**（NEW，r2 通道 A MINOR，成立）。
  §4.2 规则 3 的四条谓词里，只有 `gate_rule_violations` 是**块局部**的
  （看这一个块的 `gate` / `no_gate_reason` / wave 的 `require_task_gates`）；
  `dup_keys` / `unknown_deps` / `find_cycle` 都是**批级**的——它们的判定依赖
  文档里**别的**块。若把它们也放进写时门，一次并发的**人**的编辑（新增一个同 key
  的块、删掉一个被依赖的块）就能让 spec 的一次合法写入**非确定性地**被 400 拒绝，
  而"永不拒绝合并"正是本设计反复坚持的东西。裁决：

  | 谓词 | 写时门 | 投影诊断 |
  |---|---|---|
  | `gate_rule_violations`（块局部）| ✅ 拒 `ready:true` 的 spec 写 | ✅ |
  | `acceptance` 非空、`goal` 非空、payload schema（块局部）| ✅ | ✅ |
  | `dup_keys` / `unknown_deps` / `find_cycle`（批级）| ❌ **不拒** | ✅ 诊断 + 不可调度 |

  批级违规照旧由投影产出诊断并让该块不可调度——**后果一样（排不进去），
  但不会把别人的编辑变成你的写失败**。

同一道门，两种失败语义：对人是"我来补"，对 AI 是"你不该交这个"。

### 6.4 归因不对称

见 §4.4 的 `tasks.declared_by`。

### 6.5 in-flight 任务的删除（Q3 的真实答案）

§0.1 #14 已证：**"取消一个正在跑的 task"这条机制不存在**，`calm.plan.cancel`
明确拒绝（`plan.rs:985-994`），#653 的 parked 原语不提供它。

裁决：**不为本设计新建补偿取消。** 删块 / 立墓碑遇到 in-flight 行时：

- 投影**不删行、不改状态**（规则 1 的守卫式删除只赢 `pending`）；
- 产出一条可见诊断渲染回文档，**文案见下**——它必须把 ⑲ 的收窄一并说全，
  因为**删块正是触发那条收窄的范式情形**，不是它的例外。

**删块不是豁免情形，它是 ⑲ 的范式情形**（NEW，r7 通道 A MAJOR，成立；
**更正 r6 在这里写下的一处事实错误**）。r6 的原文说的是「删块 / 立墓碑那条
路径**不写** `context_stale_at_ms`，所以 gate 照常；**上下文失效走的是另一条
路**」——**那两条根本是同一条路**，本文自己的三处规定逐字为证：

1. **§5.1**：「**冻结集必然包含 task 块自身的
   `(wave_id, block_id, rev, content_hash)`，它是闭包遍历的根节点（深度 0）**」
   ——一个 in-flight 任务自己的块**永远在**它自己的冻结闭包里；
2. **§5.3.1** 的 sweep 判据把这一情形**逐字列了出来**：「因为内容变了、
   **块没了**、wave/cove 没了、越 cove、冻结集本身缺失——只要不能被验证为
   『与冻结值逐字节相同』，该任务一律判 `material`」；
3. **§4.2 规则 2(ii)** 已经把结论说出来了：「因为 task 块自身在它自己的冻结
   闭包里（§5.1），这次编辑**必然**被第 1 级机械检测捕获 → 走 §5.3 的裁决」。

⇒ **删除 / 墓碑覆盖一个 in-flight 任务的块 ⇒ 该任务的根冻结元组解析不到 ⇒
判 `material` ⇒ `context_stale_at_ms` 被写入 ⇒ 按 ⑲，该 task 上任何尚未进入
`prepare_tx` 的 operation 一律被拒，gate 也在内。** r6 那句「worker 会跑完，
其结果照常过 gate 并汇报」因此在删块这条路径上**恰恰最常不成立**：只有当
gate op 在删块发生之前就已越过 `prepare_tx` 的那个窄窗口里它才为真，其余
情况下 gate 被拒。（墓碑覆盖同理：墓碑块是**原位**改写，`content_hash` 必变。）

**因此本节的诊断文案（§12 切片 3c 的交付物）必须逐字说全三件事**：

> **`impl-parser` 正在执行（running），无法立即撤回。** 已经开始的那一个
> operation 会跑完——worker 卡、日志、产出全部保留并照常汇报。
> **但删掉这个块本身已经使该任务的声明上下文失效**：它**不会再获得一次新的
> gate 执行**。若 gate 尚未开始，该行落 `failed`，
> `gate_result.status_detail = "gate-infra"`、`log_tail` 含 `context-stale`。
> 已记录你的撤回意图；任务终结后不会重新声明，要重做请换一个 `key`
> （§4.2 规则 2b）。

**这与 §5.3.3 边界第 3 条、§5.3.3(2) 的 phase 表、§11.2 不变量 5(b) 是同一句话
的四处表述，四处必须一致**——本节此前是唯一说反了的那一处，而它恰好是唯一
驱动**用户可见文案**的那一处。

**为什么选这一侧**：`gate` 是 task 块 payload 的一个字段（§3.2），它与 `goal`、
`acceptance` 一起过期。在过期（或已被删除）的声明上跑一条真实 shell 命令、
再把结果写成"通过"，等于用一把过期的尺子出具一份看起来权威的合格证——这与
§5.3 第 1 级的 fail-closed 是同一条纪律。**代价如实记**：一个 worker 可能已经
产出了完全可用的东西，却因为声明在它跑的期间被改动**或删除**而以 `failed`
收场，人要重做必须换一个 `key`（§4.2 规则 2b）。这与 ⑲ 对"尚未开始的工作"的
一般处理同源，一并记在 §13.23 / §13.4。
- **同一条诊断也覆盖"守卫式删除返回 0 行"的竞态分支**（NEW，r3 通道 A MINOR，
  成立）：投影读到该行是 `pending`、发出 `DELETE … WHERE status='pending'` 时
  它已被 `task_claim_pending_tx`（同样 `WHERE status='pending'`，`task.rs:222`
  ← `scheduler/mod.rs:641`）认领 ⇒ 0 行。**0 行不是错误、不重试、不回滚**，
  它与"读到的就是 in-flight"是同一种情况，走同一条诊断。
- 墓碑块本身**保留**。**措辞更正（r3 通道 A MINOR，成立）**：r2 写的是
  "在任务终结后生效（下一次投影时该 key 不再落行）"，而终结后行是 `done/failed`，
  §11.1 规定非 pending 行永不删除、§4.2 规则 2b 又规定已派发过的 key 不复活
  ⇒ 那个"生效"**没有任何可观测后果**。准确表述是：
  > **墓碑此后只作为记录存在**（"我们决定不做/不再做这件事"）**并挡住同 key 的
  > 重新声明**；那一行执行史保持原样，该 key 本来也不会复活。

  不存在"任务终结的那一刻发生一次状态变化"这回事。

理由：这是 #644 明确划出的范围外事项；为本设计单独造一个跨 operation /
worker / gate 的补偿路径，是另一个设计的体量。**风险如实记在 §13。**

### 6.6 自动化程度作为 per-wave 策略

**先裁决 issue 自身的一处不自洽**（r1 通道 A MAJOR，优先级 H，成立）。

issue 的第二条评审**明确撤回**了"人逐条放行"这一步：「早先我提过『spec 起草草稿
→ 人逐条放行』，**也应撤回**……批准只剩摩擦」，并把 Q6 重铸成一道**机器门**：
「AI 必须先能把它变成可验收的，才能排进去」。本文 §10 Q6 忠实记录了这次撤回，
**而初稿的 §6.6 又用 `declare-and-wait` 这个名字把人逐条放行装回成默认**，
且没有给出"那次撤回是错的"的论证。两边都采纳是不允许的。

**裁决：忠于撤回。默认 `auto-declare`。** 论证：

1. **撤回的理由是对的。** 人逐条点"批准"不产生新信息——人在点之前并没有比
   `ready` 门更多的判据；它只把延迟加进每一条任务。这正是 #830
   （workers run headless）与 §5.3 第 3 级（人只在**实质变更**处出现）的同一条纪律。
2. **护栏不该由"人点一下"提供，而该由机制提供**，本设计恰好三层都有：
   §6.3 的写时机器门（spec 写 `ready: true` 而校验不过 → `-32602` 当场拒）、
   §8 的树级预算与深度上限、§3.5 的 in-wave 默认（AI 声明的任务不会自己长出
   新的 wave 树）。这三条都不需要人在场。
3. **反向的代价是不对称的**：默认等人 = 每一条任务都付摩擦；默认放行 = 失控展开
   风险，而失控展开**已经被预算机制封顶**。
4. Q5（谁选模板）**保持人选**，这不矛盾：选模板是**一次性、wave 尺度**的选择，
   与"每条任务放行一次"不是同一个量纲。§10 Q5 的理由行相应更正
   （初稿写"与 §6.6 的 declare-and-wait 默认同构"——那条理由随本裁决作废）。

于是策略列**保留**，但默认值翻转：

- **`auto-declare`（默认）** —— AI 自行声明即排队，受 §6.3 写时机器门 +
  §8 树级预算约束。
- **`declare-and-wait`（显式开启）** —— 高后果 wave 的选项：
  `declared_by: "spec"` 的块**即使 `ready: true` 也不落 pending 行**
  （诊断："本 wave 要求人确认后才排队"），直到该块的
  **`released_by_user == true`**（§3.2 的放行位）。保留它是因为"这个 wave
  值不值得逐条看"确实是 per-wave 性质；把它做成默认才是错的。

**放行必须有一个人可写、spec 不可写的独立载体**（NEW，r3 通道 A MAJOR，
成立，改设计方向 ⑪）。r2 写的是"人把 `ready` 改成 `true` 才落 pending 行"，
**这条自相矛盾且不可实现**：

1. 块上的 `ready` **本来就是 `true`**（spec 写的），"人改成 true"是**空操作**
   ——没有任何状态因此改变，投影拿不到任何新信息；
2. 判据若取 `declared_by`，§3.7 规则 2 又**禁止改它** ⇒ 高后果 wave 里
   spec 声明的任务**永远无法被人放行**，这一档策略在落地时是死的；
3. 让人把 `ready` 先改成 `false` 再改回 `true` 也不行——中间态会触发 §4.2
   规则 1 的删除，而且它把"放行"表达成了两次编辑的时序，rebuild 重建不出来。

裁决：**新增 `task` payload 的可选布尔位 `released_by_user`（§3.2）**，
由 §3.7 规则 5 强制"只有 `EditAuthor::User` 能写入或改变它"。于是：

- 它是**声明**（"人同意这条排进去"会收敛进记录，§1 判据），住文档、可 rebuild；
- `auto-declare` 下它**没有语义**（投影忽略它），所以默认路径零成本；
- `declare-and-wait` 下投影的可调度谓词多一个合取项：
  `declared_by == "user" || released_by_user == true`；
- 撤回放行 = 人把它改回 `false` ⇒ 走 §4.2 规则 1 的守卫式删除，与其它三种
  "不再声明为可调度"同构，rebuild ≡ 增量不受影响。

**两列必须有写面，否则策略无法被打开**（NEW，r3 通道 A MAJOR，成立）。
r2 只说了"新增 `waves.automation_policy` / `waves.spec_task_ceiling` 两列"，
**全文没有任何写入路径**——没有 PATCH 字段、没有 OpenAPI、没有 CLI。
现成的先例正是 #644 给 `task_budget` / `require_task_gates` 做的形状
（`crates/calm-truth/src/db/sqlite/wave.rs:168-187` 的注释逐字写明这个取舍：
**这两列刻意不上 `Wave` 结构体**，从而不动任何 `SELECT` 列表、不动
`WaveUpdated` wire payload、不动 ts-rs 导出；写面就是那两条**定向单列
UPDATE**，读面由调度器直接走 SQL；REST 侧的校验与空 patch 短路在
`crates/calm-server/src/routes/waves.rs:864-887`）。裁决：**照抄这个形状**——

- `WavePatch` 新增 `automation_policy: Option<Option<String>>` 与
  `spec_task_ceiling: Option<Option<i64>>`（`Some(None)` = 清回内核默认，
  与 `task_budget` 的语义一致）；
- `wave_update_tx` 内两条定向单列 UPDATE，**两列都不上 `Wave` 结构体**；
- REST 校验：`automation_policy ∈ {auto-declare, declare-and-wait}`、
  `spec_task_ceiling >= 0`，越界 → 400，文案与 `task_budget` 那条同构；
- `patch_has_other_changes` 的判空列表（`routes/waves.rs:880-886`）要加上这两个字段，
  否则只改策略的 patch 会被当成空 patch 短路掉、一个事件都不发；
- OpenAPI + zod + web 生成物同步（Tier-A）。**这一整块写进切片 3b**（§12）。

**但"照抄 `task_budget` 的形状"不能连它的 actor 面一起抄（NEW，r6 通道 A
MINOR，成立）——`automation_policy` 必须是 user-only。** 逐条对 HEAD 复核：

- `update_wave`（`crates/calm-server/src/routes/waves.rs:812-887`）**只对
  `lifecycle` 做 actor 闸**（`validate_transition(existing.lifecycle, to, &actor_id)`，`:849`）；
  `task_budget` / `require_task_gates` 对**任何** actor 都接受；
- 而 `X-Calm-Actor` 是**自述的**：`crates/calm-server/src/actor.rs:28-33`
  逐字写着 "**Not authenticated** … the `actor` field is a declared identity,
  not an authenticated one … **this file is plumbing, not a security boundary**"。

于是照抄之后会出现一处**自相矛盾的不对称**：§3.7 规则 5 花了整整一条 guard
规则让 `released_by_user` 对 spec **不可写**（因为它是"人同意这条排进去"的
唯一载体），而 `PATCH /api/waves/{id} {automation_policy:"auto-declare"}`
**一次调用就能把整个 wave 的否决清掉**——包括 ⑰ 那条由未清除的
`tombstoned_by:"user"` 墓碑派生出来的 `declare-and-wait`——却只受 REST 认证层
保护。逐块守住放行位、再留一个 wave 级的总开关不守，等于没守。

**裁决**：`automation_policy` 的写入加一条**镜像 `validate_transition` 形状**的
user-only 检查（非 `ActorId::User`（`crates/calm-types/src/ids.rs:73-87`）→
`CalmError::Forbidden`），与 lifecycle
那条同处 `update_wave`、同在写之前、同样"不落行也不发事件"。
**`spec_task_ceiling` 同样只允许人写**（它是 §8(A) 的存量护栏，spec 能改它
等于护栏由被约束者自己设定）；`task_budget` / `require_task_gates` 的既有行为
**不动**（那是 #644 的既定面，改它超出本设计范围，风险记在 §13.25）。

**这就是那条不对称被强制的地方**，写在这里以便被引用：
> 「人可写、spec 不可写」在本设计里有**两个**强制点，缺一不可——
> 块内的 `released_by_user` / `declared_by` / `tombstoned_by` 由 §3.7 的
> `guard_task_declarations` 守（那是文档写路径的唯一收口），
> wave 级的 `automation_policy` / `spec_task_ceiling` 由 `update_wave` 的
> user-only 检查守（那是 REST 写路径的唯一收口）。
> 两个收口对应两条写路径，**没有第三条**。

落在**切片 3b**（与两列同片；这两列在本设计之前不存在，所以这不是行为变更）。
§11.2 不变量 8 加一条否定测试：非 user actor PATCH 这两列 → **403**，且列值不变。

**形状抄 git-forge 的 `merge_policy: hold-for-ratify | auto-merge`
（`plugins/git-forge/manifest.json:291-295`）——两档 per-wave 策略。
但默认值不抄**（它默认 hold，本设计默认 auto，理由见上）；
**机制更不能抄**：§0.1 #15 已证 `merge_policy` 完全靠 `spec_instructions` 的散文
（`manifest.json:400`）强制，内核不参与。本策略必须是**内核强制**的，因为它
决定调度器是否动作。

**r5 修订（⑰）：这一列改为 `TEXT NULL`，并且"生效策略"是一个派生值。**
`NULL` = 人从未显式设过 ⇒ 走内核默认；而内核默认**不再是常量**，它多一个
由文档决定的分支（§6.1 的 ⑰）：

```
effective_policy(wave) =
    waves.automation_policy                若非 NULL（人显式设过，压过一切）
    'declare-and-wait'                     否则，若文档里存在 tombstoned_by == "user" 的墓碑块
    'auto-declare'                         否则
```

三条性质：(i) 它仍然是 **"当前文档 + wave 策略列"的函数**，因此
`evaluate_schedulability_tx`（§4.2 规则 3′）原地就能算——**不新增任何输入**，
块快照与该列它本来就都拿在手里；(ii) `tasks_rebuild_tx` 与增量、读端三条路径
自动一致（§11.1(1) 的谓词不用改措辞，只是"策略列"变成"生效策略"）；
(iii) `TEXT NULL` + `Some(None)` 清回默认，与 `spec_task_ceiling` 及 #644
`task_budget` 的 double-option 形状完全一致（`model.rs:147-174`），
**写面一个字都不用改**。

载体：**`waves.automation_policy TEXT NULL`（NEW 列；`NULL` = 内核默认，
见上面的派生规则）**，
与既有的 `waves.task_budget` / `waves.require_task_gates`（`migrations/0041_tasks.sql:37-38`）
同构——per-wave 策略列已有先例，不新造概念。在 `project_tasks_tx` 内读取并强制
（与 rule 6 读 `wave_require_task_gates_tx` 的位置完全一致，`plan.rs:781`）。
**这一列必须与投影同片落地（切片 3b，r2 前移）**：投影上线的那一刻就是声明第一次
能驱动调度器的那一刻，若策略列还在切片 6，中间**根本没有办法**把一个高后果 wave
切到 `declare-and-wait`——"默认放行 + 高后果 wave 可显式收紧"这个裁决的后半句
会有几片的空窗期（§12）。

---

## 7. 模板

### 7.1 拆分线

| workflow descriptor 字段 | 去向 | 依据 |
|---|---|---|
| `plan_template` | → **report 模板里的 `task` 块** | 今天它只被序列化进 spec 提示（`spec_harness_start_adapter.rs:229-235`），从不写行 |
| `gates` | → 模板里的 prose / `task.gate` | 今天明注 NEVER executed（`manifest.rs:229-234`） |
| `spec_instructions` | → 模板报告正文（prose 块） | 今天渲染进系统提示（`spec_harness_start_adapter.rs:222-228`） |
| `card_kinds` | **留在 plugin** | kind 命名空间注册，可执行 |
| `input_schema` | **留在 plugin** | #891 的 `workflow_input` 校验，可执行 |
| forge-action 执行语义（argv / idem_key / probe） | **留在 plugin** | `955-kernel-app-boundary.md` §2.3 的 ③ 通道 |

### 7.2 Q1 裁决：模板 = 任意 wave 的报告，wave 创建时 fork

不新建"模板 wave"实体、不给 cove 加文档载体。wave 创建时可选
`fork_report_from: "<wave_id>"`（NEW 参数，`POST /api/waves`）：在创建事务内，
用源 wave 的 report 卡的**块快照**（`payload.blocks`，改写过引用之后）
播种新报告的 `ReportDoc::from_payload`，**块 id 逐个保留、`rev` 从源承接、
零 `mint_id` 调用**。
*（r2 修正：r1 这里写的是"走一次 `Replace`，块 id 由 `reassign_ids` 在新文档里
重铸"，与 §7.3 改写后的裁决**相反**——§7.3 的整套 `neige://` 重写方案
（`#b_x` 那一半原样不动）**依赖 id 被保留**。§0.1 #13 同样已更正。全文以 §7.3 为准。）*

"这是一个模板"是一个 **kernel overlay 标记**，仅供 UI 列出候选；先例是 wave
创建时已经写的那条 overlay（`routes/waves.rs:662-675`）。
**用 `entity_kind: "view"` + `kind: "template"`**——r1 通道 A 指出初稿写的
`entity_kind: "wave"` 不是那条先例的形状：先例是
`plugin_id: "kernel", entity_kind: "view", entity_id: <wave_id>, kind: "layout"`
（`routes/waves.rs:665-668`），`entity_id` 已经是 wave id。沿用 `"view"`
就完全不需要核实一个新 entity kind 能否在 `route_scope`
（`crates/calm-truth/src/validation.rs:303-313`）里解析。**零新表、
零新权限面、零新概念、零新 entity kind。**

**fork 写入的归因与就绪状态（NEW，r1 通道 A MAJOR 补齐）。** 模板按定义就含
`task` 块，而模板恰恰是最可能带着 `ready: true` 出厂的制品——初稿没说 fork 写入
带什么 `EditAuthor` / `declared_by`，于是 fork 的首次 persist 会在 **wave 创建
事务内**投影出 pending 行，调度器立刻派发。裁决，两条都是强制的：

- **fork 强制把每个 `task` 块的 `ready` 降级为 `false`。** 没有任何东西是在
  "这次"被决定要做的；spec 的第一件事就是审阅模板任务并逐条打开。
  这也让"wave 创建事务里意外派发"在结构上不可能。
- **fork 强制把每个 `task` 块的 `declared_by` 改写为 `"spec"`。**
  理由：模板里的任务不是**这个人**为**这个 wave** 提的；标成 `spec` 把它们纳入
  §8 的树预算，是保守的那一侧。（若标 `user`，模板就成了绕过预算的后门。）
  `EditAuthor` 用 `User`（fork 由人在创建 wave 时触发），
  §3.7 规则 1 对 fork 这一次批量写**豁免**（它是复制，不是声明），
  豁免点写死在 fork 路径里，不是一个可复用的旁路。

**fork 必须自己重跑校验**（NEW，r2 通道 A MINOR，成立）。fork 在 wave 创建事务里
经 `ReportDoc::from_payload` 播种，**根本不经过 `apply_report_op`**——于是
`validate_body_fences`（`wave_report_guard.rs:28`）与 §3.7 的两个新函数
**一个都不会跑**在 fork 进来的内容上。而 fork 恰恰**还要改**这份 payload
（`ready:false`、`declared_by:"spec"`、重写 `neige://`），所以"源 wave 当初校验过"
不构成保证。裁决，三条都是 fork 路径自己的责任：

1. 对写入的**每一个** fence payload 跑 `validate_payload`（`kinds.rs:55`）——
   与 `validate_body_fences` 同一批规则，失败 → 整个 wave 创建 400；
2. 跑一次 `guard_task_declarations(before = [], after = 重写后的块, author = User)`
   的**规则 1 之外**的部分（规则 2/3/4′ 在空前态上平凡成立；规则 1 正是被豁免的
   那一条，因为 fork 强制写 `declared_by:"spec"` 而 `EditAuthor` 是 `User`）；
3. 豁免**只**豁免规则 1，且**只**在 fork 路径上——不新增任何"跳过 guard"的可复用开关。

权限与生命周期：fork 是**读源 + 写新**，源 wave 无副作用；源 wave 被删除后
已 fork 出的 wave 不受影响（复制语义，§7.4）。源 wave 必须与新 wave 同 cove
**或**在 system cove 下（system cove 已存在且对用户不可见，
`db/sqlite/cove.rs:54-74`）——避免跨 cove 泄漏。

### 7.3 fork 的 `neige://` 引用重写（机制缺口，不是开放问题）

fork 复制文本 ⇒ 文本里的 `neige://wave/<源 wave>#b_xxxx` 会**指回模板原文**
（跨 wave 引用，看起来还合法）。必须在 fork 的同一事务里重写。

**初稿的四步方案不成立**（r1 通道 A BLOCKER，成立）。它的第 4 步依赖
`reassign_ids_with_hints` 把重写后的文本钉回预先算好的 id，但
`align.rs:44-47` 的文档注释明写：**hint 在"该 id 不存在于旧块中"时被忽略**，
而 fork 目标是一份**全新**文档，旧块为零 ⇒ **每一个 hint 都会被丢弃**，
id 全部走 `mint_id`（`align.rs:352`，内容+序号哈希）。更糟的是步骤 1→4 是个
不动点：重写文本 ⇒ 内容变 ⇒ 新 id ⇒ 你据以重写的映射本身是错的。
这正是初稿自己说"做错是静默的"那一处。

**新方案：fork 保留源 block id，于是根本不存在 id 映射。**

依据（已逐行核对）：`ReportDoc::from_payload`（`wave_report_doc.rs:101-114`）
调的是
`reassign_ids(payload.blocks.as_deref().unwrap_or_default(), &split_body(&payload.body))`
——它**接受 `payload.blocks` 作为"旧块"**。初稿之所以踩空，是因为
`routes/waves.rs` 建 report 卡时用 `WaveReportPayload::initial()/new()`，
`blocks == None`；**但 fork 可以自己填**。既有测试
`from_payload_reuses_hint_block_ids`（`wave_report_doc.rs:897-914`）正是这条
路径的现成证明，注释写着 "PR1-derived ids survive the CRDT seed"。

于是 fork 的步骤变成三步，且**没有一步会出错**：

1. **在块层面重写，不在文本层面重写。** 取源 wave 的
   `payload.blocks`（`Vec<ReportBlock>`），逐块把
   `neige://wave/<src>#b_x` 改写成 `neige://wave/<new>#b_x`
   （prose 用 `scan_links` `report_links.rs:63` 定位、`task` 块用 §5.2 的字段级
   扫描 + `refs[]`）。**`#b_x` 那一半原样不动**——因为 id 会被保留。
   指向**其它** wave 的引用保持原样（那是真正的外部引用）。
2. 令新报告的 `payload.blocks = 重写后的块`、
   `payload.body = 这些块的 flat_text 顺序拼接`（`flat_text`
   `report_blocks/mod.rs:234`）。
3. `ReportDoc::from_payload` 走对齐：每个切片的 canonical 文本与它对应的旧块
   **逐字节相同** ⇒ 全部 LCS 精确匹配 ⇒ **id 保留、rev 沿用源值**，
   零 `mint_id` 调用。

**为什么这比初稿强**：整类"id 映射错了"的 bug 消失了——因为没有映射。
需要重写的只剩 `wave/<src>` → `wave/<new>` 这一半，它是一次纯字符串替换，
且替换目标（源 wave id）是已知常量。

**残留风险与它的测试**：步骤 2→3 依赖 "split_body(拼接的 flat_text) 与原块
一一对应"。这条**今天已经被生产依赖**——`persist_report_with_shadow` 的
CRDT 惰性初始化走的就是同一个 `from_payload(&current_payload)` 往返
（`wave_report.rs:515`）。但依赖不等于证明，所以 fork 必须有一条**硬测试**：

> `fork(D)` 之后新文档的块 id 序列 **== `D` 的块 id 序列**（逐个相等，同序）。

一旦某个 prose 块的 markdown 不以标题开头而与邻块粘连，这条测试会立刻红。
再加一条：fork 出的 wave 内部引用全部指向**新 wave id**、外部引用逐字节未变。
两条都必须有，因为**错了是静默的**（fork 出来的任务指回模板，仍然解析成功）。

**块 id 与模板重名不是问题**：`neige://` 引用永远携带 wave id，
`(wave_id, block_id)` 才是全局锚。同一个 `b_1f3a` 同时存在于模板与 fork 里，
互不干扰。

### 7.4 Q4 裁决：复制，不是引用

**复制**。模板改动不传播，但每个 wave 能自证当时按什么流程跑——与"产出与证据"
（#330）一致。引用会让正在跑的 wave 脚下换地板。传播需求用"模板已更新，
这些 wave 落后了"的提示解决（读时比对，不新建存储），不用共享可变状态解决。

### 7.5 Q5 裁决：人选，spec 可提议

人在 wave 创建时选（复用今天 `workflow_id` 的入口形状，`routes/waves.rs:355-363`）。
spec 可以通过 launchpad 提议（#951 已有 `calm.launchpad.propose` 的
提议-人确认形状），不是自己直接选。

### 7.6 与 #891 的关系：`workflow_input` **保留**，不消解

issue §5 猜测"走模板路线 #891 会大幅简化甚至消解"。**不成立**：模板 fork 复制的是
**文档**，`workflow_input` 传的是**本次运行的参数**（issue_url / repo /
merge_policy），两者正交。一个 issue-development 模板 fork 出来后，仍然需要
知道这次跑哪个 issue。`workflow_input` 的校验入口（`routes/waves.rs:368`）
原样保留。

---

## 8. 预算：树级 + 深度上限

**今天的事实**：per-wave `task_budget`（默认 1，`scheduler/mod.rs:80`；
per-wave 覆盖列 `waves.task_budget`，`migrations/0041_tasks.sql:38`；
`compute_ready` 用它算 capacity，`scheduler/mod.rs:164-191`）+ dispatcher 全局
信号量（默认 8 permits，`dispatcher/mod.rs:55,666`）。**没有任何东西约束
"子 wave 再 spawn 子 wave"的递归展开。**

**r2 补的一条同样重要的事实**：`task_budget` **不是存量上限**。
`compute_ready(tasks, budget)`（`scheduler/mod.rs:164-191`，逐字复核）先数出
`dispatched|running|verifying` 的 `running_cost`，再
`capacity = (budget - running_cost).max(0)`，最后对 `pending` 行
`.take(capacity)`。**它是并发容量。** 一个失控的 spec 在**单个 wave 内**声明
500 条 ready 任务，不会被 `task_budget` 挡住任何一条——它们会以并发 1
串行地一条条跑下去，直到跑完为止。r1 的 §12 用"递归展开不可能"为切片 3
辩护，但递归只是失控的一种形状；**单 wave 内的未结存量**是另一种，而且它不需要
sub-wave 就能发生。

裁决分两层，**分别在不同切片落地**：

**(A) 单 wave 的未结存量上限（切片 3b，与投影同片）**

- **`waves.spec_task_ceiling INTEGER NULL`（NEW 列）** —— 语义：
  该 wave 内 `declared_by='spec'` 且 `origin='block'` 的**非终结**行数上限。
  默认常数 `DEFAULT_SPEC_TASK_CEILING = 32`（NEW，与树预算取同一个数量级）。
- **它不是"一共能声明多少"的上限，本文不再这样称呼它**（r3 通道 B MAJOR，
  成立）。谓词只数非终结行（`status NOT IN ('done','failed','canceled')`，
  而终结行是持久留存的，`task.rs:405,541,586` 三条跃迁都写 `finished_at_ms`
  且从不删行），所以一个自动化 spec 完全可以**等前一批跑完再声明下一批**，
  如此往复无穷——`compute_ready`（`scheduler/mod.rs:164-191`）会一直消费。
  准确表述：
  > `spec_task_ceiling` 约束的是**任一时刻的未结存量（outstanding backlog）**，
  > 不是生命周期总量。它挡住的是"一次性声明 500 条排到天荒地老"，
  > 挡不住"每完成一条就再声明一条"。

- **为什么不改成真正的累计配额**（本轮驳回的补救手段，理由是结构性的）：
  累计配额需要一个**单调计数器**（例如 `waves.spec_task_declared_total`），
  而它**不是当前文档的函数**——`tasks_rebuild_tx` 重建不出它（§11.1），
  于是它要么住进事件日志（那就得在每次 rebuild 时扫全量 `PlanUpdated` 求和），
  要么成为 §2 承重墙上的第三个真源。为了一个尚未观测到的失控形状去动那堵墙，
  代价不对称。**替代**：把"每 wave 的 spec 声明速率"列进 §5.3 的可观测量
  （单位时间内该 wave 新增的 `declared_by='spec'` 行数），
  真出现"细水长流式失控"时，那条曲线会先说话；届时再上速率闸或 epoch 配额，
  它们都可以是纯粹的运行时机制，不必进声明真源。风险记在 §13.18。
- 执行点在 `evaluate_schedulability_tx` 内（因而在 `project_tasks_tx` /
  `tasks_rebuild_tx` / 读端三条路径上同一份实现），与 rule 6 同一位置同一事务
  （`plan.rs:781` 的先例）。**谓词的形状是"对声明集合的确定性准入"，
  不是裸 `SELECT count(*)`**（r5 ⑯ / r6 ⑳，§4.2 规则 3″ 给出完整定义、
  两个反例与上界证明）。**判据一句话：`pending` 行永远是输出，在飞行永远是输入**：
  一条 `SELECT count(*) … WHERE wave_id=? AND declared_by='spec'
  AND origin='block' AND status IN ('dispatched','running','verifying')`
  得到 `occupied`（**不看 key 是否仍被声明、不看有无诊断**），
  `capacity = max(ceiling − occupied, 0)`，再把"只差 ceiling 这一关"的候选按
  **块序 + key 升序**在 `capacity` 内准入。超出的声明**不落 pending 行 +
  产出诊断**，不报错（§4.2 规则 4）。
  *（r4 写的是裸 `count(*)`——不幂等，它把正在被重新求值的行也数进去 ⇒
  一次 rebuild 会把已经落地的行判成超限并删掉，下一次编辑又让它们回来；
  两个通道在 r5 各自命中。**r5 的修法把排除集定义为"当前声明集合且其余诊断
  为空"，两个方向都错**：带诊断的在声明 key 被数进去（不幂等仍在），
  而仍被声明的**在飞行**被排除出去（上界被破，§11.2 不变量 7b 当场为假）；
  两个通道在 r6 又各自命中一次。r6 的修法不再按"诊断"分集合。）*
- **它必须与投影同片落地**：投影是"声明开始有后果"的那一刻，未结存量上限是它
  唯一的非并发护栏。人声明的行不计入（§6.2）。

**(B) 树级预算与深度（切片 6，与 `spawn: "sub-wave"` 同片）**
（只在 `spawn: "sub-wave"` 存在时才需要，§3.5 已把它降为可选）：

- **`waves.parent_wave_id TEXT NULL`（NEW 列）** —— 建立树。今天 wave 之间只有
  `neige://` 反链，没有结构化父子关系。
- **`waves.tree_task_budget INTEGER NULL`（NEW 列）** —— 只在树根有意义；
  子 wave 继承根的值。语义：**整棵树内 `declared_by = 'spec'` 的
  非终结任务行总数上限**。默认值先取一个保守常数（建议 32），
  与 `DEFAULT_WAVE_TASK_BUDGET` 一样可 per-wave 覆盖。
- **深度上限**：常数 `MAX_WAVE_TREE_DEPTH = 3`（NEW）。超限 → 子 wave 创建被拒，
  在父报告里渲染成可见待办。
- **只约束 AI 声明的子树**（§6.2）：`declared_by = 'user'` 的行不计入树预算，
  人显式创建的子 wave 不计入深度。**这条的可信度完全取决于 `declared_by`
  不可伪造、且能从文档重建**——两者分别由 §3.7 规则 1/2 与 §4.4 的载体迁移
  （块 payload）保证。初稿把 `declared_by` 只放在投影列里时，本节的预算
  在任何一次 rebuild 后就失效了（r1 两个通道同时命中）。
- **执行点**：在 `project_tasks_tx` 内，与 rule 6 同一位置同一事务（`plan.rs:781`
  的先例）。超预算的任务不落 pending 行 + 产出诊断，**不报错**（§4.2 规则 4）。

**为什么不是全局配额**：`955-kernel-app-boundary.md` §1.1 判据 3 说配额只能有
一个负责人——dispatcher 的全局信号量已经是那个负责人（限并发）。(A) 与 (B) 限的是
**未结存量与递归深度**，是不同的量纲，不冲突。三者的分工写清楚：

| 机制 | 量纲 | 载体 | 何时落地 |
|---|---|---|---|
| dispatcher 全局信号量（8 permits）| 全局并发 | 既有（`dispatcher/mod.rs:55`）| 已有 |
| `waves.task_budget`（默认 1）| **单 wave 并发** | 既有（`0041_tasks.sql:38`）| 已有 |
| `waves.spec_task_ceiling`（默认 32）| **单 wave 未结存量**（spec 声明，非终结行）| NEW | **切片 3b** |
| `waves.tree_task_budget`（默认 32）+ `MAX_WAVE_TREE_DEPTH = 3` | **树内未结存量 + 递归深度** | NEW | 切片 6 |

---

## 9. 迁移

现有 wave 的 `tasks` 行由 `calm.plan.upsert` 写入，**没有对应的 `task` 块**。
plan 表降为投影后，一次 `tasks_rebuild_tx` 会把它们全部抹掉。这是 #973 教训
（迁移安全是第一现实风险）在本设计里的对应物，issue 一字未提。

裁决：**双向迁移，声明侧一次性物化，状态侧原样保留。**

1. **新增 `tasks.declared_by TEXT NOT NULL DEFAULT 'spec'`**（migration NEW）。
   存量行全部标 `spec`——它们确实全部来自 `calm.plan.upsert`（§0.4 已穷举证明）。
2. **新增 `tasks.origin TEXT NOT NULL DEFAULT 'legacy'`**（NEW，取值
   `legacy | block`）。**投影只管理 `origin='block'` 的行**：
   - `tasks_rebuild_tx` 的删除阶段**只删 `origin='block'`、当前文档不再声明、
     且 `status='pending'`（从未派发过）的行**——三个条件缺一不可，见 §11.1；
   - `origin='legacy'` 的行对投影是只读的，scheduler 照常调度它们。
   这条保证"重建不会抹掉存量"是**结构性的**，不是靠迁移脚本跑对一次。
3. **同 key 的收编规则（NEW，r1 通道 B MAJOR 补齐）。**
   `tasks` 有 `UNIQUE (wave_id, key)`（`crates/calm-truth/migrations/0041_tasks.sql:27`，
   `0058_tasks_kind_claude.sql:33` 重建后同样）。初稿说 legacy 行对投影"只读"，
   于是一个声明同 `key` 的 `task` 块**既插不进去也没有规则**——INSERT 会撞唯一键。
   裁决：**收编（adopt），不插入**。投影遇到 `(wave, key)` 已有 `origin='legacy'` 行时：
   - 行是 `pending` → **原地收编**：`origin` 翻成 `block`，声明列按块全量覆盖，
     状态列一字不动。同一条 UPDATE，同一事务；
   - 行**非** `pending` → 同样把 `origin` 翻成 `block`，但**声明列不动**
     （规则 2 的 stale 路径）；若块的声明与行不一致，产出 stale 诊断；
   - 任何情况下**都不 INSERT**，所以唯一键永远不会被撞到。

   收编是**自动且幂等**的——这让第 4 项的物化工具从"迁移必需品"降级为
   "让存量任务在报告里看得见"的便利工具。
4. **物化工具（可选、人触发）**：admin CLI 一条命令，把某个 wave 的
   `origin='legacy'` 行渲染成 `task` 块追加到报告末尾。
   **只写声明字段**（`key / kind / goal / acceptance / gate / depends_on /
   priority / cwd / context / declared_by: "spec"`）+ `ready: true`；
   **不写任何状态字段**——`task` 的 schema 里根本没有 status 字段（§3.2），
   状态由 §3.6 读时合成。（r1 通道 A MINOR：初稿的"渲染成带当前状态的块"
   有歧义，可能被读成把 status 写进 CRDT，那是 §2 明令禁止的。）
   写完之后行被第 3 项的收编规则自动翻成 `origin='block'`，因此工具本身
   **不需要**一条"先追加块再翻 origin"的特殊有序原子路径——它只写报告，
   收编发生在同一次 persist 的投影里。**不自动跑**：它会改动人的报告正文，
   必须是显式动作。

   **它需要与 fork 同类的规则 1 豁免，且理由不同**（NEW，r3 通道 A MAJOR，
   成立）。物化工具经普通报告写路径落地，于是撞 §3.7 规则 1：
   `EditAuthor::User` ⇒ 必须写 `"user"`；`EditAuthor::Kernel` ⇒ **一律拒绝**
   （fail closed）。而它必须写 `declared_by:"spec"`——否则存量任务整体逃出
   §8 的未结存量上限与树预算（"迁移一次 = 洗白全部归因"是个后门）。裁决：
   - **以 `EditAuthor::User` 写、`declared_by:"spec"`，规则 1 在该路径上豁免**；
     这是全设计**第二个也是最后一个**规则 1 豁免点（第一个是 fork，§7.2），
     同样**写死在该路径里**，不新增任何可复用的"跳过 guard"开关；
   - 两个豁免点必须在同一处枚举、同一批测试覆盖（切片 7 验收）；
     规则 2/3/4′/5 **不豁免**——工具追加的是全新块，它们在空前态上平凡成立。

   **`ready: true` 与 fork 的 `ready: false` 方向相反，这是对的**（同一条发现的
   后半）。fork 强制 `ready:false` 是因为"模板里的任务不是**这次**被决定要做的"；
   物化工具面对的是**已经存在于 `tasks` 表里的行**，其中 `pending` 行正等着被调度。
   若写 `ready:false`，§4.2 规则 1 会**当场删掉那条活的 pending 行**——
   一个本意是"让存量看得见"的工具会顺手取消存量任务。所以：
   > 物化工具对每一行写 `ready: true`。它的语义是"这条声明现在由文档承载，
   > 与它此前的状态一致"，不是"现在决定要做它"。

   非 pending 的行不受 `ready` 影响（永不删除，§11.1(1)），标 `true` 只是让
   收编后的声明与行一致，不产生任何状态变化。
5. **`claim_context_json` 必须在同一个 migration 里被 backfill**
   （NEW，r5 通道 A MAJOR，成立，改设计方向 ⑱）。此前 §9 只覆盖了
   `tasks.declared_by` / `tasks.origin` 的默认值，而 **`claim_context_json`
   是以 `TEXT NULL` 引入的新列（§5.3），全文没有任何 backfill**，同时切片 3a
   **在同一个 PR 里既加这一列又上 sweep**。后果是构造性的、必然发生的：
   > 部署那一刻，所有 `status IN ('dispatched','running','verifying')` 的行都是
   > `claim_context_json IS NULL` —— 按 §5.3.1 那是**"缺失"而不是"空"** ⇒
   > **首次开机 sweep 把升级期间每一个在飞任务判 `material`**。

   这不是边角情况，是一次**必然的批量误判**；而叠加 §5.3.3 的强制点（⑮）之后，
   它会变成一次**跨升级的必然 Stuck-ops 事件**（每条未开始的 dispatched 行被
   `fail_spawn` 成 `failed`）。修法是一行，而且已经被设计自己的"空 ≠ 缺失"
   规则蕴含——**这些行按构造就是 legacy：它们在加列之前被 claim，根本没有闭包**：

   ```sql
   -- 与 ALTER TABLE tasks ADD COLUMN claim_context_json 同一个 migration 文件
   UPDATE tasks SET claim_context_json = '[]'
    WHERE claim_context_json IS NULL
      AND status IN ('pending','dispatched','running','verifying');
   ```

   终结行不必 backfill（sweep 不看它们）。**`context_stale_at_ms`（§5.3.3）
   不需要 backfill**：`NULL` 在那一列上的语义正是"从未被判 material"，
   对存量行是正确的默认。
   （纪律提醒：这条 backfill 必须写进**新**的 migration 文件；
   **已发布的 migration 一律不可编辑**——sqlx 对整个文件做 checksum。）
6. **不做的事**：不在服务器启动时批量重写报告；不删任何 legacy 行；
   不改 `crates/calm-truth/migrations/0041_tasks.sql`（已发布的 migration
   一律不可编辑——sqlx 对整个文件做 checksum）。

**关于 `replay.rs:363`：初稿的说法是编造的，此处删除并换成事实。**
初稿写"它删完之后从事件重放，而声明的重放源变成了文档——这正是 §11.1 断言要
覆盖的路径"。逐行读过之后：`replay.rs:363` 位于 `reset_from_fixture`
（`crates/calm-server/src/replay.rs:325`），是一条**仅用于 fixture 的
dev/e2e 重置**；它按 FK 顺序清掉
`events / retention_meta / overlays / terminals / cards / tasks /
worker_sessions / waves / coves / …` 然后 `seed_events(fixture)`。
它**不**从事件重放 `tasks`（`replay.rs` 里没有任何东西重建 `tasks`），
而且它**连 `cards` 一起删**——所以没有任何报告文档存活下来供声明重放。
正确表述：**fixture 重置把两个真源一起清空，因此与 §11.1 的 rebuild 断言正交**，
既不受它影响也不覆盖它。

**但它有一件必做的事**（NEW，r3 通道 A MAJOR 的后半，成立）：`replay.rs:350-372`
的表清单必须加上 **`DELETE FROM task_ref_index`**（§5.3 的新表）。
该表与 `tasks` 一样**没有 FK**，删 `waves` / `tasks` 不会级联到它——
清单里的 `tasks` 那一条旁边写着的正是这个理由（"deliberately has no FK to
`waves`… 必须显式点名，否则 task 行会跨重置泄漏"）。同理，
`wave_delete_tx` / `cove_delete_tx` 也必须显式清理它（§5.3 的生产者清单）。

---

## 10. 开放问题裁决记录

| # | 问题 | 裁决 | 理由 | 什么能推翻它 |
|---|---|---|---|---|
| **Q1** | 模板放在哪个 wave | **fork 任意既有 wave 的报告；"是模板"只是一个 kernel overlay 标记** | 零新概念、零新存储、零新权限面；overlay 标记有 layout overlay 的先例（`routes/waves.rs:661`） | 若出现"模板需要独立于任何 wave 的生命周期"的真实需求（例如模板要跨 cove 共享），则需要独立载体 |
| **Q2** | `calm.plan.upsert` 废弃还是保留内部 shim | **从工具面移除，形状完全复刻 `calm.task.dispatch` 的隐藏 shim**（`emit.rs:85-124`：`visible_to_roles: &[]`，零写入，返回迁移指引）。**`calm.plan.list` 保留**（读口，spec 需要状态）；**`calm.plan.cancel` 保留**（in-flight 拒绝语义仍是唯一的取消入口，§6.5） | 调用者已穷举：**只有 MCP spec agent**——无 REST 端点、无 web 调用者（`web/` 仅注释提及）、无 plugin 写口、无 admin CLI（§0.4）。所以"移除"没有隐藏的破坏面 | 若发现某条恢复/重放路径依赖 upsert 的写能力（目前证据是没有），则退回保留内部函数、只隐藏工具 |
| **Q3** | 块 id 作为 task 身份 | **否。身份是 payload 的 `key`；block id 只是引用锚**（§3.3）。删 in-flight 任务**不走补偿路径**——该机制不存在（§0.1 #14），改为记录撤回意图 + 可见待办（§6.5） | block id 由启发式对齐铸造（`align.rs:358`），整文档重写下不稳定；fence 不携带 id（`fence.rs:6-8`）所以写方无法指定 | 若 `key` 的重复率在真实使用中高到让人烦（复制粘贴场景），考虑在块级写口自动 uniquify |
| **Q4** | 模板引用 vs 复制 | **复制** | 与 #330"产出与证据"一致；引用会让正在跑的 wave 脚下换地板 | 若"模板一改、所有在跑的 wave 立刻跟进"成为真实且高频的需求 |
| **Q5** | 谁选模板 | **人选**（复用 `workflow_id` 入口形状）；spec 经 launchpad 提议（#951 先例） | **理由已更换**（初稿写"与 §6.6 的 declare-and-wait 默认同构"，而 §6.6 的默认已翻转为 `auto-declare`，那条理由作废）。新理由：选模板是**一次性、wave 尺度**的选择，与"每条任务放行一次"不是同一量纲；wave 创建本来就是人的动作，在它上面多选一个下拉不产生额外摩擦 | 若 spec 提议模板的接受率高到人只是在盲点头，说明该放开 |
| **Q6** | 就绪判据落在哪 | **门是"AI 必须先能把它变成可验收的，才能排进去"，不是"人批准 AI 的草稿"。标记是文档里的文本字段 `ready`**，投影在同一事务里生效，**无滞后**（§4.1）。**`automation_policy` 默认 `auto-declare`**（§6.6 已裁决 issue 自身的不自洽） | 评审第 2 条自我更正：批准步骤解决的是"AI 判断什么是任务"的误判风险，而人一旦写下就不是猜。文本标记不构成"滞后的第二个写口"，因为投影是同事务的。**初稿的 §6.6 把被撤回的"人逐条放行"以 `declare-and-wait` 默认的形式装了回去，与本行冲突；r1 通道 A 指出后已按"忠于撤回"裁决** | 若观测到 `ready: true` 但仍产出垃圾的比例高，说明 `acceptance` 的校验太弱（今天只校验非空），需要更强的可执行性判据 |
| **§3.5** | 一个 task 一个子 wave | **否，默认 in-wave**；`spawn: "sub-wave"` 为显式选项 | 成本已算（§3.5 表）：子 wave 的边际成本主要是**一个常驻 spec LLM 会话** + 独立 CRDT 文档 + 独立 vcs 链，在几十个小任务上是数量级差异。且"必然推论"的前提（单 workflow 绑定）恰恰被本设计削弱 | 父报告被 worker 产出淹没到不可读，或 per-wave budget 成为无法通过配置解决的吞吐瓶颈 |
| **§6** | 树级预算 + 深度上限 | **`waves.parent_wave_id` + `waves.tree_task_budget`（默认 32）+ `MAX_WAVE_TREE_DEPTH = 3`，只约束 `declared_by='spec'` 的子树** | 与 per-wave `task_budget` 同构的 per-wave 列，先例已在（`0041_tasks.sql:38`）；人不受限是因为人的声明有天然上限 | 默认值 32/3 是猜的；上线后按 §5.3 的可观测量调 |
| **新** | 人的块级写口 | **新增块级 REST（§3.4）；`if_doc_rev` 守 create/positional/move，`if_block_rev` 守 update/delete** | 不补则本设计的人机叙事全部落空（§0.2(a)）。两种 rev 是因为块级 rev 守不住"块还不存在"与"改的是 order"（r1 通道 B） | —— |
| **新** | 冲突裁决要不要落事件 | **要，新增 `TaskContextFrozen` + `TaskContextAdvanced`（NEW），均 kernel-only** | 冻结集是状态，真源必须是事件日志否则 rebuild 不出（§5.3）；裁决结果不落事件就无法排查"为什么 agent 拿着过期上下文产出了东西" | —— |
| **新** | `declared_by` 住哪 | **住块 payload（文档）**，`tasks.declared_by` 降为它的投影副本 | §1 判据：「是谁提出要做这件事」会收敛进记录 ⇒ 它是声明。初稿放在投影列里，导致 rebuild 重建不出、§8 预算失守（r1 两通道，§4.4） | 若出现"归因必须能被事后修正"的需求（今天认为不该有） |
| **新** | 写路径的收口 | **新增 `guard_task_declarations`，在 `apply_report_op` 对所有 op 变体调用（§3.7）** | §0.2(a′)：`write_markdown` 不受 stomp guard 约束，spec 今天就能任意改删非 prose 块 ⇒ 入口校验一律可绕 | 若将来 `write_markdown` 被收紧到与 `Replace` 同等，可重新评估收口位置 |
| **新**（r2）| 写路径的收口是**一个函数还是两个** | **两个：`normalize_report_op`（应用前的 op 改写器）+ `guard_task_declarations`（前后态校验器）**（§3.7） | 返回 `Result<(), _>` 的校验器无法执行规则 4 的"删除 → 原位墓碑"——那是变更不是判定；且 `apply_report_op` 的分支在被调用时已经改完了 `doc`（`wave_report.rs:131-215`） | 若将来 `ReportDocOp` 增加一种内核可以直接构造的复合 op，改写器可能被它取代 |
| **新**（r2）| 声明消失时 `pending` 行怎么处理 | **守卫式删除（`task_delete_pending_tx`），不是 `pending → canceled`** | `canceled` 是非 pending ⇒ §4.2 规则 2 会**永久吸收**该 key，人删一次任务就再也无法重提，而 §6.1 明确承诺可以（§2）。删除同时让 §11.1 的 rebuild≡增量第一次真的成立 | 若出现"必须能查到某个从未派发的任务曾被撤销过"的需求——但那份记录本来就该在墓碑块与事件日志里 |
| **新**（r2）| task 块自身算不算它自己的冻结闭包 | **算，它是闭包的根（深度 0）** | 否则改一个 in-flight task 自己的 `goal`/`acceptance`/`gate` 不触发任何失效判定，而那正是 issue §5 点名的阻塞需求 | 若误报（改 `priority` 也判 material）在实测中成为主要成本，可把闭包根收窄到进 prompt 的字段子集 |
| **新**（r2）| 引用能不能跨 cove | **不能。同 cove + system cove** | 裁决会把被引用块的前后内容递给任务所在 wave 的 spec；项目每一处跨 wave 读时派生都刻意 cove 内（`report_backlinks.rs:106-130`），§7.2 也防了 fork 的跨 cove 泄漏 | 出现真实的跨 cove 引用需求时，先设计"裁决观测可以携带什么"，再谈放开边界 |
| **新**（r2/r3 改述）| 单 wave 内 spec 声明的**未结存量** | **`waves.spec_task_ceiling`（默认 32），与投影同片落地（切片 3b）；它约束存量，不约束生命周期总量** | `task_budget` 是**并发容量**不是存量上限（`compute_ready` 的 `capacity = budget - running_cost` 后 `take`，`scheduler/mod.rs:164`）；失控 spec 可声明无界 pending 行串行排下去。r3：累计配额被驳回，因为单调计数器不是文档的函数、rebuild 重建不出（§8(A)）| 默认值 32 是猜的；若"每完成一条再声明一条"的失控在速率曲线上出现，加运行时速率闸 |
| **新**（r3）| 墓碑的权属载体 | **独立的 `tombstoned_by`，`declared_by` 原样承接** | 用 `declared_by` 当载体时，"人删 spec 声明的任务"这条**默认路径必然 400**（规则 4 改写 vs 规则 2 冻结，两个通道交叉命中）；(a) 给规则 2 开豁免会把全称不变量降级成条件规则，(b) 拆成 Delete+Insert 要把安全规则挂在对齐器的 id 铸造启发式上（§3.7 的决策表）| 若将来出现"归因必须能被事后修正"的需求，两个字段可以合并——但那要先推翻 §4.4 |
| **新**（r3）| `declare-and-wait` 的放行载体 | **新增人可写、spec 不可写的 `released_by_user`；两列补上 `WavePatch` 定向单列写面** | r2 的"人把 `ready` 改成 `true`"是空操作（`ready` 本来就是 `true`），判据 `declared_by` 又被规则 2 冻住 ⇒ 这一档策略落地时是死的；两列此前没有任何写入路径（§6.6）| 若 `declare-and-wait` 在真实使用中从未被打开，整档策略与放行位可一并删除 |
| **新**（r5，**r6 换了强制点**）| 判 `material` 之后**凭什么**不再起新工作 | **持久载体 `tasks.context_stale_at_ms`（`TaskContextAdvanced{material}` 的投影列）+ 一条规则：过期判决禁止该 task 上任何 operation *开始*，强制点是四个 task 绑定适配器的 `prepare_tx`**（三个 worker kind + `task-verify`）；终结与 gate 失败都走**既有**路径（`fail_spawn` / pre-bump 失败臂） | 「不得再产生新的 `TaskDispatched`」在代码里恒真且无用（`scheduler/mod.rs:692` 唯一发射点，行永不回 `pending`）。**而 r5 的替代品选错了落点**：`resume_dispatched` 不是唯一会起活的东西——operation 开机恢复（`operation/driver.rs:1010-1024`/`:1043-1055`）、`submit` 先插行后 drive 造成的 `Pending` 窗口（`:105-123`）、gate 首启（`scheduler/mod.rs:1541-1581`）三条都从它旁边走过去（§0.2(h)、§5.3.3）| **若将来新增第五个 task 绑定的 op kind 而忘了加那一行，这条保证会静默破掉**（§13.23）；若出现"任务行可以回到 `pending`"的机制，载体语义要重新定义；若切片 4 上线后误报率仍高，不可逆终结应重新评估 |
| **新**（r5，**r6 换了排除集**）| `spec_task_ceiling` 的谓词形状 | **对声明集合的确定性准入，不是裸 `count(*)`；排除集按"这一行是不是本次求值的产物"划——`pending` 行永远是输出、在飞行永远是输入**（`occupied` 只数 `dispatched/running/verifying`，再按块序 + key 升序在剩余容量内准入） | 裸 `count(*)` 把正在被重新求值的行也数进去 ⇒ 函数不幂等 ⇒ rebuild ≢ 增量 + 编辑之间抖动（§4.2 规则 3″ 的 ceiling=2 反例）。**r5 按"诊断是否为空"划排除集又两边都错**：带诊断的在声明 key 被数进去（不幂等仍在），仍被声明的在飞行被排除出去（不变量 7b 当场为假）——两个通道在 r6 从相反方向各命中一半 | 若将来 ceiling 改成跨 wave / 跨树的量，准入顺序需要重新定义（块序在跨文档时没有意义）|
| **新**（r5）| 人的否决能不能被换 `key` 绕过 | **不能：未清除的 `tombstoned_by:"user"` 墓碑使该 wave 对 spec 声明的任务按 `declare-and-wait` 处理**（派生的 `effective_policy`，零新列）| 与 `key` 无关、零相似度判断，且复用本设计已经在付钱的两样东西（`automation_policy` + `released_by_user`）；"人的不作为是吸收态"正是 §6.1 要求的终止性质。r4 的"没有可用机制"建立在一个假前提上（§13.22）| 若实测中"一处否决收紧全 wave"造成的摩擦大于它挡住的循环，可退化为"只对**新出现的** spec 声明生效"——但那需要一个文档里表达得出的"新"，今天没有 |
| **新**（r3）| 第 1 级检测挂在哪 | **`WaveReportEdited` 分支里、`event_warrants_spec_push` 之前，对所有 author 无条件运行；并新增 `WaveDeleted` / `CoveDeleted` 触发** | 该谓词只放行 `User \| Plugin`（`dispatcher/mod.rs:63,95-97`），spec 自己的编辑被整条丢弃，而那是最常见的变更源（第四个漏报路径）；wave/cove 删除一个报告编辑事件都不发（第五个，§0.2(f′)）| 若将来该谓词被改成"按事件语义"而非"按 author"，可重新评估两级是否合并 |

---

## 11. 验收

### 11.1 核心断言（可证伪）

> **从文档重放能重建出同一份 plan。**

**初稿的形式化有一个致命洞**（r1 两个通道独立指出）：它要求
「`origin='block'` 的行 == 当前 ready 声明集合」**同时**「全部状态列逐字节不变」。
这两条**不可能同时成立**——删掉一个 `running` 行的 `task` 块再 rebuild，
按前一条那一行必须消失，而状态列（status / worker_card_id / gate_* / 租约）
就长在**同一行**上，删行就是删状态。更要命的是那一行是 operations 的幂等键
（`tasks.id = "{wave_id}:{key}"`，`plan.rs:580`，在 `dispatcher/mod.rs:180`
被当幂等键消费）与 `task_report_success_from_worker_tx` /
`task_fail_from_worker_tx` 的写入目标（`db/sqlite/task.rs:405,586`）：
**rebuild 会把一个活着的 worker 和它的 gate 变成孤儿。**
而且它与 §6.5 的增量规则（in-flight 时"不删行、不改状态"）在同一份文档上分叉——
那不是真源关系的证明，那是 rebuild 会分叉的证明。

**修正后的断言：rebuild ≡ 同一份文档上的增量投影**（不是"重置成 ready 声明集合"）：

```
对任意报告文档 D 与由它投影出的当前 tasks 行集合 T：

  tasks_rebuild_tx(wave)  ≡  project_tasks_tx(wave, project_task_declarations(D.blocks))

具体地，rebuild 之后：
  (1) 删除阶段只删同时满足三个条件的行：
        origin = 'block'  且  当前文档不再把该 key 声明为「可调度」  且  status = 'pending'
      —— 即"从未派发过、可调度的声明已消失"的行。
      「可调度」是**当前文档 + 该 wave 的策略列 + 从同 wave 在飞 tasks 行派生出的已知 key 列表**
      的函数（r3 ⑫ 统一；r4 ⑭ 更正措辞与实现位置；r5 ⑯⑰、**r6 ⑳** 见下）：
        存在非墓碑 task 块 ∧ ready == true ∧ 该块的诊断为空
        ∧（effective_policy(wave) = declare-and-wait 且 declared_by = 'spec' 时）
           released_by_user == true
        ∧（declared_by = 'spec' 时）该 key 被 §4.2 规则 3″ 的 ceiling 准入接受
      其中 `effective_policy` 是 §6.6 的派生值（策略列 + 文档里有无
      `tombstoned_by:"user"` 的墓碑，r5 ⑰），ceiling 准入是**对声明集合的
      确定性准入**而不是裸 `count(*)`（r5 ⑯——否则这个"函数"不幂等，
      rebuild 与增量当场分叉，见规则 3″ 的 ceiling=2 反例）。
      **r6 ⑳ 把第三个输入从"同 wave 既有 tasks 行"收窄为"同 wave 的**在飞**
      tasks 行"**：`pending` 行是这个函数的**输出**，不能同时是它的输入
      （规则 3″）。这条收窄让"函数"这个词第一次严格成立。
      **r7 把这条收窄补写进 §4.2 自己的规范面**（规则 3‴）——r6 只改了 §11.1
      这一侧，而 §4.2 的契约签名与规则 3′ 的表仍写着裸 `&[Task]`，
      于是 `unknown_deps` 在本文有过两个不同的规范；现在只有一个。
      唯一实现是 `evaluate_schedulability_tx`（§4.2 规则 3′），
      增量投影、rebuild、读端 `taskDiagnostics` 三条路径调同一份代码。
  (2) 其余 origin='block' 的行：声明列 == 当前文档的声明；
      **非 pending 的行声明列不动**（§4.2 规则 2 的 stale 路径）。
  (3) 所有存活行的**全部状态列逐字节不变**。
  (4) origin='legacy' 的行逐字节不变。
  (5) declared_by 从块 payload 重建（§4.4），不依赖任何行内残留。
```

**"纯函数"这个词在 r3 里是错的**（r4 通道 A MAJOR，成立，改设计方向 ⑭）。
上面 (1) 的谓词**在它自己那句话里**就含 `automation_policy`——那是 `waves` 的
**列**，不是文档；同理 `unknown_deps` 要从在飞 tasks 行派生出的已知 key 列表、跨 cove 判定要读 wave 的
`cove_id`、`spec_task_ceiling` 要 `count(*)`。r3 写成"当前文档的纯函数"之后，
§4.2 规则 7 又把读端诊断定义成"按需调用那个纯函数"，两处叠加的后果是：
**规则 1/4 会删掉一条 pending 行，而读端渲染不出删它的原因**——一种静默降级。
修正即上文：谓词的载体是 `evaluate_schedulability_tx`，读端在一个读事务里调它。
`tasks_rebuild_tx` 与 `project_tasks_tx` 都在写事务里调同一个函数，
所以 (1) 仍然是可机械验证的等价（§11.2 不变量 11 的差分测试照旧覆盖它）。

**(1) 现在真的是增量路径会做的同一件事**（r2 两个通道各自指出 r1 不是，成立）。
r1 的分叉是这样的：增量路径上一条消失的 `pending` 声明会留下一个**活着的
`canceled` 行**（`task_cancel_tx` 写 `status='canceled'`，`task.rs:157-170`），
而 rebuild 把同一行**直接删掉**——r1 自己也承认了这个分叉（"rebuild 直接删，
因为它是重放不是审计"），于是"等价"只是被重述，并不为真；叠加 §6.1 的吸收问题后，
两条路径对"这个 key 还能不能被重新声明"也不一致。
**r2 的修订消除了分叉本身**：§2/§4.2 规则 1 已把增量路径改成同一个守卫式删除
（`task_delete_pending_tx`，`WHERE status='pending'`）。**两条路径现在跑的是
同一条 SQL、同一组前置条件**，(1) 因此是可机械验证的等价，不是文字承诺。

**"不再声明为可调度"包含四种情形**，四种在两条路径上都走同一条删除：
块被删除 / 块被墓碑覆盖 / `ready` 从 `true` 改回 `false` /
**该块产生了非空诊断（含 `declare-and-wait` 下放行位被撤回）**。
（第三种是 r2 补的；第四种是 r3 补的——r3 通道 A 指出"不 insert / 不 update"
对**已经存在的 pending 行**不成立，而 §11.1(1) 的谓词对"ready:true 但被诊断"
未定义：读成"仍算声明为 ready"则增量不删、rebuild 也不删，但那条行会带着已知
有问题的声明被 `compute_ready` 交出去；读成"不算"则两条路径必须都删。
本文取后者，并在 §4.2 规则 1/4 把它写成**同一个可调度谓词**，
于是等价性不依赖两处措辞恰好一致。）

**为什么非 pending 行不删**：它们是"当前正在发生的事"或"已经发生过的事"，
删掉它们不是重建投影，是丢状态与丢证据。文档不再声明它们不代表它们没跑——
§6.5 已经裁决过这件事（人的撤回意图被记录，任务跑完再生效）。rebuild 必须和增量
路径给出同一个答案，否则"重建"这个词就没有意义。

**代价（如实记）**：于是 `tasks` 表里可能存在"文档不再声明、但仍是
dispatched/running/done/failed"的行。它们是**执行史**，不是声明。
读端要能区分：诊断里标注"该任务的声明已被移除，此行是执行记录"。

**限定（评审第 6 条第 4 项，理由已修正）**：这条断言只对**当前 plan** 成立，
对**某时点的 plan** 不成立。理由**不是** `doc_heads`（那东西不存在，§0.1 #12），
而是：`ReportDoc::doc_heads()`（`wave_report_doc.rs:151-162`）只读**当前** heads，
内核里没有任何 "resolve this doc at heads X / at time T" 的调用（automerge blob
本身保有变更图，但没有暴露口）。报告的历史态因此只在 `wave_vcs` 提交链与
`WaveReportEdited` 的 `body_before/after` 里，而前者默认每 wave 只保留 50 条
（`wave_vcs/gc.rs:17`，6 小时一轮 `:15`；注意 keep 是**下限**——活跃 harness 的
diff 端点会被额外保护，`gc.rs:24-28,71-93`）。因此断言写作：

> 从**当前**文档重放，重建出**当前**声明；状态列与 legacy 行不受影响。

先例：`proposals_rebuild_tx`（随 #978 撤回 ④ 与 migration 0066 一并删除；
形状可用 `git log -S proposals_rebuild_tx` 取回）。

### 11.2 不变量骨架（E2E 用）

**事件 kind 清单**（本设计涉及的全部）：

| 事件 | 新/旧 | actor | 何时 |
|---|---|---|---|
| `CardUpdated` | 旧 | 写者 | 每次报告 persist（无条件） |
| `WaveReportEdited` | 旧 | 写者 | 每次报告 persist（无条件，与上者成对） |
| `PlanUpdated` | 旧 | 写者（Spec/User） | 声明有变时，`changed_keys` 非空。in-tx 闸放行 User（§0.1 #1 的更正），无需改闸 |
| `TaskDispatched` | 旧 | **Kernel only**（`crates/calm-truth/src/role_gate.rs:291-327`） | scheduler claim |
| `TaskGateResult` | 旧 | **Kernel only**（`crates/calm-truth/src/role_gate.rs:331-360`） | gate 判定 |
| `TaskContextFrozen` | **NEW** | **Kernel only**（新增同构条款） | claim 事务内，与 `TaskDispatched` 同批（§5.3） |
| `TaskContextAdvanced` | **NEW** | **Kernel only**（新增同构条款） | 引用闭包变更被裁决后**或 sweep 判定后**（§5.3.1） |

**r4 的两处修订都不新增事件 kind，也不改任何既有事件的 payload。**
sweep 复用 `TaskContextAdvanced`；`WaveDeleted` / `CoveDeleted`
（`crates/calm-types/src/event.rs:419-425`）**保持原样不加字段**——r3 曾要求
把受影响 `task_id` 集合随它们落地，r4 已整条删除（§5.3）。

**r5 同样不新增事件 kind，但把 `TaskContextAdvanced` 从切片 4 前移进切片 3a**
（两个通道交叉命中：切片 3a 自述的核心验收要求"记录 material 判定"，
而记录判定所需的事件在切片 4 才存在）。**因此 r4 那句"本轮没有任何 Tier-A
wire 连带面"对 r5 不成立**：切片 3a 从此带**两个** NEW 事件的全流程
（`TaskContextFrozen` + `TaskContextAdvanced`）。这不是新增代价，是把同一片
本来就欠的代价摆到台面上——3a 已经因为 `TaskContextFrozen` 而有 Tier-A 面
（该片的"行为保持"一节已经写明），多一个同族事件属于同一批产物。

**顺序不变量**（E2E 断言）：

1. `CardUpdated` → `WaveReportEdited` → `PlanUpdated`，同一事务，同一 wave scope。
   报告写与声明投影**不可能只落一半**。
2. `PlanUpdated{key}` **严格早于** 该 key 的第一个 `TaskDispatched`。
   （否则调度器读到了未提交的声明。）
3. 每个 `TaskDispatched` **同批**必有一条 `TaskContextFrozen`（同 wave、
   同 `idempotency_key`）。冻结集缺失 = fail-closed 的前提缺失，所以这条是硬的。
   **"空"不等于"缺失"**（r2 通道 A MINOR，成立）：`origin='legacy'` 的行没有块、
   没有闭包，切片 3a 上线到切片 3b 之间**所有**被派发的行都是这种情形。
   它们**仍然发射** `TaskContextFrozen`，只是 `refs: []`。
   一个空冻结集的语义是"这个任务没有可失效的上下文"，与"我们没记下它的上下文"
   （= fail-closed 判 `material`，§5.3）是两件事，事件必须能区分。
   否则这条"硬"不变量在切片 3a 上线第一天就被 legacy 行破掉。
3b. **task 块自身在冻结集内**（§5.1）：任何 `origin='block'` 行的
   `TaskContextFrozen.refs` 必包含它自己的块（`wave_id == task.wave_id`）。
   编辑一个 in-flight task 自己的 `goal` → 必有 `TaskContextAdvanced`。
4. **（r4 重述，⑬）** 改动落在某个 in-flight task 的冻结集内
   （**按 `content_hash` 判定**，§5.1）→ **在此后第一次完成的 sweep（§5.3.1）
   结束时**必有一条 `TaskContextAdvanced`（无论判 material 还是 immaterial），
   **且它属于 task 所在的 wave**（不是被编辑 wave，§5.3 的路由）。
   *（**r3 的写法是"`WaveReportEdited` → 必有 `TaskContextAdvanced`"，那条在一次
   `RecvError::Lagged` 或一次重启之后当场为假**——总线自承 lossy
   （`scheduler/mod.rs:24-27`）、envelope 是 fire-and-forget
   （`dispatcher/mod.rs:777-782`）、唯一的跨重启补投对跨 wave 引用失明
   （`harness/mod.rs:203-228`），见 §0.2(g)。改成"sweep 完成之后"，
   这条才第一次有机制支撑。E2E 的断言形态随之变化：**不等待事件到达，
   而是显式跑一次 sweep 再断言**——事件路径只是让它更快，不是让它成立。）*
   *（初稿还写了"且它早于该 task 的 `TaskGateResult`"——**删除**：没有任何机制
   保证它，见 §5.3 末段"裁决与 gate 的时序"。）*
4b. **sweep 是 fail-closed 的**（NEW，r4 ⑬）：一轮 sweep 结束后，
   **不存在**这样的 in-flight task——它的某个冻结元组既未被验证为"与冻结值相同"，
   又没有产生 `TaskContextAdvanced` / 未被判 `material`。
   三条必测的构造：(a) **丢事件**：直接从 DB 改掉被引用块（绕过总线）→ 跑 sweep
   → 必判 `material`；(b) **重启**：commit 之后立刻杀进程、重启 → boot sweep
   必判 `material`；(c) **冻结集缺失**：抹掉 `claim_context_json` → 必判 `material`。
   **(r5 补一条前置断言)** 判定必须是**持久**的：跑完 sweep 之后
   `tasks.context_stale_at_ms` 非空，**且它在重启之后仍然非空**——
   否则第 5 条无从执行（§5.3.3）。
4c. **once-per-condition**（NEW，r5 通道 A MINOR）：一个已判 `material` 且冻结点
   未推进的 in-flight task，**在其后的任意多轮 sweep 中不再产生新的
   `TaskContextAdvanced`**（也不再被送第 2 级裁决）。构造：判 material 之后
   连跑 3 轮 sweep → 该 task 的 `TaskContextAdvanced` 计数恒为 1。
   （没有这条，sweep 是电平触发在一个按构造持续存在的条件上，会在该任务的
   剩余生命周期里每轮重发一条**不可裁剪**的事件并重复调用 LLM，§5.3.1。）
5. **（r6 重述，⑲）** 一条 `TaskContextAdvanced{verdict: material}` **提交之后**，
   该 task 上**任何进入 `prepare_tx` 的 operation 一律被拒**——
   `codex-worker` / `claude-worker` / `terminal-worker`（`build_worker_payload`
   `scheduler/mod.rs:197-253` 的封闭集合）与 `task-verify` 四个 kind；
   因而**不产生任何新的 worker、也不产生任何新的 gate 执行**。
   已越过 `prepare_tx` 的 operation 照常跑完（§6.5）。
   **三条可证伪构造（这条不变量唯一有意义的测法是崩溃重启）**：
   (a) **worker 未开始**：dispatched 行 + material 判定 → `kill -9` → 重启 →
   boot 之后**不得**出现该 task 的新 worker 卡；worker op 终结在 `failed` 且
   `last_error` 含 `context-stale`；`tasks` 行落 `failed`（`spawn-failed` +
   `TaskFailed.reason` 含 `context-stale`），**不得**长期停在 `dispatched`
   占住 `task_budget`；
   (b) **gate 未开始（r6 新增，正是 r5 漏掉的那条路径）**：`verifying` 行 +
   `gate_attempt = 0` + material 判定 → `kill -9` → 重启 → **不得有任何 gate
   shell 命令被执行**；行落 `failed`，`gate_result.log_tail` 含 `context-stale`；
   (c) **已开始的不受影响（验证没有过度收紧）**：op 已在 `SpawnStarted` +
   material 判定 → 重启 → 该 op **照常**被恢复驱动到终结、worker 照常汇报。
   *（沿革：r4 的写法「不得再产生新的 `TaskDispatched`」**恒真且无用**
   （§0.2(h)）；r5 换成「`resume_dispatched` 不再起新工作」——**方向对、
   强制点错**：operation 的开机恢复（`operation/driver.rs:1010-1024`、
   `:1043-1055`）与 `drive_gate_inner` 的 submit 分支（`scheduler/mod.rs:1541-1581`）
   都从它旁边走过去，而且"有 operation 行 ⇒ 工作已开始"这个谓词恰好在
   `submit` 先插行后 drive 的那个崩溃窗口上误判（`operation/driver.rs:105-123`）。
   r6 把强制点换到所有起活路径的必经漏斗 `prepare_tx` 上，**b1/b2 因此不再是
   一个要写的谓词，而是 phase 阶梯本身**，§5.3.3。）*
5b. **boot 顺序（r6 重述，⑲；r7 补上它的 CI 机制）**：**上下文 sweep 的 boot
   轮，在 operation 恢复漏斗与 `sweep_boot` 之前完成。**
   *（r5 只断言了 (ii)，而 (i) 正是 r6 发现 1 的那条路径。**这条现在是承重的
   而不是修辞**：不变量 5 的规则要求判决已落库才读得到，唯一读不到的情形是
   "使闭包过期的编辑发生在停机期间"，那时唯一的保护就是"sweep 先跑"。
   把它写成对顺序的断言，是为了让将来重排 boot funnel 的人**在 CI 里**看到，
   而不是静默失去这一层，§5.3.1/§5.3.3。）*

   **它怎么在 CI 里被断言（NEW，r7 通道 A MINOR，成立；不留给切片期即兴发挥）。**
   r6 只写了断言的**内容**，没写它的**载体**——而 5b 断言的那个漏斗
   **不是运行期可调用的东西**：boot funnel 是 `main()` 的直线代码
   （`crates/calm-server/src/main.rs:64/73/79` 依次是
   `reconcile_supervisor_on_boot` / `recover_operations_on_boot` /
   `scheduler_sweep_on_boot`，三者的 pub 定义在
   `crates/calm-server/src/lib.rs:203`、`:221`），**没有任何测试能"跑一遍 boot"**
   ——一个自己依次调用这些 pub 函数的测试，**顺序是它自己选的**，因此什么都
   没断言。项目里**已经有**为这件事准备好的机制，直接采用，不要发明新的：

   > **(a) 源码序测试（顺序这一半）**：`crates/calm-server/src/lib.rs:611-705`
   > 的 `mod boot_order_tests` —— 它 `include_str!("main.rs")`，用
   > `str::find` 取各 boot 调用的字节偏移并断言先后
   > （范例：`boot_order_scheduler_sweep_after_operation_recovery`，`:678-688`，
   > 逐字断言 `recover < sweep`；模块头部的注释就写着 #644 PR-B 的 boot 顺序
   > 设计）。**本设计的动作是往这条既有链上加一格**：新增
   > `boot_order_context_sweep_before_operation_recovery`，断言
   > `context_sweep_on_boot(&state).await` 的偏移 **<**
   > `recover_operations_on_boot(&state).await?` 的偏移（因而也 <
   > `scheduler_sweep_on_boot`）。谁重排 boot funnel，**CI 当场红**——
   > 这正是 5b 要的那个效果，而且零新机制。
   > **(b) seam 测试（行为这一半）**：用测试 seam 卡住上下文 sweep → 断言
   > (i) **没有任何 operation 被恢复驱动**、(ii) 没有任何 dispatched 行被
   > `resume_dispatched` 重驱动；放开 → 两者照常发生、行不丢。
   > seam 形状照抄既有的 `boot_sweep_done`（`scheduler/mod.rs:992/1015/1026/1031`
   > 有现成的读口与测试 seam），(ii) 即 §5.3.3(2) 的检查 a。

   **两半缺一不可**：(a) 挡"有人把调用挪了位置"（源码级，静态，无法用运行时
   测试表达）；(b) 挡"顺序没变但门没生效"（行为级）。**5b 是"停机期间的编辑"
   这个残余窗口的唯一机制保证**，所以它的可断言性不能是切片期的开放问题——
   落在 §12 切片 3a 的完成定义里。
6. **`tombstoned_by: "user"` 的**墓碑立起后，**在它生效之后**（**不含物化它的
   那一次编辑**）同一 `key` 的任何 `PlanUpdated` 都**不得**包含该 key ——
   死循环的机械防线（§6.1）。
   *（两处限定都是必需的：(i) 判据在 r3 从 `declared_by` 换成 `tombstoned_by`
   —— 人否决 spec 声明的任务后墓碑的 `declared_by` 仍是 `"spec"`，用旧判据
   这条不变量会漏掉**最主要**的那种墓碑（§3.7 的 ⑨ 决策表）；`tombstoned_by:
   "spec"` 的墓碑按 §6.1 可被人删除撤回，撤回后该 key 合法重现，本来就不是防线
   （r1 通道 A MAJOR）。(ii) "不含物化那一次编辑"是 r3 两个通道共同要求的：
   §4.3 已裁定删除进 `changed_keys`，而物化墓碑必然删掉那条 pending 行 ⇒
   **那一次**编辑必然发出一条含该 key 的 `PlanUpdated`。不排除它，这条 E2E
   在第一天就红。）*
6b. **换 key 也绕不过去**（NEW，r5 ⑰，§6.1/§6.6）：一条未清除的
   `tombstoned_by:"user"` 墓碑存在期间，该 wave 内**任何** `declared_by:"spec"`
   的新声明——**包括全新的 `key`**——在 `released_by_user` 未置时**不落
   pending 行**（诊断可读，见下）。
   端到端构造：AI 声明 `k1` → 人删掉它（墓碑）→ AI 改用 `k2` 重提同一件事 →
   `tasks` 中 `(wave,k2)` **无行**、`GET .../report` 的 `taskDiagnostics` 里
   能读到"本 wave 有未清除的人工否决"→ 人写 `released_by_user:true` → 落行；
   **另一条分支**：人删掉墓碑 → 无需放行位即落行（回到 `auto-declare`）；
   **第三条分支**：人 PATCH `automation_policy='auto-declare'` → 墓碑仍在
   （记录保留）但自动化恢复。三条都必须测——它们是 §6.1 那条"人的不作为是
   吸收态"的全部出口。
7. **稳态上界**（r2 通道 A MINOR 改述，成立）：**一次报告写产生的
   `TaskDispatched` 数量恒为 0** —— 派发是异步的，报告写只发 `PlanUpdated`，
   dispatcher 收到后 `self.scheduler.poke(wave_id)`
   （`dispatcher/mod.rs:968`，逐字复核），claim 发生在之后的调度周期里。
   r1 那句"一次报告写产生的 `TaskDispatched` 数量 ≤ min(...)"**按字面不可断言**。
   可断言的形式是稳态不变量：
   > 任一时刻，一个 wave 内 `status ∈ {dispatched, running, verifying}` 的行数
   > ≤ 该 wave 的 `task_budget`；且一棵 wave 树内 `declared_by='spec'` 的
   > **非终结**行数 ≤ `tree_task_budget`（§8）。
7b. **单 wave 未结存量上界**（NEW；**r6 重述为它真正能保证的那条，⑳**）：
   > **只要该 wave 的 `spec_task_ceiling` 从未被调低**，任一时刻该 wave 内
   > `declared_by='spec'` 且 `origin='block'` 的**非终结**行数
   > ≤ `waves.spec_task_ceiling`。
   > 若人把 ceiling 调低到当时的在飞行数以下，上界暂时退化为**调低那一刻的
   > 在飞行数**，并随这些行终结单调收敛回新 ceiling——期间 `capacity = 0`，
   > **不会准入任何新行**。
   证明见 §4.2 规则 3″（`非终结行数 = occupied + |准入| ≤ max(ceiling, occupied)`，
   而 `pending → dispatched` 的 claim 不改变总数）。
   *（r5 写的是无条件全称句，**在它自己的准入规则下为假**：仍被声明的在飞行
   落在排除集 `D` 里、不计入 `occupied`，于是把一个新块插到在飞任务块**之上**
   就能多拿一份容量——通道 A 在 r6 给出了 `ceiling = 1` 的两行构造。
   超额被该 wave 的 `task_budget` 封顶，所以这是上界缺陷而不是失控；
   但 7b 是切片 3b 的验收测试，照 r5 的写法会红。）*
   这条是**存量**上界（**不是生命周期总量**，§8(A) 已改述），与 7 的**并发**
   上界正交——`compute_ready(tasks, budget)`（`scheduler/mod.rs:164-191`）先减去
   `running_cost` 再 `take(capacity)`，它约束的是同时在跑几个，不是一共能排几个。
8. **§3.7 的每条规则各有一条否定测试**：
   spec 伪造 `declared_by: "user"` → 400/-32602；**`Kernel`/`Plugin` 新建
   `task` 块 → 400**（fail-closed，规则 1）；spec 用 `write_markdown`
   删除人的 `task` 块 → 400（规则 3）；人经块级 DELETE 删 `task` 块 →
   同一次编辑里出现墓碑块（规则 4）；人经整文档 `Replace` 删 `task` 块 →
   **400 且文案指向块级 DELETE 端点**（stomp guard 先拒，规则 4 在此不可达）；
   改写既有块的 `declared_by` → 400（规则 2）。
   **r3 新增的五条**：
   (a) **人删一个 `declared_by:"spec"` 的 task 块 → 200**，且产出的墓碑是
   `{key, tombstone, declared_by:"spec", tombstoned_by:"user"}`——
   这条是 r3 两个通道交叉命中的 BLOCKER 的回归测试（r2 的形状在这里必然 400），
   而它恰是 issue 点名的默认场景；
   (b) 该墓碑 payload 过 `validate_payload`（§3.2 规则 7 的封闭形态），
   且**带 `kind` 字段的墓碑 → 400**；
   (c) spec 删除 / 改写 `tombstoned_by:"user"` 的墓碑 → 400（规则 3 的第二个析取项）；
   (d) spec 把墓碑块原位改回非墓碑 task 块 → 400（规则 2b）；
   (e) spec 写入或改变 `released_by_user` → 400（规则 5），人写 → 200。
   **每条都要同时覆盖 `Replace` / `WriteMarkdown` / `UpsertBlock` / `DeleteBlock`
   四条路径**——§0.2(a′) 证明单守块级工具是无效的。
   **r6 新增一条，它守的是另一条写路径（§6.6 末段）**：
   (f) **非 `ActorId::User` 的 `PATCH /api/waves/{id}` 携带
   `automation_policy` 或 `spec_task_ceiling` → 403，且两列的值不变、
   不发任何事件**；人 PATCH → 200。
   *（没有这一条，§3.7 规则 5 逐块守住 `released_by_user` 就没有意义——
   `PATCH automation_policy='auto-declare'` 一次调用即可把整个 wave 的否决
   清掉，而 `X-Calm-Actor` 是自述的，`crates/calm-server/src/actor.rs:28-33`
   逐字："Not authenticated … not a security boundary"。）*
9. **`key` 复活的端到端断言**（§4.2 规则 2b、§6.1）：
   AI 声明 `k` → 人经块级 DELETE 删它 → `tasks` 中 `(wave,k)` **无行**、文档有墓碑 →
   AI 重声明 `k` **被拒**（诊断）→ 人删墓碑 → AI 重声明 `k` → **落一条新的
   `pending` 行**。这条走完才算"死循环防线是终止的而不是吸收的"。
10. **`refs[]` 的 cove 边界**（§5.1）：指向本 cove 与 system cove 之外 wave 的
   引用 → 该块不可调度 + 诊断；且该引用不出现在任何 `TaskContextFrozen.refs` 里。
11. **rebuild ≡ 增量的差分测试**（§11.1）：对随机生成的编辑序列，
   "逐步增量投影"与"末态一次 rebuild"必须产出**逐字节相同**的 `tasks` 表
   （含行的存在与否）。这条是 §11.1 那个等号的机制保证——r1 的
   `canceled`/删除分叉正是靠它才会被立刻抓到。
   **生成器必须能生成"制造诊断的编辑"**（r3 ⑫）：粘贴出重复 key、
   引入依赖环、把 `refs` 改成跨 cove、撤回 `released_by_user`——
   它们与"删块 / 立墓碑 / 撤 ready"在两条路径上必须给出同一个终态。
12. **索引不含幽灵行**（NEW，r3 通道 B MAJOR；**r5 保留全称形式并给了它机制**）：
   不存在 `task_ref_index` 行，其 `task_id` 指向一个已终结
   （`done/failed/canceled`）或**不存在**的 `tasks` 行。
   断言点有三：每条终结跃迁之后（§5.3 的生产者清单）、`wave/cove` 删除与
   fixture 重置之后、**以及一轮 sweep 完成之后**。
   *（r5 通道 A MINOR：r4 把这条写成**全称**，而 §13.20 同时**刻意容忍**
   漏清理（"漏一个点 = 代价 bug"、"表会长胖，且没有兜底的周期性清扫"）——
   两处不能同时为真。二选一里取**加机制、保全称**：采纳 §13.20 自己提的那条
   建议，sweep 末尾一条 `DELETE FROM task_ref_index WHERE task_id NOT IN
   (SELECT id FROM tasks WHERE status IN ('dispatched','running','verifying'))`
   （§5.3.1）。一条 SQL、无扇出，于是全称形式**有了兜底**，
   §13.20 的"没有周期性清扫"随之作废。正确性仍然由读端 JOIN 承担，
   这一条只管代价。）*
13. **删除必须能被检出**（NEW，r3 通道 A MAJOR，⑩ 的第二半；**r4 改了它的
   落地路径**：不再依赖删除事务内算出的受影响集合，而是 sweep 兜底 +
   `WaveDeleted`/`CoveDeleted` 触发一次 sweep 降低延迟，§5.3）：
   (a) 被引用块所在的**整个 wave 被删除** → 引用它的 in-flight 任务**必须**
   被判 `material`，
   (b) cove 删除同理（`CoveDeleted`），
   **(a)(b) 各要两个变体：事件正常投递、以及事件被丢弃（只跑 sweep）——
   两个变体必须给出同一个结论**，
   (c) **`EditAuthor::Spec` 编辑被引用块 → 必须被第 1 级检出**——
   这条直接覆盖 `event_warrants_spec_push` 那个漏报路径，
   是 r3 找到的第四个洞的回归测试。

**关键路径的功能验收**（逐切片，见 §12）。

---

## 12. 切片计划

**r1 的两个通道都判定初稿的切片 1 不是独立安全的**，理由相同且成立：初稿的切片 1
给了 spec 第二条声明写路径（`kind: "task", ready: true` → `status='pending'` 行
→ 被 `compute_ready`（`scheduler/mod.rs:164`）看到），而**为它设的护栏全在后面**
——stale 检测在切片 3、树预算与策略列在切片 5。那就是一个半截机制合进了 main。

**重切的原则：每一片合进 main 之后，系统必须处于一个自洽的状态。**
具体做法是**把"声明能被写下"与"声明能驱动调度"分成两段**：切片 1–3a 落地的
`task` 块在**调度语义上是惰性**的——它就是一个渲染得好看的声明块，`tasks` 表
一行不写，调度器一无所知。（r3 的限定：切片 3a 在**事件面**不是惰性的——
每次 claim 多一条空 `TaskContextFrozen`，所以那一片的正确说法是
"行为保持"而不是"惰性"，见该片。）投影（=让声明有后果）在切片 3b 一次性带齐它的护栏。

**r2 又在这条线上切了一刀**：r1 的切片 3（~1500 行）里，冻结/反向索引/第 1 级
检测那一半在投影上线之前是**完全惰性**的（没有 `origin='block'` 的行 ⇒ 闭包恒空
⇒ 索引恒空 ⇒ 检测恒不命中），所以它可以、也应该**先**落地——这正是本节用来
论证切片 2 的"护栏先于后果"。同时把 `waves.spec_task_ceiling`（单 wave 内
spec 声明的未结存量上限）与 `waves.automation_policy` 从切片 6 **前移**到切片 3b：
r1 为"切片 3 先于切片 6"所作的辩护只覆盖了**递归**，没覆盖**单 wave 未结存量**，
而 `task_budget` 是并发容量不是存量上限（§8）。

**r5 又调了两处切片边界**（两处都是"半截机制"的同一条纪律）：

1. **`Event::TaskContextAdvanced` + role_gate 条款 + Tier-A 全流程 +
   `tasks.context_stale_at_ms` 从切片 4 前移进切片 3a**（两个通道交叉命中）。
   r4 的 3a 自述"本片的核心验收 = 不变量 4b 的三条 sweep 构造（必判 material）"，
   但**记录那个判定所需要的事件在切片 4 才存在** ⇒ 3a 交付的是一个
   **判决无处可记、因而无处可执行**的 sweep：不变量 4b/13 与"记录 + 不再派发"
   全都落空。两个出口二选一——(i) 把事件移进 3a，(ii) 把不变量 4b/13 的验收
   推到切片 4 并如实写明"3a 交付一个没有判定记录的 sweep"。**取 (i)**：
   sweep 是 3a 的**正确性载体**（r4 ⑬ 的整个理由），交付一个不能执行判决的
   正确性载体，与 §12 的存在理由直接冲突。代价是 3a 的 Tier-A 面从一个事件
   变成两个（同族、同一批产物），并且 3a 的"没有 Tier-A 连带面"这句话作废
   ——它本来也不成立（`TaskContextFrozen` 就是 Tier-A）。
2. **切片 3b 的前端拆出为切片 3c**（r5 通道 A MINOR，成立）。3b 的"护栏必须与
   后果同片"论证覆盖了除**前端**以外的一切：**状态回显 + 诊断渲染不是护栏，
   API 层的 `taskDiagnostics` 字段才是**——后者留在 3b（没有它，删行的原因
   在任何客户端上都取不到，那是 r4 ⑭ 判过的静默降级）。拆掉前端之后 3b 仍然
   自洽（诊断仍可读、行仍受守卫），且回到约定规模。

### 切片 1 — `task` 块 kind + 校验面 + 谓词下沉（~900 行，**惰性**）

**今天即可开工**（不等 #979）。**合入后系统行为零变化**：多了一个可写的块 kind。

- `task` 加入 `DATA_KINDS`（`kinds.rs:45`，`[&str; 3]` → `[&str; 4]`）
  + `validate_payload` 的严格分支（§3.2 八条规则）。
- **`calm.report.blocks.kinds` 的 JSON Schema 面**（`wave_report_blocks.rs:49,82`）
  ——它是新 kind 面向 agent 的契约，r1 通道 A MINOR 指出初稿漏了它。
- `key_is_valid`（`plan.rs:165`）/ `validate_gate_shape`（`plan.rs:352`）
  下沉到 calm-types；`report_links::parse_destination` / `is_block_id`
  提升为 `pub`（§0.1 #10）。
- **四条规则谓词下沉 + `resolve_plan_batch` 重构为调用它们**（§4.2 规则 3），
  含 `find_cycle` 从 `plan.rs:524` 移动。**行为等价，无功能变化。**
- `project_task_declarations`（**块局部**纯函数，§4.2 规则 3′）——**只实现，不接线**。
  需要 DB 的那一层（`evaluate_schedulability_tx`）不在本片：它要读的列
  （`waves.automation_policy` / `spec_task_ceiling`）与 `tasks` 行都在切片 3b 才存在。
- **本片的 `validate_payload` 只接受 `declared_by: "spec"`，`"user"` 一律拒
  （NEW，r6 通道 A MINOR，成立）。** 见下面的窗口分析；切片 2 在落地
  `guard_task_declarations` 规则 1 的**同一个 PR** 里把它放宽为
  `{spec, user}`。**这是一处刻意的两步走，不是遗漏。**

**必须写死的一件事：切片 1 与切片 2 之间存在一个 `declared_by` 伪造窗口**
（NEW，r6 通道 A MINOR，成立；**这条直接影响"切片 1 今天开工"这个决定**）。
事实链逐条对 HEAD 成立：

1. 本片把 `task` 加进 `DATA_KINDS` + `validate_payload` ⇒ **该 kind 从此可写**
   （spec 走 `write_markdown`、人走整文档写，两条今天都在）；
2. `declared_by` 是**必填** payload 字段（§3.2），而它的**正确性明确不由
   `validate_payload` 保证**（§3.2 规则 8 逐字："纯函数看不到写者是谁"）——
   强制它的是切片 2 的 `guard_task_declarations` 规则 1；
3. 切片 2 的规则 2 随后把该值**永久冻住**（不可改，含转墓碑时）；
4. 而 §6.2/§8 正是**信任它**来做预算豁免（`declared_by='user'` 的行不计入
   `spec_task_ceiling` 与树预算）。

⇒ 窗口期写下的一个 `declared_by:"user"` 会被切片 2 焊死成一条**永久豁免预算**
的声明。**裁决：切片 1 直接拒 `"user"`**（而不是"切片 2 附带一次性对账"）：
对账要区分"谁真的是人写的"，而那正是窗口期**没有载体**去回答的问题；
拒绝则把窗口里的每一条都固定在**保守的那一侧**（`spec` 是受约束的一侧）。

**代价如实记**：窗口期（切片 1 → 切片 2，按计划是相邻两个 PR）里由人写下的
`task` 块会被永久归因为 `spec`。这**不是安全问题**（保守方向），但会让那些块
计入 `spec_task_ceiling`。**实践建议直接写进本片的 PR 描述**：窗口期没有任何
投影，写 `task` 块**得不到任何东西**——`tasks` 表里根本不会出现行——所以
正确做法是等切片 2。

**除此之外切片 1 今天开工是安全的**（r6 通道 A 的独立复核，本文采纳）：
没有任何投影，而调度状态的唯一消费者 `compute_ready`
（`scheduler/mod.rs:164-191`）读的是根本不存在的 `tasks` 行。

**验收**：payload 校验的逐字段单测；`resolve_plan_batch` 重构前后行为等价的
回归测试；§4.2 规则 3 的**等价性属性测试**（`resolve_plan_batch` 报错
⟺ 纯函数诊断非空，规则 2 类除外）；
**`declared_by:"user"` → 校验失败**（本片的门），且切片 2 必须带一条
"放宽为 `{spec,user}` + 规则 1 同时生效"的成对测试。

#### 切片 1 的可交付清单（r7 补：本片可**逐字**交给实现者，无需再读全文）

**目标一句话**：让 `task` 成为报告文档里一个**可写、被严格校验、但对调度器
完全不可见**的块 kind，并把四条批级规则谓词从 `plan.rs` 下沉到 calm-types。
**合入后系统行为零变化。**

| # | 动作 | 文件 / 符号（HEAD `02ef95d5`）|
|---|---|---|
| 1 | `DATA_KINDS` 从 `[&str; 3]` 扩到 `[&str; 4]`，加 `"task"` | `crates/calm-types/src/report_blocks/kinds.rs:45` |
| 2 | `validate_payload` 加 `task` 分支，落地 **§3.2 的全部八条规则**（含规则 7 的封闭墓碑形状、规则 8 的 `declared_by`/`tombstoned_by` 枚举）；逐字段路径报错，与既有 kind 一致 | `kinds.rs:55-91`（未知 kind 报错臂在 `:67-70`）|
| 3 | **`declared_by` 本片只接受 `"spec"`**（临时收紧，见上面的窗口分析）| 同上 |
| 4 | `key_is_valid` / `validate_gate_shape` **移动**到 calm-types（两端共用） | 从 `plan.rs:165` / `plan.rs:352` |
| 5 | `report_links::parse_destination` / `is_block_id` 提升为 `pub`（两行） | `report_links.rs:138` / `:152`（§0.1 #10）|
| 6 | 四条纯谓词新建 + `resolve_plan_batch` 改为调用它们（`find_cycle` 从 `plan.rs:524` **移动**）；**行为等价** | §4.2 规则 3；`plan.rs:412-484` |
| 7 | `project_task_declarations`（块局部纯函数）**只实现、不接线** | §4.2 规则 3′ |
| 8 | **`calm.report.blocks.kinds` 的 schema 面**——见下 | `mcp_server/tools/wave_report_blocks.rs` |

**第 8 项展开（它是新 kind 面向 agent 的契约，不是搭车项）**：
`calm.report.blocks.kinds` 的返回体是一张**手写**的 JSON 表
`kinds_table()`（`wave_report_blocks.rs:103` 起，每个 kind 一个
`{ kind, schema, usage }`），本片要给 `task` 加一项，其中 `schema` 就是
§3.2 那个 payload 的 JSON Schema（`additionalProperties: false`，
与规则 1 的 `deny_unknown_fields` 对齐），`usage` 说明"任务声明块，
`ready:true` 才会被投影（投影在切片 3b 上线）"。
**同一批要改的还有三处 kind 枚举/描述**，漏一处 agent 就写不进来：
`kinds_descriptor()` 的描述里那句 `Kinds: prose / chart.candles / table / app`
（`:86-88`）、`upsert_descriptor()` 的 `input_schema` 里
`"kind": { "enum": ["prose","chart.candles","table","app"] }`（`:266`）、
以及同一段描述里的 kind 列举（`:252-256`）。

**本片明确的非目标（写进 PR 描述，免得实现者顺手做了）**：
不碰 `tasks` 表、不写任何投影、不加 migration、不加事件、不接 `guard_task_declarations`
（切片 2）、不做 `evaluate_schedulability_tx`（它要读的 `waves.automation_policy` /
`spec_task_ceiling` 列与 `tasks` 行都要到切片 3b 才存在）、不做前端渲染（切片 2）。

**前端为什么本片不用动**（复核结论，免得实现者去猜）：`web` 侧的
`reportBlockSchema` 是 `z.union([typedReportBlockSchema, opaqueReportBlockSchema])`
（`web/src/cards/builtins/wave-report.tsx:153-156`），**未知 kind 走 opaque 分支**、
渲染器给出"unsupported block kind"占位而不是解析失败。所以一个 `task` 块在本片
之后是**能被安全显示**的（显示为占位），切片 2 再给它真正的渲染。

### 切片 2 — 写口收口 + 人的块级写口 + 链接扫描（~1000 行，**仍然惰性**）

依赖：切片 1、#979（`doc_rev`）。

- **`normalize_report_op` + `guard_task_declarations`（§3.7）+
  `apply_report_op` 加 `author` 参数**——改写器与校验器的**七条**规则全部落地
  （1 / 2 / 2b / 3 / 4 / 4′ / 5），包括人经块级 DELETE 删 `task` 块 →
  原位墓碑（承接 `declared_by` + 写 `tombstoned_by:"user"`，§3.7 的 ⑨）、
  `released_by_user` 的人可写/spec 不可写（规则 5，此时它还没有语义，
  语义在切片 3b 的投影里通电）、规则 4′ 在整文档路径上的 fail-closed 拒绝 +
  stomp guard 错误文案的指路增补。
- **同一个 PR 里把 `validate_payload` 的 `declared_by` 放宽为 `{spec, user}`**
  （NEW，r6）——它在切片 1 被临时收紧为只接受 `"spec"`，因为在规则 1 落地
  之前该字段是自述的、而规则 2 会把它永久冻住（§12 切片 1 的窗口分析）。
  **放宽与强制必须同一个 PR**：先放宽即重开窗口，先强制不放宽即人写不了任务。
- 块级 REST（§3.4）四个端点，`if_doc_rev`（create/positional/move）
  + `if_block_rev`（update/delete）+ OpenAPI + zod + web 生成物；
- **MCP 侧 `calm.report.blocks.*` 的 create/move 补 `if_doc_rev` = 一次工具契约
  迁移，不是搭车项**（r2 通道 A MINOR，成立）。今天 `upsert` 的 `if_rev` 只在带
  `id` 时必填（`wave_report_blocks.rs:348-357`）、`move` 的 `if_rev` 是可选的
  （`:428`）；把一个**新的必填字段**加到活着的 Spec 工具上，会让**正在运行的**
  spec agent（它上下文里是旧 schema）在下一次调用时吃 `-32602`。所以：
  - 与新块 kind 一起走 `calm.report.blocks.kinds` / 工具 descriptor 的
    schema 更新，并在工具描述里写明"缺 `if_doc_rev` 的调用将被拒绝，
    从 `calm.report.read` 读取 `docRev`"；
  - **过渡窗口**：`if_doc_rev` 缺席时先返回一条**指路的** `-32602`
    （而不是静默放行），文案给出如何取 `docRev`——这让 agent 能自愈重试；
  - 验收里加一条：旧形状调用 → 明确错误 + 可自愈的重试路径。
- `guard_non_prose_stomp` 保持不变（不放松）。
- `report_backlinks` 扫描放宽到"prose + 声明为可扫描的 kind"，按字段扫描（§5.2）。
- 前端：`task` 块的渲染（声明字段；此时**还没有**状态与诊断可显示）。

**为什么护栏先于投影落地**：§3.7 是归因与不对称的唯一收口，它必须**早于**
`declared_by` 产生任何后果的那一刻就位。反过来做，就等于让一批无法信任归因的
行先落地。

**验收**：§11.2 不变量 8 的**全部否定测试（r2 的六条 + r3 新增的五条 (a)–(e)）
× 四条写路径**；其中 **(a)「人删一个 `declared_by:"spec"` 的 task 块 → 200 且
产出规范形态的墓碑」是 r3 BLOCKER 的回归测试，必须专列**；
人经 REST 建/改/删 `task` 块；`if_doc_rev` / `if_block_rev` 冲突 → 409；
`task` 块里的 `neige://…#b_xxxx` 产生反链。

### 切片 3a — 冻结 + 反向索引 + 第 1 级机械检测 + fail-closed sweep + 判决的载体与强制点（~1000 行，**调度语义惰性、事件面非惰性**）

依赖：切片 1、2。

**r1 写的是"切片 3 不可再拆"，r2 通道 A 判定这条未被证成，复核后成立。**
冻结/索引/检测这一半在**任何 `task` 块投影出行之前都是惰性的**：没有
`origin='block'` 的行 ⇒ 每次 claim 的闭包解析结果都是空集 ⇒ `task_ref_index`
恒空 ⇒ 检测恒不命中。它满足的正是 §12 用来论证切片 2 的同一个模式——
**护栏先于后果**。而且它把 r1 那个 ~1500 行的巨片（§13.12 自承超出惯例）
拆成了两个各自可评审的形状。

- `Event::TaskContextFrozen`（NEW，Tier-A 全流程）+ role_gate **严格 Kernel** 条款；
  在 claim 事务内与 `TaskDispatched` 同批发射。**legacy 行发射空冻结集**
  （`refs: []`，§11.2 不变量 3 的"空 ≠ 缺失"）——这让不变量 3 从第一天起就被
  真实流量执行，而不是等切片 3b 才第一次通电。
- **`Event::TaskContextAdvanced`（NEW，Tier-A 全流程）+ role_gate 严格 Kernel
  条款**（与 `TaskGateResult` 同构，`crates/calm-truth/src/role_gate.rs:331-360`）
  ——**r5 从切片 4 前移进本片**：本片的核心验收（不变量 4b/13）要求"判定被记录"，
  没有这个事件就交付不了。本片里它的 `verdict` 恒为 `"material"`
  （第 2 级裁决在切片 4）。
- migration：`tasks.claim_context_json` + **`tasks.context_stale_at_ms`**
  （§5.3.3 的持久载体）+ 新表 `task_ref_index`（§5.3），
  **并在同一个 migration 文件里 backfill `claim_context_json = '[]'`**
  （§9 第 5 项，r5 ⑱——不 backfill 则首次开机 sweep 把升级期间每一个在飞任务
  判 material，叠加下面的强制点即为一次必然的 Stuck-ops 事件）。
  **`TASK_COLUMNS`（`crates/calm-truth/src/db/sqlite/task.rs:19`）与 `Task` 的
  `FromRow` 建议同步 `context_stale_at_ms`**（r5 写的是"必须"，**r7 降级为
  建议**，与 §5.3.3(1) 一致）：r5 的理由是"`resume_dispatched` 读
  `tasks_nonterminal()`（`read.rs:247`）返回的 `Task`，漏改就拿不到那一列"，
  而 **r6 已经把那道检查整条删掉**了——`resume_dispatched` 里只剩上下文
  boot 门（§5.3.3(2) 的表），强制点换成了 `refuse_if_context_stale` 的一条
  **定向 SQL 读**，它不经过 `TASK_COLUMNS`。**于是这里不再有正确性耦合，
  也不再有那个 `sqlx::query_as` 的运行期失败面**（§5.3.3(1) 已如实记下"新形状
  顺带削掉了一个运行期失败面"）。仍然建议加，理由换成读端的：**切片 3b/3c 的
  `taskDiagnostics` 与可观测量要读它**。
  **但那条纪律本身仍然成立并落在切片 3b**：`declared_by` / `origin` 必须进
  `TASK_COLUMNS` 与 `FromRow`——投影**确实**读 `Task`，漏改就是运行期
  `sqlx::query_as` 失败（r3 通道 A 对同一条纪律的原始指出）。
- **判决的强制点（§5.3.3，本片与 sweep 同等重要的一半；r6 重写为一条规则）**：
  - **NEW（~10 行）`refuse_if_context_stale(tx, task_id)`** —— 读
    `tasks.context_stale_at_ms`，非空即 `CalmError::Conflict("context-stale: …")`；
  - **四个点名的调用点**，各一行，放在各自 `prepare_tx` 的最前面：
    `CodexWorkerAdapter` / `ClaudeWorkerAdapter` / `TerminalWorkerAdapter`
    （`build_worker_payload` `scheduler/mod.rs:197-253` 的封闭集合）与
    `TaskVerifyAdapter`（`operation/task_verify_adapter.rs:627`，紧邻它既有的
    `task.status != Verifying` 检查 `:651-658`）。**非 task 绑定的适配器不碰。**
  - **下游零新分支**：`Conflict` 是 `client_failure_parts` 认的永久性客户端失败
    （`operation/driver.rs:1180-1191`）⇒ op 在 `Pending` 处 `mark_failed` →
    worker 侧落到**既有的** `fail_spawn`（`scheduler/mod.rs:891`）、
    gate 侧落到**既有的** pre-bump 失败臂（`:1679-1699`）。
  - `resume_dispatched`（`:1397`）内**只保留上下文 boot 门**（新 `AtomicBool`，
    形状照抄 `boot_sweep_done` `scheduler/mod.rs:992/1015/1026/1031`）；
    **r5 的 b1/b2 判据整条删除**——"工作是否已开始"由 phase 阶梯本身回答
    （`prepare_tx` 只在 `Phase::Pending` 上跑）。
  - **boot 顺序：上下文 sweep 排在 operation 恢复漏斗与 `sweep_boot` 之前**
    （r6 前移一格；§5.3.1）。
  **没有这一条，本片交付的 sweep 是一个判决无人执行的 sweep。**
- 闭包展开（深度 3 / 节点 64 / 耗尽即 `closure_truncated`，§5.1），
  **在 claim 事务之外解析、事务内只写**（§5.1 末段）；同 cove 边界过滤。
- **第 1 级机械检测（fail-closed）**：`WaveReportEdited{wave_id}` →
  按 `dst_wave_id` 查 `task_ref_index` → 逐条重解析冻结引用 →
  不等 / 解析不到 → **一律按 `material` 处理**（此时还没有第 2 级裁决）→
  记录 + 不再派发。
- **第 1 级挂在 `WaveReportEdited` 分支里 `event_warrants_spec_push` 之前、
  对所有 author 无条件运行**（§5.3 的 ⑩）。
- **fail-closed 全量 sweep（§5.3.1，r4 ⑬）——本片的正确性载体**：
  boot（**在 operation 恢复漏斗与 `sweep_boot` 之前**，r5 更正一次、r6 再前移
  一格）+ `RecvError::Lagged` 分支 +
  既有 reconcile tick（`NEIGE_SCHEDULER_RECONCILE_SECS`，默认 300；
  **不新增环境旋钮**）；枚举源是 `tasks` 的 in-flight 行**且
  `context_stale_at_ms IS NULL`**（once-per-condition 守卫，§5.3.1），
  不是索引；`MAX_SWEEP_NODES = 4096` 用满即把本轮剩余任务判 `material`；
  末尾一条索引清扫 DELETE（§11.2 不变量 12）。**上面那条事件路径从此是延迟
  优化，不是正确性载体**——本片的验收必须能在"事件全丢"的情况下通过。
- `WaveDeleted` / `CoveDeleted` 两条 dispatcher 分支（**很薄**）：只触发一次
  sweep 降低延迟。**不改这两个事件的 payload、不做删除事务内的受影响集合计算**
  （r4 删除了 r3 的这条要求，§5.3）。
- `task_ref_index` 的清理原语 + §5.3 生产者清单的全部接线
  （含 `wave_delete_tx` / `cove_delete_tx` / `replay.rs:363` 的表清单）；
  读端一律 JOIN `tasks` 过滤 in-flight。
- 扇出上限 `MAX_RERESOLVE_FANOUT = 64`（超出 → 直接 `material`）。
- 可观测量：检测次数 / 命中次数 / `closure_truncated` 比例 / 扇出分布 /
  **每轮 sweep 的耗时、验证元组数、命中数、`MAX_SWEEP_NODES` 触顶次数** /
  **`context_sweep_last_success_age_seconds` 与
  `context_sweep_consecutive_failures`（正向健康信号，r5；没有它，
  sweep 停摆时四个每轮指标恰好一起缺席，而缺席不是告警，§5.3.1）**。

**合入后系统行为：behavior-preserving，不是 inert**（r3 通道 B MAJOR + 通道 A
MINOR，成立）。**调度语义上**的惰性证明仍然成立且已复核（无 `origin='block'`
行 ⇒ 闭包恒空 ⇒ 索引恒空 ⇒ 检测恒不命中；切片 2 只让 `task` 块**能被写下**，
行仍不存在）。**但事件面不是零后果**：每一次 legacy claim 都多一条 Tier-A 的
`TaskContextFrozen`（`refs: []`），它会动：

- goldens（min / full）与事件计数基线；
- zod schema 与 `invalidationPolicies`；
- **既有 E2E 里按事件序列/计数断言的 dispatch 用例**——今天 claim 事务在守卫式
  状态翻转之后发 `TaskDispatched`（`scheduler/mod.rs:641` 的 claim、
  `:689` 起的事件构造），本片在同一批里多插一条。

所以这一片的正确说法是**"行为保持"（behavior-preserving）而不是"惰性"
（inert）**：调度决策逐位不变，事件流多一条。**不采纳"先不给 legacy 行发空事件、
等 3b 有 `origin='block'` 行再发"**（通道 B 的备选）：那会让 §11.2 不变量 3
（"每个 `TaskDispatched` 同批必有一条 `TaskContextFrozen`"）到 3b 才第一次通电，
本片交付的是一条**从未被真实流量执行过**的硬不变量——而让它从第一天就跑在
全部既有调度流量上，正是本片最有价值的部分。

**验收**：§11.2 不变量 3（含 legacy 空集）；**id 回收的漏报属性测试**
（删块 → 下一次编辑铸出同 id 同 rev 的新块 → 必须检出，§5.1）；
**被引用块被删除 → 必须检出**（r1 漏掉的第三个洞，§5.3）；
**§11.2 不变量 13 的三条**（spec 编辑被引用块必被检出 / wave 删除 /
cove 删除，r3 的第四、五个洞）；
**§11.2 不变量 4b 的三条 sweep 构造（r4 ⑬，本片的核心验收）**：
(a) 绕过总线直接改 DB 里的被引用块 → 跑一次 sweep → 判 `material`；
(b) 事件 commit 之后立刻杀进程重启 → boot sweep 判 `material`；
(c) 抹掉 `claim_context_json` → 判 `material`。
**并且不变量 13(a)(b) 的两个变体（事件正常 / 事件被丢弃）必须给出同一结论**
——这条是"事件路径只是优化"从散文变成机制的地方；
`MAX_SWEEP_NODES` 触顶 → 余下任务按 `material`；
跨 cove 引用被拒；闭包预算耗尽 →
`closure_truncated` → 按 material；扇出超 `MAX_RERESOLVE_FANOUT` → 余下按 material；
**§11.2 不变量 12**（终结/不存在的 task 不得拥有索引行，**含"一轮 sweep 之后"
这个新断言点**）。
**r5/r6 的四条，它们是本片"判决能被执行"的验收**：
(i) **不变量 5 的三条崩溃重启构造（r6 扩到三条）**：
(a) **worker 未开始**——dispatched 行 + material 判定 → `kill -9` → 重启 →
**不得**出现新 worker 卡；worker op 终结在 `failed` 且 `last_error` 含
`context-stale`；`tasks` 行落 `failed`，**不得**长期占住 `task_budget`；
(b) **gate 未开始（r6 新增）**——`verifying` 行 + `gate_attempt = 0` +
material 判定 → `kill -9` → 重启 → **不得有任何 gate shell 命令被执行**，
行落 `failed` 且 `gate_result.log_tail` 含 `context-stale`；
(c) **已开始的不受影响**——op 已在 `SpawnStarted` + material 判定 → 重启 →
照常被恢复驱动到终结、worker 照常汇报（**这条防的是过度收紧**）；
**再加一条 r6 发现 2 的专门回归**：op 停在 `Phase::Pending`（`submit` 已插行、
spawn 前崩溃）+ material 判定 → 重启 → **必须被拒**，不得因为"有 operation 行"
而被当成"工作已开始"；
(ii) **不变量 5b（r6 重述为对 boot 顺序的断言；r7 点名它的两个 CI 载体）**——
**两条都要，缺一不可**：
**(ii-a) 源码序测试**：往既有的 `mod boot_order_tests`
（`crates/calm-server/src/lib.rs:611-705`，`include_str!("main.rs")` + 偏移比较）
加一格 `boot_order_context_sweep_before_operation_recovery`，断言
`context_sweep_on_boot` 的偏移 < `recover_operations_on_boot`（`main.rs:73`）
< `scheduler_sweep_on_boot`（`:79`）；范例是同模块的
`boot_order_scheduler_sweep_after_operation_recovery`（`:678-688`）。
**理由**：boot funnel 是 `main()` 的直线代码，运行期不可调用——
一个自己按顺序调 pub 函数的测试什么都没断言（§11.2 不变量 5b）。
**(ii-b) seam 测试**：卡住上下文 sweep → 跑 boot → 断言 **(1) 没有任何
operation 被恢复驱动**、(2) 没有任何 dispatched 行被 `resume_dispatched`
重驱动；放开 → 两者照常发生且行不丢；
(iii) **不变量 4c**（judged material 之后连跑 3 轮 sweep，
`TaskContextAdvanced` 计数恒为 1）；
(iv) **升级路径专测**：造一个"加列之前就在飞"的行（`claim_context_json IS NULL`）
→ 跑 migration → **backfill 必须把它变成 `'[]'`** → 首次 sweep **不得**判它
material（§9 第 5 项、r5 ⑱）。**这一条是本片唯一一条会在生产首次部署当天
决定成败的测试。**
**事件面的连带更新必须列进本片的完成定义**：goldens(min/full) 重生成、
zod + `invalidationPolicies` 同步、`generated-events` 产物、
**两个** NEW 事件（`TaskContextFrozen` + `TaskContextAdvanced`）的
role_gate kernel-only 否定测试，
以及**逐一修正既有 dispatch E2E 的事件序列断言**——不得以"零后果"为由跳过。

### 切片 3b — 投影 + rebuild + 迁移 + 存量护栏（~900 行，**这一片让声明开始有后果**；前端已拆出到 3c）

依赖：切片 3a。**这一片必须一次带齐它的全部护栏**——拆开就会有一次 merge 处在
"声明能派任务、但护栏还没上"的状态，正是 r1 判定不可接受的那个形状。

- `project_tasks_tx` + `tasks_rebuild_tx`；接入 `persist_report_with_shadow`
  （插在 `wave_report.rs:542`–`:565` 之间）。
- migration：`tasks.declared_by` / `tasks.origin`
  + `waves.spec_task_ceiling` + `waves.automation_policy`（见下）。
  - **`TASK_COLUMNS` 必须同步**（NEW，r3 通道 A MINOR，成立）：
    `crates/calm-truth/src/db/sqlite/task.rs:19` 是 `tasks` 的唯一 SELECT 列表
    拼写，被 `:33`（`tasks_by_wave_tx`）、`:131`（`task_get_tx`）与
    `read.rs` 的池读共用；新增两列同时要上 `Task` 结构体的 `FromRow`
    （投影与 §8 的聚合都要读它们）。**这是 `sqlx::query_as` 的运行期失败面，
    不是编译期**——漏改会在运行时变成 Stuck ops。
  - **`waves` 的两列反其道而行**：照 #644 的先例
    （`crates/calm-truth/src/db/sqlite/wave.rs:168-187` 的注释逐字写明这是
    刻意取舍）——**不上 `Wave` 结构体**，因此不动任何 `SELECT` 列表、
    不动 `WaveUpdated` wire payload、不动 ts-rs 导出；写面是 `wave_update_tx`
    里的定向单列 UPDATE，读面由 `project_tasks_tx` 直接走 SQL。
- **两列的 PATCH / OpenAPI 写面**（NEW，r3 ⑪，§6.6 末段）：`WavePatch` 的两个
  新字段 + REST 校验 + `patch_has_other_changes` 判空列表
  （`routes/waves.rs:880-886`）+ OpenAPI/zod/web 生成物。
  **没有这一块，两列在落地当天就是不可写的死列。**
  - **两列都是 user-only（NEW，r6 通道 A MINOR，§6.6 末段）**：`update_wave`
    （`routes/waves.rs:812-887`）今天只对 `lifecycle` 做 actor 闸
    （`validate_transition`，`:849`），而 `X-Calm-Actor` 是自述的
    （`actor.rs:28-33`）。加一条镜像它的检查：非 `ActorId::User` 写这两列
    → `Forbidden`，写之前拒、不落行不发事件。
    **没有这一条，§3.7 规则 5 逐块守住 `released_by_user` 是白守的**——
    `PATCH automation_policy='auto-declare'` 一次就把整个 wave 的否决清掉。
    `task_budget` / `require_task_gates` 的既有行为**不动**（§13.25）。
- `task_ref_index` 在 wave/cove 删除与 fixture 重置里的清理（§5.3、§9 末段）
  —— 若切片 3a 未接线完，本片必须补齐。
- `task_delete_pending_tx`（NEW，§2）；legacy 同 key 收编规则（§9 第 3 项）。
- 墓碑投影（§6.1）+ 同 key 重声明的拒绝 + **`key` 复活规则**（§4.2 规则 2b）。
- **`evaluate_schedulability_tx`（NEW，r4 ⑭）——唯一的可调度谓词**：
  纯函数的块局部诊断 + 四类需要 DB 的诊断（跨 cove / `unknown_deps` /
  `spec_task_ceiling` / `declare-and-wait` 放行）。`project_tasks_tx`、
  `tasks_rebuild_tx`、读端 `taskDiagnostics` **三条路径共用它**；
  读端在一个**读事务**里调，不是调纯函数（否则删行的四类原因渲染不出来）。
  - **r5 ⑯ / r6 ⑳：`spec_task_ceiling` 在这里必须实现成"对声明集合的确定性
    准入"（§4.2 规则 3″），不是裸 `count(*)`，且排除集必须按 r6 的判据划——
    `occupied` 只数 `dispatched/running/verifying`，`pending` 行一概不数**。
    这是本片**最容易被写错的一处**，而且它已经被写错过两次：三条路径共用一份
    实现只保证"同一个函数"，不保证"幂等的函数"（r5）；按"诊断是否为空"划排除集
    则同时破坏幂等与上界（r6，两个通道从相反方向各命中一半）。
    切片验收里 (i-a)(i-b) 两条就是钉住这两个方向的。
  - **r5 ⑰：`declare-and-wait` 的判据是 `effective_policy(wave)`**
    （策略列非 NULL 则用它，否则看文档里有无 `tombstoned_by:"user"` 的墓碑，
    §6.6）。两个输入本函数本来都有，不新增任何输入。
- `Event::PlanUpdated` 的发射与抑制；诊断的读时投影面
  （`GET .../report` 与 `calm.report.read` 的 `taskDiagnostics`，§4.2 规则 7）。
- **护栏（r2 新增，从切片 6 前移）**：
  - `waves.spec_task_ceiling`（默认 32，§8(A)）—— **单 wave 内 spec 声明的
    非终结行（未结存量）上限**。理由见下；
  - `waves.automation_policy`（**`TEXT NULL`**，NULL = 内核默认，§6.6）——
    若不在这一片，从 3b 到切片 6 之间**根本没有办法**把一个高后果 wave 切到
    `declare-and-wait`，而这一片恰恰是声明第一次能驱动调度器的那一片；
  - **`effective_policy` 的派生分支（r5 ⑰）—— 人的否决把该 wave 翻成
    `declare-and-wait`**（§6.1/§6.6）。**它必须与墓碑投影同片**：墓碑投影与
    策略列都在本片，而这条机制正是"人否决 → spec 换 key 重提"这条死循环的
    唯一非启发式防线。零新列、零新字段。
- **API 层**：诊断读端字段 `taskDiagnostics`（`GET .../report` +
  `calm.report.read`）**留在本片**——它是护栏（没有它，删行的原因在任何客户端
  上都取不到）。**前端渲染拆到切片 3c**（r5）。

**为什么这两列必须前移**（r2 通道 A BLOCKER，成立）。r1 §12 为"切片 3 先于
切片 6"所作的辩护只覆盖了**递归**（"`spawn: sub-wave` 还不存在"），从未处理
**单个 wave 内的未结存量**；而 `task_budget` 是**并发容量**不是存量上限
（`compute_ready` 的 `capacity = budget - running_cost` 后 `take`，
`scheduler/mod.rs:164`，默认 1 于 `:80`），所以一个失控的 spec 可以声明无界的
pending 行，以并发 1 串行地永远排下去。这与 r1 拒绝旧切片 1 的理由是同一条。
`spec_task_ceiling` 是一列 + 一条 `count(*)` + 一条诊断，代价远小于它挡住的东西。

**这一片合入后是自洽的**：声明能驱动调度，而 (i) 未结存量有上限、(ii) 高后果 wave
可切 `declare-and-wait`、(iii) 失效检测以**最保守**形态在位（切片 3a 的
`material` 全判），误报只会让人多看一眼，不会让 agent 拿过期上下文继续跑。
第 2 级（切片 4）只是**减少误报**。

**验收**：§11.1 的重建断言全部五条 + **§11.2 不变量 11 的 rebuild≡增量差分测试**；
重复 key / 环 / 未知依赖 / 缺 gate 四类诊断各一条单测；
**§11.2 不变量 9 的 `key` 复活端到端测试**（删任务→墓碑→删墓碑→重提落新行）；
`ready` 从 true 撤回 → 行被删除、rebuild 结果相同；
一次报告写恰好 3 个事件（声明无变化时 2 个）；
`spec_task_ceiling` 超限 → 不落行 + 诊断；
**`declare-and-wait` 全链路**：spec 写 `ready:true` → 不落行 + 诊断 →
人写 `released_by_user:true` → 落 pending 行 → 人改回 `false` → 行被删除；
spec 试图写 `released_by_user` → 400（§11.2 不变量 8(e)）；
**PATCH 两列 → 200 且发 `WaveUpdated`**（含"只改策略"的 patch 不被判空短路）；
§11.2 不变量 3b（task 块自身在冻结集内，改 in-flight 的 `goal` 必被检出）；
**诊断触发删除**：已 pending 行的块被改出重复 key/环 → 该 pending 行被删除，
rebuild 给出同一终态（§11.2 不变量 11 的生成器覆盖它）；
**四类需要 DB 的诊断在读端可见（r4 ⑭）**：跨 cove 引用 / `unknown_deps` /
`spec_task_ceiling` 超限 / `declare-and-wait` 未放行——每一类都要一条断言
"pending 行被删除**且** `GET .../report` 的 `taskDiagnostics` 里能读到它的原因"，
两者缺一即为静默降级；`project_tasks_tx` 与读端走的是同一个
`evaluate_schedulability_tx`（可用一条"同输入同输出"的等价测试钉住）。
**r7 新增两条（规则 3‴，`unknown_deps` 的入参只含在飞行）**：
(A) **同一事务内不得分叉**：声明 `k1` 与 `k2`（`depends_on: [k1]`），两者皆
`pending`；一次编辑删掉 `k1` 的块 → **同一次求值**里 `k2` 必须**立即**拿到
`unknown_deps` 诊断并被删（而不是"存活到下一次求值再被删"），
且紧接着的 `tasks_rebuild_tx` 与读端 `taskDiagnostics` 给出**同一答案**
（这是 §11.1 等号在 `unknown_deps` 这一侧的钉子）；
(B) **迁移面专测（存量库上线当天的行为）**：一个 `origin='legacy'` 的
`pending` 行 + 一条 `depends_on` 指向它的块声明 → 该声明**得到
`unknown_deps` 诊断、不落行**，且诊断在 `taskDiagnostics` 里可读；
物化（切片 7）之后诊断消失、行正常落地。**这一条必须写进本片的 PR 描述**
——它是存量库上线当天**唯一**会出现"突然多出来一批诊断"的地方。
**r5/r6 的验收**：
(i) **ceiling 的幂等性、不抖动与上界**（⑯ + r6 ⑳）——同一份文档上连续两次
`tasks_rebuild_tx` 给出逐字节相同的 `tasks`；`ceiling = 2` 且已落地
`{k1,k2}` 时再声明 `k3` → **k1、k2 的行必须不动**、只有 k3 被拒；
**且 ceiling 诊断在删行之后仍然可从 `taskDiagnostics` 读到**（通道 B 的角度）。
**r6 新增两条，它们分别钉住 r5 写法的两个反例**：
(i-a) **在飞行必须计入 `occupied`**（通道 A 的构造）：`ceiling = 1`、`k1` 已
`dispatched` 且**仍被声明**，把 `k2` 的块插到 `k1` **之上** → `k2` 必须被拒，
该 wave 的非终结 spec/block 行数**恒为 1**（不变量 7b）；
(i-b) **带诊断的在声明 key 不得计入 `occupied`**（通道 B 的构造）：
`ceiling = 1`、`k1` 仍被声明但带别的诊断（因而有一条即将被删的 `pending` 行）、
`k2` 干净 → `k2` **必须被准入**，且连跑两次求值答案相同；
(ii) **不变量 6b 的三条分支**（换 key 被挡 / 删墓碑恢复 /
显式 PATCH `automation_policy` 恢复且墓碑保留，⑰）；
(iii) **人否决导致同 wave 其它未放行的 spec pending 行被删除**——
这是 ⑰ 定价过的代价，必须**有测试固定住**，并在 `taskDiagnostics` 里说明原因；
**(iv) §11.2 不变量 8(f)（NEW，r6）**：非 user actor PATCH
`automation_policy` / `spec_task_ceiling` → **403**、列值不变、无事件；
人 PATCH → 200 且发 `WaveUpdated`。

### 切片 3c — 前端：状态回显 + 诊断渲染（~350 行，NEW，r5 从 3b 拆出）

依赖：切片 3b。**纯前端，回退它不改变任何服务端行为。**

- `task` 块的状态回显（§3.6：`tasks` 行的 status / status_detail /
  gate_result / worker_card_id 读时贴在块上，不产生文档写）。
- `taskDiagnostics` 的渲染，含 r5 新增的两类文案：
  **「本 wave 有未清除的人工否决，spec 声明的任务需要逐条放行」**（⑰，
  必须写明两条清除办法）与**「该任务在过期上下文上被中止（context-stale）」**
  （⑮/⑲，必须写明"要重做请换一个 key"）。
  **后者有两种形态，文案必须分得开（r6）**：(a) **工作从未开始**——
  行落 `failed`、`TaskFailed.reason` 含 `context-stale`；(b) **worker 跑完了、
  但 gate 未开始因而被拒**——行落 `failed`、`gate_result.status_detail =
  "gate-infra"` 且 `log_tail` 含 `context-stale`。(b) 的文案必须说明
  **worker 的产出仍在**（worker 卡与日志都在），只是**没有被验证过**。
- `released_by_user` 的人用开关（人可写、spec 不可写，§3.7 规则 5）。

**为什么可以拆**：3b 的"护栏必须与后果同片"论证覆盖了除前端以外的一切——
**护栏是 API 层的 `taskDiagnostics` 字段，不是它的渲染**。拆开之后 3b 仍然
自洽（诊断可读、行受守卫），只是**在 3c 之前诊断只能从 API 读到**。
**如实记的窗口**：3b 到 3c 之间，人在 UI 上看不到"为什么这条没排队"。
这是**可见性延迟**，不是静默降级（原因始终可取）；若这个窗口令人不安，
3c 应与 3b 同时发布，但它们不必是同一个 PR。

**验收**：三类诊断文案各一条渲染测试；状态回显不产生任何文档写
（断言该页面加载后 `WaveReportEdited` 计数不变）。

### 切片 4 — 第 2 级 LLM 裁决 + 冻结点推进（~700 行）

依赖：切片 3a、3b。**纯粹是降误报**，回退它只会让系统更保守。

- 按 `task_ref_index` 扇出路由（`MAX_ADJUDICATION_FANOUT = 16`），
  `HarnessObservation::ReportEdited`（`dispatcher/mod.rs:1308`）附加 claim 上下文。
- 裁决工具（fail-closed：缺席/畸形/超时 → `material`）。
- *（`Event::TaskContextAdvanced` + role_gate 条款 + Tier-A 全流程
  **已在切片 3a**，r5 前移——没有它，3a 交付的是一个判决无处可记的 sweep。
  本片只是把它的 `verdict` 从"恒为 material"变成"由裁决决定"。）*
- 冻结点推进（判 immaterial 时推进到新的 `(rev, content_hash)`）+ 可观测量
  （检测次数 / 送裁决次数 / material 次数 / `closure_truncated` 比例）。

**验收**：§11.2 不变量 4、5；裁决超时/畸形返回 → material 的 fail-closed 测试；
判 immaterial 后同一变更不再重复触发（**与不变量 4c 的 once-per-condition 守卫
成对：material 侧靠 `context_stale_at_ms` 不重发，immaterial 侧靠冻结点推进
不重发**）；扇出超限 → 剩余按 material。

### 切片 5 — 模板 fork + `calm.plan.upsert` 退场（~1000 行）

依赖：切片 3b。

- `POST /api/waves` 的 `fork_report_from`（§7.2）+ `entity_kind: "view"` /
  `kind: "template"` overlay 标记。
- **块层面的 `neige://` 重写 + 保留源 block id**（§7.3）——本切片风险集中在这里。
- fork 强制 `ready: false` + `declared_by: "spec"`（§7.2）。
- `calm.plan.upsert` 转为隐藏 shim（复刻 `emit.rs:85-124`）；
  `plan_template` / `gates` / `spec_instructions` 的 manifest 字段
  **保留可解析但标注为过渡**（Tier-A 字段不可删）；spec 提示改为指向报告模板。

**验收**：**`fork(D)` 的块 id 序列逐个等于 `D` 的**（§7.3 的硬测试）；
内部引用全部指向新 wave id、外部引用逐字节未变（错了是静默的，必须专测）；
fork 出的 wave 里没有任何 `ready: true` 的 `task` 块；
fork 后立刻 rebuild，声明一致、状态全新；`calm.plan.upsert` 不在 tools/list。

### 切片 6 — 树级预算 + 深度上限 + `spawn: sub-wave`（~700 行）

依赖：切片 3b。

- `waves.parent_wave_id` / `waves.tree_task_budget` / `MAX_WAVE_TREE_DEPTH`；
  投影内强制（§8(B)）。
- `spawn: "sub-wave"` 的落地：子 wave 创建 + 父块的 `neige://wave/<child>` 反链。
- *（r2：`waves.automation_policy` 已前移到切片 3b，见下。）*

**为什么它不必先于切片 3b**（与 r1 的分歧点，r2 已重新论证）：切片 3b 落地时
`spawn: "sub-wave"` **还不存在**（§3.5 默认 in-wave），所以**递归展开在机制上
不可能发生**；树预算与深度上限约束的正是"子 wave 再 spawn 子 wave"，
而那条路径与它同片落地。
**但 r1 的论证到此为止是不够的**（r2 通道 A BLOCKER）：它只覆盖了递归，
没覆盖**单个 wave 内的未结存量**，而 `task_budget` 是并发容量不是存量上限
（§8 的表）。修正是把 `waves.spec_task_ceiling` 与
`waves.automation_policy` 前移到切片 3b —— 本片因此只剩树与递归。

**验收**：递归展开在深度 3 / 树预算 32 处停住并渲染成诊断；
`declared_by='user'` 的行不计入深度与树预算。

### 切片 7 — 迁移物化工具 + cove 读时聚合（~700 行）

依赖：切片 3b。

- admin CLI 的 legacy→block 物化命令（§9 第 4 项，人触发、幂等、只写声明字段）。
- cove 级任务聚合视图：**读时聚合，不新建存储**（复用 `waves_by_cove` +
  `tasks_by_wave`，与 `backlinks_for_wave` 的读时派生同构）。

**验收**：物化后 rebuild 无差异（幂等）；**物化一个含 `pending` legacy 行的 wave
后该行仍在且仍可被调度**（`ready:true` 的理由，§9 第 4 项）；
**规则 1 的两个豁免点被一处枚举并各有一条测试**（fork 与物化工具；
任何第三个豁免点出现即测试失败）；聚合视图在 100 wave 规模下的响应时间基线。

### 依赖图

```
#979（doc_rev，已在 979-if-rev worktree 落地）
   │
切片 1（惰性：kind + 校验 + 谓词下沉）
   │
切片 2（惰性：guard 收口 + 人的写口 + 反链）
   │
切片 3a（惰性：冻结 + 反向索引 + 第 1 级检测 + sweep + 判决载体/强制点 —— 护栏先于后果）
   │
切片 3b（投影 + rebuild + 迁移 + 存量上限 + 策略列 + 诊断 API —— 声明从此有后果）
   ├── 切片 3c（前端：状态回显 + 诊断渲染）
   ├── 切片 4（LLM 裁决，降误报）
   ├── 切片 5（模板 fork + upsert 退场）
   ├── 切片 6（树预算 + 深度上限 + sub-wave）
   └── 切片 7（物化工具 + cove 聚合）
```

**每一片的自洽性自查**（这是重切的验收标准）：

| 片 | 合入后系统处于什么状态 | 有没有半截机制 |
|---|---|---|
| 1 | 多一个可写块 kind，无任何行为后果 | 无 |
| 2 | 人能写/删块，归因与不对称已强制，但块仍不驱动任何东西 | 无 |
| 3a | 调度决策逐位不变；每次 claim 多一条空 `TaskContextFrozen`（事件面**有**变化，见本片的连带更新）；检测器与 sweep 恒不命中（无 `origin='block'` 行 ⇒ 冻结集恒空）；**判决的载体与强制点同片就位**（r5 ⑮：否则本片交付一个判决无人执行的 sweep）| 无 |
| 3b | 声明驱动调度，且**未结存量上限 + 策略列 + 人否决的翻档 + 诊断 API + 最保守的失效检测**同片在位 | 无 |
| 3c | 诊断从"只能从 API 读"变成"UI 上看得见"；服务端行为零变化 | 无（回退 = 可见性回退，不是机制回退）|
| 4 | 同上，误报变少 | 无（回退 = 变保守） |
| 5 | 多了模板；fork 强制 `ready:false` 故不会自发派发 | 无 |
| 6 | 多了 sub-wave，同片带树预算与深度上限 | 无 |
| 7 | 存量可视化 + 聚合读 | 无 |

**今天即可无阻塞开工的**：切片 1。

---

## 13. 风险 / 未解

1. **in-flight 任务无法撤回（§6.5）。** 这是本设计里两处"人的意图被机器
   拖延"之一（另一处是风险 4，同一个根因）。缓解是可见诊断 + 终结后生效的墓碑；根治需要一个跨 operation /
   worker / gate 的补偿取消设计，本文明确不做。**这条应该单开 issue。**
2. **块 id 稳定性依赖对齐启发式。** 身份问题已用 `key` 绕开（§3.3），但
   **引用锚**仍然是 block id。一次大幅整文档重写可能让 `refs[]` 指向的 id 失效
   → 闭包解析不到 → 按 fail-closed 应判 material，可能造成误中止潮。
   缓解：引用解析失败时的诊断要明确指向"引用已失效，请重新链接"；
   `write_markdown` 的 `<!-- neige:b_xxxx -->` marker 通道（`align.rs:49` 的 hint）
   是缓解手段但需要写方配合。**未量化。**
   *（注意 hint 通道只在"该 id 存在于旧块中"时有效，`align.rs:44-47`——
   它能稳住既有块，救不了 fork 那种零旧块的场景，§7.3 已改用别的办法。）*
3. **§3.7 的收口是本设计给既有写路径引入的最重改动。**
   它让 `apply_report_op` 从"校验文档形状"变成"改写 op + 校验文档形状 + 写者身份"，
   并给它加了一个 `author` 参数与两次 `blocks_snapshot()`。风险有三：
   (i) 任何一条规则写松了，§4.4 的归因与 §6.1 的不对称同时失守；
   (ii) 改写器（规则 4：人删 → 原位墓碑）把一次 `DeleteBlock` 变成一次
   `UpsertBlock`，与对齐器、`if_rev` 承接、块顺序的交互需要专门测试；
   (iii) **规则 4 在整文档 `Replace` 上不可达**（`guard_non_prose_stomp` 先拒），
   所以"人删任务 → 墓碑"这条默认路径**完全依赖 UI 走块级 DELETE 端点**——
   前端若在某处退回整文档写，人会看到一个费解的 400 而不是墓碑。
   缓解：§11.2 不变量 8 的否定测试矩阵（含 `Replace` 路径的 400 + 指路文案）。
4. **裁决与 gate 之间没有栅栏**（§5.3 末段）。一个已在跑的 worker，其 gate
   可能在 stale 裁决返回之前落定；本设计**明确不造**这个栅栏，因为它需要
   "中断正在跑的东西"——与风险 1 同一个根因。**这是本设计第二处"人的意图被
   机器拖延"。**
   **r6 收窄了这条豁免的范围，两条都要如实记**：
   - **豁免只覆盖"已经开始的那一次 gate"**（op 已越过 `prepare_tx`）。
     一次**判决之后才首次启动**的 gate 会被 §5.3.3 的规则拒掉——r5 之前把这
     两种情形当成一种，于是"gate 首启"这条路径实际上是从强制点旁边走过去的
     （r6 通道 A MAJOR）。收窄之后 §13.4 说的仍然是真话，只是范围小了。
   - **代价：判 material 之后不会再有新的 gate 执行**，于是一个 worker 可能
     已经产出了完全可用的东西，却以 `failed`（`gate-infra` + `context-stale`）
     收场，人要重做必须换一个 `key`（§4.2 规则 2b）。定价与理由见 §6.5，
     与 §13.23 的 b2 代价同源。
   - **不新增 gate 原因枚举值**：`gate_result.status_detail` 沿用既有的
     `"gate-infra"`，过期信息只在 `log_tail` 里（`reconcile_gate_outcome`
     的既有映射，`scheduler/mod.rs:1639-1651`）。这是**刻意省下的一次 wire 面
     变更**，代价是 UI 上这两类失败长得一样，只能靠文案区分（§12 切片 3c）。
5. **「编辑稀疏」是未验证的假设**（评审第 5 条自承）。切片 4 的可观测量是它的
   证伪装置；若稀疏假设不成立，三级阶梯的第 2 级（LLM 裁决）会变成主要成本项。
6. **`ready` 门的实际强度未知。** 今天 `acceptance` 只校验非空。"AI 必须先能把它
   变成可验收的"这条门在机制上等价于"有个非空字符串 + 有 gate 或有
   `no_gate_reason`"。这可能不够；但更强的判据（例如"gate 必须可执行"）需要在
   声明时就能跑命令，那是另一个设计。
7. **#976（活数据块）无法同步。** 它不在代码里（§0.1 #16）。本文只作弱约束
   （§3.6：状态回显不引入新的读时数据源抽象）。若 #976 先落地且形状不同，
   §3.6 需要复审。
8. **#761（workflow 组合）被削弱但未解决。** 模板化后"组合两个流程 ≈ 拼两份
   文档"，但**依赖/顺序语义仍未设计**：两份模板各自的 `depends_on` 只在文档内
   有效（§3.2），拼起来之后跨模板的顺序靠什么表达，本文未答。
9. **树预算默认值 32 / 深度 3、闭包预算 3/64、扇出 16 全是猜的。** 没有数据支撑，
   只有"必须有上限"这条论证。切片 4 的可观测量（尤其 `closure_truncated` 比例）
   是它们的校准装置。
10. **`declared_by` 的语义边界模糊**（§4.4 自述）：人改了 AI 任务的措辞后归因仍是
   `spec`。这在"事后审视为什么做了这件事"时可能不够——完整答案要读事件日志。
11. **`tasks` 表会积累"文档不再声明、但已派发过"的行**（§11.1 的代价段）。
   它们是执行史，不是声明；读端必须能把它们和活声明区分开，否则 UI 上会出现
   "这个任务哪来的"。没有清理机制——刻意的：删掉执行史就是删掉证据（#330）。
12. ~~**切片 3 是一片大活（~1500 行）**~~ **已在 r2 拆开**：3a（冻结 + 反向索引 +
   第 1 级检测，行为保持，~700 行）+ 3b（投影 + rebuild + 迁移 + 存量上限 + 策略列，
   ~1100 行）。~~3b 仍略超惯例，且**不能再拆**~~ **r5 又拆了一刀且拆对了**
   （通道 A MINOR）："护栏必须与后果同片"的论证覆盖不到**前端**——状态回显与
   诊断渲染不是护栏，**API 层的 `taskDiagnostics` 字段才是**。于是前端拆出为
   **切片 3c（~350 行）**，3b 回到 ~900 行。**同一轮里 3a 反向长了**
   （~850 → ~1000）：`TaskContextAdvanced` + 判决载体 + 强制点从切片 4 前移，
   因为没有它们那一片交付的是一个判决无人执行的 sweep（⑮）。
   **教训**：这两处调整方向相反，但用的是同一条判据——**"这一件事是不是让
   本片的核心断言成立所必需的"**，而不是行数。行数只是它的副产品。
13. **§4.2 规则 3 的等价性属性测试是"投影不比 `calm.plan.upsert` 松"的
   唯一机制保证。** 它一旦被跳过或写弱，那条断言就退回散文。切片 1 必须交付它。
14. **闭包展开与重解析的开销未量化，且它没有天然的常数上界**（§5.1 末段、
   §5.3，**r3 通道 A MAJOR 改写**）。每次 claim 最多 `MAX_REF_NODES = 64` 次
   `cards.payload.blocks` 行读（在 claim 事务**之外**做，所以不持锁）。
   r2 说"上界由 in-flight 任务数（全局 8 permits）封住"——**这是错的**：
   `DEFAULT_PERMITS = 8`（`dispatcher/mod.rs:55`，注释与 `:475` 都写明是
   **global concurrent-spawn cap**）限的是**同时 spawn**，不是生命周期持有量；
   `dispatched/running/verifying` 的真实上界是 **Σ 各 wave 的 `task_budget`**
   （`compute_ready` 只在单 wave 内做 `capacity = budget - running_cost`，
   `scheduler/mod.rs:164-191`），而 **wave 数无界**。所以一次 `WaveReportEdited`
   的重解析量上界是 (Σ per-wave `task_budget`) × `MAX_REF_NODES`，
   **靠常数封不住**——正是"一个 wave 被很多 claim 引用"的形状。
   缓解是 §5.3 新增的 `MAX_RERESOLVE_FANOUT = 64`（超出直接判 `material`，
   fail-closed 因而不引入漏报）。**仍然没有实测**：切片 3a 的可观测量必须包含
   闭包解析耗时、每次触发的重解析条数、以及扇出触顶的比例。
   **r4 补**：sweep（§5.3.1）把同一笔开销变成了**周期性的常态支出**，
   它的上界与预算记在 §13.21，与本条是同一类代价。
15. **`if_doc_rev` 是一次活工具的破坏性契约变更**（§12 切片 2）。正在运行的
   spec agent 上下文里是旧 schema，下一次 create/move 调用会吃 `-32602`。
   缓解是"指路的错误 + 可自愈重试"，但**没有版本协商机制**——项目今天也没有
   MCP 工具 schema 版本化。若将来 spec 会话变长，这类变更需要一条真正的迁移通道。
16. **引用闭包被限制在同 cove（§5.1）** 是保守裁决。若出现真实的跨 cove 引用需求
   （例如一个 cove 的规范被另一个 cove 的实现引用），需要重新设计裁决观测能携带
   什么，而不是简单放开边界。
17. **`content_hash` 是抗碰撞检测，不是不可能性证明**（§5.1）。"不允许漏报"这条
   断言的精确形式带一个 SHA-256 碰撞的例外。这是可接受的残余风险，但它意味着
   该断言**不能**写成全称命题，也不能作为更强推理的前提。
18. **`spec_task_ceiling` 只约束未结存量，不约束生命周期总量**（§8(A)，
   r3 通道 B MAJOR）。"每完成一条就再声明一条"的细水长流式失控不被它挡住。
   本文**刻意不引入累计配额**——单调计数器不是当前文档的函数，rebuild
   重建不出（§11.1），会成为 §2 承重墙上的第三个真源。证伪装置是"每 wave 的
   spec 声明速率"这条可观测量；真出现时再上速率闸或 epoch 配额（它们可以是
   纯运行时机制，不进声明真源）。
19. **`released_by_user` 是本设计给 `task` payload 加的第二个"权属位"**
   （§6.6 的 ⑪；第一个是 `declared_by`/`tombstoned_by` 那一对）。三个位都
   **只能靠 §3.7 的收口守住**——`validate_payload` 是纯函数，看不见写者。
   §3.7 的规则集因此从 4 条长到 7 条，风险与 13.3 同源：任何一条写松，
   归因、否决权、放行权三者中的一个就失守。缓解是 §11.2 不变量 8 的
   否定测试矩阵（r3 已扩到覆盖新规则）。
20. **`task_ref_index` 的清理点分散在九条路径上**（§5.3 的生产者清单）。
   设计上已用"读端 JOIN `tasks` 过滤 in-flight"把正确性与清理完备性解耦
   （漏一个点 = 代价 bug 而非正确性 bug），但**表会长胖**。
   ~~且没有兜底的周期性清扫~~ **r5 已把这条兜底做进 sweep**（采纳本条自己提的
   建议）：每轮 sweep 末尾一条
   `DELETE FROM task_ref_index WHERE task_id NOT IN (SELECT id FROM tasks
    WHERE status IN ('dispatched','running','verifying'))`（§5.3.1）。
   于是 §11.2 不变量 12 得以保留**全称**形式（此前它与本条的"刻意容忍"
   互相矛盾——r5 通道 A MINOR）。残余风险只剩"sweep 本身停摆"，
   见 §13.21 的健康信号。
20b. **一条 r3 遗留的错判已被删除**：r3 曾要求删除事务内先读出受影响 `task_id`
   集合"随事件一起落地"，而 `Event::WaveDeleted { id, cove_id }` /
   `Event::CoveDeleted { id }`（`crates/calm-types/src/event.rs:419-425`）
   **没有这个载体**，加字段是一次 Tier-A wire 变更，而同一节刚以同样理由驳回过
   "给 `WaveReportEdited` 加变更块 id"。r4 用 sweep 替代了它（§5.3）。
   **这条留在这里是为了记住一个模式**：本设计里凡出现"某个事务算出一个集合、
   指望订阅者收到它"，都要先问一句总线丢了怎么办——§0.2(g) 的答案是"会丢"。
21. **事件丢失 / 重启：本设计此前完全没有的一类风险**（NEW，r4 通道 A BLOCKER，
   §0.2(g)/§5.3.1）。总线自承 lossy、每条 envelope fire-and-forget、
   唯一的跨重启补投对跨 wave 引用失明——这一类在 r1–r3 的 20 条风险里**一条都没有**，
   因为那三轮只审了"谁能改内容"，没审"改了之后凭什么保证有人去看"。
   缓解是 §5.3.1 的 fail-closed sweep，它把这一类从**正确性风险**降级为
   **延迟风险**，但残余的三条要如实记：
   - **发现窗口 = 一个 reconcile 周期**（默认 300 秒）。事件路径正常时是毫秒级，
     丢事件 / 重启时最坏 300 秒。这个窗口里一个 worker 可能拿着已失效的上下文
     继续跑，并且它的 gate 可能落定（§13.4 的同一根因）。
     调短周期就是加 sweep 频率，代价见下条——**这是一个可调的取舍，不是一个洞**。
   - **sweep 的代价没有实测**：一轮 = （in-flight 行数）× `MAX_REF_NODES` 次
     `cards.payload.blocks` 行读 = (Σ per-wave `task_budget`) × 64，
     每 300 秒一次、在读连接上做。`MAX_SWEEP_NODES = 4096` 是硬顶，
     用满即把本轮剩余任务判 `material`（fail-closed，因而不引入漏报，
     只在极端规模下制造误报潮）。**4096 与 300 秒都是猜的**，与 §13.9 的其它
     常数同一性质；证伪装置是切片 3a 的 sweep 可观测量（耗时 / 触顶比例）。
   - **sweep 只覆盖 in-flight**。已终结任务的上下文不再被验证——这是对的
     （没有可失效的工作），但它意味着"跑完之后才发现依赖变了"永远只能靠人，
     与 §5.3 第 3 级一致。
   - **sweep 自身的失败在 r4 里完全不可观测**（NEW，r5 通道 A MINOR，成立）。
     r4 给的四个可观测量全是**每轮指标**，也就是说**恰恰在 sweep 停摆时它们
     一起缺席，而缺席不是告警**——对一个承载全称断言的机制，这是一个真实的洞。
     两种停摆都已复核：(i) 既有 sweep 的形状是"DB 出错 → warn + 跳过本轮"
     （`scheduler/mod.rs:1050-1055`），**反复失败会静默降级这条保证**，
     不只是延迟一个周期；(ii) reconcile tick 是 `tokio::spawn` 里的裸 `loop`
     （`dispatcher/mod.rs:814-829`），**一次 panic 静默终结该进程余生的所有
     sweep**。缓解是 §5.3.1 新增的正向健康信号
     （`context_sweep_last_success_age_seconds` + 连续失败计数，
     告警阈值 `3 ×` reconcile 周期）——它对两种停摆同时有效。
     **如实记**：健康信号只让停摆**可见**，不让它不发生；在信号被接上告警之前，
     这条保证的实际强度等于"有人在看那个 gauge"。
22. **人的否决是 `key` 作用域的，换一个 `key` 即可完全绕过**（NEW，r4 通道 A
   MAJOR，§6.1）。墓碑按 `key` 落块、§3.7 规则 2b/3 保护**那一块**、
   §4.2 规则 2b 按 `(wave_id, key)` 判定、§11.2 不变量 6 按 `key` 断言——
   换 key 之后没有一条命中，于是 §6.1 开头点名的那条"人删一次、AI 建一次、
   每一轮都合法"的循环**原样复活**；`spec_task_ceiling` 也挡不住
   （被删的 pending 行不留行，未结存量恒不增长，与 §13.18 同一个盲区）。
   ~~**本文刻意不造机制**~~ **r5 采纳了机制（⑰），本条降级为"残余风险"。**
   r4 的裁决建立在一个假前提上——「唯一便宜的候选是相似度启发式」。拒绝那个
   候选仍然是对的（相似度阈值是启发式，§3.3 与 §3.7 ⑨ 已两次拒绝过这种做法），
   但**由此得出"没有可用机制"不对**：`automation_policy` + `released_by_user`
   （§6.6，与墓碑投影同在切片 3b）是一条**非启发式、与 `key` 无关**的解法，
   而且设计已经在为它付钱。裁决见 §6.1 / §6.6：
   **一条未清除的 `tombstoned_by:"user"` 墓碑使该 wave 对 `declared_by:"spec"`
   的任务按 `declare-and-wait` 处理**，于是"人否决 → spec 可以提议 →
   人的不作为是吸收态"。**残余风险（这些是真的，不是修辞）**：
   - 它**不阻止 spec 反复提议**，只阻止提议自动跑起来。一个失控的 spec 仍然可以
     把该 wave 的文档写满；挡它的是 `spec_task_ceiling`（那条只数**落了行**的，
     §13.18 的盲区在这里仍然存在）与人的注意力。
   - **"人一直不放行"与"人没看见"在机制上不可区分**。吸收态的代价是：如果人
     忘了，这个 wave 的 spec 任务就静静地停住。缓解只有 §12 切片 3c 的诊断渲染
     （必须写明两条清除办法），没有第二层。
   - **一次否决改变整个 wave 的姿态**，并会连带删掉该 wave 内其它未放行的
     spec pending 行（§6.1 已定价、§12 切片 3b 已列为必测）。
     这是本设计里"一处否决、全 wave 收紧"的**唯一**一处。
   **证伪装置（保留）**：可观测量「同一 wave 内，在一次 `tombstoned_by:"user"`
   否决之后 N 分钟内，spec 新增 `task` 声明的速率与条数」——与 §13.18 的
   "每 wave spec 声明速率"同一组指标、同一处上报。它现在测的是**提议**速率
   （落行已被机制挡住），仍然是"spec 是否在原地打转"的直接信号。
23. **判决的执行路径是 r5 才补上、r6 才放对位置的，它未经实测**（NEW，r5 ⑮；
   **r6 ⑲ 重写**，§5.3.3）。这条规则是本设计**唯一**让"判 material 之后不再
   起新工作"变成真的东西，而它**改的是崩溃恢复路径**——那是全项目最难在测试里
   覆盖、又最难在生产里观察的一条路径。四条如实记：
   - **终结是有代价的**：一条从未开始的 dispatched 行落到 `failed`
     （`fail_spawn`，reason 含 `context-stale`），按 §4.2 规则 2b
     **该 `key` 不复活** ⇒ 人要重做必须换一个 key。选它而不是"留着不动"的
     理由是代码自己写的（留着会 "pinning the wave budget forever"，
     `scheduler/mod.rs:789-792`），但它确实把一次**误报**（sweep 判错）变成了
     一次**需要人改 key 的不可逆终结**。**r6 之后这条代价还多了一种形态**：
     worker 已跑完、gate 未开始 ⇒ 同样落 `failed`（§6.5/§13.4）。
     若切片 4 上线后误报率仍高，这条应重新评估（例如改成一个可由人一键重开的
     形态）。
   - **r5 的判据被证伪过一次，这本身是个警示**：r5 用"该 task 的幂等键下有没有
     operation 行"来分"工作是否已开始"，而 `submit` 是**先插行再 drive**
     （`operation/driver.rs:105-123`）⇒ 它恰好在自己要处理的那个崩溃窗口上
     误判。r6 换成"这个 op 是否已越过 `prepare_tx`"之后，判据由 phase 阶梯
     结构性地给出，**不再有一个我们自己写的谓词会错**。但这也意味着
     **正确性现在依赖 `prepare_tx` 确实是所有起活路径的必经点**——那条论证
     （§5.3.3 的三条逐字事实）必须随 operation 层的演进复审：
     **若将来有人给某个 task 绑定的适配器加一条绕过 `prepare_tx` 的路径，
     或新增第五个 task 绑定的 op kind 而忘了加那一行，这条保证就破了，
     且是静默的。** 缓解只有不变量 5 的三条构造性测试。
   - **并发面**：`prepare_tx` 在事务内读 `tasks`，与判 material 的那次
     `UPDATE tasks SET context_stale_at_ms`（同样在事务内，§5.3.3(1)）之间
     由 SQLite 的写序列化定序 ⇒ 结果只有"拒"或"放"，不会读到半态。
     但**"放行之后微秒级才落库的判决"**是真实存在的（下条），需要专门的
     竞态测试而不是推理。
   - **上下文 boot 门是一个新的全局门**。它开得晚，boot 恢复就慢；开得早，
     §5.3.3 的 boot 窗口保护就没有。本文选"boot funnel 里的第一件事"，
     于是正常情况下它在 operation 恢复与 `sweep_boot` 之前就已打开、
     零延迟代价。**r6 更正 r5 的一句话**：门的存在**不是**为了让顺序不再承载
     正确性——顺序在"停机期间发生的编辑"这一种情况上**确实承重**（不变量 5b
     现在就是对顺序的断言）。让顺序不承重的是**另一条**东西：`prepare_tx` 的
     强制点，它在判决已落库时与顺序无关。
24. **`prepare_tx` 提交之后、spawn 之前的那一小段窗口够不着**（NEW，r6 ⑲）。
   op 一旦到 `TxCommitted`，按本设计的定义它"已经开始"（工作区租约 / worker 卡
   等副作用已经落库），恢复漏斗会把它驱动到 spawn 而不再经过强制点。
   要观察到它，必须**恰好在这两步之间崩溃**（正常情况下它们在同一次
   `drive_one` 里相隔微秒），**并且**判决在停机期间才落库。
   **不修的理由**：修它需要在 `spawn_side_effect`（无事务）或 operation 的
   通用机器里再插一个读 `tasks` 的点——前者是新形状，后者是把 task 的关切写进
   与 task 无关的层。**它与 §13.4 / §6.5 "不打断已开始的工作"是同一条纪律的
   延伸**：分界线画在准入点上，而任何分界线都会有一个跨越它的瞬间。
   若将来这个窗口被实际观测到，正确的修法是让 `prepare_tx` 与 spawn 落进
   同一次不可分割的推进，而不是再加一个检查点。
25. **`task_budget` / `require_task_gates` 仍然没有 actor 闸**（NEW，r6）。
   §6.6 末段给 `automation_policy` / `spec_task_ceiling` 加了 user-only 检查，
   但 #644 那两列的既有行为**本设计不动**——`update_wave`
   （`routes/waves.rs:812-887`）对它们接受任何自述 actor。
   于是一个 spec 仍然可以把 `task_budget` 调高来提升自己的并发度。
   **为什么可接受**：那两列限的是**并发**，`spec_task_ceiling` 限的是**存量**、
   `automation_policy` 决定的是**能不能自动跑**——后两者才是本设计的护栏，
   而它们已经被守住。**为什么仍然要记**：这是一条**已知的、超出本设计范围的
   不对称**，改它属于 #644 的面，应单开 issue；在它被改之前，
   §6.6 那句"两个收口对应两条写路径"只对本设计新增的两列成立。

---

## 14. 处置历史

评审方法：每一条发现**先对着 HEAD 源码复核，再决定采纳或驳回**。
驳回的一律附反证。

- **r1**：共 31 条（通道 A 20 条、通道 B 11 条），**接受 27 条、驳回 4 条**
  （其中 2 条是部分驳回：主张成立、所引事实有误）。
- **r2**：共 21 条（通道 A 17 条、通道 B 4 条），**全部接受**——逐条复核后
  没有一条的**事实**站不住。但**驳回了 3 条建议的补救手段**并换成更便宜/更保守的
  方案：①「把变更块 id 塞进 `WaveReportEdited` 或新事件」（给核心事件加 Tier-A
  字段，换一条重解析已经能给出的信息，且对"块被删除"仍然要靠重解析兜底）；
  ②「冻结规范字节本身」（256 KiB × 64 = 16 MiB/claim 进事件 payload）；
  ③「引入持久的块化身 id」（动身份层，换的检测强度与 32 字节哈希相同）。
  r2 有四处**改变设计方向**（⑤–⑧，见文首修订状态）。
- **r3**：共 19 条（通道 A 13 条、通道 B 6 条），**事实全部成立、全部接受**。
  **驳回 2 条建议的补救手段**：①「把 `spec_task_ceiling` 改成真正的累计/epoch 配额」
  （单调计数器不是当前文档的函数 ⇒ rebuild 重建不出，会成为 §2 承重墙上的第三个
  真源；改为如实限定它只约束**未结存量**，并留一条速率可观测量做证伪装置）；
  ②「切片 3a 先不给 legacy 行发空 `TaskContextFrozen`」（那会让 §11.2 不变量 3
  到 3b 才第一次通电，交付一条从未被真实流量执行过的硬不变量）。
  另**驳回通道 A 为墓碑冲突提出的两个候选形状**（规则 2 开豁免 / 改成
  Delete+Insert），采纳通道 B 的第三种（独立的 `tombstoned_by`），理由见 §3.7
  的决策表。**更正 3 处行号**：`DEFAULT_PERMITS` 在 `dispatcher/mod.rs:55` 不是
  `:53`；`wave_delete_tx` 是 `wave.rs:191-234` 不是 `:195-225`；
  `cove.rs:162` 是 `cove_delete_tx`（`:147` 起）内部的 `DELETE FROM tasks` 行。
  r3 有四处**改变设计方向**（⑨–⑫，见文首修订状态）。
- **r4**：共 7 条（通道 A 6 条、通道 B 1 条），**事实全部成立、全部接受**，
  外加通道 A 的一张"内容维穷举表"（已收录进 §5.3.2）与两个通道的 ⑨–⑫ 确认
  （通道 B：⑨ ⑪ ⑫ 成立、⑩ 前半成立；通道 A：⑨ ⑪ 成立、⑩ 后半与 ⑫ 的
  可见性一维不成立——后两条即本轮的 BLOCKER 与 ⑭）。
  **驳回 1 条建议的补救手段**：通道 B 建议"给 `WaveDeleted` / `CoveDeleted` 加
  `affected_task_ids` 字段，或新增一个 Tier-A 事件"——**驳回**，理由不是它做不到，
  而是**采纳 sweep 之后它不再必要**：dst wave 被删 ⇒ 冻结元组解析不到 ⇒ 下一次
  sweep 必判 `material`。用一次 Tier-A wire 变更去换一条 sweep 已经免费给出的
  信息，与 r2 驳回"给 `WaveReportEdited` 加变更块 id"是同一条纪律。
  **顺带删掉了 r3 自己的一条要求**（删除事务内先读后删算受影响集合）——
  它是同一个错误假设（"事务能把集合交给订阅者"）的另一面。
  **另有一条建议部分改写后采纳**：通道 A 给 sweep 的查询是
  `task_ref_index JOIN tasks`，本文改成**直接从 `tasks` 枚举 in-flight 行**——
  前者对"索引行被提前清掉"仍然敏感，而 wave 删除恰好就是那种情况（§5.3.1）。
  r4 有两处**改变设计方向**（⑬⑭，见文首修订状态）。
- **r5**：共 12 条（通道 A 9 条、通道 B 3 条），**事实全部成立、全部接受**。
  **驳回 1 条建议的补救手段**：通道 A 给"人的否决被换 key 绕过"提的备选之一是
  **有界时间窗**（否决之后 N 分钟内翻档）——**驳回**：窗口一到循环即恢复，
  **吸收态没了**，而吸收态正是 §6.1 要求的那条终止性质；且窗口需要一个时间戳，
  它不是当前文档的函数 ⇒ rebuild 重建不出（与 §8(A) 驳回累计配额同一条理由）。
  改用**无窗口、由人显式清除**的形态（删墓碑 / 显式 PATCH 策略列）。
  另有**两处对建议的加强**：(i) 通道 A 建议"命名载体 + 让 `resume_dispatched`
  查它 + 修 boot 顺序（或让 resume 查载体使顺序无关）"——本文**两条都做**，
  并额外规定 b2 用既有的 `fail_spawn` 终结（只留下不动会永久占住 `task_budget`，
  理由是 `scheduler/mod.rs:789-792` 自己写的）；(ii) 通道 A 对 ceiling 建议
  "只数不在声明集合里的行 + 确定性顺序准入"——本文照收，并补上**准入顺序的
  具体定义**（块序 + key 升序）与三条必须成立的性质（幂等 / rebuild≡增量 /
  诊断可渲染），因为"确定性"本身不指定是哪一个确定的顺序。
  r5 有四处**改变设计方向**（⑮–⑱，见文首修订状态）。
- **r6**：共 10 条（通道 A 8 条、通道 B 2 条；**0 BLOCKER**），**事实全部成立、
  全部接受**，外加通道 A 的一张"已清掉部分"审计表（收录为确认，本轮刻意不动
  它覆盖的任何一处）。**驳回 2 条建议的落点/手段**：
  ①「在 operation 恢复漏斗里跳过其 task 行已 stale 的 worker-spawn op」——
  那是第三个豁免口，而且 `apply_recovery_item` 的 `Err` 分支会 `mark_stuck`，
  把"暂时不该跑"表达成终结态是错的；改用**唯一的漏斗** `prepare_tx`。
  ②「加一个持久的 work-started 载体」（通道 B 的备选）——不需要：
  `prepare_tx` 只在 `Phase::Pending` 上跑、且在任何副作用之前，
  "work-started" 因此是**免费的结构性事实**，不是又一个要维护的列。
  另有**两处对建议的加强**：(i) 通道 A/B 都建议把 b1/b2 改到 `op.phase` 上判
  ——本文采纳判据、换掉落点，于是那个谓词**不再需要被写出来**；
  (ii) 通道 A 建议"在飞行即使 key ∈ D 也计入 `occupied`"、通道 B 建议
  "排除集 = 当前声明集合"——**两条各自都不够**（前者不修带诊断的在声明 key，
  后者不修在飞行被排除），本文把它们合并成一条更简单的判据：
  **`pending` 行永远是输出，在飞行永远是输入**。
  r6 有两处**改变设计方向**（⑲⑳，见文首修订状态）。
- **r7（收尾轮）**：共 5 条（通道 A 1 MAJOR + 4 MINOR；**通道 B：APPROVE，
  零发现**），**事实全部成立、全部接受，0 BLOCKER，无设计方向变更**。
  五条都是**同一类**：设计已经在别处做对了，而某一处的措辞/规范/机制载体
  没有跟上——**一处说反了事实**（§6.5 把删块说成 ⑲ 的例外，而它是范式情形）、
  **一处规范分叉**（`unknown_deps` 的入参在 §4.2 与 §11.1 有两个规范）、
  **一处断言没有载体**（5b 断言的 boot funnel 运行期不可调用）、
  **一处措辞过强**（"每个 operation 必经 prepare_tx 且只经一次"）、
  **一处理由已过期**（切片 3a 的 `TASK_COLUMNS` 仍带 r5 的理由）。
  **零驳回**——本轮没有一条建议的补救手段需要被换掉。

**收敛记录（r7 收尾）**

七轮，双通道（A = subagent，B = codex），每一条发现先对 HEAD 源码复核再处置。
**BLOCKER 趋势：10 → 4 → 3 → 1 → 1 → 0 → 0**（r1 → r7）；
发现总数 31 → 21 → 19 → 7 → 12 → 10 → 5；
改变设计方向的修订 ①–⑳ 共 20 处，**最后两轮为 0**。
通道 B 在 r7 给出 **APPROVE / 零发现**，通道 A 的 5 条**无一阻塞**。

**三个维度是这份文档的骨架**，它们不是同时被发现的，而是一轮压出一个：

| 维度 | 问题 | 什么时候被关上 | 用什么关的 |
|---|---|---|---|
| ① **谁能改内容** | 哪些写路径能让一个 in-flight 任务的声明上下文变掉 | r1–r3（五个静默漏报洞逐个补上），r4 收录穷举表 | **写路径是封闭的**（全部汇于 `apply_report_op`）+ 冻结元组带 `content_hash`（§5.3.2）|
| ② **谁保证有人去看** | 改了之后，凭什么保证有代码去比对 | **r4（⑬）** | **fail-closed 全量 sweep**（boot + reconcile tick）——事件路径降级为延迟优化；这一维**没有封闭性，不能靠枚举**（§5.3.1）|
| ③ **什么强制那个判决** | 看过之后判了 `material`，凭什么真的不再起活 | **r5 立载体（⑮）→ r6 换漏斗（⑲）** | 持久列 `tasks.context_stale_at_ms` + **一条规则**：过期判决禁止该 task 上任何 operation *开始*，强制点是四个适配器的 `prepare_tx`（§5.3.3）|

**这轮评审产出的最可复用的东西，是维度 ③ 逼出来的那条自查规则**（r5 立、
r6 加强一格）：

> **凡出现"此后 X 不得再发生"，(1) 必须点名那个判决的**持久值**存在哪一列 /
> 哪条事件；(2) 必须点名**哪一行代码读它**；(3) 还要能证明那个读者站在**所有
> X 的必经之路上**。三条缺一条，写下的就是散文而不是不变量。**

它的三次应用把同一句话从「不得再产生新的 `TaskDispatched`」（恒真且无用）
推到「`resume_dispatched` 会查载体」（有读者，但读者不在漏斗上）
再推到「任何 operation 都过不了 `prepare_tx`」（读者就是漏斗）。
**r7 的 MAJOR 是这条规则的第四次应用**：§6.5 的用户可见文案说的正是
"此后不得再有 gate 执行"的**反面**，而三处规定（§5.1 根节点 / §5.3.1 判据
「块没了」/ §4.2 规则 2(ii)）都已经在文档里写着相反的事实——
**一份文档内部的不一致，和代码里没有读者，是同一种缺陷。**

**r6 的一条元教训**（它是 r5 那条的下一格）：r5 立的规则是"凡出现『此后不得再
X』，必须点名哪一行代码读哪一个持久值"。r6 证明**那还不够**——r5 自己照着这条
规则做了，点名了 `context_stale_at_ms` 与 `resume_dispatched`，**然后仍然漏掉了
三条会做 X 的路径**（operation 开机恢复、`Pending` 窗口、gate 首启）。
失败的原因是：**它点的是当时想得起来的那个调用点，而不是所有 X 的必经之路。**
新规则：

> **凡出现"此后不得再 X"，不但要点名读者，还要能证明那个读者站在所有 X 的
> 必经之路上。证明不出来，说明选的是调用点，不是漏斗——那就继续往下找漏斗。**

这条与 r4 的"先问它属于哪一维"是同一个方向上的两步：r4 说枚举不能承载全称断言，
r5 说断言必须有读者，r6 说**读者必须在漏斗上**。三轮下来，维度 ③ 的强制点从
"一句散文"→"一个列 + 一个调用点"→"一个列 + 一个漏斗"。

**r5 的一条元教训**（与下面 r4 的那条成对）：r4 说"凡是把全称断言挂在
'我列全了触发器'上的地方，都该先问它属于哪一维"。r5 找到的是**第三维**，
而它的失败模式与前两维都不同：**文档写了一句正确的话，而代码里没有任何东西
读它**。「判 material 后不得再产生新的 `TaskDispatched`」在纸面上无懈可击，
在代码里恒真且无用——因为 `TaskDispatched` 是 *claim 的记录*，不是
*开始工作* 的同义词。新的自查规则：**凡出现"此后不得再 X"，必须点名
哪一行代码读哪一个持久值，才不会做 X。** 一条没有读者的不变量不是不变量。

**r4 的一条元教训**：三轮里每一轮都从"触发器枚举"里掉出一个洞（r1 两个、
r2 一个、r3 两个），r4 找到的第六个则根本不在那个枚举的维度上。分界线是：
**维度 ①（谁能改内容）枚举的是写路径**——有限、静态可穷举、且都汇于
`apply_report_op`，所以枚举能承载结论（§5.3.2 的表就是那个结论）；
**维度 ②（改了之后凭什么有人看）枚举的是运行时投递**——它没有封闭性，
只能靠 fail-closed 的重扫，不能靠枚举。凡是把全称断言挂在"我列全了触发器"上的
地方，都该先问它属于哪一维。

**r6 双通道交叉命中的 2 条**（最高置信度，两条都改了设计方向）：
「有 operation 行 ⇒ 工作已开始」这个谓词恰好在它要处理的崩溃窗口上误判（⑲）、
ceiling 的排除集选错因而幂等性与上界**同时**不成立（⑳，两个通道从相反方向
各命中一半，合起来才是完整的缺陷）。

**r3 双通道交叉命中的 3 条**（最高置信度，全部接受，其中两条改了设计方向）：
墓碑默认路径恒 400（`declared_by` 的改写与冻结互斥）、
`PlanUpdated.changed_keys` 的删除语义未定义且不变量 6 以它为断言对象、
切片 3a 在事件面不是零后果。

**r2 双通道交叉命中的 3 条**（最高置信度，全部接受且全部改了设计方向）：
`guard_task_declarations` 的签名无法执行规则 4 的墓碑物化、
`WaveReportEdited` 不携带块 id 因而"从事件算变更块 id"不可实现、
rebuild 与增量在删除/取消路径上仍然分叉。

**r1 双通道交叉命中的 5 条**（两个通道独立发现同一问题，最高置信度）：
`declared_by` 不可重建、rebuild 删行与"状态逐字节不变"矛盾、
墓碑投影写 `status` 破 §2、跨 wave 引用无反向索引与路由、
`claim_context_json` 两个真源都重建不出。**全部接受，且其中两条改变了设计方向。**

### 14.1 通道 A（subagent）

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r1 | A | **BLOCKER** §7.3 fork 的 hint 通道无效：`reassign_ids_with_hints` 对"id 不在旧块中"的 hint 一律忽略（`align.rs:44-47`），fork 目标零旧块 ⇒ 全部 hint 被丢弃；且步骤 1→4 是不动点 | **接受，改设计方向。** 复核 `from_payload`（`wave_report_doc.rs:101-114`）确认它把 `payload.blocks` 当旧块用，既有测试 `from_payload_reuses_hint_block_ids`（`:897`）已证。改为**在块层面重写 + 保留源 block id**，于是不存在 id 映射，整类静默错误消失 | §7.3（重写） |
| r1 | A | **BLOCKER** `tasks_rebuild_tx` 删 `origin='block'` 行会孤儿化活 worker + 幂等键，且与 §6.5 的增量规则分叉 | **接受。** `tasks.id` 是幂等键（`plan.rs:580` → `dispatcher/mod.rs:180`）、状态列与声明列同行，均已复核 | §11.1（断言重写）、§9 第 2 项 |
| r1 | A | **BLOCKER** `declared_by` 只活在投影列里 ⇒ rebuild 重建不出 ⇒ §8 树预算失守 | **接受，改设计方向。** 采纳三选一里的第一项：**声明者进块 payload**——它按 §1 判据本来就是声明 | §4.4（重写）、§3.2、§8 |
| r1 | A | **BLOCKER** 墓碑只在人主动写块时触发；块级 DELETE 不产生墓碑 ⇒ issue 点名的死循环在默认路径上无缓解 | **接受。** 采纳"删除即原位物化墓碑"，并把它放进 §3.7 的收口而非某个端点（否则 `write_markdown` 绕过） | §3.7 规则 4、§3.4、§6.1 |
| r1 | A | **BLOCKER** 闭包深度 1 与"不允许漏报"不相容，且设计自己在鼓励引用链 | **接受。** 选"传递闭包 + 深度/节点双预算 + 耗尽即 fail-closed"，而不是"禁止被引用块再带引用"（后者与"引用而非复述"的指导直接打架） | §5.1（重写） |
| r1 | A | **BLOCKER** 块 id 可回收：删块后 id 释放，新块可铸出同 id 且 `rev=1`，冻结 `(id, rev)` 会静默漏报 | **接受。** 复核 `align.rs:151`（`used` 只由存活 id 预置）、`:167`（新块 rev=1）、`wave_report_doc.rs:198`（"vanished blocks are deleted"）全部属实。改为冻结 `(wave_id, block_id, rev, content_hash)`；canonical 文本本已算出，哈希白拿 | §5.1、§0.2(f) |
| r1 | A | **MAJOR** `resolve_plan_batch` 是整批 `Result<_, String>`，逐块诊断不可能"同一份代码"；rule 2 会中止文档写，违反永不拒绝合并 | **接受。** 复核 `plan.rs:412-484` 属实（四类各自 `return Err`，`:459-463` 即 dispatched 那条）。改为"谓词下沉 + `resolve_plan_batch` 重构为调用它们 + 等价性属性测试"，并如实记下规则 2 是唯一分歧（且方向更严） | §4.2 规则 2、3；切片 1 验收 |
| r1 | A | **MAJOR** rule 1"绝不触碰 status"与 §6.1 调 `task_cancel_tx` 直接矛盾 | **接受。** `task_cancel_tx`（`db/sqlite/task.rs:160`）确实写 `status/updated_at_ms/finished_at_ms`。按建议重述为"声明列 + 恰好两个由声明决定的跃迁"，并写进 §2 的分栏 | §2、§4.2 规则 1 |
| r1 | A | **MAJOR** `claim_context_json` 无人发射，文档与事件日志都重建不出 | **接受。** 新增 kernel-only `Event::TaskContextFrozen`；列与新索引表均降为它的投影；缺失 → fail-closed | §5.3 |
| r1 | A | **MAJOR** 机械检测没有"块 → 冻结它的任务"的反向索引，触发路径未定义 | **接受。** 新增 `task_ref_index` 表 + 检测路径 + 扇出上界 | §5.3 |
| r1 | A | **MAJOR** 切片 1 是半截机制：给 spec 开了写 `pending` 行的路，护栏全在后面 | **接受，重切全部切片。** 不是把某一列前移，而是把"声明能被写下"与"声明能驱动调度"分成两段：切片 1–2 惰性，切片 3 一次带齐护栏 | §12（重写） |
| r1 | A | **MAJOR** 不变量 5 被 §6.1 的可撤回 spec 墓碑证伪 | **接受。** 限定到 `declared_by: "user"` 的墓碑，并明说 spec 墓碑不是防线 | §11.2 不变量 6、§6.1 |
| r1 | A | **MAJOR** `replay.rs:363` 的关联是编造的：`reset_from_fixture` 连 `cards` 一起删，不从事件重放 tasks | **接受。** 逐行读 `replay.rs:325-380` 确认属实（删 `events/…/cards/tasks/…` 后 `seed_events`，全文无重建 `tasks` 者）。删除该说法，换成"两个真源一起清空，与 rebuild 断言正交" | §9 末段 |
| r1 | A | **MAJOR** fork 未钉 `EditAuthor`/`declared_by`，模板里的 `ready: true` 会在 wave 创建事务内派发 | **接受。** fork 强制 `ready: false` + `declared_by: "spec"`（保守侧，且堵住"模板=绕过预算的后门"） | §7.2 |
| r1 | A | **MAJOR（优先级 H）** §6.6 的 `declare-and-wait` 默认把 issue 已撤回的"人逐条放行"装了回来，与 §10 Q6 冲突 | **接受，改设计方向。** 显式裁决 issue 自身的不自洽：**忠于撤回，默认翻转为 `auto-declare`**，并给出四条论证；`declare-and-wait` 降为显式选项。Q5 的理由随之更换 | §6.6（重写）、§10 Q5/Q6 |
| r1 | A | **MINOR** 插入点是 542–565 不是 556–565；`if_rev` 检查在 :525 不是 :527 | **部分接受、部分驳回。** 542–565 **接受**（`:542` 是 `projected_payload.blocks = Some(…)`，`:550` 才是 `let patch = CardPatch {`）。**"`if_rev` 在 :525"驳回**：`:525` 是注释行 "`if_rev` checks happen in here"，实际调用 `let outcome = apply_report_op(&mut doc, &op)?;` **就在 `:527`**，初稿是对的 | §4.1 |
| r1 | A | **MINOR** `parse_destination` / `is_block_id` 是私有的 | **接受。** 复核属实（`report_links.rs:138,152` 无 `pub`，只有 `scan_links` `:63` 是 `pub`）。标注为切片 1 的 NEW 两行 | §0.1 #10、§3.2 规则 5、切片 1 |
| r1 | A | **MINOR** `PlanUpdated` 的订阅者是 dispatcher 不是 scheduler | **接受。** 复核：`dispatcher/mod.rs:137/968/1449`，`scheduler/` 零命中 | §4.3 |
| r1 | A | **MINOR** overlay 先例是 `entity_kind: "view"` 不是 `"wave"` | **接受。** 复核 `routes/waves.rs:662-675` 属实。改用 `"view"`，顺带免掉核实新 entity kind 的路由 | §7.2 |
| r1 | A | **MINOR** 切片 1 漏了 `calm.report.blocks.kinds` 的 schema 面 | **接受。** 它是新 kind 面向 agent 的契约 | 切片 1 |
| r1 | A | **MINOR** migrations 缺 `crates/calm-truth/` 前缀；存在两个 `role_gate.rs`，被引条款在 **calm-server** 那个里 | **部分接受、部分驳回。** 前缀问题**接受**（已补）。**"条款在 calm-server"驳回**：`crates/calm-server/src/role_gate.rs` **全文只有一行** `pub use calm_truth::role_gate::*;`；错误变体与条款 2.6/2.7 全在 `crates/calm-truth/src/role_gate.rs`（`:118,:123,:291-327,:331-360`）。已在 §0.1 #9 写明这条消歧 | §0.1 #9 |

**r2**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r2 | A | **BLOCKER** 墓碑是**吸收**不是终止：`task_cancel_tx` 留下 `status='canceled'`（`task.rs:160-170`），而 `canceled` 是非 pending ⇒ §4.2 规则 2 让该 `key` 永久只出陈旧诊断，"删墓碑后 AI 可合法重提"落空；且这是默认路径 | **接受，改设计方向（⑤）。** 逐字复核 `task_cancel_tx` 与 `plan.rs:451-463` 属实。**不采纳"给 canceled 行加一条复活规则"**——那要在两条路径上各写一条特例；改为**声明面从此不写 `canceled`，只守卫式删除从未派发过的 `pending` 行**（`task_delete_pending_tx`）。一次修订同时关掉吸收、消除 rebuild 分叉、并让 §2 的墙更干净（声明面写的唯一 status 取值就是创建时的 `pending`） | §2（重写）、§4.2 规则 1/2/2b、§6.1、§11.1、§11.2 不变量 9/11 |
| r2 | A | **BLOCKER** 切片 3 把"声明驱动调度"与它的总量护栏分开三片：`automation_policy` / `tree_task_budget` / `MAX_WAVE_TREE_DEPTH` 全在切片 6；§12 只辩护了递归，没辩护单 wave 总量；`task_budget` 是并发容量（`scheduler/mod.rs:164,80`）；3→6 之间无法把 wave 切到 `declare-and-wait`。且"切片 3 不可再拆"未被证成 | **接受，重切（⑧）。** 复核 `compute_ready` 属实（`capacity = (budget - running_cost).max(0)` 后 `take`）。三处修订：(a) 新增 `waves.spec_task_ceiling`（单 wave 内 spec 声明的非终结行总量上限，默认 32）并与投影同片；(b) `waves.automation_policy` 从切片 6 前移到同片；(c) 切片 3 拆成 3a（冻结/索引/检测，**惰性**——无 `origin='block'` 行时闭包恒空、索引恒空、检测恒不命中）+ 3b（投影/rebuild/迁移/护栏），正是本节论证切片 2 用的"护栏先于后果" | §8（新增 (A)/(B) 分层 + 三者分工表）、§12（切片 3a/3b、切片 6、依赖图、自洽表）、§6.6、§11.2 不变量 7b、§13.12 |
| r2 | A | **REGRESSION** §7.2 与 §0.1 #13 仍断言 fork 重铸块 id，与 §7.3 改写后的"保留 id"裁决相反 | **接受。** §7.3 的 `neige://` 重写方案（`#b_x` 那一半原样不动）**依赖**保留；`from_payload_reuses_hint_block_ids`（`wave_report_doc.rs:897-914`）已证。两处逐句改写为"块层面播种、id 保留、rev 从源承接、零 `mint_id`" | §7.2、§0.1 #13 |
| r2 | A | **REGRESSION** §11.1 的"rebuild ≡ 增量"仍不为真：增量留 `canceled` 行、rebuild 直接删，文档自己也承认了这个分叉 | **接受。** 与上面第一条 BLOCKER 是同一个根因，用同一处修订解决：两条路径现在跑同一条 `DELETE … WHERE status='pending'`。另补：`ready` 从 true 撤回也必须走同一条删除，否则 rebuild（只看当前 ready 声明集合）与增量会在这一点再次分叉。加 §11.2 不变量 11（增量序列 vs 末态 rebuild 的逐字节差分测试）作为机制保证 | §11.1、§4.2 规则 1、§11.2 不变量 11 |
| r2 | A | **MAJOR** 第三个静默漏报洞：`reassign_ids` 只返回**存活**切片（`align.rs:152-168`），被删除的被引用块不会出现在任何"变更块 id 集合"里 ⇒ 索引查找落空 | **接受，改设计方向（⑦）。** 复核属实（它 `map` 的是 `new_slices`）。采纳建议的形状：**只按 `dst_wave_id` 查 `task_ref_index`，再对每个受影响任务把整份冻结集逐条重解析**；解析不到 = `material`。删除因此表现为"解析不到"而不是"不在集合里"，结构性免疫。代价有界（`task_ref_index` 只在 in-flight 期间有行，受全局 8 permits 封顶） | §5.3（检测路径重写）、§12 切片 3a 验收 |
| r2 | A | **MAJOR** task 块自身不在它自己的冻结闭包里 ⇒ 改 in-flight 任务的 `goal`/`acceptance`/`gate` 不触发裁决，而这正是 issue 点名的阻塞需求 | **接受。** 复核：r1 的闭包定义确实只含 `refs[]` 与正文链接。改为**task 块自身是闭包的根（深度 0，计入 `MAX_REF_NODES`）**；§4.2 规则 2 因此从"只出诊断"改为"诊断 + 走同一条失效裁决"，两者是同一件事的两半 | §5.1、§4.2 规则 2、§11.2 不变量 3b |
| r2 | A | **MAJOR** `guard_task_declarations(before, after, author) -> Result<(), _>` 是校验器，无法执行规则 4 的原位墓碑物化；且规则 4 在 `Replace` 上不可达（`guard_non_prose_stomp` 先拒删除非 prose 块） | **接受，改设计方向（⑥）**（与通道 B 交叉命中）。逐字复核 `wave_report_guard.rs:58-80` 与 `wave_report.rs:131-215` 属实。拆成 `normalize_report_op`（应用前的 op 改写器）+ `guard_task_declarations`（前后态校验器），新增规则 4′ 覆盖整文档路径的 fail-closed，并**如实写出**：`Replace` 上人删 `task` 块得到的是 400 不是墓碑，UI 必须走块级 DELETE，stomp guard 文案要增补指路 | §3.7（重写）、§3.4、§13.3、§11.2 不变量 8 |
| r2 | A | **MAJOR（NEW）** 跨 cove 泄漏：§5.1 允许任意跨 wave `refs`，裁决会把 cove B 某块的前后内容递给 cove A 的 spec；而 `backlinks_for_wave` 刻意 cove 内 | **接受。** 复核 `report_backlinks.rs:106-130`（先读 `target_wave.cove_id` 再 `wave_report_cards_by_cove`）属实。闭包限制为同 cove + system cove；执行点在 `project_tasks_tx`（纯 `validate_payload` 看不到 cove），越界 → 块不可调度 + 诊断（**不静默丢弃**，否则一条本该被检测的引用会悄悄退出闭包）；claim 时再过滤一次 | §5.1、§11.2 不变量 10、§13.16 |
| r2 | A | **MINOR** fork 经 `from_payload` 播种，绕过 `apply_report_op` ⇒ `validate_body_fences` 与新 guard 都不跑，而 fork 还会改 payload | **接受。** fork 路径自己对每个 fence 跑 `validate_payload`、自己跑校验器（规则 1 之外的部分），豁免**只**限规则 1 且写死在 fork 路径 | §7.2 |
| r2 | A | **MINOR** 规则 1 没为 `EditAuthor::Kernel`/`Plugin` 定义映射 | **接受。** 复核 `event.rs:153-169`：四个变体，`Kernel`/`Plugin` 注释均写 "Reserved; no emitter today"。补 fail-closed：其它任何 author 新建 `task` 块一律拒 | §3.7 规则 1、§11.2 不变量 8 |
| r2 | A | **MINOR** §0.2(f) 的机制不精确：`align.rs:151` 用**全部** `old_blocks` 预置 `used`，回收需要一次*后续*编辑 | **接受。** 逐字复核 `used: HashSet<String> = old_blocks.iter().map(...)` 属实。改写为"下一次对齐时释放"，并指出"删块 → 下一次编辑铸出同 `(id, rev=1)`"是完全可达的两步序列 ⇒ 结论不变 | §0.2(f)、§5.1 |
| r2 | A | **MINOR** 不变量 7 不可断言：一次报告写产生零个 `TaskDispatched`，派发是异步的（`dispatcher/mod.rs:968` `scheduler.poke`） | **接受。** 复核属实。改述为两条稳态上界（并发 / 总量），并把总量那条单列为 7b | §11.2 不变量 7/7b |
| r2 | A | **MINOR** 不变量 3 未说明空闭包时是否仍发射；legacy 行没有块，会让这条"硬"不变量第一天就破 | **接受。** 明写**空冻结集照常发射**（`refs: []`），并区分"空"（没有可失效的上下文）与"缺失"（fail-closed 判 material）。切片 3a 因此从第一天就用真实流量执行这条不变量 | §11.2 不变量 3、§12 切片 3a |
| r2 | A | **MINOR** §6.3 的写时门"校验不过"范围未定义：批级规则（重复 key / 环 / 未知依赖）依赖别的块 ⇒ 并发的人的编辑能让 spec 的写非确定性被拒 | **接受。** 限定写时门为**块局部谓词**（`gate_rule_violations` + schema + 非空），批级违规留给投影诊断 + 不可调度。给出逐谓词的分工表 | §6.3 |
| r2 | A | **MINOR** 闭包遍历在 `task_claim_pending_tx`（调度器写事务）内做，最多 64 个跨 wave 节点，代价未预算 | **接受（"哈希是白拿的"这句本身只针对哈希，部分保留）。** 改为**在 claim 事务之外解析、事务内只写**；并论证中间窗口是安全的——冻结到旧哈希 ⇒ 下次检测必判 `material`，竞态只会更保守。代价记进 §13.14 | §5.1 末段、§13.14 |
| r2 | A | **MINOR** §0.3 仍说失效检测靠块级 rev 单调性，与 §5.1 的 `content_hash` 裁决矛盾 | **接受。** §0 是全文的事实基座，已更正为"权威是 `content_hash`，rev 只是便宜先判 + 诊断信息" | §0.3 |
| r2 | A | **MINOR** 在活的 Spec 工具上把 `if_doc_rev` 设为必填是破坏性契约变更，被当成搭车项 | **接受。** 复核 `wave_report_blocks.rs:348-357`（带 id 才必填）、`:428`（move 可选）属实。写成一次显式的工具契约迁移：descriptor/schema 同步 + 指路的 `-32602` + 可自愈重试 + 专门验收；风险单列 | §12 切片 2、§13.15 |

**r3**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r3 | A | **BLOCKER** §3.7 规则 4 与规则 2 直接冲突：被删的块通常是 `declared_by:"spec"`，改写器原位写 `"user"`，校验器随后拒 ⇒ **人删 spec 声明的任务这条默认路径恒 400**，⑤/⑥ 的整条死循环防线不通电 | **接受，改设计方向（⑨）**（与通道 B 交叉命中，最高置信度）。逐字复核 §3.7 的两条规则与 `apply_report_op`（`wave_report.rs:131-215`）的收口位置属实。**三个候选形状逐条比过**：通道 A 的 (a) 规则 2 开豁免——驳回（把全称不变量降级为条件规则，污染 §4.4/§8 对 `declared_by` 的读法）；(b) 改成 Delete+Insert——驳回（`normalize_report_op` 只返回单个 op；且"新块 vs 已存在块"依赖 `align.rs:352-364` 的启发式 id 铸造，把安全规则挂在启发式上正是 §3.3 拒绝过的做法）；**采纳通道 B 的 (c)**：`declared_by` 原样承接 + 新增不可变的 `tombstoned_by`，防线改判后者。规则 3 补第二个析取项（否则 spec 可直接删掉 `declared_by:"spec"` 的墓碑再重提），新增规则 2b（禁改 `tombstoned_by`、禁原位改回非墓碑）| §3.7（规则 1/2/2b/3/4/4′ + ⑨ 决策表）、§3.2、§6.1、§11.2 不变量 6/8、§10 |
| r3 | A | **BLOCKER** 合成的墓碑 payload `{key, tombstone, declared_by}` 在 §3.2 下**非法**：`kind` 是必填而规则 7 未豁免它；§6.1 的示例连 `declared_by` 都缺 ⇒ `validate_payload` 会拒掉改写器自己的产物 | **接受。** 复核 §3.2 的必填集与规则 7 的豁免集属实。**不采用"把 `kind` 加进豁免列表"这种打补丁式改法**，改为把墓碑定义成**封闭四字段形态**（`key` / `tombstone` / `declared_by` / `tombstoned_by`，其余一律必须缺席，含 `kind`），并规定"非墓碑块上 `tombstoned_by` 必须缺席"——于是墓碑与任务是两个互斥的封闭形状，改写器的产物是唯一的规范形态。§6.1 的示例按此重写 | §3.2 规则 7/8、§6.1、§3.7 规则 4、§11.2 不变量 8(b) |
| r3 | A | **MAJOR（第四个静默漏报路径）** 第 1 级检测被挂在 `event_warrants_spec_push` 之后（`dispatcher/mod.rs:989`，谓词 `:63`，`WaveReportEdited` 分支 `:95-97`），该谓词只放行 `User \| Plugin` ⇒ **spec 自己的编辑被整条丢弃**，而"spec 改被引用的方案块 / 改 in-flight task 自己的 goal"是最常见的变更源 | **接受，改设计方向（⑩）。** 逐字复核：`else` 分支只 `tracing::trace!`。明写**第 1 级对所有 author 无条件运行、是该分支里前置的独立一步；谓词只管第 2 级推送**，并论证这不与谓词存在的理由（防 spec 自激环，`:90-94` 注释）冲突——第 1 级不推送任何东西。切片 3a 加验收 | §0.2(e′)、§5.3 第 1 级、§12 切片 3a、§11.2 不变量 13(c) |
| r3 | A | **MAJOR（第五个洞）** wave/cove 删除不发 `WaveReportEdited`（`wave_delete_tx` `wave.rs:191-234`、`Event::WaveDeleted` `routes/waves.rs:1050`、`cove_delete_tx` `cove.rs:147-`）⇒ "结构性免疫删除"失效；且 `task_ref_index` 在 wave/cove 删除与 `replay.rs:363` 没有清理 | **接受**（行号更正：`wave_delete_tx` 是 `:191-234` 不是 `:195-225`；`cove.rs:162` 是其中的 `DELETE FROM tasks` 行）。新增 `WaveDeleted` / `CoveDeleted` 两条触发路径，并写明**必须在删除事务内先读后删**（`Event::CoveDeleted { id }` 只带 cove id，`event.rs:419-420`，订阅者事后反查不出来）；索引清理进 §5.3 的生产者清单与 §9 末段的 `replay.rs` 表清单。**（r4 已撤销"先读后删"这一半：它没有载体，且 sweep 使它不再必要——见 r4 通道 B 的 MAJOR。触发路径与索引清理保留。）** | §0.2(f′)、§5.3、§9 末段、§12 切片 3a/3b、§11.2 不变量 13(a)(b) |
| r3 | A | **MAJOR** §6.6 的 `declare-and-wait` 不可实现：块的 `ready` 本来就是 `true`（"人改成 true"是空操作），判据 `declared_by` 又被规则 2 冻住；且 `automation_policy` / `spec_task_ceiling` 两列**没有任何写入面** | **接受，改设计方向（⑪）。** 复核属实。新增 `released_by_user`（人可写、spec 不可写，§3.7 规则 5），可调度谓词多一个合取项，撤回放行走同一条守卫式删除；两列的写面**照抄 #644 `WavePatch.task_budget` 的形状**（`wave.rs:168-187` 的定向单列 UPDATE + 两列不上 `Wave` 结构体 + `routes/waves.rs:864-887` 的校验/判空），并特别点出 `patch_has_other_changes` 的判空列表必须同步，否则只改策略的 patch 会被短路 | §6.6（重写）、§3.2、§3.7 规则 5、§12 切片 3b、§13.19 |
| r3 | A | **MAJOR** §4.2 规则 4 对**已经是 `pending` 的行**不成立（不 insert/不 update ≠ 删除）⇒ 诊断出现后该行仍被 `compute_ready` 交出去；且 §11.1(1) 的谓词对"ready:true 但被诊断"未定义 ⇒ rebuild ≡ 增量再次破 | **接受，改设计方向（⑫）。** 复核 `compute_ready`（`scheduler/mod.rs:164-191`）只看 `status='pending'`，与诊断无关，属实。二选一里**取"诊断态也删"**：把"声明消失 / 墓碑 / `ready` 撤回 / 诊断非空 / 放行位撤回"统一成**同一个可调度谓词**，增量与 rebuild 共用它。如实记下代价：一次瞬时编辑失误会删掉一条从未派发的 pending 行（保守侧，且该行不含执行史）| §4.2 规则 1/4/5、§11.1(1)、§11.2 不变量 11、§12 切片 3b 验收 |
| r3 | A | **MAJOR** §5.3 的"代价有界"不成立：`DEFAULT_PERMITS = 8` 是**并发 spawn 上限**（`dispatcher/mod.rs:55`，注释 + `:475`），不是生命周期持有量；真实上界是 Σ 各 wave `task_budget`，wave 数无界 | **接受**（行号更正：通道 A 写 `:53`，实际 `:55`；文档原文引的 `:55` 是对的）。改写为真实上界，并**加一个显式扇出上限 `MAX_RERESOLVE_FANOUT = 64`**（超出直接判 `material`，与 `MAX_ADJUDICATION_FANOUT`、闭包预算耗尽同一条 fail-closed 纪律，因而不引入漏报）；§13.14 整条重写 | §5.3「代价的真实上界」、§13.14、§12 切片 3a |
| r3 | A | **MAJOR** §9 的迁移物化工具撞 §3.7 规则 1（`User` ⇒ 必须写 `"user"`、`Kernel` ⇒ 一律拒），fork 有写死的豁免而它没有；且它的 `ready:true` 与 fork 的强制 `ready:false` 取向相反 | **接受。** 给它**同类的写死豁免**（第二个也是最后一个规则 1 豁免点，只在 admin CLI 路径，两点同处枚举 + 同批测试）。**`ready` 的方向分歧给出理由而不是对齐**：fork 面对的是"没有任何东西是这次被决定要做的"，物化工具面对的是**已存在的行**——写 `ready:false` 会让 §4.2 规则 1 当场删掉活的 `pending` 行，一个"让存量看得见"的工具会顺手取消存量任务 | §9 第 4 项、§7.2、§12 切片 7 验收 |
| r3 | A | **MAJOR** `PlanUpdated.changed_keys` 对删除的语义未定义，而不变量 6 正以它为断言对象：删除若进 `changed_keys`，物化墓碑那一次编辑本身就会发一条含该 key 的 `PlanUpdated`，不变量在边界上自破 | **接受**（与通道 B 交叉命中）。定义 `changed_keys` = 插入 ∪ 声明列更新 ∪ 删除（含 `ready` 撤回），排序去重；只出诊断而未写行的 key 不进。不变量 6 限定为"墓碑生效**之后**（不含物化那一次编辑）"。**采纳通道 A 复核的结论**：dispatcher 侧无风险，`Event::PlanUpdated { wave_id, .. } => scheduler.poke(wave_id)`（`dispatcher/mod.rs:968`）与 key 无关 ⇒ 把删除算进去只可能多一次 poke，漏掉才会丢 poke | §4.3、§11.2 不变量 6 |
| r3 | A | **MINOR** 新增列的连带面未列出：`tasks` 的两列要同步 `TASK_COLUMNS`（`task.rs:19`，被 `:33`/`:131`/`read.rs` 共用）；`waves` 的两列应沿用 #644"不上结构体 + 定向 UPDATE + SQL 直读"的形状（`wave.rs:168-187` 的注释） | **接受。** 逐字复核 `TASK_COLUMNS` 与 `wave.rs:168-187` 的注释属实（后者明写这样做是为了不动 `SELECT` 列表 / `WaveUpdated` wire payload / ts-rs 导出）。两条相反的取向各自写进切片 3b 的 migration 项，并点明 `sqlx::query_as` 的漏改是**运行期**失败 | §12 切片 3b |
| r3 | A | **MINOR** 守卫式删除的 0 行分支未定义（与 `task_claim_pending_tx` 竞争，`task.rs:222` ← `scheduler/mod.rs:641`） | **接受。** 明写 0 行 ⇒ 不是错误、不重试、不回滚，按 §6.5 处理（不改状态 + "正在执行，无法立即撤回"的诊断），与 `calm.plan.cancel` 对 0 行的消歧同构（`task_get_tx` 复读，`task.rs:130-131`）| §4.2 规则 1、§6.5 |
| r3 | A | **MINOR** §6.5"墓碑在任务终结后生效"没有可观测后果（终结后行是 `done/failed`，§11.1 不删、§4.2 规则 2b 不复活） | **接受。** 改写为"墓碑此后只作为记录存在**并挡住同 key 的重新声明**；该 key 本来也不会复活"，并明说不存在"终结那一刻发生一次状态变化"这回事 | §6.5 |
| r3 | A | **MINOR** 切片 3a 的"零后果"只在调度语义上成立，事件面会动 goldens/zod/invalidationPolicies 与既有 E2E 事件序列断言 | **接受**（与通道 B 交叉命中）。惰性证明本身保留（通道 A 复核后确认成立），但把结论从"零后果/惰性"改述为**"行为保持（behavior-preserving）：调度决策逐位不变，事件流多一条"**，并把四类连带更新写进本片的完成定义 | §12 切片 3a、§12 自洽表 |

**r4**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r4 | A | **BLOCKER（第六个静默漏报路径，且不在前五个所在的那一维上）** 第 1 级是**边沿触发在一条自承 lossy 的进程内广播**上，既无持久水位也无兜底重扫：`scheduler/mod.rs:24-27` 逐字写着 "The bus is lossy…backstopped by `Scheduler::sweep_all`"；`dispatcher/mod.rs:784-806` 对 `RecvError::Lagged(n)` 只 warn + `sweep_all`（那是**调度活性**兜底，对冻结集一无所知）；`:777-782` 每条 envelope 是 fire-and-forget `tokio::spawn`；唯一的跨重启补投 `replay_harness_events_since`（`harness/mod.rs:203-228`）按 spec card 展开且只查该 wave 自己的事件 ⇒ 对跨 wave 引用结构性失明；`WaveDeleted`/`CoveDeleted` 的丢失**按设计不可恢复**。§11.2 不变量 4 在一次 `Lagged` 或一次重启后当场为假；§13 的 20 条风险里没有一条涉及事件丢失/重启 | **接受，改设计方向（⑬）。** 逐条对 HEAD 复核，五个事实全部逐字成立。采纳建议的 **fail-closed 全量 sweep**：boot（`sweep_boot` 之后）+ `Lagged` 分支 + 既有 reconcile tick（`NEIGE_SCHEDULER_RECONCILE_SECS`，默认 300，**不新增环境旋钮**），重解析全部 in-flight 冻结元组，无法验证即 `material`；**事件路径降级为延迟优化**。**对建议的查询做了一处加强**：枚举源从 `task_ref_index JOIN tasks` 改为**直接扫 `tasks` 的 in-flight 行**——前者对"索引被提前清掉"仍敏感，而 wave 删除正好是那种情况。加 `MAX_SWEEP_NODES = 4096` 硬顶（用满即把本轮剩余判 `material`，fail-closed 因而不引入漏报）。**连带**：前五个洞与通道 B 本轮的 MAJOR 全部退化为优化；r3 的"删除事务内先读后删"整条删除；不变量 4 重述为"sweep 完成之后"，新增 4b 与三条构造性测试 | §0.2(g)、§5.3（前言）、**§5.3.1（新增）**、§11.2 不变量 4/4b/13、§12 切片 3a、§13.20b/21 |
| r4 | A | **MAJOR** 否决权是 **`key` 作用域**的，而 `key` 由 spec 自选 ⇒ 换 key 即完全绕过；这一层**只剩** prompt，而 §6.1 自己判过"prompt 不是机制" | **接受事实，如实记而不造机制。** 逐条核实：墓碑按 `key` 落块、§3.7 规则 3 按 `tombstoned_by` 保护那一块、§4.2 规则 2b 按 `(wave_id, key)` 判定、§11.2 不变量 6 按 `key` 断言——换 key 后没有一条命中；`spec_task_ceiling` 也挡不住（删掉的 pending 行不留行 ⇒ 未结存量恒不增长）。**驳回"墓碑携带 goal 指纹 + 投影对高相似度新 key 出诊断"这条补救**：逐字节相等之外只能靠**相似度阈值**，把安全判定挂在启发式上正是 §3.3 与 §3.7 ⑨ 决策表两次拒绝过的做法；一条会漏绝大多数情况、读起来却像机制的诊断比一条诚实的风险条目更危险。改为：§6.1 把机制面**限定**为同 key、明说 prompt 不是机制，风险与**可观测量**（一次 user 否决之后 N 分钟内该 wave 的 spec 新增声明速率/条数）按 §13.18 的同一形状记进 §13.22 | §6.1、§13.22 |
| r4 | A | **MAJOR** "「可调度」是一个当前文档的纯函数"在**它自己那句话里**就是假的（谓词含 `automation_policy`，那是 `waves` 的列），且四类实际决定可调度性的诊断（跨 cove、`unknown_deps`、`spec_task_ceiling`、`declare-and-wait` 放行）造不出于 `project_task_declarations(blocks)` 这个明写"不读 DB"的签名 ⇒ **行被删掉而原因渲染不出来**，静默降级 | **接受，改设计方向（⑭）。** 四条逐一复核成立（`unknown_deps` 的签名自带 `&[Task]`；跨 cove 的执行点 §5.1 自己写在 `project_tasks_tx`；`spec_task_ceiling` 是 `count(*)`；`declare-and-wait` 读 `waves` 列）。采纳建议：读端改成**读事务内的派生调用**，并把可调度谓词收敛成**唯一实现** `evaluate_schedulability_tx`，增量/rebuild/读端三条路径共用；§11.1(1) 的措辞改为"当前文档 + wave 策略列 + 同 wave 既有行的函数"。切片 3b 的验收加"删行 + 原因可见"的成对断言 | §4.2（签名块、规则 3′、规则 7）、§11.1(1)、§12 切片 1/3b |
| r4 | A | **MINOR** §3.7 规则 3 的主语是 `author == Spec` ⇒ `Kernel`/`Plugin` 删除或改写 `tombstoned_by:"user"` 的墓碑**行为未定义**；规则 1 对同样这两个 author 在**新建**方向已 fail closed 并写明理由 | **接受。** 复核 `wave_report.rs:330-331` 逐字确认 `EditAuthor::Kernel` "has no caller today and is reserved for future server-internal rewrites"，Plugin 通道随 #978 撤回（`532eed6c`）⇒ 潜伏而非现网缺口。规则 3 主语改为 **`author != User`**，与规则 1 同一形状 | §3.7 规则 3 |
| r4 | A | **MINOR** 行号漂移：`patch_has_other_changes` 是 `routes/waves.rs:880-886`（文中 `:884-890`），定向单列 UPDATE 是 `db/sqlite/wave.rs:168-187`（文中 `:167-186`） | **接受，并按 HEAD（`02ef95d5`）重校了全文 248 处 `file:line`。** 另更正 11 处此前未被指出的漂移：`report_links.rs:62`→`:63`（`scan_links`）、`routes/waves.rs:1211`→`:1207`（`update_wave_report`）、`dispatcher/mod.rs:988`→`:989`（分支体首行；`:988` 是注释）与 `:983-992`→`:983-998`、`event.rs:755-775`→`:764-781`（`PlanUpdated` 的 doc comment + 变体；`:755` 落在 `TaskFailed` 里）、`wave_report.rs:576-584`→`:580-585`（事件 push）与 `:566-580/578`→`:566-579`、`wave_report_doc.rs:473`→`:474`（`saturating_add`）、`gc.rs:65-99/65-70`→`:71-93`、`gc.rs:128-144`→`:129-145`、`routes/waves.rs:860-895`→`:864-887` | 全文 |
| r4 | A | **确认（非缺陷）** `move_block` 不构成漏报，但文中未言明 | **接受建议，补一行。** `move` 不动 rev（`wave_report_blocks.rs:12-19` "rev untouched"）也不动内容，而位置式引用已被 §5.1 硬禁并由 `is_block_id`（`report_links.rs:152`）强制 ⇒ 该路径在本设计里不存在 | §5.1 |
| r4 | A | **证据（非缺陷）** "谁能改内容"这一维的穷举表（10 条通道，全部关闭） | **接受并收录进正文。** 逐条对 HEAD 复核后收进 §5.3.2，并在其上加了"两个维度"的框架：维度 ① 枚举的是**写路径**（有限、静态可穷举、汇于 `apply_report_op`）所以枚举能承载结论；维度 ② 枚举的是**运行时投递**，没有封闭性，只能靠 fail-closed 重扫。**CRDT merge 那一行按原文保留为 UNVERIFIED-for-future**（今天唯一的 `.merge(` 在单测 `wave_report_doc.rs:1194`；将来开 sync 必须重审） | §5.3.2 |

**r5**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r5 | A | **MAJOR（BLOCKER-adjacent，第三个维度）** 「有人看过之后凭什么执行那个判决」是空的：(a)「这个任务被判过 material」**没有持久载体**（没有列、没有派生查询）；(b) `TaskDispatched` 只在 claim 事务里发射而被 claim 的行永不回 `pending` ⇒ **不变量 5 对 sweep 覆盖的行是空洞的**；(c) 真正会拿着过期闭包重新起工作的是 `sweep_reconcile` 的 `Dispatched` 分支 → `resume_dispatched` → `drive_spawn`（`scheduler/mod.rs:1067`→`:1397`→`:761`），**重新 spawn 且不发 `TaskDispatched`**；且 boot 顺序在不变量 4b(b) 测试的那个确切场景上就是反的（`sweep_boot` `:1015` 的第一件事就是 `sweep_reconcile` `:1016`）| **接受，改设计方向（⑮）。** 逐条对 HEAD 复核，四个事实逐字成立（`grep TaskDispatched scheduler/mod.rs` 只有 `:39/:596` 注释 + `:692` 唯一发射点；`task.rs` 无任何写回 `'pending'` 的 SQL）。裁决**三件事**：(i) 持久载体 `tasks.context_stale_at_ms`（`TaskContextAdvanced{material}` 的投影列，§2 的墙不动）；(ii) 强制点放在 **`resume_dispatched` 函数体内**（上下文 boot 门 + 载体），**采纳建议里"让 resume 查载体使顺序无关"的那一支**，同时也修 boot 顺序（顺序只影响恢复延迟）；(iii) **建议未覆盖的一点由本文补上**：判 material 的 `dispatched` 行若"留着不动"会永久占住 `task_budget`（`compute_ready` 把 `dispatched` 计入 `running_cost`）⇒ 分 b1（已有 operation ⇒ 只对账，§6.5）/ b2（无 operation ⇒ 既有 `fail_spawn`，理由是 `scheduler/mod.rs:789-792` 自己写的 "pinning the wave budget forever"）。不变量 4/5 重述 + 新增 4c/5b | §0.2(h)、**§5.3.3（新增）**、§5.3/§5.3.1/§5.3.2、§11.2 不变量 4/4c/5/5b、§12 切片 3a、§13.23 |
| r5 | A | **MAJOR** `claim_context_json` 是 `TEXT NULL` 新列且切片 3a 同片既加列又上 sweep ⇒ 部署那一刻每条在飞行都是"缺失"而非"空" ⇒ 首次开机 sweep 把**升级期间每一个在飞任务**判 `material`；叠加发现 1 即为跨升级的必然 Stuck-ops 事件 | **接受，改设计方向（⑱）。** 复核：§9 的五项里确实没有 `claim_context_json`，而 §5.3.1 的"缺失 → material"与 §11.2 不变量 3 的"空 ≠ 缺失"合在一起使这条**必然发生**。修法即建议的那一行：同一个 migration 文件里 `UPDATE tasks SET claim_context_json='[]' WHERE claim_context_json IS NULL AND status IN (…)`——这些行按构造就是 legacy，没有闭包。`context_stale_at_ms` 不需要 backfill（`NULL` 的语义对存量行正确）。切片 3a 验收加一条**升级路径专测** | §9 第 5 项、§12 切片 3a |
| r5 | A | **MAJOR** ceiling 谓词不幂等：共用实现只保证"同一个函数"，而 `spec_task_ceiling` 的 `count(*)` 把**正在被重新求值的行**也数进去 ⇒ ceiling=2 时 {k1,k2} 落地后一次 rebuild 会把三者全判超限 → 规则 1 删掉 k1/k2 → 下次编辑又落地（rebuild≢增量 + 抖动）| **接受，改设计方向（⑯）**（与通道 B 交叉命中）。**采纳建议的形状并补全它**：`occupied` 只数 `key ∉ 当前声明集合` 的非终结行；`capacity = max(ceiling − occupied, 0)`；按**块序 + key 升序**（建议只说"确定性顺序"，本文指定是哪一个）在 capacity 内准入。写明三条必须成立的性质（幂等 / rebuild≡增量 / 诊断删行后仍可渲染），并给切片 3b 加两条验收 | §4.2 规则 3″（新增）、§4.2 规则 3′ 表、§8(A)、§11.1(1)、§12 切片 3b |
| r5 | A | **MAJOR** §13.22 的 r4 前提「唯一便宜的候选是启发式」是假的：`waves.automation_policy` + `released_by_user`（§6.6，切片 3b）是非启发式、与 key 无关的现成解法；拒绝指纹方案是对的，由此得出"没有机制"不对 | **接受，改设计方向（⑰）。** 复核成立：两样东西都已在本设计的账上，且都落在切片 3b。裁决：**未清除的 `tombstoned_by:"user"` 墓碑 ⇒ 该 wave 对 spec 声明的任务按 `declare-and-wait` 处理**，形式化为派生的 `effective_policy(wave)`（策略列改 `TEXT NULL`，显式设置压过派生值）——**零新列、零新字段**，且仍是"当前文档 + 策略列"的函数（rebuild 不破）。**驳回建议里的"有界窗口"变体**：窗口一到吸收态就没了，且时间戳不是文档的函数。UX 后果逐条定价：粒度（一处否决收紧全 wave）、已有 pending 行会被删、可发现性（必须有专门诊断文案）、人怎么清除（删墓碑 / 显式 PATCH，两条语义不同且都需要）| §6.1（重写该条）、§6.6、§11.2 不变量 6b、§12 切片 3b/3c、§13.22 |
| r5 | A | **MAJOR** 切片 3a 自述的核心验收要求"记录 material 判定"，而 `Event::TaskContextAdvanced` 在切片 4 才引入 ⇒ 3a 交付不了它自己声明的验收 | **接受**（与通道 B 交叉命中）。二选一里取"把事件移进 3a"：sweep 是 3a 的**正确性载体**，交付一个判决无处可记、因而无处可执行的正确性载体与 §12 的存在理由直接冲突。连带：3a 的 Tier-A 面从一个事件变成两个（同族、同批产物），"本轮无 Tier-A 连带面"那句话作废（它本来也不成立——`TaskContextFrozen` 就是 Tier-A）；切片 4 只把 `verdict` 从"恒 material"变成"由裁决决定" | §11.2 事件表注、§12 切片 3a/4、§5.3.1 |
| r5 | A | **MINOR** sweep 自身的失败不可观测：四个可观测量全是每轮指标，**恰在 sweep 停摆时一起缺席**；且 reconcile tick 是 `tokio::spawn` 里的裸 `loop`（`dispatcher/mod.rs:814-829`），一次 panic 静默终结该进程余生的所有 sweep | **接受。** 逐字复核两处（`scheduler/mod.rs:1050-1055` 的 warn-and-skip、`dispatcher/mod.rs:814-829` 的裸 `loop`）。加**正向健康信号**：`context_sweep_last_success_age_seconds`（对 DB 反复失败与 panic 两种停摆同时有效）+ 连续失败计数，告警阈值 `3 ×` reconcile 周期；并在 §13.21 如实记下"反复的 DB 失败会**静默**降级这条保证" | §5.3.1、§12 切片 3a、§13.21 |
| r5 | A | **MINOR** sweep 是**电平触发**在一个按构造持续存在的条件上（判 material 不推进冻结点）⇒ 每轮重新检出、重发**不可裁剪**的 `TaskContextAdvanced`（`events_prune.rs:95-101` 白名单）、从切片 4 起每轮重调 LLM | **接受。** 复核：冻结点只在判 immaterial 时推进（§5.3 末段），`events_prune` 确为白名单式。守卫**与 ⑮ 的载体合一**：sweep 的枚举查询加 `AND context_stale_at_ms IS NULL` ⇒ 每个 `(task, 冻结点)` 至多一条 material 事件。新增不变量 4c（判 material 后连跑 3 轮，事件计数恒为 1）| §5.3.1、§11.2 不变量 4c、§12 切片 3a/4 |
| r5 | A | **MINOR** 不变量 12 用全称措辞，而 §13.20 刻意容忍漏清理（"漏一个点 = 代价 bug"、"没有兜底的周期性清扫"）——两者不能同时为真 | **接受，取"加机制、保全称"**（采纳 §13.20 自己提的建议）：sweep 末尾一条 `DELETE FROM task_ref_index WHERE task_id NOT IN (SELECT id FROM tasks WHERE status IN (…))`——一条 SQL、无扇出。不变量 12 保留全称形式并增加断言点"一轮 sweep 之后"；§13.20 的"没有周期性清扫"随之作废 | §5.3.1、§11.2 不变量 12、§13.20 |
| r5 | A | **MINOR** 切片 3b 的实际规模高于 ~1100 行；"护栏必须与后果同片"覆盖不到**前端**——状态回显 + 诊断渲染不是护栏，API 层的 `taskDiagnostics` 才是 | **接受。** 前端拆出为**切片 3c**（~350 行，纯前端、回退不改服务端行为），`taskDiagnostics` 字段留在 3b。如实记下拆开后的窗口：3b→3c 之间诊断只能从 API 读到——那是**可见性延迟**，不是静默降级（原因始终可取）。依赖顺序其余部分复核后不变 | §12 切片 3b/3c、依赖图、自洽表 |
| r5 | A | **已核实、不构成发现** `NEIGE_SCHEDULER_RECONCILE_SECS` 存在且默认 300（`scheduler/mod.rs:83`、`:430-434`、`dispatcher/mod.rs:386`）；sweep 从 `tasks` 而非索引枚举使索引清理两个方向都不承载正确性；§5.3.2 维度①表格抽查全部准确；`MAX_SWEEP_NODES` 的 fail-closed 行为已定义且与 300s 一并被标为猜测值 | **接受为确认，不改动。** 已复核 `DEFAULT_RECONCILE_SECS: u64 = 300` 在 `scheduler/mod.rs:83`（本文此前引 `:432` 是 `reconcile_secs_from_env`，两处都对，已在 §5.3.1 补全 `:83`）| §5.3.1（补一处行号）|

**r6**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r6 | A | **MAJOR** **operation 的开机恢复是第二条独立的 spawn 入口，且从不读该载体。** `plan_recovery_for`（`operation/driver.rs:1010-1024`）把 `Pending \| TxCommitted \| AppServerInteract \| SpawnStarted \| SpawnSucceeded` 一律映射为 `Recover`，`apply_recovery_item`（`:1043-1055`）调 `drive_one`——**它真的会 spawn**。而 §5.3.1 把上下文 sweep 排在 operation 恢复**之后** ⇒ 在不变量 5 自己的标准构造上过期 worker 先被拉起来；**即使 sweep 先跑也一样**，因为那条路径上没有任何东西读 `context_stale_at_ms` | **接受，改设计方向（⑲）。** 逐条对 HEAD 复核，全部逐字成立。**不采纳"在恢复漏斗里跳过 stale 的 worker-spawn op"这一支**（那是第三个豁免口，且 `apply_recovery_item` 的 `Err` 分支会 `mark_stuck`，把一个"暂时不该跑"表达成终结态是错的）；**采纳"适配器在自己的 drive 内 fail closed"那一支并把它一般化**：强制点落在 `prepare_tx`——每个 op 恰好经过它一次、在事务内、在任何副作用之前，`submit` 与开机恢复**都**通向它（`driver.rs:388-393`）。**顺带**：boot 顺序再前移一格（上下文 sweep 排在 operation 恢复之前），并把不变量 5b 改成**对 boot 顺序本身的断言**——顺序在"停机期间发生的编辑"这一种情况上确实承重，写清楚比假装无关更安全 | **§5.3.3（2）重写**、§5.3.1 boot 顺序、§5.3.2 维度表、§11.2 不变量 5/5b、§12 切片 3a、§13.23/24 |
| r6 | A | **MAJOR** **b1/b2 的谓词恰好在它要处理的崩溃窗口上误判。** `OperationRuntime::submit`（`operation/driver.rs:105-123`）**先** `insert_operation` **再** `drive()`；op 以 `Phase::Pending` 创建，phase 阶梯（`0042_operations_parked.sql:13-24`）在任何 spawn 之前还有 `pending`/`tx_committed` ⇒ 崩溃留下的无副作用 op 使 **b1 命中** ⇒ `drive_spawn` → submit 去重 → `drive()` → **worker 在过期闭包上被首次启动** | **接受（与通道 B 交叉命中），并入 ⑲。** 通道给的修法是"改到 `op.phase` 上判"（一行谓词）；本文**采纳它的判据、但不采纳它的落点**——把判据放进 `prepare_tx` 之后，"工作是否已开始"由 phase 阶梯**结构性地**回答（`prepare_tx` 只在 `Phase::Pending` 上跑），**不再有一个我们自己写的谓词会错**，而且同一句话顺带覆盖了发现 1 与发现 3。§13.23 记下这次误判本身作为警示 | §5.3.3（b1/b2 整表删除）、§11.2 不变量 5（新增 `Pending` 窗口的回归构造）、§13.23 |
| r6 | A | **MAJOR（空洞类）** **gate 路径会在判决之后起新活，且那里没有任何代码读该载体。** `sweep_reconcile` 的 `Verifying` 分支（`scheduler/mod.rs:1107-1112`）spawn `drive_gate`；`drive_gate_inner`（`:1541-1581`）分支 2 **提交**一个 gate op，它用该行已冻结、已过期的 `gate_json` 构造并运行真实 shell 命令（`task_verify_adapter.rs:660-665`）。这是 b2 的情形逐字重现，却被给了相反的待遇；§5.3/§13.4 的既有豁免论证的是一个**已经在跑**的 gate | **接受，并入 ⑲。** 复核成立。两个建议里取**两条都做**：`task-verify` 与三个 worker kind 一样落进那条唯一的规则（它的 `prepare_tx` `:627` 今天就在做 `task.status != Verifying` 的同类检查，加的是紧邻的第二个合取项）；**且**不变量 5 被改述为"任何 operation 都不得**开始**"，并把 §6.5「产出照常过 gate」**明确收窄并定价**——判 material 之后不再有新的 gate 执行，worker 产出保留但未被验证。下游收敛走**既有的** pre-bump 失败臂（`scheduler/mod.rs:1679-1699`），零新分支、零新 gate 原因枚举值 | §5.3 末段、§5.3.3、§6.5（新增定价段）、§11.2 不变量 5(b) 构造、§12 切片 3a/3c、§13.4 |
| r6 | A | **MINOR** ⑯ 的准入规则推不出不变量 7b 的全称上界：仍被声明的**在飞** key 落在排除集 `D` 里 ⇒ 不计入 `occupied`，把新块插到在飞任务块**之上**即可多拿容量（`ceiling = 1` 的两行构造）。超额被 `task_budget` 封顶，是上界/措辞缺陷而非失控，但 7b 是切片 3b 的验收测试，照 r5 的写法会红 | **接受，改设计方向（⑳）**（与通道 B 从相反方向交叉命中同一处）。**采纳"在飞行即使 key ∈ D 也计入 `occupied`"**，并把它与通道 B 的那一半合并成**一条更简单的判据**：**`pending` 行永远是输出，在飞行永远是输入**（证明：本次求值后存活的非终结行只有"在飞行"与"本次准入"两类，其余 pending 行必被规则 1/4 删掉，没有第三种）。7b 按建议改述，但改成**能被证明的那条**（含 ceiling 被调低时的退化形态），而不是 `≤ ceiling + task_budget` | §4.2 规则 3″ 重写、§8(A)、§11.2 不变量 7b、§12 切片 3b 验收 (i-a) |
| r6 | A | **MINOR** 承载不变量 6 的那条诊断（同 `key` 存在未清除墓碑）**不在 §4.2 规则 3′ 的诊断唯一枚举里**。§12 切片 3b 的交付清单有它，所以不空洞，但谓词自己的枚举漏了墓碑防线所依赖的那一项 | **接受。** 按建议加在 `dup_keys` 旁边，作为**整文档纯谓词**（墓碑块与任务块在同一份块快照里，不需要 DB），并写明它与不变量 6 / §6.1 的承重关系 | §4.2 规则 3′ 表 + 新增一段 |
| r6 | A | **MINOR** 能推翻整条否决的那一列没有 actor 闸：`update_wave`（`routes/waves.rs:812-887`）只对 `lifecycle` 做闸（`:849`），而 `X-Calm-Actor` 是自述的（`actor.rs:28-33` 逐字 "Not authenticated … not a security boundary"）⇒ §3.7 规则 5 花整整一条规则让 `released_by_user` 对 spec 不可写，而 `PATCH automation_policy='auto-declare'` 一次调用即可清掉整个 wave 的否决 | **接受。** 复核成立。按建议加一条**镜像 `validate_transition`** 的 user-only 检查，**并扩到 `spec_task_ceiling`**（它是 §8(A) 的存量护栏，被约束者能自己调等于没有护栏）；落在切片 3b（两列本来就是本设计新增的，因此不是行为变更）。§6.6 明写"两个收口对应两条写路径"。**`task_budget` / `require_task_gates` 不动**——那是 #644 的既定面，改它超出本设计范围，如实记进 §13.25 | §6.6 末段（新增）、§11.2 不变量 8(f)、§12 切片 3b、§13.25 |
| r6 | A | **MINOR** 切片 1 开了一个 `declared_by` 伪造窗口：本片使该 kind 可写而 `validate_payload` 不强制 `declared_by`（那是切片 2 的规则 1），切片 2 的规则 2 随后把伪造值**永久冻住**，而 §6.2/§8 正是信任它来豁免预算 | **接受。** 两个候选里取**"切片 1 拒 `declared_by:\"user\"`"**而不是"切片 2 一次性对账"：对账要回答"谁真的是人写的"，而那正是窗口期**没有载体**去回答的问题；拒绝则把窗口里每一条都固定在保守的一侧。放宽与规则 1 必须**同一个 PR**（先放宽即重开窗口，先强制不放宽即人写不了任务）。代价如实记（窗口期人写的块被永久归因为 spec），并写明窗口期写 `task` 块**得不到任何东西**——没有投影。**同时采纳该通道对"切片 1 今天开工是安全的"的独立复核** | §3.2 规则 8、§12 切片 1/2 |
| r6 | A | **MINOR** §3.1 的安全边界引用讲的是 **card** kind（`plugin_host/manifest.rs:491-499` 是 `CardKindRegistry::builtins().claims_kind`），不是 block kind | **接受，按建议换掉引用。** 结论不变而理由更强：报告块 kind 是封闭常量 `DATA_KINDS`，**根本没有 plugin 注册面**，且 `validate_payload` 对未知 kind 直接报错。**顺带更正一处本轮自查发现的行号**：`DATA_KINDS` 在 `kinds.rs:45` 不是 `:44`（该通道与本文此前都写的 `:44`）| §3.1 重写、§12 切片 1 |
| r6 | A | **确认（非缺陷）** 一张"已清掉部分"的审计表：ready 门 / 规则 1 删除的载体与读者、`declared_by`/`tombstoned_by`/`released_by_user` 的唯一变更点（`apply_report_op` 恰好被调用一次，`wave_report.rs:527`；`ReportDoc::update` 测试外恰好一次，`:171`）、`origin='legacy'` 保护的结构性写法、墓碑/ceiling/declare-and-wait 三者的可重建性、⑱ backfill 的正确性与充分性 | **接受为确认，不改动。** 这张表是"目标 1（持久值 + 点名读者）在本文其余部分确实非空洞"的证据，**本轮刻意不去动它所覆盖的任何一处**。树预算/深度上限（§8(B)）按该通道所记保持 UNVERIFIED（尚无代码，落在切片 6，强制点已点名）| —（不改动）|

**r7（收尾轮，通道 A）**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r7 | A | **MAJOR** **§6.5 的 ⑲ 连带段陈述了一个假事实，而它驱动用户可见文案。** 该段把"删块 / 立墓碑"与"上下文失效"当成两条不相交的路径（「删块那条路径不写 `context_stale_at_ms`，所以 gate 照常」）。**它们是同一条路**：§5.1 规定 task 块自身是它自己冻结闭包的**根**（深度 0）、§5.3.1 的 sweep 判据逐字含「**块没了**」、§4.2 规则 2(ii) 已经明说这次编辑「必然被第 1 级机械检测捕获」⇒ 删掉一个 in-flight 任务的块 **必然**判 `material` ⇒ **必然**写 `context_stale_at_ms` ⇒ 按 ⑲，尚未开始的 gate 被拒、行落 `failed('gate-infra')` + `context-stale`。原文案「worker 会跑完，其结果照常过 gate 并汇报」至多在一个窄竞态窗口里为真 | **接受。** 三处引用逐条对文复核，全部逐字成立。**重写该段**：删块**不是** ⑲ 的例外，而是它的**范式情形**；把 §6.5 的诊断字符串（切片 3c 的交付物）改写为与 §5.3 末段第 3 条、§5.3.3(2) 的 phase 表、§11.2 不变量 5(b) **逐字一致**的三句话（已开始的 operation 跑完 / 不会再有新的 gate 执行 / 未开始的 gate 被拒且行落 `failed` 带 `context-stale`）。并写明这四处必须一致，本节此前是唯一说反的一处、也是唯一驱动用户可见文案的一处 | **§6.5 重写**（含新的诊断文案）；§13.4/§13.23 的措辞已一致，不动 |
| r7 | A | **MINOR** **`unknown_deps` 在本文有两个规范。** §11.1(1) 已按 r6 ⑳ 把可调度谓词的第三个输入收窄为「同 wave 的**在飞**行」，但 §4.2 的契约签名注释与规则 3′ 的表仍写着裸 `&[Task]`。用"全部非终结行"会让 ⑳ 的失败类在另一个诊断上原样重现：声明 `k1`/`k2(depends_on k1)` 皆 pending，一次编辑删掉 `k1` 的块 ⇒ 求值 E1 仍看得见 `k1` 的 pending 行 ⇒ `k2` 无诊断、存活，而同一事务把 `k1` 的行删掉 ⇒ 同一份文档上的 E2 给 `k2` 判 `unknown_deps` 并删它 ⇒ **rebuild ≢ 增量**，且读端 `taskDiagnostics` 与写路径刚做的决定互相矛盾 | **接受。** 复核成立（§11.1 的收窄早已在文，缺的是 §4.2 这一侧）。**新增规则 3‴** 作为规范条款（表行 + 签名注释同步改），并**如实定价它的正确后果**：`depends_on` 指向一个**未被声明但有 pending 行**的 key 从此得到 `unknown_deps` 诊断——两类（刚被删/撤回的块：诊断说的是真话；**`origin='legacy'` 的存量 pending 行**：保守侧，物化后自然消失），后者**专列进切片 3b 的迁移验收**，免得上线当天出现一批无人预期的诊断 | **§4.2 新增规则 3‴**、规则 3′ 表行、契约签名注释、§11.1(1) 交叉引用、§12 切片 3b |
| r7 | A | **MINOR** **不变量 5b 按现在的写法不可断言。** 它断言的 boot funnel 是 `main()` 的直线代码（`main.rs:64/73/79`；pub 定义在 `lib.rs:203/221`），**运行期不可调用**——一个自己依次调这些 pub 函数的测试，**顺序是它自己选的**，因此什么都没断言。而 5b 是"停机期间的编辑"这个残余窗口的**唯一**机制保证 | **接受。** 复核成立，且项目里**已有**为这件事准备的机制：`crates/calm-server/src/lib.rs:611-705` 的 `mod boot_order_tests` —— `include_str!("main.rs")` + `str::find` 偏移比较（范例 `boot_order_scheduler_sweep_after_operation_recovery` `:678-688`，逐字断言 `recover < sweep`）。裁决：**5b 的 CI 载体 = (a) 往这条既有链上加一格 `boot_order_context_sweep_before_operation_recovery` + (b) 既有的 seam 行为测试**，两半缺一不可（(a) 挡"调用被挪位"，(b) 挡"顺序没变但门没生效"），并落进切片 3a 的完成定义——不留给切片期即兴发挥 | **§11.2 不变量 5b（补 CI 机制）**、§12 切片 3a 验收 (ii-a)/(ii-b) |
| r7 | A | **MINOR** **§5.3.3 事实 2「每个 operation 必经 `prepare_tx`，且只经一次」不是字面真话。** `prepare_tx_and_advance`（`operation/repo_sqlite.rs:277-330`）在事务内先跑 `adapter.prepare_tx`，再把 phase UPDATE 守卫在 `lease_owner` 上；`rows_affected() == 0` 时**回滚并返回 `Ok(None)`**（`:321-323`），op 留在 `Pending` ⇒ 另一个 driver 会**再跑一次** `prepare_tx` | **接受事实，承重方向不受影响。** 复核逐字成立。**但整条规则压的方向没有被削弱**：那次调用是**只读检查**、**fail-closed**、且**必在任何副作用之前**（副作用随同一事务一起回滚）；phase 只前进不后退 ⇒ 越过 `TxCommitted` 之后不再经过。**改述为「至少一次，且必在任何副作用之前；phase 只前进不后退 ⇒ 越过 `TxCommitted` 后不再经过」**——这是整条保证所依赖的那一段，措辞精度值这几个字 | §5.3.3(2) 事实 2 |
| r7 | A | **MINOR** **§12 切片 3a 仍带着 r5 的过期理由**：「`TASK_COLUMNS`/`Task` 的 `FromRow` **必须**同步 `context_stale_at_ms`，因为 `resume_dispatched` 读 `tasks_nonterminal()`」。**r6 已把那道检查整条删掉**（`resume_dispatched` 只剩上下文 boot 门），强制点换成 `refuse_if_context_stale` 的定向 SQL 读、不经过 `TASK_COLUMNS`；§5.3.3(1) 也已记下"新形状顺带削掉了一个运行期失败面"。两处不一致 | **接受。** **降级为"建议"**并把理由换成读端的（3b/3c 的 `taskDiagnostics` 与可观测量要它），与 §5.3.3(1) 对齐；**同时保留那条纪律本身**——`declared_by`/`origin` 在切片 3b **必须**进 `TASK_COLUMNS` 与 `FromRow`，因为投影**确实**读 `Task`（r3 通道 A 的原始指出） | §12 切片 3a；§14.3 的 r5 自查行加注 |

### 14.2 通道 B（codex）

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r1 | B | **BLOCKER** `declared_by` 号称可从文档重建，实则文档不存块作者 | **接受**（与通道 A 同一条，交叉命中）。采纳"把不可变的声明来源放进文档" | §4.4、§3.2 |
| r1 | B | **BLOCKER** rebuild 删 `origin='block'` 行 ⇒ 同行的状态列必然一起没，与"逐字节不变"矛盾 | **接受**（交叉命中）。采纳"按生命周期定义 rebuild"：只删 `pending` 且声明已消失的行 | §11.1、§9 |
| r1 | B | **BLOCKER** `saturating_add(1)`（`align.rs:156-167`、`wave_report_doc.rs:466-481`）⇒ 在 `u32::MAX` 处内容变而 rev 不变 | **接受。** 复核行号精确为 `align.rs:163` 与 `wave_report_doc.rs:474`，事实成立。**不采纳"checked 溢出即写失败"**（在报告写路径上引入一类不可恢复的硬失败，代价不对称），改用与通道 A 同一个修复：冻结集加 `content_hash`，一并覆盖 id 回收与溢出两个反例 | §5.1、§0.2(f) |
| r1 | B | **BLOCKER** `WaveReportEdited` 只推送被编辑 wave 的 harness；跨 wave 引用无反向索引/路由；且无栅栏阻止 gate 在裁决前落定 | **前半接受、后半驳回。** 反向索引 + 扇出路由**接受**（复核 `dispatcher/mod.rs:983-998,1308-1312,95-97` 属实）。**"加原子状态栅栏"驳回**：它要求在任务状态机里新增一个能让 gate runner 阻塞的状态，等价于"中断正在跑的东西"——§0.1 #14 已证该机制不存在，§6.5 已明确不造。改为如实承认时序缺口并删掉 §11.2 里那句无机制支撑的"早于 `TaskGateResult`" | §5.3（含末段）、§11.2 不变量 4、§13.2c |
| r1 | B | **MAJOR** 冻结集只写在 `claim_context_json`，不在任何事件里 | **接受**（交叉命中）。新增 `TaskContextFrozen` | §5.3 |
| r1 | B | **MAJOR** 块级 rev 守不住 create 与位置性操作；应分 `if_doc_rev` / `if_block_rev` | **接受。** 复核 `wave_report_blocks.rs:348-357`（有 id 才必填）、`:428`（move 可选）属实。按建议分成两种 rev，并把 MCP 侧的同一个洞一并补上 | §3.4、§5.4 |
| r1 | B | **MAJOR** 墓碑投影调 `task_cancel_tx` 写 `status`，与 §4.2 硬规则矛盾 | **接受**（交叉命中）。**不采纳"拆成内核自有的取消命令/事件"**——那会让人的否决不再有即时效果，且多一条写路径；改为把这两个跃迁写进 §2 的墙里并说明它们各自带前置条件 | §2、§4.2 规则 1 |
| r1 | B | **MAJOR** 诊断没有存储/读取契约；写进报告会让内核成为文档写者 | **接受。** 采纳"显式定义为读时投影"：`taskDiagnostics` 只读字段，零存储零缓存零事件 | §4.2 规则 7 |
| r1 | B | **MAJOR** `origin='legacy'` 行 + `UNIQUE(wave_id,key)`：同 key 的块插不进去且无冲突规则 | **接受。** 复核唯一键在 `0041_tasks.sql:27` / `0058_tasks_kind_claude.sql:33`。定义"收编而非插入"，顺带让物化工具不再需要特殊有序原子路径 | §9 第 3/4 项 |
| r1 | B | **MAJOR** 切片 1 并非真正可独立合入 | **接受**（与通道 A 同一条）。全部切片重切 | §12 |
| r1 | B | **MINOR** `PlanUpdated` 的 in-tx 闸不是 Spec-only，放行 User/Kernel/KernelDispatcher/Plugin | **接受。** 复核 `crates/calm-truth/src/role_gate.rs:257-278` 属实。改述为"worker-AI 排除 + MCP 侧 Spec 软闸"，并指出这对本设计是有利事实（人的写口发 `PlanUpdated` 无需改闸） | §0.1 #1、§4.3 |

**r2**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r2 | B | **BLOCKER（REGRESSION）** `guard_task_declarations(before, after, …) -> Result` 拿到的是不可变快照，而 `apply_report_op` 的每个分支在调用它之前已经改完了 `doc`（`wave_report.rs:131-215,523-542`）⇒ 无法把删除改写成墓碑；且整文档删除路径未被规定 | **接受**（与通道 A 交叉命中）。采纳建议的形状：**应用前的变更式归一化 + 应用后的前后态校验**，并逐路径写清 `Replace` / `WriteMarkdown` / 块级 op 各自会发生什么 | §3.7（重写）、§11.2 不变量 8 |
| r2 | B | **BLOCKER** `WaveReportEdited` 只持久化扁平 markdown，不含块 id/rev（`wave_report.rs:566-579`）；fence 不带 id（`wave_report_doc.rs:98-112`、`align.rs:38-47`）⇒ 在事件体上重跑对齐恢复不出权威 id，任务会被漏掉 | **接受**（与通道 A 交叉命中）。逐字复核事件字段属实。**驳回建议 ①"把变更/删除的块 id 放进 `WaveReportEdited` 或新事件"**：那是给一个所有报告写都会发的核心事件加 Tier-A 字段，换来的信息重解析已经能给出，且对"块被删除"这一类**仍然**要靠重解析兜底。采纳建议 ②（同通道 A）：按 `dst_wave_id` 拉全部冻结引用再逐条重解析 | §5.3（检测路径重写） |
| r2 | B | **MAJOR（REGRESSION）** `tasks_rebuild_tx` 与增量投影不等价：增量走 `task_cancel_tx` 改 `status/updated_at_ms/finished_at_ms`（`task.rs:157-170`），rebuild 直接删行；文档承认差异却仍断言等价 | **接受**（与通道 A 交叉命中）。采纳建议的"为两条路径选一条统一的生命周期规则"，选的是**都删**——理由三条（从未派发的行不含执行史；"决定不做"的记录在墓碑块里；这是唯一能让等号成立的选法）| §2、§4.2 规则 1、§11.1 |
| r2 | B | **MAJOR** `content_hash` 被表述成绝对的不可能性证明，而定长哈希不是单射 | **接受。** 改述为**抗碰撞检测**，并写明"不允许漏报"的精确形式带一个 SHA-256 碰撞例外；同时说清它与 r1 那两个**构造性、日常发生**的漏报不在一个量级。**驳回两条备选**：「冻结规范字节」（`MAX_CANONICAL_BYTES = 256 KiB` × `MAX_REF_NODES = 64` = 16 MiB/claim 进事件 payload，`kinds.rs:42`）与「引入持久块化身 id」（动身份层换取同等检测强度）| §5.1、§13.17 |

**r3**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r3 | B | **BLOCKER** 人删除 spec 声明的任务无法成功：归一化改写 `declared_by:"user"`，校验器随后拒该字段变更（`985-doc-as-plan.md:560,562`）；所有块操作汇于 `apply_report_op`（`wave_report.rs:131`）| **接受，改设计方向（⑨）**（与通道 A 交叉命中）。**直接采纳本通道建议的形状**：`declared_by` 与墓碑权属分离，新增不可变的 `tombstoned_by:"user"`，防线（§6.1 终局性 / §11.2 不变量 6 / §3.7 规则 3）一律改判后者。它比通道 A 的两个候选都好：规则 2 保持**无例外**的全称形式，且不把安全判定挂在对齐器的 id 铸造上。并按建议**同时规定并测试"人删 user 声明的任务"与"人删 spec 声明的任务"两种**（§11.2 不变量 8(a)）| §3.7、§3.2、§6.1、§11.2 |
| r3 | B | **MAJOR** `spec_task_ceiling` 被称为总量上限，但只数非终结行（`:1436`），而任务会沉淀成持久的 `done`/`failed` 行（`task.rs:413,595`），scheduler 随容量释放继续消费（`scheduler/mod.rs:164`）⇒ 挡不住"每完成一条再声明一条" | **接受事实、驳回其中一个补救手段。** 复核属实：三条终结跃迁都写 `finished_at_ms` 且从不删行。**采纳"明确限定保护范围、不再称它为总量"**：改述为**未结存量（outstanding backlog）上限**，并明写它挡不住细水长流式失控。**驳回"定义真正的累计/epoch 配额"**：累计配额需要一个单调计数器，而它**不是当前文档的函数** ⇒ `tasks_rebuild_tx` 重建不出（§11.1），会成为 §2 承重墙上的第三个真源；为一个尚未观测到的失控形状动那堵墙代价不对称。替代是"每 wave 的 spec 声明速率"可观测量 + 将来的纯运行时速率闸 | §8(A)、§11.2 不变量 7b、§13.18、§10 |
| r3 | B | **MAJOR** 切片 3a 不是"可证惰性/零后果"：每一次 legacy claim 都多一条持久事件与投影写，扩大了失败面与可观测事件流（`:1797,:1810`；今天 claim 事务在守卫翻转后发 `TaskDispatched`，`scheduler/mod.rs:641,689`）| **接受事实、驳回备选。** 采纳**"改名为 behavior-preserving 并证明兼容性"**：调度决策逐位不变的证明保留，事件面的四类连带更新（goldens min/full、zod、invalidationPolicies、既有 E2E 事件序列断言）写进完成定义。**驳回"先不给 legacy 行发空事件、等有 `origin='block'` 行再发"**：那会让 §11.2 不变量 3 到 3b 才第一次通电，本片将交付一条**从未被真实流量执行过**的硬不变量——而让它从第一天就跑在全部既有调度流量上正是本片最有价值的部分 | §12 切片 3a、§12 自洽表 |
| r3 | B | **MAJOR** `task_ref_index` 的终结清理跨多条独立跃迁（`task.rs:160,405,541,586`）+ reaper/超时/wave-cove 删除/replay，漏一处就留下陈旧索引行，破坏代价上界并让已完成任务被反复重解析 | **接受。** 逐条复核四个跃迁函数属实（另补 `task.rs:493` 的 `→done/verifying` 与 NEW 的 `task_delete_pending_tx`）。**两条都做**：(i) 收敛成**一个清理原语** `task_ref_index_clear_tx` + 表级的 `_by_wave_tx`，并给出九条生产者的完整清单；(ii) **正确性不依赖清单穷尽**——索引读端一律 JOIN `tasks` 过滤 in-flight，于是漏一个点是代价 bug 而非正确性 bug。按建议新增不变量"无终结/不存在的 task 拥有索引行" | §5.3、§11.2 不变量 12、§13.20、§9 末段 |
| r3 | B | **MAJOR** `changed_keys` 的删除语义未定义：抑制它会隐藏真实的 plan 变更并跳过 poke；包含它又与现有 wire 契约（`event.rs:764` 只写 created/updated/canceled）冲突 | **接受**（与通道 A 交叉命中）。按建议定义为**插入 ∪ 声明列更新 ∪ 删除（含 `ready` 撤回）的排序去重并集**，并把 **Tier-A 文档更新列为切片 3b 的必做项**：`Event::PlanUpdated` 的 doc comment（`crates/calm-types/src/event.rs:764-781`）今天既只写 created/updated/canceled，也仍写着"Spec-only"——后者按 §0.1 #1 的更正一并改述为"worker-AI 排除"。加删除/空操作测试 | §4.3、§12 切片 3b |
| r3 | B | **MINOR** 硬删除在运行期是兼容的（守卫式删除只在 `pending` 时获胜，`task.rs:222`），但审计契约未写明：`tasks` 行不再留下"曾存在一条从未派发的声明"，而既有取消保留时间戳/状态（`task.rs:157`）| **接受。** 明写审计契约：**`WaveReportEdited`（带 `body_before/after`）+ 同事务的 `PlanUpdated{deleted key}` + 文档里的墓碑块**是durable 记录，并复核 `events_prune` 是**白名单**式（可裁剪 kind 只有 `claude.hook`/`codex.hook`/`harness.phase.changed`/`harness.item.added`/`overlay.set`，`crates/calm-truth/src/events_prune.rs:96-100`）⇒ 这两条事件不可被裁剪。同时**明确不再声称** `tasks` 表本身保留撤销历史 | §6.1 末段 |

**r4**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r4 | B | **MAJOR** 删除事务算出受影响 `task_id` 并"随 `WaveDeleted`/`CoveDeleted` 落地"，但全文从未改这两个事件的 payload，§11.2 也仍把它们标为"旧" ⇒ **dispatcher 没有任何从删除事务到它的新分支的载体**（`crates/calm-types/src/event.rs:419-425`、`routes/waves.rs:1046-1054`、`routes/coves.rs:386-387`）；cove 删除尤甚，事后事件只剩 cove id | **接受事实、驳回补救手段。** 逐字复核：`CoveDeleted { id: CoveId }` 在 `event.rs:420`、`WaveDeleted { id, cove_id }` 在 `:425`，**都不带任何 task 集合**——通道 B 与通道 A 在这一点上交叉命中。**驳回"加 `affected_task_ids` 字段或新增一个 Tier-A 事件"**：理由不是做不到，而是**采纳 sweep（⑬）之后它不再必要**——dst wave 被删 ⇒ 冻结元组解析不到 ⇒ 下一次 sweep 必判 `material`；用一次 Tier-A wire 变更（goldens min/full、zod、`invalidationPolicies`、ts-rs、event-version）换一条 sweep 免费给出的信息，与 r2 驳回"给 `WaveReportEdited` 加变更块 id"是同一条纪律。**同时删掉 r3 自己那条"删除事务内先读后删"**——它是同一个错误假设的另一面。删除路径因此简化为：只清索引、事件不动、dispatcher 分支只触发一次 sweep 降低延迟 | §5.3（删除触发源整段重写）、§11.2 事件表注、§12 切片 3a、§13.20b |
| r4 | B | **确认** ⑨ 成立（封闭四字段墓碑在 §3.2 下合法，`kind` 显式缺席；§3.7 保 `declared_by`、冻 `tombstoned_by`、禁 spec 删改人的墓碑、禁原位复活；未找到逃逸）；⑪ 成立（#644 形状逐条对上：`model.rs:147-173` 的 double-option、`db/sqlite/wave.rs:168-186` 的定向单列写、`routes/waves.rs:864-887` 的校验与空 patch 短路；两个新字段必须进 `patch_has_other_changes`）；⑫ 成立（统一谓词覆盖消失/墓碑/`ready != true`/诊断/放行撤回，且 `compute_ready` 只看 `status == pending`、不看诊断，`scheduler/mod.rs:164-190`）；⑩ **部分成立**——无条件前置于 `event_warrants_spec_push` 是对的（`dispatcher/mod.rs:95-97` 过滤、`:983-998` 只 trace），但删除触发缺一个载体 | **接受。** ⑨⑪⑫ 与通道 A 的确认一致，无需改动（行号已按 HEAD 重校：`model.rs:147-174`、`wave.rs:168-187`）。⑩ 的缺口即上一行，已由 sweep 关闭：**"删除必须能被检出"从此不依赖任何载体** | §3.2/§3.7/§6.6/§4.2（不动）、§5.3.1 |

**r5**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r5 | B | **BLOCKER** key 作用域的墓碑满足不了 §6.1 自己声明的目标：spec 换一个 key 就能把被否决的工作原样复活，而文档明写循环"原样复活"；§6.1 要求的是**机制**，不是可观测量（`985-doc-as-plan.md:1533,1592`）| **接受，改设计方向（⑰）**（与通道 A 交叉命中，两个通道对同一处给出同一判断）。采纳通道 A 指出的那条现成机制：**未清除的 `tombstoned_by:"user"` 墓碑 ⇒ 该 wave 的 spec 声明按 `declare-and-wait`**。通道 B 要求的是"与 spec 自选 `key` 无关的可强制否决身份"——本裁决给出的正是它：判据是 wave 级的、spec 写不了也删不掉（§3.7 规则 1/2b/3），**对任何 key 都成立**。**不采纳"缩窄 issue 的交付目标"那一支**：目标本身是对的，缺的是机制而不是野心 | §6.1、§6.6、§11.2 不变量 6b、§13.22 |
| r5 | B | **MAJOR** 共用 `evaluate_schedulability_tx` 并不保证"投影之后每条诊断仍可渲染"：`spec_task_ceiling` 用 `count(*)` 数存量非终结行，而超限声明不落行、既有 pending 行可能被删 ⇒ 随后的读事务看到的是**不同的 DB 状态**，同一个函数仍可能丢掉那条诊断（`:768,:1991,:2604`；行读直接来自 `tasks`，`task.rs:28`）| **接受**（与通道 A 从幂等性角度交叉命中，同一个缺陷）。采纳建议的方向——**对一个稳定的"前瞻视图"求值，并在写/rebuild/读三条路径上用同一套输入构造**——落地形态即 §4.2 规则 3″：输入 =（当前块快照的声明集合，**不在该集合里**的非终结行），两者都不含"本次求值正在创建/删除的那些行"，于是读事务与写事务看到同一份输入。并按建议补上**删行之后 ceiling 诊断仍可读**的专门测试 | §4.2 规则 3″、§8(A)、§12 切片 3b 验收 |
| r5 | B | **MAJOR** 切片 3a 的 sweep 会发 `TaskContextAdvanced`，而该事件在切片 4 才引入；3a 却声称交付 sweep 的正确性不变量与"不再派发"行为（`:1444,:2481,:2521,:2617`）| **接受**（与通道 A 交叉命中）。采纳建议的第一支：**把 `TaskContextAdvanced` 及其 kernel-only/事件 schema 全流程移进切片 3a**；建议的第二支（"定义一个 3a 专属的持久 material 否决机制并弱化 3a 之前的不变量"）在本轮以**更强的形式**被采纳——那个持久机制就是 ⑮ 的 `tasks.context_stale_at_ms`，但它**不是 3a 专属的临时物**，而是全设计的常设载体，因此不变量不需要被弱化 | §12 切片 3a/4、§11.2 事件表注、§5.3.3 |

**r6**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r6 | B | **MAJOR** "operation 行存在"证明不了"工作已开始"：插行先于驱动（`operation/repo_sqlite.rs:100`、`operation/driver.rs:121-122`），而 `drive_spawn` 会**重新驱动**既有 operation（`scheduler/mod.rs:754-760`）⇒ 在插入 `pending` op 之后、执行之前崩溃，b1 会调 `drive_spawn` 并**起动过期的工作** | **接受，并入 ⑲**（与通道 A 交叉命中，两个通道独立发现同一条）。建议是"按一个能证明 spawn 副作用已开始的 phase 分岔，或加一个持久的 work-started 载体"。**采纳前者、驳回后者**：不需要新载体——`prepare_tx` 本身就是那个 phase 边界（`driver.rs:388-393`：只在 `Phase::Pending` 上跑，在任何副作用之前，在事务内），把强制点放到它里面之后，"work-started" 是**免费**的结构性事实而不是又一个要维护的列 | §5.3.3（2）重写、§11.2 不变量 5、§13.23 |
| r6 | B | **MAJOR** ⑯ 的幂等性仍然不成立：`occupied` 只排除 `key ∈ D`，而 `D` 被限制为"其余诊断均为空"的声明 ⇒ 一条**当前被声明但带诊断**的 pending 行会先被数进去、可能拒掉一条合法声明，随后又被删掉，改变下一次求值 | **接受，改设计方向（⑳）**（与通道 A 从相反方向交叉命中同一处）。建议是"排除集 = 当前声明集合，与诊断无关；ceiling 候选单独算，按块序后 key 升序准入"。**采纳它的方向，并换成一条更强也更简单的判据**：与其定义"排除哪些声明"，不如按"这一行是不是本次求值的产物"分——**`pending` 行永远是输出（本次事务里必被删或被重新准入），在飞行永远是输入**。它同时消灭通道 A 那个反向反例（仍被声明的在飞行此前被排除出计数 ⇒ 上界被破），而"排除集 = 当前声明集合"单独并不能 | §4.2 规则 3″ 重写、§8(A)、§11.2 不变量 7b、§12 切片 3b 验收 (i-b) |

**r7（收尾轮）**

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r7 | B | **APPROVE — 零发现。** 通道 B 对 r6 之后的全文（含 ⑲⑳ 两处方向变更、§5.3.3 重写的强制点、§4.2 规则 3″ 的准入判据、§11.2 不变量 5/5b/7b 的重述、§12 各片的完成定义）逐条复核后**未提出任何 BLOCKER / MAJOR / MINOR**，判定设计可作为实施计划 | **记录为通道 B 的收敛点。** 本轮通道 A 的 5 条**无一阻塞**、**无一改变设计方向**，且全部是"文档内部一致性/机制载体"类，不触碰任何已被通道 B 复核过的裁决——因此 r7 的修订**不使通道 B 的 APPROVE 失效**。**双通道在 r7 收敛，评审到此结束** | —（无改动）|

### 14.3 本轮修订自查中新发现的事实（非评审提出）

| 轮次 | 通道 | 发现 | 处置 | 落在哪一节 |
|---|---|---|---|---|
| r1 | 自查 | 初稿 §0.2(a)「人根本写不了非 prose 块」**说过头了**：`guard_non_prose_stomp` 只遍历**已存在**的非 prose 块，对"新增一个 fence"零约束 | 更正为"能新建，不能改、不能删" | §0.2(a) |
| r5 | 自查 | **`fail_spawn` 已经是一条为"永久性原因"准备好的终结路径**：`drive_spawn` 对幂等键永久冲突走的就是它，注释逐字写着留着 `dispatched` 会 "retry the same error every sweep while **pinning the wave budget forever**"（`scheduler/mod.rs:789-792`，函数在 `:891`）| 直接复用它作为 §5.3.3 b2 的终结手段，而不是新造一条"停住的行"状态。**顺带确认了通道 A 未提的一点**：`compute_ready` 把 `dispatched` 计入 `running_cost`（`scheduler/mod.rs:164-191`）⇒ "留着不动"会永久占住该 wave 的 `task_budget`（默认 1）= 整个 wave 死锁 | §5.3.3、§13.23 |
| r5 | 自查 | **`resume_dispatched` 读的是 `tasks_nonterminal()`（`read.rs:247`）返回的 `Task`，而它走 `TASK_COLUMNS`（`task.rs:19`）** | 于是 `context_stale_at_ms` 必须同时进 `TASK_COLUMNS` 与 `Task` 的 `FromRow`，否则强制点拿不到那一列——**`sqlx::query_as` 的运行期失败面**，与 r3 通道 A 对 `declared_by`/`origin` 指出的是同一条纪律。**（r6/r7 后记：这条已随 ⑲ 失效——`resume_dispatched` 的载体检查被整条删除，强制点换成不经过 `TASK_COLUMNS` 的定向 SQL 读 ⇒ `context_stale_at_ms` 进 `TASK_COLUMNS` **降级为建议**（读端诊断/可观测量要它），r7 通道 A MINOR。**纪律本身对 `declared_by`/`origin` 仍然成立**，那两列在切片 3b 必须进——投影确实读 `Task`。）** | §5.3.3、§12 切片 3a |
| r6 | 自查 | **`prepare_tx` 是一个已经存在的、强制性的、事务内的准入点**：`drive_one` 的 `Phase::Pending` 分支（`operation/driver.rs:388-393`）唯一的动作就是 `prepare_tx_and_advance`，phase 只前进不后退 ⇒ 越过 `TxCommitted` 之后不再经过（**r7 更正：不是"恰好一次"而是"至少一次"**——`prepare_tx_and_advance` 的 phase UPDATE 守卫在 `lease_owner` 上，0 行时回滚并把 op 留在 `Pending`，`operation/repo_sqlite.rs:321-323`；承重方向不变，§5.3.3 事实 2）；签名带 `&mut Tx`（`operation/mod.rs:585-590`）；`task_verify_adapter::prepare_tx`（`:627`）**今天就在**同一处读 task 行并对不合适的状态返回 `Conflict`（`:651-658`）| 这是 ⑲ 能成为"一条规则"而不是"三个豁免口"的全部原因：不需要新机制、不需要新载体、不需要新状态，只需要在四个 `prepare_tx` 的最前面各加一行 | §5.3.3 |
| r6 | 自查 | **拒绝之后的收敛路径也已经存在**：`CalmError::Conflict` 是 `client_failure_parts`（`operation/driver.rs:1180-1191`）认的永久性客户端失败 ⇒ op 在 `Pending` 处 `mark_failed`；worker 侧由 `reconcile_spawn_result`（`scheduler/mod.rs:812`）落到既有的 `fail_spawn`（`:891`）；gate 侧正好落进 #685 review F4 留下的 **pre-bump 失败臂**（`:1679-1699`，注释逐字描述的就是"`prepare_tx` 在 guarded bump 之前返回 Conflict"这一情形）| 于是 r5 的 b2 动作**原封不动地发生**，但 scheduler 里一个 `if` 都不用加；gate 侧的行终结同理。**这也是把强制点选在 `prepare_tx` 而不是选在 scheduler 的第二个理由** | §5.3.3、§12 切片 3a |
| r6 | 自查 | **`DATA_KINDS` 在 `crates/calm-types/src/report_blocks/kinds.rs:45`，不是 `:44`**；`validate_payload` 是 `:55-91`，未知 kind 的报错臂在 `:67-70` | 更正 §3.1 与 §12 切片 1 的两处引用。（r4 声称已按 HEAD 重校全文 248 处 `file:line`，这一处是漏网的第 249 处；通道 A 本轮也沿用了错误值——**两边同错说明"引用已复核"本身也需要抽样复核**）| §3.1、§12 切片 1 |
| r6 | 自查 | **旧的第五条起活入口 `calm.task.dispatch` 确已退役**：今天只剩一个直接报错的兼容 shim（`crates/calm-server/src/mcp_server/tools/emit.rs:88-118`，逐字 `"calm.task.dispatch was retired (#644); no task was dispatched"`）| 这条对 ⑲ 是必需的：只有它成立，"为 task 起活的 op kind 是一个封闭集合（三个 worker + `task-verify`）"才是真话，而整条规则压在那个封闭性上。**若将来新增第五个 task 绑定的 op kind 而忘了加那一行，保证会静默破掉**——已记进 §13.23 | §5.3.3、§13.23 |
| r1 | 自查 | **`calm.report.write_markdown` 完全不受 stomp guard 约束**（`wave_report.rs:167-184` 只对 `Replace` 调它）⇒ spec 今天就能任意改删任何非 prose 块 | 这是本轮最重要的自发现：它使"块级工具的入口校验"作为安全边界**整体失效**，逼出 §3.7 的收口设计 | §0.2(a′)、§3.7 |

---

## Related

- **#973 / #978** —— 单写者原则（本设计是它在计划面的延伸）；④ proposal 通道撤回，
  `proposals_rebuild_tx` 是 §4.2 rebuild 的形状先例
- **#979** —— 整文档 / REST 写路径的 `if_rev`。**不阻塞本设计**（§5.4）；
  已在 `.claude/worktrees/979-if-rev` 以文档级 `doc_rev` 形状落地（§0.3）
- **#644** —— `tasks` 表 / scheduler / gate 的现状事实源；本设计的投影目标就是它的表
- **#653** —— parked operations；**不提供**"取消正在跑的 task"（§0.1 #14）
- **#760** —— workflow 即插件平台（本设计拆解其 descriptor，§7.1）
- **#891** —— `workflow_input`。**保留，不消解**（§7.6）
- **#830** —— workers run headless，无 worker 级 human-in-loop（§5.3 第 3 级的归宿）
- **#761** —— workflow 组合（被削弱，未解决，§13.6）
- **#976** —— 活数据块（未落地，§13.5）
- **#955** —— 内核 ↔ app 能力边界（§3.1 的安全边界依据；§1 的判据与它的 §1.1 正交）
- **#330** —— "产出与证据，不是协作文档平台"（§1 判据与 §7.4 取舍的动机）
- **#951** —— launchpad propose（§7.5 的"spec 提议、人裁决"形状先例）
