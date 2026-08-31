# Parked operation

状态：当前 operation saga 契约。

## 解决的问题

有些外部工作已经启动，但不能在短事务或驱动 lease 内等待完成，例如验证命令、远程 CI 或其他有 durable identity 的进程。

`parked` 表示：

> operation 已完成启动提交，外部工作仍在进行；operation 不持有驱动 lease，结果稍后通过受控入口写回。

它不是“成功”，也不是“暂时忽略”。任何把 parked 当终态或普通可重驱 phase 的代码都会造成重复副作用。

## Phase

核心 phase：

```text
pending
→ tx_committed
→ app_server_interact / spawn_started
→ spawn_succeeded
→ parked
→ succeeded

失败路径：
任意可补偿阶段 → compensating → failed
无法安全自动处理   → stuck
```

普通 spawn 返回 `SpawnOutcome::Ready`；长任务返回：

```rust
SpawnOutcome::Parked {
    deadline_ms,
    observer,
}
```

进入 parked 前必须已经提交足够的恢复证据。

## Spawn evidence

`SpawnArtifacts` 记录可验证的外部身份，例如：

- pid/pgid；
- process start time；
- boot id；
- log/exit evidence path；
- consumer-specific attempt identity。

PID 不能单独作为所有权证明，因为会复用。信号、kill、恢复和完成都必须验证完整 owned-process identity。

Evidence 在 spawn 后、park 提交前通过受控 hook 写入 operation row。不能只留在 observer task 的内存里。

## Lease 与单 winner

Operation driver lease 只保护推进数据库 phase 的临界区。Parked 外部工作可能持续很久，因此进入 parked 后释放 lease。

以下动作都必须以 phase + lease/CAS 证明唯一 winner：

- 进入 parked；
- 完成 parked；
- cancel；
- deadline/sweep fail；
- boot recovery 重新挂观察者。

只检查 lease owner 不够；迟到 writer 可能仍持有旧 lease token。每个终态写还必须检查当前 phase 和 attempt identity。

## 完成

`complete_parked_tx` 是 parked 结果进入数据库的唯一入口。它在调用者事务内：

1. 验证 operation 仍是对应 attempt 的 parked phase；
2. 把结果合入 operation result；
3. 运行 consumer 的同事务投影；
4. 写 operation/task event；
5. 转为 succeeded 或 failed。

完成后才 broadcast。重复相同结果返回 already-resolved；冲突结果不能覆盖先到终态。

Observer 只负责等待外部结果并提交 evidence。Observer 崩溃不应改变 operation 的真实状态，boot/sweep 必须能从持久证据恢复。

## Recovery

Boot 和周期 sweep 对 parked operation 做以下分类：

| 外部状态 | 动作 |
|---|---|
| 进程身份可信且仍存活 | 调 consumer 的 `recover_parked` 重新挂 observer |
| 有完整、可解析的 exit evidence | 调完成入口 |
| 已死亡且无可恢复结果 | 记 infra failure |
| 超过 deadline | 验证身份后终止，进入失败/补偿 |
| 身份不匹配或证据矛盾 | fail closed，进入 stuck 或明确失败 |

恢复不得重新执行已经启动的外部动作。若 consumer 无法从 evidence 判断，则必须保留不确定状态或失败，不能假装“从未发生”。

## Cancel 与 deadline

Cancel 和 deadline 都与正常完成竞争。它们必须：

- 先赢得 operation CAS；
- 只对验证为本 operation 所有的进程发信号；
- 保存终止原因；
- 让 consumer projection 与 operation 终态一致；
- 对迟到 outcome 返回 already-resolved。

Deadline 是 liveness backstop，不是精确计时器。没有 background driver 时，它依赖 waiter、consumer reconcile 或 boot 才被执行；需要严格时限的 consumer 必须提供周期 tick。

## Consumer 责任

Parked primitive 不解释外部结果。每个 consumer 必须实现：

- 如何记录足够的 spawn evidence；
- 如何验证进程仍属于本 attempt；
- 如何恢复 observer；
- 如何解析 durable outcome；
- 如何把 operation 结果与自己的 projection 同事务完成；
- 哪些错误可补偿，哪些必须 stuck/fail closed。

#644 gate runner 是典型 consumer。未来 remote CI 也可以复用，但不能绕过上述合同另写一套 parked 状态机。

## 风险

- Parked 时间远长于普通 operation，固定 25ms 轮询会产生无意义 DB/procfs 压力；waiter 应采用适合长任务的退避。
- Observer 在 kernel 活着时崩溃，可能直到 sweep/deadline 才被发现。
- Table migration、phase CHECK 和所有索引必须同步；漏索引会把恢复扫描变成全表热点。
- Exit file 必须使用临时文件 + atomic rename；存在但不可解析按失败处理，不能等待它“以后完整”。
- 外部系统若没有 idempotency key、operation id 或可查询 receipt，parked 只能保存不确定，不能提供 exactly-once。

## 必须保持的测试

- 两个恢复者竞争同一 parked op 只有一个 winner。
- 完成、取消、deadline 三方竞态只产生一个终态。
- Observer/进程/kernel 在每个提交边界崩溃后均可恢复。
- PID 复用、boot id 变化和 start time 不符时拒绝发信号。
- 相同 outcome 重放无副作用，冲突 outcome fail closed。
- Consumer projection 与 operation 终态原子提交。
- Exit evidence 半写、损坏、来自旧 attempt 时不会被接受。
- 活进程重启后重新挂 observer，不重复 spawn。
