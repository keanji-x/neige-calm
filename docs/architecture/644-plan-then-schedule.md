# Plan、Scheduler 与验证门

状态：当前架构。任务声明的权威已经迁到 report `task` block；本文只说明投影后的执行状态机。

## 权威分工

- Spec/用户决定做什么，并在报告中维护任务声明。
- `tasks` 表是可重建的执行投影，不是第二份计划真源。
- Scheduler 只做机械判定：依赖、wave lifecycle、并发预算、任务状态和 gate policy。
- Operation saga 负责启动 worker 或 gate，并提供幂等、恢复和补偿。
- Worker 报告执行结果；内核验证所有权和状态迁移。
- Gate 由内核运行，决定任务能否从 `verifying` 进入终态。

Scheduler 不解释任务目标，不替 spec 做质量判断，也不能用“看起来成功”代替明确报告或 gate 结果。

## 状态机

```text
pending → dispatched → running → done
                              ↘ verifying → done
                                          ↘ failed

pending/dispatched/running/verifying → canceled
dispatch/spawn/report/gate 失败       → failed
```

终态 `done | failed | canceled` 不再迁移。同一次报告、恢复或 sweep 重放必须通过 compare-and-set 变成无副作用的重复。

`worker_card_id` 在 dispatch/worker 建立时绑定，此后 worker 报告必须证明自己拥有该任务。Spec verdict 与 worker report 是不同权限类别，不能共享一个宽松写入口。

## Ready 集合

一个任务只有同时满足以下条件才可 claim：

- 状态为 `pending`；
- 所有依赖为 `done`；
- 声明仍有效且没有阻断诊断；
- wave lifecycle 允许调度；
- wave/树的并发预算有容量；
- 没有其它 scheduler 已经 claim。

Ready 集合在 wave 锁与数据库事务内计算。Claim 与 `TaskDispatched` 事件同事务提交，避免两个 scheduler 启动同一任务。

任务优先级和文档顺序只决定 ready 集合内的稳定选择顺序，不能绕过依赖、预算或诊断。

## Dispatch

每次 dispatch 使用稳定 task identity 派生 operation idempotency key。重启或重复 poke 必须命中同一个 operation，而不是再创建一次 worker。

典型流程：

1. claim `pending → dispatched`；
2. 提交 task event；
3. operation `prepare_tx` 创建/绑定 worker card 和 session projection；
4. 启动外部 worker；
5. worker 开始后 CAS 到 `running`；
6. worker 通过受权工具报告完成或失败。

Terminal worker 的退出由 live hook 和 boot/sweep 两条路径收敛到同一终结函数。Codex/Claude runtime 无声死亡也必须由 reaper/reconcile 转成显式失败，不能让 `running` 永久悬挂。

## Worker 报告与所有权

Worker 成功/失败报告必须同时满足：

- actor 是该任务绑定的 worker；
- task 处于允许的非终态；
- card/session/runtime 归属一致；
- 同一终态没有已经赢得 CAS。

Worker 报告和 task row flip、事件、必要的 wave lifecycle 推进同事务提交。CAS 输掉时先读取当前行区分“同结果重复”与“冲突终态”；冲突不能静默当成功。

Kernel reaper 是单独的权限类别。它处理 worker 已死、无法再自证所有权的情况，但必须证明 runtime/operation 身份和 scheduler 来源，不能成为任意终结任务的后门。

## 验证门

要求 gate 的任务在 worker 成功报告后进入 `verifying`。Gate 使用 operation 的 parked 能力运行外部命令：

- 提交前记录 operation 与 attempt identity；
- 启动后记录 owned process identity 和日志路径；
- gate 运行时 operation 处于 `parked`，不持有驱动 lease；
- 退出结果通过 durable evidence 和 `complete_parked_tx` 收敛；
- task flip 与 `TaskGateResult` 同事务完成。

Gate 是 at-least-once 恢复模型，因此命令必须可重跑。发布、扣费等非幂等副作用不应作为 gate。

Gate 结果：

- 全部步骤通过：`verifying → done`；
- 步骤失败：`verifying → failed`；
- 基础设施错误、超时或无法证明进程身份：fail closed，并保留可读诊断。

Gated task 的 worker 自报完成不能直接提升 wave lifecycle；提升只能在 gate 终态事务中发生。

## 触发与恢复

Scheduler 可以被 task/report/lifecycle/gate 事件唤醒，但正确性不能依赖 broadcast 不丢。它还必须有：

- boot sweep；
- 周期 reconcile；
- broadcast lag 后的全量 sweep；
- operation 状态与 task 状态的双向对账。

事件负责低延迟，数据库 sweep 负责最终收敛。

恢复时：

- 已有相同 operation：继续或读取终态，不创建新 operation；
- operation 已 parked 且进程存活：重新挂观察者；
- 有可信 exit evidence：完成 parked operation；
- 进程死亡且无可信结果：按 infra failure 处理；
- task 已终态：任何迟到回调都不再写。

## 配置与风险

- `task_budget` 限制 wave 内并发；树预算还受 doc-as-plan 的共享额度约束。
- `require_task_gates` 打开时，缺 gate 的声明不进入 ready 集合，除非显式声明受支持的跳过理由。
- 并行 worker 若共享同一 checkout 会相互污染；在 workspace lease 完整隔离前，低默认并发是安全边界。
- Gate log 和长期 operation evidence 需要有界保留策略。
- 用户直接修改计划的入口是 report task block UI，不应重新开放独立 tasks 写 API。
- Scheduler 不拥有业务判断；新增任何“自动猜测成功”的分支都需要重新设计权限和恢复语义。

## 必须保持的测试

- 并发 claim 只有一个 winner，且只创建一个 worker operation。
- Worker 报告、reaper 和 spec verdict 的竞态不会产生两次终态。
- Gate 在 T1 后、spawn 后、park 后和结果写入前崩溃均能收敛。
- Lost broadcast、boot 和周期 sweep 得到相同 task 终态。
- Incremental report projection 与 rebuild 产生同一 plan，scheduler 不读取已失效声明。
- Terminal 快速退出和 kernel downtime 都能终结对应 task。
- 迟到/重复 report 与 gate outcome 不会覆盖既有终态。
