# #985 切片 6 设计增量 v3 · 对抗性评审 r3（codex）

结论：**NO**。r2 的原最小阻塞集已按字面清空，但 v3 的 quiescence 与 harness 恢复各制造了一个同形状的新永久等待；另有一条验收 seam 仍未闭合。只读代码，未跑 cargo；用 sqlite3 `:memory:` 复现了 §2.1 三种 FK 结果。

## BLOCKER

### [BLOCKER-1] `Done + pending` 使 quiescence 永远不成立

- **结论**：`no-op + 等下一轮 sweep` 对 `verifying` 成立，对 `pending` 不成立；这是确定性活锁。
- **攻击**：子 wave 留一条 `origin='block'`（或 legacy）的 pending task，再合法 `Reviewing → Done`。父闭合每轮都因该行 no-op；而 Done wave 的 scheduler 在 lifecycle gate 后不再 claim pending，该行没有任何终结写者。父 task 又按 v3 永久 `running`、无 deadline，于是父子永久占位。
- **证据**：v3 的 quiescence 明确计入 pending，且只说“下一轮 sweep”（`docs/_985-s6-design.md:519-523`）；投影实际创建 `origin='block', status='pending'` 行（`crates/calm-truth/src/db/sqlite/task_projection.rs:965-983`）；scheduler 先驱动 verifying、随后对不允许调度的 lifecycle 直接返回，pending claim 在返回之后（`crates/calm-server/src/scheduler/mod.rs:552-585`）；FSM 允许 `Reviewing → Done`，不检查 task（`crates/calm-types/src/wave_lifecycle.rs:279-286`；`crates/calm-server/src/wave_lifecycle.rs:47-68`）。
- **为什么 v3 验收抓不到**：#13b 只造 verifying，且只断言当下 no-op（`docs/_985-s6-design.md:637`）。verifying 即使 wave 已 Done 仍会被 lifecycle gate 之前的代码驱动，所以该 fixture 后续能静止；把残余行换成 pending，#13b、#15、#16 仍全绿，而性质为假（`crates/calm-server/src/scheduler/mod.rs:552-578`）。
- **最小修法**：对“被 `child_wave_id` 引用的 wave”在所有 lifecycle writer 的同一 in-tx 收口拒绝 `→ Done`，只要还存在 pending/dispatched/running/verifying；或在该 transition 内以明确语义原子终结 pending。新增 `block pending`、`legacy pending` 两例，必须既证首次拒绝/终结，又证父任务最终闭合，不能只证即时 no-op。

### [BLOCKER-2] bootstrap operation 的 `Failed/Stuck` 没有父任务映射

- **结论**：v3 只规定 bootstrap 的重试、顺序和 exactly-once，没有规定 `spec-harness-start` 自身 `Failed/Stuck` 后父 task/child wave 怎么收敛。
- **攻击**：`child-wave` op 已成功并落下 Draft child；稳定 idem 的 bootstrap 返回 `Stuck`。每次 resume 都命中同一终态 op，harness 不存在，running flip 无成功前提。现有 dead-root 只认最新 start-op `phase='failed'`，明确放过其它终态，因此 Stuck child 与 dispatched/running parent 可永久停住。
- **证据**：v3 的修法只写 submit 必须重复、早于 flip、稳定 key，并且验收只有两个 crash 点（`docs/_985-s6-design.md:376-395`）；operation 结果模型把 `Failed` 与 `Stuck` 分成两臂（`crates/calm-server/src/operation/mod.rs:531-547`）；现有普通 spawn 显式把两臂都送入 `fail_spawn`（`crates/calm-server/src/scheduler/mod.rs:1031-1036`），而 dead-root SQL/说明只接受最新 `phase='failed'`（`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:177-195,211-225`）。
- **为什么 v3 验收抓不到**：#19 的两种变异都是“成功链上的崩溃/重复启动”，没有注入 bootstrap Failed/Stuck；保持正确 idem、正确顺序，只删 outcome reconcile，#19 仍绿（`docs/_985-s6-design.md:644`）。
- **最小修法**：写死两级 operation 的 outcome 表：child-create 或 bootstrap 的 Failed/Stuck 都须走 eventized、guarded 的父 task failure；已有 child skeleton 时同时规定 child lifecycle/可删除状态。各注入 Failed 与 Stuck，断言父状态、理由、事件及重启后幂等。

## MAJOR

### [MAJOR-1] #5 与 #5b 虽拆行，制造“报告与冻结行分歧”的 seam 仍未定义

- **结论**：r2 的 stale-fence 抵消问题只在表格里拆开了，#5 的真实 adapter 如何看见“当前报告不同、但不 stale”仍未写出。
- **攻击**：实现者让纯 payload builder 正确读冻结 Task，#5 只测 builder；随后在 child adapter 外层重读当前报告并覆盖一个字段。#5 绿；真实报告编辑又被 stale fence 在第一副作用前拒绝，#5b 也绿；生产性质仍假。
- **证据**：正文仍要求“op insert 后、prepare/recovery 前编辑纳入哈希字段再走真实 resume”（`docs/_985-s6-design.md:161-167`），但 prepare 第 0 步必先按该编辑留下的 stale 位拒绝（`docs/_985-s6-design.md:347-350`）；新表只说四字段 sentinel，并未给非 stale 分歧 seam，而 #5b 明确消费真实编辑（`docs/_985-s6-design.md:627-628`）。
- **为什么 v3 验收抓不到**：被测 builder 与 #5 expected 可以都只看冻结行；真正错误的 adapter 覆盖发生在 builder 之后。#5b 又只证明 fence 存在，二者没有共同覆盖该边界。
- **最小修法**：明确一条真实 child adapter seam：直接构造“冻结 tasks 行四个 sentinel / 当前报告四个不同 sentinel / `context_stale_at_ms IS NULL`”，驱动 adapter 到持久 seed，expected 手写；禁止只调用 builder。四字段逐一变异必须分别红。

## MINOR

- 文档标题仍写 v2，而正文称 v3（`docs/_985-s6-design.md:1,13-15`）。
- §7 不是“28 条”：表内实际有 30 个编号行（含 3a/3b/3c、5b、13b、14b、21b、21c），而正文与 §12 都写 28（`docs/_985-s6-design.md:619-650,802-803`）。这不影响安全性，但会让门禁清单/完成计数漂移。

## v3 的修法里，哪几处制造了新洞

1. **quiescence 修法**把“过早成功”改成 no-op，却没有证明被计入集合的每一种状态都会离开；pending 在 Done lifecycle 下恰好失去唯一 claim 路径（`docs/_985-s6-design.md:519-523`；`crates/calm-server/src/scheduler/mod.rs:572-585`）。
2. **harness 顺序 + exactly-once 修法**封住 crash/重复，却只覆盖 Succeeded 时间轴；已有 outcome 枚举中的 Failed/Stuck 没被消费（`docs/_985-s6-design.md:376-395`；`crates/calm-server/src/operation/mod.rs:531-547`）。
3. **#5/#5b 拆分修法**分离了断言，却没提供能绕开 stale、又驱动真实 adapter 的分歧 seam，仍可降级成 builder 单测（`docs/_985-s6-design.md:161-167,627-628`）。

## §7 的 28 条里，哪几条在我设计的变异下仍然绿

实际按表是 30 条。直接相关的误绿如下：

| 变异 | 仍绿 |
|---|---|
| child `Done` 时留下 pending（不动 quiescence SQL） | **#13b** 只测 verifying 的即时 no-op；**#15/#16** 用静止 fixture 时仍闭合/同构；其余项无关（`docs/_985-s6-design.md:636-643`）。 |
| bootstrap 返回 `Stuck`，删除/遗漏其 outcome reconcile，但保留稳定 idem 与正确顺序 | **#19** 只跑成功链 crash 点；#14 只人工造 child Failed/Canceled/Deleted（`docs/_985-s6-design.md:638,644`）。 |
| builder 正确，真实 adapter 在 builder 后用当前报告覆盖单字段 | **#5** 可只测 builder，**#5b** 仍会被 stale fence 正确拒绝（`docs/_985-s6-design.md:627-628`）。 |

其余 v3 替换过的变异，我没有找到仍绿的同形攻击：特别是 #1、#6、#7、#11、#17、#20、#21/#21b、#23 的 oracle 已独立于被测事实源（`docs/_985-s6-design.md:621,629-634,642,645-650`）。

## §12 五问逐答

1. **NO ACTION 实证覆盖真实删除形状：是。** REST handler 逐 wave 做的是进程/terminal/lease 清理，事务里最终只调用一次 `cove_delete_tx`，没有逐 wave `wave_delete_tx`（`crates/calm-server/src/routes/coves.rs:317-386`）。`cove_delete_tx` 虽逐 wave 清无 FK 的 task/session，删除 waves 的动作仍只有一条 `DELETE FROM coves`（`crates/calm-truth/src/db/sqlite/cove.rs:147-185`），而 `waves.cove_id` 是 CASCADE（`crates/calm-truth/migrations/0001_init.sql:20-29`）。我用 sqlite3 得到同 cove NO ACTION=成功/0 行、RESTRICT=FK 失败、跨 cove NO ACTION=FK 失败，与 v3 一致。
2. **下一轮 sweep 会来，但 pending 仍会永远等。** 周期 tick 默认 300s 且持续调用 reconcile（`crates/calm-server/src/dispatcher/mod.rs:838-857`），全局 sweep 枚举全部非终结 task（`crates/calm-server/src/scheduler/mod.rs:1240-1267`；`crates/calm-truth/src/db/sqlite/read.rs:422-431`）。子 task 终结事件只 poke 子 wave（`crates/calm-server/src/dispatcher/mod.rs:975-1005`），所以父只靠该周期兜底；verifying/running/dispatched 可进展，pending 在 Done 下不可进展，见 BLOCKER-1。
3. **跨 cove typed 写路径：本 PR 设计下没有第二条。** v3 明定 parent 指针只由内核写（`docs/_985-s6-design.md:223-226`），且刻意不进客户端 `NewWave`，只由 child adapter 定向写（`docs/_985-s6-design.md:784-791`）；现有公共创建 INSERT 是固定列清单（`crates/calm-truth/src/db/sqlite/wave.rs:47-84`），迁移后只会留下 NULL。任意 raw SQL 仍可造毒数据，但 #21c 的全表 tripwire 会抓；应保留反向跨 cove 删除失败作为约束证据（`docs/_985-s6-design.md:648`）。
4. **会误绿的验收**：#5/#5b、#13b/#15/#16、#19，详见上一节。另：#10 可以施工为真实 adapter 测试，不必降级。三个 worker adapter 的 stale check 都在 prepare 的首个 DB 副作用前（`crates/calm-server/src/operation/codex_adapter/mod.rs:766-780`；`claude_adapter/mod.rs:768-780`；`terminal_adapter.rs:576-595`）；task-verify 也在 verifying/gate 前检查（`crates/calm-server/src/operation/task_verify_adapter.rs:627-665`）。每个 kind 需要自己的合法 payload/fixture builder；若改成只断言名单成员，直接不满足 #10 的“驱动真实 adapter”（`docs/_985-s6-design.md:633`）。
5. **PR-A 最小阻塞集未清空。** r2 的三项/七项按字面均已修；新最小集只有 BLOCKER-1（Done+pending）与 BLOCKER-2（bootstrap Failed/Stuck）。

补充攻击面：第二层 sub-wave 不会与第一层 idem 碰撞；task id 是 `wave_id:key`，而 operation 唯一键还带 kind（`crates/calm-truth/src/db/sqlite/task_projection.rs:965-979`；`crates/calm-truth/migrations/0042_operations_parked.sql:96-98`）。一个仍活着但永不终态的 child spec 确实可让无 deadline 父 task 长期 running，这是 v3 选择的人工 cancel/delete 恢复语义，不单列 blocker（`docs/_985-s6-design.md:451-462,586-590`）；真正不可接受的是上述两种“系统已失去任何自然进展写者”的永久等待。

## 可以施工了吗

**NO。** 最小阻塞集：

1. 收口 referenced child 的 `→ Done` 与非终结 task：禁止带 pending 进入 Done，或原子终结 pending，并加最终闭合 oracle。
2. 定义 child-create/bootstrap 的 Failed/Stuck outcome 表，eventized fail 父 task，并给 child skeleton 一个确定状态；补两种失败注入。

修完这两项后，MAJOR-1 只需把 seam 写实；其余只剩 MINOR，可判 YES。
