# ui/state

## 用途

提供组件使用 React `useState` / `useReducer` 的受控公开入口。它位于 UI 层，因为 React 是端侧能力，而 systems、features、app 都能沿单向依赖合法消费 UI。

## 契约

普通状态保持 React 19 的调用形状，因为迁移不应改变 setter/dispatch 体验。状态整体满足 `Persistent<unknown>` 时返回类型塌成 `never`，因为持久值落入组件内存会在重载时静默丢失；条件用单元素元组包裹，因为联合类型不能发生分配。运行时只转发 React 原 hook，因为 brand 是零成本的编译期约束。

## 故意不做

这里不实现 overlay 获取、写回、loading、缓存或错误 UI，因为这些属于后续端侧组装；也不导出 React 的其它 API，避免本入口膨胀成 barrel。
