# Upgrade stability policy

状态：当前策略。适用于持久化和跨进程契约；具体版本值以代码与握手响应为准。

Neige 仍允许破坏性改动，但必须明确它改变了哪一类边界、如何迁移，以及不兼容时怎样失败。

## Tier A：持久化契约

包括数据库 schema、持久事件、内核拥有的卡片 payload、operation/session 状态和 plugin manifest。

要求：

- 新二进制能够迁移旧数据；
- migration forward-only，不支持旧二进制继续写新 schema；
- 未知 migration、schemaVersion 或 manifest version 必须拒绝启动或读取；
- 破坏性变更与迁移在同一变更中交付；
- 事件可能按 retention 删除，永久事实不能只存在于事件日志。

数据库回滚依赖升级前备份，而不是 down migration。

## Tier B：跨进程契约

包括 REST、sync/WebSocket、MCP、terminal/supervisor framing、AppBridge 和 frontend compatibility。

要求：

- 首次通信携带明确版本或 capability；
- 接收端验证兼容性；
- 不兼容时明确拒绝，不能部分工作或静默丢字段；
- 每个版本只描述自己的边界，不能用产品版本代替协议版本；
- 改变 wire 语义时同时更新两端、生成产物和失败路径测试。

## Tier C：进程内部契约

包括 Repo trait、route 实现、React 组件结构和内部调度细节。

这些接口不承诺稳定，也不应增加版本字段。若内部改动改变了持久化或 wire 形状，必须重新归类为 Tier A/B。

## Tier D：实验性外露面

包括尚未形成稳定消费者的解析器、TUI 适配和第三方 app 表达面。

实验性能力必须明确标记，消费者必须容忍删除或破坏性变化。它们不进入稳定版本协商，成熟后再提升到 Tier A/B。

## Review checklist

- 改动是否触及持久数据？迁移、备份和旧数据测试是否同时存在？
- 改动是否跨进程？版本、能力协商和拒绝行为是否同步更新？
- 生成的 OpenAPI、wire types、event goldens 是否与源码一致？
- 旧二进制面对新数据库是否 fail closed？
- 新客户端面对旧服务、旧客户端面对新服务是否得到明确结果？
- 这只是内部重构吗？如果是，不要制造版本承诺。
- 这是实验能力吗？如果是，不要把它写成稳定 API。
