# 独立设计评审 A：扫码完成手机入网与 Neige 配对

结论：**REQUEST_CHANGES**。只有 1 项必要修订；无需重开主恢复设计或扩大设备管理范围。

审查对象：`750f61579e8742d57b1e5a60abf884d80f41f417`，`docs/architecture/1712-scan-only-enrollment.md` 共 165 行，SHA-256 `7272c7bd5640940dd3c5581c36a76e95854bd8f6dfd899fa65bd7f48ddec70a8`。独立工作树 `/tmp/neige-scan-review-a`，只读核查设计及相关生产源码，未联网、未读取账户密钥、未运行测试、未修改仓库。

## 1. [P1] 明确 pair 页面到 bundled gate 的原生一次性许可交接，避免要求一个不可观测的实时条件

**设计位置：** §4 L69、L85；§5 L102、L108–111；§6 L123–129。

设计只规定 native 向本次顶层配对页一次性注入 ticket/attempt 信息，然后 pair.js 将 receipt 写入 sessionStorage 并导航 `/next/`。但新文档的 gate 又必须确认“仍有效的 native scan generation”。前一个远端文档的 JS 内存随导航销毁；receipt 的字段也没有携带原生许可，更不能证明 native 当前代际。现有 launcher-only IPC 不允许新文档读取 native 状态，因此这项检查目前没有定义可实现的数据来源及撤销方式。把 generation 数字放到 sessionStorage 也不能解决“当前仍有效”，反而会把内存意图变成重开可恢复的许可。另需说明正常 `pair → /next/` 是允许的交接，否则 L85 的“离开失效”会阻断正常成功路径。

**源码证据：**

- `mobile/src-tauri/capabilities/launcher-bundled-frontend.json:5–8` 仅授权 packaged launcher；`BundledFrontendPlugin.kt:76–84` 再校验当前页面为 `tauri.localhost`。不能默认工作区可调用原生代际查询。
- `mobile/src-tauri/gen/android/app/src/main/java/io/neigecalm/next/BundledWebViewClient.kt:29–40` 仅转发导航/页面回调，没有面向下一个文档的扫码许可交接。
- `mobile/src-tauri/gen/android/app/src/main/java/io/neigecalm/next/BundledFrontendAssets.kt:39–57` 提供静态 APK 页面资源，没有每文档许可注入。
- `crates/calm-server/src/mobile_access/pair.js:40–41` 成功后使用 `location.replace('/next/')`，因此并非同一 JS 文档内的状态切换。
- `fe/web/src/app/auth/session-gate.tsx:33–60` 现有 gate 输入不含 native lifecycle；新设计必须把这项新增 authority-boundary 合同说清楚，不能作为已有能力引用。

**最小修订：** 保留 launcher-only IPC，明确新增一个窄的、原生主动提供的 bundled gate 文档许可，无需远端可调用 bridge。约定原生仅在本次已验证的 pair 顶层文档向同 origin `/next/` 的单次预期导航中，复核 generation、origin、attempt 后，在 gate 启动前供给不可持久化的一次性上下文；普通导航/刷新/重开不得供给。说明该允许跳转如何消耗配对页许可，并指定取消/超时/新扫描如何关闭或替换该文档及其请求许可，使已复制进 FE 内存的上下文无法继续生效。主设计“无 native bridge”的例外范围写明为此单向交接，不增加任何远端原生权限。也可以选择同一 bundled 文档完成 v2 配对与 gate 来消除跨文档交接，但不必同时支持两套方式。

验收只补三条真实路径：正常 pair→next 自动通过；取消恰逢跳转时不解锁；已有 receipt 的刷新/进程重开不取得新许可。无需扩大到通用消息桥或设备平台。

## 已接受的设计判断

- 未入网 bootstrap 没有私有 origin 循环依赖：auth key 先与 Tailscale 控制面完成非交互授权，再按当前可信 map/TLS 访问 Neige。复用唯一 tsnet 目录及身份与现有引擎结构一致。
- 300 秒真实云端寿命尚未验证，设计明确可能导致功能不可发布，并要求验证真实返回值与未知签发结果；没有把本地 QR 倒计时伪装成云端撤销。此限制可以接受为明确发布前置条件，不是本轮继续加固理由。
- 长期 OAuth secret 留 host；PasswordLogin 来源明确标为待实现的新边界；v1 不自动升级批准语义。
- 同 attempt 重兑只能重发同一仍有效 session；receipt 绑定新 session cookie，且 whoami 指纹复核；Tailscale key 和 Neige grant 非原子、取消后节点仍保留均诚实描述。
- 已运行旧身份只在当前可信 map 能验证目标时复用；不隐式登出或跨 Tailnet 换身份。正式目标继续要求稳定 peer ID、受控地址和拨号前重验，未放松既有 target.go 边界。

上述判断为设计评审结论，不代表实现、云端政策或 Android 真机链路已验证。
