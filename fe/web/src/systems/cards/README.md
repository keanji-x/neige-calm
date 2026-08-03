# Cards system

## 用途

`public.ts` 是 registry、kind resolver、host capability 与资源生命周期的唯一公开入口。它只冻结卡片系统的组装协议；具体 terminal、codex、spec、iframe、file-viewer 与 wave-report 实现不属于本模块阶段。这样做是因为卡片有独立资源生命周期，而页面布局属于 feature。

## 契约

- `CardDataMap` 只允许由 `systems/cards/**` 内的卡片模块通过 type-only `declare module './registry.js'` 合并，`public.ts` 重导出合并后的 `CardDataMap` 与 `RegisteredCard`。这是新增 kind 进入可辨识联合并保持穷尽检查的唯一机制；depcruise 不检查声明合并的深路径逃逸，因此这也是书面边界。
- `createCardRegistry()` 返回 app 所拥有的 registry；重复 type 覆盖，exact claim 优先于最长 prefix claim，最后按显式注册顺序全扫其余 adapter。全扫保留是为了兼容共用 kernel kind 的 codex/spec。
- app 必须通过 `app/cards.ts` 的单行注册序列组装 built-ins。顺序是 terminal → codex → spec → claude → wave-report → file-viewer → iframe → plugin-iframe，因为 codex/spec 的兜底命中依赖插入顺序。
- card 只能读取冻结 lifecycle snapshot、订阅变化、使用 instance slots、发送 runtime command；宿主独占 visibility/focus/geometry writer。这样卡片不能伪造宿主观测状态。
- `setVisible(false)` 只发布生命周期变化，绝不卸载。只有显式 `unmount()` 才注销 resolver、退订 controller 并 dispose 一次，因为离开视口卸载会丢 PTY 或 iframe 会话。
- controller 生命周期回调的 Promise rejection 不传播给宿主调用方，也不阻断后续回调投递；宿主通过 `onControllerError(error, { cardId, callback })` 统一观察，未配置时降级为 `console.error`。
- 默认 snapshot 是 visible=true、focused=false、geometry=0/0/not-ready、refreshEpoch=0；相同状态不通知，而每个 refresh 命令都推进 epoch。默认可见让无 observer 环境仍能工作。
- resolver 注销会比对当前 instance identity，旧挂载的 cleanup 不能删除快速重挂产生的新 instance。这样 StrictMode 与竞态 cleanup 不会击穿宿主路由。
- entry 的 `wheelTarget` 冻结为接收 card 与只含 `cardId`/`slots` 的 instance，并返回 xterm ref、native-scroll ref、sink 或 null；具体路由与卡片壳接线留给后续 slice。

## 故意不做

- 不让 registry 顺序变得无关紧要；顺序是 `INV-CARD-225` 的业务语义，而非需要“优化掉”的副作用。
- 不保留 `INV-CARD-224` 的模块级 `registerBuiltins` 一次性守卫；registry 已改为 app 持有的实例，重复 type 覆盖是实例级幂等语义，不再需要跨实例共享 boot 状态。
- 不因不可见而卸载，也不向 card 暴露 lifecycle writer；这两处缺口分别保护资源会话与宿主真相源。
- 不在卡片容器上承诺 `role="region"`；逐卡 landmark 会制造冗余播报。`INV-A11Y-037` 的机器锁由后续卡片壳 slice 在壳组件落地时兑现，并在引入 titled-landmark 方案时重新评估。
- 不实现 card head、visibility observer、slots 的 React hook、具体 card adapter、overlay、schema version 或 iframe/plugin 行为；这些应在后续实现 slice 兑现各自 browser/jsdom 契约。
- 不提供 barrel `index.ts` 或任何私有深导入承诺；消费者缺少能力时应对 `public.ts` 发 change request。
