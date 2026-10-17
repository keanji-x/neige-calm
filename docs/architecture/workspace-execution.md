# Workspace execution（S2a 实施合同）

状态：S2a「registered workspace execution」的重设计，替代 PR #2770（`codex/2666-terminal-vertical`，用户已否决）。第四版
是**范围收窄**，不是又一轮补丁：第三轮两路 L2 评审（codex、claude，均“需修改”）的三类阻断（Legacy 共存、失去活见证后的破
坏性 release、唤醒路由）已连续三轮复现；按 AGENTS.md 三轮规则，前两类都来自 S2a 试图拥有 **资源**，本版只拥有**执行**
（Planner 决定 S1–S5，处置见 §13）。只是设计，不声称已实现；等级 L2。父合同为 [track-workspace.md](track-workspace.md)；
本文只取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的少量条款（§9）。英文类型是合同草案。main 的
executor 协议（#2716）仍是执行物理事实与 currentness 的 owner，`Executor` port 是它的薄投影（§4）。S2a 不建 typed 资源、
不改变 Track worktree 的任何 Legacy 用户、不驱动任何破坏性 source effect。

## 1. Outcome 与范围

1. 新 crate `workspace-runtime` 的 `execution` 模块拥有执行的**动作政策**：按 `Desired` 与 executor 报告的事实决定下一步
   副作用；只经存储与 executor 两个 port，不依赖 calm-server 即可测试。
2. 物理事实与 currentness 只有 executor 一个 owner：进程、journal、输出、alias slot、执行私有 registration（§7.1）。
3. 正常驱动与崩溃恢复走同一个 `Engine::drive`，由提交后的 `DriveTicket` 与 executor 变更代次唤醒（§6.3），不轮询。
4. 一个 migration、一张表 `workspace_executions`（§10），不建阶段镜像。
5. 首个消费者是 opt-in Terminal（typed `Config`，默认关闭）：cwd 与 Legacy 终端**同一**选择，与资源的交互和 Legacy 终端
   对等（§7.2）。未启用时行为不变。

范围外：track-workspace S2a 行的后续里程碑 **resource cutover**、borrowed/Legacy adoption、resource task-admission
cutover、workspace_service 迁入 `workspace-runtime`（均见 §11）；同宿主 Unknown 的人工出口、精确屏幕重建、S2b/S3/S4/S5、
C4 Area 共同执行授权。

## 2. 诊断

依据 `git diff origin/main...origin/codex/2666-terminal-vertical`：`Export/ExportAck`、`Cleanup`、`RetireRegistration`、
`durably_settled`、`permits_handoff` 与 `apply_event` 是 **#2716 已合入 main 的 executor 代码**，故 E1a 修改它们及其测
试。前版诊断 R1–R4 仍成立：#2770 没有执行动作的 owner；main 的 `Process` 把 child、PTY、pidfd 与输出 ring 绑在一起
（`process.rs:20-27`）；`apply_recovery_item` 不持 `drive_mutex`（`driver.rs:396-404`，另开 issue）；`apply_event` 仅凭
helper 报告写 Drained（`mod.rs:601-621`）。**R5（第三轮）** main 上每个 Track worktree 都是 Legacy selection
（`workspace_resource.rs:217-256` 同时写 `track_workspace_bindings`），Planner、worker、gate 经 server 直接 spawn 在其中
运行（`claude_planner/session.rs:476` 等）。给这个资源加 fence、Legacy 拒绝与 typed release，每轮都在“其他用户仍是
Legacy”或“executor 失去活见证”处重新开洞。

## 3. Crate 边界与依赖方向

```
workspace            (叶子机制库：身份类型、HeldSourceObjects、物化/回收机制)
   ▲          ▲
calm-session  workspace-runtime   (kernel 侧 workspace 框架；S2a 只含 `execution` 模块)
   ▲              ▲  (依赖 workspace、calm-session 协议类型；feature `session-executor`：
calm-proc-supervisor   Executor port 的协议适配器；feature `test-support`：fake 与合同套件)
   ▲ (dev-dep)    ▲
calm-server ──────┘ (SqliteExecutionStore、Terminal 消费者、HTTP/WS/auth)
```

依赖只有 `workspace`、`calm-session`、`async-trait` 与 async 原语，无环。不合成一个大 crate：calm-session 依赖
`workspace` 的类型，若 `workspace` 再含依赖 calm-session 的执行政策即成环；executor（calm-proc-supervisor）也依赖
`workspace`，不能链接 kernel 政策。后续一片把 `calm-server/src/workspace_service` 的政策（resolve、生命周期、占用、物化/
回收编排）经 port 迁入 `workspace-runtime`，calm-server 只留 HTTP/WS/auth/DB 适配器（§11，S2a 之外）。

| 层 | 拥有 | 不拥有 |
|---|---|---|
| `workspace_runtime::execution` | `ExecutionIntent`、`Desired`、`next_step`、`Engine`、每 ExecutionId 一个驱动任务、两个 port、`session_executor` 适配器（组装 wire、维护变更代次） | 授权、路径选择、资源生命周期、SQL、HTTP、渲染 |
| calm-proc-supervisor | journal、settled 谓词、alias slot 与 tombstone、执行私有 registration 与其 fence、native child/PTY/pidfd、`OutputLog`、stop 升级、变更流 | 领域删除、授权结论、资源 |
| calm-server | 路由/WS、auth/C1 结论、`SqliteExecutionStore`、Terminal 消费者（cwd 选择同 Legacy）、既有 disposal/clean 路径、renderer 订阅输出 | 执行状态推导 |

## 4. 公共 API 草案

所有 port 用 `#[async_trait]`（同 `operation/tx_only.rs:30`），trait 带 `Send + Sync`，future 为 `Send`。wire 类型直接复
用 calm-session `control/execution.rs`。

```rust
pub struct ExecutionIntent {             // 不可变
    pub id: ExecutionId, pub alias: ProcessAlias, // alias 如 "term:<id>"
    pub owner: OperationRef, pub track: TrackId,  // track 是占用谓词的键（§7.2）
    pub source: PrivateSource,
    pub program: ExecutionSpecification, // main 类型：argv/显式 env/io_mode/replay_bytes
    pub ticket: LaunchTicket,            // { id: LaunchTicketId, secret: CapabilityToken }
    pub tokens: ExecutionControlTokens,  // main 类型：observe/interactive/lifecycle
    pub replaces: Option<ExecutionId>,   // 显式 alias 交接；不含 slot 版本
}
pub struct PrivateSource {               // 执行私有 registration 的冻结输入（§7.1）
    pub root: PathBuf, pub cwd: WorkspaceCwd,     // 消费者给出，与 Legacy 同一选择；cwd 相对 root
    pub registration: ObjectRegistrationId, pub nonce: TransferNonce,
    pub directory: ExternalDirectoryId, pub incarnation: ExternalDirectoryIncarnationId,
    pub authority: ExecutionAuthority,   // 以上均每执行新生成；authority 是不透明值，C1 结论在路由
}
pub enum Desired { Run, Stop, Retire }   // 单调：Run < Stop < Retire
#[must_use] pub struct DriveTicket(ExecutionId);
impl DriveTicket { pub fn committed(id: ExecutionId) -> Self; } // tx-only 提交结果的接缝

#[async_trait]
pub trait ExecutionStore: Send + Sync {  // insert/request 借用调用者事务
    type Tx<'t>: Send;                   // server：sqlx::Transaction<'t, Sqlite>
    async fn insert(&self, tx: &mut Self::Tx<'_>, intent: &ExecutionIntent) -> Result<(), StoreError>;
    async fn request(&self, tx: &mut Self::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError>; // intent+desired+settled
    async fn settle(&self, id: &ExecutionId) -> Result<(), StoreError>; // 写一次，清除 secret
    async fn unsettled(&self) -> Result<Vec<ExecutionId>, StoreError>;
}
#[async_trait]
pub trait Executor: Send + Sync {
    async fn prepare(&self, intent: &ExecutionIntent) -> Result<Fact, ExecError>; // 适配器打开 root
    async fn launch(&self, c: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn revoke(&self, c: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn generation(&self) -> Result<Generation, ExecError>; // 等到 Watching 后返回快照
    async fn control(&self, c: ExecutionControl) -> Result<ExecutionReply, ExecError>;
    async fn retire(&self, id: &ExecutionId, lifecycle: &CapabilityToken) -> Result<Fact, ExecError>;
}
pub enum Fact {                          // main 的 ExecutionState 原样投影
    Absent,
    Journal { state: ExecutionState, receipt: Option<ExecutionReceipt>, output: OutputEnd },
    Retired(Tombstone),                  // { execution_id, ticket_id, slot: Option<(alias, version)>, outcome: Revoked|Retired }
}
pub enum OutputEnd { Open, Complete { final_cursor: u64 }, Incomplete(IncompleteReason) }
pub enum ExecError { Busy(BusyReason), Refused(String), Transport(TransportLost) }
pub enum BusyReason { AliasOccupied { owner: ExecutionId }, OutputOpen, ObservationInFlight }
impl<S: ExecutionStore, X: Executor> Engine<S, X> {
    pub async fn admit(&self, tx: &mut S::Tx<'_>, intent: ExecutionIntent) -> Result<DriveTicket, AdmitError>;
    pub async fn request(&self, tx: &mut S::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<DriveTicket, StoreError>;
    pub fn drive(&self, ticket: DriveTicket);           // 调用者提交后调用
    pub async fn recover(&self) -> RecoverReport;      // 启动时对 unsettled() 逐个 drive
    pub async fn wait_settled(&self, id: &ExecutionId, deadline: Duration) -> Result<(), DriveError>;
    // interact(id, WriteStdin|Resize|Signal) 与 output(id, from) 包装 Control，capability 由 inspect 的 receipt 组装
}
```

`admit`/`request` 借用调用者的 SQLx 事务写入，不触发外部副作用，与领域写入一起回滚；调用者提交后把 `DriveTicket` 交给
`drive`。tx-only 的 terminal launch 在 `prepare_tx` 内 admit，把 ExecutionId 放进 `TxOutput`，`commit_keyed` 返回
`KeyedCommit{result, ..}`（`tx_only.rs:150-203`）后以 `DriveTicket::committed(id)` 驱动。`#[must_use]` 只是 lint 辅助：
提交后经 `?` 提前返回仍可丢弃 ticket，此时行保持 unsettled、占用谓词失败关闭，由启动时的 `recover()` 或删除路径
`request(Retire)` 自带的新 ticket（§7.2）补上。

**Executor port 与 main 协议一一对应（E1a 改动）：**

| port | wire（`ExecutionRequest`） | 相对 main |
|---|---|---|
| `prepare` | `Prepare(PrepareExecution)`，`source: PreparedSource::Private(AttachRegistration)`，root FD 经 SCM_RIGHTS 随同一请求 | main 的 Prepare 只引用已 Attach 的 receipt（保留为 `PreparedSource::Registered`）；新增 `Private`：executor 在该执行的资源 gate 下先跑 main 的 `attach_objects`，再跑 preparation。`handoff: Option<AliasHandoff>` 改为 `replaces: Option<ExecutionId>` |
| `launch` / `revoke` | `Launch` / `Revoke(ConsumeExecution{execution_id, ticket_id, ticket})` | 三个请求都先取 ExecutionId gate，记录已知时再取 main 的资源 gate（revoke 仍与 launch 共用资源 gate）；未知 id 的 Revoke 在 id gate 下写 Revoked tombstone，之后同 id 的 Prepare 被拒 |
| `inspect` | **新** `Inspect(ExecutionId)` → 记录、tombstone 或 Absent | 新增（无 capability 的只读查询，经 client secret 认证） |
| `generation` | **新** `Watch`：每个 executor 一个流，先回 `Watching{boot}`，之后每个持久化的生命周期转移推送 `Changed` | 新增；无 id、无 seq（§6.3） |
| `control` | `Control(ExecutionControl)` | 删除 `Cleanup`；**新** `ProcessAction::Stop`（lifecycle）；`Observe` 读 `OutputLog` |
| `retire` | **新** `Retire{execution_id, lifecycle}` | 取代 `Cleanup`；同时退役该执行的私有 registration |
| （不变） | `Attach`、`Export/ExportAck`、`ObserveDirectories`、`ObserveTransferredDirectories`、`RetireRegistration` | S2a 无调用者；去留属 resource cutover |

回复新增 typed `ExecutionReply::Busy(BusyReason)`；其余拒绝仍是 main 的 `Refused{reason}`，engine 不解析字符串。
`EXECUTION_ENVELOPE_VERSION` 提升到 4（daemon 可跨 kernel 升级存活）。

**细则。** prepare/launch/revoke/retire 按 ExecutionId 幂等。prepare 重放由 executor 判等：同一 nonce 的 attach 必须得到
相同的不可变 transfer（`attachment.rs:64-73`），且保留首次收到的 open description（`:96-106`）；root 在两次尝试之间被替
换则 `Refused` → revoke。`WriteStdin` 保持 main 的 `StdinProgress{accepted_bytes}` 语义，回复丢失为 `Transport`，不自动
重发。三种 `Busy` 都由后续 `Changed` 结束；fence、boot、摘要或 ticket 不符是 `Refused`；只有 `Transport` 让 engine 重新
`inspect`。`Retire` 与未知 id 的 `Revoke` 写同一种不可变 tombstone（不含 token），保留期无界，回收需 kernel 无引用证明
（另开 issue）；executor `open` 加载全部 tombstone，启动成本随历史终端数线性增长，opt-in 期间可接受。

## 5. 唯一入口

Intent 只经 `Engine::admit`，在接受请求的 operation 事务内写一次（`intent_json`）。Desired 只经存储的唯一 request 语句
（`Engine::request` 与 `terminal_delete_tx`，§7.3），单调。prepare/launch/revoke/stop/retire 只由 `drive` 发出；Drained
只由 executor monitor 写。私有 registration 随 Prepare 建立、随 Retire 退役。输出字节与终止 cursor 只在 executor 的
`OutputLog`（按 ExecutionId，独立于 `Process`；reader 追加，`retire` 删除）；屏幕在 server 持有的 calm-session
renderer，订阅 `Engine::output`，缺 geometry 历史时 invalidate。`settled_at_ms` 只由 `drive` 在观察到 tombstone 后写。
kernel 不保存 Prepared/Running/Drained 副本，需要时 `inspect`。tx-only launch 的 keyed 重放返回 operation 行冻结的**准入
结果**（`tx_only.rs:189-203`）。journal 目录必须持久（不得是 tmpfs），由 executor 的 typed 配置/CLI 给出（main
`main.rs:49`）。

## 6. 一个状态机，两个谓词

### 6.1 谓词与动作表

(1) executor 的 settled 谓词（main `durably_settled`/`permits_handoff`：Revoked、Drained、NoChild 且已持久化）裁决物理释
放：alias 交接与私有 registration 退役由 executor 自己检查；`Retire` 另要求 `OutputEnd ≠ Open`。(2)
`next_step(desired, fact) -> Step` 只选动作：

| desired \ fact | Absent | Prepared | Forking/Spawned/Exited | Revoked/NoChild/Drained | Unknown | Retired |
|---|---|---|---|---|---|---|
| Run | prepare | launch | Wait | Park | Wait | Settle |
| Stop | revoke | revoke | stop→Wait | Park | Wait | Settle |
| Retire | revoke | revoke | stop→Wait | retire | Wait | Settle |

`Wait` = 等变更代次或 drive 通知；`Park` = 无事可做，任务按 §6.3 握手退出；`Settle` = 若 `settled_at_ms` 为空则写入后退
出。**结算与 Desired 无关**（S4）：tombstone 是终态事实，任何 Desired 下都结算；Absent→Stop→未知 id revoke→tombstone 的
执行因此不会卡住。之后的 `request(Retire)` 只是单调写入，行已结算。`Busy(_)` → `Wait`；prepare/launch 的 `Refused` →
revoke；其他动作的 `Refused` 是缺陷，任务记录 `DriveError` 后 Park。

### 6.2 currentness：执行私有 fence

main 的 fence 已在资源 gate 下于 Prepare、Launch、Interactive 检查 receipt 相等、registration 版本、`admission_open`、
incarnation、executor boot 与 receipt 中的 authority（`mod.rs:320-368` 的 `resolve`，`validation.rs:254-317`）。私有
registration 的资源键是本执行独有的 `ExternalDirectoryId`，因此这个 fence 只约束本执行，不跨执行、不跨资源。S2a 没有
`UpdateRegistration`：本执行的准入只经 stop/revoke/retire 结束。授权撤销（grant/会话，C1）在撤销事务内写
`Desired=Stop`；任务在每个 prepare/launch 前重新 `load`，其后提交的撤销至多放过一次 fork，随即被 stop。

### 6.3 驱动与等待（S3）

每个 ExecutionId 至多一个驱动任务（任务表 `Mutex<HashMap<id, Slot{dirty, notify}>>`）：`drive` 在锁内对已有任务置
`dirty` 并 `notify_one`（Notify 保存许可），否则插入并 spawn。适配器维护**一个 executor 级变更代次**
`tokio::sync::watch<u64>`：每收到一个 `Changed`（任何执行、tombstone、OutputEnd 或借用者释放）、每次重连后的
`Watching`、每次 boot 改变或断流，都加一；`generation()` 只在流已回 `Watching` 后返回当前值。任务每轮：锁内清 `dirty` →
取代次快照 `g` → `load` → `inspect` → `next_step` → 执行一步（Busy 的重试也在快照之后发出）；`Wait` 时 `select`“代次 >
g” 与 `notify`；退出前在同一把锁内复查 `dirty`，已置则继续，否则移除自身。

正确性只依赖一个顺序：executor 先持久化转移再发 `Changed`，适配器收到即加一。快照之后的转移必使代次越过 `g`，之前的转移
已被随后的 `inspect` 看到；等待 alias owner 或观察者因此不需要按主体路由，断流丢失的事件由重连加一覆盖。删除按 id 扇出与
seq。代价是等待中的任务在每个生命周期转移（不含输出字节）上各重读一次。`recover()` 只是对 `unsettled()` 调 `drive`，与正
常驱动共用任务表；server 中没有定时器或轮询。

### 6.4 executor 侧规则

**stop 升级。** `ProcessAction::Stop`：向 scope 发 SIGTERM，经常量 `STOP_GRACE`（2s）仍未排空则令 helper 对全部子孙
SIGKILL（复用 `scope_helper.rs:156-165` 的 SIGUSR1→SIGKILL）；宽限由合同测试固定，`Signal(Term)` 仍只发信号。

**Drained 的顺序。** helper 报告 scope Drained → 在资源 gate 下立即持久化 `Drained{status, proof, output: Open}`（同
main `mod.rs:601-621`）→ reap helper（它持有 slave/pipe，`scope_helper.rs:113-120`）→ reader 读到 EOF，或 PTY master 在
全部 slave 关闭后的 `EIO`（Linux 行为，由 E1a 测试固定）→ `OutputLog` fsync → 持久化 `output: Complete{final_cursor}`。
等待 reader 时不持 gate；其他读错误记 `Incomplete(reason)`，executor 重启时 `Open` 记 `Incomplete(executor_restarted)`。
slave fd 经 SCM_RIGHTS 逃出 scope 时 scope 仍 settled（交接可进行），但 `Retire` 回复 `Busy(OutputOpen)`，不按超时伪造
EOF。

**boot 规则。** journal 持久化宿主 `boot_id`：宿主重启 → 非终态记为 `NoChild{host_rebooted}`；同宿主 executor 重启沿用
main `open`（Prepared→Revoked、未 fork→NoChild、其余→Unknown）。终态事实与 tombstone 的重放与 executor boot 无关；launch
与 interact 检查 boot（旧 boot 的私有 registration 不在 `live_objects`，`resolve` 拒绝）。

**同宿主残留 Unknown。** helper 没有 PDEATHSIG，`stop_known` 只发 TERM（`shutdown.rs:48`），helper 消失也不证明子树排
空，因此没有人工 NoChild 出口。用户可见：executor 重启后残留 Unknown 的终端不能 retire，其重启得到
`Busy(AliasOccupied)`，删除路径按 §7.2 有界等待后失败关闭，直到宿主重启；正向同宿主排空是后续 issue。

## 7. 私有 registration、资源交互与删除

### 7.1 Terminal 用哪种 executor scope（S1 决定）

main 上每个执行都引用一个 Attach 过的 registration（`ExecutionIdentity.source_registration`）。两种形状：

- **Worktree**：当前 fence 每 `(workspace_id, binding_id)` 一个（`validation.rs:192`；`resolve` 在 `mod.rs:349` 查该键的
  当前 receipt），同资源新 Attach 受 `source_successor`/`older_runtime`/incarnation 约束（`attachment.rs:28-58`），需要
  跨执行一致的 generation、install_epoch、`SourceIncarnationId` 与 Git 布局 FD，只有资源生命周期 owner 能提供。
- **ExternalDir**：只需 root FD（`HeldSourceObjects::open(root, false)`，`objects.rs:151-188`），资源键是
  `directory_id`，cwd 在持有的 root 下按 FD 解析（`directory.rs:57-66`：“ExternalDir has no canonical workspace/source
  proof. Keep its explicit FD contract”）。main 的合同测试正是每次 Attach 用新的 `ExternalDirectoryId`、版本 1、新
  incarnation（`launch.rs:177-205`、`tests/execution_support/mod.rs:140-160`）。

**选择：执行私有的 ExternalDir registration，随 Prepare 在同一请求中建立，随 Retire 退役。** 其 id、nonce、directory、
incarnation 在 admit 时新生成并冻结进 intent，`registration_version = 1`、`admission_open = true`；executor 拒绝
`directory_id` 已知的 Private attach，所以它不与共享资源或其他执行相交，main 的 fence 语义（§6.2）原样保留、作用域是一个
执行。必须与 Prepare 同一请求：分两次时 kernel 崩溃于其间会留下无执行引用的活 registration，要找回它就需要 registration
列表与恢复协议，也就是被删除的资源生命周期。

**这不是资源声明。** 它不产生 WorkspaceRef、LaunchHandle、typed Ready、PreparedLaunch 或 CanonicalWorkspaceId，只持有
Legacy 终端会作为原始 `EnsureProc.cwd`（`terminal_renderer/mod.rs:725-737`）使用的同一路径在 Prepare 时打开的对象；不作
身份证明，不授权任何破坏性 effect。authority 只在私有 registration 与 identity 之间判等。**旧 boot：** executor 重启后旧
进程连同其 FD 已不存在（journal 独占 flock，`journal.rs:118-137`；描述符以 CLOEXEC 接收，calm-session
`rights.rs:160`），`Retire` 对旧 boot 的私有 registration 只记账。这不是 source 销毁许可：S2a 从不驱动 source effect。

### 7.2 资源交互：与 Legacy 终端对等（S2）

S2a 不给资源新增保证，只让 typed 执行在**既有**路径中得到 Legacy 终端的待遇。唯一占用谓词是 calm-truth
`workspace_execution.rs` 的 `unsettled_executions(scope)`（`settled_at_ms IS NULL`；scope 为 Track、Area 经
`tracks.area_id`、或 alias，Terminal/Card scope 由 Terminal 消费者映射为 `term:<id>`）。读者：

1. `terminal_disposal::unresolved_tx`（`terminal_disposal.rs:28`）把它与未决 Legacy launch 并列，所以 `require_safe_tx`
   （提交前复查；repoint 也用它）遇 typed 占用即拒绝。
2. `terminal_disposal::require_safe`（`:118`）对未决 Legacy launch 请求 stop 后报错；对 typed 执行则在自己的事务里
   `request(Retire)`、提交后 `drive`、`wait_settled` 至既有 quiesce 宽限（2s，同 `terminal_sweeper.rs:501`），超时返回既
   有 Conflict 错误，行与 workspace 保留，调用者重试。main 上 `require_safe` 的调用者全是删除路径（卡片、Track、Area 删
   除，sweeper、renderer disposal）；repoint 只用 `require_safe_tx`，故 repoint 只拒绝、不停止。
3. 定时清理 `worktree_clean.rs` 在谓词非空时跳过该 Track，不请求 Stop/Retire：**Track close 不是 stop**。已停止但未删除
   的 typed 终端（Drained、未结算）同样推迟清理，这是比 Legacy 保守的差异，不丢数据。
4. typed 终端的 `quiesce_terminal_artifacts_for_deletion` 退订 renderer 并等待上一步请求的结算。

破坏性 effect（recycle、teardown、sweep）仍由既有路径在既有时点执行。Legacy 已有的竞态（quiesce 之后、recycle 之前的新
launch，删除中仍在 worktree 内运行的 Planner/worker 等）对 typed 终端同样存在，各自另开 issue，不在此修复。

### 7.3 删除、回滚与 alias 交接

1. 领域事务：`terminal_delete_tx`（calm-truth `card.rs:565`）在同一事务内执行存储的 request 语句（按当前 alias 写
   `Retire`），返回 `#[must_use] TerminalDeleted{retire: Option<ExecutionId>}`，调用者提交后转成 `DriveTicket`；不做外部
   副作用，回滚时外部状态、OutputLog 与 renderer 订阅不变。仅测试使用的 `RepoOutOfDomain::terminal_delete`
   （`out_of_domain.rs:358`）改为同一语句，保持单点。
2. 提交后：`drive` → stop → Wait → retire（释放 native 残余、删除 OutputLog、退役私有 registration、写 tombstone
   `{alias, slot_version, outcome}`、移出热记录；每步可重放）→ `settle`；renderer 在 Retired 后退订；崩溃后由 `recover`
   继续。

**终端重启。** 同一事务先 `request(Retire)` 旧执行，再 admit `replaces = Some(旧 id)` 的新执行；部分唯一索引保证每 alias
一个当前 intent。executor 在 launch 时裁决：slot 空闲 → 版本 1；属于本执行 → 重放；属于另一执行 → 要求
`replaces.is_some()` 且 owner 已 settled 或已有 tombstone，则版本 +1，否则 `Busy(AliasOccupied{owner})`，按 §6.3 等待。
executor 不比较 `replaces` 与 owner，不做 slot CAS。tombstone 保留 slot，`open` 从热记录与 tombstone 取每 alias 的最高持
久化版本重建 alias 表。连续两次重启（A 运行，B 未 fork 即被 C 替换）：B 被 revoke，C 等 A settled 后取版本 2。

## 8. 按失败说明去除的缺陷类

| #2770 失败 | 机械原因 | 结构决定与改写后的验收 |
|---|---|---|
| `terminal_delete_retains_rows_until_actual_registration_borrower_releases` | 无 Retire 发送者；测试的借用者是 Export（`cleanup.rs:380-430`） | 真实借用者是引用私有 registration 的未结算执行与进行中的观察。验收：DELETE 返回 204，执行行保留到 retire 完成；观察进行中 `Retire` 得到 `Busy(ObservationInFlight)`，观察结束后完成 |
| `terminal_product_deletion_releases_operation_storage_and_workspace_anchor` | 无 Cleanup/Retire 发送者；anchor 行无删除者 | 终端删除释放执行存储与私有 registration；anchor 行不存在；workspace 由既有路径处理 |
| `terminal_delete_rollback_retains_screen_and_retries_release_after_restart` | 提交前删除 renderer 条目 | 事务内只写 `Desired`；副作用都在提交后；重启由 `recover` 重驱 |
| `terminal_exact_retired_result_replays_after_executor_restart` | `retirement.rs:8-11` 先比较 boot 再查 `.retired` | 改用执行 tombstone：重放与 executor boot 无关；旧 boot 私有 registration 只记账（§7.1）。`RetireRegistration` 不变，S2a 不调用 |
| `terminal_real_output_eof_precedes_durable_drain_and_final_screen` | Drained 仅凭 helper 报告；server 把 Drained 当退出 | §6.4 Drained 顺序：`Complete{final_cursor}` 之前的字节完整；renderer 按序喂入，缺 geometry 历史时 invalidate（main `terminal_renderer/mod.rs:832`），不承诺冷恢复精确屏幕 |
| `recovery_and_ordinary_drive_share_the_effect_serialization_fence` | `apply_recovery_item` 绕过 `drive_mutex` | 每 id 一个驱动任务；terminal launch operation 改为 tx-only，只有一个恢复 owner。通用 operation 缺陷另开 issue |

## 9. 不复用的部分与被取代的条款

不复用 #2770 的 migration 0169（编号也冲突）、server 侧执行输入/保留/轮询、renderer/sweeper 的 registered 分支、
`RegisteredV3QueryV1`、`RegistrationUnwitnessed` 与测试 flag；保留 main executor 的 helper、pidfd、fence、journal、alias
与 ticket 机制，以及 #2770 的 token 映射、`FreshInitialGeometry` 区分与回归场景。

本文只取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的以下条款，且只对 S2a 执行消费者生效：

- 「Real PreparedLaunch and frozen executor seam」中“do not wrap a raw-cwd launch with fabricated IDs or empty
  authority”与 Register → intent → Prepare 顺序：S2a Terminal 改为 intent → 带私有 attach 的 Prepare（§7.1）。它明确不是
  PreparedLaunch。该节其余内容（含必要改动 1–4）留给 resource cutover。
- 「ExternalDir, legacy executors and providers」中 ExternalDir 是“distinct authorized current directory registration”的
  表述：执行私有 registration 只借用 ExternalDir 的 wire 形状，不建立 ExternalDir 资源。
- 「Smallest implementation slices」第 1 片的“one real terminal consumer”：S2a 的 Terminal 消费者不是它，不签发 typed
  Ready。该切片表继续作为 resource cutover 的计划。

仍然有效、且属于 resource cutover：kernel 持 FD 与反向导出、current-object proof、崩溃/退役表、RuntimeSafety closure（首
个 typed Ready 之前）、Required kernel states、owned materialization/borrowed adoption、release/cancellation、One
task-admission cutover。授权（C1）与身份表不变。

## 10. 最小 migration

取合入时 main 的下一个空闲编号（当前最大 0168）；不修改已发布 migration 或既有表；不加外键（领域删除不级联，0093 允许的
无键 operation 回收不受阻）。

```sql
CREATE TABLE workspace_executions (
    execution_id  TEXT PRIMARY KEY,
    track_id      TEXT NOT NULL,     -- 读者：unsettled_executions 的 Track/Area scope
    alias         TEXT NOT NULL,     -- 读者：当前 alias 索引、Terminal/Card scope
    intent_json   TEXT NOT NULL CHECK (json_valid(intent_json)),   -- 不含 secret
    secrets_json  TEXT CHECK (secrets_json IS NULL OR json_valid(secrets_json)), -- ticket secret 与 tokens
    desired       TEXT NOT NULL CHECK (desired IN ('run', 'stop', 'retire')),
    settled_at_ms INTEGER,
    CHECK ((settled_at_ms IS NULL) = (secrets_json IS NOT NULL))
);
CREATE UNIQUE INDEX workspace_executions_current_alias
    ON workspace_executions(alias) WHERE desired <> 'retire';
CREATE INDEX workspace_executions_unsettled
    ON workspace_executions(track_id) WHERE settled_at_ms IS NULL;
-- 触发器：execution_id/track_id/alias/intent_json 不可改；desired 只能不降（允许同值重写与
-- run→retire）；settled_at_ms 写一次；secrets_json 只在结算时置空。
```

结算与 `desired` 无关（§6.1），故 CHECK 不要求 `desired = 'retire'`；交接在同一事务先 Retire 旧行再插入新行。secret 只经
store port 读取，结算时清除，不进入事件、调试产物或 API。SQL 位于 calm-truth `db/sqlite/workspace_execution.rs`，
calm-server 的 `SqliteExecutionStore` 包装它，`terminal_delete_tx` 与占用谓词复用同一模块。

## 11. 有序切片

每片独立可合入，按 [track-workspace.md](track-workspace.md) §6 的公共 gate G 验证，等级 L2；测试名是实现时注册的新过滤
器。

| # | 结果 | 验收与聚焦测试 |
|---|---|---|
| E1a | supervisor 协议，带真实合同：§4 映射表全部改动（`PreparedSource::Private`、`Inspect`、`Watch`/`Changed`、`Retire` 与 tombstone 及私有 registration 退役、`ProcessAction::Stop` 升级、`replaces` 交接、id gate → 资源 gate、未知 id revoke、typed `Busy`、借用者释放事件）；宿主 boot_id；`OutputLog` 与 `Process` 分离及 Drained 顺序；删除 Cleanup；envelope 4。main 上 server 无 registered executor 生产调用者 | `-p calm-proc-supervisor`：`private_attach_and_prepare_are_atomic`、`private_attach_refuses_known_directory`、`prepare_replay_after_root_replaced_refused`、`drained_persisted_before_output_complete`、`output_complete_after_reader_eof_or_pty_eio`、`retire_waits_output_open`、`retire_busy_observation_then_changed`、`retire_retires_private_registration`、`handoff_after_retired_previous`、`handoff_after_retire_and_executor_restart`、`double_restart_before_fork`、`revoke_unknown_id_blocks_late_prepare`、`stop_escalates_after_grace`、`host_reboot_is_no_child`、`changed_after_every_persisted_transition`；保留 `execution_launch_revoke_share_gate_and_ticket_is_single_use`。对 EOF 前置、交接谓词与 Private 原子性做变异验证。可选再拆：输出存储/Drained 顺序/stop 升级/boot_id 一片，其余协议一片，各带真实测试 |
| E1b | `workspace-runtime` crate 的 `execution` 模块：类型、`next_step`、`Engine`、两个 port、最小 fake（脚本化 Fact 序列与调用日志，不复制 journal）、`session_executor`；合同套件在 fake 与 E1a 的真实进程内 executor 上各跑一次 | `-p workspace-runtime`：`next_step` 全表、`absent_stop_unknown_revoke_settles`、`drained_between_inspect_and_wait_wakes`、`request_after_park_decision_is_not_lost`、`retire_between_load_and_park_drives`、`handoff_owner_settles_between_busy_and_wait`、`observation_ends_between_busy_and_wait`、`recover_racing_drive_effect_once`、`watch_reconnect_wakes_all_waiters`；`-p calm-proc-supervisor`：`port_contract::executor`。对 Park 握手、“快照先于 inspect/Busy 重试”与 `next_step` 释放分支做变异验证，断言 fake 调用日志（否则 fake 的 settled 守卫会掩盖变异） |
| E2 | migration + `SqliteExecutionStore` + opt-in Terminal 消费者 + 占用集成同片：typed `Config`（默认关闭）；terminal launch operation tx-only admit 与 `DriveTicket::committed`；`terminal_delete_tx` 请求 Retire 并返回 `TerminalDeleted`；`unsettled_executions` 接入 `terminal_disposal`、`require_safe` 的 Retire+有界等待与 `worktree_clean` 跳过；renderer 订阅 `Engine::output`；boot 调用 `Engine::recover` | `-p calm-server`：SQLite 上 `port_contract::store`，领域写入与 admit 同事务回滚；触发器与索引拒绝改写；Stop 结算通过 CHECK；§8 六个场景（按改写后的验收）；HTTP 创建、WS 输入/resize/stop、冷启动恢复；Track 删除对 typed 终端先 Retire 后 recycle；repoint 遇 typed 占用拒绝且不停止；closed Track 清理跳过 typed 占用且终端继续运行、reopen 后可再开 typed 终端；Legacy 终端测试不改 |
| E3… | 每个后续消费者一片：直接 pipe 进程（同片定义 close-stdin/half-close）、gate、provider 启动；同宿主正向排空 | 每片只增加一个 `ExecutionIntent` 构造点或一个恢复机制及其真实入口测试 |

S2a 之外、track-workspace S2a 行的后续里程碑：**resource cutover**，即 typed 资源、workspace service 拥有的 registration
生命周期（Worktree 形状、共享 fence、关闭/恢复）、RuntimeSafety closure、Legacy 入口拒绝、typed release 与失去见证后的恢
复合同，前提是该资源再无 Legacy 消费者（Planner、worker、gate、Legacy 终端都已迁移），届时执行改为引用共享 registration
（`PreparedSource::Registered`），engine 不变；**workspace_service 迁入 `workspace-runtime`**，resolve、生命周期、占用、
物化/回收编排经 port 迁入，calm-server 只留 HTTP/WS/auth/DB 适配器，不改行为；borrowed/Legacy adoption；resource
task-admission cutover。

## 12. AGENTS.md 架构五点

- **抽象边界**：动作政策只在 `workspace_runtime::execution`；物理事实、settled 谓词、alias slot 与 fence 只在 executor；
  资源生命周期与破坏性 effect 留在既有 server 路径，S2a 只给它们一个占用谓词。
- **重复逻辑**：多处 stop/退出推导收为 `next_step` 与一个驱动任务；Cleanup 并入 `Retire`；currentness 只有 main 的
  fence；占用只有一个谓词，接入既有 disposal 路径；唤醒只有一个代次。
- **硬编码假设**：engine 不知道 Terminal；alias/OperationRef/Track 由调用者给出；`Desired`、`Fact`、`BusyReason` 是本层
  闭合集合；`term:<id>` 映射只在 Terminal 消费者。
- **外部合同事实**：进程与 fence 状态取自 executor 回复，宿主重启取自 `boot_id`，PTY EIO 由测试固定；不从 PID、路径或
  dev/inode 推导排空或身份。
- **永久成本**：一张表、两个索引、一组触发器；合同套件在 fake 与真实实现各跑一次；新增消费者、测试或文件不改名单。

## 13. 评审处置（第三轮）

| 评审项 | 处置 |
|---|---|
| codex R3-B1、claude R3-B1（Legacy choke point；与 R2-B5 同类，结构性） | 收窄：删除 typed 资源、`check_legacy_ingress` 与“拒绝全部 Legacy 入口”。Track worktree 的 Legacy 用户保持 main 语义；S2a 不签发 typed Ready，RuntimeSafety 前提留给 resource cutover，并写入 track-workspace §3.2 与 launch-admission 注记 |
| codex R3-B2（`boot_changed` 被当作销毁许可） | 删除：`Released(boot_changed)`、`RetireRegistration` 规则改动、release 意图顺序都不再存在；S2a 不驱动 source effect；旧 boot 私有 registration 只做 holder 记账（§7.1） |
| codex R3-B3、claude R3-N1（Stop + tombstone 违反 CHECK） | S4：任何 Desired 下 tombstone 都结算；CHECK 去掉 `desired='retire'`；E1b/E2 加 Absent→Stop→未知 revoke 用例 |
| claude R3-B2（单向 fence + 定时清理） | 删除：无 `UpdateRegistration`/admission 列；清理遇 typed 占用跳过、不停止；Track close 不是 stop；E2 加 reopen 用例 |
| claude R3-B3、codex R3-N1（唤醒路由；与 R2-B2 同类，结构性） | S3：executor 级变更代次，快照先于 load/inspect/Busy 重试，删除按 id 扇出与 seq；E1b 加 handoff 与观察者两种调度 |
| codex R3-N2、claude R3-N6（E1a 体量） | 删去 UpdateRegistration、ListRegistrations、RetireRegistration 规则、Attach 撤回后显著变小；可选拆分写入 E1a |
| claude R3-N2（revoke gate） | id gate → 资源 gate；revoke 仍与 launch 共用资源 gate；保留 main 测试 |
| claude R3-N3（DriveTicket 接缝） | `DriveTicket::committed` 取自 `KeyedCommit.result`；删除“丢弃即编译失败”，写明 `recover` 与失败关闭的兜底 |
| claude R3-N4、N5、N7(a)(b) | 已失效：没有可关闭的共享 fence、Attach 撤回、UpdateRegistration 或 RetireRegistration 规则；私有 attach 只随 Prepare，按 nonce 重放，已知 `directory_id` 被拒 |
| claude R3-N7(c)(d)、N8、N9 | §5 journal 目录持久；§6.4 用户可见结果；launch-admission 注记改正（task-admission 未被取代，RuntimeSafety 等仍有效）；`out_of_domain.rs:358` 并入单点（§7.3）；§4 tombstone 启动成本；删除 `intent_digest`、`operation_id`、`workspace_id` 列 |

**被收窄作废的早期处置。** 只服务于资源闭包的第一、二轮处置随本版作废，机制不再存在：第一轮 codex B2/claude B1
（registration 归 workspace service、`ListRegistrations`）；第二轮 claude B1（`UpdateRegistration`、`closing`）、claude
B6（`workspace_registrations`）、codex R2-B3/claude B4（`boot_changed` 与 `Busy(UnsettledExecutions)`）、codex R2-B5
（Legacy choke point）、claude N10（ExternalDir 现作私有 registration 形状）。其余早期处置仍有效（async port 与借用事
务、一条输出路径、删除 `EffectAuthority`/ `AttestNoChild`、tombstone + `replaces`、Park 握手、typed `Busy`、Drained 顺
序、E1a/E1b 拆分等）。
