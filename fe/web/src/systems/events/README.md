# Systems events

## 用途

`event-stream.ts` 冻结事件流资源的 typestate 和 driver port。未配置态先注册 handler，再用一次原子 `configure({ syncEventVersion, topics })` 得到唯一的配置后 handle；只有该 handle 暴露 `start/stop`。当前 slice 只冻结接口与生命周期语义，不提供浏览器 transport。

## 契约

- `on` 与 `onConnectionState` 只属于 `UnconfiguredEventStream`，handler 因而在 configure/start 前已经就位，不会漏掉第一帧；连接状态注册同步收到当前 `disconnected` 快照。
- `configure()` 只冻结 version/topics 并创建 handle，绝不调用 driver `start`，因为兼容性裁决完成前不得连接（INV-APP-021）。
- 同一实例用相同 version 和有序 topics 重复 configure 时幂等返回同一 handle；任何不同配置抛 `TypeError`，因为一个资源不能悄悄分叉协议天花板或订阅集合。
- 生产集成中 `app/events-glue/EventBridge` 是共享流唯一 `start()` owner（INV-APP-020）。typestate 无法证明调用点唯一，后续 app slice 必须用 architecture contract test 锁住。
- `EventBridge` 必须挂在 `ServerCompatGate` 内（INV-APP-001）。typestate 无法表达 React 树父子关系，后续 `app/events-glue` contract test 负责锁住。
- 测试可用 `EventStream.forTest(url, driver).configure(...).start()` 绕过 bridge，保留可连接逃生口；仍不得跳过配置，因为 INV-APP-105 已由 typestate 明确取代。

## 故意不做

- 不实现真实 WebSocket、指数退避、unauthorized probe、browser cursor store 或 idle batching；这些是后续 platform adapter，不属于接口冻结。
- 不实现 `EventBridge.tsx`、React connection hook 或 QueryClient adapter；它们属于后续 `app/events` slice。
- 不迁移 INV-APP-019 的运行时调用排序守卫；configure 已把 set/subscribe/start 的前三步坍缩为类型上不可倒置的一步。
- 不迁移 INV-APP-105 的“未配置也能 start”；保留它会重新打开 typestate 正在封死的缺口。
