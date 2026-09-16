> 归档说明：第一次 Codex CLI 评审超时，未形成结论；本文件为新会话重试产生的有效独立评审。未执行实际 App 或设备验收。以下原文保留。

**REQUEST_CHANGES**

离线首屏方向可成立：本地资源拦截不依赖服务器响应，设计也要求封闭代理安装完成后才导航；未发现这部分的架构阻塞。以下三处需要补齐边界合同，不代表已证明存在漏洞。

1. **热恢复缺少明确的写入与旧响应拦截点。** 当前 [queries.ts:101](/tmp/neige-recovery-baseline/fe/web/src/app/providers/queries.ts:101) 仅按 `onlineManager` 拒写，[transport.ts:22](/tmp/neige-recovery-baseline/fe/web/src/app/providers/transport.ts:22) 直接发送请求。保留已挂载页面时，“暂停写入口”还不足以指定权限边界。最小修订：明确业务请求发送前统一检查恢复许可；响应及其 401 广播按会话代际过滤；定义 401、重新登录时的代际切换，以及鉴权探测、logout、配对请求的放行规则。

2. **离线 logout 标记与重新配对尚未闭环。** 设计禁止标记存在时自动 whoami，但没有定义谁在何时解除标记。现有 [redeem:155](/tmp/neige-recovery-baseline/crates/calm-server/src/mobile_access/routes.rs:155) 仅签发 cookie；而 S1 承诺不依赖服务器升级。最小修订：指定标记的 owner、持久 scope，并给出兼容旧服务器的用户主动重新验证流程；验证成功才清标记，残留 cookie 或旧异步响应不得解除退出状态。

3. **可配置 target 的 TLS 与 peer 关系仍未定义。** 当前 [main.go:27](/tmp/neige-recovery-baseline/mobile/p2p-native/main.go:27) 分别固定 HTTPS hostname 与 Tailnet IP，`:113` 将前者限定映射到后者。改成二维码配置后，仅“同一配置生成”不能说明两者如何核验。最小修订：明确 HTTPS hostname 同时作为 origin、SNI 和证书校验名；规定通过受信 Tailnet 节点信息解析、核对目标 peer，字段不一致或目标变化时拒绝连接并重新确认。

服务端已规定固定受限 upstream、独立监督、内核重启后沿原地址恢复及单 writer 升级回滚约束；本次未发现需要额外阻塞的架构矛盾。以上仅为静态设计评审，未运行测试、联网或修改文件。