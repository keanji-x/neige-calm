# Kernel 与 App 的能力边界

状态：当前架构。代码中的 `plugin` 是面向用户的 app。

## 边界性质

App 进程是本机安装并受信的协作代码。当前没有 OS sandbox，进程继承服务用户的文件、网络和进程权限。因此下面的边界只约束通过 Neige 通道发生的行为，不能阻止恶意进程绕过通道直接产生副作用。

通道边界提供三项保证：

- 产品写入经过内核事务、授权和审计；
- 内核不变量只实现一次；
- 外部副作用可以进入 operation 的幂等与恢复协议。

不受信代码需要独立的进程、凭据和文件系统隔离。

## 归属规则

满足任一条件的能力属于内核：

- 必须和产品状态在同一事务提交或进入事件日志；
- 是多个 workflow 共用的领域原语；
- 错误实现可能破坏一致性、权限、磁盘配额或回收安全。

其余能力可以由 app 实现。报告正文仍由 planner agent 负责组织；app 提供数据和建议，不成为新的报告写者。

内核拥有：

- 领域事实、role gate 和事件写路径；
- operation、scheduler、gate 和 worker 生命周期；
- 内建卡片的持久化契约；
- Track VCS、配额和 GC；
- app 进程、MCP 传输和 `ui://` 资源宿主。

## 通道

### App 调用内核

App 通过 `neige.*` 回调管理自己的 overlay、卡片和私有 KV，并订阅事件。Plugin identity 由连接上下文注入，不能从请求参数接受。

Overlay 和 card 修改走事件化产品写路径。KV 是 app 私有命名空间，不进入产品事件日志，但受配额约束。

Iframe 只能调用 manifest 为 view 声明的 `neige.*` 工具。浏览器入口最终复用同一 callback 实现，不能形成第二套写路径。

### Agent 调用 App

内核把 app 工具代理为 `plugin.<id>_<tool>`。可见性由 track 的 `plugin_scope` 决定，并且 fail closed：归属不明确或 app 不可用时，不向 Agent 暴露工具。

普通工具调用直接代理。Forge action 只接受结构化请求，由内核 operation 执行，以获得幂等、归因和恢复能力。

### 声明式能力

Manifest 可以声明工具、workflow、card view、entrypoint 和 permissions。Manifest gate 只能提供建议或 prompt 输入；可执行 gate 必须由内核拥有。

## App 能表达什么

- 自己的 card kind 和 `ui://` 界面；
- 附着在 track/card 上的 overlay；
- workflow 的任务模板、输入和 planner 指令；
- 提供给 Agent 的外部工具；
- 私有 KV 与事件驱动行为。

App 不能通过 Neige 通道：

- 修改内核拥有的卡片或报告正文；
- 直接写 Track VCS 或 track 文件投影；
- 定义由内核直接执行的 shell gate；
- 绕过 role gate、scope 或事件写路径。

这些限制保护单一事实源、报告 CRDT、持久化 schema、配额和恢复协议，不是待补的扩展点。

## 当前限制

一个 track 只能绑定一个 workflow。绑定后只暴露所属 app 的工具；归属缺失时不回退到全部 app。多 app 协作需要新的组合模型，不能放宽 fail-closed scope。

当前 trust 来自内核配置，没有签名或进程隔离。后续实现可以替换信任来源，但必须保持“由内核判定、消费端 fail closed”。

报告采用单一逻辑作者：planner agent 组织正文，人可以直接编辑，app 通过工具、overlay 和自有 UI 贡献。该规则目前是产品策略，不是完整并发机制；整文档写入仍需要独立的版本冲突控制。
