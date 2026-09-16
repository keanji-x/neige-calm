# 扫码设计 v3 独立窄复核 A

**APPROVE**。旧唯一 P1 已闭合；全文未发现新的必要设计阻塞。

对象：HEAD `7a8563a544c665b61eab0a7e20cfa122e040ad1a`，`docs/architecture/1712-scan-only-enrollment.md`（184 行）。已阅读 v1 两路报告并核对相关生产源码；未改仓库、运行测试、联网或派生 agent。

- **许可来源闭合。** 设计 L112–116 明确由 native 向唯一 APK 顶层文档一次交付 context，claim、redeem 与 gate 均在该文档内，后续仅 SPA 路由；刷新、历史恢复与重开不得重发。不再要求跨 `pair.js:41` 的 `location.replace('/next/')` 保存 JS 内存或从无授权 IPC 查询 native 代际。`BundledFrontendAssets.kt:39–57` 已是 APK HTML 响应的生产接入位置，新增有界启动数据与 nonce CSP 的职责清楚；不将这些新增能力冒充现有实现。
- **不再依赖不可观察的 TLS 内容。** 设计 L116–121 将协议顺序与迟到结果判定交给 FE，native 只隔离目标、实例与 socket，并以关闭连接和销毁 WebView 完成原生取消。这符合 `mobile/p2p-native/main.go:265–295` 的 CONNECT/`io.Copy` 边界，也避免以现有 `BundledWebViewClient.kt:31–40` 的普通页面回调推断配对响应。已到服务端的操作及已接受成功不承诺撤回，边界可实施。
- **删除 receipt 后仍有明确的新 cookie 证明。** 设计 L129–140 保留同 attempt 幂等重兑、一次 whoami、redeem session 指纹与实际 cookie 的 sessionId 指纹相等及退出旧指纹不等；所有结果先复核代际，成功才清退出标记并进入 version/scope。与现有 `fe/web/src/app/auth/session-gate.tsx:1–6,42–60` 的身份 gate 先于版本及 router、abort/epoch 采纳模式一致，没有新增并行身份 owner。

批准仅限设计闭合。逐实例 socket 生命周期、一次启动数据供给、同文档 cookie 与取消竞态应按文档既定验收在实现 diff 中验证；本轮未将未实现内容或真实设备行为宣称为已通过。已接受的 issuer、短 TTL、来源边界及 tag policy 判断不重开。
