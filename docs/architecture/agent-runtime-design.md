# Agent Runtime 领域与接口设计

## 阅读指引

本文按“契约在里面，宿主在外面”的边界组织。第 1–4 节是目标、方案选择、模块归属和领域模型；第一部分是 `provider` 公共契约本身；第二部分是契约外由宿主拥有的状态与输入规则；第三部分是 R1 必须保持的逐 backend 现有行为；第四部分是迁移阶段与验证；第 19 节保留用户提出的 Q1–Q8，第 20 节记录后续六项意见的处理。

只想知道“契约长什么样”读第一部分；要实现迁移切片，第二、三部分是约束；第 19–20 节的决定已落实到对应章节；本轮修改需重新经过 L2 双通道设计评审，旧版通过结论不自动覆盖本版。

## 1. 目标与交付边界

将 Neige 当前 Codex、Claude、managed OpenCode 的接入收敛为中立的工厂、会话与能力契约，同时完整保留产品输入所有权、权限、持久恢复和任务结算。采用 OAR 的契约与证据分层方法，不引入 OAR 作为运行依赖。

本轮交付设计稿、覆盖映射与设计评审，不修改运行代码。设计按 L2 评审：涉及未来权限、执行资源与持久化边界；使用两路独立 Codex 评审，行为设计修订后两路重新审阅。

R1 是现有能力的行为保持迁移，不是实现 OAR 全部功能。R2 是独立的能力扩展候选。每个能力在 [覆盖表](runtime-capability-coverage.json) 中都有当前证据、目标 owner、R1/R2 或宿主归属；R2 的缺失不允许阻塞或删除本地已有能力。

## 2. 三种方案的选择

| 方案 | 优势 | 限制 | 采用范围 |
|---|---|---|---|
| OAR 当前架构 | 多 harness 公共 API、发现管理、原生记录与归属较系统 | 普通 Session 日志在内存；产品持久性、权限和仲裁归宿主 | 借鉴契约和行为测试，不直接替换内核 |
| Neige 当前架构 | 输入版本、持久回执、权限接管、历史控制、任务交付已有路径 | 公共输入受 Codex wire 牵引；协议与产品记录形状耦合；能力表达分散 | 保留真实 owner、事务和恢复机制 |
| 本设计 | 中立接口、按能力组合、明确失败证据和模块归属 | 收敛收益必须由调用点减少与行为保持证明 | 分 backend 替换，不并行维护两套运行真相 |

不对性能、可用性或完整兼容率排名；这些需要后续同负载测量。图形上的依赖整洁不能作为收益证明。

## 3. 模块归属与依赖

公共契约放在现有 `provider` 包内的职责模块，具体路径在实施时随已有入口收敛，不创建新 crate。协议实现继续由 `provider::{codex,claude,acp}` 拥有。`calm-exec::WorkerProvider` 与被动 worker flow 保持原职责，不强并为 managed Session。

**有持久事务、凭据、接管与进程所有权的 factory/controller 实现先留在 server 集成层。** 这是对前一版“先抽通用 provider 控制核心”的收窄：没有证据证明这些协调可以无损搬入 provider，就不先增加一套协调器。

| 模块职责 | 当前 owner | R1 目标 |
|---|---|---|
| 公共输入、结果、事件、能力契约 | `provider/input.rs`、`events.rs`、`held_requests.rs` | 只包含中立请求、具体操作结果、原生证据与不透明相关性；不包含 QueueEntry/旧快照/宿主停止结果 |
| 运行工厂与能力装配 | `harness/wiring.rs`、`backend.rs`、启动 operation | server 集成实现 `AgentRuntime`；复用原来启动 operation |
| 协议会话控制 | `PlannerBackend`、各 server native session | 对外暴露 SessionController；包装原路径，不另建 actor/FSM |
| 持久输入与产品调度 | `PlannerHarness`、queue、snapshot | 唯一队列、版本、折叠与调度 owner 不变 |
| 原生 transport、解析与翻译 | `provider` 原生模块 | 复用既有实现，不从私有会话文件布局猜协议事实 |
| 权限、审批与恢复 fencing | truth、role gate、thread seals、harness registry | 每次有权威效果前重新检验当前绑定，不把能力引用当授权 |
| 回执与 transcript 投影 | server integration、truth repos | 保留持久提交顺序、去重与现有 codec |
| 进程、沙箱与工作区 | 现有 supervisor、worker runtime、sandbox、workspace | 仅原 owner 可停止、回收或移交资源 |
| 查询和界面投影 | harness 查询、`fe/core` | R1 保留产品投影；可移植 runtime 投影另按需求提取 |

依赖方向为产品会话入口 → 公共契约，server 组合根 → 具体集成实现 → 原生协议。公共契约和 provider 协议模块不导入 Card、Track、SQLx 或 server。工厂实现可以依赖 server，因为它正是持久化与授权的集成 owner；不假装它是纯协议库。

判断一个概念属于契约还是宿主，可以问：换一个没有 Neige 数据库的宿主驱动同一个 agent，它是否也需要这个概念？需要的是协议事实，进入契约；不需要的是产品身份、持久化、权限或政策，留在宿主。Q1、Q2 的边界决定已落实：队列与旧格式证据归宿主，宿主停止结果不进入公共协议结果。

## 4. 领域身份与不变量

| 概念 | 含义及使用边界 |
|---|---|
| Conversation | 用户可见产品对话；沿用 Card 身份与显式历史重置边界 |
| WorkerSession | 持久运行与权限载体；以现有状态、绑定和接管规则决定有效性 |
| SessionController | 宿主集成门面，持有既有原生 session 的引用；不取得其协议状态的第二份所有权 |
| NativeBinding | provider、宿主配置/存储范围、原生 session/thread ID 的绑定；ID 本身不授权 |
| Input | 一条有证据可识别的用户输入实例；旧输入可以没有 message identity，不用内容相等推断同一输入 |
| QueueEntryRevision | 仅宿主：已有条目 ID 与 rev 的不可变引用；不新增修订表，不进入 provider 请求 |
| QueuedInputEvidence | 仅宿主：固定排队内容与实际存在的身份/寻址证据；旧格式分支不进入 provider |
| DispatchAttempt | 宿主的一次固定载荷投递；公共协议只得到其中不透明 CorrelationKey，不读取 operation/receipt 结构 |
| Turn | 一轮 agent 执行；使用已有本地相关性，原生 turn ID 仅在原生报告时记录 |
| TaskAttempt | 产品任务尝试；与 Turn 分开，provider 结束不代表任务验收 |

一条 Conversation 可经历多个 WorkerSession；同一卡的当前授权与原生写入 owner 由现有 fencing 决定。Controller 重建不自动生成新原生上下文，也不继承失效权限。**控制对象可构造不等于原生绑定已就绪。** 工厂返回允许后续安装和首次发轮的逻辑句柄，不能为满足接口提前 mint/spawn 或发送空 prompt。

多条输入可以合并成一次发轮；steer 可以加入已有轮次；同一逻辑输入可以有多次被记录的投递尝试。一次投递固定内容、目标，以及确实存在的身份和版本证据。一个 idempotency key 不能绑定不同载荷。产品幂等不证明原生服务实现 exactly-once。

---

# 第一部分 · 公共契约（provider 内）

## 5. 工厂与能力接口

以下是接口语义，不是可以直接编译的 Rust 签名；具体 async/object-safety 形式随现有调用点选择。不得为了实现示意代码增加平行协议。

```text
AgentRuntime
  describe() -> RuntimeDescription
  discovery() -> 可选的发现/查询能力
  open(OpenRequest) -> Session | OpenFailure
  resume(ResumeRequest) -> Session | OpenFailure

Session
  binding() -> BindingEvidence
  capabilities() -> 借用既有 session 的能力视图
  observations() -> 实时观察订阅，连续性显式描述
  provider_requests() -> 原生请求及一次性回应句柄

能力视图
  TurnControl.start_turn(TurnRequest) -> TurnStartOutcome
  TurnControl.interrupt(NativeTurnRef) -> InterruptOutcome
  Steer?.steer(NativeTurnRef, InputRequest) -> SteerOutcome
  Compact?.compact(CompactRequest) -> CompactSubmission
  HistoryEdit?.prepare(NativeEditTarget) -> EditPreparation
  ContextBreakdown?.read() -> 事实或未知/不支持
  ResourceRead?.read() -> 事实或未知/不支持
```

公共请求只携带固定载荷、中立配置、原生目标/计划与不透明 `CorrelationKey`。CorrelationKey 可以复用现有 client_id 的值，但契约不解释它是投影 key、operation ID 或队列地址，不据其格式推导身份，也不把它当幂等保证或权限。`TurnRequest` 不包含 Card、WorkerSession、QueueEntry、rev、LegacyUnversioned 或数据库行。

历史编辑的产品替换与原生修改分开。公共 `EditPreparation` 返回绑定原生上下文的计划；宿主按原 codec 保存 pending_rewind，后续 `TurnRequest` 选择 `NoEdit` 或 `ApplyNativePlan`，仍在既有 start 路径应用。R1 不再声明一个通用立即 `HistoryEdit.apply()`，以免把产品替换提交误称为原生修改成功。Claude 的 previous transcript rows 由 server 现有准备路径转换成原生 anchor，不作为公共契约参数。

`Session` 观察订阅的取消只取消该订阅，不暗含停止原生工作。自有进程终止、原有 hibernate/shutdown 与资源释放仍是宿主动作，见第 10、11、14 节；不在 provider 的协议能力里新增 `StopOwnedExecution`。

`AgentRuntime` 只描述会话工厂与独立的 runtime 查询，不把所有账户和机器管理操作塞入 Session。安装、升级、登录、登出和 provider 凭据写入是独立宿主管理动作；R1 不新增这些写能力。查询的 scope、观测时间、部分结果和 unknown 必须可表达；不把“没有读到”变成空列表或零额度。

### 5.1 绑定证据

`BindingEvidence` 是现有事实的只读视图：`Unbound`（尚无原生 ID）、`Recorded`（持久记录了原生 ID，未证明本连接已加载）、`Observed`（本次原生报告/握手已确认绑定，带连接代次）。它不新增持久 FSM；resume 可以返回 Recorded 的逻辑句柄而尚未加载原生。接口不以 Observed 为获得 start_turn 能力的前提，因此不会形成“先发轮才能取得发轮接口”的循环。

### 5.2 请求载荷与宿主准备结果分开

`OpenRequest/ResumeRequest/TurnRequest/InputRequest` 是中立数据，不表示宿主已授权，也不需要借用宿主数据库。固定载荷可复制，不等于复制了执行权。宿主的 `PreparedInvocation` 是第 11 节的私有调用内对象，不属于 provider 的输入类型。

R1 的 server 组合根先按现有 operation 准备 carrier、配置范围与授权上下文，再通过构造参数将它们绑定到本次宿主集成 factory/controller 视图。公共 `open` 不接收这些产品类型；具体实现也不从 CorrelationKey 的形状重建授权。共享的是 runtime 注册与可复用 transport，不是一份可变的“当前 Card”全局上下文。接口允许有范围的 factory 实现，不强制所有宿主调用共用一个全局工厂对象；已有 row/wiring 注入留在原 server 构造路径。

对于首次 Unbound → Observed，原生 ID 首次出现是同一当前 carrier 的预期建立过程；不能因自身绑定建立就拒绝首次发轮。已有原生目标被另一实例替换、cwd/配置范围不匹配时，仍由 server 原检查拒绝旧目标。

### 5.3 能力视图

`CapabilityViews` 是借用的能力入口集合，不是持久对象，也不另持有一份状态机。注册已能证明支持的 R1 能力可在首次原生启动前暴露。需要协商才能知道的能力用同一能力视图报告 `PendingActivation`，不能把尚未知误报为 Unsupported；只有已知不支持时入口为空且给出 Unsupported 原因。HTTP/UI 描述从该视图导出，不手写第二份支持清单。即使入口存在，本次调用仍可因原生状态、模型约束、旧目标或权限而拒绝。

基础 TurnControl 仅覆盖当前三个 managed backend 的共同操作；若引入没有打断能力的新 backend，应调整该能力的可选性，而不是实现永远成功的 interrupt。新 trait 只有在真实调用者需要独立能力时才增加。

## 6. 每个能力只有一种操作结果

**决定采用操作特有结果作为唯一公共返回词汇；不再把四态 ControlResult 作为所有能力的第二种返回类型。** 具体结果保留该操作所需的证据与原因，消费者只处理这一个值。四态仅可作为有实际消费者的宿主只读投影，M4 也不将它升级成通用重试依据；本稿未确定具名消费者，因此 R1 默认不实现该分类器。

| 公共结果 | 要保留的事实与区分 | 宿主映射边界 |
|---|---|---|
| TurnStartOutcome | Started；通信/准备失败；明确拒绝；认证未满足、原生报告恢复要求等带来源的原因事实；原生相关性与可证实发送阶段 | 必须逐分支保留当前 TurnStartFailure 的语义，不把 Transient 与 AwaitingRecovery 合成“失败” |
| SteerOutcome | 原生接管、明确未接管、无运行目标、结果未知；已知目标与原生回应 | 队列 CAS、entry_id/rev 和回队属于宿主 SteerCommandResult，不放进 provider SteerOutcome |
| CompactSubmission | 已提交；确定未提交；是否提交未知；观测基础 | submission 不是压缩完成；后续完成/边界仍通过 Observation |
| InterruptOutcome | 原生 ack、本地信号交付、没有可打断目标、未确认效果 | 不把本地 stop 接管或 driver join 变成原生 ack |
| EditPreparation | 绑定的原生编辑计划或明确准备失败 | 宿主历史事务、pending cut 和后续发轮返回分开 |

表中是需要表达的语义分支，不要求将相近分支复制成两份字段。每个操作结果用互斥变体及该变体必填数据表示；不另存一个可能与变体矛盾的 `category` 或 `retryable`。M1 需列出旧返回分支到新操作结果的一一映射，并证明 server 恢复到原 owner 决策时不丢信息；不能用 prose reason 或厂商名称猜回旧判定。

公共结果不包含宿主 hold/retry 指令。原生或 transport 可证实的认证/恢复原因用类型化事实表达；server 自有的 issuance_hold 在宿主入口处理，不冒充原生报告。server 以这些事实及自身准入状态无损映射到既有 Transient / Refused / AwaitingRecovery 分支，再由原 run loop 决定 hold。既有来源无法提供更细原因时保留其实际可证实的原因与来源，不默认标成“需要登录”，也不靠字符串重建政策。

`LocalWrite` 和 `NativeAck` 可作为协议侧观测基础；“请求可能已生效但没有回应”显式保留未知。`HostStopTakenOver`、process stop 返回和 driver join 是宿主 `HostStopOutcome` 的事实，留在第 11、14 节，不能作为公共 Accepted 的一种原生证据。

宿主只读显示分类见第 11.2 节，不属于另一种公共操作返回。

## 7. 观察记录

R1 的中立 Observation 必须可以无损承载本地现有事件变体及来源，包括文本、工具调用、结果、用量、轮次和 context replacement；旧 JSON 与数据库字符串继续由原 codec 输出。不是先把新类型序列化进旧表再补兼容。

公共观察 envelope 区分不透明本地关联 key、原生绑定、连接/流代次、该流位置、请求/turn 相关性、root/child/unattributed 归属、观测时间与原生证据引用。Card/WorkerSession/队列/receipt 的关联由宿主 envelope 包装，不作为公共协议字段。字段是否可缺由证据决定；不存在原生 ID 就不伪造。现有 worker capture position 与产品 envelope ID 不转换成同一个全局 seq。

原生子 agent 归属与产品 Task 委派是两张不同关系图。R1 明确保留当前“未观察/未建模”边界，不承诺补回历史；R2 才扩展 nested stream 采集与归属。新增采集必须发生在丢弃前，完成敏感数据与保留成本审计，不能事后从 Task 图推断。

实时订阅明确给出初始快照/开始位置与 gap。若当前 transport 没有回放能力，使用已存在的 snapshot/catch-up/receipt 路径，不宣称原生连续回放。新增订阅缓冲必须有容量策略；满载不能静默丢失负载关键终止和回应事实，必须使订阅失败或报告 gap，并进入现有恢复路径。

## 8. 入站原生请求

`provider_requests()` 是对入站观察的可操作视图：复用 `HeldRequestMessage/HeldResponder`，不另造原生 approval 通道。观察和该视图必须共享请求相关性与发生位置，先记录事实再交给宿主；显示的问题由适配层翻译，但权威 ask 与批准由内核创建和验证。

入站请求身份由当前 carrier、连接代次与原生 request key 限定。一次性回应句柄只能消耗一次。R1 的线性化点是原 `HeldRequests::take`：取消/结束/connection lost 先取走仍被持有的 responder，后续用户答复不再发送；回应先取走后，表内取消不能撤销该已取出的发送。用户决定事务与 take 之间取消获胜时，保留用户决定但不发送原生答复。

各 adapter 保留现有后置守卫：ACP 的 pending/fenced 检查可能进一步拒绝已取出的回应；Claude 的异步写或 Codex ReplyPort 不被升级为“所有取消均先于回应生效”的强保证。晚到的答复不产生新授权，原生是否接受仍由证据决定。断连不证明远端请求被拒绝。重启不反序列化闭包，不恢复失效 responder；须等原生重询或报告答复效果未知。

当前 `HeldResponder::respond` 不能证明原生确认收到了答复。持久 ask 的用户决定与原生答复效果分开；R1 没有跨 backend 的 durable reply-delivery receipt，不能从不存在的 receipt 声称已记录确认。没有可用投递证据时只报告 Unknown/NotObserved；若新增 durable 答复回执或取消优先协议，另做行为设计。

## 9. 配置与外部能力

配置分期望值和原生报告值。类型化 Open/Turn 配置至少保持本地 model、effort、instructions、输入模态、工作区、MCP 与权限模式的现有语义；权限和环境允许列表仍由宿主决定。每个配置项定义 scope、变更时机、缺省含义和可观察结果，不用通用 JSON extras 暗藏政策。

service tier、原生会话名称、最终输出 schema、任意附加 MCP、原生 inventory 等未有统一实现的功能属于 R2。schema 传递、原生结构化结果和宿主验证必须分别建模，不能把 MCP 工具 outputSchema 当成 agent 最终结果约束。原生工具列表重载不自动证明模型下一次请求已经刷新。

发现/查询可按需提供 installation/readiness、models、inventories、accountUsage。Harness 登录与模型服务商凭据管理是不同 owner；后者不能由一次会话 open 隐式写入。查询只报告原生或宿主有证据的事实，不能查宿主私有目录命名规则来补齐契约。

## 10. 执行资源形态

资源绑定是说明能力和责任的事实，不是授权 token：

| 资源形态 | 允许的通用行为 | 终止/回收 owner |
|---|---|---|
| OwnedProcess | 可观测已准入子进程；停止须精确匹配身份与租约 | 原有 supervisor/启动集成 |
| SharedProcess | 释放本绑定、按原生协议打断目标轮次 | daemon 生命周期 owner；单个 Session 无权 kill 整个 daemon |
| BorrowedHarness | 可停止观察；打断工作需该宿主支持和授权 | 借出 Harness 的宿主 |
| InProcess | 无独占子进程保证；读取资源可能不可用 | 承载该 SDK 的宿主 |

本节只定义可跨宿主解释的资源形态与观测限制，不授予终止权。provider 不提供通用 `release(StopOwnedExecution)`。停止/退役接口归宿主资源 owner，返回 HostStopOutcome；只有它可以记录已请求停止、原 stop 返回、driver join、未知/超时。打断原生 Turn 仍是协议操作。取消观察订阅与宿主关闭 transport 分开；对象 Drop 不承诺业务完成、工作继续或进程终止。Borrowed/InProcess 仍为 R2 扩展形态。

---

# 第二部分 · 宿主侧 owner（契约外）

## 11. 状态协调的唯一 owner

Controller 是现有 backend 控制路径的门面。R1 不在 provider 增加通用 actor、轮次调度 FSM、第二份 durable queue 或第二份 busy 标志。

- `PlannerHarness` 保留产品队列、Issuing/Running/Wedged 等调度状态、冻结预算和回执投影进度。
- 现有 backend 实现保留连接代次、wire pending requests、原生报告的 active turn 与协议退出证据：Claude/ACP 为各自 session，Codex 包含共享 app-server 的按线程状态。NativeBinding 只是身份，不是这些状态的 owner。
- Controller 的能力入口复用原先的 issuance/operation 同步边界；不得拿到裸 transport 后绕过它。

这些是不同状态轴，不互相覆盖。连接断开不等于原生轮次结束；UI idle 不决定权限；产品已记接管回执不证明原生已收到；观察到原生 turn end 不替代进程清理证明。

start、steer、compact、interrupt、宿主历史替换事务与后续 ApplyNativePlan 发轮的互斥规则从现有入口保持。R1 不扩大并发许可：目标 turn 必须仍匹配；历史准备、宿主事务提交与后续发轮各沿原入口校验；native completion 可以晚于取消决定到达，仍记录证据而不重新激活旧权限。

工厂和宿主安装严格保持现有顺序：`PlannerFactory::open` 构造控制对象 → `PlannerHarness::run` → registry 的 `reservation.install/mark_installed` → 原有 snapshot/issuance 路径。Claude/ACP 保留安装前的 start_turn 拒绝守卫，安装后首次发轮内才按原路径 mint/spawn、new/load 和绑定。Codex 保留原 thread 准备位置及认证/seal 检查，不为统一接口新增 installed 门槛，也不强制全 backend 统一成 eager 或 lazy open。

### 11.1 宿主私有准备范围

`PreparedInvocation` 不导出到 provider，不实现 Clone、Serialize、Deserialize 或 Default；构造器仅由当前 Controller 的宿主入口调用。它绑定所属 owner 与本次固定请求，只在现有 issuance/operation 同步作用域内按值消费，不能取出后存入队列、用于另一 Session 或持久恢复。优先借用所属 SessionBinding 和原 guard；不要求把 guard 移入对象或移交新任务。

现有 run_loop 的 issuance 为 tokio::sync::Mutex<()>，已在原函数持锁跨 await；因此“guard 必然非 Send”不是当前路径的事实。但包装借用的 async/Send 约束仍需 M2 在实际入口编译确认。若借用 guard 使 future 不满足既有调用约束，可让 guard 留在原函数局部作用域，准备对象仅私有构造、绑定 owner、不可 Clone/序列化并按值消费；准备和 execute 不得逃出该作用域，也不得提前释放锁。此收窄不减少 live 检查或改变同步边界；若仍做不到就暂停切片，不以放宽锁范围补救。

私有调用形状为 `Controller::dispatch(request)` 内部建立 scope，再调用 `PreparedInvocation::execute(self)`；execute 从对象保存的 owner 引用取目标，不接受另一个 Session 参数。编译约束验证针对这套实际入口，不能仅用“两个 Session 恰好有相同 Rust 类型”就声称类型系统证明了对象身份。第 17 节规定验证方式与维护成本，不引入专门的新测试依赖。

不可 Clone 和生命周期只是防误用，不是权限证明：同一借用仍可能跨 await 后已失效。server 必须在现有同步边界内、每次原生效果前，复检当前 carrier、scope/config、seal 和原来的 task-context；沿原迟到结算规则记录事实，不让旧调用取得新效果权限。不得因类型包装新增锁、强制串行范围或比旧入口更强的恢复保证。

宿主 `HostStopOutcome` 单独承载 stop 请求、本地清理与 join 的事实；它不装进公共操作结果，也不自动改变 registry 接管规则。第 14 节继续保留各入口的弱保证。

### 11.2 宿主的四态显示分类

本稿尚无具名 UI/聚合消费者，R1 默认不新增四态类型、分类器、存储字段或事件。若具体切片证明既有消费者需要，才由宿主从上述**唯一操作结果**就地派生 NotSubmitted / Refused / Accepted / Unconfirmed，并在该切片列明消费者与所需显示，不建立公共分类服务。分类仅供显示与聚合。Accepted 的协议依据必须说明 LocalWrite 或 NativeAck，不能证明模型采用；四态不写成第二份 runtime 状态，不持久双写，也不替代第 13 节 operation-specific 判定。历史宿主停止提示可显示单独的宿主状态，不混入协议分类。

超时、断连与 partial write 不能仅凭错误文本被归为确定未提交。当前路径没有暴露更细发送阶段时保留未知，不为统一字段重造证据。宿主仍按第 13 节保留既有 retry/requeue、hold 或 Wedged；分类本身不发起也不禁止重试。

## 12. 输入所有权与旧输入

本节的 Input/QueueEntryRevision/QueuedInputEvidence/DispatchAttempt 均为宿主模型，不是 provider public API。产品通过既有幂等入口提交，成功返回现有 durable receipt；server 先固定 claim/版本证据，再将 CorrelationKey 与固定载荷交给公共请求。ACP 的冻结 queue claim 留在其 server receipt 集成，不进入 provider::acp 的 wire 请求。公共操作结果不替代该回执。QueueEntry 所有权转移、回退、修订 bump、projection key 和 message identity 继续由同一队列 owner 处理。

输入状态不建成一条必然前进的枚举链，而保留独立证据：产品持久接管、已发出、原生回显/ack、明确采用关联、原生结束。缺少采用证据仍是未知；最终文本不能反向证明具体输入进入了模型。

旧快照的 `QueueEntry::LegacyUser` 仍是支持输入：它没有条目 ID/rev，message identities 也可能为空。`QueuedInputEvidence` 使用互斥寻址分支 `Versioned {entry_id, rev}` 或 `LegacyUnversioned`；身份独立使用 `Known {message_ids}` 或 `LegacyUnknown`，不要求版本和消息身份同时存在。每个已知分支字段是必填的，未知分支没有可供 backfill 的字段。批次可混合两种输入，投递至少关联一份内容证据，但关联零到多个已知 QueueEntryRevision。

R1 按现有 snapshot/queue decoder 产生该证据，不填造旧条目的 ID、rev 或 message identity，不拒绝原可发轮的旧队列。`projection_client_id` 是批次投影/请求相关性，不得冒充旧条目地址。只有现有 transfer 边界按原规则 mint 的 message identity 才能被采用；它不使 LegacyUser 变成可按 ID/rev 编辑的输入。源码依据为 `harness/queue.rs` 的 LegacyUser 分支、`snapshot.rs::pending_entries` 与 `run_loop.rs` 的 projection key/drain 路径。

---

# 第三部分 · R1 行为保持约束

本部分的矩阵是保留现状的迁移约束，不是对这些行为安全性的新增背书。已知弱点的独立行为设计次序见第 16.3 节；不并入 R1。

## 13. 命令结果逐 backend 现状

| 操作 / backend | 现有证据与判定 | R1 原 owner 的动作和保证 |
|---|---|---|
| start / Codex | 明确 CodexRefused 与其他 Transient 分开；传输错误未必确定未发 | `codex_turn_start_failure` → harness `apply_refusal/rebuffer_head`；保留原节奏的自动再尝试，认证 hold 仍按原判定阻止；不承诺 exactly-once |
| start / Claude | `start_spawn` 内有 line_sent，但普通 turn_start 丢弃该细节，只返回原 failure | 保留原 harness 回队/重试。部分写的潜在重复与非持久未知窗口仍存在；不能宣称已具 ACP 式 receipt 或偷偷改变重试 |
| start / ACP | 已冻结 receipt 精确绑定原输入；同 key 可回读未确认/完成结果 | 原 ACP owner 退休原批次而不再写原生；有 unresolved receipt 时拒绝新输入并要求显式 reset。不是所有 backend 共用的政策 |
| steer / Codex | NotTaken 是明确拒绝；Unanswered 是结果未知 | 原 `handle_steer` 均把输入放回队列，未知输入仍可能下一轮再发；保留重复风险与既有提示，不改成全局永久 hold |
| steer / Claude、ACP | 当前不支持 managed steer | 沿原准入拒绝并保持队列，不产生原生写入 |
| compact / Codex | 原生拒绝为 NotSubmitted；其他可能已发错误为 Unconfirmed | 原 `handle_compact`：前者恢复先前 phase，后者记 compact Unconfirmed 并 Wedged，reset 后恢复；不自动重试 compact |
| compact / Claude | compact 专用路径保留 line_sent，只有可能已写才 Unconfirmed | 同一 `handle_compact` 政策；与普通 Claude start 的错误分类不同，不混成一种恢复语义 |
| compact / ACP | 当前不支持 | 原准入拒绝，无原生效果 |

改变 Codex/Claude 未确认重发、建立新的 durable dispatch receipt 或跨 backend 的统一恢复政策，均属于独立行为设计/前置切片，不在 R1。

## 14. 停止路径现状

R1 仍调用原来的 hibernate/shutdown/teardown 路径，**仅报告原路径确实具备的停止证据**。普通 replacement 与 deletion/ACP recovery 的保证并不相同：

| 路径 | 当前停止证据与弱保证 | R1 接管行为 |
|---|---|---|
| harness 普通 `shutdown()` | `shutdown_inner(false)` 等 issuance 边界、persist snapshot、调用 backend，abort run loop 但不 join；backend 停止错误可被普通路径吞掉 | registry 仍按原 `shutdown()` 返回和实例 identity CAS 接管；Ok 不等于通用 quiescence，不新增“必须完全清理”门槛 |
| Codex backend | 按缓存/后备 turn 调 interrupt，释放原 approval route；共享 daemon 不因本 Session 被销毁 | 保留原错误传播层次；不从 interrupt ack 推断原生 Turn 已结束，不把共享 daemon 标为停止 |
| Claude backend | 原 stop 结果返回给 strict 调用者；等待 settled 超时只记日志，driver 是独立 spawn，未通用 join | 保持普通 replacement 现有弱保证；不得输出 Joined/Quiesced 事实。加强 join 或严格阻止接管是另一个行为切片 |
| ACP backend/recovery | 原 session shutdown 保留并 await driver；取消不能丢其 handle，错误按原顺序返回 | 保留 ACP 自有的 join/recovery 边界；普通 harness 外层是否吞错仍沿原入口，不推广成全局保证 |
| deletion 的 strict 路径 | `shutdown_for_deletion` 要求原 strict interruption 成功才保留 seal | 保持原来的 fail-closed 与错误传播，不将普通 replacement 的弱规则套入删除 |

registry 在调用前驱 shutdown 期间保留可发现 slot，并在返回后核对实例；这一顺序保留，但普通 shutdown 的 Ok 不被重命名为“前驱已完整静止”。

## 15. 持久与恢复

R1 不新增或改写数据库迁移、持久 wire、snapshot 版本、operation key 或 transcript 身份。新中立类型通过存储 owner 的边界映射到既有记录。若无法保留语义，该 backend 暂不迁移，不加 silent fallback。

| 中断窗口 | 恢复依据与动作 |
|---|---|
| 尚未提交产品输入 | 既有输入幂等事务，重试相同 key |
| durable 接管后、原生发送前 | 既有队列/receipt 中已冻结的 claim；只由当前 owner 调度 |
| 部分写入或原生 ack 丢失 | 按第 13 节矩阵保留原 retry/requeue 或 ACP receipt；没有 receipt 的路径不冒称 durable unknown。门面不新增重发，也不取消原 owner 的重试 |
| 原生完成已入 receipt、投影尚未落地 | 按现有 receipt 恢复投影，不重新执行 |
| 用户回应已存、原生响应未确认 | 用户决定不回滚；原生效果按可用证据报告 Unknown，不伪造 durable reply receipt；保留原询问/恢复行为 |
| 请求已取消或旧连接结束 | 仍在 HeldRequests 的句柄由原 take/sweep 失效；已取出的回应保留 adapter 的竞态与后置守卫，不赋新连接权限 |
| 停止中途进程清理失败 | 普通 replacement、strict deletion、ACP recovery 分别沿第 14 节原入口的传播/接管规则；不能统一禁止接管或统一宣称静止 |
| 历史准备后又有输入/轮次变化 | 宿主替换事务提交前按原版本/phase 复检，失败不提交；后续 ApplyNativePlan 发轮仍按原入口校验。已提交的产品事务不因后续原生失败被冒称自动回滚 |

不统一重写各 backend 的恢复语义。Codex Unanswered、Claude partial write、ACP 未确认 receipt 的差异以第 13 节矩阵为准。history 的产品替换事务与原生 pending cut 也不是分布式原子提交：原生 cut/start 成功、snapshot 尚未写入的现有崩溃窗口继续保留，不能把宿主历史事务提交或后续发轮返回成功解释为产品与原生历史的原子提交。改变这些限制均另行设计。

---

# 第四部分 · 迁移与验证

## 16. 分阶段迁移与退出条件

| 阶段 | 修改范围 | 退出条件 |
|---|---|---|
| D 设计收敛 | 本文、覆盖表、图与 HTML | 两路独立 Codex 无开放阻塞；源码与图一致 |
| M1 中立类型 | provider input/event/request 的边界类型；原 codec 映射 | 所有原事件/输入有对应；现有 wire/存储 golden 不漂移；不改变原生调用 |
| M2 Codex 试点 | 先迁移现有 Codex managed 入口与恢复路径 | 第 16.1、16.2 节的行为保持、边界收益和退场条件满足；不新增 owner |
| M3 ACP 然后 Claude | 用不同恢复语义检验同一契约；一次一个 backend | 各自矩阵通过；公共消费不加厂商分支，原 owner 保持，旧并行路径同切片删除 |
| M4 统一能力消费 | 上游只消费操作结果与同源能力视图 | 删除迁移用旧结果桥接和被替换入口；四态只在有具名消费者时作宿主只读投影，默认不实现；旧 wire/codec 按原 owner 保留 |
| R2 能力扩展 | 覆盖表中标记的新增能力，逐项独立实施 | 原生探针与协议证据、行为测试、资源成本与安全 owner 均具备 |

回退 M1/M2 的代码必须仍读取本阶段已写的同一旧格式。R1 不双写 shadow truth、不引入长期兼容分支；每个切片在当前 owner 内替换。如果需要改变旧 snapshot、原生 ID 绑定、权限或业务恢复规则，就停止把它称为行为保持切片并另提 L2 设计。

### 16.1 M2 选择 Codex

先选 Codex，不以未经测量的“流量最大”作为依据。源码已证明它同时覆盖 steer、compact、history、approval 路由、共享 daemon、未知回队和普通停止弱保证，最能检验接口是否真正隔离产品与协议。M2 是保持这些路径的门面迁移，不改变其重复风险。

ACP 作为 M3 第一个对照，用其 lazy new/load、durable receipt、不重发与 driver join 检验公共契约是否误装成 Codex 政策；然后迁移 Claude，核对 per-turn process、普通 start/compact 的不同 partial-write 证据。当前顺序是本轮迁移计划，不是通用层的永久厂商闭集。

### 16.2 边界收益与停止条件

M2 开始前，对目标 Codex 方法族和真实调用者作一次源码基线，作为该切片评审附件，不加固定文件/测试名 CI pin。记录：越过 owner 的厂商分支与能力清单、传输/存储/队列类型穿过公共签名的位置、每种状态/队列/回执 owner 数、原生发出入口及其生产/恢复调用者。

退出需同时满足：

1. 迁移调用者通过中立请求和操作结果使用能力，公共签名没有 Card/QueueEntry/rev/旧快照类型；Codex-only wire 编码与策略分支仍在明确 owner。
2. 被迁移调用路径全部切换并删除原平行入口；不得留下“新门面只用于测试、生产继续裸调”的状态。未迁移 backend 的旧实现不计为该切片复制。
3. 新增队列/FSM/busy/回执 owner 为零；能力描述不新增第二份静态清单。
4. 实测消除所选范围内至少一类跨边界依赖或重复消费机制，并给出前后调用者证据。若基线已没有该问题，不能用机械更名或增加包装层冒充收益，须改选范围或停止该试点。
5. 原第 13–15 节的行为测试、错误映射和旧 codec 保持验证通过。仅减少 PlannerBackend 内部匹配数量不算收益；owner 内合理的原生分支无需归零。

若 M2 只有成本增长而不能证明边界改善，先收窄或取消后继 M3，不自动铺开。具体数值以该切片基线为准，不在设计中编造尚未测量的计数。

### 16.3 已知弱点的独立后继顺序

H2 未确认投递可能造成用户可感知的重复效果，优先整理设计与证据；H1 可并行设计，不再要求全部 R1/M3–M4 结束后才处理风险。编号是主题身份，不代表实施先后。以下只规定设计依赖，不是本轮已创建的 issue 或获准实施的任务。

| 优先关系 | 独立 L2 设计 | 实现依赖与验收边界 |
|---|---|---|
| H2 优先，H1 可并行设计 | Codex/Claude 未确认投递与潜在重发，评估原生去重、宿主 receipt 与用户恢复语义 | M2 固定 Codex 的行为/恢复基线后，即可另立 H2-Codex 行为切片，不等 M3–M4；实施仍需独立授权、L2 与原生证据。Claude 按其当前 owner 单独固定基线，不必等迁移才调查。start/steer/compact 分别验证，不承诺一般 exactly-once；此前不扩展自动重试政策 |
| H1 按资源接管依赖推进 | 普通 replacement 的停止/接管证明，明确是否加强 Claude join 与错误传播 | 先明确所需停止事实；要求更强静止保证的接管功能以 H1 为前置。与 H2 涉及同一 owner 时串行实施并重定基线，不并行改恢复政策；不得把 wait 超时当证明 |
| H3 后继，必要时独立提前 | durable 回应投递证据与取消/回应竞争的可观察结果 | 以现有 take 线性化为基线；改变取消优先或授权作用域须独立授权与测试，不能只加日志声称可靠答复 |

H2 等行为改造必须与 R1 门面迁移分成不同切片。若插入行为修复，先暂停该 owner 的迁移，完成修复及评审、固定新基线，再继续 R1 并更新第 13–15 节矩阵；不能让行为保持测试继续以旧政策为准。若出现需优先处理的独立运行缺陷，按风险调整顺序；不编造没有资源估算的日历日期。

## 17. 验证与维护成本

设计阶段运行文本门禁、文档链接和图源一致性检查，以及 HTML 浏览器检查；不以文档验证冒充原生能力证明。两路 Codex 分别从接口/覆盖与权限/持久恢复审阅，但每路都必须检查仓库的五项架构优先项。

实施阶段由具体切片决定一次验证计划：worker 跑聚焦测试并修复格式/lint；kernel gate 跑 text/contract/quick 与一次拒绝空选择的聚焦 replay；CI 跑广域套件。真实 Codex E2E 不在共享宿主运行。仅对权限、重放等关键断言按仓库规则做 mutation。

必须覆盖的场景族包括公共契约拒绝依赖宿主队列/权限类型、宿主准备对象不能复制/跨 Session 使用、每种操作结果无损映射且四态不参与政策，以及：构造/安装/首次发轮的懒启动顺序、Unbound/Recorded/Observed 区分、旧快照无版本/无身份与混合批次、能力未知与明确不支持、输入合并和过期 rev、按 backend 区分的部分写/丢 ack、迟到 completion、旧实例调用、普通/strict 停止差异、ACP 投影恢复不重发、历史准备、宿主替换事务、后续 ApplyNativePlan 发轮与 pending cut 崩溃窗口、HeldRequests take 的取消/回应竞态、gap 与归属缺失、配置缺省/实际值差异。场景族不是新增永久测试名称清单；测试加到现有所属 target。

准备对象在 M2 通过实际 crate 的编译与原模块测试验证：正例必须经过实际 async/Send 入口，并保留 owner 与锁作用域；私有构造、无 Clone/序列化实现、无第二 owner 参数由实际 API 及源码审查确认。不把尚无可运行入口的编译负例写成已验证保证，也不为此新增 trybuild、公开私有类型或另建 rustdoc 编译框架。若既有测试入口能直接承载针对实际 API 的负例，可在切片中增加；必须同时有可编译的配对正例，不能把私有项不可见等无关错误当作目标约束成立。没有这一条件则收窄验证承诺，保留原模块的行为与权限测试，不声称类型系统证明完整不可转授。测试归现有 target：worker 聚焦、gate 一次回放、CI 按原 target 执行；普通 API 变化只同步所属模块及其测试，不加固定测试名清单。

覆盖 JSON 是这次设计的评审证据，不加入 CI 的固定行数或固定 provider 列表断言。新增 backend 通常修改注册、原生实现、已有行为套件，不要求 UI 增加厂商分支。未来能力或字段的增加应在所属契约、实现及消费者变更，不通过复制 registry/golden 清单增加维护点。

## 18. 参考与图源

- 源码基线：Neige `17373c0716195fd328289f7f50437b83d8274cde`；OAR `c88e551ea7635ed975150aa36f279367523f97e4`。
- [模块与接口的可视报告](oar-runtime-comparison.html)，以及 [模块](runtime-modules.mmd)、[接口](runtime-interfaces.mmd)、[能力](runtime-capabilities.mmd)、[身份](session-identity-model.mmd)、[投递](runtime-delivery-model.mmd) 图源。
- [OAR 设计](https://github.com/botiverse/oar/tree/c88e551ea7635ed975150aa36f279367523f97e4/docs/design)、[OAR 公共契约](https://github.com/botiverse/oar/tree/c88e551ea7635ed975150aa36f279367523f97e4/packages/oar/src/contracts)。
- 本地 owner 依据：`harness/backend.rs`、`harness/queue.rs`、`harness/snapshot.rs`、`harness/registry.rs`、`provider/held_requests.rs`、`acp_planner/session.rs`、`claude_planner/session.rs`、`operation/driver.rs`、`provider_registry.rs`。

---

# 用户问题的处理

## 19. Q1–Q8 的决定与落实位置

保留用户提出的问题作为设计依据。以下决定已纳入本版；本版变更了边界与迁移条件，必须重新经过 L2 双通道设计评审。旧版 `695cdf3` 的通过仅适用于旧版，不自动覆盖本版。

| 编号 | 用户问题 | 本次决定 | 落实与验证 |
|---|---|---|---|
| Q1 | 队列与旧格式证据越过契约边界 | 采纳：这些类型仅归 harness/server；公共输入只有中立载荷、原生目标与不透明相关性 | 第 3–5、7、12 节；M1 检查公共字段/依赖，保留 ACP server receipt claim 和 LegacyUser 行为 |
| Q2 | HostStopTakenOver 混入协议 Accepted | 采纳：HostStopOutcome 留宿主；移除公共 StopOwnedExecution，协议结果只报告原生/传输事实 | 第 5、6、10、11、14 节；禁止把本地 stop/join 当 NativeAck |
| Q3 | 四态和操作失败类型并存 | 决定操作特有结果为唯一公共返回；四态仅在有具名消费者时作宿主只读派生，默认不实现，M4 不转为通用判定 | 第 5、6、11.2、13、16 节；M1 证明分支无损，M4 删除迁移桥接而非长期双轨 |
| Q4 | 准备结果不可转授仅靠文字 | 采纳类型约束并收窄其保证：PreparedInvocation 私有构造、绑定原 owner 且按值消费；优先借用 guard，也可让 guard 留在原调用作用域，保持锁范围与 live 复检 | 第 5.2、11.1、17 节；编译约束加运行时 live 复检，不把生命周期当权限证明 |
| Q5 | 图与正文的状态所有权冲突 | 采纳：现有 backend 实现拥有协议状态；Controller/能力视图引用原对象，兼容 Codex 的共享 app-server，不组合拥有第二份状态 | 第 11 节与接口图/HTML；图文所有权逐项比较 |
| Q6 | 门面缺可测量收益 | 采纳，但不以 owner 内厂商分支总数为目标；以越界依赖/重复机制消除、原路径退场和零新增 owner 判定 | 第 16.2 节；M2 前作基线，收益不足停止/收窄 M3，不加永久计数 pin |
| Q7 | 已知弱点没有排期 | 调整为 H2 优先、H1 可并行设计；M2 固定 Codex 基线后即可独立推进 H2，不等 M3–M4；行为修复与 R1 分切片、重新定基线 | 第 16.3 节；新能力在相应硬化前不得声称更强保证，不并入 R1 |
| Q8 | 首个 backend 未定 | 选择 Codex M2，ACP 后 Claude M3；依据协议/恢复差异和可检验边界，不使用未经测量的流量判断 | 第 16.1 节；先验证高覆盖试点，再用 durable 与 per-turn 模型作对照 |

## 20. 后续六项意见的处理

1. **H2 时机：采纳提前。** 第 16.3 节取消等待全部 R1 的要求，先固定 owner 基线，再独立改行为；本轮不启动实施。
2. **四态消费者：采纳收窄。** 没有具名消费者则 R1 不实现，只有实际显示需求才就地派生（第 6、11.2 节）。
3. **历史术语：采纳。** 明确区分原生准备、宿主替换事务与后续 ApplyNativePlan 发轮，删除含混的 history apply 用语（第 11、15、17 节）。
4. **guard 与验证成本：部分采纳。** 现有 Tokio guard 已跨 await，不能判定必然不可行；补充保持原锁范围的局部准备对象收窄方式，以及无需新依赖的编译示例计划，不放松 owner/live 检查（第 11.1、17 节）。
5. **恢复 hold：采纳边界修正。** 协议表达带来源的认证/恢复原因，server 自有 hold 留宿主，不由公共结果下达政策（第 6 节）。
6. **空行：修正。** 第 5.2 与 5.3 节之间补分隔。

本版含设计和排程语义修订，须重新经过两路独立 Codex 完整复审，旧版通过结论不覆盖本版。
