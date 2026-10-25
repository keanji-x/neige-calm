# Provider 无关的多 Agent 后端池与负载均衡设计

状态：设计稿，只改文档，不改代码。核实基线为 `main` `4da52ae8b`。
简写：`server/` = `crates/calm-server/src/`，`truth/` = `crates/calm-truth/src/`，`provider/` = `crates/provider/src/`，
`types/` = `crates/calm-types/src/`。文中源码事实都带 `file:line`；没有源码证据、要在固定版本原生 CLI 上实测的事实标为 **〔待实测〕**。

本设计受两份进行中的架构设计约束，冲突时以它们为准，并在 §9 写明冲突：

- [Agent Runtime 领域与接口设计](architecture/agent-runtime-design.md)（下称 ART）：§3 模块归属、§5 工厂与能力、§9 配置与凭据 owner、
  §10 执行资源形态、§15 持久与恢复、§16 M1–M4 迁移。
- [Task worker 执行模型](architecture/task-worker-execution.md)（下称 TWE）：W 系列，WA1a 的调用者准入/释放端口。

设计评审等级：**L2**。理由：涉及凭据隔离、守护进程生命周期、数据库迁移与原生 ID 绑定。

## 0. 结论

1. **不新建 Provider trait。** 池不是第二个 AgentRuntime，也不复制 `PlannerFactory`、`WorkerProviderRegistry` 或
   `ProviderAvailabilityCache`。池是宿主侧 owner，只做三件事：保存类型化的后端登记、汇总健康与容量信号、为新原生会话选出一个
   `BackendId`。原生会话怎样打开，仍由既有协议臂完成（`server/harness/backend/factory.rs:139-163` 按 `AgentProtocol` 分派，
   这是 `types/runtime.rs:41-49` 声明的闭集）。
2. **挂接点在 ART 的 NativeBinding。** ART §4 把 NativeBinding 定义为“provider、宿主配置/存储范围、原生 session/thread ID 的绑定”。
   本设计中的 `BackendId` 就是其中的“宿主配置/存储范围”。工厂在 `open` 时用行上已持久的 `BackendId` 解析出具体实例；
   选择只发生在原生 ID 第一次建立之前（Unbound → Observed，ART §5.1）。
3. **粘性是硬约束，不是一种策略。** 三种协议的原生会话都绑在凭据目录上：Codex 线程存放在其 `CODEX_HOME` 中；Claude 会话记录在
   `CLAUDE_CONFIG_DIR` 下〔待实测：是否只在该目录〕；ACP 会话由注册摘要钉住，摘要包含 env
   （`server/acp_planner/config.rs:47-82`，不一致时 `truth/db/sqlite/acp_submission.rs:102` 返回 `Changed`）。
   所以已绑定的会话不跨后端迁移。限流时它等待冷却，或由用户走既有的 reset 路径开新会话。负载均衡只作用于新会话。
4. **thread→backend 绑定必须落库**，按 ART §16 单独作为一份 L2 切片（P3），不并入 R1 或 M2。
5. **容量/限流部分复用 `provider_execution::Admission`。** 对 OwnedProcess（Claude/ACP 每轮一个 execution），
   把“绑定后端可用”组合进 `Admission::authorize_tx`（`server/provider_execution/admission.rs:5-11`）。Codex 共享 daemon 的轮次
   不经过 Engine，这个端口覆盖不到。Codex 的在途计数是 daemon 内的内存事实，由池读取，不新建持久计数（§4.6）。
6. 实现放进新模块 `server/agent_backend_pool/`。对现有文件只留少量窄挂接点（§10）。`shared_codex_appserver.rs` 只改构造入口，
   另有 6 处单行替换（daemon 行读写改为按后端键）。
7. 共 4 个切片：P1 配置与池模型（L1）；P2 多实例 Codex supervisor（L2）；P3 thread→backend 绑定（独立 L2）；
   P4 限额感知调度与按后端可用性（L1）。Claude/ACP 多凭据档是后续候选，不在本计划内（§11）。

## 1. 目标、非目标与约束

**目标**

- 同时运行多个 Codex 后端（多账号、多 `CODEX_HOME`，每个一个 app-server 守护进程），新会话在它们之间负载均衡。
- 遇到 429、并发上限、额度耗尽或认证失效时，把该后端从新会话的候选中摘除，满足条件后恢复。
- 抽象能以低成本扩展到多 Claude 凭据档、多 OpenCode 注册、多 Grok 注册。池和调度器不按 provider 身份分支。

**非目标**

- 不迁移已绑定会话的原生上下文。从另一 home 的私有目录复制会话，违反 AGENTS.md 的“外部契约事实”规则。
- 不做登录、登出或凭据写入。ART §9：登录与凭据管理是独立的宿主管理动作，不能由会话 open 隐式写入。
- 不改变 Codex/Claude 的未确认投递与重试政策（ART §13、§16.3 的 H2 范围）。

**不变的约束**：AGENTS.md 的类型化配置（不加隐式环境变量）、子进程环境用显式白名单、已发布迁移字节冻结，
以及架构五点（§10.3 逐项回答）。

## 2. 现状调用链

### 2.1 守护进程的构造、启动与健康

| 步骤 | 位置 | 说明 |
|---|---|---|
| home 路径 | `server/state_clients.rs:125` | `SharedCodexHome::new(data_dir.join("codex-home"))`，是 `CodexClient` 的字段 |
| home 播种 | `server/state.rs:1425` → `provider/codex/shared/home.rs:48-90` | 首次启动从宿主 `$CODEX_HOME` 或 `~/.codex` 拷入 `auth.json` 与净化后的 `config.toml` |
| MCP 净化与写入 | `server/state.rs:1433`、`:1517` → `server/shared_codex_home.rs:21-37` | 每个 home 一份 `[mcp_servers.<key>]` |
| 构造 | `server/state.rs:1557` → `server/shared_codex_appserver.rs:758-827` | socket、认证检查点路径、日志目录都从 `cfg` 派生 |
| 存放 | `server/state.rs:93`、`:207`、`:240`、`:408` | `Arc<SharedCodexAppServer>`，分别在 RouteState、CodexShellState、BootState、AppState |
| 启动/接管 | `server/lib.rs:676` → `shared_codex_appserver.rs:842-858` | `boot` = `start_or_takeover` + `ensure_respawn_for_current_settings` |
| 进程环境 | `shared_codex_appserver.rs:1676` → `provider/codex/shared/environment.rs:55-70` | `env_clear` 后按白名单传入，并设置 `CODEX_HOME` |
| 自愈 | `shared_codex_appserver.rs:861`、`:1916`、`:2793`、`:2924-2994` | `ensure_running`、`transition_replace`、`restart_after_crash`、`heal_loop` |
| 就绪信号 | `shared_codex_appserver.rs:885`、`:1503-1511`、`:1538` | watch 通道、`is_running*`、`status_snapshot` |
| 持久运行记录 | `crates/calm-truth/migrations/0026_shared_codex_daemon.sql:1-13`、`0027_shared_daemon_env_signature.sql` | `CHECK (id = 1)` 的单行表 |

### 2.2 登录探测与可用性

- `server/agent_providers.rs:339-357` 的 `check_codex`：先要求 `is_running_per_readiness`，再调用 `account/read`，判断 `logged_in`。
  未登录时提示用户 “run `codex login` with CODEX_HOME=…”（`:349`）。
- `ProviderAvailabilityCache`（`server/agent_providers.rs:189-207`）：codex 一个 slot，claude 一个 slot，ACP 每个 provider 一个 slot，TTL 30 s。
  `GET /api/agent-providers`、track 创建和启动检查（`:361`）都读这里。
- 认证失效观察：`CodexAuthentication` 在构造时绑定 `data_dir/state/codex-authentication.json` 与 home（`shared_codex_appserver.rs:765-769`），
  每秒轮询一次（`:1429-1449`）。`require_start_authentication`（`:1452`）在认证失效时拦住发行。

### 2.3 线程的 mint、resume 与 release

- mint：`thread_start_for_card`、`thread_start_mint_for_card`（`shared_codex_appserver.rs:920-1040`）在
  `kernel_thread_start_serial` 内发 `thread/start`，再写入 `thread_cache`（thread→card）。
- Planner 的准备入口：`server/harness/backend/factory/codex.rs:43` 的 `prepare_codex_thread`。
- worker（TUI）：`server/operation/codex_adapter/mod.rs:570-577` 生成 `codex [resume <thread>] --remote <uri>`，
  `:1274` 注入 `CODEX_HOME = status_snapshot().codex_home`。`thread/started` 由 `server/pending_codex_threads.rs:27` 的
  内存 FIFO 绑到最早等待的 card（`:373` 写入归属）。
- resume：启动时 `rebuild_thread_cache_from_db`（`shared_codex_appserver.rs:3166-3191`）读取**全部**活跃 Codex 线程，
  `resume_cached_threads`（`shared_codex_appserver/resume_replay.rs:45`）在同一个 daemon 上逐个 resume。
- release：`shared_codex_appserver/thread_release.rs:13-60` 对唯一连接 `thread/unsubscribe`；
  sweeper 的入口在 `server/terminal_sweeper.rs:151-156`。

### 2.4 worker 与 Planner 怎样拿到 daemon

- Planner：`PlannerFactory` 持有 `Weak<SharedCodexAppServer>`（`server/harness/backend/factory.rs:40`、`:131`），
  打开后的 Codex 臂是 `Arm::Codex(Arc<SharedCodexAppServer>, …)`（`server/harness/backend.rs:111`）。
  启动适配器把它存为字段（`server/operation/planner_harness_start_adapter.rs:101`）。
- worker 与 reaper：`WorkerProviderRegistry::new` 把它包成 `Arc<dyn CodexDaemonProbe>`（`server/provider_registry.rs:36`、`:51`；
  实现在 `shared_codex_appserver.rs:4294`）。
- 订阅原生通知：`server/harness/codex_events.rs:24`、`server/dispatcher/mod.rs:820`、`server/operation/codex_adapter/mod.rs:448`、`:1146`。
- 其余调用者：MCP 工具重载（`server/codex_mcp_toolset.rs:73`、`:163`）、代理设置变更（`server/routes/settings.rs:111`）、
  休眠（`server/planner_hibernation.rs:86`）、track/area 删除（`server/routes/tracks.rs:936`、`:3328`、`:3456`）。
  非测试文件中共 53 个引用了这个类型或字段。

### 2.5 模型列表

`GET /api/models` → `server/routes/models.rs:80-86` → `server/planner_model/acquisition.rs:298` `codex_catalog`
→ `shared_codex_appserver/metadata_reads.rs:16` `model/list`（不缓存；没有连接时读该 home 的 `config.toml` 默认值）。
每轮的模型解析在 `server/harness/codex_selection.rs:39`。Claude 和 ACP 的模型目录缓存在可用性 slot 中。

### 2.6 配置

`server/config.rs:110` 是 `codex_bin`，`:157-225` 是 daemon 的重启、超时和日志参数，都是 CLI 参数，带同名的 `CALM_*` 环境变量。
Claude Planner 的凭据目录来自 `--claude-planner-config`（`server/config.rs:41`，结构见 `server/claude_planner/config.rs:28-35`）。
ACP 注册来自 `--acp-planner-config`（`server/config.rs:45`；校验见 `server/acp_planner/config.rs:129-140`，**每个 provider 只能登记一次**）。

### 2.7 持久化中线程与 daemon 的关系

- `worker_sessions` 存 `provider`、`thread_id`、`agent_session_id`，索引为 `(provider, thread_id)`（迁移 0163）。
  **没有任何列记录 daemon、home 或账号。**
- 按线程查找时只用 `provider='codex'` 一个命名空间，例如 `server/mcp_server/transport.rs:1780` 和
  `truth/db/sqlite/session_repo_impl.rs:172-195`。
- 唯一识别 daemon 的是单行表 `shared_codex_daemon`。读写接口 `shared_daemon_runtime_get/set` 不带键（`truth/db/mod.rs:403`、`:590`）。
- ACP 已有先例：会话以注册摘要（含 env）为键持久在 `acp_managed_sessions` 中（`truth/db/sqlite/acp_submission.rs:66-112`）。

## 3. 单例假设清单

| # | 位置 | 单例假设 | 多后端时的后果 | 处理切片 |
|---|---|---|---|---|
| S1 | `provider/codex/shared/home.rs:32-33` | 注释写明 “the shared daemon's single CODEX_HOME” | 只有一个凭据目录 | P2 |
| S2 | `server/state_clients.rs:125` | 只有一个 `data_dir/codex-home` | 同上 | P2 |
| S3 | `types/runtime_paths.rs:6-8` | socket 固定为 `run/codex-appserver.sock` | 第二个 daemon 会抢同一 socket | P2 |
| S4 | `shared_codex_appserver.rs:765-769` | 认证检查点固定为 `state/codex-authentication.json` | 各账号的认证状态互相覆盖 | P2 |
| S5 | `shared_codex_appserver.rs:795`、`server/config.rs:297` | 只有一个日志目录 | 日志交错 | P2 |
| S6 | `crates/calm-truth/migrations/0026_shared_codex_daemon.sql:2`、`truth/db/mod.rs:403`、`:590` | `CHECK (id = 1)`，读写不带键 | 无法分别记录与接管多个进程 | P2 |
| S7 | `shared_codex_appserver.rs:4290` 起的注释 | 关机时不停 daemon，留给下次启动接管 | 每个后端都要能接管；被删除的后端要有回收路径 | P2 |
| S8 | `shared_codex_appserver.rs:778`、`server/state.rs:285` | “The server's one deletion-seal registry” 在 daemon 内部构造 | N 个实例会产生 N 个 seal 表，删除时的 fence 被拆散 | P2（改为外部注入同一份） |
| S9 | `shared_codex_appserver.rs:3166-3191` | 冷启动时 resume **所有**活跃 Codex 线程 | 每个 daemon 都会 resume 不属于它的线程 | P3 |
| S10 | `server/pending_codex_threads.rs:27`、`:373` | 一条 FIFO 把下一个 `thread/started` 绑给最早等待的 card | TUI 连到不同 daemon 时会错绑 | P3（按后端分队列） |
| S11 | `server/mcp_server/transport.rs:1780`、`truth/db/sqlite/session_repo_impl.rs:183-187` | 线程只在 `provider='codex'` 一个命名空间内查找 | Codex 线程 ID 是 UUID，碰撞可以忽略；但路由需要知道后端 | P3（查找不变，路由加后端） |
| S12 | `server/harness/backend/factory.rs:40`、`:131` | 工厂只持有一个 `Weak` daemon | Planner 只能落在这一个后端上 | P3 |
| S13 | `server/harness/backend.rs:111` | `Arm::Codex` 持有唯一 daemon | 同上；本身不用改，只需换成绑定后端的实例 | P3 |
| S14 | `server/provider_registry.rs:36`、`:51` | reaper 只有一个 `CodexDaemonProbe` | 存活探测会问错 daemon | P3 |
| S15 | `server/operation/codex_adapter/mod.rs:570-577`、`:1274` | TUI 连唯一 `remote_uri`，用唯一 `codex_home` | worker 无法分到别的后端 | P3 |
| S16 | `server/harness/codex_events.rs:24`、`server/dispatcher/mod.rs:820` | 只订阅唯一的通知广播 | 其他后端的事件收不到 | P3（池提供合流订阅） |
| S17 | `server/agent_providers.rs:190`、`:339-357` | 可用性是“每个 provider 一个 slot” | 无法表达“codex 有 2/3 个后端可用” | P1 定义，P4 接入 |
| S18 | `server/routes/models.rs:85`、`server/planner_model/acquisition.rs:298` | 模型目录只来自唯一 daemon | 不同账号的模型可能不同〔待实测〕 | P4 |
| S19 | `server/codex_mcp_toolset.rs:73`、`:163`；`server/routes/settings.rs:111` | MCP 重载和代理变更后的重启只发给一个 daemon | 其他后端的工具目录或环境过期 | P2（池转发给每个实例） |
| S20 | `server/claude_planner/config.rs:28-35` | Claude Planner 只有一个 `config_dir` | 无法有多个 Claude 账号 | 后续 Claude 片 |
| S21 | `server/acp_planner/config.rs:129-140` | 每个 ACP provider 只能登记一次 | 无法有多个 Grok/OpenCode 账号 | 后续 ACP 片 |
| S22 | `provider/worker/failure.rs:6-64` | 只能识别 Codex 并发上限的固定前缀；没有限流和额度耗尽的类别 | 调度器拿不到摘除依据 | P4 |

没有全局 `static`/`OnceLock` 持有 daemon 或 home；单例都来自构造参数、路径派生和单行表。所以多实例化主要改构造与接线，不需要清理全局状态。

## 4. 抽象

### 4.1 与 Agent Runtime 的分工：不建平行 trait

ART §3 规定，有持久事务、凭据、接管与进程所有权的工厂和控制器实现留在 server 集成层。§5.2 允许“有范围的 factory 实现”，
row/wiring 的注入留在原 server 构造路径。本设计照此分工：

| 关注点 | owner | 本设计做什么 |
|---|---|---|
| 协议会话的打开与能力视图 | `PlannerFactory::open` 与各协议臂（M2 之后是 AgentRuntime 实现） | 不改语义，只把唯一 daemon 换成按 `BackendId` 解析出的实例 |
| 后端登记、健康汇总、选择 | **新** `agent_backend_pool`（宿主侧） | 新增 |
| 原生错误分类 | `provider/worker/failure.rs`（provider 自有） | 扩展变体，只用原生证据 |
| 每轮准入 | `provider_execution::Admission` 的调用者实现 | 组合“绑定后端可用”这一条件 |
| 进程生命周期 | 各协议 host：`SharedCodexAppServer` 和 Engine | 每个后端一个 supervisor 实例，池持有它们的 `Arc` |

池对外只有宿主内部的类型和函数，不进入 `provider` 公共契约。按 ART §3 的判别法，换一个没有 Neige 数据库的宿主，它不需要多账号负载均衡，
所以这属于宿主政策。

### 4.2 Backend：一个可服务的实例

```text
BackendRef      = (AgentProvider, BackendId)        // BackendId: 运维声明的稳定短名，如 "default"、"team-b"
BackendProfile  = 类型化登记（§6）：BackendRef、凭据范围、权重、容量上限、是否接收新会话
BackendInstance = 宿主内的运行句柄，按 AgentProtocol 闭集持有已有 host 的实例：
                    Codex  -> Arc<SharedCodexAppServer>        (ART §10 SharedProcess)
                    Claude -> 凭据档 + 已有 ClaudePlannerHost   (OwnedProcess，每轮一个 execution)
                    Acp    -> 一条 AcpAgentConfig + 已有 AcpPlannerHost (OwnedProcess)
BackendSignals  = 健康（Ready / Unavailable(reason) / NotConfigured）、在途数、冷却截止时间（带来源证据）、额度状态、观测时间
```

“能力”不在池里另存一份。能力视图属于 ART §5.3 的 Session，池只关心“能否接新会话”。同一 provider 的各后端运行同一个二进制与 profile，
能力相同。若出现账号级差异（例如模型目录不同），它作为 `BackendSignals` 里的观测事实报告，不作为静态能力清单。

`BackendInstance` 的三个分支和 `PlannerFactory` 一样，按 `AgentProtocol` 分派。这是 `types/runtime.rs:41-44` 声明的闭集，
不是按 provider 身份分支。新增一个 ACP provider 不需要新分支。

### 4.3 BackendPool

```text
BackendPool
  profiles(provider)                     -> 该 provider 已登记的后端（配置顺序）
  instance(BackendRef)                   -> BackendInstance | NotRegistered     // 解析已持久的绑定
  select_for_new_session(provider, hint) -> BackendRef | NoEligibleBackend{per-backend reasons}
  signals(BackendRef)                    -> BackendSignals（带观测时间；读不到是 Unknown，不是 0）
  observe(BackendRef, BackendObservation)// 由 host/分类器报告：失败分类、限流事件、认证变化
  subscribe_notifications()              -> 各 Codex 实例通知的合流（每条带 BackendRef）
```

- 池只持有句柄和内存信号，不持有队列、轮次状态或第二份 busy 标志。ART §11 规定这些归 `PlannerHarness` 和各 backend。
- 没有配置文件时，池里每个 provider 有一个隐式的 `default` 后端，等价于今天的单实例行为（§6.3）。

### 4.4 Scheduler / Policy

调度器是池内的纯函数 `select(profiles, signals, policy) -> Decision`，只读 `BackendSignals` 与登记数据，从不按 provider 分支。

**粘性（硬约束）**
- 只有 Unbound 会话（尚无原生 ID）才会被选择。一旦原生 ID 出现，`BackendRef` 与它在同一事务里落库（P3），之后 `open/resume`
  只按库里的 `BackendRef` 解析。
- 绑定的后端不可用时：resume/发轮返回 ServiceUnavailable 或可重试拒绝，沿用既有 hold 和 retry 路径（ART §6 的映射不变）；
  **不**自动改绑。用户显式的“新会话/reset”走既有入口，生成新的 Unbound 会话后重新选择。
- 已删除登记的后端：绑定它的会话显式报 `BackendRetired`，提示用户 reset，不静默回落到 default。

**新会话的选择**
1. 筛选：`accepts_new_sessions`，健康为 Ready，不在冷却期，额度未标记耗尽，`inflight < max_inflight`（有上限时）。
2. 打分：默认按加权最少在途，`score = inflight / weight`，同分时按配置顺序。也可选 `ordered`（主备：按顺序取第一个合格的）。
   两种策略都是配置值，策略集合属于池自身，是封闭集合。
3. 没有合格后端：返回 `NoEligibleBackend`，带每个后端的原因。调用方按既有可重试拒绝处理，并在 UI 显示原因。

**摘除与恢复**

| 触发（证据来源） | 动作 | 恢复 |
|---|---|---|
| 健康检查失败：daemon 不在运行或 `account/read` 未登录（`server/agent_providers.rs:339-357`） | 从候选中摘除 | 下一次检查为 Ready |
| 认证失效被确认（`AuthenticationNoticeKind::holds_issuance`，`server/codex_authentication.rs:31-33`） | 摘除 | 沿用既有 owner retry（`POST /api/agent-providers/codex/retry`），按后端各自进行 |
| 并发上限（`WorkerFailureKind::ConcurrencyLimit`） | 冷却，默认 60 s，按 `cooldown_after_concurrency_limit_secs` 配置 | 冷却到期 |
| 限流或额度耗尽，且原生给出重置时间（Claude 的 `rate_limit_event.rate_limit_info.resetsAt`，见 `provider/tests/fixtures/claude_planner_stream/pH.ndjson`） | 冷却到 `resetsAt` | 到期，或下一次成功的原生观测 |
| 限流或额度耗尽，原生没有给出时间（Codex 的 `codexErrorInfo` 类别〔待实测：固定版本 app-server 的枚举值〕） | 冷却，按配置的默认时长，指数退避，有上限 | 到期后半开：只放一个新会话试探 |
| 进程崩溃 | 摘除，交给既有 `heal_loop` 自愈 | readiness 为 running |

规则：只用原生字段或 transport 事实判定，不靠错误文本猜（`ConcurrencyLimit` 现有的固定前缀匹配是 provider 自有的已知服务消息，
`provider/worker/failure.rs:42-49`，可以保留）。冷却只影响新会话的选择；已绑定会话的失败仍走原 owner 的 retry/hold 判定。

### 4.5 信号从哪里来

- 健康：复用 `ProviderAvailabilityCache` 的 `Slot`，把键从 provider 扩展为 `BackendRef`（`server/agent_providers.rs:189-207`）。
  检查函数不变，例如 `check_codex(daemon)` 改为对每个实例调用一次。`GET /api/agent-providers` 的 provider 级结论改由各后端汇总：
  任一后端 Ready 即为 Ready，原因列出各后端的情况。
- 在途数：Codex 从实例内现有的 native turns 状态读出（`SharedCodexAppServer` 已有 `active_turn_id_for_thread`）。
  OwnedProcess 从 `provider_execution` 已持久的 Held/Observing 行统计（需要 P3 的绑定列才能按后端分组）。
- 失败分类：provider 自有的 `WorkerFailureCause` 增加 `RateLimited { resets_at: Option<…> }` 与 `QuotaExhausted`（只在有原生证据的分支中产生）；
  宿主在既有失败消费点（例如 `server/scheduler/running_worker.rs:94`）调用 `pool.observe(...)`。

### 4.6 能否复用 provider_execution 的 Admission

可以部分复用，范围如下：

- **OwnedProcess（Claude/ACP Planner 轮，以及 WA2 之后的 claude-worker 轮）**：每轮都经过 `ProviderExecution::admit`
  （`server/provider_execution/mod.rs:250-280`），调用者的 `Admission::authorize_tx` 在准入写事务里执行。
  可以组合一个 `BackendAdmission` 条件：读取行上的 `BackendRef`，确认该后端仍登记、未退役、未在冷却期。
  条件失败就不准入，返回可重试拒绝。这正是 WA1a 端口的用途，不新增端口。
- 冷却与健康是内存信号，不在事务内。`authorize_tx` 读取池的内存快照，事务只保证“准入”和“绑定行”的一致性。
  对内存信号，这是“检查时刻的事实”，不是持久保证。这一点与 ART §11.1 的 live 复检一致。
- 容量上限（`max_inflight`）对 OwnedProcess 可以在准入事务内按 Held/Observing 行计数，从而做成持久保证。
  默认建议不开启，只作为可选项（待决问题 Q4）。
- **`ReleaseAuthority` 不适用**：释放授权保护的是 task 与载荷的一致性，与后端容量无关。
- **Codex SharedProcess**：轮次不经过 Engine（`turn/start` 直接发给 daemon）。准入沿用既有发行同步边界内的
  `require_start_authentication` 一类宿主检查；池只在选择阶段起作用。不为 Codex 新建一个准入端口。
- **选择不是准入。** 选择发生在会话的原生 ID 建立之前，每个会话一次；准入每轮一次，只检查绑定后端，**从不改选**。

## 5. 各 provider 的差异映射

| 维度 | Codex | Claude | ACP（Grok / OpenCode） |
|---|---|---|---|
| 资源形态（ART §10） | SharedProcess：每个后端一个长驻 app-server，多个线程复用一个连接 | OwnedProcess：每轮一个 `claude -p` execution（Engine） | OwnedProcess：每轮一个 agent 进程（Engine） |
| 后端实例 = | 一个 `SharedCodexAppServer`（home + socket + 认证检查点 + 运行记录行） | 一份凭据档：`config_dir` + 固定版本二进制（`server/claude_planner/config.rs:28-35`） | 一条注册：command/args/env，其中 env 含 `GROK_HOME` 等（`server/acp_planner/config.rs:18-32`） |
| 凭据 | `CODEX_HOME/auth.json`（播种见 `provider/codex/shared/home.rs:54-70`） | `CLAUDE_CONFIG_DIR`，owner 在其中 `/login` | 由 agent 自行登录后缓存的 token（Grok 用 `cached_token`，`server/acp_planner/profile/grok.rs:1-7`） |
| 原生会话粘性的证据 | 线程属于该 daemon 的 home | `--resume <session>` 需要同一个 config dir〔待实测〕 | 注册摘要不一致时返回 `Changed`（`truth/db/sqlite/acp_submission.rs:102`） |
| 健康探测 | daemon readiness + `account/read` | `--version` + `auth status --json` + 模型目录（`server/claude_planner/availability.rs:58-117`） | `configured` + `check_ready`（`server/agent_providers.rs:261-295`） |
| 在途数来源 | 实例内的 native turns | Held/Observing execution 行 | 同 Claude |
| 限流证据 | `codexErrorInfo`〔待实测〕；已知的并发上限消息 | `rate_limit_event`（stream 中已有，现在未消费） | 尚无；只有 `StopReason::Refusal` |
| 多实例所需改动 | P2/P3：supervisor 多实例化与绑定 | 凭据档从一份变成列表，`ConfigSource::Dedicated(path)` 按档取 | 去掉“每个 provider 只登记一次”，改为 `(provider, backend_id)` 唯一 |

**扩展到多个 OpenCode 需要新增什么。** `AgentProvider::OpenCode` 已存在，并且走 ACP（`types/runtime.rs:31`、`:70`）。多实例只需：

1. ACP 注册的唯一键从 `provider` 改为 `(provider, backend_id)`（`server/acp_planner/config.rs:131-134` 一处校验，
   `:269-280` 的 `configured` 改为按 `BackendRef` 查找）。
2. 注册摘要（`:47-82`）已覆盖 env，所以同一 provider 的两条注册天然得到不同的会话键。会话绑定已有持久先例，不需要新机制。
3. 健康 slot 的键改为 `BackendRef`（与 P1/P4 相同的改动）。

池、调度器和 UI 都不加 OpenCode 分支。一个全新协议（既不是 Codex，也不是 Claude 或 ACP）属于 ART 的范围：先增加协议实现，
再登记到池；池本身不改。

## 6. 配置模型

### 6.1 形状

新增一个 CLI 参数 `--agent-backend-config <path>`，内容是 JSON 文件，与 `--claude-planner-config`、`--acp-planner-config` 的形式一致。
不新增 `CALM_*` 环境变量，这也是本设计与现有 daemon 参数（带 env 别名）不同的地方。

```json
{
  "policy": "weighted_least_inflight",
  "cooldown_after_concurrency_limit_secs": 60,
  "cooldown_default_secs": 300,
  "cooldown_max_secs": 3600,
  "backends": [
    { "provider": "codex", "id": "default", "weight": 1, "accepts_new_sessions": true,
      "credentials": { "kind": "codex_home", "path": "/srv/neige/codex/default" } },
    { "provider": "codex", "id": "team-b", "weight": 2, "max_inflight": 8, "accepts_new_sessions": true,
      "credentials": { "kind": "codex_home", "path": "/srv/neige/codex/team-b" } }
  ]
}
```

- `credentials` 是按协议的闭合枚举：`codex_home { path }`，后续增加 `claude_config_dir { path }` 和 `acp_registration { … }`。
  枚举随协议实现增加，不提供“任意 JSON extras”（ART §9）。
- 校验（`deny_unknown_fields`）：`id` 在 provider 内唯一，并且满足 `[a-z0-9-]{1,32}`（会用于路径和 socket 名）；路径是绝对路径；
  各后端的凭据路径两两不重叠，也不位于 `data_dir` 内另一后端的派生目录中；`weight >= 1`。
- 每个后端的 socket、认证检查点和日志目录由池按 `BackendId` 从 `data_dir` 派生，例如 `run/codex-appserver-<id>.sock`，不可配置。
  这样少一个出错点。`default` 保持今天的路径，保证接管兼容（§7.3）。

### 6.2 生效时机

只在启动时读取。热重载留作后续：它需要定义“后端退役期间的在途会话”，这超出本设计。更改配置要重启 server；
共享 daemon 在重启后按后端各自接管（S7）。

### 6.3 缺省

没有 `--agent-backend-config` 时，等价于只有一个 `codex/default`，`path = data_dir/codex-home`，行为与今天逐字节一致。
`default` 的播种（从宿主 `~/.codex` 导入）只对 `default` 进行；运维声明的其他后端**不播种**，由 owner 在各自的 home 里登录，
避免把同一个账号复制成多个“后端”。

## 7. 持久化与迁移

### 7.1 thread→backend 绑定需要落库

需要，理由有三：

1. 重启后 `rebuild_thread_cache_from_db` 必须知道每个线程该在哪个 daemon 上 resume（S9）。内存映射在重启后会丢失。
2. Claude/ACP 的 resume 必须回到同一个凭据档。ACP 今天就靠持久摘要保证这一点。
3. 从 daemon 一侧反查（例如问每个 daemon“这个线程在不在你这里”）是根据原生私有布局推断，违反“外部契约事实”规则；
   而且在 daemon 不在线时无法回答。

形状（P3 的新迁移，编号取当时的下一个）：

```sql
ALTER TABLE worker_sessions ADD COLUMN backend_id TEXT NOT NULL DEFAULT 'default';
```

- `NOT NULL DEFAULT 'default'` 不是在掩盖缺失值：所有已有会话确实运行在今天唯一的那个后端上，而它在新模型里的名字就是 `default`。
  回填值是真实事实，不是推测。
- 绑定写入点：与原生 ID 首次写入的同一事务，即 `session_bind_attribution_tx`（`server/pending_codex_threads.rs:373` 与
  §2.7 列出的 ACP/Claude 绑定点）。行在准备阶段就带上选中的 `backend_id`，所以 Unbound 的行也已经确定后端；
  原生 ID 出现时，行上的值已经是最终值。
- 原生 ID 已绑定之后，`backend_id` 不允许更新。SQL 层在既有的“执行持有期间不替换 ID”守卫旁边加同类守卫
  （`truth/db/sqlite/session_mirror.rs:484-525`）。
- 查找：`(provider, thread_id)` 的查找保持不变，因为 Codex 线程 ID 全局唯一；路由时多读一列 `backend_id`。

### 7.2 daemon 运行记录

新迁移建 `codex_backend_daemons`，主键是 `backend_id`，列与 `shared_codex_daemon` 相同，并把 `id = 1` 那一行迁移为 `backend_id = 'default'`。
已发布的 0026/0027 不动（字节冻结）。旧表在同一迁移中删除，避免两个运行真相；回退方案见 §7.3。
`truth/db/mod.rs:403`、`:590` 的读写接口加 `backend_id` 参数。

### 7.3 迁移影响与回退

- 升级时，`default` 保持原 socket、原 home 和原认证检查点路径，所以 P2 之后的首次启动仍能接管升级前留下的 daemon（S7 的 takeover 路径不变）。
- P2 的迁移只改表的形状，不改语义。回退到 P2 之前的二进制会读不到 `shared_codex_daemon`，所以 P2 的回退说明要求
  “回退前先停掉 daemon”；或者 P2 保留旧表只读一个版本。由待决问题 Q6 决定。
- P3 的列只增不删，旧二进制会忽略它。但在 P3 之后创建并绑定到非 default 后端的会话，回退后会被旧二进制当作 default 的线程
  去 resume，导致失败（线程不在那个 home）。回退说明要写明这一点，并建议先把非 default 后端设为 `accepts_new_sessions=false`，
  排空后再回退。
- 本设计不触碰 snapshot 版本、`handle_state_json` codec 或 transcript 身份。

## 8. 权限、隔离与凭据

- **凭据不出宿主边界**：池、信号、API 和日志只携带 `BackendRef` 和 provider 原话中的原因，不携带凭据路径以外的账号身份。
  这与 `server/agent_providers.rs:9-10` 的现有承诺一致。凭据路径只在启动诊断中出现；UI 只显示 `BackendId`，
  owner 在配置里自己知道它对应哪个账号。
- **子进程环境**：每个 Codex 实例沿用 `SpawnEnvironment` 白名单（`provider/codex/shared/environment.rs:8-43`），只有 `CODEX_HOME` 不同。
  `OPENAI_API_KEY`、`CODEX_API_KEY` 这类白名单键是**进程级**环境，会被所有实例共享，与“多账号”冲突。
  因此多后端模式下，若检测到这些键存在，启动时拒绝；在单后端（default）模式下保持今天的行为（待决问题 Q5）。
- **目录隔离**：每个 home 都是 0700，配置文件写入沿用 `write_config_0600`（`provider/codex/shared/home.rs:432`）。
  校验各后端路径互不重叠，防止一个 daemon 读到另一个账号的 `auth.json`。
- **MCP token**：每个线程的 card token 仍随线程启动配置下发（`server/shared_codex_appserver.rs:365-403`），
  daemon 级的 MCP 条目对每个 home 各写一份（`server/shared_codex_home.rs:21-37`），用同一个 kernel socket 和同一个 daemon token。
  kernel 侧按 token 识别 card，与后端无关，所以不扩大授权面。
- **删除 fence**：所有实例共用一份 `ThreadSeals`（S8）。删除 workspace 时，seal 对所有后端同时生效。
- **TUI 的 `--remote`**：worker 的 TUI 只拿到其绑定后端的 socket URI，不会连到别的账号。
- **不跨账号迁移会话**：避免一个账号的对话内容进入另一个账号的历史（同时也是隐私边界）。

## 9. 与 Agent Runtime 迁移（M2 Codex 试点、W 系列）的关系

### 9.1 重叠文件

| 文件 | ART M2 / W 系列在改什么 | 本设计在改什么 |
|---|---|---|
| `server/harness/backend/factory.rs`、`factory/codex.rs`、`factory/recovery.rs` | M2：Codex managed 入口与恢复路径迁到 AgentRuntime 门面 | P3：工厂把唯一 daemon 换成 `pool.instance(BackendRef)` |
| `server/harness/backend.rs`（`Arm::Codex`） | M2：Controller 包装 | P3：实例来源改变，不改语义 |
| `server/shared_codex_appserver.rs` | M2：作为 SharedProcess 的原 owner，保持不变 | P2：构造入口与 6 处运行记录读写 |
| `server/provider_execution/*` | WA1/WA2：owner 层推广、worker turn 授权 | P4（可选）：组合 `BackendAdmission` |
| `server/claude_planner/role.rs`（`ConfigSource`） | WA2：worker 档选择 `UserHome`/`Dedicated` | 后续 Claude 片：`Dedicated(path)` 按凭据档取 |
| `server/agent_providers.rs` | 无 | P1/P4：slot 的键 |

### 9.2 冲突点

1. **ART §15/§16 与 P3**：R1 明确“不新增数据库迁移”，并规定“改变原生 ID 绑定须另立 L2 设计”。P3 正好新增绑定列，
   并改变 resume 的路由。所以 P3 **不能**算作 R1 或 M2 的一部分，必须单独做 L2 设计评审与实施。本文 §7 就是这份设计的草案，
   P3 开始前还要单独过评审。
2. **M2 与 P3 改同一条调用路径**：M2 迁移 Codex 的 open/resume/recovery，P3 在同一路径里插入后端解析。
   并行实施会在同一 owner 上产生两套改动，违反 ART §16.3“同一 owner 串行”的原则。
3. **ART §16.2 的基线**：M2 开始前要对 Codex 方法族做源码基线。如果 P2 在基线之后合入，会改变构造路径，基线需要重做。
4. **TWE 的 OQ10（`config_source = UserHome | Dedicated`）**：claude-worker 的凭据来源选项与本设计的“Claude 凭据档”是同一概念。
   后续 Claude 片必须复用 WA2 定下的 `ConfigSource`，不能再造一个。如果 OQ10 选择 `UserHome`，那么“多个 Claude 账号”只能用于 Planner，
   worker 仍使用 owner HOME 中的唯一登录。这是方向上的真实限制，下面明确写出。

**方向冲突（明确写出）**：ART 把“NativeBinding 的宿主配置范围”留作一个已存在的概念，但 R1 不允许改变它。
本设计的核心正是让这个范围从“唯一”变成“可选”。两者不矛盾的前提是：P3 排在 M2 之后，作为 M2 落地后的第一个独立 L2 行为切片，
与 ART §16.3 的 H 系列同级排序。如果用户希望在 M2 之前就上线多账号，就要接受 M2 的基线包含 P3，M2 的范围相应变大。
这需要 ART 的 owner 同意，本文不擅自决定（待决问题 Q1）。

### 9.3 建议排序

```text
P1 (L1, 只新增模块)   ──可与 M1/M2/W 并行──────────────┐
P2 (L2, 多实例 supervisor) ── 建议在 M2 基线之前合入；否则等 M2 合入后再做 ─┤
M2 (ART Codex 试点)                                     │
P3 (独立 L2, 绑定与路由) ── 必须等 M2 合入 ─────────────┘
P4 (L1, 限额感知 + 按后端可用性) ── 等 P3
Claude/ACP 多凭据档 ── 等 M3 对应 backend 迁移与 WA2/OQ10
```

- 可以并行：P1 与 M1、M2、WA 系列（P1 只新增文件，挂接点只有配置解析和启动日志）。
- P2 只改 `SharedCodexAppServer` 的构造与运行记录，ART 把这一层视为不迁移的原 owner（§10 SharedProcess 的生命周期 owner）。
  它可以与 M2 并行，但 M2 的基线要在 P2 之后取，或 P2 推迟到 M2 之后。建议 P2 先于 M2 基线合入，让 M2 面对的是已经多实例化的 owner。
- P3、P4 必须等 M2 落地。

## 10. 新模块布局与窄挂接点

### 10.1 新模块

层次：池是 server 集成层的宿主 owner（ART §3）。它依赖 `calm_types::runtime::AgentProvider` 与各协议 host，
不进入 `provider` crate，也不新建 crate。

```text
crates/calm-server/src/agent_backend_pool/
  mod.rs            BackendPool：登记、实例解析、对外入口（P1 起）
  config.rs         --agent-backend-config 的类型化解析与校验（P1）
  policy.rs         选择纯函数、冷却与半开（P1）
  signals.rs        BackendSignals、BackendObservation，以及从 WorkerFailureCause 的映射（P1 定义，P4 接线）
  codex.rs          Codex 实例的构造：派生 socket/检查点/日志路径，注入共享 ThreadSeals，转发 MCP 重载与代理变更（P2）
  binding.rs        BackendRef 的读写（调用 truth 的新函数）、BackendAdmission（P3/P4）
  notifications.rs  多实例通知合流（P3）
  tests.rs          （按现有惯例，测试拆为同名 *_tests.rs）
crates/calm-truth/src/db/sqlite/backend_daemon.rs   codex_backend_daemons 的读写（P2）
crates/calm-truth/migrations/NNNN_codex_backend_daemons.sql（P2）、NNNN_worker_session_backend.sql（P3）
```

这不是重复实现：守护进程的监督、接管与自愈仍然只在 `SharedCodexAppServer` 里，健康检查函数仍是现有的那些，
模型目录仍走 `acquisition.rs`。新模块只新增“多个”与“选择”。

### 10.2 挂接点（预计改动行数为手写增删的粗估）

| 切片 | 文件:行 | 改动 | 行数 |
|---|---|---|---|
| P1 | `server/config.rs:41-45` 附近 | 新增 `agent_backend_config: Option<PathBuf>` CLI 参数（无 env 别名） | ~5 |
| P1 | `server/state.rs:1425` 附近 | 启动时解析并校验配置，挂到 AppState | ~10 |
| P2 | `server/shared_codex_appserver.rs:754-827` | `new_with_pending` 改为接收 `CodexBackendLaunch { backend_id, sock, auth_checkpoint, log_dir, seals }`，不再从 `cfg` 派生这些路径 | ~25 |
| P2 | `server/shared_codex_appserver.rs:1854`、`:2005`、`:2063`、`:2259`、`:2412`、`:2556` | `self.repo.shared_daemon_runtime_{get,set}` 改为带 `backend_id` 的调用，各 1 行 | ~6 |
| P2 | `truth/db/mod.rs:403`、`:590`；`truth/db/sqlite/out_of_domain.rs:435-490`、`read.rs:740-781` | 接口加键，SQL 改表 | ~40 |
| P2 | `server/state_clients.rs:125`、`server/state.rs:1425-1557` | home 与实例由池构造；`AppState.shared_codex_appserver` 暂时保留为 `default` 实例的句柄，未迁移的调用者不受影响 | ~20 |
| P2 | `server/codex_mcp_toolset.rs:163`、`server/routes/settings.rs:111` | 改为 `pool.for_each_codex(...)` | ~8 |
| P2 | `types/runtime_paths.rs:6` | 新增按后端派生的 socket 函数；`default` 保持原路径 | ~8 |
| P3 | `server/harness/backend/factory.rs:40`、`:131` | `Weak<SharedCodexAppServer>` 改为 `Weak<BackendPool>`，`daemon()` 改为 `codex(row.backend)` | ~15 |
| P3 | `server/harness/backend/factory/codex.rs:43`、`factory/recovery.rs` | 准备线程前选择后端，写入行 | ~20 |
| P3 | `server/shared_codex_appserver.rs:3166-3191` | `rebuild_thread_cache_from_db` 按自己的 `backend_id` 过滤（构造时已知） | ~3 |
| P3 | `server/pending_codex_threads.rs:27` | 等待队列按 `BackendRef` 分组 | ~15 |
| P3 | `server/operation/codex_adapter/mod.rs:570-577`、`:1274` | `remote_uri` 与 `codex_home` 从绑定后端取 | ~10 |
| P3 | `server/provider_registry.rs:36` | `CodexDaemonProbe` 由池实现：按线程的绑定转发 | ~10 |
| P3 | `server/harness/codex_events.rs:24`、`server/dispatcher/mod.rs:820` | 订阅合流 | ~6 |
| P3 | `truth/db/sqlite/session_row.rs:551-598`、`session_mirror.rs:484-525` | 插入行时带 `backend_id`；绑定后不可变的守卫 | ~25 |
| P4 | `provider/worker/failure.rs:6-64` | 新增 `RateLimited`、`QuotaExhausted`，只从原生字段产生 | ~40 |
| P4 | `server/agent_providers.rs:189-207`、`:224-257` | slot 的键改为 `BackendRef`；provider 级结论由各后端汇总 | ~40 |
| P4 | `server/routes/models.rs:85`、`server/planner_model/acquisition.rs:298` | 模型目录取自绑定后端；新会话取自会被选中的后端或 default | ~15 |
| P4 | `server/scheduler/running_worker.rs:94` 等失败消费点 | `pool.observe(...)` | ~10 |

P3 之后，`AppState.shared_codex_appserver` 的其余 40 多个只需 default 实例的调用点（删除、休眠、sweeper 等）逐步改为经过池。
凡是按线程操作的调用点，都改为“按线程的绑定解析实例”。这部分在 P3 里列清单，逐个改。工作量的主要风险也在这里（§12 R2）。

### 10.3 架构五点

- **抽象边界**：选择与冷却政策在池（宿主）；原生错误分类在 provider；协议会话控制在工厂与协议臂；生命周期在各 host。
- **重复逻辑**：不复制监督、健康检查、模型目录或准入。新增的只有“多个”与“选择”，以及一个汇总函数。
- **硬编码身份**：池与调度器不按 provider 分支；`BackendInstance` 按 `AgentProtocol` 闭集分派，这是 ART 声明的闭集。
  `default` 是池自己拥有的固定标识，用于升级兼容。
- **外部契约事实**：粘性依据原生会话与凭据目录的已记录关系，以及 ACP 的摘要；不从私有目录推断线程归属。
  限流只读原生字段；Codex 的枚举值标为〔待实测〕，在 P4 中先核实固定版本的 app-server schema。
- **永久成本**：新增的持久内容只有一列（`worker_sessions.backend_id`）和一张表（`codex_backend_daemons`）。
  新增后端只改配置文件，不改代码、测试清单或 golden。调度测试是按场景的单元测试，不固定测试名清单；CI 中只多了这些单元测试本身。

## 11. 切片计划

### P1 配置与池模型（L1）

- **范围**：`agent_backend_pool/{mod,config,policy,signals}.rs`；CLI 参数；启动时解析、校验并记录日志。不接管任何流量：
  没有配置文件时，池中只有隐式的 `codex/default`，所有调用点保持原样。
- **新增文件**：上面 4 个文件和测试文件。**修改文件**：`server/config.rs`、`server/state.rs`（各几行），`server/lib.rs`（模块声明）。
- **测试**（`cargo nextest run -p calm-server agent_backend_pool`）：配置校验（重复 id、相对路径、路径重叠、未知字段、非法 id）；
  选择：加权最少在途、`ordered`、上限、冷却、半开只放一个、全部不合格时的逐个原因、同分时按配置顺序；无配置时等价于单个 default。
- **等级**：L1。不跨越持久、权限或进程边界；只新增类型与纯函数。

### P2 多实例 Codex supervisor（L2）

- **范围**：按配置为每个 Codex 后端构造一个 `SharedCodexAppServer`（独立 home、socket、认证检查点、日志和运行记录行），共用 `ThreadSeals`；
  `codex_backend_daemons` 迁移；MCP 重载与代理变更后的重启转发给每个实例；非 default 后端启动并做健康检查，但**还不接收会话**。
- **新增文件**：`agent_backend_pool/codex.rs`、`truth/db/sqlite/backend_daemon.rs`、迁移文件、测试。
  **修改文件**：`server/shared_codex_appserver.rs`（构造与 6 处单行替换）、`truth/db/mod.rs`、`truth/db/sqlite/{out_of_domain,read}.rs`、
  `server/state.rs`、`server/state_clients.rs`、`types/runtime_paths.rs`、`server/codex_mcp_toolset.rs`、`server/routes/settings.rs`。
- **测试**：两个 fixture 实例的 socket、检查点和日志路径互不相同；`default` 路径与今天逐字节一致；从旧单行表迁移后能接管升级前的 daemon；
  一个实例崩溃时只有它自愈，另一个的 readiness 不变；seal 对两个实例都生效。
  按 AGENTS.md 做变异验证：把派生 socket 改回固定路径，预测上面路径隔离的用例变红。
- **等级**：L2（进程生命周期、凭据隔离、数据库迁移）。

### P3 thread→backend 绑定与路由（独立 L2，按 ART §16）

- **范围**：`worker_sessions.backend_id` 迁移；会话准备时选择后端（只针对 Unbound）；工厂、恢复、冷启动 resume、pending 队列、TUI `--remote`、
  reaper 探测、通知订阅全部按绑定路由；绑定后不可变的 SQL 守卫；退役后端报 `BackendRetired`。
- **新增文件**：`agent_backend_pool/{binding,notifications}.rs`、迁移文件、测试。
  **修改文件**：§10.2 中 P3 的各行，以及按线程操作的调用点清单。
- **测试**：两个后端各有一个会话，重启后各自 resume 到自己的 daemon（`rebuild_thread_cache_from_db` 不跨后端）；TUI 的
  `thread/started` 只绑给同一后端的等待者；绑定后改 `backend_id` 被拒；绑定后端不可用时发轮是可重试拒绝且不改绑；
  删除时 seal 对两个后端都生效。
  变异验证：去掉 `rebuild_thread_cache_from_db` 的后端过滤，预测“重启后各自 resume”的用例变红；去掉不可变守卫，预测改绑被拒的用例变红。
  E2E 只跑 tier 1；真实 Codex 的多账号 E2E 在专用宿主上进行。
- **等级**：L2，且按 ART §16 需要单独的设计评审（改变原生 ID 绑定）。

### P4 限额感知调度与按后端可用性（L1）

- **范围**：provider 的失败分类增加限流与额度耗尽（只用原生证据，先核实 Codex 固定版本的 `codexErrorInfo`）；
  消费 Claude 的 `rate_limit_event`；失败消费点调用 `pool.observe`；可用性 slot 按后端；`GET /api/agent-providers` 返回各后端的状态
  （新增字段，生成 OpenAPI 与 `fe` 绑定）；模型目录按后端；可选的 `BackendAdmission`（组合进 OwnedProcess 的 `authorize_tx`）。
- **新增文件**：`agent_backend_pool/signals.rs` 的接线部分与测试。**修改文件**：`provider/worker/failure.rs`、`server/agent_providers.rs`、
  `server/routes/models.rs`、`server/planner_model/acquisition.rs`、失败消费点、OpenAPI/生成物。
- **测试**：用录制的原生载荷（Claude `rate_limit_event`，Codex 失败的 turn fixture）做分类测试；冷却后新会话改选另一后端，
  已绑定的会话不改绑；`resetsAt` 驱动冷却到期；半开试探；provider 级可用性的汇总规则；API golden 与 `fe` 构建。
- **等级**：L1。理由：只消费 P1–P3 已声明的合同；新增的可选 `BackendAdmission` 只读池的快照，不改变准入事务的既有条件。
  如果实现时决定做持久的容量计数（Q4），就升为 L2。

### 后续候选（不在本计划内）

- Claude 多凭据档：`ClaudePlannerConfig` 从单个变为按档列表，复用 WA2 的 `ConfigSource`。依赖 M3 对 Claude 的迁移与 OQ10。L2。
- ACP 多注册：唯一键改为 `(provider, backend_id)`。依赖 M3 对 ACP 的迁移。L1（摘要已提供会话隔离），若改动绑定写入则为 L2。

## 12. 风险

| # | 风险 | 缓解 |
|---|---|---|
| R1 | 多个 daemon 的内存与 CPU 成本（每个都会回填自己的 state db） | 启动时依次拉起；在文档中给出每个实例的资源估计〔待实测〕；建议后端数量不超过个位数 |
| R2 | P3 中按线程操作的调用点有遗漏，导致线程被发到错误的 daemon | 在 P3 中把调用点清单作为评审附件；路由统一经过 `pool.codex_for_thread`；对唯一入口做变异验证 |
| R3 | 多账号策略违反某个服务的使用条款 | 这是部署方的决定；本设计不提供账号轮换以规避限额的自动化，只为新会话做均衡（Q7） |
| R4 | 冷却时长不准，导致长时间闲置或反复撞限 | 有原生 `resetsAt` 时使用它；否则用指数退避和半开试探 |
| R5 | 与 M2 合并冲突 | 按 §9.3 排序；P1 只新增文件 |
| R6 | 回退后，绑定到非 default 后端的会话不可用 | §7.3 的排空步骤 |
| R7 | `OPENAI_API_KEY` 一类进程级凭据跨后端泄漏 | 多后端模式下启动时拒绝（Q5） |

## 13. 待用户决定的问题（附默认建议）

| # | 问题 | 默认建议 |
|---|---|---|
| Q1 | P3 排在 M2 之后，还是让 M2 的范围包含多后端？ | 排在 M2 之后，作为独立 L2；P1/P2 先行 |
| Q2 | 新会话的默认策略 | 加权最少在途；同时提供 `ordered`（主备） |
| Q3 | 已绑定的会话遇到长期限流时，是否允许“一键在另一后端新开会话”（丢失原生上下文，保留产品历史）？ | 只提供 UI 提示，引导用户走既有 reset；不自动进行 |
| Q4 | OwnedProcess 的 `max_inflight` 是否做成持久的准入条件？ | 先不做，只用内存信号；有实际需求时升为 L2 |
| Q5 | 多后端模式下，若进程环境中存在 `OPENAI_API_KEY`/`CODEX_API_KEY`，如何处理？ | 启动时拒绝，并提示改用各 home 的 `auth.json` |
| Q6 | P2 是否保留旧 `shared_codex_daemon` 表一个版本，以便回退？ | 不保留，在回退说明中要求先停 daemon；保持单一运行真相 |
| Q7 | 是否需要“按账号额度比例”的权重，或者手动权重就足够？ | 先用手动权重；有原生额度读数后再考虑自动权重 |
| Q8 | 非 default 后端是否要从宿主 `~/.codex` 播种？ | 不播种；owner 在各 home 中自行 `codex login` |
| Q9 | Claude 多账号是否也要覆盖 worker？ | 依赖 TWE 的 OQ10；OQ10 选 `UserHome` 时只覆盖 Planner |
