# #985 切片 6 设计增量对抗性评审（codex）

结论：**拒绝按现设计施工。** 我先在未读 §10 时独立列出攻击面，再读 §10；共证出 4 个 BLOCKER、5 个 MAJOR。以下行号均指当前工作树。

## Findings

### [BLOCKER] `child-wave` 未被要求接入 task-bound stale fence，material 后仍可创建子 wave
**攻击**：claim `spawn=sub-wave` → 编辑已纳入哈希的 `goal/context` → `context_stale_at_ms` 已持久 → 把新 adapter 合法注册进 `NON_TASK_BOUND_ADAPTER_KINDS` → `prepare_tx` 仍创建 child。注册表集合测试会绿，执行却在已判 material 的冻结规格上起活。
**证据**：设计的 `prepare_tx` 原子序列没有 `refuse_if_context_stale`（`docs/_985-s6-design.md:192-203`）；权威设计要求所有 task-bound adapter 在副作用前 fail-closed（`docs/architecture/985-doc-as-plan.md:835-860`）；现有分类表允许把新 kind 放进任一侧（`crates/calm-server/src/operation/mod.rs:54-75`），拒绝函数还明确把 task 缺失也当 Conflict（`crates/calm-server/src/operation/mod.rs:78-95`）。
**为什么现有验收抓不到**：§7 #3 只改被哈希排除的 `spawn`，#5 只验 idem/child 相同，#16 只看读时结构（`docs/_985-s6-design.md:408-410,428`）；现有 registry 测试只证明“被分类”，被错分 non-task-bound 仍绿（`crates/calm-server/tests/scheduler.rs:2374-2399`）。
**建议**：把 `child-wave` 加进 `TASK_BOUND_ADAPTER_KINDS`，在 adapter `prepare_tx` 第一条 DB 动作前以 payload.task_id 调 `refuse_if_context_stale`；新增 material `goal` 后直驱真实 adapter、断言零 wave/card/overlay/event 的测试。

### [BLOCKER] 复用 worker 的成功 reconcile 会把长生命周期 child 当成 2 小时 worker 超时
**攻击**：父 task kind=`codex`/`claude`、`spawn=sub-wave` → child op 成功 → 沿现有 `reconcile_spawn_result → mark_running` → 写 `running_deadline_ms=now+2h` → child 合法运行超过 2h → sweep 把父任务错误终结为 liveness failure，child 仍活着。
**证据**：设计要求同一 `drive_spawn` 分流，却未裁决 running stamp/deadline（`docs/_985-s6-design.md:82-90,346-361`）；现有 op 成功必调 `mark_running`（`crates/calm-server/src/scheduler/mod.rs:991-1006`），该写同时 stamp deadline（`crates/calm-truth/src/db/sqlite/task.rs:281-302`）；Codex/Claude 被 deadline 覆盖（`crates/calm-server/src/scheduler/mod.rs:277-279`），默认 7200 秒（`crates/calm-server/src/scheduler/mod.rs:88,491-505`），超时 sweep 会 fail task（`crates/calm-server/src/scheduler/mod.rs:1288-1296,1350-1379`）。
**为什么现有验收抓不到**：§7 #5/#12/#14 都在 child 创建或终态附近断言，没有“child 非终态跨过 deadline”时间轴（`docs/_985-s6-design.md:410,424,426`）；所有快速测试都会绿。
**建议**：按 `(task.spawn, task.kind)` 决定 liveness；sub-wave 成功用专用 `dispatched→running` 写，`running_deadline_ms=NULL`，且不得把 child result.id 当 `worker_card_id`。加短 deadline+仍 Working child 的 sweep 测试。

### [BLOCKER] 树预算把当前 pending 输出当 occupied，会让无关报告编辑删除并重建同一任务
**攻击**：树预算=1，child 报告已有 pending spec 任务 A → 只改 prose/重跑 rebuild → §4.1 CTE 数到 A，树项 capacity=0 → A verdict 变 unschedulable → `project_tasks_tx` 的删除阶段删 A；下一次写 count=0 又重建 A。预算从未超，但投影不幂等且任务静默消失/振荡。
**证据**：设计明确树计数包含 pending（`docs/_985-s6-design.md:257-266,268-275`），又只说把树项接入 `evaluate_schedulability` 并取较严者（`docs/_985-s6-design.md:280-294`）；现实现刻意规定 pending 是输出、不是 occupancy 输入（`crates/calm-truth/src/db/sqlite/task_projection.rs:777-796`），unschedulable 的 pending 会被删（`crates/calm-truth/src/db/sqlite/task_projection.rs:839-876,939-945`）。
**为什么现有验收抓不到**：§7 #9 用“全 pending 子树”只证明 pending 被数，#10 只证明跨 wave 会挡新声明（`docs/_985-s6-design.md:421-422`）；两者都可在不重复投影原声明的 fixture 上全绿。
**建议**：写路径用 `external_occupied = 全树非终结 - 当前 wave pending`，再把当前报告的 clean spec declarations 按稳定顺序重新准入；现有同 key pending 是候选输出，不是已占槽。新增“同文档投影两次结果/事件完全相同”和“删 A 同事务释放给 B”测试。

### [BLOCKER] child 终态不是单调事实，reopen/删除使父任务结果依事件顺序而变
**攻击**：① child Done → 父进 `done`/`verifying` → user reopen child 到 Planning → live arm忽略非终态，sweep 又只扫 `dispatched/running` 父行，之后 child Failed 也改不回父结果。② child Done 的 handler 读到 Done → child 被删/重开 → handler 仅以“父状态+child id”guard 仍写 success。③ success 先提交、child 后删除时父保持 done；反序则映射为 `child-wave-deleted`。同一最终 DB 状态得到不同父结论。
**证据**：终态可由 user reopen 到 Planning（`crates/calm-types/src/wave_lifecycle.rs:236-252`）；设计 live 只在 `to.is_terminal()` 闭合，sweep 只枚举 `dispatched/running`，UPDATE 不 guard child lifecycle/version（`docs/_985-s6-design.md:346-361`）；映射却宣称 row missing 必 failed（`docs/_985-s6-design.md:329-339`）。
**为什么现有验收抓不到**：§7 #13 是三个顺序式终态/删除 fixture，#15 只比较 live 与 sweep 的同构初态（`docs/_985-s6-design.md:425-427`）；没有 terminal→Planning 或 terminal/delete 交错，均会绿。
**建议**：先裁决产品语义：最小安全修法是 linked child 一旦闭合父任务便拒绝 reopen/delete；若必须允许，则要有 parent task reopen/重新验证状态机。child 状态读取必须放进与父 flip 相同的 IMMEDIATE tx，并在 SQL guard 中复核 child 当前 lifecycle/存在性。

### [MAJOR] child Done→verifying 没有规定同事务 `TaskCompleted`，gate 快路与 spec 通知可失活
**攻击**：child Done → 共享函数只调用 `task_report_success_from_worker_tx` 把父行写成 verifying → 不发 `TaskCompleted` → dispatcher 不 poke 父 wave，gate 只能等下一轮 300 秒 sweep；spec 也收不到父任务完成 claim。
**证据**：设计只点名 DB flip 函数与 UPDATE guard（`docs/_985-s6-design.md:333-361`）；该函数本身只做 UPDATE，不产事件（`crates/calm-truth/src/db/sqlite/task.rs:523-541`）；现有 gate 快路依赖 `TaskCompleted` poke（`crates/calm-server/src/dispatcher/mod.rs:975-1005`、`crates/calm-server/src/scheduler/mod.rs:552-570`）。前置 fork5 反而明确要求 flip+事件同事务（`docs/_985-s6-fork5.md:20`）。
**为什么现有验收抓不到**：§7 #12 只要求父状态到 `verifying`，#14 只验证 child closure sweep（`docs/_985-s6-design.md:424,426`）；断言状态后立即结束就会绿。
**建议**：共享 reconcile 必须是“guarded flip + `TaskCompleted/TaskFailed` + 必要 lifecycle events”的 eventized tx；带 gate 用例继续断言 gate attempt 实际启动/终结，而非只断言 `verifying`。

### [MAJOR] `UNION ALL` CTE 没有环/步数 guard，FK 与 self-CHECK 挡不住 `A→B→A`
**攻击**：建 A、B 两行后把 A.parent=B、B.parent=A；两个 FK 都有效、都非 self。向上 CTE永远找不到 NULL root，且不是“零行”，而是递归触顶/报错；深度异常链也先完整展开再检查，成为写路径 DoS。
**证据**：DDL 只要求自 FK 与 `parent<>id`（`docs/_985-s6-design.md:124-133`）；CTE 使用 `UNION ALL` 且无 visited/path/depth 截断（`docs/_985-s6-design.md:155-170`）。前置清查“内核创建可避免环”的结论依赖 writer 纪律，不是 FK 性质（`docs/_985-s6-forks.md:71-83`）。
**为什么现有验收抓不到**：§7 #6 只有合法 0/3/4 深度，#17 只有 descendant delete（`docs/_985-s6-design.md:418,429`）；环和超深导入态均未覆盖。
**建议**：CTE携带 path/visited 与 `depth<=MAX+1`，返回 `{root, cycle, truncated}` 三态；cycle/truncated 均 fail-closed、可诊断。migration 加触发器阻止 parent 更新成后代，至少加原始 SQL 环 fixture。

### [MAJOR] §7 #11 的“逐字节行为不变”永远证明不了“零新增查询”
**攻击**：删除非树短路、对每个 standalone wave 总是执行只读递归 CTE，但保持返回 verdict 完全相同。声称的性能性质为假，响应逐字节仍相同。
**证据**：性质是“非树一条 CTE 都不跑/零新增查询”（`docs/_985-s6-design.md:290-298`），验收却只断言行为逐字节不变（`docs/_985-s6-design.md:423`）。
**为什么现有验收抓不到**：这正是 #11 指定的变异；只读 SELECT 没有可观察响应差异，所以该测试按构造仍绿。
**建议**：给 tree query helper 加测试 seam/计数器，或用 SQLite trace/authorizer 断言 standalone 路径零次命中；行为回归另保留，不能冒充查询数验收。

### [MAJOR] §7 #5 的重复 submit 不能证明 payload 来自冻结行
**攻击**：让 child payload 重读当前报告的 `goal/context/cwd`；不编辑报告地重复 submit，两次 hash 相同、同一 child，测试绿。claim 后先改 `goal` 再 crash/retry，第二次 hash 才冲突并把父任务错误 fail。
**证据**：设计要求 payload 只来自冻结行（`docs/_985-s6-design.md:82-90,214-219`），但 #5 的唯一观察是“重复 submit ⇒ 同一 child”（`docs/_985-s6-design.md:410`）。
**为什么现有验收抓不到**：变异引入的非冻结事实在 fixture 中没变化；断言与错误实现看到同一报告事实源。
**建议**：在第一次 op insert 后、prepare/recovery 前编辑纳入哈希字段，重启真实 `resume_dispatched`，断言 op payload hash、child seed、child id 都保持第一次冻结值；禁止用 payload builder 计算 expected。

### [MAJOR] §1 前置条件 3 没有独立的“改坏会红”载体
**攻击**：保留 claim 后 `task_get_tx` 供日志/事件，却把 `drive_spawn` 参数改回 claim 前 `task`；在没有制造 row-only 差异的测试里，前后快照相同，四条路由测试全绿，性质已不再由重读承载。
**证据**：设计把同事务重读列为承重前置（`docs/_985-s6-design.md:76-80`），现代码确实重读并返回 frozen（`crates/calm-server/src/scheduler/mod.rs:803-812,920-925`），但 §7 没有“返回/使用 pre-claim snapshot”变异；#2 只删 docRev fence，#3 只让恢复重读报告（`docs/_985-s6-design.md:407-412`）。
**为什么现有验收抓不到**：生产报告编辑会升 docRev，先被 #2 race-lost，无法制造“pre-read 与 tx re-read 不同但 fence 仍通过”的区分；未注入 row seam 时两份事实同值。
**建议**：加 claim test hook，在解析后用同一 IMMEDIATE tx seam 改 pending row 的 frozen selector，再 claim；断言实际 op payload取 tx re-read。更直接地把 `claim_task` API 只返回 frozen、删除外层旧 `Task` 的可用性。

## 四条前置条件逐条可证伪性

| 条件 | 使其成立的落点 | 改坏应红的验收 | 裁决 |
|---|---|---|---|
| 1 规范化+投影 | `TaskDeclaration`/UPSERT/`TASK_COLUMNS`，设计 `docs/_985-s6-design.md:43-69` | §7 #1、#4（`:406,409`） | 有载体；还应遍历 5 个完整 Task reader |
| 2 编辑→claim 栅栏 | `scheduler/mod.rs:741-800` | §7 #2（`docs/_985-s6-design.md:407`） | 成立 |
| 3 tx 内重读冻结行 | `scheduler/mod.rs:803-812` | 无直接变异 | **只在文档成立，见 MAJOR** |
| 4 三路只读冻结行 | `drive_spawn/resume_dispatched`，设计 `docs/_985-s6-design.md:82-90` | §7 #3 即时+恢复、#5 child（`:408-412`） | child 测试事实源不足；见 MAJOR |

## 并发与竞态交错审计

- 两个 spec 同时声明：S1 `BEGIN IMMEDIATE` 后读树并投影；S2 卡在 writer slot，S1 commit 后 S2 重数，理论上不会双准入（`crates/calm-truth/src/db/sqlite/events.rs:338-347`、`crates/calm-truth/src/db/sqlite/task_projection.rs:459-503`）。但必须用 barrier 验收，且先修 pending 自计数。
- 两个 child 同时创建：C1 prepare 先拿 writer slot，C2 等待后重跑 root/count；两条父 task 在 child 创建前已经是被计数的非终结行，所以 skeleton INSERT 本身不消费 task 预算（`docs/_985-s6-design.md:253-278`）。
- claim 与报告写并发：claim 先赢则后写只可更新 pending；报告先赢则 docRev fence race-lost（`crates/calm-server/src/scheduler/mod.rs:741-812`、`crates/calm-truth/src/db/sqlite/task_projection.rs:976-985`）。
- child 终结与 parent wave 删除：按本设计 parent 有 descendant 必 Conflict；两个写事务串行，先后都不能绕过（`docs/_985-s6-design.md:363-373`）。child **自身**终结/删除/reopen 与父 task flip 则不收敛，见 BLOCKER。
- sweep 与 live：两者可同时读同一 child，父行 guard 使首写者赢；但 guard 不含 child 当前态，读后 reopen 会让胜者写入过期结论（`docs/_985-s6-design.md:346-361`）。
- 非树→树短路：报告先提交则 child admission 看到新 pending；child 先提交则报告看到 parent/child 关系，均由 writer slot串行。已有 pending 在后续投影中的自计数仍会振荡，见 BLOCKER。

## fail-closed 三处复核

- §2.3 “零行⇒拒绝”只覆盖断链；环不是零行而是递归错误（`docs/_985-s6-design.md:155-170`），所以当前表述不完整。
- §3.5 深度/预算拒绝明确禁止降级（`docs/_985-s6-design.md:239-247`），但 material stale 可在新 adapter 错分时直接绕过，见 BLOCKER。
- §5.2 row missing 顺序执行会 failed（`docs/_985-s6-design.md:333-339`），但 Done→delete/reopen 后父已终态，guard 不再覆盖，见 BLOCKER。

## 与 §10 的交集/差集

我在读 §10 前的独立清单与其交集只有：是否拆片、强制点二及其短路可证伪性。§10 的“prose 反链”和 Canceled 文案不在我的首轮承重清单。我的差集是：task-bound stale fence、worker deadline、pending 自计数、reopen/delete 非收敛、gate 事件、CTE 环、#5/#11 假验收、前置条件 3 无变异；这些均未出现在 `docs/_985-s6-design.md:479-486`。

## 我抽查了前置清查的哪些行号，哪些对不上

- **对得上**：survey 的 REST route/request/helper（`docs/_985-s6-survey.md:9-12`）对应 `crates/calm-server/src/routes/waves.rs:87-123,125-128,371-376,631-679`。
- **对得上**：survey 的 5 个完整 Task reader（`docs/_985-s6-survey.md:41`）对应 `crates/calm-truth/src/db/sqlite/read.rs:400-431` 与 `crates/calm-truth/src/db/sqlite/task.rs:33-39,131-137`。
- **对得上**：survey 的 claim 后冻结重读（`docs/_985-s6-survey.md:61-63`）对应 `crates/calm-server/src/scheduler/mod.rs:803-812,920-925`。
- **对得上**：forks 的 op prepare 原子性（`docs/_985-s6-forks.md:31-35`）对应 `crates/calm-server/src/operation/repo_sqlite.rs:277-325`。
- **对得上**：fork5 的 gate 触发链（`docs/_985-s6-fork5.md:18`）对应 `crates/calm-server/src/dispatcher/mod.rs:975-1005` 与 `crates/calm-server/src/scheduler/mod.rs:552-570`。
- **对不上**：forks 说“只允许内核创建可避免环”（`docs/_985-s6-forks.md:83`）；其引用只证明当前 writer 纪律，FK+self-CHECK 并不能排除 A↔B，结论超出证据。
- **对不上**：fork5 用 immutable terminal exit 的 precedent 支撑 child reconcile（`docs/_985-s6-fork5.md:24-26`），却漏了 child terminal 可 reopen（同文 `:8`）；该 precedent 不支撑“外部读后再 guarded 写”对可变 lifecycle 安全。
- **对得上**：migration 最大号确为 0070，0071 空闲（`docs/_985-s6-survey.md:97-99`）；目录当前末项是 `0070_task_context_withdrawal_and_verify.sql`。

## 本片应该拆吗

**应该拆。** 切分线是“先铺冻结载体，后一次性激活语义”：

1. **6a（inert plumbing）**：migration、`TaskDeclaration.spawn` 规范化、`Task/TASK_COLUMNS` 五 reader、claim tx re-read 的直接变异测试；`drive_spawn` 暂时仍只有现有 in-wave 分支，因此生产行为不变。
2. **6b（atomic activation）**：task-bound `child-wave` adapter、无 worker deadline 的专用 running 写、root/depth/tree admission、child 创建、父闭合/reopen-delete 裁决、事件、read-state/UI；最后一个提交才打开 `spawn=sub-wave` 分支。

这样 6a 合入后没有“创建而不闭合”，6b 内仍把两个预算强制点与创建/闭合一起交付；只是把高风险的冻结投影连带面先独立审清。若不接受 inert plumbing 作为自洽片，则不能再拆行为片，但仍应拆 PR 且保持部署原子。
