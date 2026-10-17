# Workspace execution（S2a 实施合同）

状态：S2a「registered workspace execution」的结构性重设计，替代草稿 PR #2770
（`codex/2666-terminal-vertical`，用户已否决）。只是设计，不声称已实现；
实现等级 L2，须由 Planner 安排两路独立评审。父合同为
[track-workspace.md](track-workspace.md)；本文取代
[workspace-launch-admission.md](workspace-launch-admission.md) 的部分章节（见 §8），
其余章节继续有效。英文类型是合同草案，不代表已有 API。

## 1. Outcome 与范围

S2a 在 Track workspace 中通过 executor journal 启动 Terminal 及其他进程：

1. 一个独立 crate `workspace-execution` 拥有唯一的执行状态机
   `Intent → Prepared → Running → Drained → Retired`，以 port 访问存储与执行器，
   不依赖 calm-server 即可单元测试。
2. 每个事实（intent、source hold、进程/journal、输出保留、删除）只有一个 owner、
   一个入口；正常驱动与崩溃恢复走同一个 `Engine::drive`。
3. calm-server 只剩 HTTP/WS 适配、授权、DB 接线；calm-proc-supervisor 是进程、
   source hold 与输出的唯一物理 owner。
4. 一个新 migration、一张表，不建镜像表；已发布 migration 字节冻结。
5. 未配置 registered executor 时 Legacy 默认行为不变；首个消费者是 opt-in Terminal。

范围外：S2b 同路径 COW view（[workspace-view-backend.md](workspace-view-backend.md)）、
S3 交付/Revision（[workspace-delivery-protocol.md](workspace-delivery-protocol.md)）、
S4 provider 执行环境、S5 产品设置。C4 Area 共同执行授权仍由协作授权 owner 交付。

## 2. 诊断：#2770 为什么只能不断打补丁

依据：PR #2770 正文、`git diff origin/main...origin/codex/2666-terminal-vertical`
（merge-base `ab21d4a46`，operation 写入部分已由 #2756 合入 main）。这里有五个结构根因，
后续补丁都是它们的症状。

**R1 没有执行生命周期的 owner。** 同一次执行的事实分散在五处：
`operations.execution_request_json` 与 `operation_execution_grants`（server）、
`workspace_execution_sources`（server）、executor journal（supervisor）、renderer
内存条目（server）、`terminal_sweeper`（server）。“执行是否结束/可否释放”有
15 处独立判断：supervisor 9 处（`durably_settled`、`permits_handoff`、
`observed_state`、`apply_event`、boot 改写、Cleanup/Retire 守卫、Attach 门、Control
终态检查），server 6 处（`stop_original`、`attach_reader` 把 Drained 当退出、
`ensure_entry` 接受态、registered `stop_child`、`quiesce_original_registered`、
`stop_original_registered`）。每修一个失败，都要在另一处补一个判断。

**R2 资源生命期纠缠在错误的边界上。** supervisor 的 `Process` 同时拥有
child、PTY master、pidfd 与输出 ring（`process.rs:20-27`），所以 `Cleanup` 释放
native 句柄时连输出一起删除。source 退役是另一条路径 `RetireRegistration`，
单独检查 settlement 与借用者。server 生产代码**从未发送** `Cleanup` 或
`RetireRegistration`，deletion 只做 Revoke 和删 renderer 条目。删除顺序也错：
提交 DB 事务之前就移除了 renderer 条目（`stop_original_registered`），
事务回滚时屏幕已经丢失。

**R3 source 所有权重复。** server 用 `ExecutionInputRetention::Held` 持有 FD，
直到最后一个 `Arc` 释放；冷恢复时改用 `OriginalControlOnly`；executor 同时持有
registration FD；workspace 级 anchor 行（`workspace_execution_sources`）是第三份
意图。三方各自判断 boot/代次，于是需要 Export/ExportAck 反向传 FD、
`RegistrationUnwitnessed`，还需要 boot 比对。

**R4 有两条驱动路径。** `OperationRuntime::drive()` 持 `drive_mutex`，而 boot 的
`apply_recovery_item`（`driver.rs:1184`）直接调用 `drive_one`，不持该锁。终端还有
第三、四条 stop 路径（renderer `stop_child`、sweeper quiesce），各自调用 executor。

**R5 终态由错误的 owner 宣布。** supervisor 的 `apply_event` 仅凭 helper 报告
就写 `Drained`（`mod.rs:634-653`），输出 reader 线程没有 EOF 信号；server 的
`attach_reader` 再把 `Drained` 转成退出，并用当时已读到的输出持久化。
“排空”和“输出读完”跨两个进程竞态。

另有一个呈现问题：全部行为都写在 calm-server 的 terminal 模块中，只能用
17 个 HTTP 级集成测试覆盖；加入 `TerminalExecutionTransport` 字段又迫使约
50 个无关测试文件改一行。

## 3. Crate 边界与依赖方向

```
workspace            (已有：身份类型、HeldSourceObjects、物化/回收机制)
   ▲        ▲
calm-session   workspace-execution  (新增：状态机 + ports + session 适配器)
   ▲              ▲   │ (session_executor 模块依赖 calm-session)
calm-proc-supervisor  │
   ▲  (dev-dep: 运行 workspace-execution 的 executor 合同套件)
calm-server ──────────┘  (实现 store/target ports，HTTP/WS，授权，DB 接线)
```

新 crate 命名为 `workspace-execution`（按职责命名，不加 calm- 前缀）。不并入
`workspace` 的原因：`workspace` 是 supervisor 也依赖的文件系统机制库，
把 kernel 端编排放进去会让 executor 依赖 kernel 政策。依赖只有 `workspace`、
`calm-session` 与 async 原语；不依赖 calm-truth、calm-server、sqlx、axum。

| 层 | 拥有 | 不拥有 |
|---|---|---|
| `workspace-execution` | `ExecutionIntent` 类型与摘要；`Desired`；状态推导；`Engine::{admit,request,drive,recover,control}`；每 ExecutionId 的串行化 fence；`Executor`/`ExecutionStore` port；`session_executor` 适配器；`test-support` 内存 fake 与 port 合同套件 | 授权、路径选择、SQL、HTTP、渲染 |
| `calm-proc-supervisor` | journal（Prepared…Drained/NoChild/Unknown）、registration 与借用计数、native child/PTY/pidfd、`OutputLog`、`Retire(ExecutionId)` | 领域删除、授权 |
| `workspace`（已有） | 打开/验证 `HeldSourceObjects`，物化/回收机制 | 执行状态 |
| calm-server | 路由/WS 适配；auth/C1 授权结论；workspace service 的 resolve 与 RuntimeSafety（沿用 launch-admission）；`SqliteExecutionStore`；renderer 作为输出订阅者 | 执行状态推导、释放判断 |

calm-server 实现两个 port；crate 用 `test-support` fake 独立测试。同一套
`port_contract` 测试同时跑 fake 与真实实现（supervisor 用进程内 Executor，
server 用 SQLite store），fake 无法偏离真实语义。

## 4. 公共 API 草案

```rust
// crates/workspace-execution/src/lib.rs
pub use workspace::execution::{ExecutionId, ExecutorBootId, LaunchTicketId,
    ObjectRegistrationId, CapabilityToken};

/// 不可变；摘要参与 executor Prepare 的相等性检查。由 server 在授权与 resolve 后构造。
pub struct ExecutionIntent {
    pub id: ExecutionId,
    pub alias: ProcessAlias,              // 例如 "term:<id>"；只作查找键
    pub owner: OperationRef,              // 接受该请求的既有 operation
    pub target: ExecutionTarget,
    pub authority: AuthorityConclusion,   // auth owner 的密封结论，crate 不解释
    pub program: ProgramPlan,             // argv/env allowlist/io=Pty|Pipe/相对 cwd
    pub ticket: LaunchTicketId,
    pub tokens: ExecutionControlTokens,   // observe/interactive/lifecycle
    pub handoff: Option<AliasHandoff>,
}
pub enum ExecutionTarget {
    Workspace { workspace: WorkspaceRef, registration: RegistrationKey },
    ExternalDir { registration: RegistrationKey },
}
pub enum Desired { Run, Stop, Retire }    // 单调：Run < Stop < Retire

pub enum Phase {
    Intent,
    Prepared,
    Running(ExecutionReceipt),
    Drained(DrainReceipt),                // 含 Revoked / NoChild / 进程排空
    Retired(RetiredReceipt),
}
pub enum DriveOutcome { Settled(Phase), Busy(Unknown) }

pub trait ExecutionStore {
    type Tx<'t>;
    fn insert(&self, tx: &mut Self::Tx<'_>, intent: &ExecutionIntent) -> Result<(), StoreError>;
    fn request(&self, tx: &mut Self::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError>; // intent+desired+version+retired
    async fn settle_retired(&self, id: &ExecutionId, version: u64, r: &RetiredReceipt)
        -> Result<Cas, StoreError>;
    async fn unsettled(&self) -> Result<Vec<ExecutionId>, StoreError>;
}

/// 每个调用按 ExecutionId 幂等。终态事实与 boot 无关；boot 只约束活句柄。
pub trait Executor {
    async fn ensure_registration(&self, key: &RegistrationKey, objects: Option<HeldSourceObjects>)
        -> Result<RegistrationFact, ExecError>;
    async fn prepare(&self, intent: &ExecutionIntent) -> Result<Fact, ExecError>;
    async fn launch(&self, id: &ExecutionId, ticket: &LaunchTicketId) -> Result<Fact, ExecError>;
    async fn revoke(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn stop(&self, id: &ExecutionId, cap: &CapabilityToken) -> Result<Fact, ExecError>;
    async fn inspect(&self, id: &ExecutionId) -> Result<Fact, ExecError>;
    async fn retire(&self, id: &ExecutionId, drained: &DrainReceipt) -> Result<RetiredReceipt, ExecError>;
    async fn control(&self, cap: ExecutionCapability, action: ProcessAction) -> Result<ControlReply, ExecError>;
    async fn read_output(&self, id: &ExecutionId, cursor: OutputCursor) -> Result<OutputChunk, ExecError>;
}
pub enum Fact { Absent, Prepared, Running(ExecutionReceipt), Drained(DrainReceipt),
                Unknown(Unknown), Retired(RetiredReceipt) }

pub struct Engine<S: ExecutionStore, X: Executor> { /* store, executor, fences */ }
impl<S: ExecutionStore, X: Executor> Engine<S, X> {
    pub fn admit(&self, tx: &mut S::Tx<'_>, intent: ExecutionIntent) -> Result<ExecutionId, AdmitError>;
    pub fn request(&self, tx: &mut S::Tx<'_>, id: &ExecutionId, d: Desired) -> Result<(), StoreError>;
    pub async fn drive(&self, id: &ExecutionId) -> Result<DriveOutcome, DriveError>;
    pub async fn recover(&self) -> RecoverReport;      // = 对 unsettled() 逐个 drive
    pub async fn control(&self, id: &ExecutionId, action: ProcessAction) -> Result<ControlReply, ControlError>;
    pub async fn output(&self, id: &ExecutionId, cursor: OutputCursor) -> Result<OutputChunk, ControlError>;
}
```

`admit`/`request` 只写 DB，可在调用者自己的事务内执行，无外部副作用；`drive`
在提交之后执行所有外部副作用。crate 不提供按 cwd 构造 intent 的入口，
`ExecutionTarget` 只能由 server 在 resolve 成功后构造。

## 5. 唯一 owner 与唯一入口

| 事实 | 唯一 owner | 唯一入口 | 存储 |
|---|---|---|---|
| Intent（目标、授权结论、程序、ticket、token） | `workspace-execution` | `Engine::admit`，在接受该请求的 operation 事务内 | `workspace_executions.intent_json`，写一次 |
| Desired（Run/Stop/Retire） | `workspace-execution` | `Engine::request`，在领域事务内（stop 路由、卡片/Track 删除、终端重启） | `workspace_executions.desired`，单调 |
| Source hold（registration FD 与借用者） | executor registry | `Executor::ensure_registration`；借用随 `retire` 归还；anchor pin 随 workspace release intent 解除 | executor `.registration` |
| 进程/journal（Prepared…Drained/Unknown） | executor journal | engine 的 `drive` 发出 prepare/launch/revoke/stop；Drained 只由 executor monitor 写 | executor journal |
| 输出保留 | executor `OutputLog`（按 ExecutionId，独立于 `Process`） | reader 线程追加；只由 `retire` 释放 | executor journal 目录 |
| 删除/退役 | 领域 owner 写 `Desired::Retire`；engine 执行 | `Engine::drive` → `Executor::retire` → `settle_retired` | `workspace_executions.retired_json` |
| 授权与 RuntimeSafety | auth/C1 与 workspace service（沿用 launch-admission） | server resolve，在构造 intent 之前 | 既有资源/绑定行 |

kernel 不保存 Prepared/Running/Drained 的副本，每次需要时 `inspect` executor；
`retired_json` 是该执行的最终结果（供已完成键重放），不是阶段镜像。
renderer 是 `Engine::output` 的订阅者，不再拥有屏幕事实。

## 6. 一个状态机

```
 admit(tx)           drive: prepare        drive: launch (desired=Run)
 Intent ───────────────────▶ Prepared ───────────────────▶ Running
   │  desired≠Run: revoke      │ desired≠Run: revoke          │ executor monitor：
   ▼  (写 Revoked tombstone)   ▼                              ▼ scope 排空 ∧ reader EOF ∧ OutputLog fsync
 Retired ◀── drive: retire ── Drained(Revoked/NoChild/进程排空) ◀────────────────┘
          (仅 desired=Retire)
```

| 转换 | 唯一执行者 | 证据 / 规则 |
|---|---|---|
| ∅→Intent | `Engine::admit` | 与 operation/领域行同一事务；intent 摘要固定 |
| Intent→Prepared | `drive` → `Executor::prepare` | 先 `ensure_registration`；Prepare 按 ExecutionId 幂等且摘要必须相等 |
| Prepared→Running | `drive` → `Executor::launch` | 仅当 desired=Run；executor 单 ticket 核销，内部 Forking/Spawned 不外露 |
| Intent/Prepared→Drained | `drive` → `Executor::revoke` | desired≥Stop；对未知 ExecutionId 也写 Revoked tombstone，迟到的 Prepare 被拒绝；Revoked 是正向终态证据 |
| Running→Drained | executor monitor | engine 只能请求 `stop`；Drained = helper scope 排空 **且** reader 读到 EOF **且** OutputLog fsync，三者在 supervisor 同一 owner 内顺序完成 |
| Drained→Retired | `drive` → `Executor::retire` → `settle_retired` | 仅当 desired=Retire；retire 一次性释放 native 残余、归还 registration 借用、删除 OutputLog，写 `.retired`；store CAS 最后执行 |
| 任意→Unknown | 不是状态 | `Fact::Unknown` 让 `drive` 返回 `Busy`，保持占用；不推断 NoChild/Drained |

`drive(id)` 的过程：取该 ExecutionId 的进程内 fence → `store.load` → `executor.inspect`
→ 由 `(desired, fact)` 纯函数 `next_step` 计算一步 → 执行 → 循环到稳定或 Busy。
`next_step` 是全仓库唯一的“是否结束/可否释放”判断；executor 内部只保留
自己的 journal 守卫，不另设终态谓词。

**崩溃恢复。** 没有恢复专用函数。`recover()` 对 `store.unsettled()` 逐个调用
`drive`。每一步都由 `inspect` 的当前事实重新推导，因此任何位置崩溃后，
重放都从真实事实继续：prepare/launch/revoke/retire 在 executor 中按
ExecutionId 幂等；`settle_retired` 是版本 CAS。fence 由 engine 自己持有，不依赖调用者
是否持 `drive_mutex`。跨进程只有 kernel 一个 engine，DB 版本 CAS 防止旧驱动者覆盖。

**boot 规则。** executor 重启后：Prepared→Revoked、未 fork→NoChild、其余→Unknown
（沿用现有 `open`）；registration 成为 Unwitnessed，不重开路径。终态事实
（Revoked/NoChild/Drained/Retired）按 ExecutionId 查询，与 boot 无关；
只有活句柄操作（launch、control、ensure_registration 复用）检查 boot。
Unwitnessed 的 registration 只能经 workspace owner 按 launch-admission 的
adoption 规则重新 attach，engine 不自行补造。

**删除与回滚的顺序。**

1. 领域事务：删除领域行（终端卡片/Track），同一事务 `Engine::request(Retire)`；
   Track 删除另外按 launch-admission 写 workspace release intent。不做任何外部副作用。
2. 事务回滚：没有任何外部状态改变；OutputLog 与 renderer 订阅保持原样。
3. 事务提交后：`drive` stop → 等 Drained → retire → `settle_retired`；随后 renderer
   因 Retired 退订。崩溃后由 `recover` 继续。
4. source：最后一个借用归还，且 workspace anchor pin 已由 release intent 解除，
   registration 才在 executor 内退役；正在传输中（未 ACK）的借用阻止退役，
   但不阻止领域删除（见 §7 第 1 行）。

终端重启复用 `AliasHandoff`（identity-launch 合同）：在同一事务内 admit 新执行并对旧执行
`request(Retire)`，executor 只在旧执行正向排空后交接 alias。

## 7. 按失败说明去除的缺陷类

| #2770 失败 | 机械原因 | 去除该缺陷类的结构决定 |
|---|---|---|
| `terminal_delete_retains_rows_until_actual_registration_borrower_releases` | server 从不发 Retire，借用者从未参与；DELETE 直接 204 | 退役只有 `drive→retire` 一个入口，借用计数在 executor registry 内；领域行删除与执行行分离（执行行不随卡片级联）。验收改为：DELETE 提交后执行行与 registration 一直保留，直到借用 ACK 后 retire 完成；不再用 409 暴露内部传输状态 |
| `terminal_product_deletion_releases_operation_storage_and_workspace_anchor` | 无 Cleanup/Retire 发送者；anchor 行无删除者 | `Retire` 是 executor 中的单一转换，同时释放 native、借用和输出；anchor 不再是 server 表，而是 executor registry 的一个 pin，由 workspace release intent 解除 |
| `terminal_delete_rollback_retains_screen_and_retries_release_after_restart` | 提交前已删除 renderer 条目 | 事务内只写 `Desired`；所有外部副作用都在提交后的 `drive` 中；屏幕事实属于 executor OutputLog，renderer 只是订阅者；重启由 `recover` 重驱 |
| `terminal_exact_retired_result_replays_after_executor_restart` | `retirement.rs:8-11` 先比较 boot，再查已持久化的 `.retired` | 终态事实按 ExecutionId 查询，与 boot 无关；boot 只约束活句柄。合同套件统一断言 retire 的精确重放 |
| `terminal_real_output_eof_precedes_durable_drain_and_final_screen` | `apply_event` 仅凭 helper 报告写 Drained；server 把 Drained 当退出 | Drained 由 supervisor 一个 owner 宣布，以 reader EOF + OutputLog fsync 为前置；最终屏幕从完整 OutputLog 计算，server 不再推导退出 |
| `recovery_and_ordinary_drive_share_the_effect_serialization_fence` | `apply_recovery_item` 绕过 `drive_mutex` | 执行副作用的 fence 属于 engine，正常驱动与恢复都走 `drive`。`apply_recovery_item` 对其他 adapter 仍不持锁，是既有缺陷，另开 issue，不并入 S2a |
| 重复 source 所有权 | server `Held`/`OriginalControlOnly` + anchor 行 + executor FD | kernel 只在 Attach 传输期间持 FD，ACK 后立即释放；executor 是唯一 holder，删除 Export/ExportAck 反向传输 |

## 8. 不复用 / 删除的 #2770 部分，以及被取代的文档章节

不复用（不 cherry-pick）：

- migration `0169_terminal_registered_execution.sql`：`execution_admitted`、
  `execution_request_json`、`operation_execution_grants`、`terminals.launch_operation_id`、
  `workspace_execution_sources` 及其触发器。由 §9 的一张表取代；该编号还与协作分支冲突。
- `workspace_service/execution_input.rs`、`execution_sources.rs`（server 持 FD 与 anchor 意图）。
- `operation/execution_request.rs`、`operation/admission.rs`、`terminal_adapter/admission.rs`
  的授权表写入：intent 并入 crate 的 `ExecutionIntent`；授权结论由 auth owner 产生后放进 intent。
- `terminal_launch/registered.rs`、`registered/recovery.rs`（`RegisteredExecution`、
  `ExecutionInputRetention`、`stop_original` 的 200ms/2s 轮询）。
- renderer 的 `RendererExecution::Registered`、`stop_original_registered`，以及
  `attach_reader` 把 Drained 当退出的逻辑；sweeper 的 `quiesce_original_registered`。
- supervisor 的 `RegisteredV3QueryV1` 模式、`RegistrationUnwitnessed` 回复与测试钩子
  （`--test-output-reader-release`、`.drained` 文件）：`inspect` 进入正式协议版本；
  可测性由 fake 与合同套件提供，生产二进制不加隐藏 flag。
- 约 50 个测试文件中增加 `TerminalExecutionTransport::Legacy` 的改动：transport 进入
  typed `Config`，默认 Legacy，由 AppState 构造器取默认值。

可参考但需重新评审：`terminal_executor_client.rs` 的 token 映射（移入
`session_executor`）、`auth/execution.rs` 的会话复核（留在 server 作授权适配）、
`legacy_start.rs` 的机械拆分、HTTP/WS 测试场景（作为 §10 第 5 片的验收）。
#2756 已合入的 operation 写入直接使用 main 上的版本。

本文取代 [workspace-launch-admission.md](workspace-launch-admission.md) 的以下部分：

- “Real PreparedLaunch and frozen executor seam”：launch intent 的存储位置、
  持有者与顺序改为 §4–§6；第 3 项“recovery export / 反向传 FD”删除，
  kernel 不在 ACK 后保留 `SourceObjectLease`。
- “Crash/restart and retirement boundary” 表的前两行（kernel 保留 lease、
  kernel 重启后从 executor 反向取回 FD）改为 §6 的 boot 规则。
- “Smallest implementation slices and acceptance” 改为 §10。

仍然有效：授权（C1）、身份表、current-object proof、owned materialization/borrowed
adoption、RuntimeSafety、task-admission cutover、release/cancellation、ExternalDir/legacy。

## 9. 最小 migration

在合入时取 main 上下一个空闲编号（当前 main 最大为 0168；不得复用协作分支的编号）。
不修改 `terminals`、`operations` 或已发布的任何 migration。

```sql
CREATE TABLE workspace_executions (
    execution_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE RESTRICT,
    alias        TEXT NOT NULL,
    intent_json  TEXT NOT NULL CHECK (json_valid(intent_json)),
    intent_digest TEXT NOT NULL,
    desired      TEXT NOT NULL CHECK (desired IN ('run', 'stop', 'retire')),
    version      INTEGER NOT NULL CHECK (version > 0),
    retired_json TEXT CHECK (retired_json IS NULL OR json_valid(retired_json))
);
CREATE INDEX workspace_executions_alias ON workspace_executions(alias);
CREATE INDEX workspace_executions_unsettled ON workspace_executions(execution_id)
    WHERE retired_json IS NULL;
-- 触发器：intent_json/intent_digest 不可改；desired 只能 run→stop→retire；
-- retired_json 写一次。
```

不对领域行加外键，领域删除不会级联执行行。capability token 存于 intent，
但只经 store port 读取，不进入事件、调试产物或 API。没有 grants 表、
source 表或阶段列。

## 10. 有序切片

每片独立可合入，按 [track-workspace.md](track-workspace.md) §6 的公共 gate G 验证，
等级 L2。测试名是新增合同过滤器，实现时注册，不是已验证的名字。

| # | 结果 | 验收与聚焦测试 |
|---|---|---|
| 1 | `crates/workspace-execution`：类型、`next_step`、`Engine`、两个 port、`test-support` 内存 fake（带 journal 语义、boot 重启、借用、OutputLog、故障注入）与 `port_contract` 套件。没有生产调用者；它就是合同 | `cargo nextest run -p workspace-execution`：`drive_recovers_from_every_crash_point`、`concurrent_drive_and_recover_effect_once`、`retire_waits_for_inflight_borrow`、`retired_replays_after_executor_restart`、`rollback_before_commit_has_no_effect`、`drained_requires_output_eof`、`unknown_is_busy_never_released`、`desired_is_monotonic`。对 `next_step` 的释放分支做变异验证 |
| 2 | supervisor 对齐：`Retire(ExecutionId)` 取代对外的 `Cleanup` 与 `RetireRegistration`；registration 借用计数与 anchor pin；`OutputLog` 与 `Process` 分离；Drained 以 reader EOF 与 fsync 为前置；终态查询不比较 boot；`inspect` 进入协议版本；删除 Export/ExportAck；新增 `session_executor` 适配器 | `-p calm-proc-supervisor`：在进程内 executor 上运行 `workspace_execution::port_contract::executor`；`drained_after_reader_eof`、`retire_exact_replay_across_boot`、`registration_retires_at_last_borrow`。对 EOF 前置和 boot 无关性做变异验证 |
| 3 | calm-server：新 migration 与 `SqliteExecutionStore`（DB 接线）；`Config` 增加 typed executor transport，默认 Legacy | `-p calm-server execution_store`：在 SQLite 上运行 `port_contract::store`；触发器拒绝改写；Legacy 默认配置启动路径不变 |
| 4 | workspace service 的 resolve 与 RuntimeSafety（launch-admission 仍有效部分），产出 `ExecutionTarget`；Attach 后释放 kernel FD；release intent 解除 anchor pin | launch-admission 第 1 片的安全验收（old clean vs 新 prepare 竞态等），以及 `anchor_released_by_release_intent` |
| 5 | opt-in Terminal 消费者：terminal operation adapter 调用 admit/drive；stop/删除/重启路由调用 `request`；renderer 订阅 `Engine::output`；boot 调用 `Engine::recover` | `-p calm-server terminal_registered`：#2770 的 4 个场景（第 1 个按 §7 改验收）、EOF/final screen、HTTP 创建、WS 输入/resize/stop、冷启动恢复；Legacy 终端测试不改 |
| 6 | 其余调用者（直接 pipe 进程、gate、provider 启动）及 task-admission 切换 | 按 launch-admission 第 2、3 片；每个调用者只增加一个 `ExecutionIntent` 构造点 |

切片 1–3 不改变任何生产行为；第 5 片起才有 opt-in 用户可见能力。

## 11. AGENTS.md 架构五点

- **抽象边界**：状态推导与释放判断只在 `workspace-execution`；物理事实（FD、进程、
  输出）只在 supervisor；授权、选择、RuntimeSafety 留在 server 的 auth/workspace
  service。server adapter 只调用 `admit/request/drive/control/output`，不解释 journal。
- **重复逻辑**：15 处终态判断收为一个 `next_step`；Cleanup + RetireRegistration 合为一个
  `Retire`；server、anchor 表与 executor 三份 source 持有收为 executor 一份；
  恢复与正常驱动共用 `drive`。删除 Export/ExportAck、QueryV1 模式与测试钩子 flag。
- **硬编码假设**：engine 不知道 Terminal；`ProcessAlias` 与 `OperationRef` 由调用者给出。
  `Desired`、`Phase`、`ExecutionTarget` 是本层拥有的闭合集合；新调用者只构造 intent，
  不修改 engine。
- **外部合同事实**：进程、PTY、registration 状态只取自 executor 的回复与 journal，
  不从 PID、路径或 dev/inode 推导；Git 布局取自 `workspace` 已有的 Git 报告机制。
- **永久成本**：一张表与三个触发器；两个 port 合同套件，各在本包测试中运行一次
  （fake 与真实实现各一次，即每个 CI workflow 两次）。新增调用者、测试或文件
  不需要修改任何名单；新增 `ProcessAction` 只改 executor 分发与合同套件。
  测试可配置性只存在于 fake，生产二进制不增加隐藏 flag。
