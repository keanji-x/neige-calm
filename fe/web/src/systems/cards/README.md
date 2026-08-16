# Cards system

## 用途

`public.ts` 是 registry、kind resolver、host capability、资源生命周期**与内置卡片组合**的唯一公开入口。built-in 的集合、注册顺序与 kernel 解析适配器都归本模块所有，实现落在 `builtins/`（#1091）：卡片有独立资源生命周期，只有本模块能保证解析顺序与生命周期语义一致；页面布局仍属于 feature。terminal、codex、claude、iframe、file-viewer、plugin-iframe 的 entry 尚未落地，届时加入同一个 `builtins/` 组合，不另开注册路径。

## 契约

- `CardDataMap` 只允许由 `systems/cards/**` 内的卡片模块通过 type-only `declare module './registry.js'` 合并，`public.ts` 重导出合并后的 `CardDataMap` 与 `RegisteredCard`。这是新增 kind 进入可辨识联合并保持穷尽检查的唯一机制；depcruise 不检查声明合并的深路径逃逸，因此这也是书面边界。
- `createCardRegistry()` 返回 app 所拥有的 registry；重复 type 覆盖，exact claim 优先于最长 prefix claim，最后按显式注册顺序全扫其余 adapter。全扫保留是为了兼容共用 kernel kind 的 codex/spec。
- 注册顺序的唯一权威是 `builtins/register.ts` 的 `BUILTIN_CARD_ORDER`：terminal → codex → spec → claude → wave-report → file-viewer → iframe → plugin-iframe，因为 codex/spec 的兜底命中依赖插入顺序。`registerAvailableBuiltinCards(registry)` 按该 tuple 遍历，只注册**已落地**的 entry 并跳过其余项——不造占位/no-op/unknown entry，未落地卡型的 kernel card 落到 unknown slot。app 侧只剩 `app/cards.ts` 的薄 wrapper `bootCards(registry)`：不接收 entry 参数、不持顺序表、不存模块状态，只调用一次组合函数。
- 「无头」是 entry 自己的声明：`CardEntry.headless?: boolean`（由 `builtins/headless-filter.ts` 通过 type-only `declare module './registry.js'` 合并，与 `createController`/`wheelTarget` 同一约定），写在各 entry 的 `component`/`defaultSize` 旁边。不存在与之平行的类型名清单，也不靠执行 component 反推——后者在 React renderer 之外会让任何用 Hook 的 entry 抛异常。接口上可选而非必填，因为冻结的 `public.test.ts` 与 `public.contract.test.ts` 都构造不含该字段的 entry 并真的 `register()`；缺省即「有面」。built-in 不吃这个缺省：`builtins/register.ts` 只经一个把 `headless: boolean` 收成**必填**的 helper 注册，漏写即 typecheck 错误（冻结文件不经过它）。它只挡漏写；写错方向（有面的卡声明 `true`）由 `builtins/register.contract.test.ts` 对生产 registry 的逐 entry 断言机器锁住，两层各挡一半。
- `partitionWaveCards(registry, cards)` 是 `INV-CARD-226` 的实现：先按未过滤数组绑定 `originalIndex`，再按 `registry.get(card.type)?.headless` 剔除已解析的无头卡，unknown 分支另按原始 `kind === 'wave-report'` 防御过滤。它不排序；排序与 tie-break 归 grid。
- card 只能读取冻结 lifecycle snapshot、订阅变化、使用 instance slots、发送 runtime command；宿主独占 visibility/focus/geometry writer。这样卡片不能伪造宿主观测状态。
- `setVisible(false)` 只发布生命周期变化，绝不卸载。只有显式 `unmount()` 才注销 resolver、退订 controller 并 dispose 一次，因为离开视口卸载会丢 PTY 或 iframe 会话。
- controller 的同步异常与异步 Promise rejection（包括 `dispose`）都通过 `onControllerError(error, { cardId, callback })` 路由，既不传播给宿主调用方，也不中断宿主向其它回调或其它 controller 投递；未配置时降级为 `console.error`。
- 默认 snapshot 是 visible=true、focused=false、geometry=0/0/not-ready、refreshEpoch=0；相同状态不通知，而每个 refresh 命令都推进 epoch。默认可见让无 observer 环境仍能工作。
- resolver 注销会比对当前 instance identity，旧挂载的 cleanup 不能删除快速重挂产生的新 instance。这样 StrictMode 与竞态 cleanup 不会击穿宿主路由。
- entry 的 `wheelTarget` 冻结为接收 card 与只含 `cardId`/`slots` 的 instance，并返回 xterm ref、native-scroll ref、sink 或 null；具体路由与卡片壳接线留给后续 slice。

## 故意不做

- 不让 registry 顺序变得无关紧要；顺序是 `INV-CARD-225` 的业务语义，而非需要“优化掉”的副作用。
- 不保留 `INV-CARD-224` 的模块级 `registerBuiltins` 一次性守卫；registry 已改为 app 持有的实例，重复 type 覆盖是实例级幂等语义，不再需要跨实例共享 boot 状态。
- 不因不可见而卸载，也不向 card 暴露 lifecycle writer；这两处缺口分别保护资源会话与宿主真相源。
- 不在卡片容器上承诺 `role="region"`；逐卡 landmark 会制造冗余播报。`INV-A11Y-037` 的机器锁由后续卡片壳 slice 在壳组件落地时兑现，并在引入 titled-landmark 方案时重新评估。
- 允许（并要求由本模块承载）：`builtins/` 下的 spec、wave-report 无头 adapter 及后续真实 card adapter、card head、geometry/visibility observer、overlay，以及 `ui/board-host.tsx` 里的薄 React bridge。这些都是卡片系统自己的壳与观测面，放到 feature 会让资源生命周期和布局同时改写同一份状态。它们仍按 slice 落地，各自兑现 browser/jsdom 契约。
- 仍不实现：slots 的 React hook、schema version、iframe/plugin 行为。
- 不提供 barrel `index.ts` 或任何私有深导入承诺；`builtins/` 也不例外，其组合函数、`BUILTIN_CARD_ORDER`、`partitionWaveCards` 与 `isSpecHarnessPayload` 全部经 `public.ts` re-export（`cards-public-entry-only` 禁止外部直引子路径）。消费者缺少能力时应对 `public.ts` 发 change request。
