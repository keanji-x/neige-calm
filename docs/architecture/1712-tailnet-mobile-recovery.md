# 内置 Tailnet 服务与 Android 页面恢复

状态：设计 v2 已获两路独立 APPROVE（2026-09-16）；未实现、未完成设备验收。
跟踪：[issue #1712](https://github.com/keanji-x/neige-calm/issues/1712)。
事实基线：`c442f3440dc5ab819eb82eca0c43aeec8a29429d`，2026-09-16。
用户目标：电脑端随 Neige 安装 Tailscale、从设置启停；手机复用已有集成，退出再进先显示上次页面，右上角后台重连，断网不白屏。

## 1. 交付范围与改动估计

生产代码预计约 1,800–3,000 行，另有测试、生成产物及构建修改；这是实施前估计，不是已测得的 diff。
涉及 Android 生命周期、网页鉴权呈现、代理权限及发布状态，因此采用完整设计评审；按可用结果分片，不做一个巨型 PR。

| 切片 | 用户可独立使用的结果 | 预计生产增量 | 依赖与完成条件 |
| --- | --- | --- | --- |
| S1 页面恢复 | 杀进程/离线重开后直接进入原路由的本地页面结构，右上角显示恢复状态；联网自动恢复正文 | 500–800 行 | 不依赖服务器升级；真实 Activity 离线重开通过 |
| S2 连续使用 | Wi-Fi/蜂窝切换、后台返回、事件断线后自动恢复；活跃终端回到原进程 | 400–650 行 | 基于 S1；真实设备切网、WS 和终端恢复通过 |
| S3 内置服务 | 安装 Neige 即拥有独立 Tailnet 节点，设置启停/登录/诊断，访问受限网页及终端入口 | 650–1,100 行 | 新 Go 程序、neige-app 管理、受限服务入口及发布构建；独立服务故障不阻塞本机 |
| S4 两端配置闭环 | 手机从配置/配对信息连接任意已批准的内置节点，不再仅支持当前固定部署 | 250–450 行 | S3；同网内两端真实登录、配对、重启、撤销、重连通过 |

首个实现 PR 合并 S1/S2，交付完整“原页面进入 + 右上角后台自动重连”结果；S3/S4 不得被视为取消，只是独立交付和回滚单元。
本轮先落实 S1/S2；完整设计评审收敛后创建 issue，发布全文，再开始实现。每个切片需独立评审实际 diff。

“恢复上次页面”的具体含义：
- 热恢复保留已挂载页面、滚动、输入草稿和当前内存内容；恢复期间标记内容可能过时，禁止发出业务写操作。
- 冷恢复先显示原页面类型、导航位置、布局和占位区域；验证后填充权威正文，不把聊天、报告或终端输出另存成离线库。
- 冷启动终端区域先显示“等待恢复终端”；不伪造旧屏幕，不启动新进程。草稿未新增跨进程持久保证。
- 首次安装、没有有效记录、存储损坏或主动退出账号时有完整可操作入口；绝不以空 DOM 等待网络。

## 2. 已核对的实现事实

| 事实 | 源码证据 | 对设计的约束 |
| --- | --- | --- |
| APK 已内置 Next HTML/JS；资源在选定服务 origin 下由 Android 拦截返回 | `mobile/src-tauri/gen/android/app/src/main/java/io/neigecalm/next/BundledFrontendAssets.kt:17` | 复用本地资源，不另建远程页面缓存或 Service Worker |
| launcher 先探测可达性，bind 再探测，最后才装本地资源和代理 | 同目录 `BundledFrontendPlugin.kt:115`、`:171`、`:198`；`mobile/www/app.js:60` | 冷启动第一层阻塞在原生入口，不能只改 React loading |
| 手机已有 tsnet 节点，身份存在 noBackupFilesDir；目的地在 Kotlin/Go/build profile 中固定 | 同目录 `P2PConnection.kt:10`；`mobile/p2p-native/main.go:27` | 保留身份、用户态网络和单节点；S4 必须扫齐三处固定目的地 |
| CookieManager 已保留 HttpOnly cookie，但它只是提示，服务器 session 仍在内存 | 同目录 `RememberedSession.kt:5`；`crates/calm-server/src/auth.rs:23` | 不新增 JS token 存储；服务端重启后仍可能要重新配对 |
| whoami 外层 gate 和 bundled version gate 均阻止 router 挂载 | `fe/web/src/app/auth/session-gate.tsx:1`；`fe/web/src/app/providers/public.tsx:86` | 本地呈现与联网权限必须拆开，不绕过 whoami |
| dbInstanceId 每次服务进程启动重新产生 | `crates/calm-server/src/routes/version.rs:5`；`crates/calm-server/src/state.rs:1672` | 它是数据/事件缓存失效 epoch，不是持久数据库身份 |
| UI 已保存对话选择、侧栏显示偏好；TrackView 状态仅存于 router 生命周期 | `fe/web/src/app/providers/ui-preferences.tsx:11`；`fe/web/src/app/router/track-view-state.tsx` | 扩展小型显示状态，不序列化整个 QueryClient |
| EventBridge 唯一启动事件流；driver 已有代际检查、500ms–8s 退避及 401 探测 | `fe/web/src/app/events/event-bridge.tsx:1`；`fe/web/src/systems/events/websocket-driver.ts:25` | 保留唯一订阅 owner，补暂停、抖动、超时，不另写平行重连器 |
| xterm 重连保留输出，已有同终端重连测试 | `fe/web/src/systems/cards/builtins/terminal-reconnect.browser.test.tsx:67` | 自动恢复应进入现有 attach/replay 路径 |
| 服务端现有手机入口调用系统 Tailscale Funnel；公开 router 排除 worker hooks、管理及密码登录 | `crates/calm-server/src/mobile_access/funnel.rs:20`；`crates/calm-server/src/routes/application.rs:28` | 新服务不能代理完整 application_router，也不能默默改为公网入口 |
| neige-app 已分别管理内核和终端 supervisor，含重启上限；通用 spawn 当前继承环境 | `crates/neige-app/src/main.rs:493`、`:572`、`:1103` | 复用生命周期，新增敏感子进程必须有明确环境白名单 |
| 相关源码扫描门禁已检查 | `fe/tools/architecture/README.md`；`crates/calm-server/tests/cases/deferred_write_tx_invariant.rs:1`；`scripts/gate-web-compat-version-lockstep.sh` | core 不接平台，持久 key 归 core/keys；不得加入 deferred SQL 事务；接口改动跑真实生成器 |

另已检查 `boot_invariants.rs`、同步事件版本及术语/文本棘轮脚本头注：本方案不修改 worker 恢复语义或已发布迁移，不需要随意升事件版本。
引用只定位承重决策；实施基线变化时重查调用路径和门禁，不能沿用旧行号当验证证据。

## 3. 状态 owner 与持久边界

| 状态 | 唯一 owner / 保存位置 | 生命周期 |
| --- | --- | --- |
| 手机 Tailnet 密钥、节点名 | 现有 Go tsnet / `noBackupFilesDir/p2p-node` | 杀进程、更新 APK、暂时关闭保留；明确退出 Tailnet 才注销；卸载自然删除 |
| 手机连接配置 | Android ConnectionProfiles / 私有 preferences | 增加 schemaVersion、profileId、configRevision；原子提交，坏记录回配置页 |
| 原生入口指针 | **NEW** Android ResumeEntry / 私有 preferences | 只含 profileId、configRevision、规范 origin、白名单 route；不是授权或正文缓存 |
| 安全显示快照 | **NEW** FE RecoveryContext / 注入 Storage port | 小型有版本记录；scope 为规范 origin + 在线 userId + dbInstanceId；profile 修订由原生入口匹配 |
| 登录 cookie | 现有 CookieManager | 不复制进 snapshot/URL/日志；过期、注销或服务端撤销都以 whoami 为准 |
| 本机退出标记 | **NEW** SessionGate / origin 隔离的 Storage port、独立 core key | 只存版本及已退出会话的 SHA-256 指纹；普通清缓存不删除，成功的显式新登录才清除 |
| 事件游标与内存查询 | 现有 cursorStore、QueryClient | 仅同 origin/profile/已验证账号和 db epoch 可继续；切 scope 清理旧实例 |
| 服务器 Tailnet 身份与期望启用状态 | **NEW** neige-app 私有 data dir | 与发布目录分离；0700 目录/0600 状态；每份状态仅一个活跃 writer |

RecoveryContext 必须通过版本化解码；最多 8KiB，仅保存 pageKind、白名单相对路径、pane 枚举、资源定位 ID、有限滚动值、已存在会话选择。
不保存用户名展示文本、Track 标题、正文、文件路径、命令、终端输出、登录凭证或接口响应。资源定位只用于在线验证后恢复，不作为可显示内容或执行依据。
使用既有 route/search codec；丢弃 pairing/login/logout 路径、URL fragment、任意 query、绝对 URL、`..`、反斜杠、非法编码。
允许恢复 Today、Track、Recipes、已有 Settings 分页；未提交的创建页回到安全的所属入口，不复放创建表单。
新快照存储写失败只影响下次恢复，本次仍可用；先完整序列化再单 key 替换，未知版本删除恢复记录而不是猜测字段。

Android 从 `doUpdateVisitedHistory` 观察已绑定 origin 内的 SPA 路由；只接受当前 profile/generation 的白名单路径，不读取网页正文。
FE 只在在线已验证 scope 下提交显示快照，限制写频率并在 pagehide/visibilitychange 做尽力刷新；不能依赖退出回调一定执行。
native 指针是跨 origin 定位提示，web 快照是显示状态，两者不互相充当身份。没有匹配快照时仍能打开无数据页面结构。
默认不增加 native→web bootstrap 协议：原生入口独立匹配 profile/revision，浏览器 origin 隔离无正文记录，联网后再验证 userId/db epoch。
本轮两路未发现要求 native profile 下传的泄露反例；保留上述分层 scope，不为无正文页壳下传原生配置，更不扩展远端 Tauri 权限。

当前 userId 是固定 owner；whoami 另有 sessionId，可辨认新会话，但其值本身就是 cookie 凭证，只留内存；显示快照不保存它或其指纹。
当前没有持久 database ID；dbInstanceId 变化必须丢弃旧内容/游标/会话选择，只把 route 当候选，再由新服务器验证资源。
同主机不同端口共享浏览器 cookie 的既有约束仍在；不同 origin 的数据和配置不能合并，也不能拷贝 cookie 来加速切换。

## 4. 启动、退出和页面呈现

### 4.1 冷启动顺序

1. MainActivity/Tauri 初始化后先读本地连接配置与 ResumeEntry；无配置则呈现现有设置入口。
2. 已配置时先安装 exact-origin 本地资源拦截和一个已绑定 loopback 的封闭代理，再导航保存的 `/next/...`；不等待 DNS、tsnet Running 或 `/api/version`。
3. 代理尚未就绪时对网络请求返回暂不可用，绝不回落到系统直连；本地资产仍可读。保留 Wry 原始 client 的权限记账。
4. FE 首次 render 立即生成原 pageKind 的导航、布局、占位和右上角状态；读本地恢复记录，不启动真实业务 router loader 或 cards。
5. Android 后台启动/恢复原有 tsnet 或 direct proxy；FE 按 whoami → version → scope 校验顺序探测。
6. 只有 whoami 成功、兼容版本通过且 scope 明确后，才挂载授权 router/业务读写和 EventBridge；仍使用原 URL，无自动跳首页。
7. 获取当前资源后恢复有效 pane/会话选择/滚动；不存在的资源显示可退出的“已删除/不可访问”，提供回上级动作。

本地 RecoveryPresentation 复用 shell 的纯视觉部件；不得直接挂载现有 AppShell，因为它会取服务端列表。
先建立无 I/O 的 shell frame，再由授权态填充现有 ShellRoute；不是通过假 whoami、缓存鉴权 query 或吞掉 401 显示私有正文。
普通浏览器保留既有启动合同；恢复框架通过现有 bundled 编译标记启用，不能改变网页版鉴权先于 router 的合同。
存储损坏、离线首次启动、资源安装损坏都必须有可操作的本地提示；错误页不是白屏，也不循环刷新。

### 4.2 热恢复与退出

Activity 暂停不停止用户终端、不退出 Tailnet、不清 cookie；保留 WebView 与已授权页面实例。
回前台立即显示原页面，先暂停写入口，后台刷新网络接口/身份/版本；网络错误只更新角标，不用连接页替换正文。
WebView renderer 或 App 进程已死亡则执行冷启动流程，不依赖 Android 自动恢复旧 WebView 正文。
普通 Back/切到后台/杀进程与“退出账号”明确不同；返回配置页仍是可达的恢复操作，不能让断网用户被困在页面里。
显式退出账号只在有已验证会话的页面提供；冷启动未鉴权页提供登录/配对/配置入口。退出先关许可并增代，再从内存 sessionId 计算指纹、持久写入退出标记，然后清恢复上下文/查询/游标/私有正文并停订阅；清理不得删除退出标记。
指纹使用锁定版本的成熟纯 JS SHA-256 实现，适用 IP HTTP 的不安全上下文；不依赖 crypto.subtle、不自写摘要算法、不持久化原 sessionId。此标记只能拒绝恢复，不能授予身份。
在线以有截止时间的既有 logout 撤销原会话；离线立即在本机退出，不谎称服务端已撤销、不留稍后自动发送的 logout。写标记失败仍清内存并保持本次 gate 关闭，明确显示“无法保存退出状态，不能保证重开后仍退出”；不能报告持久退出成功。
标记存在时，普通 `/next/` 导航、残留原生指针、旧异步结果及自动 whoami 均不能恢复授权；坏标记也保持阻断并显示存储错误。重新登录必须由用户动作开始，普通关 App 再开不增加操作。
账号登录使用本次成功 login 响应，再由 whoami 确认同一新 sessionId；旧服务器扫码配对成功仍只跳 `/next/`，此时显示“验证本次配对”按钮，不将导航视为成功证明。
点击该按钮才授予当前文档/代际一次 8s whoami；仅成功返回的 sessionId 指纹不同于已退出值才证明新会话。验证成功且退出标记清除成功后进入正常版本 gate、建立新上下文；相同指纹拒绝，要求重新配对。
新尝试不持久化、不自动重试；失败、取消、离页、超时或进程死亡均保留标记，迟到响应无效。取消后即使配对已签发 cookie，重开仍需新的明确验证动作；旧 pair.js 无需升级，也不新增 native bridge。

## 5. 连接协调和右上角状态

每层仅一个 owner：Android 管引擎/代理，SessionGate 管身份结论，ServerCompatGate 管版本与 db epoch，EventBridge/driver 管事件 socket，terminal view 管终端 attach。
**NEW** app 层 RecoveryCoordinator 仅组合这些事实并下发暂停/恢复许可；不另建一份 tsnet 状态，不代替 driver 连 WebSocket。
纯状态转换、退避计算和快照 codec 在 core；平台事件/计时器在 systems，经 port 注入；app 只组装，features 只显示页面操作。
原生在 Activity resume、ConnectivityManager 网络变化时更新 AndroidNetworkSnapshot，调用现有 netmon Poll；一次恢复任务在运行时合并重复触发。
修复现有 P2PConnection 在启动前设置 started、失败永久缓存的状态类：启动失败必须可再次尝试，先释放部分资源，保留节点身份，并保证单个正在启动的实例。
页面继续由 whoami/version/event 判断“可用”，不能把原生 `Running` 或一次 version 200 显示成整个工作区已连接。
没有原生事实通道时只显示“正在恢复连接/服务器暂不可达”，不猜测 VPN 状态、直连/中继或根因；细节诊断留在连接设置页。

| 角标状态 | 条件 | 行为 |
| --- | --- | --- |
| 正在恢复 | 冷启动或回前台尚未确认会话/版本 | 原页面结构或热页面可见，写操作关闭 |
| 离线 / 正在重试 | 已知离线或瞬态传输失败 | 展示最近失败时间和下一次重试；可立即重试/进入连接设置 |
| 正在同步 | 身份/版本可用，事件流尚未 replay-complete | 允许只读显示；防止把未同步数据当最终状态 |
| 已连接 | 当前代际身份/版本通过，事件回放完成 | 恢复常规业务操作；终端各自仍需 ServerHello/owner 确认 |
| 需要重新登录 | whoami/受保护请求明确 401 | 立即隐藏私有内存内容，停止所有重试/订阅，显示登录或配对 |
| 需要更新 / 配置异常 | 版本不兼容、配置无效、TLS 校验失败 | 不自动重复请求，保留本地可操作提示 |

角标固定右上，窄屏也能点击，文本含义不依赖颜色；使用节制的 aria-live，避免每次重试读屏播报。
点击 Retry 只是唤醒同一恢复任务；点击多次不会创建多个代理、whoami、事件流或终端连接。

业务写的必经边界是共享 REST command/transport 以及终端 send/attach owner，禁用按钮不能替代它们。所有业务意图在 mutation/serial queue 接受前按当前已连接 generation 准入，携带不可更新的许可，发送前再次复核；缺许可、已暂停或过期均立即拒绝，恢复后不得自动重放。
扫齐默认 paused mutations、serial writer、普通 Promise/upload 与终端 pending frames；暂停/失效时拒绝尚未发送的旧意图，不能恢复许可后再将它们记为“新点击”。业务 mutation 禁止网络暂停与自动重试，现有串行顺序仍保留但不得跨恢复代际续发。
终端输入、owner claim、resize 均通过同一发送检查；身份重验时即使旧 socket 仍 OPEN 也不得发出。恢复握手仅在身份/版本及事件恢复许可通过后由唯一 attach owner 发起，随后仍需 ServerHello/owner 确认输入权限。
恢复专用放行仅覆盖 SessionGate 的 whoami、版本探测、本次用户 login/logout、既有明确配对 claim/redeem；各有代际、取消和超时，不以通用“允许 POST”绕过业务门禁。退出标记存在时 whoami 仅允许 §4.2 的显式尝试；事件 driver 复用此身份 owner，不另探测。

## 6. 断连、重试与恢复合同

所有异步结果携带当前 generation；进入恢复/后台暂停、401、更换配置/导航离开绑定 origin/退出账号/开始新登录/销毁 Activity 时增代并取消任务。
过期结果既不能写配置或显示快照，也不能 setProxyOverride、启动流或把角标变绿；代理安装回调同样必须验代。
REST 成功/失败响应及 401 广播在触及 cache、页面或身份 owner 前都验代；旧请求的迟到 401 不得登出新会话。当前代际 401 原子关闭许可、增代并清私有状态，后续同代际广播失效。
切换配置先关闭旧 socket/原代理，清除旧 scope 的内存数据，安装新 exact-origin fence 后才开放新请求。
不能只靠取消外层 Future：JNI 中的启动/探测亦需可终止或有硬截止时间，超时不得永久堵住现有单线程 worker。

| 层 | 重试策略 | 停止条件 |
| --- | --- | --- |
| 原生启动/链路探测 | 指数上限 30s，加随机抖动；resume/network-change 合并为一次提前尝试 | 明确未登录/待审批/配置错误暂停，等用户动作；不得重建节点身份 |
| whoami/version | 同代际单飞；每次 8s 硬超时，可取消；500ms 起、上限 30s、随机抖动 | 401/版本不符终止；已知离线不轮询；前台稳定 30s 后重置退避 |
| 事件 socket | 沿用 driver 唯一退避，补抖动、握手/回放截止时间；网络许可恢复后才连接 | 暂停/登出停止；只有 replay-complete 才 connected |
| 活跃终端 | 恢复许可由断→可用时只触发现有 reconnect 一次；失败走同一个终端重试 owner | Exited/协议错误/明确权限拒绝不自动重试；不对后台未挂载卡片启动连接 |

手机进入后台暂停网页恢复定时器/新请求；返回立即重查，不承诺 Android 会让后台网络持续运行，也不引入常驻前台服务。
网络通知只是唤醒提示，不能证明服务可用；短时间 Wi-Fi/蜂窝来回切换用单飞+取消处理，不同时跑两条有写能力的数据链路。
事件重连复用 cursor/replay/reducer/invalidation；若服务器 epoch 改变，清旧 cursor、建立新流并重读当前页面，不能把旧游标套到新库。
终端恢复沿用原 terminalId 与协议握手/回放。只有恢复 owner 授权后才接收输入；断连期间按键不缓存、不自动重发。
已发出但结果未知的 POST 不盲重试；现有幂等/查询确认机制保持各自业务 owner，不因恢复功能新增离线任务队列。
恢复过程中资源 404 与网络错误分开：前者终止该资源恢复并保留返回入口，后者保留位置并继续后台重连。
冷恢复默认使用上次明确选中的 origin；禁止因为另一个 IP 更快就切到不同服务。首次未进入过工作区仍可沿用现有 IP→Tailnet 候选流程。
跨 origin 切换需要用户选择，重新在目标 origin 鉴权；不复制 cookie、snapshot、游标或尚未发送草稿。
现有 Go CONNECT 的 30 分钟绝对 deadline 要进入长连接验收：改为由健康/取消管理连接，不能把正常空闲终端固定截断。

## 7. 内置服务端 Tailnet 服务

**NEW** 发布 `neige-tailnet` 小型 Go 可执行文件，使用与手机一致、锁定版本的 tsnet；由 neige-app 管理独立进程，用户仍只安装 Neige。
只提供私有 tailnet 中的 Neige HTTPS 网页/API/终端；不开 Funnel、公网监听、子网路由、exit node 或主机 SSH。
沿用现有系统 Tailscale 的用户不用迁移：新节点使用自己的 userspace 网络、状态目录与名称，不调用系统 socket、不修改系统 Serve/Funnel。

链路：远端设备 → tsnet ListenTLS → 固定的 Neige 受限 ingress → 已有 auth/REST/events/terminal。
该 ingress 从 `public_mobile_router` 组装，继续排除 worker hooks、管理 API 和密码登录；配对须由本机已登录 owner 批准。
复用既有配对、会话撤销和 RevocableListener 的活跃连接关闭语义；不要让 loopback 转发获得 worker 权限。
`neige-app` 新增受权限保护的本地控制通道；calm-server 通过注入的窄 client 请求 start/stop/status/login，不把 admin token 暴露给网页。
优先受 filesystem 权限保护的 Unix socket；公开入口不注册这些控制路由。控制请求有版本、大小上限、超时及幂等期望状态。
Go 程序只接收明确 CLI/config 字段（状态目录、名称、固定 upstream/control 地址）；spawn 环境白名单，不继承服务端凭证、代理或云账号环境。
ingress 目标由部署配置固定，不能由 HTTP 参数/Host 随意选择；外来转发身份头清除，保持 TLS、WS Upgrade、流式响应及取消传播。

设置区分“启用远程访问”和“退出 Tailnet”：关闭停止监听、撤销入口及活动连接但保留节点；退出才清身份并要求重新登录。
持久保存 desiredEnabled 与配置修订，进程启动后异步协调；tsnet 启动失败不能阻塞 Neige 本机网页或终端服务。
进程退出才由 supervisor 拉起，退避+抖动+熔断；断网/待登录/待审批是运行态，不能导致无限杀进程重启。
本机设置显示 disabled、starting、needs-login、needs-approval、online、degraded、failed；服务活着、HTTPS 可用、upstream 可用分别诊断。
登录 URL 仅当前授权操作短暂返回，不记录日志或普通设置；节点密钥只在 private data dir。
MagicDNS/HTTPS 未配置时说明具体操作，禁止降级到明文绕过。官方依据见 [tsnet Server API](https://tailscale.com/docs/reference/tsnet-server-api) 与 [HTTPS 设置](https://tailscale.com/docs/how-to/set-up-https-certificates)。

S3 保留现有显式 Funnel 配置与入口合同，不静默启用或迁移；新 private-tailnet provider 使用独立配置判别字段，两种入口不能共享控制状态。

S4 用一个原子配置替代三处硬编码，绑定规范 HTTPS origin、peer 稳定节点 ID、受控 Tailnet IP 与唯一端口；hostname 同时是 URL authority、TLS SNI 和证书校验名，端口必须在 1–65535 且 origin/CONNECT/dial 一致。证书不匹配绝不忽略或降级。
二维码只提议 `https://<peer DNSName>:<port>/mobile/pair#v1.…`：用户确认目标，再从手机已认证 tsnet 的当前节点信息核对 DNSName、稳定节点 ID 与 IP。同后缀或二维码自报 peer/IP 不构成信任；仅允许该可见节点记录中的 100.64.0.0/10 或 fd7a:115c:a1e0::/48 地址。
Tailnet 路径不经系统 DNS，不拨公网、LAN、loopback、未指定、link-local、multicast、子网路由或 exit-node 目标。缺少受信节点信息时先保持未连接；拨号前重验绑定并只拨已验证地址，peer 身份/目标变化关闭旧连接、增代并要求重新确认，不静默接受 QR/DNS 替换。
显式 direct-IP 配置是独立的用户授权，可允许用户输入的普通 LAN IP，仍拒绝保留地址；二维码不得切换到该模式。已有 direct HTTPS hostname 必须校验完整解析集合并固定本次拨号 IP，拒绝保留地址、验证后再解析和无确认的地址变更；TLS 仍校验原 hostname。
本地资源、导航、proxy CONNECT 与普通 HTTP 都维持 exact-origin fence；跨 origin/端口/协议的 redirect 在转发 cookie 或配对 secret 前拒绝，同 origin 才可继续。先验证配置、节点绑定和 TLS 再配对，禁止链接自动改代理；既有 owner 审批仍必需。

## 8. 升级与回滚

恢复记录独立 schemaVersion，坏记录和未来版本 fail closed；丢的是页面位置，不是用户正文。
APK 更新通过实际打包 manifest/兼容版本检查，继续本地资源策略；不把服务端 bundle 缓存当 App 更新。
ServerCompatGate 在网络恢复后仍严格执行，不能为了旧页面能开就执行不兼容 API；提示更新哪个端。
发布清单、校验摘要、许可文本、支持平台构建都加入 neige-tailnet；先支持当前 Linux 服务部署，其他平台明确不可用。
内核升级不必重启 Tailnet 子进程；受限 ingress 暂不可用时返回可识别错误，恢复后沿原地址工作；会话是否仍有效仍由 auth 决定。
服务器进程重启会丢现有 session/配对状态，本设计不伪装“跨服务器重启免登录”；需要时重新配对，但仍显示可操作页面。
若要长期持久 session，另开带有效期、撤销、凭证保护和数据迁移的独立切片，不捎带引入无限期 token。
tsnet state 旧版兼容无已核实保证；升级前停止唯一 writer，再以同权限做一致性备份，记录二进制/state 版本组合。
回滚只在经过兼容验证的组合间进行；禁止两个版本同时打开状态，禁止仅换旧二进制猜测兼容；失败时保留本机访问和手动重新登录入口。
停止服务和清身份是不同动作；回滚不回滚用户已执行的撤销意图，不恢复旧业务会话。

## 9. 验收表与实际入口

下列均为待执行验收，不代表当前代码已满足。实现修复前先在真实入口固定最小失败，记录可重现条件。

| 场景 | 必须看到的正例 | 必须拒绝的反例 | 真实入口与证据 |
| --- | --- | --- | --- |
| 离线杀进程重开 | 本地原 route 页面结构和右上角状态先出现 | 等 HTTP/DNS 才呈现、空 root、自动跳首页 | Android MainActivity + 真 APK；禁网后首帧截图/请求记录 |
| 热后台返回 | 原正文/滚动保留，后台确认后可操作 | 切成全屏连接页、输入在离线后悄悄发送 | 真机 home/resume 与内存页面断言 |
| 写入准入与旧响应 | 离线意图即时报拒绝，恢复后需新动作 | paused mutation/serial queue 在新代际续发、重验中 OPEN 终端发输入、旧 401 登出新会话 | production mutation/transport + serial writer + 真终端 send 边界 |
| 网络恢复 | 同一页面自动显示权威数据 | 多次点击造成多任务、过时代际变绿 | production mount + 网络故障注入；真实手机切网 |
| 401/撤销 | 立即停止流、清私有数据并提示配对 | 缓存身份继续发写请求、旧正文在新登录显示 | whoami/REST/WS 三入口；现有 mobile pairing 撤销测试 |
| 退出账号离线重开 | 无自动登录或旧资源恢复 | 残留 cookie/原生 pointer 重新登录 | ProductionApp sign-out + Activity 杀进程重开 |
| 退出后重新配对 | 旧 pair.js 返回后明确验证，新 session 通过才清标记 | 相同 cookie 解锁、失败/取消/杀进程自动解锁、标记写失败谎报成功 | HTTPS 配对与 IP HTTP 登录真实入口；无 crypto.subtle 环境、存储故障注入 |
| 配置/账号/db epoch 改变 | 旧 scope 清除，目标重新验证 | 跨 origin 复用 cache/cookie/cursor、旧响应污染 | native config + SessionGate/ServerCompatGate 合同测试 |
| 路由损坏/资源删除 | 安全降级或明确资源不可用，可返回 | 外部 URL 导航、恢复任意 query、无限重试 404 | 实际路由 parser + 真 router/服务响应 |
| 事件恢复 | 单订阅，回放结束才绿色；遗漏事件触发重读 | socket open 就绿色、旧 cursor 带入新 epoch | EventBridge + websocket-driver + 真 events endpoint |
| 终端恢复 | 原 terminalId，回放后恢复输入，原进程仍在 | 新建进程、重发断网输入、协议拒绝仍抢 owner | 现有 terminal-reconnect 浏览器用例 + 真 terminal WS |
| 原生权限 | 本地资源可离线开；非法 origin/重定向失败 | 代理未就绪走系统网络、远端页面获得 Tauri | 现有 BundledOrigin/DirectConnection/权限 instrumentation |
| S4 目标绑定 | 已确认节点的 HTTPS 名称、peer、端口一致才连接 | origin/peer 错配、保留/重绑定地址、TLS 错配、跨 origin redirect 携密钥 | QR parser + 受信节点解析/Go dial + WebView TLS/导航负例 |
| 内置服务 | 设置启用/登录/地址访问，系统 Tailscale 可并行 | worker/admin 经 loopback proxy 暴露，继承密钥环境 | neige-app 生产 spawn + 受限 ingress 真实 HTTP/WS |
| 崩溃与升级 | 单进程拉起、身份保留、失败不影响本机 | 双 writer、反复 enrollment、回滚旧授权 | 真子进程故障注入、发布安装/回滚演练 |

覆盖 API 26/35 模拟器和至少一台实际手机；Wi-Fi→蜂窝、飞行模式恢复和长时间空闲 WS 必须真机验证，mock 不能替代。
已有入口：`mobile/tests/native/run.mjs`、`mobile/src-tauri/gen/android/app/src/androidTest/...`、`mobile/tests/native/run-release-smoke.py`。
只对承重边界做单因素生产 mutation：取消 origin fence、移除 generation 校验、开放鉴权前业务挂载等；事先写完整预计红测试名，再比较实际全集并还原。
mutation 期间独占 worktree，禁止并发作者/评审读取短暂变异；不改测试制造红绿，不跳过还原后的绿测。

## 10. 门禁与实施纪律

S1/S2：fe lint/build/test + 相关浏览器用例；mobile 单测/浏览器、Go 测试、原生 JVM/instrumentation、真实 APK 验证与真机恢复。
S3/S4：受影响 Rust package 的目标 nextest（保持 NEIGE_CODEX_BIN unset、限制并发）、quick Rust gates、Go 服务测试、发布产物与受限入口合同。
涉及新 API/必需字段时执行 `fe` 的真实 `gen:api`，包含所有生成产物；版本变更执行 web compat 和 sync-event lockstep 对应门禁。
涉及数据库的后续扩展才跑相关 invariant suite，并遵守 `deferred_write_tx_invariant`；当前页面恢复不用新 SQL 表或持久数据库身份。
所有 fe key 由 core/keys 生成，平台 I/O 经注入 port；新模块及冻结接口先走现有 ownership change request，不扩宽 architecture allowlist 躲检查。
Android 自定义代码在已提交 gen 目录中，构建不得运行 android:init 覆盖；资源/安全配置修改跑真实生成器和 APK 检查。
完成标准是本切片 required checks 真实绿色、双通道实现评审收敛、diff 无无关改动；本地缺少设备/工具时明确缺口，不能以浏览器 mock 宣称端到端完成。
设计评审：同一静态文档交 fresh subagent 与 fresh read-only Codex，各轮归档；逐条验证并修订，再复核上轮裁决。
收敛后 issue 发布全文（超限按章节拆分），实现 PR 携带本文件；issue、PR 与测试报告明确每片完成状态。

## 11. 处置历史

### v1 — 初稿

- 核对手机已有 tsnet、本地 bundle、cookie、事件和终端恢复路径，避免重复建设。
- 将“恢复画面”固定为冷启动无正文页面结构、热启动内存画面；不扩张成离线数据同步系统。
- 拒绝把 dbInstanceId 当稳定数据库身份、把 CookieManager 当已登录证明、把系统 Funnel 当私有内置服务。
- 采用原生 history 观察，默认不增 bootstrap/native bridge；profile 下传作为开放 fork 交评审证伪，保留既有 launcher-only 权限边界。
- 服务端目标保留为 S3/S4；先交付手机恢复，但不声称内置服务已实现。
- 待处理：双通道设计评审、实际实现增量复估、各切片最小红例与设备验收；本稿无“已验证通过”的实现结论。

### v2 — 双路 v1 发现裁决

- A-1 / B-1，P1，接受：共享 transport 已在 `production-app.tsx:46`，但 `queries.ts:772` 的默认 mutation 和 `:344` 的 serial writer 能延迟执行；`xterm-view.tsx:579` 独立发帧。§5/6 明确排队前准入、发送前复核、失效意图拒绝及响应/401 代际检查，§9 补负例；未声称现有实现已修复。
- A-2 / B-2，P1，接受退出再配对闭环问题；驳回 A 的“whoami 无 sessionId”证据：基线 `fe/core/api/auth.ts:9` 为必需字段，`crates/calm-server/src/auth.rs:334` 定义、`:343` 返回该会话值。它也是 cookie 凭证，不能持久化原值；§4.2 采用独立拒绝标记+成熟 SHA-256 指纹+显式新会话验证，兼容旧 pair.js，仅退出后多一次确认。
- A-3 / B-3，P2，接受：`scanner.js:43` 与 `p2p-native/main.go:27,113` 的固定 origin/peer 绑定将被 S4 移除，`direct.go:143,166` 会系统解析；§7 补受信节点、origin/SNI/证书/端口、地址类别、DNS 与 redirect 合同，§9 补负例，范围仍属 S4。
- 两路认可冷启动无正文、封闭代理先于网络、唯一事件 owner 和独立受限 Tailnet 服务方向；无源码反例要求增加 profile bootstrap，维持 S1/S2 首个 PR 与 S3/S4 后续交付。
- 原始评审：[A](../_tailnet-mobile-recovery-design-review-subagent-v1.md)、[B](../_tailnet-mobile-recovery-design-review-codex-v1.md)。本轮只改设计并核对源码，未运行 App/设备验收。
- v2 两路复核均 APPROVE：[A](../_tailnet-mobile-recovery-design-review-subagent-v2.md)、[B](../_tailnet-mobile-recovery-design-review-codex-v2.md)，核对的设计 SHA-256 为 `1cd96e962f6036a8452e76732b5a5651fa2cb80ed5f505cd4775476589fbb452`。批准后仅更新状态及本条归档信息，未改设计合同。

### S3 实施窄裁决（2026-09-16）

- Orchestrator 批准部署级 `private-tailnet`/既有显式 Funnel 二选一；冲突报错，选定 provider 独占该部署的 PairingState；不要求同时开放两个入口。
- Tailnet 身份/desiredEnabled 属于 neige-app；窄 `calm-tailnet-control` client 只表达 status/enable/disable/login/logout。进程不跨 neige-app 重启 adoption；PDEATHSIG 加双层私有锁保证无孤儿 writer，kernel restart 仍保留节点进程。
- `neigeTailnet` 发布单元为 deferUntilFullReboot；app 启动固定 canonical helper，变更 binary 后只在原 writer 退出并独占 node 锁时备份。禁止自动恢复身份备份或猜测 tsnet state 版本兼容。
- 首个真实 HTTP 测试发现已配对会话可经主端口调用管理：原 Session 无凭证来源，owner() 只检查存在。改为必需 SessionAuthority，所有创建调用显式选择，所有 mobile 管理操作仅接受当前有效 PasswordLogin。业务入口仍接受有效配对会话；不扩为账号系统或持久会话。
- 桌面授权 URL 的 `displayForSeconds` 只限制当前 UI 展示，不宣称上游 URL 到期。手机扫码入网另按增补合同实现；不得把此桌面登录操作当手机流程。

- S3 源检追加裁决：固定 loopback 端口在 kernel 停机窗口可被其他本机用户抢占，且 bind 失败若传播会阻塞本机服务。改为同一 0700 目录内固定 `ingress.sock`，Go 始终 Unix dial，不从 HTTP 选择目标；RevocableListener 对 TCP/Funnel 与 Unix/private 复用同一取消语义。现存活跃 socket、非 socket、非私有目录不被 unlink；private setup 失败只降级远程入口，本机 kernel 继续启动。
