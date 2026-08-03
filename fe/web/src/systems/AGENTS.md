# Systems layer

## 放什么

具备独立资源生命周期、协议或宿主能力的 cards、terminal、wheel、fs-viewers、editor、events 平台实现。

## 不放什么

不放纯页面行为、app 组装、跨端纯协议或无资源生命周期的 UI primitive。

## 依赖方向

只可依赖 `core` 与 `ui`，不得依赖 `features` 或 `app`。Cards 只能通过 `systems/cards/public.ts` 被消费。

## 契约模板

Public entry 明列 lifecycle、host、port、ownership 与清理语义；内部文件不对外深导入，变更冻结面先发 change request。
