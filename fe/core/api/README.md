# core/api

## 用途

这里冻结平台无关的 HTTP 契约、认证 schema、事件 wire schema、错误归一化与 unauthorized 通知端口。消费者按语义直接导入 `types.ts`、`client.ts`、`auth.ts`、`session.ts`、`unauthorized.ts` 或 `schemas.ts`；不设 barrel，原因是让依赖和 owner 从路径直接可见。

## 契约

所有请求都由端侧实现 `ApiTransportPort`，并显式使用 `credentials: 'include'`，因为会话 cookie 是认证边界。`performApiRequest` 每次只发一次请求，并把成功或失败返回为 `status` 判别联合，因为它必须与 `core/state` 的 `StorageReadResult` 使用同一种“失败即数据”表示法。401 单独归为 `unauthorized`；网络错误、非 401 HTTP 错误和响应解码错误保持不同 kind，因为只有 401 可以触发登出语义。会话探测保留 `unknown / authed / unauthed / error` 四态，因为首帧不能提前猜测登录状态。

当前 `generated/wire.ts` 是已冻结的 ts-rs 类型快照，`schemas.ts` 用双向类型测试与之对齐。未来生成链由 Rust wire model owner 维护，从 workspace 的 OpenAPI/ts-rs 任务写入 `core/api/generated/`；schema 与手写 transport port 仍由 `core/api` owner 维护。生成器只能替换 `generated/`，原因是避免覆盖手写错误与生命周期语义。

## 故意不做

不实现 `fetch`、重试、路由跳转、登录页、React Query 或缓存清理，因为它们是端侧组装责任；尤其 core 不会把非 401 故障解释成登出。不在本 PR 搭建 OpenAPI/ts-rs 生成命令，因为生成链由后续 Rust wire owner 接入；契约测试先固定生成物将来必须满足的类型面。不实现事件 dispatch 的 log-and-skip、重放默认值之外的迁移或恢复卡片，因为它们分别属于 events/app/features owner；oracle 的 `intentional_omission` 不应在这里被“顺手补全”。
