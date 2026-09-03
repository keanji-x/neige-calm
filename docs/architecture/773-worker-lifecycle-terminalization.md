# Worker 生命周期与任务终结

状态：当前协议。本文保留跨模块不变量，不列 emit-site 清单；调用点按符号搜索。

## 两个状态机

`TaskStatus`：

```text
pending → dispatched → running → verifying → done
                    ↘ failed
任意非终态          → canceled
```

`WorkerSessionState` 描述 runtime/session 的观察状态。它和 task 状态相关，但不是同一事实：

- task 回答“这项工作在计划里处于什么阶段”；
- worker session 回答“这个执行实例发生了什么”。

不能看到 session 终态就直接无条件覆盖 task，也不能用 task 终态反推所有 runtime 已清理。

## 两类终结权限

### Worker / kernel 类

Worker 自报、terminal exit hook、runtime reaper 等路径可以推进 task，但必须证明执行所有权：

- task 绑定的 worker card/session/runtime 与报告者一致；
- task 仍处于该路径允许的非终态；
- kernel reaper 证明 worker 已死亡且 operation 来源可信。

这类路径写 `TaskCompleted` 或 `TaskFailed`，并通过 CAS 决定 winner。

### Spec verdict 类

Spec verdict 是对结果的裁决，不是 worker 自报。它使用独立 guard 和事件语义，不能伪装成 worker ownership。

Gate result 同样是独立类别：它推进 `verifying`，但不借用 worker report 的权限。

将这几类收进一个“任意 actor 可终结任务”的 façade 会破坏所有权边界。

## 原子提交

一次终结必须在同一数据库事务中完成：

- 校验 actor/worker/operation 归属；
- CAS task 状态；
- 必要的 worker session/runtime 投影；
- task event；
- track lifecycle 推进或阻断原因。

事务提交后才 broadcast。任何 best-effort liveness stamp 都不能成为终结正确性的前置条件。

## CAS loser

终结 UPDATE 影响 0 行时，调用者必须读取当前 task：

- 已经是同一终态：幂等成功；
- 已经是另一终态：返回冲突，不覆盖；
- 仍是非终态但 ownership 不匹配：拒绝；
- 行不存在：明确 not found。

不能把所有 0-row 都吞成成功，否则真正的 ownership bug 会被伪装成幂等。

## Live 与 sweep

同一执行可能同时被 live hook 和 background/boot sweep 发现。两条路径必须调用同一个终结原语并依靠 CAS 收敛。

事件/broadcast 只提供低延迟。Kernel downtime、丢 envelope 或 listener lag 后，数据库与 supervisor reconcile 必须仍能终结任务。

Reaper 处理“worker 已经死了，无法自己报告”的情况。它可以绕过活 worker 自证，但必须证明：

- runtime/session 属于该 task；
- 对应进程确实终止或丢失；
- operation/dispatcher 创建关系可信；
- 这不是仍可能提交结果的活 worker。

## Gate 与 verifying

需要 gate 的任务，worker 成功只推进到 `verifying`。只有 gate outcome 可以从 `verifying` 进入 `done | failed`。

Spec verdict 不能跳过 gate；迟到 worker success 也不能把 gate failure 改回 done。Track lifecycle 的完成推进发生在最终 gate 事务，而不是 worker 自报事务。

## 不变量

1. Task 终态不可逆。
2. 一个 task 最多产生一个有效终结事实。
3. Worker actor 只能终结自己拥有的 task。
4. Spec verdict、gate 和 reaper 各自保留独立权限。
5. Live 与 sweep 结果通过同一 CAS 收敛。
6. 终结 row flip、投影、event 和 lifecycle 同事务。
7. Broadcast 丢失不影响最终状态。
8. Liveness telemetry 失败不能阻止正确终结。
9. 迟到 callback 不覆盖终态。
10. Kernel bypass 必须比普通 worker guard 更窄，而不是更宽。

## 必须保持的竞态测试

- Worker success 与 reaper failure 同时发生。
- Terminal live exit 与 boot sweep 同时发生。
- Worker report 与用户 cancel 同时发生。
- Worker success 与 gate failure 乱序到达。
- Spec verdict 与 worker report 竞争。
- 同一 report/outcome 重放多次。
- Worker card、session 或 operation identity 被替换后旧 callback 到达。
- Event subscriber lag/重启时数据库 sweep 仍收敛。
- CAS loser 能区分幂等与冲突。
