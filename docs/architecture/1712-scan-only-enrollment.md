# 扫码完成手机入网与 Neige 配对

状态：设计 v1，待两路独立评审；未实现，未使用账户凭证进行实网验收。
关联：[issue #1712](https://github.com/keanji-x/neige-calm/issues/1712)。
基线：`5f2ff75c6` 的[主设计 v2](1712-tailnet-mobile-recovery.md)，以及 `b4ed59c6f` 的 `mobile/p2p-native/target.go`。
新增硬要求：电脑 owner 发起“添加手机”后，手机启动扫码，一次正常流程完成 Tailnet 设备授权与 Neige 配对；不打开 Tailscale 登录页，不再点配对确认或验证按钮。

## 1. 范围与授权变化

本增补属于 S4，并给 S3 的本机控制通道增加一个窄 enrollment 模块；不重做 S1/S2 的恢复、退出或重连 owner。
手机启动“扫描电脑上的添加手机二维码”本身是本次入网、选择工作区及配对的明确意图；相机系统权限仍按 Android 要求申请。
电脑生成新 v2 二维码是 owner 对持码设备的一次预批准，替代 v1 扫码之后的电脑批准；界面必须在生成操作旁说明这一点。
主设计 §7 的“手机先登录同网、再确认目标、再 owner 批准”仅对本 v2 流程由本增补替代；v1 不改变审批语义。
主设计 §4.2 的旧服务器“验证本次配对”按钮继续用于 v1；新 v2 成功凭据触发自动一次核验，细节见 §6。
不增加公网 bootstrap 服务、第三方账户系统、通用设备管理平台或跨服务分布式事务。
电脑首次管理员配置允许使用 Tailscale 控制台；手机不使用 OAuth 用户同意页、系统浏览器或 `StartLoginInteractive`。
S4 必须移除手机 `loginTailscale`/`p2pLogin` 的可达调用、打开外部 Tailscale 页的 Intent 及对应 capability，不能只隐藏按钮；桌面 S3 登录保留。

## 2. 已核对事实与最小方案

| 事实 | 证据 | 约束 |
| --- | --- | --- |
| Auth key 可非交互注册，支持一次性与预批准；撤销 key 不删除已加入的节点 | [Auth keys](https://tailscale.com/docs/features/access-control/auth-keys) | QR 只携带一次性 key；持久手机使用 `ephemeral=false`；设备撤销另算 |
| OAuth client 的 `auth_keys` scope 可签发指定 tags 的 auth key | [OAuth clients](https://tailscale.com/docs/features/oauth-clients) | 长期 client secret 只在电脑；手机成为 tag-owned 节点，不冒充某个人的 Tailscale 用户身份 |
| 已登录 tsnet 并不提供 OAuth 签发权限 | 同上；`mobile/p2p-native/main.go` 的节点启动与 LocalClient 调用 | 不能仅凭“服务已连接”展示可用的添加手机二维码 |
| 锁定 SDK 可传 `expirySeconds` 并返回真实 `created`、`expires`、capabilities，可删除 key | [v1.102.3 keys.go](https://github.com/tailscale/tailscale/blob/v1.102.3/client/tailscale/keys.go) | 以实际返回值验收短有效期；不能把 QR 倒计时当云端 key 到期 |
| LocalClient `Start(ctx, ipn.Options{AuthKey: ...})` 支持非交互节点授权 | [local.go](https://github.com/tailscale/tailscale/blob/v1.102.3/client/local/local.go)、[backend.go](https://github.com/tailscale/tailscale/blob/v1.102.3/ipn/backend.go) | 由现有唯一 Go 引擎执行，有截止时间，不另建 tsnet 节点 |
| 当前扫码器仅接受固定 origin 的 v1 URL，随后有手机确认；远端页面没有原生扫码权限 | `mobile/www/scanner.js`、`pairing-url.js`、`mobile/src-tauri/capabilities/launcher-*.json` | 新增本地 v2 data parser 与窄 enrollment capability，不放开远端权限 |
| 当前 claim 生成 secret，默认未批准；redeem 才在同一锁内创建 session | `crates/calm-server/src/mobile_access/pairing.rs`、`routes.rs`、`pair.js` | 新 v2 类型单独预批准；不把现有 `approved=false` 默认值改成 true |

采用电脑签发的一次性 Tailnet auth key，加同期限内有效的 Neige 预批准邀请，通过一个本地 data QR 送达手机。
未入网手机直接持 key 联系 Tailscale 控制面；加入后才连接私有 Neige origin，因此没有必须先访问私有服务器才能取得入网资格的循环依赖。
以上源码已在本机锁定模块缓存中核对；接口存在不等于云端策略、设备审批或 Android 真机链路已验证。

## 3. 电脑 provisioning 与 key 生命周期

`neige-tailnet` enrollment 模块持有类型化配置：明确 tailnet、OAuth client ID、私有 secret 文件引用、固定 phone tags、预期节点 origin。
secret 文件属于运行 Neige 的用户，目录 0700、文件 0600；拒绝错误 owner/权限及符号链接，配置读取错误关闭签发能力。
密钥内容不进 CLI 参数、环境、普通设置返回、日志或 calm-server；子进程环境仍使用 S3 的显式白名单。
OAuth token 仅在该模块内存中按官方期限刷新；API origin 固定为官方 HTTPS，拒绝携凭证的 redirect，不从扫码或网页参数指定。
首次电脑设置说明管理员需创建 `auth_keys` scope 的 client、限制到既定 tags，并预先允许 phone tags 访问 Neige 节点的唯一 HTTPS 端口。
Neige 不自动修改整网 ACL/grants、tagOwners 或设备审批策略；tag 在其他服务上已有权限仍由 tailnet 管理员负责。
缺少 credentials、scope/tags、HTTPS/MagicDNS 或可用私有 ingress 时返回具体 `setup-required`，不展示半有效二维码。
Tailnet Lock 未配置可用的签名流程时同样阻止此功能；本切片不实现签名基础设施，也不回退手机浏览器登录。
签发、取消与 provisioning 设置仅允许当前有效的 `PasswordLogin` 管理员来源；`PairedDevice` 即使直连电脑主端口也必须拒绝。
这是 S3 待实现的必需 session 来源边界；当前 `owner()` 仅检查 session 存在，不能声称已隔离，也不能用“未出现在 pairing 记录”推断管理员。

每次 owner 创建邀请，请求 `reusable=false, preauthorized=true, ephemeral=false`，tags 必须等于固定配置；请求 key 有效期 300 秒。
检查实际返回 capabilities、非空 key ID、created/expires；要求云端实际寿命不超过 300 秒，且剩余时间足够完成流程，否则删除 key 并报告不支持。
官方文档描述控制台 1–90 天有效期；当前未实测 API 是否接受 300 秒。若拒绝或延长，扫码入网功能保持不可用，不能静默退为一天。
Neige invitation TTL 为最多 180 秒，截止点不得晚于真实 key expiry；显示两者实际截止时间，不承诺手机时钟提供安全保证。
收到 key ID 后立即记录清理元数据，再核验 capabilities/expiry；只有记录落盘、元数据核验、Neige invitation 创建均成功才一次性返回 QR。
中途失败使 invitation 无效并尽力删除 key，包括能力不符/实际 expiry 过长的 key；清理失败仍保留实际期限，落盘失败需明确报告清理结果未知。
签发 HTTP 超时属于结果未知，不盲重试；未收到真实 expiry 的 key 不得宣称 300 秒后失效，提示管理员核对；实网寿命能力未验证前不得发布签发功能。
同一电脑只允许一个未完成 v2 invitation；并发创建串行化，替换先使旧 Neige invitation 无效，再安排旧 key 撤销。
owner 取消、替换、关闭入口、邀请到期或配对成功均使旧 ticket 立即不可用，并安排尚未使用 key 的删除。
清理记录仅持久化 key ID、实际 expires、enrollment ID、状态，数量有界；重启补偿删除，已删除按成功处理，其他错误保留到云端到期。
删除失败显示“Neige 邀请已取消；入网 key 最晚在……失效”；不能声称已经撤销云端资格。
创建/取消与异步签发返回有 generation fence；已取消结果只进入清理，不能重新发布 QR 或激活 invitation。

## 4. 最小合同与状态归属

| 表面 | 新合同 |
| --- | --- |
| 本机管理 API | `POST /api/mobile/enrollments`、`DELETE /api/mobile/enrollments/{id}`；依赖 S3 显式 session 来源修复，仅允许有效 PasswordLogin，拒绝 PairedDevice/dev_autologin，仅注册于受保护本机管理入口 |
| 创建返回 | enrollment ID、QR data/image、真实 key 与 invitation 截止时间；`no-store`，正文与 SVG 不进入请求日志/遥测；未知字段拒绝 |
| host 控制协议 | enrollment create/cancel/status/cleanup；有版本、大小上限、截止时间与 generation，长期 secret 不跨此通道返回 |
| 公共配对 API | 新 `/api/mobile/enrollments/claim`、`redeem`、`receipt`；只接受 v2，均受限 body、no-store、速率/数量限制；v1 端点不能消费 v2 ticket |
| Android capability | packaged launcher 独占 `enroll-from-scan` 与 cancel/status；复用现有扫码插件，不给远端 origin、子 frame 或配对页面原生权限 |
| 原生结果 | 仅返回进度、受验证 origin、错误分类；auth key 不返回给远端 JS，不使用现有交互式 login 命令 |

二维码格式是 `neige-enroll:v2:` 加 base64url 编码的严格 JSON data envelope，总长最多 2048 字节；它不是可导航 URL。
必需字段：`version:2`、`enrollmentId`、规范 HTTPS `origin`、`authKey`、`authKeyExpiresAt`、`pairTicket`、`pairExpiresAt`。
时间为 UTC 毫秒整数；ID/随机 ticket/各字段均有长度限制；拒绝未知字段、重复键、错误类型、超长数据、无效编码和未知版本。
不携带 OAuth/API secret、cookie、sessionId、任意回调 URL、API endpoint 或 QR 自报的 peer/IP；不要压缩或嵌套解析。
bundled scanner 仅在本次用户扫描代际中把原始字节交给 native，随后释放；不持久保存完整 payload，不复制到剪贴板或任意 URL。
auth key 只用于 native LocalClient；pair ticket/attempt secret 仅在目标通过节点和 TLS 验证后向本次受限配对页提供。

| 状态 | 唯一 owner / 生命周期 |
| --- | --- |
| OAuth secret、token、key 清理记录 | host enrollment 模块；secret 私有文件、token 内存、有限清理元数据持久化 |
| v2 invitation、claim、receipt、Neige session | 现有 PairingState/SessionStore；内存、有界、同锁 grant/revoke；不加数据库迁移 |
| 手机节点密钥和 hostname | 原有 tsnet 私有 noBackupFilesDir；入网重试、杀进程、更新继续使用同一目录与身份 |
| pending enrollment | Android 私有 noBackup 文件；最多一条、原子写入、权限受限；不属于 FE 恢复快照 |
| 扫码意图 generation、配对页注入许可 | Android 内存；仅本次扫描，取消/离开/超时/进程死亡失效，不因 pending 文件自动授权 |
| 成功 receipt | 服务端内存与本次 WebView 同 origin sessionStorage 的短期 nonce；不保存 cookie/sessionId/指纹，不进入恢复快照 |
| 正式目标绑定 | 原 ConnectionProfiles；只在当前身份的可信 map 与 TLS 校验后原子提交，包含主设计要求的完整绑定 |

pending 只含 enrollment ID、origin、阶段、截止时间、尚需使用的 key/ticket、随机公开 attempt ID 和 32 字节随机 attempt secret；不得存整份 QR、正文或 API 长期凭证。
确认 `Running` 后立即删 pending auth key；完成、到期或取消清除其余秘密；清理失败保持阻断并显示本地存储错误。
初始化时清理过期/损坏记录；设备时间回拨或时钟不确定时不延长寿命，要求重新扫码。真实云端和服务端期限仍是最终限制。
不把配对继续许可写入 FE 显示快照；本机数据只使重试识别同一操作，不构成网络鉴权结论。

## 5. 手机入网、目标选择与中断

1. 用户开启扫描；创建新的内存 generation，解码/校验 envelope，保存有界 pending；无配置的新安装停留在扫描入口，不构造旧固定部署目标。
2. 检查同一 tsnet 目录的当前身份。无既有身份时调用有截止时间的 `LocalClient.Start(AuthKey)`，等待可信状态变为 `Running`。
3. 已有运行身份且目标可在其当前 map 中验证时直接复用，跳过 key 使用；成功配对后电脑仍删除这个未使用 key。
4. 已有身份但网络不符、目标不可见、身份过期或归属不明时停止并解释；不调用 Logout/清 state/以新 key 偷换身份，需单独明确处理旧身份。
5. 本次未完成 enrollment 的重试先检查同一节点状态；已入网就继续，尚未完成且 key 有效才重用该 key，不能换目录或生成新 hostname。
6. 用 `resolveTailnetTarget` 从已认证当前 map 解析规范 origin；用 `validateTailnetTarget` 重验后仅拨保存的受控 Tailnet IP，保持 SNI/证书/端口一致。
7. 成功验证 TLS 后安装 exact-origin 导航/代理/资源 fence，向本次顶层配对页注入 ticket、attempt secret 与仅内存的 scan attempt 信息，再自动 claim/redeem。
8. 通过 §6 的新 session 验证和正常 version/scope gate 后进入同 origin 合法原 route；无有效原 route 时进入首页，恢复右上角连接状态。

扫码表达目标选择意图，但不把 QR 自报信息当 peer 证据；既有 `target.go` 的唯一 DNSName、稳定 peer ID、节点地址类别与拨号前重验全部保留。
首次扫码若换 origin，先停旧流/代理并增代，绑定成功前不覆盖旧可用 profile；禁止向新 origin 复制 cookie、恢复快照或未发送草稿。
fresh map 无目标、peer 变化、TLS 不符、跨 origin/端口/协议跳转均关闭该次流程；不系统 DNS fallback、不忽略证书、不改成 direct-IP。
上述注入是 native 对精确顶层页面的一次性数据供给，不是远端页面可调用的通用 bridge；auth key 从不进入它。
任一步取消先增代并关闭新请求许可；迟到的 Start/代理/claim/redeem 结果不能改 profile、开 gate 或重新导航。
取消不能保证撤回已经完成的 Tailscale 入网；保留节点身份并显示实际状态，重新配对继续使用它。
进程死亡丢失本次配对许可；重开读取 pending 只用于状态核对/清理，需再次扫码建立明确意图，同码有效时可重扫且不重建节点。
断网/超时保留可操作页面及阶段；提示“尚未入网”“已入网，Neige 未配对”或“配对结果待核实”，不把一个绿色网络状态当整体成功。

## 6. v2 配对与退出标记的闭环

v2 invitation 的 kind 是明确的 `scan-preapproved`；owner 在创建时授权，PairingState 仅对该种类允许自动批准。
claim 发送 ticket、deviceName、客户端 attempt ID/secret；服务端仅存 ticket/attempt secret 摘要，原子将邀请绑定到首个 attempt。
同 ticket + 同 attempt 可在原 TTL 内重试并获得同一 claim ID；不同 attempt 拒绝，丢 claim 响应不要求重复创建节点或泄露服务端 secret。
redeem 校验该 attempt，沿用现有同锁 session 创建与设备限额；首次成功仅创建一个 session，设置原有 Secure/HttpOnly cookie。
响应丢失后的同 attempt 重试只可在原 TTL 内重发同一仍有效 session 的 cookie，不能新建会话；取消/禁用/撤销先使记录失效，重试不能复活它。
成功 redeem 同时返回随机 receipt nonce，服务端将其绑定于 enrollment、attempt、刚创建的 session；TTL 最多 30 秒且不越过 invitation 截止时间。
同 attempt 重试可替换尚未消费的 receipt，旧 nonce 立即无效，始终最多一个；receipt 消费后关闭整个 v2 重兑窗口。
pair.js 只在本次 redeem 成功后把 `{nonce,enrollmentId,attemptId,expiresAt}` 放到同 origin sessionStorage 并转入 `/next/`；不从 query/hash 构造“成功”。
bundled gate 需要仍有效的 native scan generation 和该次 origin/attempt，取出后立即删除 receipt；普通导航、重开 App、v1 跳转都不满足条件。
gate 向同 origin `receipt` 端点提交 nonce；服务端在同锁中核验 nonce/attempt/未过期且请求 cookie 正是新 session，再一次性消费。
校验响应仅返回本次 session 的 SHA-256 指纹和 attempt ID；nonce 本身没有业务权限，也不能为另一个 cookie 或 origin 提供成功证明。
receipt 校验成功只开放当前文档/代际一次 8 秒 whoami；whoami 的 sessionId 仅驻内存，摘要必须与 receipt 响应匹配，并不同于退出标记的旧指纹。
只有上述检查成功且退出标记删除成功，才进入原有 version/scope gate；receipt、URL、node Running、204 或已有 cookie 单独均不能解锁业务。
取消、超时、进程死亡、相同旧 session、错误 cookie 或任何存储失败保持退出标记与业务阻断；迟到结果按主设计 generation 规则丢弃。
已消费 receipt 后 whoami 失败不自动再发授权探测；可重新扫码新邀请，仍复用已入网节点。正常成功流程没有“验证本次配对”按钮。
需要的 FE 变化仅为 S4 配对结果适配与一次验证入口；共享身份结论仍归 S1 SessionGate，不平行创建另一套登录 owner。

## 7. 撤销、重启与泄漏范围

一个 QR 是短期 bearer 权限：先取得它的人可注册一个 tagged 节点并领取一个 Neige 会话；截图泄漏不能靠“仅扫码”消除。
Neige 权限沿用现有手机 owner 会话的访问面；Tailnet 权限取决于预设 phone tag policy，不宣称 QR 只能访问 Neige 而忽略已有 grants。
两份一次性资格分别消费，不具备跨 Tailscale 与 Neige 原子性；入网成功后配对失败，不自动删除节点，也不声称全部回滚。
取消 invitation 与删除 auth key 只阻止尚未完成的各自阶段；auth key 消费后的节点仍保留，必须由 tailnet 管理员在设备管理中另行删除。
本切片只授予 `auth_keys`，不为“一键撤销”额外索取整网 devices 写权限；UI 分别显示 Neige 设备撤销与 Tailnet 设备清理说明。
Neige 撤销复用 `sessions.remove` 与活跃连接关闭，同时删除相关 v2 重兑/receipt；不影响手机节点密钥或其他设备的有效会话。
calm-server 重启仍丢 invitation、claim、receipt 和 sessions，手机 cookie 因而无效；旧 QR 不再配对，电脑生成新 QR，手机复用节点重新配对。
host 重启从有限 key ID 清理记录撤销旧未使用 key；清理失败保留真实到期状态。新实例不能从日志或 QR 缓存恢复邀请。
手机卸载丢节点身份；不保证自动清理 tailnet 的旧设备，电脑提示管理员清理，禁止偷偷创建无限量 replacement nodes。

## 8. 验收与未验证条件

| 验收入口 | 正例 | 必须失败的负例 |
| --- | --- | --- |
| host issuer fake + 真实模块边界 | 一次 key 与一份预批准邀请；真实期限和固定能力核对 | 缺 scope、tags 不同、reusable/ephemeral、过长 expiry、迟到创建复活、秘密进日志 |
| 真实管理入口 | 有效 PasswordLogin 可创建/取消/configure | PairedDevice 直连主端口、过期 session、dev_autologin、缺来源或把记录缺失当管理员 |
| host 取消/重启 | 本地立即禁用 ticket，云端失败可见并补偿 | 删除 key 被误报为踢设备；超时盲重试造成多个 key；未知 key 宣称清理完成 |
| Android QR/parser/capability | 新安装扫描 data QR 并发起单一 native enrollment | 恶意 URL、重复键、过长 payload、远端 IPC、旧 generation、外部浏览器 Intent |
| Android + Go 生产入口 | 首次无身份 key 授权；中断复用同目录/节点；已入网跳过 key | 外网现有身份被注销、隐式切换、重试新 hostname、首次捏造固定 target |
| Go dial + WebView | 当前 map 的目标、完整 TLS 与 exact-origin fence 通过 | QR peer/IP 当可信、名称重用、证书错、系统 DNS fallback、跨 origin 携 secret |
| v1/v2 Rust pairing | v2 自动批准且重试最多一个 session，v1 仍等 owner 批准 | v1 ticket 跨端点升级、不同 attempt 争抢、撤销后重兑、禁用时签发 cookie |
| bundled gate 浏览器测试 | 有退出标记时扫码成功自动验证新 session | 伪造/重放 receipt、旧 cookie、指纹错、失败/取消/重开自动清 marker、普通导航解锁 |
| 真实电脑 + Android | 手机一次扫码→入网→配对→原页/首页；重开原页及右上角恢复 | OS 浏览器弹出、二次手动验证、离线白屏、节点或会话重复创建 |

承重断言需按仓库纪律做 production mutation 验证：v1/v2 隔离、单会话重兑、撤销 fence、receipt 与 cookie/attempt 绑定、目标来源和旧身份保护。
跑受影响的 Go、Rust 目标测试、Android instrumented 与浏览器集成测试；不跑共享生产主机的真实 Codex E2E；schema 改动跑真实生成器。
实施 diff 必须重新双路评审；本设计获批不替代各切片的代码评审或真实设备验收。

发布前必须用专用测试 tailnet/设备实测：API 300 秒真实 expiry、OAuth tags/预批准、control-plane 中断后同节点恢复、Android cookie 与 receipt 跳转。
另验 HTTPS/MagicDNS、tailnet policy 可见性与端口可达性、禁用设备审批和启用审批两种条件；Tailnet Lock 不满足前提必须明确阻断。
目前这些实网条件均未验证，尤其短 key 寿命可能使方案暂不具备发布条件；不能用 mock 成功替代，也不能读取用户现有密钥擅自试验。
