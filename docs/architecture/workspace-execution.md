# Workspace execution（S2a 实施合同）

状态：S2a「registered workspace execution」的结构性重设计，替代草稿 PR #2770
（`codex/2666-terminal-vertical`，用户已否决）。第三版：按第二轮两路独立 L2 评审
（codex、claude，均为“需修改”）与 Planner 决定 P1–P7 修订，处置见 §13。只是设计，
不声称已实现；等级 L2。父合同为 [track-workspace.md](track-workspace.md)；本文取代
[workspace-launch-admission.md](workspace-launch-admission.md) 的部分条款（逐条见 §9），
其余条款继续有效。英文类型是合同草案，不代表已有 API。

**本版规则。** main 的 executor 协议（#2716）是执行物理事实与 currentness 的 owner：
registration fence、alias slot/handoff、ticket secret、control capability。`Executor` port
是该协议的薄投影（§4 映射表），只新增 §4 列出的请求；凡 main 已拥有的事实，engine
与 server 复用而不复制。

## 1. Outcome 与范围

1. 独立 crate `workspace-execution` 拥有执行的**动作政策**：根据 `Desired` 与 executor 报告的
   事实决定下一步副作用；只经两个 port（存储、executor），不依赖 calm-server 即可测试。
2. 物理事实与 currentness 只有 executor 一个 owner：进程、journal、输出、alias slot、source
   registration 与 lifecycle fence 都在 calm-proc-supervisor（§6）。
3. source registration 是 **workspace 资源生命周期**，由 workspace service 拥有（§7）。
4. 正常驱动与崩溃恢复走同一个 `Engine::drive`；唤醒来自提交后的 `DriveTicket` 与 executor
   有序变更流，server 不轮询。
5. 两个新 migration、两张表（E2 资源 registration，E3 执行），不建阶段镜像。
6. 未启用 typed `Config` 时 Legacy 行为不变。首个消费者是 opt-in Terminal，只用于
   **fresh-owned、未别名** 的 typed 资源；该资源对全部 Legacy 入口关闭（§7）。

范围外：task-admission cutover（track-workspace S2a 行的独立里程碑）、borrowed/Legacy
adoption（含 Unwitnessed 资源的重新见证）、跨 alias 的完整 RuntimeSafety closure、同宿主
Unknown 的人工出口、精确屏幕重建、S2b/S3/S4/S5、C4 Area 共同执行授权。

## 2. 诊断（已更正归属）

依据：`git diff origin/main...origin/codex/2666-terminal-vertical`（merge-base `ab21d4a46`）。
`Export/ExportAck`（`calm-session` `control/execution.rs:113-114`）、`Cleanup`（`:147`）、
`RetireRegistration`、`durably_settled`、`permits_handoff`、`apply_event` 写 Drained 与输出 ring 都是
**#2716（`4cece86f9`）已合入 main 的 executor 代码**；#2770 新增 `observed_state`、
`RegistrationUnwitnessed`、`RegisteredV3QueryV1`、reader 测试 flag、migration 0169、server 侧 FD 保留、
anchor 行与 renderer/sweeper 的 registered 驱动。本设计因此**修改已合入的 executor 代码及其测试**
（`calm-proc-supervisor/tests/execution_registration/*`，§11 E1a）。

**R1 没有执行动作的 owner。** #2770 中 `stop_original`、`attach_reader`、`ensure_entry`、registered
`stop_child`、`quiesce_original_registered`、`stop_original_registered` 各自读 executor 状态并决定
下一步。executor 内的 `durably_settled`/`permits_handoff`/boot 改写是物理守卫，不是重复政策。
**R2 资源生命期纠缠。** main 的 `Process` 同时拥有 child、PTY master、pidfd 与输出 ring
（`process.rs:20-27`），`Cleanup` 连输出一起删除；#2770 的 server 没有 `Cleanup`/`RetireRegistration`
发送者，deletion 在提交前移除 renderer 条目。**R3 source 持有意图分散**于 server 持 FD、executor
registration 与 anchor 行，三处各自判断 boot，于是需要 Export 与 `RegistrationUnwitnessed`。
**R4 驱动路径不唯一。** main `driver.rs:1184-1194` 的 `apply_recovery_item` 不持 `drive_mutex`
（`:396-404`），main 既有缺陷另开 issue；#2770 又加两条 stop 路径。**R5 终态宣布跨 owner 竞态。**
`apply_event` 仅凭 helper 报告写 Drained（main `mod.rs:601-621`），reader 无 EOF 反馈。

**R6（第二轮）engine 侧自创语义。** 第二版在 engine/server 重建了 main 已拥有的事实：
`EffectAuthority` 复制 registration fence（`mod.rs:320-368` 的 `resolve`，在 Prepare
`preparation.rs:97`、Launch `launch.rs:32`、Interactive `mod.rs:449` 于 scope gate 下检查）；
intent 内的 `AliasHandoff` slot CAS 依赖 executor slot 事实；`store.changed()` 在调用者的 SQLx
事务上无法实现；`AttestNoChild` 把 helper 消失当作子树排空。本版逐项删除或收窄（§13）。

## 3. Crate 边界与依赖方向

```
workspace          (已有：身份类型、HeldSourceObjects、物化/回收机制)
   ▲        ▲
calm-session   workspace-execution
   ▲              ▲   (核心依赖 calm-session 的协议类型；feature `session-executor`：
calm-proc-supervisor   Executor port 的协议适配器；feature `test-support`：fake 与合同套件)
   ▲ (dev-dep)
calm-server ──────────┘ (SqliteExecutionStore、workspace service、HTTP/WS)
```

依赖只有 `workspace`、`calm-session`、`async-trait` 与 async 原语，无环；不并入 supervisor 也依赖的
`workspace` 机制库，以免 executor 依赖 kernel 政策。

| 层 | 拥有 | 不拥有 |
|---|---|---|
| `workspace-execution` | `ExecutionIntent`；`Desired`；`next_step`；`Engine`；每 ExecutionId 一个驱动任务；两个 port；`session_executor` 适配器（只组装 wire、扇出变更流） | 授权、currentness、路径、registration、SQL、HTTP、渲染 |
| calm-proc-supervisor | journal、settled 谓词、alias slot 与 tombstone、registration 活索引与 lifecycle fence、native child/PTY/pidfd、`OutputLog`、stop 升级、有序变更流、Legacy fork 准入 | 领域删除、授权结论 |
| workspace service（server） | resolve；registration 生命周期（Attach、`UpdateRegistration`、恢复查询、`RetireRegistration`）；typed 资源的维护 effect | 执行动作 |
| calm-server 其余 | 路由/WS；auth/C1 结论；`SqliteExecutionStore`；renderer 订阅输出 | 执行状态推导 |

## 4. 公共 API 草案

所有 port 用 `#[async_trait]`（同 `operation/tx_only.rs:30`），trait 带 `Send + Sync`，
future 为 `Send`。wire 类型直接复用 calm-session `control/execution.rs`。

```rust
pub struct ExecutionIntent {             // 不可变；组装为 main 的 PrepareExecution
    pub id: ExecutionId,
    pub alias: ProcessAlias,             // "term:<id>"
    pub owner: OperationRef,
    pub target: ExecutionTarget,
    pub program: ExecutionSpecification, // main 类型：argv/显式 env/io_mode/replay_bytes
    pub ticket: LaunchTicket,            // { id: LaunchTicketId, secret: CapabilityToken }
    pub tokens: ExecutionControlTokens,  // main 类型：observe/interactive/lifecycle
    pub replaces: Option<ExecutionId>,   // 显式 alias 交接；不含 slot 版本
}
pub struct ExecutionTarget {             // 只能由 workspace service 的 resolve 构造
    pub registration: RegistrationFact,  // receipt id、executor_boot、incarnation
    pub scope: ExecutionScope,           // main 类型：binding_id/version、generation、install_epoch、cwd
    pub authority: ExecutionAuthority,   // main 类型：receipt 中冻结的 grant
}
pub enum Desired { Run, Stop, Retire }   // 单调：Run < Stop < Retire

#[must_use] pub struct DriveTicket(ExecutionId);

#[async_trait]
pub trait ExecutionStore: Send + Sync {
    type Tx<'t>: Send;                   // server：sqlx::Transaction<'t, Sqlite>
    async fn insert(&self, tx: &mut Self::Tx<'_>, intent: &ExecutionIntent) -> Result<(), StoreError>;
    async fn request(&self, tx: &mut Self::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError>; // intent+desired+settled
    async fn settle(&self, id: &ExecutionId) -> Result<(), StoreError>; // 写一次，清除 secret
    async fn unsettled(&self) -> Result<Vec<ExecutionId>, StoreError>;
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn prepare(&self, r: &PrepareExecution) -> Result<Fact, ExecError>;
    async fn launch(&self, c: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn revoke(&self, c: &ConsumeExecution) -> Result<Fact, ExecError>;
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn subscribe(&self, id: &ExecutionId) -> Result<FactChanges, ExecError>;
    async fn control(&self, c: ExecutionControl) -> Result<ExecutionReply, ExecError>;
    async fn retire(&self, id: &ExecutionId, lifecycle: &CapabilityToken) -> Result<Fact, ExecError>;
}
pub enum Fact {                          // main 的 ExecutionState 原样投影
    Absent,
    Journal { state: ExecutionState, receipt: Option<ExecutionReceipt>, output: OutputEnd },
    Retired(Tombstone),                  // { execution_id, ticket_id, slot: Option<(alias, version)>, outcome }
}
pub enum OutputEnd { Open, Complete { final_cursor: u64 }, Incomplete(IncompleteReason) }
pub enum ExecError { Busy(BusyReason), Refused(String), Transport(TransportLost) }
pub enum BusyReason { AliasOccupied { owner: ExecutionId }, OutputOpen, UnsettledExecutions, ObservationInFlight }

impl<S: ExecutionStore, X: Executor> Engine<S, X> {
    pub async fn admit(&self, tx: &mut S::Tx<'_>, intent: ExecutionIntent) -> Result<DriveTicket, AdmitError>;
    pub async fn request(&self, tx: &mut S::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<DriveTicket, StoreError>;
    pub fn drive(&self, ticket: DriveTicket);           // 调用者提交后调用
    pub async fn recover(&self) -> RecoverReport;      // 崩溃后备：对 unsettled() 逐个 drive
    pub async fn wait_settled(&self, id: &ExecutionId) -> Result<(), DriveError>; // executor settled 谓词
    pub async fn interact(&self, id: &ExecutionId, a: ProcessAction) -> Result<ExecutionReply, ControlError>; // 仅 WriteStdin/Resize/Signal
    pub async fn output(&self, id: &ExecutionId, from: Option<u64>) -> Result<ExecutionReply, ControlError>;
}
```

`admit`/`request` 借用调用者的 SQLx 事务写入，不触发外部副作用，与领域写入一起回滚；返回的
`DriveTicket` 由调用者在提交后交给 `drive`（`#[must_use]` 加 CI 的 `-D warnings`，丢弃即编译
失败）。`interact`/`output` 由 `inspect` 得到的 receipt（identity、slot_version）与 intent tokens 组装
capability。`EffectAuthority`、`store.changed()`、`Phase`、version/CAS、`ensure_registration` 均已删除。

**Executor port 与 main 协议一一对应（E1a 改动）：**

| port | wire（`ExecutionRequest`） | 相对 main |
|---|---|---|
| `prepare` | `Prepare(PrepareExecution)`：identity 由 intent 与 target 组装，`ticket` 为 intent 中的 secret | `handoff: Option<AliasHandoff>` 改为 `replaces: Option<ExecutionId>` |
| `launch` / `revoke` | `Launch` / `Revoke(ConsumeExecution{execution_id, ticket_id, ticket})` | revoke 改用 ExecutionId 为键的 gate；未知 id 写 Revoked tombstone，之后同 id 的 Prepare 被拒 |
| `inspect` | **新** `Inspect(ExecutionId)` → 记录、tombstone 或 Absent | 新增（无 capability 的只读查询，经 client secret 认证） |
| `subscribe` | **新** `Watch`：每个 executor 一个流，先回 `Watching{boot, seq}`，再推送 `Changed{seq, id}` | 新增；适配器按 id 扇出，port 仍按 id |
| `control` | `Control(ExecutionControl)`：capability 由 receipt 的 identity/slot_version 与 intent tokens 组装 | 删除 `Cleanup`；**新** `ProcessAction::Stop`（lifecycle）；`Observe` 读 `OutputLog` |
| `retire` | **新** `Retire{execution_id, lifecycle}` | 取代 `Cleanup` |
| （workspace service） | `Attach`、`ObserveDirectories`、`ObserveTransferredDirectories` 不变；**新** `UpdateRegistration`、`ListRegistrations`；`RetireRegistration` 改 boot 规则 | 删除 `Export`/`ExportAck` |

回复新增 typed `ExecutionReply::Busy(BusyReason)`；其余拒绝仍是 main 的 `Refused{reason}`，engine
不解析字符串。`EXECUTION_ENVELOPE_VERSION` 提升到 4（daemon 可跨 kernel 升级存活）。

**细则。** prepare/launch/revoke/retire 按 ExecutionId 幂等，prepare 要求整个 `PrepareExecution`
相等。`WriteStdin` 保持 main 语义：单次非阻塞写回复 `StdinProgress{accepted_bytes}`，调用者只重试
剩余后缀，回复丢失为 `Transport`，不自动重发。四种 `Busy` 都会在变更流上得到后续事件；stale/closed
fence、boot 改变、摘要或 ticket 不符都是 `Refused`；只有 `Transport` 让 engine 重新 `inspect`。
`Retire` 与未知 id 的 `Revoke` 写同一种不可变 tombstone（不含 token），保留期无界（ExecutionId
随机），回收需要 kernel 无引用证明，另开 issue。

## 5. 唯一 owner 与唯一入口

| 事实 | owner | 入口 | 存储 |
|---|---|---|---|
| source registration 与 lifecycle fence | executor（物理与 currentness）；workspace service（生命周期意图） | Attach、`UpdateRegistration`、`RetireRegistration`（§7） | executor `live_objects`/`registrations`；`workspace_registrations` 行 |
| Intent | `workspace-execution` | `Engine::admit`，在接受请求的 operation 事务内 | `intent_json`，写一次 |
| Desired | `workspace-execution` | 存储的唯一 request 语句：`Engine::request` 与 `terminal_delete_tx`（§7） | `desired`，单调 |
| 进程、journal、settled、alias slot | executor | `drive` 发出 prepare/launch/revoke/stop/retire；Drained 只由 executor monitor 写 | executor journal 与 tombstone |
| 输出字节与终止 cursor | executor `OutputLog`（按 ExecutionId，独立于 `Process`） | reader 追加；`retire` 删除 | executor journal 目录 |
| 屏幕 | calm-session renderer（server 持有，订阅 `Engine::output`） | 既有 renderer；缺完整 geometry 历史时 invalidate | 内存 |
| 结算标记 | `workspace-execution` | `drive` 在 executor retire 之后写 | `settled_at_ms` |

kernel 不保存 Prepared/Running/Drained 副本，需要时 `inspect`。`settled_at_ms` 的读者是 `unsettled()`
与占用索引；tx-only launch 的 keyed 重放返回 operation 行冻结的**准入结果**（`tx_only.rs:189-203`），
不需要结果载荷。

## 6. 一个状态机，两个谓词

```
 admit(tx)      drive: prepare           drive: launch (desired=Run)
 ∅ ──────▶ Absent ─────────▶ Prepared ───────────────▶ Forking/Spawned/Exited
              │ desired≥Stop 或 Refused: revoke                │ executor monitor
              ▼                                                ▼
           Revoked ───────────────▶ NoChild | Drained{output} ◀┘
                  desired=Retire: retire → Retired(tombstone) → settle
```

**两个谓词。** (1) executor 的 settled 谓词（main `durably_settled`/`permits_handoff`：Revoked、
Drained、NoChild 且已持久化）裁决物理释放：alias 交接与 `RetireRegistration` 由 executor 自己
检查；`Retire` 另要求 `OutputEnd ≠ Open`。(2) `next_step(desired, fact) -> Step` 只选动作，只读
main 的 `ExecutionState`：

| desired \ fact | Absent | Prepared | Forking/Spawned/Exited | Revoked/NoChild/Drained | Unknown | Retired |
|---|---|---|---|---|---|---|
| Run | prepare | launch | Wait | Park | Wait | Settle |
| Stop | revoke | revoke | stop→Wait | Park | Wait | Settle |
| Retire | revoke | revoke | stop→Wait | retire | Wait | Settle |

`Wait` = 等变更流或 drive 通知；`Park` = 无事可做，任务按下述握手退出；`Settle` = 若
`settled_at_ms` 为空则写入后退出。`Busy(_)` → `Wait`；prepare/launch 的 `Refused` → revoke；其他
动作的 `Refused` 是缺陷，任务记录 `DriveError` 后 Park。对 `next_step` 释放分支的变异验证必须断言
fake 的调用日志，否则 fake 自身的 settled 守卫会掩盖变异。

**currentness：只有 executor fence（P1）。** main 的 registration fence 已在 scope gate 下于
Prepare、Launch、Interactive 检查 receipt 相等、`binding_version/generation/install_epoch`、
`admission_open` 与 receipt 中的 authority（`validation.rs:254-317`）。本设计不在 engine 或 server
重复它：

1. **准入。** `admit` 所在事务读取 `workspace_registrations` 的 `ready` 与 `admission='open'`；
   SQLite 写事务串行，因此与关闭事务不交错（N9）。
2. **关闭。** 撤销事务（绑定关闭、Track 删除、release）写 `admission='closing'`（持久的 fence
   更新意图），并对占用集逐个 `request(Stop|Retire)`。提交后 workspace service 发送
   `UpdateRegistration{receipt, admission_open:false}`：executor 取同一 resource scope gate 改写
   内存 fence 并 ACK，随后写 `closed`。ACK 之后该 receipt 上的 Prepare/Launch/Interactive 全部被拒；
   在此之前通过 `resolve` 的 launch 已持有 Reserved 以上的记录，属于占用集。任何破坏性 effect
   都在 ACK **且**占用集全部 settled 之后才执行。kernel 崩溃于 `closing` → 恢复时重发（幂等）；
   executor 重启 → fence 为空（`mod.rs` `open` 不恢复 `registrations`），视为已关闭。
3. **S2a 的 `UpdateRegistration` 只接受 open→closed。** 其余 lifecycle 前进需要新对象，仍经带 FD
   的 Attach（main `source_successor`），fresh-owned 资源在 S2a 中没有。关闭后 stdin/resize 被拒是
   main 现有语义（Interactive 要求 admission）。
4. **只有授权撤销**（grant/会话；main fence 有意不看，`mod.rs:344`）在撤销事务内写 `Desired=Stop`。
   任务在每个 prepare/launch 前重新 `load`，其后提交的撤销至多放过一次 fork，随即被 stop。授权撤销
   之后没有破坏性 source effect，破坏性撤销者都走第 2 步，因此删除 `EffectAuthority`。

**驱动与等待（P3）。** 每个 ExecutionId 至多一个驱动任务；任务表 `Mutex<HashMap<id, Slot{dirty,
notify}>>`。`drive` 在锁内：任务存在则置 `dirty` 并 `notify_one`（Notify 保存许可），否则插入并
spawn。任务每轮：锁内清 `dirty` → `subscribe(id)`（适配器在本地登记等待者，且只在 `Watch` 已回
`Watching` 后返回）→ `load` → `inspect` → `next_step` → 执行一步；`Wait` 时 `select` 等待者与
`notify`。退出前在同一把锁内复查 `dirty`：已置则继续，否则移除自身。先订阅后读取，所以
`inspect` 之后的任何 executor 转移都会唤醒；锁内复查使 Park 与并发 `request` 不丢唤醒。`Watch`
断开或 executor 重启时唤醒全部等待者，各自重新 `inspect`。`recover()` 只是对 `unsettled()` 调
`drive`，与正常驱动共用任务表。server 中没有定时器或轮询。

**stop 升级。** `ProcessAction::Stop` 由 executor 执行固定合同：向 scope 发 SIGTERM，经常量
`STOP_GRACE`（2s）仍未排空，则令 helper 对全部子孙 SIGKILL（复用 `scope_helper.rs:156-165` 的
SIGUSR1→SIGKILL）。宽限不可配置，由合同测试固定；`Signal(Term)` 仍只发信号。

**Drained 的顺序（N4）。** helper 报告 scope Drained → 在 scope gate 下立即持久化
`Drained{status, proof, output: Open}`（与 main `mod.rs:601-621` 相同，排空证明不推迟）→ reap
helper（helper 持有 slave/pipe，`scope_helper.rs:113-120`）→ reader 读到 EOF，或 PTY master 在全部
slave 关闭后的 `EIO`（Linux 行为，由 E1a 测试固定，不引为文档合同）→ `OutputLog` fsync → 持久化
`output: Complete{final_cursor}`。等待 reader 时不持 scope gate，reader 也不等 Drained。其他读错误
记为 `Incomplete(reason)`；executor 重启时 `Open` 记为 `Incomplete(executor_restarted)`。slave fd 经
SCM_RIGHTS 逃出 scope 时 EOF 不到达：scope 仍是 settled（交接与 registration 退役可进行），但
`Retire` 回复 `Busy(OutputOpen)`，不按超时伪造 EOF。pipe 的 close-stdin/half-close 在第一个 pipe
消费者片定义。

**boot 规则。** journal 持久化宿主 `boot_id`（`/proc/sys/kernel/random/boot_id`）。宿主 `boot_id`
改变 → 非终态记录为 `NoChild{host_rebooted}`；同宿主 executor 重启 → 沿用 main `open`（Prepared→
Revoked、未 fork→NoChild、其余→Unknown）。终态事实、tombstone 与已退役 registration 的重放与 executor
boot 无关；launch、interact 与当前 registration 操作检查 boot。

**同宿主残留 Unknown（P5）。** helper 没有 PDEATHSIG，`stop_known` 只发 TERM（`shutdown.rs:48`）；
helper 消失也不证明子树排空（子孙重挂到 init）。因此没有人工 NoChild 出口：Unknown 只让**该资源**
保持 Busy，宿主重启（boot_id）清除它；正向的同宿主排空机制是后续 issue。

## 7. registration、删除与恢复

**registration 生命周期。** opt-in 后 owned materialization 新建的 worktree 有一行
`workspace_registrations`：`materializing` 时先持久化 transfer nonce，再经既有 `Attach`（SCM_RIGHTS）
传 root/Git 布局 FD；executor 放入 `live_objects` 与 fence 后回复 receipt，kernel 关闭自己的 FD、记录
receipt、进入 `ready`。resolve 只在 `ready`、`admission='open'` 且 receipt 属于当前 executor boot 时
构造 `ExecutionTarget`；alias 比较复用 `ObserveDirectories`/`ObserveTransferredDirectories`。Attach 回复
无法送达（对端已关闭，EPIPE）时 executor 按退役路径撤回该次 Attach，故 `ListRegistrations` 未列出即
证明没有活 holder。

**release。** §6 第 2 步关闭 → `wait_settled` 占用集（在关闭提交与 ACK 之后枚举，N9）→
`RetireRegistration(receipt)` → 维护/删除 effect → `released`。executor 退役规则：(a) 先查 `.retired`，
与 boot 无关地重放；(b) 旧 executor boot 的 receipt：旧进程连同 FD 已不存在，引用它的记录都 settled
则写 `.retired` 并回复 `RegistrationRetired{boot_changed}`，否则 `Busy(UnsettledExecutions)`；(c) 当前
boot 走 main 精确路径（`retirement.rs:30-62`）。`ObserveDirectories` 超时后 `spawn_blocking` 观察者
仍持 root `Arc`（`directory_relation.rs:5-54`），是真实借用者：回复 `Busy(ObservationInFlight)`，该
观察者结束时 executor 在 `Watch` 上发事件，release 重试。不经 adoption 不能重新 launch，但都能 release。

**崩溃窗口。**

| 情形 | 恢复 |
|---|---|
| nonce 已持久化，Attach 未发出或未生效 | `ListRegistrations`（本 boot 的活 receipt）不含该 nonce → `unwitnessed`：不可 launch、可 release；release 无 receipt 可退役，不重开路径 |
| Attach 已 ACK，receipt 未落库 | `ListRegistrations` 含该 nonce → 补记 receipt，进入 `ready` |
| Attach 之后、admit 之前 Track 被删除 | release 发现无执行，直接 `RetireRegistration` |
| kernel 重启，executor 存活 | 与 `ListRegistrations` 精确比对（含 executor boot）：匹配者保持 witnessed；`closing` 重发 `UpdateRegistration`。kernel 不需要 FD，不做 path/stat 推断 |
| executor 重启（同宿主） | 全部 registration `unwitnessed`（不可 launch、可 release，adoption 是后续片）；旧 boot intent 的 Prepare/Launch 得到 `Refused` → revoke；release 走规则 (b)，Unknown 执行使该资源 Busy 直到宿主重启 |
| 宿主重启 | 非终态执行为 `NoChild{host_rebooted}`，可 retire；release 走规则 (b) 完成 |

**Legacy 入口（P6）。** typed 资源拒绝**全部** Legacy 入口，而非只拒绝破坏性入口。Legacy 只在两处
得到 cwd：Legacy selection（`track_workspace_bindings` 与 `tracks.workspace_path/workspace_worktree_path`
的连接，`workspace_resource.rs:64-77`；`TrackWorkspace`，calm-types `model.rs:192`），或调用者
给出、最终成为 `EnsureProc.cwd` 的原始路径。typed 资源没有 Legacy selection（E2 以未别名方式创建，
别名收敛被拒），前者不可达。后者的唯一 choke point 是 supervisor 的 Legacy fork 准入
`check_legacy_alias`（`lib.rs:333`），E2 把它扩为 `check_legacy_ingress(alias, cwd)`：
以 supervisor 打开的 cwd 为 subject、用 `ObserveTransferredDirectories` 的持有对象关系判断，cwd
位于任一活 typed root 之内则拒绝；判断与 fork 同在该资源的 scope gate 内，与 `RetireRegistration`
互斥。server 直接 spawn 的进程（planner、gate、app-server）的 cwd 只来自 selection 或 lease 记录，
typed root 从不写入。非 typed 资源不变；无关的人工文件系统活动不在保证内（launch-admission 同条）。

**删除与回滚顺序。**

1. 领域事务：删除终端行并请求 Retire；Track 删除另写 release。不做外部副作用；回滚时外部状态、
   OutputLog 与 renderer 订阅均不变。
2. 提交后：`drive` → stop → Wait → retire（executor：释放 native 残余、删除 OutputLog、写 tombstone
   `{alias, slot_version, outcome}`、移出热记录；每步可重放）→ `settle`；renderer 在 Retired 后退订；
   崩溃后由 `recover` 继续。registration 不随终端删除，随 workspace release 退役。

终端行只在 `terminal_delete_tx`（calm-truth `card.rs:565`）一处被删除。E3 让它在同一事务内执行
存储的 request 语句（按当前 alias 找到执行，写 `Retire`），并返回 `#[must_use]`
`TerminalDeleted{retire: Option<ExecutionId>}`，调用者提交后转成 `DriveTicket`。不维护调用者名单：
新调用者由编译器强制处理。

**终端重启与 alias 交接（P2）。** 同一事务先 `request(Retire)` 旧执行，再 admit
`replaces = Some(旧 id)` 的新执行；部分唯一索引保证每个 alias 只有一个当前 intent。executor 在
launch 时裁决交接：slot 空闲 → 版本 1；slot 属于本执行 → 重放；属于另一执行 → 要求
`replaces.is_some()`，且 slot owner 的热记录满足 settled 谓词或已有 tombstone，则版本 +1，否则
`Busy(AliasOccupied{owner})`，任务等待 owner 的事件。executor 不把 `replaces` 与 slot owner 比较，
也不做 slot 版本 CAS（alias 当前性由 kernel 索引拥有）。tombstone 保留 slot，executor `open` 从热记录
与 tombstone 共同重建 alias 表，因此前任在 launch 前完整退役、其间 executor 重启都不影响交接。
连续两次重启（A 运行，B 未 fork 即被 C 替换）：B 被 revoke，C 等 A settled 后取 slot 版本 2。

## 8. 按失败说明去除的缺陷类

| #2770 失败 | 机械原因 | 结构决定与改写后的验收 |
|---|---|---|
| `terminal_delete_retains_rows_until_actual_registration_borrower_releases` | 无 Retire 发送者；测试的借用者是 Export（`cleanup.rs:380-430`） | 真实借用者是引用 registration 的未 settled 执行与进行中的观察。验收：终端 DELETE 返回 204，执行行保留到 retire 完成；release 在执行 settled 或观察结束前得到 Busy，之后完成 |
| `terminal_product_deletion_releases_operation_storage_and_workspace_anchor` | 无 Cleanup/Retire 发送者；anchor 行无删除者 | 终端删除释放执行存储；anchor 行不存在，registration 由 release 退役。两条验收分开 |
| `terminal_delete_rollback_retains_screen_and_retries_release_after_restart` | 提交前删除 renderer 条目 | 事务内只写 `Desired`；副作用都在提交后；重启由 `recover` 重驱 |
| `terminal_exact_retired_result_replays_after_executor_restart` | `retirement.rs:8-11` 先比较 boot 再查 `.retired` | §7 退役规则 (a)(b)：已退役重放与 boot 无关，旧 boot receipt 回复 `boot_changed` |
| `terminal_real_output_eof_precedes_durable_drain_and_final_screen` | Drained 仅凭 helper 报告；server 把 Drained 当退出 | §6 Drained 顺序：`Complete{final_cursor}` 之前的字节完整；renderer 按序喂入，缺 geometry 历史时 invalidate（main `terminal_renderer/mod.rs:832`），不承诺冷恢复精确屏幕 |
| `recovery_and_ordinary_drive_share_the_effect_serialization_fence` | `apply_recovery_item` 绕过 `drive_mutex` | 每 id 一个驱动任务；terminal launch operation 改为 tx-only（在 `prepare_tx` 内 admit），只有一个恢复 owner。通用 operation 缺陷另开 issue |

## 9. 不复用的部分与被取代的条款

不复用：#2770 的 migration 0169（编号也与协作分支 `0169_conversation_inputs.sql` 冲突）；
`execution_input.rs`、`execution_sources.rs`；`execution_request.rs`/`operation/admission.rs` 的授权表；
`terminal_launch/registered*`（`ExecutionInputRetention`、200ms/2s 轮询）；renderer 的
`RendererExecution::Registered`、`stop_original_registered`、sweeper 的 `quiesce_original_registered`；
supervisor 的 `RegisteredV3QueryV1`、`RegistrationUnwitnessed`、生产二进制的测试 flag 与 `.drained`
文件；约 50 个测试文件的 `TerminalExecutionTransport::Legacy` 改动。

保留或参考：main executor 的 helper、pidfd、registration fence、journal fsync、alias 与 ticket 机制；
#2770 的 token 映射、`FreshInitialGeometry`/不可信重附着的区分、HTTP/WS/崩溃/EOF 回归场景；真实
reader/helper/fsync 时序测试改为 supervisor 包内测试。

本文取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的以下条款：

- 「Identity」中 kernel 保活 held witness（“The kernel retains controlled object leases”）、「Required
  kernel states」的 `SourceObjectLease`：held witness 是 executor registration，`RegisteredObjectWitness`
  即 `RegistrationFact`；kernel 只在 Attach 传输期间持 FD。
- 「Current-object proof」的 kernel 重启重建活索引：改为 `ListRegistrations`。「Crash/restart and
  retirement boundary」表前两行：改为 §7 崩溃窗口表，registration 持有到 `RetireRegistration`。
- 「Real PreparedLaunch and frozen executor seam」：`LaunchHandle` 持 lease 改为 `ExecutionTarget`；
  “each invocation revalidates”就是 main executor fence 的 `resolve`；必要改动第 3 项“recovery export”
  删除，第 2 项由 `ListRegistrations` 满足。Register → durable intent → Prepare → Launch 顺序不变。
- 「RuntimeSafety closure」：“executor revoke/consume … linearize with that seal”由 §6 第 2 步实现
  （`UpdateRegistration` ACK 在 executor scope gate 下）；“all retained new ingress”对 typed 资源由 §7
  Legacy 入口段实现；首个 typed launch 的 closure 范围收窄为 §11 E2，跨 alias 完整 closure 留作后续片。
- 「Smallest implementation slices and acceptance」及首片安全验收：改为 §11，reverse-export 验收删除。
  「One task-admission cutover」内容不变，但不在本计划内。

仍然有效：授权（C1）、身份表、current-object proof 的 FD 传输合同、owned materialization/borrowed
adoption、release/cancellation、ExternalDir/legacy executor 规则。

## 10. 最小 migration

各取合入时 main 的下一个空闲编号（当前最大 0168）；不修改已发布 migration 或既有表；都不加外键
（领域删除不级联，0093 允许的无键 operation 回收不受阻）。

E2：

```sql
CREATE TABLE workspace_registrations (
    workspace_id   TEXT PRIMARY KEY,     -- workspace_resources.id；只有 typed 资源有行
    state          TEXT NOT NULL CHECK (state IN
                     ('materializing','ready','unwitnessed','releasing','released')),
    transfer_nonce TEXT NOT NULL,        -- Attach 之前写入
    receipt_json   TEXT CHECK (receipt_json IS NULL OR json_valid(receipt_json)),
    admission      TEXT NOT NULL CHECK (admission IN ('open','closing','closed'))
);
-- 触发器：nonce 不可改；receipt 写一次；admission 只能 open→closing→closed；
-- state 只能前进（released 为终态）。
```

E3：

```sql
CREATE TABLE workspace_executions (
    execution_id  TEXT PRIMARY KEY,
    operation_id  TEXT NOT NULL,
    alias         TEXT NOT NULL,
    workspace_id  TEXT NOT NULL,
    intent_json   TEXT NOT NULL CHECK (json_valid(intent_json)),   -- 不含 secret
    intent_digest TEXT NOT NULL,
    secrets_json  TEXT CHECK (secrets_json IS NULL OR json_valid(secrets_json)), -- ticket secret 与 tokens
    desired       TEXT NOT NULL CHECK (desired IN ('run', 'stop', 'retire')),
    settled_at_ms INTEGER CHECK (settled_at_ms IS NULL OR desired = 'retire'),
    CHECK ((settled_at_ms IS NULL) = (secrets_json IS NOT NULL))
);
CREATE UNIQUE INDEX workspace_executions_current_alias
    ON workspace_executions(alias) WHERE desired <> 'retire';
CREATE INDEX workspace_executions_occupancy
    ON workspace_executions(workspace_id) WHERE settled_at_ms IS NULL;
-- 触发器：execution_id/operation_id/alias/workspace_id/intent_json/intent_digest 不可改；
-- desired 只能不降（允许同值重写与 run→retire）；settled_at_ms 写一次；secrets_json 只在结算时置空。
```

部分唯一索引让“某 alias 的当前执行”无歧义：交接在同一事务先 Retire 旧行再插入新行（SQLite
按语句检查）。secret 只经 store port 读取，结算时清除，不进入事件、调试产物或 API。SQL 位于
calm-truth `db/sqlite/workspace_execution.rs`（与其他仓储相同），calm-server 的
`SqliteExecutionStore` 包装它，`terminal_delete_tx` 复用同一 request 语句。

## 11. 有序切片

每片独立可合入，按 [track-workspace.md](track-workspace.md) §6 的公共 gate G 验证，等级 L2。
测试名是新增合同过滤器，实现时注册，不是已验证的名字。

| # | 结果 | 验收与聚焦测试 |
|---|---|---|
| E1a | supervisor 协议，带真实合同：§4 映射表全部改动（`Inspect`、`Watch` 有序流、`Retire` 与 tombstone、`ProcessAction::Stop` 升级、`replaces` 交接、ExecutionId gate 的 revoke、`UpdateRegistration`、`ListRegistrations`、`RetireRegistration` boot 规则、Attach 回复失败撤回、typed `Busy`、观察者结束事件）；宿主 boot_id；`OutputLog` 与 `Process` 分离及 §6 Drained 顺序；删除 Export/ExportAck/Cleanup；envelope 4。server 在 main 上无 registered executor 生产调用者 | `-p calm-proc-supervisor`：`drained_persisted_before_output_complete`、`output_complete_after_reader_eof_or_pty_eio`、`retire_waits_output_open`、`handoff_after_retired_previous`、`handoff_after_retire_and_executor_restart`、`double_restart_before_fork`、`revoke_unknown_id_blocks_late_prepare`、`stop_escalates_after_grace`、`host_reboot_is_no_child`、`update_registration_refuses_later_launch`、`retired_registration_replays_across_boot`、`old_boot_registration_retires_boot_changed`、`observation_timeout_busy_then_event`、`attach_reply_undeliverable_rolls_back`、`watch_sequence_covers_every_transition`。改写 `tests/execution_registration/*` 中依赖 Export/Cleanup/RetireRegistration 的用例。对 EOF 前置、boot 无关重放与交接谓词做变异验证 |
| E1b | `workspace-execution` crate：类型、`next_step`、`Engine`、两个 port、最小 fake（脚本化 Fact 序列与调用日志，不复制 journal）、`session_executor`；合同套件在 fake 与 E1a 的真实进程内 executor 上各跑一次 | `-p workspace-execution`：`next_step` 全表、`drained_between_inspect_and_wait_wakes`、`request_after_park_decision_is_not_lost`、`retire_between_load_and_park_drives`、`handoff_owner_settles_before_subscribe`、`recover_racing_drive_effect_once`、`watch_reconnect_wakes_all_waiters`；`-p calm-proc-supervisor`：`port_contract::executor`。对 Park 握手与 subscribe-before-inspect 做变异验证 |
| E2 | workspace service 与 typed opt-in `Config`（默认关闭），**仅限 fresh-owned、未别名资源**：E2 migration；materializing→ready、Attach、receipt、`ListRegistrations` 恢复、`unwitnessed`；`admission` 关闭与 `UpdateRegistration`；release 与 §7 退役规则；resolve 产出 `ExecutionTarget`；本资源上的维护 effect（`worktree_clean.rs:54-69` 的定时清理、recycle、sweep、area compensation、repoint、Track 删除）在 ACK 与占用 settled 之后执行；`check_legacy_ingress`；别名收敛被拒，该 Track 保持 LegacyOnly | `nonce_persisted_attach_never_sent_is_unwitnessed`、`attach_then_crash_before_receipt_recovers`、`track_deleted_between_attach_and_admit_retires`、`kernel_restart_with_live_executor_restores_witness`、`kernel_restart_resends_closing_fence`、`executor_restart_unwitnessed_not_launchable_but_releasable`、`release_after_executor_restart_completes`、`release_after_host_reboot_completes`、`maintenance_waits_fence_ack`、`legacy_start_refused_before_or_after_maintenance_reservation`、`typed_resource_has_no_legacy_selection`、`alias_convergence_refused` |
| E3 | E3 migration + `SqliteExecutionStore` + opt-in Terminal 消费者同片：typed 资源上的终端为 typed；terminal launch operation tx-only admit；`terminal_delete_tx` 请求 Retire 并返回 `TerminalDeleted`；E2 的占用改读 `workspace_executions`；renderer 订阅 `Engine::output`；boot 调用 `Engine::recover` | `-p calm-server`：SQLite 上 `port_contract::store`，领域写入与 admit 同事务回滚；触发器与索引拒绝改写；§8 六个场景（按改写后的验收）；HTTP 创建、WS 输入/resize/stop、冷启动恢复；清理预留先赢与 typed launch 先赢两种次序；Legacy 终端测试不改 |
| E4… | 每个后续消费者一片：直接 pipe 进程（同片定义 close-stdin/half-close）、gate、provider 启动；Unwitnessed adoption；同宿主正向排空 | 每片只增加一个 `ExecutionIntent` 构造点或一个恢复机制及其真实入口测试 |

## 12. AGENTS.md 架构五点

- **抽象边界**：动作政策只在 `workspace-execution`；物理事实、settled 谓词、alias slot 与
  currentness fence 只在 executor；registration 生命周期与 resolve 在 workspace service。适配器
  只组装 main 的 wire 类型。
- **重复逻辑**：server 多处 stop/退出推导收为 `next_step` 与一个驱动任务；Cleanup 并入 `Retire`；
  Export/ExportAck 由一次查询取代；`EffectAuthority` 删除，currentness 只有 executor fence；
  slot CAS 删除，交接只用 executor 谓词；输出只有一条读取路径。
- **硬编码假设**：engine 不知道 Terminal；alias/OperationRef 由调用者给出；`Desired`、`Fact`、
  `BusyReason` 是本层闭合集合；`ExecutionState` 直接复用 main。
- **外部合同事实**：进程、registration 与 fence 状态取自 executor 回复；宿主重启取自内核 `boot_id`；
  PTY EIO 是 Linux 行为，由测试固定；不从 PID、helper 身份、路径或 dev/inode 推导排空或身份。
- **永久成本**：两张表、三个索引、两组触发器；合同套件 fake 与真实实现各跑一次；新增终端删除
  调用者由 `#[must_use]` 强制；新增消费者、测试或文件不改任何名单。

## 13. 评审处置

第一轮（codex B1–B4、N1–N7；claude B1–B5、N1–N14），以本版为准：

| 评审项 | 处置 |
|---|---|
| codex B1、claude B3 | §4：port async（`async_trait`、Send），`admit/request` 借用 SQLx 事务；E3 共事务回滚 |
| codex B2、claude B1 | §7：registration 归 workspace service，`ListRegistrations` 活索引，§9 取代 kernel 持 FD 条款 |
| codex B3 | 第二轮改为 executor fence（§6），`EffectAuthority` 删除 |
| codex B4、claude N8 | §5、§8：OutputLog 只拥有字节与 cursor，一条输出路径；renderer 与 geometry invalidation 保留 |
| codex N1、N2、N7；claude N2、N12 | §3 feature 隔离适配器、无环；§6 两个谓词，变异验证断言 fake 调用日志；§9 保留清单 |
| codex N3、N4、N5；claude N3、N5、N6 | §4 ExecutionId gate 与 tombstone；§6 Drained 顺序；无 version/CAS；第二轮 N2/N3/N4 进一步收窄 |
| codex N6；claude B4、N10、N11 | §11 E2 逐项列出；E1 与真实 executor 同片、迁移与首个写者同片；task-admission 移出 |
| claude B2、B5 | §6 驱动与等待、宿主 boot_id；第二轮 R2-B2 与 R2-B1 继续收紧 |
| claude N1、N4、N7、N9 | §2 归属；§8 真实借用者；§10 占用与 alias 索引；launch operation tx-only |
| claude N13、N14 | 名单改为 `terminal_delete_tx` 单点；§10 无外键，`apply_recovery_item` 另开 issue |

第二轮：

| 评审项 | 处置 |
|---|---|
| codex R2-B1、claude B5 | 删除 `AttestNoChild`；同宿主 Unknown 只让该资源 Busy，宿主重启清除；正向排空列为后续（§6、§11 E4） |
| codex R2-B2、claude B3 | 删除 `store.changed()`；`#[must_use] DriveTicket`；先 subscribe 后 load/inspect；锁内 dirty 复查的 Park 握手；全局 `Watch` 断开唤醒全部；E1b 六个确定性竞态测试 |
| codex R2-B3、claude B4 | §7 退役规则：已退役重放与 boot 无关；旧 boot receipt 在引用执行 settled 后回复 `boot_changed`；Unwitnessed 不可 launch 但可 release，不依赖 adoption；E2 两个 release 测试 |
| codex R2-B4、claude B2 | intent 只持 `replaces`；executor 以 settled 谓词或 tombstone 交接，不做 slot CAS；tombstone 保留 slot 并参与 `open` 重建；重启两次与退役后重启测试 |
| codex R2-B5 | §7 Legacy 入口：typed 资源无 Legacy selection；原始 cwd 在 `check_legacy_ingress` 单点于 gate 内拒绝；非 typed 不变 |
| claude B1 | 单一 fence 为 main executor fence；新增 FD-less `UpdateRegistration`（S2a 只 open→closed），撤销写 `closing` 意图、提交后等 ACK 再做破坏性 effect；`ExecutionTarget` 携带 `ExecutionScope` 与 `ExecutionAuthority`；删除 ticket-gate 线性化说法 |
| claude B6 | E2 自带 migration（`workspace_registrations`：状态（`releasing` 即 release 意图）、nonce、receipt、admission 意图）与 typed opt-in `Config`；§1、§10、§12 改为两表；新增“nonce 已持久化、Attach 未发出”崩溃行 |
| codex R2-N1、claude N5 | intent 持 `LaunchTicket{id, secret}`，ConsumeExecution 带 secret；control capability 由 receipt 的 slot_version 组装；secret 只存于结算时清除的 `secrets_json` |
| codex R2-N2 | 保留 executor 真实借用者守卫：`Busy(ObservationInFlight)` 与观察者结束事件；删除“不会 Busy”的说法 |
| codex R2-N3、claude N8 | E1 拆为 E1a（supervisor 协议与真实合同）与 E1b（crate、fake、适配器、合同套件跑真实 executor）；不合入只有 fake 的 engine |
| codex R2-N4、claude N3 | tx-only 重放返回 operation 行的准入结果；`retired_json` 改为 `settled_at_ms` 与 CHECK；alias/operation/workspace 投影不可改；允许同值 Desired 与 run→retire |
| claude N1、N6 | `Busy` 只剩四种，均有后续事件，stale registration 归 `Refused`；每个 executor 一个 `Watch` 有序流，适配器扇出 |
| claude N2、N4 | 删除 stdin `(id, seq)`，保留 main `StdinProgress`；排空证明立即持久化，输出完成是后续事实，重启记 `Incomplete(executor_restarted)`，EIO 由测试固定 |
| claude N7 | 删除 reap 调用者名单；`terminal_delete_tx` 单点请求 Retire，`#[must_use]` 强制调用者 |
| claude N9 | admit 事务内读取 `ready`/`admission`；release 在关闭提交与 ACK 之后枚举占用 |
| claude N10 | 删除 `ExternalDir` 目标变体与 `target_kind` 列（S2a 无消费者） |
