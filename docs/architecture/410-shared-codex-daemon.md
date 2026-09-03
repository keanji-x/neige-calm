# Shared Codex daemon

状态：当前架构。

## 边界

Kernel 只运行一个 `codex app-server`。所有 Codex-backed card 在共享 daemon 中拥有独立 thread，并通过持久映射关联：

```text
card
→ card_codex_threads
→ daemon thread
→ turn / hook / terminal projection
```

共享 daemon 是必需组件，没有受支持的 per-card daemon 回退。它的 `CODEX_HOME` 位于 Neige data dir 下，由 kernel 管理配置、socket、日志和重启。

共享进程减少重复 cache/config/token 磁盘开销，但也把 daemon 生命周期变成 kernel 级故障域；任何改动都要同时考虑所有 card。

## Card 与 thread

- 有初始 prompt 的 card：先创建并持久绑定 thread，再启动 turn。
- 空 prompt 的交互 card：进入 pending registry，daemon 报告 thread started 后完成绑定。
- Planner card：使用相同 thread 机制，但带 Planner role 和产品指令。
- Worker card：由 scheduler/dispatcher 创建，使用 Worker role 和稳定 task identity。
- Reset：为同一 card 创建新 thread 并原子替换映射；旧 turn 被 interrupt，旧 thread 不再作为 card 权威。

Thread id 是 provider identity，card id 是产品 identity。调用方不能靠内存 cache 猜测二者关系；重启后以数据库映射恢复。

## 权限与环境

共享 daemon 的 MCP 配置位于 daemon home；每个 thread/card 通过受控环境和 session identity获得自己的 Neige capability。

Planner、Worker 和 Plain 的工具权限不同。共享同一进程不意味着共享 actor、token 或 role。Hook/MCP 写入仍必须解析到 card/session 并通过 role gate。

任何全局 daemon 配置变更都需要评估：

- 是否影响已经存在的 thread；
- 是否改变所有 card 的 approval/sandbox；
- 是否需要重建 daemon；
- 重建期间如何恢复 mapping、turn 和 terminal。

## 运行与恢复

Kernel 负责：

- 启动与健康检查；
- 有界退避重启；
- 重新加载 card/thread 映射；
- 重连 observation stream；
- 对 pending thread 建立超时和失败原因；
- 在 daemon 丢失时让 operation/runtime 进入可解释状态。

Broadcast 或内存 registry 不能成为唯一事实。Daemon 重启后，数据库映射、provider thread list 和 runtime projection需要 reconcile。

同一 card 同时只能有一个 authoritative thread mapping。旧 thread 的迟到 hook 必须根据 session/thread identity 拒绝，不能写到新 runtime。

## 已知限制

- Provider 未提供可靠 thread close 时，reset 后的旧 thread 可能继续占 daemon 内存，但不能再被产品引用。
- 共享 daemon 故障会影响所有 Codex card；隔离靠 thread/session 权限，不靠进程边界。
- Empty-card pending 绑定依赖 provider started event，必须对乱序、重复和超时 fail closed。
- Daemon 内部 transcript 是 provider 事实，不替代 Neige 的 task/report/operation 权威。

## 必须保持的测试

- 多 card 并发创建不会交叉绑定 thread。
- Kernel 在持久绑定前崩溃不会留下看似可用的 card。
- Reset 后旧 thread hook 不能修改新 session。
- Daemon 重启后映射恢复，不重复创建 worker turn。
- Planner/Worker/Plain 的 MCP capability 不串权。
- Pending thread started event 重复、乱序或缺失时结果明确。
- 全局 daemon 重建不会丢失产品状态或把运行中状态伪装成成功。
