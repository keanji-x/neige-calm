# App layer

## 放什么

Router、providers、theme、shell、依赖注入，以及 EventBridge 和 Query invalidation 等 React/TanStack 组装胶水。

## 不放什么

不放可下沉的业务规则、system 生命周期实现、UI primitive 或 core 纯逻辑；卡片创建不得藏在路由。

## 依赖方向

`app` 可依赖所有更低层并连接跨域行为；下层任何代码都不得反向 import `app`。

## 契约模板

组装点应列出所注入的接口、owner、初始化/清理顺序和 contract test；冻结接口不足时走 change request。
