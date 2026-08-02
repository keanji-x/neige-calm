# Systems events

## 用途

`event-stream.ts` 冻结事件流资源的 typestate 和 driver port。未配置态先注册 handler，再用一次原子 `configure({ syncEventVersion, topics })` 得到唯一的配置后 handle；只有该 handle 暴露 `start/stop`。当前 slice 只冻结接口与生命周期语义，不提供浏览器 transport。

## 契约

- `on`、`onFrame` 与 `onConnectionState` 只属于 `UnconfiguredEventStream`，handler 因而在 configure/start 前已经就位，不会漏掉第一帧；连接状态注册同步收到当前 `disconnected` 快照。`onFrame` 投递普通事件、坏事件和两种 control frame，供端侧把完整协议交给 core reducer；`on` 保留为普通 `WireEvent` 的便捷通道。
- configure 之后才通过仍持有的未配置引用注册 handler，会漏掉此前已同步投递的帧；这是有意的标准 pub/sub 语义，不重放历史帧，也不视为数据丢失。
- `configure()` 只冻结 version/topics 并创建 handle，绝不调用 driver `start`，因为兼容性裁决完成前不得连接（INV-APP-021）。
- 同一实例用相同 version 和有序 topics 重复 configure 时幂等返回同一 handle；任何不同配置抛 `TypeError`，因为一个资源不能悄悄分叉协议天花板或订阅集合。
- 生产集成中 `app/events-glue/EventBridge` 是共享流唯一 `start()` owner（INV-APP-020）。当前 handle 的 `start()` 幂等，为后续 app slice 的唯一调用点 architecture contract test 提供行为锚点。
- `EventBridge` 必须挂在 `ServerCompatGate` 内（INV-APP-001）。typestate 无法表达 React 树父子关系，后续 `app/events-glue` contract test 负责锁住。
- platform driver 是 `EventSubscriptionFrame` / `eventSubscriptionFrame` 出站义务的接收者：每次连接都必须用当前 topics 与 cursor 构造必带 `since` 的订阅帧；systems port 只传入原始配置与 URL，不重复冻结 transport 编码。

## 故意不做

- 不实现真实 WebSocket、指数退避、unauthorized probe、browser cursor store 或 idle batching；这些是后续 platform adapter，不属于接口冻结。
- 不实现 `EventBridge.tsx`、React connection hook 或 QueryClient adapter；它们属于后续 `app/events` slice。
- 不迁移 INV-APP-019 的运行时调用排序守卫；configure 已把 set/subscribe/start 的前三步坍缩为类型上不可倒置的一步。
- 不迁移 INV-APP-105 的“未配置也能 start”；保留它会重新打开 typestate 正在封死的缺口。
