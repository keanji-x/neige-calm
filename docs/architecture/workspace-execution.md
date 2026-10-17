# Workspace execution（S2a 实施合同）

状态：S2a「registered workspace execution」的结构性重设计，替代草稿 PR #2770
（`codex/2666-terminal-vertical`，用户已否决）。第二版：按首轮两路独立 L2 评审
（codex、claude，均为“需修改”）与 Planner 决定 D1–D8 修订，处置见 §13。只是设计，
不声称已实现；等级 L2。父合同为 [track-workspace.md](track-workspace.md)；本文取代
[workspace-launch-admission.md](workspace-launch-admission.md) 的部分条款（逐条见 §9），
其余条款继续有效。英文类型是合同草案，不代表已有 API。

## 1. Outcome 与范围

1. 独立 crate `workspace-execution` 拥有执行的**动作政策**：根据领域写入的
   `Desired` 与 executor 报告的 `Fact`，决定下一步请求哪个副作用。它以 port 访问
   存储、executor 与 effect-time 授权，不依赖 calm-server 即可测试。
2. 物理事实只有 executor 一个 owner：进程、journal、输出与 source registration 的
   持有都在 calm-proc-supervisor。“可否物理释放”由 executor 的 settled 谓词裁决
   （§6）。
3. source registration 是 **workspace 资源生命周期**，由 workspace service 在 resolve 时、
   admit 之前完成（D1）；执行引擎不创建、不退役 registration。
4. 正常驱动与崩溃恢复走同一个 `Engine::drive`；等待由 executor 的变更订阅唤醒，
   server 不轮询（D3）。
5. 一个新 migration、一张表，不建阶段镜像；已发布 migration 字节冻结。
6. 未配置 registered executor 时 Legacy 默认行为不变。首个消费者是 opt-in Terminal，
   只用于 **fresh-owned、未别名** 的资源（§11 E2）。

范围外：task-admission cutover（移到 track-workspace S2a 行，独立里程碑）、
borrowed/Legacy adoption 与跨 alias 的完整 RuntimeSafety closure（launch-admission
既有条款，后续 workspace-service 片）、精确屏幕重建（D5）、S2b/S3/S4/S5、C4 Area
共同执行授权。

## 2. 诊断（已更正归属）

依据：`git diff origin/main...origin/codex/2666-terminal-vertical`（merge-base `ab21d4a46`；
#2756 已于 `616fcc379` 合入）。首版把若干机制归给 #2770，这是错误的：
`Export/ExportAck`（`calm-session` `control/execution.rs:113-114`）、`ProcessAction::Cleanup`
（同文件 `:147`）、`RetireRegistration` 与 `retirement.rs`、`durably_settled`、`permits_handoff`、
`apply_event` 写 Drained、`process.rs` 的输出 ring，都是 **#2716（`4cece86f9`）已合入
main 的 executor 代码**。#2770 新增的是 `observed_state`、`RegistrationUnwitnessed`、
`RegisteredV3QueryV1`、reader 测试钩子与 flag、migration 0169、server 侧 FD 保留、
anchor 行、renderer/sweeper 的 registered 驱动。因此本设计**修改已合入的 executor 代码
及其测试**（`calm-proc-supervisor/tests/execution_registration/*`），见 §11 E1。

**R1 没有执行动作的 owner。** #2770 中同一次执行的动作由 server 多处各自决定：
`stop_original`、`attach_reader` 把 Drained 当退出、`ensure_entry`、registered
`stop_child`、`quiesce_original_registered`、`stop_original_registered`，各自读 executor 状态
并决定下一步。executor 内的 `durably_settled`/`permits_handoff`/boot 改写等是物理事实守卫，
不是重复政策（首版“15 处收成 1 处”的说法已删除，见 §6）。

**R2 资源生命期纠缠。** main 的 `Process` 同时拥有 child、PTY master、pidfd 与输出 ring
（`process.rs:20-27`），所以 `Cleanup` 释放句柄时连输出一起删除。#2770 的 server 生产代码
没有 `Cleanup`/`RetireRegistration` 发送者；deletion 在提交前移除 renderer 条目
（`stop_original_registered`），事务回滚时屏幕已经丢失。

**R3 source 持有意图分散。** #2770 的 server 以 `ExecutionInputRetention::Held` 持 FD、
冷恢复改 `OriginalControlOnly`；executor 持 registration；anchor 行是第三处**意图**
（不是第三份物理 FD）。三处各自判断 boot，于是需要 Export 反向传 FD 与
`RegistrationUnwitnessed`。

**R4 驱动路径不唯一。** main `driver.rs:396-404` 的 `drive()` 持 `drive_mutex`，
`apply_recovery_item`（`driver.rs:1184-1194`）调用 `drive_one` 不持锁；这是 main 既有
缺陷，另开 issue，不并入本片。#2770 又加了 renderer `stop_child` 与 sweeper quiesce 两条
stop 路径。

**R5 终态宣布跨 owner 竞态。** `apply_event` 仅凭 helper 报告写 Drained（main
`mod.rs:601-621`），reader 线程没有 EOF 反馈；#2770 的 `attach_reader` 再把 Drained 转成
退出，用当时已读到的输出持久化。

#2770 的新增行为几乎全落在 calm-server 的 terminal 模块（17 个 HTTP 级测试）；
supervisor/session/workspace 本身已有独立测试（如 `execution/monitor_tests.rs`）。

## 3. Crate 边界与依赖方向

```
workspace          (已有：身份类型、HeldSourceObjects、物化/回收机制)
   ▲        ▲
calm-session   workspace-execution
   ▲              ▲   (核心依赖 calm-session 的 ProcessAction 子集、ExecutionControlTokens；
calm-proc-supervisor   feature `session-executor`：Executor port 的协议适配器；
   ▲ (dev-dep)         feature `test-support`：fake 与 port 合同套件)
calm-server ──────────┘ (实现 ExecutionStore、EffectAuthority；workspace service；HTTP/WS)
```

依赖只有 `workspace`、`calm-session`、`async-trait` 与 async 原语，无环（supervisor 依赖
workspace/session，不依赖本 crate 的非 dev 部分）。不并入 `workspace`：它是 supervisor
也依赖的机制库，放入 kernel 编排会让 executor 依赖 kernel 政策。

| 层 | 拥有 | 不拥有 |
|---|---|---|
| `workspace-execution` | `ExecutionIntent`/摘要；`Desired`；`next_step`；`Engine`；每 ExecutionId 一个驱动任务；三个 port；`session_executor` 适配器（只映射 token/wire/错误） | 授权、路径、registration、SQL、HTTP、渲染 |
| calm-proc-supervisor | journal、settled 谓词、registration 活索引（`live_objects`）、native child/PTY/pidfd、`OutputLog`、stop 升级、变更订阅、`Retire(ExecutionId)` | 领域删除、授权 |
| workspace service（server） | resolve；registration 生命周期（Attach、恢复查询、admission close、`RetireRegistration`）；`EffectAuthority` 实现；本片范围内的破坏性入口守卫 | 执行动作 |
| calm-server 其余 | 路由/WS；auth/C1 结论；`SqliteExecutionStore`；renderer（calm-session 模型）订阅输出 | 执行状态推导 |

## 4. 公共 API 草案

所有 port 用 `#[async_trait]`（同 `operation/tx_only.rs:30`），trait 带 `Send + Sync`，
future 为 `Send`，可被 Tokio spawn。

```rust
pub struct ExecutionIntent {             // 不可变；摘要参与 executor Prepare 相等性
    pub id: ExecutionId,
    pub alias: ProcessAlias,             // "term:<id>"
    pub owner: OperationRef,
    pub target: ExecutionTarget,
    pub authority: AuthorityConclusion,  // auth owner 的密封结论，crate 不解释
    pub program: ProgramPlan,            // argv/env allowlist/io=Pty|Pipe/相对 cwd
    pub ticket: LaunchTicketId,
    pub tokens: ExecutionControlTokens,
    pub handoff: Option<AliasHandoff>,
}
pub enum ExecutionTarget {               // 只能由 workspace service 的 resolve 构造
    Workspace { workspace: WorkspaceRef, registration: RegistrationFact },
    ExternalDir { registration: RegistrationFact },
}
pub struct RegistrationFact { pub id: ObjectRegistrationId, pub executor_boot: ExecutorBootId,
                              pub incarnation: SourceObjectIncarnation }
pub enum Desired { Run, Stop, Retire }   // 单调：Run < Stop < Retire

#[async_trait]
pub trait ExecutionStore: Send + Sync {
    type Tx<'t>: Send;                   // server：sqlx::Transaction<'t, Sqlite>
    async fn insert(&self, tx: &mut Self::Tx<'_>, intent: &ExecutionIntent) -> Result<(), StoreError>;
    async fn request(&self, tx: &mut Self::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError>; // intent+desired+settled
    async fn settle(&self, id: &ExecutionId, m: &SettledMarker) -> Result<(), StoreError>; // 写一次
    async fn unsettled(&self) -> Result<Vec<ExecutionId>, StoreError>;
    async fn changed(&self) -> DesiredChanges;  // 提交后的 desired 变更通知（进程内）
}

#[async_trait]
pub trait EffectAuthority: Send + Sync {  // server 实现；drive 在每个新副作用之前调用
    async fn permit(&self, intent: &ExecutionIntent, effect: NewEffect) -> Result<(), Revoked>;
}
pub enum NewEffect { Prepare, Launch }

#[async_trait]
pub trait Executor: Send + Sync {
    async fn prepare(&self, intent: &ExecutionIntent) -> Result<Fact, ExecError>;
    async fn launch(&self, id: &ExecutionId, ticket: &LaunchTicketId) -> Result<Fact, ExecError>;
    async fn revoke(&self, id: &ExecutionId, ticket: &LaunchTicketId) -> Result<Fact, ExecError>;
    async fn stop(&self, id: &ExecutionId, lifecycle: &CapabilityToken) -> Result<Fact, ExecError>;
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn subscribe(&self, id: &ExecutionId) -> Result<FactChanges, ExecError>;
    async fn retire(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn interact(&self, cap: ExecutionCapability, a: Interaction) -> Result<InteractReply, ExecError>;
    async fn read_output(&self, cap: ExecutionCapability, cursor: OutputCursor) -> Result<OutputChunk, ExecError>;
}
pub enum Fact { Absent, Prepared, Running(ExecutionReceipt), Drained(DrainReceipt),
                Unknown(UnknownReason), Retired(DrainReceipt) }
pub enum Interaction { WriteStdin { seq: u64, bytes: Vec<u8> }, Resize(Geometry), Signal(ProcSignal) }
pub enum ExecError { Busy(BusyReason), Refused(Refusal), Transport(TransportLost) }

impl<S: ExecutionStore, A: EffectAuthority, X: Executor> Engine<S, A, X> {
    pub async fn admit(&self, tx: &mut S::Tx<'_>, intent: ExecutionIntent) -> Result<ExecutionId, AdmitError>;
    pub async fn request(&self, tx: &mut S::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    pub fn drive(&self, id: &ExecutionId);              // 确保该 id 的唯一驱动任务在运行
    pub async fn recover(&self) -> RecoverReport;      // 对 unsettled() 逐个 drive
    pub async fn wait_settled(&self, id: &ExecutionId) -> Result<SettledMarker, DriveError>;
    pub async fn interact(&self, id: &ExecutionId, a: Interaction) -> Result<InteractReply, ControlError>;
    pub async fn output(&self, id: &ExecutionId, cursor: OutputCursor) -> Result<OutputChunk, ControlError>;
}
```

`admit`/`request` 借用调用者的 SQLx 事务 `await` 写入，不触发外部副作用；真实领域
写入与 execution 写入共用一个事务并一起回滚。`drive` 在提交后执行全部副作用。
首版的 `Phase` 类型、`version`/CAS、`ensure_registration` 与 `control(Cleanup)` 均已删除：
状态就是 `Fact`；lifecycle 动作只经 `drive`，`interact` 不接受 Cleanup/retire/stop。
输出只有 `read_output` 一条读取路径（适配器映射到 wire `Observe`）。

port 细则：

- **幂等键。** prepare/launch/revoke/retire 按 ExecutionId 幂等，prepare 还要求摘要相等。
  `WriteStdin` 不按 ExecutionId 幂等：同一执行写两次相同字节可以是两次合法动作，
  executor 按 `(id, seq)` 至多执行一次；回复丢失时报告 `Transport`，调用者不自动重发。
- **未知 id 的 revoke。** executor 以 ExecutionId 为键的 gate（Prepare 与 revoke 共用，
  不再从已加载 record 取 gate，main `mod.rs:379-382`）写入绑定 `ticket_id` 的 Revoked
  tombstone；之后同 id 的 Prepare 被拒绝。tombstone 与 `.retired` 小且不可变，保留期无界
  （ExecutionId 为随机值）；回收它们需要 kernel 侧无引用证明，另开 issue，不在 S2a。
- **typed 错误。** `Busy`（alias handoff 未就绪、registration 不在本 boot、借用中）、
  `Refused`（摘要不等、ticket 已撤销、stale boot/registration）与 `Transport`（结果未知）
  分开；只有 `Transport` 让 engine 重新 `inspect`，不当作任何事实。

## 5. 唯一 owner 与唯一入口

| 事实 | owner | 入口 | 存储 |
|---|---|---|---|
| source registration | workspace service（生命周期）；executor（物理持有） | resolve 前 Attach；release 时 `RetireRegistration`（§7） | 资源行的 receipt；executor `live_objects` |
| Intent | `workspace-execution` | `Engine::admit`，在接受请求的 operation 事务内 | `intent_json`，写一次 |
| Desired | `workspace-execution` | `Engine::request`，在领域/撤销事务内 | `desired`，单调 |
| 新副作用的当前授权 | auth/C1 + workspace service | `EffectAuthority::permit`，由 `drive` 在 prepare/launch 前调用 | 既有绑定/资源/会话行 |
| 进程、journal、settled | executor | `drive` 发出 prepare/launch/revoke/stop；Drained 只由 executor monitor 写 | executor journal |
| 输出字节与终止 cursor | executor `OutputLog`（按 ExecutionId，独立于 `Process`） | reader 追加；`retire` 删除 | executor journal 目录 |
| 屏幕 | calm-session renderer/model（server 持有，订阅 `Engine::output`） | 既有 renderer；缺完整 geometry 历史时 invalidate | 内存 |
| 结算标记 | `workspace-execution` | `drive` 在 executor retire 之后写 | `retired_json` |

kernel 不保存 Prepared/Running/Drained 副本，需要时 `inspect`。`retired_json` 是最小
结算标记 `{outcome: exited|revoked|no_child, status}`，读者有两个：(1) `unsettled()` 与
workspace 占用查询（是否为 NULL）；(2) launch operation 的 completed-key 重放——在
Track 删除、executor 记录退役后返回原结果，不访问 executor 或当前对象
（launch-admission「completed keyed replay」条款）。它不含阶段、cursor 或 receipt 镜像。

## 6. 一个状态机，两个谓词

```
 admit(tx)      drive: permit+prepare    drive: permit+launch (desired=Run)
 ∅ ──────▶ Absent ─────────────▶ Prepared ──────────────────▶ Running
              │ desired≥Stop 或 permit 撤销 / stale registration: revoke     │ executor monitor
              ▼                                                              ▼
           Drained(Revoked) ──────────────▶ Drained(NoChild | Exited) ◀──────┘
                       desired=Retire: retire → Retired → settle(retired_json)
```

**两个谓词。** (1) executor 的 settled 谓词（main 的 `durably_settled`/`permits_handoff`）
是物理释放的唯一裁决：retire、alias handoff、`RetireRegistration` 都由 executor 自己
检查它，不信任调用者。(2) `next_step(desired, fact) -> Step` 只选择动作，只读 `Fact`，
不推断事实：

| desired \ fact | Absent | Prepared | Running | Drained | Unknown | Retired |
|---|---|---|---|---|---|---|
| Run | permit→prepare | permit→launch | Wait | Park | Wait | Settle |
| Stop | revoke | revoke | stop→Wait | Park | Wait | Settle |
| Retire | revoke | revoke | stop→Wait | retire | Wait | Settle |

`Wait` = 等待 executor 订阅或 desired 变更；`Park` = 无事可做，驱动任务退出，直到下次
`request`/恢复；`Settle` = 若 `retired_json` 为空则写入后退出。`Busy(HandoffPending)`
让任务订阅前一执行；`Refused(stale boot/registration)` 与 `Revoked` 都映射到 revoke，
得到 `Drained(Revoked)`（D1）。对 `next_step` 释放分支的变异验证必须断言 fake 的调用
日志，否则 fake 自身的 settled 守卫会掩盖变异。

**驱动与等待（D3）。** 每个未结算 ExecutionId 至多一个 engine 驱动任务，该任务就是
串行化 fence，正常驱动与 `recover()` 都只是确保它存在。任务循环：`load` → `inspect`
→ `next_step` → 执行一步；`Wait` 时 `select` executor 的 `subscribe(id)` 与
`store.changed()`。订阅流在 executor 断连/重启时结束，重连事件即唤醒，之后重新
`inspect`。server 中没有定时器或轮询。

**stop 升级（D3）。** `stop` 由 executor 执行固定合同：向 scope 发 SIGTERM，经过
executor 常量 `STOP_GRACE`（2s）仍未排空，则令 helper 对全部子孙发 SIGKILL（复用
`scope_helper.rs:156-165` 的 SIGUSR1→SIGKILL 路径）。宽限值不可配置，由合同测试固定。

**effect-time 授权与线性化（D2）。** `drive` 在 prepare 与 launch 之前调用
`EffectAuthority::permit`，两者之间除该 effect RPC 本身外没有其他 await。permit 复核
operation 仍拥有该请求、绑定/资源版本与 Ready 状态、授权结论仍有效（复用 #2770
`auth/execution.rs` 的会话复核与 `admit_new_crossing` 的不变量）。所有撤销者——绑定
关闭/重指向、Track 删除、release intent、授权撤销——在**撤销事务内**对受影响执行写
`Desired=Stop|Retire`，撤销因此只经这一个状态机。线性化点是 executor 现有的单 ticket
gate：launch 与 revoke 消费同一 ticket（main `ConsumeExecution`）。permit 之后提交的撤销
挡不住已在途的那一次 launch RPC：executor 先处理 launch，则它是“抢在封闭之前的 fork”，
成为占用执行，由同一任务下一轮（每步前重新 `load` desired）`stop` 收敛；先处理 revoke，
则 launch 被拒绝。窗口至多一个 RPC。撤销者对后续破坏性 effect 的屏障是该执行的
settled 事实，而不是 DB 提交（launch-admission「RuntimeSafety」中“won before sealing”条款）。
因此不需要 version 列或 permit 版本号（D2 所举的版本检查只是示例）。stop/revoke/retire/读输出不需要 permit：
撤销后原 scope 仍须能收敛，completed 重放也不需要当前路径。

**Drained 的顺序（N3）。** executor monitor 依次：helper 报告 scope Drained → reap helper
（helper 自身持有 slave/pipe，`scope_helper.rs:113-120`）→ reader 读到 EOF，或 PTY master
在全部 slave 关闭后的 `EIO` → `OutputLog` fsync → 持久化 Drained。等待 reader 时不持
scope gate（`apply_event` 当前持有它，main `mod.rs:576-577`），reader 也不等待 Drained。
其他读错误不是 EOF：Drained 记为 `output: Incomplete(reason)`，进程事实仍为正向。
slave fd 经 SCM_RIGHTS 逃出 scope 时 EOF 永不到达，执行保持 Running（Busy），不按超时
伪造排空。pipe 的 close-stdin/half-close 在第一个 pipe 消费者片定义。

**boot 规则（D6）。** journal 持久化宿主 `boot_id`（`/proc/sys/kernel/random/boot_id`，
内核文档化合同）。executor 启动时：宿主 `boot_id` 改变 → 所有非终态记录为
`NoChild{host_rebooted}`（宿主重启后无进程存活，是正向证据）；同一宿主上 executor
重启 → Prepared→Revoked、未 fork→NoChild、其余→Unknown（沿用 main `open`，
`mod.rs:113-115`）。终态事实按 ExecutionId 查询，与 executor boot 无关；只有 launch、
interact、registration 相关操作检查 boot。

**同宿主残留 Unknown 的出口。** helper 没有 PDEATHSIG，`stop_known` 只发 TERM
（`shutdown.rs:48`），因此 executor 重启后 helper 及其子孙可能仍在运行。显式 owner
动作为 `AttestNoChild{execution_id, ticket}`：仅接受 password-login 管理权限（auth/C1 的
会话来源规则）；executor 只有在 journal 记录的 helper 身份（pid + start_time_ticks）已不
对应任何存活进程时才写 `NoChild{attested}`，否则拒绝并要求先终止该 helper。它是
workspace 释放被 Unknown 阻塞时唯一的人工出口，不是超时。

## 7. registration、删除与恢复

**registration 生命周期（D1）。** workspace service 在资源 Materializing→Registered 时，
先持久化 transfer nonce，再经既有 `Attach`（SCM_RIGHTS）传递 root/Git 布局 FD；
executor 校验并放入 `live_objects` 后 ACK，kernel 随即关闭自己的 FD。receipt 写入
资源行，即 `RegistrationFact`。resolve 只在资源 Ready 且 registration 属于当前 executor
boot 时构造 `ExecutionTarget`；alias 比较复用 executor 的 `ObserveDirectories` /
`ObserveTransferredDirectories`（`directory_relation.rs:24-88`），kernel 不持 FD。
admission close（Canceling/Releasing）让 resolve 拒绝，并在同一事务对该 workspace 的
未结算执行 `request(Stop|Retire)`。release intent 在全部执行结算后（`wait_settled`）
调用既有 `RetireRegistration`；executor 自己检查引用它的记录均已 settled。
kernel 的观察与 retire 在同一资源 fence 下串行，因此 retire 不会因本 owner 的进行中
观察而 Busy；删除 Export 后不再有其他借用者（N4）。

**崩溃窗口。**

| 情形 | 恢复 |
|---|---|
| Attach 之后、receipt 落库之前崩溃 | 重启后 `ListRegistrations`（替代 Export/ExportAck 的新请求，返回本 boot 的 `live_objects` receipt）中出现 nonce 已知但未落库的条目 → 补记 receipt；nonce 未知 → `RetireRegistration` |
| Attach 之后、admit 之前 Track 被删除 | release intent 发现无执行，直接 `RetireRegistration` |
| kernel 重启，executor 存活 | 由 `ListRegistrations` 与资源行 receipt 精确比对（含 executor boot）重建可用性；匹配者仍为 witnessed，不匹配者 Unwitnessed。kernel 不需要 FD，也不做 path/stat 推断 |
| executor 重启（同宿主） | `live_objects` 为空：所有 registration Unwitnessed，资源进入 RecoveryRequired，按 launch-admission 的 adoption 规则重新建立；绑定旧 boot 的 intent 在 Prepare 处得到 `Refused(stale)` → `Drained(Revoked)` |
| 宿主重启 | 同上，且非终态执行为 `NoChild{host_rebooted}`，可以 retire |

executor 重启后需要重新 adoption，这是删除 kernel 侧 FD 保留的已知代价：换来的是
只有一个物理 holder，且没有反向导出的崩溃窗口。

**删除与回滚顺序。**

1. 领域事务：删除终端卡片（或 Track 等），同一事务 `Engine::request(Retire)`；Track 删除
   另写 release intent。不做任何外部副作用。
2. 回滚：外部状态不变；OutputLog 与 renderer 订阅保持原样。
3. 提交后：`drive` → stop → Wait → retire（executor：释放 native 残余、删除 OutputLog、
   写 `.retired`、移出记录；每步可重放，不用计数递减）→ `settle`。renderer 在 Retired 后退订。
   崩溃后由 `recover` 继续。
4. registration 不随终端删除；它随 workspace release 退役（上段）。

终端重启复用 `AliasHandoff`：同一事务先对旧执行 `request(Retire)`，再 admit 新执行；
executor 只在旧执行 settled 后交接 alias（`Busy(HandoffPending)` → 订阅旧执行）。

## 8. 按失败说明去除的缺陷类

| #2770 失败 | 机械原因 | 结构决定与改写后的验收 |
|---|---|---|
| `terminal_delete_retains_rows_until_actual_registration_borrower_releases` | 无 Retire 发送者；测试的借用者是 Export（`cleanup.rs:380-430`） | Export 删除后，registration 的真实引用是引用它的未 settled 执行记录。验收：终端 DELETE 返回 204，执行行保留到 executor retire 完成；同一 workspace 的 release 在该执行 settled 前不退役 registration |
| `terminal_product_deletion_releases_operation_storage_and_workspace_anchor` | 无 Cleanup/Retire 发送者；anchor 行无删除者 | 终端删除释放执行存储（native、OutputLog）；anchor 行不存在，registration 由 release intent 退役。两条验收分开 |
| `terminal_delete_rollback_retains_screen_and_retries_release_after_restart` | 提交前删除 renderer 条目 | 事务内只写 `Desired`；副作用都在提交后；重启由 `recover` 重驱 |
| `terminal_exact_retired_result_replays_after_executor_restart` | `retirement.rs:8-11` 先比较 boot 再查 `.retired` | 终态事实查询不比较 boot；合同套件断言跨 boot 精确重放 |
| `terminal_real_output_eof_precedes_durable_drain_and_final_screen` | Drained 仅凭 helper 报告；server 把 Drained 当退出 | §6 的 Drained 顺序；final cursor 之前的字节完整。屏幕由既有 renderer 按序喂入；缺完整 geometry 历史时 invalidate（main `terminal_renderer/mod.rs:832`），不承诺冷恢复精确屏幕（D5） |
| `recovery_and_ordinary_drive_share_the_effect_serialization_fence` | `apply_recovery_item` 绕过 `drive_mutex` | 每 id 一个驱动任务；terminal launch operation 改为 tx-only（在 `prepare_tx` 内 admit），operation 不再有恢复驱动，因此只有一个恢复 owner（N9）。通用 operation 缺陷另开 issue |

## 9. 不复用的部分与被取代的条款

不复用：#2770 的 migration 0169（编号也与协作分支 `0169_conversation_inputs.sql` 冲突）；
`execution_input.rs`、`execution_sources.rs`（server 持 FD、anchor 行与反向 export）；
`execution_request.rs`/`operation/admission.rs` 的授权表；`terminal_launch/registered*`
（`ExecutionInputRetention`、200ms/2s 轮询）；renderer 的 `RendererExecution::Registered`、
`stop_original_registered`、sweeper 的 `quiesce_original_registered`；supervisor 的
`RegisteredV3QueryV1`、`RegistrationUnwitnessed`、生产二进制的测试 flag 与 `.drained` 文件；
约 50 个测试文件的 `TerminalExecutionTransport::Legacy` 改动（transport 进 typed `Config`，默认
Legacy）。

保留或参考：main executor 的 helper、pidfd、registration 相等性、journal fsync、alias 与 ticket
机制；#2770 的 token 映射（移入 `session_executor`）、`auth/execution.rs` 会话复核与
`admit_new_crossing` 不变量（进入 `EffectAuthority`）、`FreshInitialGeometry`/不可信重附着的
区分（renderer 既有 invalidation）、HTTP/WS/崩溃/EOF 回归场景。删除测试 flag 不删除真实
reader 时序测试：它们作为 supervisor 包内测试驱动真实 reader/helper/fsync，fake 不替代。

本文取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的以下条款：

- 「Identity」末段“The kernel retains controlled object leases”及 identity-index 段中由 kernel
  保活 held witness 的部分：held witness 是 executor registration；kernel 只在 Attach 传输期间
  持 FD（「Current-object proof」中“kernel retains its lease until this acknowledgment”仍成立，
  ACK 后即释放）。
- 「Current-object proof」中“After kernel restart, rebuild this live index from … actual
  transferred handles”：改为查询 executor 的 `ListRegistrations`（§7）。
- 「Crash/restart and retirement boundary」表前两行（kernel 保留 lease、反向取回 FD）：改为
  §7 崩溃窗口表；“Root leases hold the object until release” 改为 registration 持有到
  `RetireRegistration`。
- 「Required kernel states」中 kernel 拥有描述符的 `SourceObjectLease`：删除；
  `RegisteredObjectWitness` 即 `RegistrationFact`。
- 「Real PreparedLaunch and frozen executor seam」：`LaunchHandle` 持 lease 改为
  `ExecutionTarget`；“each invocation revalidates”由 `EffectAuthority` 实现（§6）；
  必要改动第 3 项“recovery export”删除；第 2 项由 `ListRegistrations` 满足。
  其中 Register → durable intent → Prepare → Launch 的顺序保持不变，本文与之一致。
- 「RuntimeSafety closure」中“validate the same latch/version at actual fork/effect”：由 §6 的
  permit + ticket gate 线性化实现；首个 typed launch 的 closure 范围收窄为 §11 E2，跨 alias
  完整 closure 留在原条款，作为后续片。
- 「Smallest implementation slices and acceptance」及其后的首片安全验收：改为 §11；
  reverse-export 崩溃窗口验收删除。
- 「One task-admission cutover」：内容不变，但不在本计划内，移到 track-workspace S2a 行。

仍然有效：授权（C1）、身份表、current-object proof 的 FD 传输合同、owned
materialization/borrowed adoption、release/cancellation、ExternalDir/legacy executor 规则。

## 10. 最小 migration

合入时取 main 下一个空闲编号（当前最大 0168）；不修改 `terminals`、`operations` 或已发布
migration。

```sql
CREATE TABLE workspace_executions (
    execution_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,          -- OperationRef；无外键，不阻塞 0093 允许的无键 operation 回收
    alias        TEXT NOT NULL,
    target_kind  TEXT NOT NULL CHECK (target_kind IN ('workspace', 'external_dir')),
    workspace_id TEXT CHECK ((target_kind = 'workspace') = (workspace_id IS NOT NULL)),
    intent_json  TEXT NOT NULL CHECK (json_valid(intent_json)),
    intent_digest TEXT NOT NULL,
    desired      TEXT NOT NULL CHECK (desired IN ('run', 'stop', 'retire')),
    retired_json TEXT CHECK (retired_json IS NULL OR json_valid(retired_json))
);
CREATE UNIQUE INDEX workspace_executions_current_alias
    ON workspace_executions(alias) WHERE desired <> 'retire';
CREATE INDEX workspace_executions_occupancy
    ON workspace_executions(workspace_id) WHERE retired_json IS NULL;
-- 触发器：intent_json/intent_digest/target 不可改；desired 只能 run→stop→retire；retired_json 写一次。
```

部分唯一索引让“某 alias 的当前执行”无歧义：handoff 先 Retire 旧行再插入新行（N7）。
不对领域行加外键，领域删除不级联执行行。capability token 只经 store port 读取，不进入
事件、调试产物或 API。

## 11. 有序切片

每片独立可合入，按 [track-workspace.md](track-workspace.md) §6 的公共 gate G 验证，等级 L2。
测试名是新增合同过滤器，实现时注册，不是已验证的名字。

| # | 结果 | 验收与聚焦测试 |
|---|---|---|
| E1 | crate 与真实 executor 同片：`workspace-execution` 的类型、`next_step`、`Engine`、三个 port、最小 fake（脚本化 Fact 序列与调用日志，不复制 journal）、`session_executor`；supervisor：`Retire(ExecutionId)` 取代 `Cleanup`，`ListRegistrations` 取代 Export/ExportAck，ExecutionId gate 与 tombstone，宿主 boot_id，终态查询不比较 boot，stop 升级，变更订阅，`OutputLog` 与 `Process` 分离，Drained 在 EOF+fsync 之后，`AttestNoChild`；提升 `EXECUTION_ENVELOPE_VERSION`（daemon 可跨 kernel 升级存活）。过大时拆为 E1a（协议/journal/Retire/订阅）与 E1b（OutputLog/EOF） | `-p calm-proc-supervisor`：在真实进程内 executor 上运行 `port_contract::executor`；`drained_after_reader_eof_or_pty_eio`、`retire_exact_replay_across_boot`、`revoke_unknown_id_blocks_late_prepare`、`stop_escalates_after_grace`、`host_reboot_is_no_child`。`-p workspace-execution`：`next_step` 全表、`concurrent_drive_and_recover_effect_once`、`permit_revoked_before_launch_revokes`。改写现有 `tests/execution_registration/*` 中依赖 Export/RetireRegistration 的用例。对 EOF 前置与 boot 无关性做变异验证。行为变化：只有 supervisor 协议版本；server 在 main 上无 registered executor 生产调用者 |
| E2 | workspace service，**仅限 fresh-owned、未别名资源**：opt-in 后由 owned materialization 新建的 worktree 进入 Planned→Materializing→Registered→Ready；Attach、receipt、`ListRegistrations` 恢复、release 时 `RetireRegistration`；resolve 产出 `ExecutionTarget`；`EffectAuthority` 实现；本资源上的破坏性 effect 入口（`worktree_clean.rs:54-69` 的定时清理、recycle、sweep、area compensation、repoint、Track 删除）在资源 fence 下复验并预留维护 effect，未迁入的破坏性入口在 effect 构造处对 typed 资源拒绝；另一 selection 收敛到同一路径（别名/borrowed adoption）被拒绝，该 Track 保持 LegacyOnly。非破坏性 Legacy 启动不变（launch-admission：latch 不增加 worker 互斥） | `attach_then_crash_before_receipt_recovers`、`track_deleted_between_attach_and_admit_retires`、`kernel_restart_with_live_executor_restores_witness`、`executor_restart_makes_registration_unwitnessed`、`unmigrated_destructive_entry_refuses_typed_resource`、`alias_convergence_refused`、`release_retires_registration` |
| E3 | 新 migration + `SqliteExecutionStore` + opt-in Terminal 消费者同片：typed `Config` transport（默认 Legacy）；terminal launch operation tx-only admit；E2 的撤销路径与 cleanup 占用改读 `workspace_executions`；每个 reap 调用者在自己的事务内 `request(Retire)`：`routes/cards.rs:607`、`terminal_sweeper.rs:340/381/445`、`plugin_host/callbacks.rs:512`、`operation/driver.rs:236`、`operation/worker_cleanup.rs:63`、`operation/terminal_adapter.rs:550`；renderer 订阅 `Engine::output`；boot 调用 `Engine::recover` | `-p calm-server`：SQLite 上 `port_contract::store`，领域写入与 admit 同事务回滚；触发器与部分唯一索引拒绝改写；§8 六个场景（按改写后的验收）；HTTP 创建、WS 输入/resize/stop、冷启动恢复；旧 ignored-clean 在 SQL 抽样后暂停、typed prepare 先落库，清理复验拒绝且无过期 cleaned receipt，以及清理预留先赢；Legacy 终端测试不改 |
| E4… | 每个后续消费者一片：直接 pipe 进程（同片定义 close-stdin/half-close）、gate、provider 启动 | 每片只增加一个 `ExecutionIntent` 构造点及其真实入口测试 |

范围外的里程碑（track-workspace S2a 行）：borrowed/Legacy adoption 与跨 alias RuntimeSafety
closure、resource task-admission cutover。它们不是本 crate 的依赖。

## 12. AGENTS.md 架构五点

- **抽象边界**：动作政策只在 `workspace-execution`；物理事实与 settled 谓词只在 executor；
  registration、resolve、effect-time 授权在 workspace service/auth。适配器只映射协议。
- **重复逻辑**：server 侧多处 stop/退出推导收为 `next_step` 与一个驱动任务；Cleanup 并入
  `Retire`；Export/ExportAck 由一次查询取代；恢复与正常驱动共用驱动任务；输出只有一条读取路径；
  首版的 `Phase` 镜像与 version CAS 删除。
- **硬编码假设**：engine 不知道 Terminal；alias/OperationRef 由调用者给出；`Desired`、
  `Fact`、`ExecutionTarget`、`NewEffect` 是本层闭合集合。
- **外部合同事实**：进程与 registration 状态取自 executor 回复；宿主重启取自内核
  `boot_id`；PTY EOF/EIO 取自内核文档化行为；不从 PID、路径或 dev/inode 推导。
- **永久成本**：一张表、两个索引、三个触发器；合同套件 fake 与真实实现各跑一次；新增
  消费者、测试或文件不改任何名单。

## 13. 评审处置

| 评审项 | 处置 |
|---|---|
| codex B1 | §4：port async（`async_trait`、Send），`admit/request` 借用 SQLx 事务；E3 验收共事务回滚 |
| codex B2 | §7：registration 由 workspace service 拥有，无执行时的退役、Attach 与 admit 之间的窗口、两种重启；§9 取代 kernel 持 FD 条款 |
| codex B3 | §6：`EffectAuthority` 在 prepare/launch 前调用，撤销写 Desired，ticket gate 线性化 |
| codex B4 | §5、§8：OutputLog 只拥有字节与 cursor；保留 renderer 与 geometry invalidation（D5） |
| codex N1 | §3：feature 隔离适配器；适配器不承担政策 |
| codex N2 | §6：两个谓词，删除 “15→1”；`interact` 不接受 Cleanup/retire |
| codex N3 | §4：stdin 按 `(id, seq)`；typed Busy/Refused；tombstone 改 journal 存储（E1） |
| codex N4 | §6 Drained 顺序、EIO 与其他读错误区分；§9 保留真实 reader 时序测试；pipe 留给 E4 |
| codex N5 | §5 最小 `retired_json` 与读者；§7 retire 每步可重放、不用计数递减、不持 monitor 锁等待 |
| codex N6 | §11：E2 收窄为 fresh-owned 未别名资源并在 effect 入口拒绝；E1 与真实 executor 同片；E3 迁移与首个写者同片；E4 每消费者一片；task-admission 移出 |
| codex N7 | §9 保留与参考清单 |
| claude B1 | §7、§4：registration 移出 engine port，intent 绑定 `RegistrationFact`，stale → Revoked；`ListRegistrations` 活索引；§9 取代条款 |
| claude B2 | §6：订阅 + `Wait` + 每 id 一个驱动任务；executor 拥有 stop 升级；handoff 等待 |
| claude B3 | 同 codex B1 |
| claude B4 | §11 E2 逐项列出范围与验收，不再引用 launch-admission 第 1 片 |
| claude B5 | §6：宿主 boot_id 为 NoChild 证据；同宿主残留 Unknown 的 `AttestNoChild` |
| claude N1 | §2 更正归属；E1 改写已合入代码与测试、提升 envelope 版本、可拆 E1a/E1b |
| claude N2 | §6：两个谓词；变异验证断言 fake 调用日志 |
| claude N3 | §6 Drained 顺序：先 reap helper，不持 scope gate，逃逸 slave 保持 Busy |
| claude N4 | §8 第 1 行与 §7：真实引用者为未 settled 执行记录与本 owner 的观察 |
| claude N5 | §4、§10：删除 version 与 CAS；`retired_json` 为最小标记并点名读者；保留期无界已说明 |
| claude N6 | §4：ExecutionId gate 与 Prepare 共用，tombstone 绑定 `ticket_id` |
| claude N7 | §10：`workspace_id` 列与占用索引；alias 部分唯一索引 |
| claude N8 | §4：只保留 `read_output`；删除 `Phase`，Retired 只由 `Fact` 与 `retired_json` 表达 |
| claude N9 | §8 最后一行：launch operation tx-only |
| claude N10 | §11：E1 合并旧 1+2，E3 合并旧 3+5 |
| claude N11 | §1、§11：task-admission 移到 track-workspace S2a 行 |
| claude N12 | §3：核心也依赖 calm-session 类型，适配器 feature 隔离 |
| claude N13 | §11 E3：逐一列出 reap 调用者 |
| claude N14 | §10：`operation_id` 不设外键；`apply_recovery_item` 缺陷另开 issue（§2 R4） |
