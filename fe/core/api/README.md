# core/api

## 用途

这里冻结平台无关的 HTTP 契约、认证 schema、事件 wire schema、错误归一化与 unauthorized 通知端口。消费者按语义直接导入 `types.ts`、`client.ts`、`auth.ts`、`session.ts`、`unauthorized.ts` 或 `schemas.ts`；不设 barrel，原因是让依赖和 owner 从路径直接可见。

## 契约

所有请求都由端侧实现 `ApiTransportPort`，并显式使用 `credentials: 'include'`，因为会话 cookie 是认证边界。端侧 adapter 必须逐字段转发 core 给出的 `ApiRequest`，不得重新构造为裸 `fetch(req.path)` 而丢失 credentials、headers 或 body。`performApiRequest` 每次只发一次请求，并把成功或失败返回为 `status` 判别联合，因为它必须与 `core/state` 的 `StorageReadResult` 使用同一种“失败即数据”表示法。204 空响应由 `z.void()` 显式表达，client 会以 `undefined` 解码。401 单独归为 `unauthorized`；网络错误、非 401 HTTP 错误和响应解码错误保持不同 kind，因为只有 401 可以触发登出语义。需要全局登出语义的端侧接收者必须把 `UnauthorizedChannel` 注入 `performApiRequest`，core 在 401 归一化后调用 `notify()`，但不负责路由或缓存清理。会话探测保留 `unknown / authed / unauthed / error` 四态，因为首帧不能提前猜测登录状态。

当前 `generated/wire.ts` 由 Rust wire model 的 ts-rs 导出，`generated/openapi.json` 由 workspace 的 `emit-openapi` 生成；在 `fe/` 运行 `npm run gen:api` 会刷新两者。`schemas.ts` 用双向类型测试与 wire 类型对齐，CI 则通过真实生成器检查产物 freshness。`generated/` 仍接受全部 ESLint 架构约束，只针对 ts-rs 原样生成的 `unknown | null` 关闭冗余联合类型规则；任何手工修改生成物都是违规。schema 与手写 transport port 仍由 `core/api` owner 维护。生成器只能替换 `generated/`，原因是避免覆盖手写错误与生命周期语义。

`ApiDecodeFailure` 与 `core/state` 的 `DecodeFailure` 分属两个平级 slice，跨 slice复用会制造不必要的 owner 耦合，因此有意保留两份声明；类型闸门要求两者始终同形。`WireEventDecodeResult` 则直接复用 `ApiDecodeFailure`。

## 故意不做

不实现 `fetch`、重试、路由跳转、登录页、React Query 或缓存清理，因为它们是端侧组装责任；尤其 core 不会把非 401 故障解释成登出。CAP-APP-063 的契约是“非 401 不得跳 LoginPage”而非整条不实现；本 slice 不落地页面行为，是因为其 owner 是 `features/auth/session`。不在本 PR 搭建 OpenAPI/ts-rs 生成命令，因为生成链由后续 Rust wire owner 接入；契约测试先固定生成物将来必须满足的类型面。

已知冻结面缺口：`ApiOperation` 尚无 `headers` 字段，因此还不能表达 `X-Calm-Actor` 等逐 operation header；由后续契约变更处理。`wireEventSchema` 仍裸导出，消费者可以绕过 `decodeWireEvent` 调用 `.parse()` 并抛异常；端侧 events owner 落地 GATE-WIRE-006 时必须统一走 `decodeWireEvent` 的 log-and-skip 路径。不实现重放默认值之外的迁移或恢复卡片，因为它们分别属于 events/app/features owner。
