# Workspace execution（S2a 实施合同）

状态：S2a「registered workspace execution」的重设计，替代 PR #2770（`codex/2666-terminal-vertical`，用户已否决）。第七版
在第四版的**范围收窄**（S2a 只拥有**执行**，不拥有资源；Planner 决定 S1–S5）与第五版的 F1–F4 之上再做一次纯收窄：按
Legacy 对等删除 typed 终端重启（第六轮评审，处置见 §13），不重开范围。只是设计，不声称已实现；等级 L2。父合同为
[track-workspace.md](track-workspace.md)；本文只取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的少量
条款（§9）。英文类型是合同草案。main 的 executor 协议（#2716）仍是执行物理事实与 currentness 的 owner，`Executor` port 是它
的薄投影（§4）。S2a 不建 typed 资源、不改变 Track worktree 的任何 Legacy 用户、不驱动任何破坏性 source effect。

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

`Export/ExportAck`、`Cleanup`、`RetireRegistration`、`durably_settled`、`permits_handoff` 与 `apply_event` 是 **#2716
已合入 main 的 executor 代码**，E1a 修改它们及其测试。#2770 没有执行动作的 owner；main 的 `Process` 把 child、PTY、
pidfd 与输出 ring 绑在一起（`process.rs:20-27`）；`apply_recovery_item` 不持 `drive_mutex`（定义 `driver.rs:1184`、调用
`:442`，另开 issue）；`apply_event` 仅凭 helper 报告写 Drained（`mod.rs:601-621`）。main 上每个 Track worktree 都是 Legacy selection（`workspace_resource.rs:217-256`），Planner、
worker、gate 经 server 直接 spawn 在其中运行；给这个资源加 fence、Legacy 拒绝与 typed release 每轮都重新开洞，故收窄。

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

依赖只有 `workspace`、`calm-session`、`async-trait` 与 async 原语，无环；`workspace` 若含执行政策会与 calm-session 成环，
executor 也不能链接 kernel 政策，故不合成一个 crate。

| 层 | 拥有 | 不拥有 |
|---|---|---|
| `workspace_runtime::execution` | `ExecutionIntent`、`Desired`、`next_step`、`Engine`、每 ExecutionId 一个驱动任务、两个 port、`session_executor` 适配器（组装 wire、维护变更代次） | 授权、路径选择、资源生命周期、SQL、HTTP、渲染 |
| calm-proc-supervisor | journal、settled 谓词、alias slot 与 tombstone、执行私有 registration 及其唯一释放例程、native child/PTY/pidfd、`OutputLog`、stop 升级（含优雅关闭）、变更流 | 领域删除、授权结论、资源 |
| calm-server | 路由/WS、auth/C1 结论、`SqliteExecutionStore` 与消费者投影、Terminal 消费者（cwd 选择同 Legacy）、既有 disposal/clean 路径、renderer 订阅输出 | 执行状态推导 |

## 4. 公共 API 草案

所有 port 用 `#[async_trait]`（同 `operation/tx_only.rs:30`），trait 带 `Send + Sync`，future 为 `Send`。wire 类型与时限常
量直接复用 calm-session `control/execution.rs`。

```rust
pub struct ExecutionIntent {             // admission/prepare 的完整输入
    pub metadata: ExecutionMetadata,
    pub secrets: ExecutionSecrets,
}
pub struct ExecutionSecrets {
    pub ticket: CapabilityToken,
    pub tokens: ExecutionControlTokens,  // observe/interactive/lifecycle
}
pub struct ExecutionMetadata {           // 不可变；不含 ticket secret 或 control tokens
    pub id: ExecutionId, pub alias: ProcessAlias, // alias 如 "term:<id>"
    pub owner: OperationRef, pub track: TrackId,  // track 是占用谓词的键（§7.2）
    pub source: PrivateSource,
    pub program: ExecutionSpecification, // main 类型：argv/显式 env/io_mode/replay_bytes
    pub specification: SpecificationId,
    pub ticket_id: LaunchTicketId,
}
pub struct PrivateSource {               // 执行私有 registration 的冻结输入（§7.1）
    pub root: PathBuf, pub cwd: WorkspaceCwd,     // 消费者给出，与 Legacy 同一选择；cwd 相对 root
    pub registration: ObjectRegistrationId, pub nonce: TransferNonce,
    pub directory: ExternalDirectoryId, pub incarnation: ExternalDirectoryIncarnationId,
    pub authority: ExecutionAuthority,   // 以上均每执行新生成；authority 是不透明值，C1 结论在路由
}
pub enum Desired { Run, Stop, Retire }   // 单调：Run < Stop < Retire
pub const STOP_GRACE: Duration = Duration::from_millis(200);   // = Legacy TERM_TO_KILL_GRACE
pub const DRAIN_BUDGET: Duration = Duration::from_millis(1000); // = Legacy EXIT_PERSIST_GRACE
pub const QUIESCE_DEADLINE: Duration = STOP_GRACE + DRAIN_BUDGET; // 每个结算等待的唯一时限
#[must_use] pub struct DriveTicket(ExecutionId);
impl DriveTicket { pub fn committed(id: ExecutionId) -> Self; } // tx-only 提交结果的接缝

pub enum Stored {
    Active {
        intent: ExecutionMetadata, secrets: ExecutionSecrets,
        desired: Desired, settlement: Option<Settlement>,
    },
    Finished { intent: ExecutionMetadata, settlement: Settlement },
}
// Settlement 是直接来自 Inspect 的无 secret 证据；Finished 必须已结算且无能力字段。
#[async_trait]
pub trait Store: Send + Sync {  // insert/request 借用调用者事务
    type Tx<'t>: Send;                   // server：sqlx::Transaction<'t, Sqlite>
    async fn insert(&self, tx: &mut Self::Tx<'_>, intent: &ExecutionIntent) -> Result<(), StoreError>;
    async fn request(&self, tx: &mut Self::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError>; // Active 或 Finished；None 只表示无此行
    async fn spawned(&self, id: &ExecutionId) -> Result<(), StoreError>; // 幂等；只驱动消费者投影
    async fn settle(&self, id: &ExecutionId, s: &Settlement) -> Result<(), StoreError>; // 写一次 + 投影
    async fn finish(&self, id: &ExecutionId) -> Result<(), StoreError>; // 观察到 tombstone：清除 secrets，保留 Finished
    async fn pending(&self) -> Result<Vec<ExecutionId>, StoreError>;
}
#[async_trait]
pub trait Executor: Send + Sync {
    async fn prepare(&self, intent: &ExecutionIntent) -> Result<Fact, ExecError>; // 适配器打开 root
    async fn launch(&self, c: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn revoke(&self, c: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn generation(&self) -> Result<Generation, ExecError>; // 等到 Watching 后返回快照
    fn available(&self) -> bool;         // 不等待：流此刻处于 Watching 且 envelope 为 4
    async fn control(&self, c: ExecutionControl) -> Result<ExecutionReply, ExecError>;
    async fn retire(&self, id: &ExecutionId, lifecycle: &CapabilityToken) -> Result<Fact, ExecError>;
}
pub enum Fact {                          // main 的 ExecutionState 原样投影
    Absent,
    Journal { state: ExecutionState, receipt: Option<ExecutionReceipt>, output: OutputEnd },
    Retired(Tombstone),                  // { execution_id, ticket_id, slot: Option<(alias, version)>, registration, directory: Option<id>, outcome: Revoked|Retired }
}
pub enum OutputEnd { Open, Complete { final_cursor: u64 }, Incomplete(IncompleteReason) }
pub enum ExecError { Busy(BusyReason), Refused(String), Transport(TransportLost) }
pub enum BusyReason { OutputOpen }
impl<S: Store, X: Executor> Engine<S, X> {
    pub async fn admit(&self, tx: &mut S::Tx<'_>, intent: ExecutionIntent) -> Result<DriveTicket, AdmitError>;
    pub async fn request(&self, tx: &mut S::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<DriveTicket, StoreError>;
    pub fn drive(&self, ticket: DriveTicket);           // 调用者提交后调用
    pub async fn recover(&self) -> RecoverReport;      // 启动时对 pending() 逐个 drive
    pub async fn wait_settled(&self, id: &ExecutionId) -> Result<(), DriveError>; // 时限 QUIESCE_DEADLINE
    pub fn available(&self) -> bool;                    // 转发 Executor::available
    // interact(id, WriteStdin|Resize|Signal) 与 output(id, from) 包装 Control，capability 由 inspect 的 receipt 组装
}
```

`admit`/`request` 借用调用者的 SQLx 事务写入，不触发外部副作用，与领域写入一起回滚；调用者提交后把 `DriveTicket` 交给
`drive`。typed terminal 创建是**新的 operation kind** `terminal-execution-create`（tx-only，在 `prepare_tx` 内 admit，把
ExecutionId 放进 `TxOutput`）。`prepare_tx` 在 admit 之前检查 `Engine::available()`，为假时返回 typed
`ExecutionUnavailable`，不写卡片或执行行，不回落 Legacy；既有 `terminal-create` kind 与其 `ProviderAdapter` 不变，升级前遗留的行照旧驱动
（`tx_only.rs:1-6`）。`commit_keyed` 对新提交与 keyed 重放都返回 `OperationResult`（`driver.rs:182-203`），Terminal 调用者
从中取 ExecutionId，以 `DriveTicket::committed(id)` 立即驱动；不改 `KeyedCommit` 或通用 tx-only。`#[must_use]` 只是 lint
辅助：提交后经 `?` 提前返回仍可丢弃 ticket，此时行保持 pending、占用谓词失败关闭，由启动时的 `recover()`、删除路径的
quiesce 或 `terminal_delete_tx` 返回的新 ticket（§7.2–7.3）补上。

**Executor port 与 main 协议一一对应（E1a 改动）：**

| port | wire（`ExecutionRequest`） | 相对 main |
|---|---|---|
| `prepare` | `Prepare(PrepareExecution)`，`source: PreparedSource::Private(AttachRegistration)`，root FD 经 SCM_RIGHTS 随同一请求 | main 的 Prepare 只引用已 Attach 的 receipt（保留为 `PreparedSource::Registered`）；新增 `Private`，顺序见 §7.1：先暂存校验、再持久化执行记录、最后发布。main 的 `handoff: Option<AliasHandoff>` 与交接代码不变，S2a 总是传 `None`（§7.3） |
| `launch` / `revoke` | `Launch` / `Revoke(ConsumeExecution{execution_id, ticket_id, ticket})` | 三个请求都先取 ExecutionId gate，记录已知时再取 main 的资源 gate（revoke 仍与 launch 共用资源 gate）；未知 id 的 Revoke 在 id gate 下写 Revoked tombstone 并调用私有释放例程（§7.1），之后同 id 的 Prepare 被拒 |
| `inspect` | **新** `Inspect(ExecutionId)` → 记录、tombstone 或 Absent | 新增（无 capability 的只读查询，经 client secret 认证） |
| `generation` | **新** `Watch`：每个 executor 一个流，先回 `Watching{boot}`，之后每个持久化的生命周期转移推送 `Changed` | 新增；无 id、无 seq（§6.3） |
| `control` | `Control(ExecutionControl)` | 删除 `Cleanup`；**新** `ProcessAction::Stop`（lifecycle）；`Observe` 读 `OutputLog` |
| `retire` | **新** `Retire{execution_id, lifecycle}` | 取代 `Cleanup`；只删 `OutputLog`、写 tombstone（私有 registration 已在结算时释放） |
| （不变） | `Attach`、`Export/ExportAck`、`ObserveDirectories`、`ObserveTransferredDirectories`、`RetireRegistration` | S2a 无调用者；去留属 resource cutover |

回复新增 typed `ExecutionReply::Busy(BusyReason)`；main 的 `Refused{reason}` 与 `ObjectQualificationFailed{failure}`
（`mod.rs:671-679`）都映射为 `ExecError::Refused`，engine 不解析字符串。`EXECUTION_ENVELOPE_VERSION` 提升到 4。
握手拒绝或 executor 缺失（`lib.rs:591-620` 在 `executor: None` 时回复 Refused）由适配器映射为 `Transport`，不是动作的
`Refused`：行保持 pending、占用谓词失败关闭、重连回到 `Watching` 后重驱，从不走 Refused→revoke→Park。

**细则。** prepare/launch/revoke/retire 与 `Stop` 按 ExecutionId 幂等（Stop 见 §6.4）。Prepare 先按 ExecutionId 分派：已有记录且 transfer 完全相同
（同 nonce，`attachment.rs:64-73`）是精确重放，返回已记录的事实并保留首次持有的 open description（`:96-106`），丢弃新
FD，不重新打开或采纳替换后的 root；transfer 不同、或 `directory_id` 已绑定到另一 ExecutionId（记录的私有 transfer 或
tombstone 的 `directory`）才是 `Refused`。`WriteStdin` 保持 main 的 `StdinProgress{accepted_bytes}` 语义，回复丢失为 `Transport`，不自动重发。
`Busy(OutputOpen)` 由后续 `Changed` 结束；fence、boot、摘要或 ticket 不符是 `Refused`；只有 `Transport` 让 engine 重新 `inspect`。
`Retire` 与未知 id 的 `Revoke` 写同一种不可变 tombstone（不含 token；写入时删除同 id 的 `.execution` 文件，
`open` 遇两者并存时 tombstone 优先），保留期无界，回收需 kernel 无引用证明（另开
issue）；executor `open` 加载全部 tombstone，启动成本随历史终端数线性增长，opt-in 期间可接受。

## 5. 唯一入口

Intent 只经 `Engine::admit`，在接受请求的 operation 事务内写一次（`intent_json`）。Desired 只经存储的唯一 request 语句
（`Engine::request` 与 `terminal_delete_tx`；Retire 只有删除单点一个写者，§7.3），单调：低于当前值的 request 是空操作。prepare/launch/revoke/stop/retire 只由 `drive` 发出；Drained
只由 executor monitor 写。私有 registration 随 Prepare 建立、由 executor 的唯一释放例程在结算时释放（§7.1）。输出字节与
终止 cursor 只在 executor 的 `OutputLog`（按 ExecutionId，独立于 `Process`；reader 追加，`retire` 删除）；屏幕在 server
持有的 calm-session renderer，订阅 `Engine::output`，缺 geometry 历史时 invalidate。`settled_at_ms` 与 secret 清除只由
`drive` 写（§6.1）。kernel 不保存 Prepared/Running/Drained 副本，需要时 `inspect`；消费者投影（§7.2）是领域字段，不是
阶段镜像。journal 目录必须持久（不得是 tmpfs），由 executor 的 CLI 给出，生产配置见 §11 E2a。

## 6. 一个状态机，两个谓词

### 6.1 谓词与动作表

(1) executor 的 settled 谓词（main `durably_settled`/`permits_handoff`：Revoked、Drained、NoChild 且已持久化）裁决物理释
放：私有 registration 释放（及 main 不变、S2a 不用的 alias 交接）由 executor 自己检查；`Retire` 另要求 `OutputEnd ≠ Open`。(2)
`next_step(desired, fact) -> Step` 只选动作：

| desired \ fact | Absent | Prepared | Forking/Spawned/Exited | Revoked/NoChild/Drained | Unknown | Retired |
|---|---|---|---|---|---|---|
| Run | prepare | launch | Wait | Settle→Park | Wait | Finish |
| Stop | revoke | revoke | stop→Wait | Settle→Park | Wait | Finish |
| Retire | revoke | revoke | stop→Wait | Settle→retire | Wait | Finish |

`Wait` = 等变更代次或 drive 通知；`Park` = 无事可做，任务按 §6.3 握手退出；`Settle` = 若 `settled_at_ms` 为空则经
`store.settle` 写入（含消费者投影）；`Finish` = Settle 后经 `store.finish` 清除 secret 并退出。首次观察到 Spawned 时调用
幂等的 `store.spawned`。**结算与 Desired 无关**，且表示 executor 已持久化“无进程可残留”：占用（§7.2）在此结束，不等
Retire；Retire 只回收 OutputLog 与 alias slot，需要 lifecycle token，故 secret 保留到 tombstone。Absent→Stop→未知 id
revoke→tombstone 的执行直接 Finish，不会卡住。`Busy(_)` → `Wait`；prepare/launch 的 `Refused` → revoke；其他动作的
`Refused` 是缺陷，任务记录 `DriveError` 后 Park。

### 6.2 currentness：执行私有 fence

main 的 fence 已在资源 gate 下于 Prepare、Launch、Interactive 检查 receipt 相等、registration 版本、`admission_open`、
incarnation、executor boot 与 receipt 中的 authority（`mod.rs:320-368` 的 `resolve`，`validation.rs:254-317`）。私有
registration 的资源键是本执行独有的 `ExternalDirectoryId`，因此这个 fence 只约束本执行，不跨执行、不跨资源。S2a 没有
`UpdateRegistration`：本执行的准入只经 stop/revoke/retire 结束。S2a 不新增授权撤销写者；需要时由后续消费者在撤销事务内
`Engine::request(Stop)`，任务在每个 prepare/launch 前重新 `load`。

### 6.3 驱动与等待（S3）

每个 ExecutionId 至多一个驱动任务（任务表 `Mutex<HashMap<id, Slot{dirty, notify}>>`）：`drive` 在锁内对已有任务置
`dirty` 并 `notify_one`（Notify 保存许可），否则插入并 spawn。适配器维护**一个 executor 级变更代次**
`tokio::sync::watch<u64>`：每收到一个 `Changed`（任何执行、tombstone 或 OutputEnd）、每次重连后的 `Watching`、每次 boot
改变或断流，都加一；`generation()` 只在流已回 `Watching` 后返回当前值。任务每轮先锁内清 `dirty`，并预读 `load`：
不存在或 `Finished` 立即退出，不接触 executor；`Active` 则取代次快照 `g` →
`load` → `inspect` → `next_step` → 执行一步（Busy 的重试也在快照之后发出）；`Wait` 时 `select`“代次 > g” 与 `notify`；
退出前在同一把锁内复查 `dirty`，已置则继续，否则移除自身。

正确性只依赖一个顺序：executor 先持久化转移再发 `Changed`。快照之后的转移必使代次越过 `g`，之前的已被随后的 `inspect`
看到；不需要按主体路由，断流由重连加一覆盖。代价是等待中的任务在每个生命周期转移上各重读一次。`recover()` 只是对
`pending()` 调 `drive`。适配器唯一的定时器是重连退避，它只重建流，从不推导事实；server 不轮询执行。

### 6.4 executor 侧规则

**一个 stop 例程。** `ProcessAction::Stop`：向 scope 发 SIGTERM，经 `STOP_GRACE` 仍未排空则令 helper 对全部子孙 SIGKILL
（复用 `scope_helper.rs:156-165` 的 SIGUSR1→SIGKILL）；`Signal(Term)` 仍只发信号。Stop 按 ExecutionId 幂等：首个被接受的
Stop 固定 KILL 截止时刻，代次唤醒导致的重复 Stop 不重发 TERM、不延后截止。**优雅关闭**（supervisor 收到 SIGTERM，
`lib.rs:460-465`）对每个未结算执行调用同一例程，并等待各自持久化 Drained，总时限 `STOP_GRACE + DRAIN_BUDGET`；删除只发
TERM 的 `stop_known`（`shutdown.rs:5-56`，及 `execution_unsupported.rs:55` 的同名桩）。未 fork 的记录照 main 转 Revoked/NoChild。OutputEnd 可仍为 Open，下次 `open`
记为 `Incomplete(executor_restarted)`。neige-app 把自己的 `stop_grace`（默认 5000ms，`config.rs:591`）作为 typed CLI 参数
传给 supervisor；executor 拥有这些常量。参数不大于该时限时 supervisor 照常启动但不启用 executor（记错误日志），
Legacy 不受影响，typed 创建得到 typed 错误；从不因此拒绝启动。

**Drained 的顺序。** helper 报告 scope Drained → 在资源 gate 下立即持久化 `Drained{status, proof, output: Open}`（同
main `mod.rs:601-621`）并调用私有释放例程 → reap helper（它持有 slave/pipe，`scope_helper.rs:113-120`）→ reader 读到
EOF，或 PTY master 在全部 slave 关闭后的 `EIO`（Linux 行为，由 E1a 测试固定）→ `OutputLog` fsync → 持久化
`output: Complete{final_cursor}`。等待 reader 时不持 gate；其他读错误记 `Incomplete(reason)`。slave fd 经 SCM_RIGHTS 逃出
scope 时 scope 仍 settled（占用结束），但 `Retire` 回复 `Busy(OutputOpen)`，不按超时伪造 EOF。

**boot 规则。** journal 持久化宿主 `boot_id`：宿主重启 → 非终态记为 `NoChild{host_rebooted}`；同宿主 executor 重启沿用
main `open`（Prepared→Revoked、未 fork→NoChild、其余→Unknown）。终态事实与 tombstone 的重放与 executor boot 无关；launch
与 interact 检查 boot（旧 boot 的私有 registration 不在 `live_objects`，`resolve` 拒绝）。

**同宿主残留 Unknown。** Unknown 来自一切无法取得正向排空事实的情形：executor 崩溃或被 SIGKILL，以及 main 的
fork/setup 错误与 fork 成功但 receipt 持久化失败（`launch.rs:120-129`、`:143-150`）。helper 没有 PDEATHSIG，helper 消失也
不证明子树排空，因此保持失败关闭，没有人工 NoChild 出口。用户可见：残留 Unknown 的终端不能 retire，删除路径按 §7.2 有界等待后失败关闭，直到宿主重启；正向同宿主排空是后续 issue。

## 7. 私有 registration、资源交互与删除

### 7.1 Terminal 用哪种 executor scope（S1 决定）与私有 registration 的归属（F1）

main 上每个执行都引用一个 Attach 过的 registration。**Worktree** 形状的 fence 每 `(workspace_id, binding_id)` 一个
（`validation.rs:192`），需要只有资源生命周期 owner 能提供的跨执行身份与 Git 布局 FD。**ExternalDir** 只需 root FD
（`objects.rs:151-188`），资源键是 `directory_id`，cwd 在持有的 root 下按 FD 解析（`directory.rs:57-66`）；main 的合同
测试每次 Attach 用新的 `ExternalDirectoryId`、版本 1、新 incarnation（`launch.rs:177-205`）。

**选择：执行私有的 ExternalDir registration，id、nonce、directory、incarnation 在 admit 时新生成并冻结进 intent**，
`registration_version = 1`、`admission_open = true`，不与共享资源或其他执行相交，main 的 fence 语义原样保留、作用域是一个
执行。**归属在执行 journal**，Prepare(Private) 在 id gate → 该 directory 的资源 gate 下（一次取得；内部调用 attach 与
preparation 的不加锁核心，不调用各自取同一 tokio mutex 的包装 `attachment.rs:18-19`、`preparation.rs:51-52`）：

1. **暂存**：由收到的 FD 构造 `HeldSourceObjects`，完成 boot/规格/环境校验、`objects.rs:294-300` 的重开比对与 cwd 解析。
   全部在本地，失败 → `Refused`，无任何持久化或发布。
2. **持久化**：写入执行记录 `Prepared`，其中绑定完整的私有 transfer 与 receipt，标记 `PreparedSource::Private`；不写独立的
   `<id>.registration` receipt 文件。写失败 → 丢弃暂存对象，`Refused`；rename 后父目录 fsync 失败时文件可能已可见，engine
   随后的未知 id Revoke 写 tombstone 并删除它（§4 细则），迟到的 Prepare 被拒。
3. **发布**：把暂存对象放入 `live_objects`/`registrations`（内存插入，不再失败），回复 Prepared。

于是发布出去的私有 registration 必有一个按 ExecutionId 可 inspect/revoke 的记录；没有记录的 id 从未拥有 registration，
未知 id 的 Revoke 走 tombstone 捷径是正确的。**唯一释放例程** `release_private(execution_id)`（资源 gate 下、幂等）由该
id 的每个终态结果调用：Revoke（含未知 id：按绑定查找，按构造为空）、monitor 持久化 Drained、未 fork 的 NoChild、
`open` 对旧 boot 记录（只记账）、`Retire`（已释放则空操作）。它移除 registration 与持有的 root FD；执行结算后终端不再在
executor 中持有目录描述符。崩溃发生在步骤 2、3 之间时进程与 FD 一同消失，重启后记录按 main `open` 转 Revoked。

**这不是资源声明。** 它不产生 WorkspaceRef、LaunchHandle、typed Ready、PreparedLaunch 或 CanonicalWorkspaceId，只持有
Legacy 终端会作为原始 `EnsureProc.cwd`（`terminal_renderer/mod.rs:725-737`）使用的同一路径在 Prepare 时打开的对象；不作
身份证明，不授权任何破坏性 effect。receipt 只存在于标记为 `Private` 的执行记录内，未来的 resource cutover 不得把它读作
资源证据。旧 boot 的进程连同 FD 已不存在（journal 独占 flock，`journal.rs:118-137`；描述符以 CLOEXEC 接收，calm-session
`rights.rs:160`）。

### 7.2 资源交互：与 Legacy 终端对等（S2，F2）

S2a 不给资源新增保证，只让 typed 执行在**既有**路径中得到 Legacy 终端的待遇。**模式在创建时冻结**：终端是 typed 当且仅当
创建事务为 alias `term:<id>` 写入了 `workspace_executions` 行（S2a 从不删除这些行）。typed 终端**只运行一次**，同 Legacy
普通终端：main 对 `kind=terminal` 卡片没有重启入口（重启路由只服务 Claude/Planner 卡片，`claude_restart_adapter.rs:194`
拒绝非 Claude 卡片），再运行就是新卡片；终端重启是 S2a 之外可能的后续消费者特性（§7.3、§11）。`Config` 只决定新建，engine 不论 `Config` 都驱动已有 typed 行。typed 启动遇到 Legacy 占用的 slot
（共享 `term:<id>` 命名空间，`terminal_renderer/mod.rs:706`）按模式冻结只能是缺陷，executor 回复 `Refused`，不是 `Busy`。

**占用谓词只报告。** 唯一占用谓词是 calm-truth `workspace_execution.rs` 的 `unsettled_executions(scope)`
（`settled_at_ms IS NULL`；scope 为 Track、Area 经 `tracks.area_id`、或 alias，Terminal/Card scope 由消费者映射为
`term:<id>`）。它只接入写事务的复查 `require_safe_tx`（遇 typed 占用返回既有 Conflict）；预检 `require_safe` 保持 Legacy
语义（只看 `unresolved_tx` 的 Legacy launch operation），故删除顺序与 Legacy 相同：预检 → 在 Legacy 停止终端的时点 quiesce
→ 写事务复查。两者从不 request、drive 或等待。生命周期动作只在调用者：

- **quiesce（删除调用者，Stop）**：calm-server `execution_quiesce::quiesce(scope)`（按 scope，不按消费者，后续消费者复用）
  在一个事务内对 scope 内全部未结算执行 `request(Stop)`，提交后 `drive` 并 `wait_settled`（`QUIESCE_DEADLINE`）；超时返回
  既有 Conflict，行与 workspace 保留，调用者重试。它位于 Legacy `stop_child`/quiesce 的时点，屏幕保留；预检被拒的删除不
  停止 typed 终端，之后才中止的与 Legacy quiesce 之后中止相同。
- **Retire 只有一个写者**：删除单点 `terminal_delete_tx`（卡片、Track、Area 与 orphan 删除事务都经它删除终端行）。

| 调用者（main） | typed 终端的行为 |
|---|---|
| 卡片删除 `cards.rs:592` / `:616` | `require_safe` 预检 → `quiesce(Card)`；typed 分支的 `reap_terminal_artifacts_with_renderer` 只退订 renderer；写事务 `require_safe_tx` 复查后 `terminal_delete_tx` 写 Retire，提交后 drive，返回 204 |
| Track 删除 `tracks.rs:2844` / `:2947` | teardown 预检不变；在 Legacy 停止终端处（`:2871-2877`）`quiesce(Track)`，`quiesce_terminal_artifacts_for_deletion` 的 typed 分支只退订 renderer；最终写者复查后对每个终端 `terminal_delete_tx`；之后的 recycle 面对的是已结算执行 |
| Area 删除 `areas.rs:434` / `:590` | 同 Track，scope 为 Area |
| orphan 回收 `terminal_sweeper.rs:375` / `:407` | 预检后 `quiesce(Terminal)`；`reap_terminal_artifacts` 对 typed 只退订（`terminals.pid` 为 NULL，`:483-495` 的裸 SIGTERM 不会命中）；写事务复查后 `terminal_delete_tx` |
| settled-worker 回收 `terminal_sweeper.rs:234` | 非删除路径，预检照旧（不含 typed）；IMMEDIATE 认领事务复查 `in_settled_worker_set` 后写 session Exited 并 `request(Stop)`，提交后 drive。行、OutputLog 与屏幕保留，同 Legacy `reap_live_terminal`（`retain_screen=true`）；认领期间 reopen → 认领失败，不 Stop |
| repoint `tracks.rs:2214`、`:2389`（只 `require_safe_tx`） | 遇未结算 typed 执行拒绝，不停止；已停止（已结算）的 typed 终端不阻塞，也不会再运行 |
| `worktree_clean.rs:21-26` | 谓词非空时跳过该 Track，不请求 Stop/Retire：**Track close 不是 stop**；settled-worker 回收停止并结算后照常清理 |
| boot reconcile（calm-server `lib.rs:36-90`） | 跳过有 `workspace_executions` 行的终端：不 Probe、不写 exit 与会话（typed 行未 launch 时 Probe 必为 false） |

**消费者投影**（`SqliteExecutionStore` 按 intent 的 operation kind 注册投影，Terminal 注册一个，与执行写入同一事务）。对
typed 终端每个投影字段只有一个写者：exit 字段只由 `settle`、会话只由创建事务与 `spawned`/`settle` 写（settled-worker
认领的 Exited 是唯一例外）；Legacy 的 boot reconcile、attach reader 与 WS ensure 按模式跳过。

- **创建**：创建事务写卡片、`WorkerSessionKind::Terminal` 会话（starting）、`terminals` 行（`pid` NULL）与 admit。每个终端
  一生只有一个执行，投影无需判断“当前执行”；被删除执行迟到的投影因终端行已在删除事务中随卡片删除而无行可改。唯一需要的
  守卫是会话状态：因 settled-worker 认领也写会话，`spawned` 与 `settle` 的会话写入各是一条 `UPDATE … WHERE` 当前状态
  （starting / 仍活跃）的语句，不先读后写。executor 不等待投影。
- **字段**：`spawned` 把仍为 starting 的会话置 running；`settle` 按 Drained 的 status 写：`Some(c)` → `exit_code = c`；
  `None`（信号终止，每次 Stop 都是，`scope_helper.rs:146-149`）与 NoChild/Revoked → `signal_killed = 1`，同 Legacy
  （`attach_reader.rs:112,124-130`），使 `read.rs:787-812` 保留它、orphan 回收不删被停止的 typed 行；并把仍活跃的会话置
  exited。`pty_output` 不复制：typed 终端的 MCP persisted-exit 读者（`mcp_server/tools/terminal.rs`）经 `Engine::output`
  读 OutputLog 尾部（同 Legacy 8 KiB 尾部语义，保留到 retire）。于是 `SETTLED_WORKER_SESSIONS_SQL`、`terminals_orphaned`
  与 `worktree_clean` 读到与 Legacy 同义的字段。
- **Prepare 被拒**（如 cwd 已删除）：Legacy 在 spawn 失败时回滚卡片（`terminal_adapter.rs:487-530`），tx-only 的 typed 创建
  已提交，于是留下一张已退出的终端卡片（Revoked → `signal_killed = 1`），用户删除即可。这是 tx-only 创建的 UX 差异，不是
  资源问题，不加机制。

破坏性 effect（recycle、teardown、sweep）仍由既有路径在既有时点执行。Legacy 已有的竞态（quiesce 之后、recycle 之前的新
Legacy launch，删除中仍在 worktree 内运行的 Planner/worker 等）对 typed 终端同样存在，各自另开 issue；quiesce 之后新 admit
的 typed 执行由写事务的 `require_safe_tx` 拒绝。

### 7.3 删除、回滚与 alias slot

1. 领域事务：`terminal_delete_tx`（calm-truth `card.rs:565`）在同一事务内执行存储的 request 语句（按当前 alias 写
   `Retire`），返回 `#[must_use] TerminalDeleted{retire: Option<ExecutionId>}`；六个生产调用者（`cards.rs:623`、
   `tracks.rs:2954`、`areas.rs:596`、`terminal_sweeper.rs:414`、`plugin_host/callbacks.rs:524`、calm-truth
   `card_composite.rs:132`）把它带出写闭包，提交后转成 `DriveTicket`。不做外部副作用，回滚时外部状态、OutputLog 与
   renderer 订阅不变。仅测试使用的 `RepoOutOfDomain::terminal_delete`（`out_of_domain.rs:358`）改为同一语句。插件路径
   没有 quiesce：它的 Retire 自带 stop→Wait，丢失的 ticket 由 `recover` 与后续 Track 范围的谓词失败关闭兜底。
2. 提交后：`drive` → stop → Wait → Settle → retire（删除 OutputLog、写 tombstone `{alias, slot_version, outcome}`、移出热
   记录；每步可重放）→ Finish；renderer 在 Retired 后退订；崩溃后由 `recover` 继续。没有调用者等待 retire。

**alias slot。** S2a 没有交接：每个 `term:<id>` 只有一个执行，main 的 launch 裁决原样适用（`launch.rs:40-76`）：slot
空闲 → 版本 1；属于本执行 → 重放；属于另一执行或 Legacy → `Refused`（S2a 总是 `handoff: None`，按模式冻结只能是缺陷）。
tombstone 保留 slot，`open` 从热记录与 tombstone 重建 alias 表，同 main“slot 一经占用不释放”。终端重启若将来成为消费者
特性，需另行给出路由、tx-only kind、等同卡片修改的 C1 授权、幂等键、renderer 切换到新 ExecutionId 与 alias 交接，届时再
评审；S2a 不预留这些机制。

## 8. 按失败说明去除的缺陷类

| #2770 失败 | 机械原因 | 结构决定与改写后的验收 |
|---|---|---|
| `terminal_delete_retains_rows_until_actual_registration_borrower_releases` | 无 Retire 发送者；测试的借用者是 Export（`cleanup.rs:380-430`） | 真实借用者是引用私有 registration 的未结算执行。验收：DELETE 返回 204，执行行保留到 retire 完成 |
| `terminal_product_deletion_releases_operation_storage_and_workspace_anchor` | 无 Cleanup/Retire 发送者；anchor 行无删除者 | 结算释放私有 registration，retire 释放执行存储；anchor 行不存在；workspace 由既有路径处理 |
| `terminal_delete_rollback_retains_screen_and_retries_release_after_restart` | 提交前删除 renderer 条目 | 事务内只写 `Desired`；副作用都在提交后；重启由 `recover` 重驱 |
| `terminal_exact_retired_result_replays_after_executor_restart` | `retirement.rs:8-11` 先比较 boot 再查 `.retired` | 改用执行 tombstone：重放与 executor boot 无关。`RetireRegistration` 不变，S2a 不调用 |
| `terminal_real_output_eof_precedes_durable_drain_and_final_screen` | Drained 仅凭 helper 报告；server 把 Drained 当退出 | §6.4 Drained 顺序：`Complete{final_cursor}` 之前的字节完整；renderer 按序喂入，缺 geometry 历史时 invalidate（main `terminal_renderer/mod.rs:832`），不承诺冷恢复精确屏幕 |
| `recovery_and_ordinary_drive_share_the_effect_serialization_fence` | `apply_recovery_item` 绕过 `drive_mutex` | 每 id 一个驱动任务；typed 创建是新的 tx-only kind，只有一个恢复 owner。通用 operation 缺陷另开 issue |

## 9. 不复用的部分与被取代的条款

不复用 #2770 的 migration 0169、server 侧执行输入/保留/轮询、renderer/sweeper 的 registered 分支与测试 flag；保留 main
executor 的 helper、pidfd、fence、journal、alias 与 ticket 机制，以及 #2770 的 token 映射与回归场景。

本文只取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的以下条款，且只对 S2a 执行消费者生效：

- 「Real PreparedLaunch and frozen executor seam」的 raw-cwd 包装规则与 Register → intent → Prepare 顺序：S2a Terminal
  改为 intent → 带私有 attach 的 Prepare（§7.1），明确不是 PreparedLaunch；该节其余内容留给 resource cutover。
- 「ExternalDir, legacy executors and providers」的 registration 表述：私有 registration 只借用 ExternalDir 的 wire 形状。
- 「Smallest implementation slices」第 1 片的 terminal consumer：S2a 的消费者不是它，不签发 typed Ready。

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
    alias         TEXT NOT NULL,     -- 读者：当前 alias 索引、Terminal/Card scope、模式冻结
    intent_json   TEXT NOT NULL CHECK (json_valid(intent_json)),   -- 不含 secret
    secrets_json  TEXT CHECK (secrets_json IS NULL OR json_valid(secrets_json)), -- ticket secret 与 tokens
    desired       TEXT NOT NULL CHECK (desired IN ('run', 'stop', 'retire')),
    settled_at_ms INTEGER,
    settlement_json TEXT CHECK (settlement_json IS NULL OR json_valid(settlement_json)), -- Settlement 原样持久化
    CHECK ((settled_at_ms IS NULL) = (settlement_json IS NULL)),
    CHECK (secrets_json IS NOT NULL OR settled_at_ms IS NOT NULL)
);
CREATE UNIQUE INDEX workspace_executions_current_alias
    ON workspace_executions(alias) WHERE desired <> 'retire';
CREATE INDEX workspace_executions_unsettled
    ON workspace_executions(track_id) WHERE settled_at_ms IS NULL;
-- 触发器：execution_id/track_id/alias/intent_json 不可改；desired 只能不降（允许同值重写与
-- run→retire）；settled_at_ms/settlement_json 同事务写一次；secrets_json 只置空一次。
```

`pending()` 是 `settled_at_ms IS NULL OR (desired = 'retire' AND secrets_json IS NOT NULL)`：未结算的行，与已结算但仍待
retire 的行；已结算、未要求 Retire 的 Run/Stop 行 Park，保留 secret 与当前 alias，直到删除写 Retire。结算与
`desired` 无关（§6.1）。`load` 在 `secrets_json IS NOT NULL` 时返回 `Stored::Active`（结算前
`settlement: None`，结算后 `Some`）；清空后返回 `Stored::Finished`，保留 metadata 与必需的 Settlement，
不补造 ticket 或 tokens。`wait_settled` 对 Finished 立即返回成功，重复 `drive` 无 executor 调用。
secret 只经 store port 读取，tombstone 后清除，不进入
事件、调试产物或 API。SQL 位于 calm-truth `db/sqlite/workspace_execution.rs`，calm-server 的 `SqliteExecutionStore` 包装
它，`terminal_delete_tx` 与占用谓词复用同一模块；同一模块提供唯一的 Terminal alias helper（Rust 函数与 SQL 片段
`'term:' || terminals.id`），供 Terminal 消费者、Card/Terminal scope 映射与 boot reconcile 查询共用。

## 11. 有序切片

每片独立可合入，按 [track-workspace.md](track-workspace.md) §6 的公共 gate G 验证，等级 L2；测试名是新过滤器。

| # | 结果 | 验收与聚焦测试 |
|---|---|---|
| E1a | supervisor 协议，带真实合同：§4 映射表全部改动（`PreparedSource::Private` 暂存→持久化→发布、唯一私有释放例程、`Inspect`、`Watch`/`Changed`、`Retire` 与 tombstone、`ProcessAction::Stop` 与优雅关闭同一例程、id gate → 资源 gate、未知 id revoke、typed `Busy`、`ObjectQualificationFailed`→Refused）；宿主 boot_id；`OutputLog` 与 `Process` 分离及 Drained 顺序；删除 Cleanup 与 `stop_known`；envelope 4；`STOP_GRACE`/`DRAIN_BUDGET` 常量。main 上 server 无 registered executor 生产调用者 | `-p calm-proc-supervisor`：`private_prepare_failure_after_staging_publishes_nothing`（root 在 FD 接收与暂存重开比对之间被替换、journal 写失败、rename 后父目录 fsync 失败三种调度，之后真实 Inspect→未知 id Revoke→tombstone，断言 `live_objects`/`registrations` 为空、tombstone 优先于残留 `.execution` 文件且 executor 重开后仍然如此、迟到 Prepare 被拒）、`private_root_replaced_after_prepare_launch_no_child`（NoChild 后 `release_private` 已运行、`live_objects` 为空）、`private_prepare_kernel_restart_executor_alive_releases_on_settle`、`private_prepare_executor_restart_releases`、`private_registration_released_at_drained_not_retire`、`private_prepare_exact_replay_keeps_original_description`、`private_attach_refuses_directory_of_other_execution`、`drained_persisted_before_output_complete`、`output_complete_after_reader_eof_or_pty_eio`、`retire_waits_output_open`、`revoke_unknown_id_blocks_late_prepare`、`stop_escalates_after_grace`（含代次唤醒下的重复 Stop，KILL 截止不延后）、`graceful_shutdown_term_ignoring_child_persists_drained`、`host_reboot_is_no_child`、`changed_after_every_persisted_transition`；保留 `execution_launch_revoke_share_gate_and_ticket_is_single_use` 与 main 的交接测试（交接代码不变，S2a 无调用者）。变异验证：EOF 前置、Stop 每次重设截止、发布先于持久化（交换步骤 2、3，第一个测试须红；只杀 executor 不算）、优雅关闭退回只发 TERM。可选再拆：输出存储/Drained 顺序/stop 例程/boot_id 一片，其余协议一片 |
| E1b | `workspace-runtime` crate 的 `execution` 模块：类型、`next_step`、`Engine`、两个 port、最小 fake（脚本化 Fact 序列与调用日志，不复制 journal）、`session_executor`；合同套件在 fake 与 E1a 的真实进程内 executor 上各跑一次 | `-p workspace-runtime`：`next_step` 全表、`absent_stop_unknown_revoke_finishes`、`drained_settles_before_retire`、`drained_between_inspect_and_wait_wakes`、`request_after_park_decision_is_not_lost`、`retire_between_load_and_park_drives`、`recover_racing_drive_effect_once`、`watch_reconnect_wakes_all_waiters`；`-p calm-proc-supervisor`：`port_contract::executor`。对 Park 握手、“快照先于 inspect/Busy 重试”与 `next_step` 的 Settle/Finish 分支做变异验证，断言 fake 调用日志 |
| E2a | migration + `SqliteExecutionStore`（无消费者，store 是 migration 的写者）+ executor 生产配置 + boot 调用 `Engine::recover`。neige-app `proc_supervisor_config`（`supervisor.rs:108-123`，现只传 `--control-sock`）增加 `--execution-state-dir <calm data dir>/execution` 与 `--kernel-client-key-file`；neige-app 在 calm data dir 下以 0600 创建该 key 文件，经 typed 配置把同一路径交给 calm-server，并传 `--stop-grace-ms`（§6.4）。被接管的升级前 supervisor 没有 executor 或 envelope 4 时，`Engine::available()` 为假，typed 创建在提交前得到 `ExecutionUnavailable`，不写行、不回落 Legacy；启用需重启 supervisor，这会结束 Legacy PTY（写入升级说明） | `-p calm-server`：SQLite 上 `port_contract::store`，领域写入与 admit 同事务回滚；触发器与索引拒绝改写；Stop 结算与保留 secret 通过 CHECK；`pending` 含已结算待 retire 行；`-p neige-app`：supervisor 参数与 key 文件权限；`-p calm-proc-supervisor`：`stop_grace` 不大于关闭时限时照常启动、不启用 executor，Legacy `EnsureProc` 可用、握手拒绝经适配器映射为 `Transport`（不 revoke/Park） |
| E2b | opt-in Terminal 消费者 + 占用集成：typed `Config`（默认关闭）；`terminal-execution-create` kind 与 `DriveTicket::committed`；模式冻结；消费者投影；`terminal_delete_tx` 请求 Retire 并返回 `TerminalDeleted`；`unsettled_executions` 接入 `require_safe_tx`；`execution_quiesce::quiesce` 接入四个删除调用者的 Legacy 停止时点；settled-worker 回收的 Stop 分支；`worktree_clean` 跳过；renderer 订阅 `Engine::output`；按模式分支的 Legacy `term:<id>` 入口清单（WS ensure、boot reconcile、`claude_auto_resume`、attach reader 退出写入），防止 Legacy `EnsureProc` 在 typed Launch 前占 slot | `-p calm-server`：§8 六个场景（按改写后的验收）；HTTP 创建、WS 输入/resize/stop、冷启动恢复；`trap '' TERM` shell 的卡片删除与 Track 删除首次即成功；Track 删除对 typed 终端先预检、再 quiesce、后 recycle，预检被拒时 typed 终端仍运行；`Config` 打开时对未启用 executor 的 supervisor 发起 typed 创建，得到 `ExecutionUnavailable` 且无卡片与执行行；Prepare 被拒（cwd 已删除）留下已退出的 typed 卡片且可删除；被 settled-worker 回收停止的 typed 终端（`signal_killed = 1`）在下一次 orphan tick 后仍在，MCP 读到 OutputLog 尾部；boot reconcile 不改未 launch 的 typed 行；repoint 遇未结算 typed 执行拒绝且不停止；closed Track：settled-worker 回收 Stop typed 终端（行与屏幕保留），认领期间 reopen 则不 Stop，clean 在结算前跳过、结算后清理；`kind=terminal` 卡片在两种模式下都没有重启路由；`Config` 关闭后已有 typed 终端仍被驱动；Legacy 终端测试不改 |
| E3… | 每个后续消费者一片：直接 pipe 进程（同片定义 close-stdin/half-close）、gate、provider 启动；同宿主正向排空；可能的终端重启（§7.3 列出的前提，首个需要 alias 交接的消费者再定义交接） | 每片只增加一个 `ExecutionIntent` 构造点或一个恢复机制及其真实入口测试 |

S2a 之外的后续里程碑：**resource cutover**（typed 资源、共享 registration 生命周期、RuntimeSafety closure、Legacy 入口拒
绝、typed release；该资源再无 Legacy 消费者时进行，执行改引用 `PreparedSource::Registered`，engine 不变）、
**workspace_service 迁入 `workspace-runtime`**（不改行为）、borrowed/Legacy adoption、resource task-admission cutover。

## 12. AGENTS.md 架构五点

- **抽象边界**：动作政策只在 `workspace_runtime::execution`；物理事实、settled 谓词、alias slot、fence 与私有 registration
  归属只在 executor；共享安全谓词只报告占用，生命周期动作只在删除/回收调用者；投影的 currentness 在 Terminal 投影 SQL，不进 engine 或 executor；资源与破坏性 effect 留在既有 server 路径。
- **重复逻辑**：一个 `next_step` 与驱动任务；executor 一个 stop 例程（删除 `stop_known`）与一个私有释放例程；Cleanup 并入
  `Retire`；一个占用谓词、一个 scope 级 quiesce、一个代次；每个投影字段对 typed 终端一个写者；Retire 一个写者；时限只由 `STOP_GRACE + DRAIN_BUDGET` 推导。
- **硬编码假设**：engine 不知道 Terminal；alias/OperationRef/Track 由调用者给出；投影按 operation kind 注册；`Desired`、
  `Fact`、`BusyReason` 是本层闭合集合；`term:<id>` 映射只有一个 helper（calm-truth `workspace_execution.rs`，§10），Terminal 消费者与 boot reconcile 查询共用。
- **外部合同事实**：进程与 fence 状态取自 executor 回复，宿主重启取自 `boot_id`，PTY EIO 与 bash 忽略 SIGTERM 由测试固
  定；不从 PID、路径或 dev/inode 推导排空或身份。
- **永久成本**：一张表、两个索引、一组触发器；S2a 不新增交接协议；合同套件在 fake 与真实实现各跑一次；新增消费者、测试或文件不改名单；新
  kind 进入既有 kind 注册表一次。

## 13. 评审处置（第六轮）

第五轮的处置见 6d511f2fd 的本节（第四轮见 b5bff6931，第三轮及更早见 a894efeb8），除下表改动外仍然有效；codex R5-B1
的当前执行条件随重启一起删除（其竞态不再可能），claude R5-N8 的“Retire 两个写者”改为一个。codex 第六轮无发现。

| 评审项 | 处置 |
|---|---|
| claude R6-B1（typed 终端重启没有 Legacy 对应与入口） | 按其最小修复删除，与 Legacy 普通终端对等：typed 终端只运行一次（§7.2）。删除重启事务（Retire 只剩 `terminal_delete_tx`）、intent 的 `replaces`、E1a 的交接改动与版本 +1（main 的 `handoff` 代码不变，S2a 传 `None`，§7.3）、`BusyReason::AliasOccupied`、重启投影与当前执行条件（剩余守卫是 settled-worker 认领所需的会话状态 `UPDATE … WHERE`，§7.2）、`handoff_after_*`、`double_restart_before_fork`、`handoff_owner_settles_between_busy_and_wait`、`terminal_restart_stale_projection_is_ignored`、Legacy 重启模式测试及对应变异。终端重启记为 S2a 之外可能的后续消费者特性（§7.3、§11 E3） |
| claude R6-N1 | `Executor::available()`/`Engine::available()`：typed 创建在 admit 前检查，不可用时返回 `ExecutionUnavailable` 且不写行；握手拒绝或 executor 缺失映射为 `Transport`（§4、E2a、E2b） |
| claude R6-N2 | 记录 tx-only 创建的 UX 差异：Prepare 被拒留下可删除的已退出卡片，不加机制（§7.2、E2b） |
| claude R6-N3 | calm-truth 一个 Terminal alias helper，消费者、scope 映射与 boot reconcile 共用（§10、§12） |
| claude R6-N4 | 条件已删除；剩余会话守卫写明为单条 `UPDATE … WHERE`（§7.2） |

## Upgrade note: E2a executor wiring

New supervisor launches receive an execution journal directory and a kernel-only
0600 capability file from neige-app. calm-server receives the same file path via
typed CLI configuration and schedules pending executions with `Engine::recover`.
This slice adds no Terminal consumer.

An already-running pre-upgrade supervisor is still taken over for Legacy sessions,
but cannot provide the v4 executor handshake. The execution engine stays unavailable;
there is no Legacy fallback for registered execution. Enabling the executor requires
a supervisor restart, which ends existing Legacy PTYs. Plan that restart explicitly
when upgrading. A shutdown grace no greater than `STOP_GRACE + DRAIN_BUDGET` also
disables the executor while keeping Legacy service available.
