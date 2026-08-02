# Core layer

## 放什么

平台无关的 API 契约、domain 纯逻辑、zod schemas、持久化 keys/ports、无 React state codec、tokens、markdown AST policy，以及纯 events protocol/reducer/plan。

## 不放什么

不得放 React、JSX、DOM/browser API、fetch/WebSocket/localStorage 的直接调用、组件或端侧 renderer。

## 依赖方向

`core` 是最底层，不依赖 `ui`、`systems`、`features` 或 `app`，也不依赖 React 运行时。

## 契约模板

导出窄而具名的类型/纯函数/port；说明输入、输出、错误通道和不变量。冻结后变更须发 change request，不得新增 `index.ts` barrel。
