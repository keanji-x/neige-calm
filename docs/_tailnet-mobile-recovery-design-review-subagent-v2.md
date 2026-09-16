# Fresh design review A — v2

**APPROVE**

只读复核基线：`c442f3440dc5ab819eb82eca0c43aeec8a29429d`。文档：`docs/architecture/tailnet-mobile-recovery.md`，SHA-256 `1cd96e962f6036a8452e76732b5a5651fa2cb80ed5f505cd4775476589fbb452`。已读完整 v2 与两路 v1 报告；未发现需要再次修订设计的阻塞架构矛盾。

## 旧发现的裁决

1. **A-1 / B-1 已闭合。** §5/6 已明确排队前准入、不可换代许可、发送前复核、旧意图拒绝和响应/401 代际检查。它们覆盖实际存在的共享 transport（`fe/web/src/app/auth/production-app.tsx:46`）、默认 mutation（`providers/queries.ts:772`）、serial writer（同文件 `:344`）以及独立终端 sender/pending frames（`systems/terminal/xterm-view.tsx:579`）。`fe/core/api/client.ts:54` 的 401 广播位于 transport 返回之后，说明代际检查必须落实到结果消费边界；v2 已明确这一合同，无需设计阶段列出所有函数。

2. **A-2 / B-2 已闭合，并确认旧 A 的源码判断错误。** `fe/core/api/auth.ts:9` 要求 `sessionId`；`crates/calm-server/src/auth.rs:334,343` 定义并返回该值，login 也实际返回新 session。`mobile_access/routes.rs:155` 配对签发 cookie，旧 `pair.js:39-41` 只跳 `/next/`。因此 v2 的 origin 隔离拒绝标记、仅存摘要、明确点击后单次 whoami、新 session 比较、成功清标记后继续版本 gate，可兼容现有服务器和旧配对页。取消/超时/进程死亡不自动解锁，标记写失败不宣称持久退出；普通退出 App 无新增确认。无需新增 native bridge 或服务器持久 session。

3. **A-3 / B-3 已闭合。** §7 已用受信节点信息绑定 DNSName、稳定节点 ID、地址和端口，并规定 SNI/证书、地址类别、DNS 固定拨号与跨 origin redirect 的拒绝规则。现有 `mobile/p2p-native/main.go:113` 的固定 hostname→peer dial 是可替换边界；`:125-140` 已使用本节点 `LocalClient().Status()`。`direct.go:143,166` 的系统解析拨号确实需要按新合同替换，不能仅参数化常量。具体锁定版 SDK 的 peer 字段映射留在 S4 实现检查；此次尝试读取的本地模块缓存路径不存在，未把 SDK 字段支持冒充已验证事实。

## 全文复核结论

冷恢复的无正文原页面结构、热恢复的原内存页面、右上角后台恢复状态符合用户目标。原生资源读取（`BundledFrontendAssets.kt:39-57`）独立于服务网络；当前先探测再 install 的顺序（`BundledFrontendPlugin.kt:171-198`）确需重排，但未构成不能实施的约束。拆分封闭 loopback listener 与 tsnet 启动、保留现有 Wry 权限记账已有明确设计要求。

唯一身份 owner 与唯一事件 owner 的职责可成立；§5 明确让 driver 复用身份 owner，覆盖当前 `fe/web/src/app/composition.ts:45-51` 的独立 whoami 探测。S3 使用受限入口也有现成边界：`crates/calm-server/src/routes/application.rs:28` 排除 worker hooks、管理及密码登录。S1/S2 首个交付与 S3/S4 独立服务切片没有互相阻塞或被暗中取消。

**最小必要修订：无。** 后续实际 diff 重点验证排队前许可传递、迟到结果/401、退出后真实配对以及原生离线首帧；这些是执行与验收责任，不是要求开启新设计轮次。未运行测试、App、设备验收或网络调用，未修改源码或设计；本结论仅批准设计进入已规定的实施流程。
