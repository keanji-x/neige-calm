# Sync 与事件架构

状态：当前架构说明。

## 定位

Neige 不是 event-sourced 系统。实体表、报告 CRDT、任务投影和 operation 行仍是产品事实；`events` 是持久通知、有限审计、客户端增量同步和内核调度的公共通道。事件会按保留策略裁剪，因此任何需要永久恢复的事实都不能只存在于事件日志。

少数明确标为 out-of-domain 的写入不发事件。新增产品写路径时，必须先判断它是否需要客户端同步、审计或内核消费；需要其中任一项就必须走事件写入口。

## 写入不变量

领域写入通过 `RepoEventWrite`：

```text
BEGIN IMMEDIATE
→ 修改实体/投影
→ 生成 typed Event
→ 在事务内执行 role/decision gate
→ 插入 event row
→ 必要时写 Wave VCS commit
→ COMMIT
→ broadcast
```

实体修改、event 和同次 Wave VCS commit 要么一起提交，要么一起回滚。Broadcast 只能发生在 commit 之后。

一次事务产生多个事实时使用批量事件入口，不能拆成多个独立提交制造中间状态。只记录事件、不修改实体的事实使用 pure-event 入口，但遵守同样的授权与 commit-before-broadcast 规则。

Raw `*_tx` helper 只用于组合进上述事务。生产代码不得直接插入 event 行，也不得先 broadcast 后补写数据库。

## Event 行

每行至少包含：

- 单调递增的 `id`，作为同步 cursor；
- typed `kind` 与 JSON payload；
- `event_version`；
- actor；
- wall-clock 时间；
- 可选 correlation；
- system/area/wave/card scope。

`id` 负责顺序，时间只用于展示和诊断。事件不对实体表建外键，因为删除事件需要比实体行活得更久。

Actor 是审计归因，不等于远程安全身份。当前本地部署由受信调用方构造 actor；对外开放 API 前必须另建认证边界，不能把 header 中的自述 actor 当安全保证。

## Scope 与订阅

Scope 决定：

- WebSocket 客户端能收到哪些事件；
- dispatcher/scheduler 等内核消费者的过滤范围；
- role gate 检查所需的领域上下文；
- Wave VCS 是否参与同次提交。

事件 kind 和 scope 是不同轴：kind 表示发生了什么，scope 表示它属于哪里。不得通过解析 payload 临时推导订阅范围。

## Replay 协议

客户端携带最后提交成功的 cursor 连接事件 WebSocket。服务端：

1. 读取连接建立时的 replay 上界；
2. 返回 `(cursor, 上界]` 的持久事件；
3. 发送 replay-complete；
4. 接入 live broadcast，并去掉已被 replay 覆盖的 envelope。

客户端只在事件通过版本校验并被 reducer 接受后推进 cursor。重复 envelope 必须幂等；cursor 倒退、数据库实例变化或协议不兼容时清空本地 cursor/cache 后重新建立快照。

以下情况服务端返回 snapshot-required，而不是提供有缺口的 replay：

- cursor 早于 retention watermark；
- warm replay 超过有界预算；
- replay/live 衔接无法证明连续；
- broadcast lag 造成事件缺口。

Cold start 可以直接取得当前快照/水位，不应因为历史事件很多而陷入 snapshot-required 重连循环。

## 版本

`SYNC_EVENT_VERSION` 描述当前 event envelope/union。新增或改变会让旧客户端误解的事件形状时，必须：

1. 更新 Rust typed event；
2. bump 版本；
3. 迁移或明确处理存量 row 的版本；
4. 重新生成前端 wire；
5. 覆盖旧版本拒绝与 cache bust。

只增加向后兼容的可选字段是否 bump，取决于旧 schema 是否仍能正确解析并保持语义；不能仅凭“JSON 能读”判断。

## 前端消费

`fe/core/events` 只负责协议解析、cursor reducer 和纯计划；`fe/web/src/systems/events` 管理 socket 生命周期；`app/events` 把已接受事件转换为 query invalidation。Feature 不直接拥有 WebSocket。

客户端把服务器查询结果视为权威快照。事件主要触发精确或宽失效，不在多个 feature 中各自维护一套长期实体副本。Snapshot-required 必须清理 cursor 和相关 cache，再重新读取服务器状态。

## 保留与恢复边界

Pruner 可以删除保留期外事件，并推进 durable watermark。删除事件不会删除当前产品状态，但会失去该窗口以前的增量 replay 和部分审计细节。

需要永久保留或可重建的内容必须落在：

- 当前实体/投影表；
- Wave VCS；
- operation/session 等专用持久状态；
- 或明确的外部备份。

事件保留、检查和回收操作见 [events-retention.md](events-retention.md)。

## 修改检查表

- 实体与事件是否在同一事务？
- 授权是否在提交前执行？
- broadcast 是否只在提交后发生？
- scope 是否由写入方明确提供？
- 重复事件是否幂等？
- retention 后客户端是否能重新取得完整快照？
- 新 event shape 是否需要版本 bump？
- 内核消费者 lag 或重启后是否有 DB sweep/reconcile 后备路径？
- 该事实若丢掉事件行，是否仍能从权威状态回答？
