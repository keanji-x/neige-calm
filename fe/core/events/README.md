# Core events

## 用途

`protocol.ts` 负责把未知输入分成普通事件、坏事件与两种 control frame；`reducer.ts` 把帧和 cursor/version 状态纯归约为端侧 effect；`invalidation-plan.ts` 为每个 `WireEvent` 生成与 TanStack 无关的 query-key/cache 计划。这样 cursor 推进、control frame 和 version gate 可直接在 Node 中测试，不需要 React、QueryClient 或假 WebSocket。

## 契约

- `_id` 与 `eventVersion` 是 envelope metadata，不得塞进 `WireEvent`，因为 wire payload 仍由 `core/api` 的 schema 冻结。
- future-version 帧不得推进 cursor；in-range 坏帧必须推进 cursor，因为前者要留给未来客户端重播，后者不能永久钉住重连窗口。
- `_replay_complete` 宣告 replay 收敛；tip 回退时必须清 cursor/cache 并重连。`_snapshot_required` 同样清 cursor/cache 并重连，因为旧 cursor 已不可服务。
- effect 只描述 `persist-cursor`、`invalidate`、`remove`、`write-through`、`clear-cache`、`reconnect`；端侧决定如何执行，确保 core 平台无关。
- invalidation policy 对 `WireEvent['ev']` 类型级穷尽；确实无 cache 行为的事件必须显式 `noop('reason')`，因为空对象无法区分“评估过”和“忘了”。
- effect 顺序固定为 cursor 持久化、write-through、invalidate、remove，因为直写必须先于 refetch，而删除必须晚于失效。
- 出站订阅帧总是包含 `since`；没有持久 cursor 的冷启动用 `0`，明确请求从日志起点重放（INV-APP-049）。
- 运行时遇到当前类型联合之外的未知 `ev` 静默返回空计划；类型穷尽负责本版本开发约束，早退负责跨版本 wire 输入（INV-APP-026）。

## 故意不做

- 不 import 或调用 QueryClient，也不实现 query-invalidation adapter；那属于后续 `app/events`，以免纯计划与某个缓存库耦合。
- 不连接 WebSocket、不访问 storage、不调定时器；真实 transport、cursor batching 与持久化属于端侧，cursor key 继续复用 `core/keys` 的 `SYNC_CURSOR_KEY`。
- 不检测已打开 socket 中途发生的服务端日志重置（INV-APP-043）；协议没有就地信号，只在新连接的 `_replay_complete` 检测，避免制造不可验证的猜测。
- 不 debounce 或抑制 card invalidation（INV-APP-027）；card 创建已原子化，补回窗口会重新暴露陈旧状态。
- `replace-existing-cove` 只描述“替换已存在项”；执行 adapter 必须拒绝借此创建 phantom cove，后续 `app/events` contract test 锁 INV-APP-031。
