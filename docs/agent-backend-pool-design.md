# Provider 无关的多 Agent 后端池与负载均衡设计

状态：设计稿，第一轮 L2 评审修订；只改文档，不改代码。源码复核基线为 `927adecd8`。
本版是对两路发现的处置，尚不声称通过修订后的两路 L2 复审；逐项证据见 §14。
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
   选择并持久固定发生在首个原生副作用之前；Unbound 不等于仍可改选（§7.1）。
3. **粘性是硬约束，不是一种策略。** 三种协议的原生会话都绑在凭据目录上：Codex 线程存放在其 `CODEX_HOME` 中；Claude 会话记录在
   `CLAUDE_CONFIG_DIR` 下〔待实测：是否只在该目录〕；ACP 会话由注册摘要钉住，摘要包含 env
   （`server/acp_planner/config.rs:47-82`，不一致时 `truth/db/sqlite/acp_submission.rs:102` 返回 `Changed`）。
   所以已绑定的会话不跨后端迁移。限流时它等待冷却，或由用户走既有的 reset 路径开新会话。负载均衡只作用于新会话。
4. **thread→backend 绑定必须落库**，按 ART §16 单独作为一份 L2 切片（P3），不并入 R1 或 M2。
5. **容量只作软选择信号，不新增每轮准入条件。** Codex 的在途数来自既有 native turns；P4 只消费原生观测。
   OwnedProcess 的准入/授权扩展不在 P4 内，若需要须另立 L2（§4.6）。
6. 尽可能用新文件实现池与登记；现有 owner 保留生命周期、授权和恢复逻辑，通过窄入口接线。
   多实例还必须修改恢复范围、探测端口及所有路由调用者，不能承诺仅构造入口与 6 处替换（§10）。
7. 共 4 个切片：P1 配置与池模型（L1）；P2 多实例 Codex supervisor（L2）；P3 thread→backend 绑定（独立 L2）；
   P4 限额观测与选择信号（L1，不含准入/认证授权扩展）。Claude/ACP 多凭据档是后续候选，不在本计划内（§11）。

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
  内存 FIFO 绑到最早等待的 card（`:373` 写入归属）。这是空 prompt 交互卡路径；task worker 的首 turn 已显式
  `thread_start_mint_mcp_shell` → `persist_shared_worker_runtime_fields` → 发轮（adapter `:1161-1208`），不能统一描述成 FIFO。
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
| S9 | `shared_codex_appserver.rs:847-853`、`:3166-3191` | 启动/接管时回填并 resume **所有**活跃 Codex 线程 | P2 就会跨 home resume | P2 default-only 历史范围；P3 按持久绑定 |
| S10 | `server/pending_codex_threads.rs:27`、`:373` | 一条 FIFO 把下一个 `thread/started` 绑给最早等待的 card | TUI 连到不同 daemon 时会错绑 | P3（按后端分队列） |
| S11 | `server/mcp_server/transport.rs:1780`、`truth/db/sqlite/session_repo_impl.rs:183-187` | 线程只在 `provider='codex'` 一个命名空间内查找 | 缺少后端范围校验，UUID 不能充当隔离依据 | P2 token 范围；P3 组合绑定解析 |
| S12 | `server/harness/backend/factory.rs:40`、`:131` | 工厂只持有一个 `Weak` daemon | Planner 只能落在这一个后端上 | P3 |
| S13 | `server/harness/backend.rs:111` | `Arm::Codex` 持有唯一 daemon | 同上；本身不用改，只需换成绑定后端的实例 | P3 |
| S14 | `server/provider_registry.rs:36`、`:51` | reaper 只有一个 `CodexDaemonProbe` | 存活探测会问错 daemon | P3 |
| S15 | `server/operation/codex_adapter/mod.rs:570-577`、`:1274` | TUI 连唯一 `remote_uri`，用唯一 `codex_home` | worker 无法分到别的后端 | P3 |
| S16 | `server/harness/codex_events.rs:24`、`server/dispatcher/mod.rs:820` | 只订阅唯一的通知广播 | 其他后端的事件收不到 | P3（池提供合流订阅） |
| S17 | `server/agent_providers.rs:190`、`:339-357` | 可用性是“每个 provider 一个 slot” | 无法表达“codex 有 2/3 个后端可用” | P1 定义，P3 接入 |
| S18 | `server/routes/models.rs:85`、`server/planner_model/acquisition.rs:298` | 模型目录只来自唯一 daemon | 不同账号的模型可能不同〔待实测〕 | P3（含模型写入校验） |
| S19 | `server/codex_mcp_toolset.rs:73`、`:163`；`server/routes/settings.rs:111` | MCP 重载和代理变更后的重启只发给一个 daemon | 其他后端的工具目录或环境过期 | P2（每实例原 follower；设置广播） |
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
| 每轮准入 | `provider_execution::Admission` 的调用者实现与 Codex 发行 owner | 保持原条件；新增准入另立 L2 |
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

“能力”不在池里另存一份。能力视图属于 ART §5.3 的 Session。相同二进制/profile 不证明账号模型与可用性相同；
目录、认证和可用性按 BackendRef 查询（P3），不复制成静态能力清单。

`BackendInstance` 的三个分支和 `PlannerFactory` 一样，按 `AgentProtocol` 分派。这是 `types/runtime.rs:41-44` 声明的闭集，
不是按 provider 身份分支。新增一个 ACP provider 不需要新分支。

### 4.3 BackendPool

```text
BackendPool
  profiles(provider)                     -> 该 provider 已登记的后端（配置顺序）
  instance(BackendRef)                   -> BackendInstance | NotRegistered     // 解析已持久的绑定
  select_for_new_session(provider, hint) -> 候选 BackendRef 与可选试探租约 | NoEligibleBackend{per-backend reasons}
  signals(BackendRef)                    -> BackendSignals（带观测时间；读不到是 Unknown，不是 0）
  observe(BackendRef, connection_generation, BackendObservation) // 拒绝旧连接代次的状态更新
  subscribe_notifications()              -> 各 Codex 实例通知的合流（每条带 BackendRef）
```

- 池只持有句柄和内存信号，不持有队列、轮次状态或第二份 busy 标志。ART §11 规定这些归 `PlannerHarness` 和各 backend。
- 首次无配置时保留隐式 Codex default；其他协议仍由原登记 owner 管理。本期只接线 Codex，已有持久登记不能以移除配置绕过（§6.3）。

### 4.4 Scheduler / Policy

调度器是池内的纯函数 `select(profiles, signals, policy) -> Decision`，只读 `BackendSignals` 与登记数据，从不按 provider 分支。

**粘性（硬约束）**

- 只为尚未固定后端的新会话选择；现有会话 owner 在首个 mint/resume/发轮或 TUI spawn 之前用 CAS 持久固定 BackendRef（§7.1）。
  无原生 ID、发送 unknown 或没有收到 ack 均不解除固定。后续 `open/resume` 只读这一绑定。
- 绑定的后端不可用时：resume/发轮返回 ServiceUnavailable 或可重试拒绝，沿用既有 hold 和 retry 路径（ART §6 的映射不变）；
  **不**自动改绑。用户显式的“新会话/reset”走既有入口，生成新的 Unbound 会话后重新选择。
- 本期拒绝配置移除已登记后端；停止接新会话用 `accepts_new_sessions=false`。显式退役与资源回收另立 L2（§6.2）。

**新会话的选择**

1. 筛选：`accepts_new_sessions`，健康为 Ready，不在冷却期，额度未标记耗尽；`inflight < max_inflight` 只作软筛选。
   未知在途数不得当作 0；候选缺证据时返回原因。并发选择和已绑定会话可超过软上限，不承诺硬容量。
2. 打分：默认按加权最少在途，`score = inflight / weight`，同分时按配置顺序。也可选 `ordered`（主备：按顺序取第一个合格的）。
   两种策略都是配置值，策略集合属于池自身，是封闭集合。
3. 没有合格后端：返回 `NoEligibleBackend`，带每个后端的原因。调用方按既有可重试拒绝处理，并在 UI 显示原因。

**摘除与恢复**

| 触发（证据来源） | 动作 | 恢复 |
|---|---|---|
| 健康检查失败：daemon 不在运行或 `account/read` 未登录（`server/agent_providers.rs:339-357`） | 从候选中摘除 | 下一次检查为 Ready |
| 认证失效被确认（`AuthenticationNoticeKind::holds_issuance`，`server/codex_authentication.rs:31-33`） | 摘除 | 沿用既有 owner retry（`POST /api/agent-providers/codex/retry`），按后端各自进行 |
| 并发上限（`WorkerFailureKind::ConcurrencyLimit`） | 冷却，默认 60 s，按 `cooldown_after_concurrency_limit_secs` 配置 | 冷却到期 |
| 限流或额度耗尽，且原生给出重置时间（拒绝状态与 `resetsAt` 的关联〔待实测〕；现有 pH.ndjson 仅 `status=allowed`） | 冷却到 `resetsAt` | 到期，或下一次成功的原生观测 |
| 限流或额度耗尽，原生没有给出时间（Codex 的 `codexErrorInfo` 类别〔待实测：固定版本 app-server 的枚举值〕） | 冷却，按配置的默认时长，指数退避，有上限 | 到期后半开：只放一个新会话试探 |
| 进程崩溃 | 摘除，交给既有 `heal_loop` 自愈 | readiness 为 running |

**半开 owner**：policy 纯函数只筛选、打分、返回候选，不持有或分配租约。池在自身原子临界区复检 BackendRef、
冷却代次与连接代次，并为该后端取得唯一试探租约；竞争失败重新评分，不能再发一个试探。租约在绑定 CAS 失败、
发送前取消、确定未发送时释放；已发送/unknown 时保留至有相关性的成功/失败终态或有界超时。成功关闭冷却，
失败/超时重开冷却并递增代次，晚到旧代次结果不能解除新冷却。进程重启丢失租约时先恢复健康/冷却观测，
未知时不放行试探；这不是持久容量或原生副作用 exactly-once 保证。

规则：只用原生字段或 transport 事实判定，不靠错误文本猜（`ConcurrencyLimit` 现有的固定前缀匹配是 provider 自有的已知服务消息，
`provider/worker/failure.rs:42-49`，可以保留）。冷却只影响新会话的选择；已绑定会话的失败仍走原 owner 的 retry/hold 判定。

### 4.5 信号从哪里来

- 健康：复用 `ProviderAvailabilityCache` 的 `Slot`，把键从 provider 扩展为 `BackendRef`（`server/agent_providers.rs:189-207`）。
  检查函数不变，例如 `check_codex(daemon)` 改为对每个实例调用一次。`GET /api/agent-providers` 的 provider 级结论改由各后端汇总：
  任一后端 Ready 即为 Ready，原因列出各后端的情况。
- 在途数：Codex 从实例内现有的 native turns 状态读出（`SharedCodexAppServer` 已有 `active_turn_id_for_thread`）。
  OwnedProcess 后续多凭据片才接入按 BackendRef 的 Held/Observing 统计；本期不增加该接线。
- 失败分类：provider 自有的 `WorkerFailureCause` 增加 `RateLimited { resets_at: Option<…> }` 与 `QuotaExhausted`（只在有原生证据的分支中产生）；
  宿主在既有失败消费点（例如 `server/scheduler/running_worker.rs:94`）调用 `pool.observe(...)`。

### 4.6 准入范围收窄

本期不增加 `BackendAdmission`，也不把池的内存健康快照变成事务授权条件。现有
`server/provider_execution/admission.rs:5-11` 的 `authorize_tx` 是可复用端口，但改变准入/认证授权是独立 L2，
不因“只读快照”就降为 L1。`ReleaseAuthority` 保护 task/载荷一致性，不用于容量。

`max_inflight` 只影响新会话选择，是软上限；既有会话继续由原 owner 的 authentication、hold/retry 和发行围栏控制，
不会为了容量改绑。Codex 不经过 Engine，不新增平行准入端口。后续若要求 OwnedProcess 的持久硬容量，
需另立 L2，明确统计 owner、事务、崩溃释放与 WA 系列关系。

## 5. 各 provider 的差异映射

| 维度 | Codex | Claude | ACP（Grok / OpenCode） |
|---|---|---|---|
| 资源形态（ART §10） | SharedProcess：每个后端一个长驻 app-server，多个线程复用一个连接 | OwnedProcess：每轮一个 `claude -p` execution（Engine） | OwnedProcess：每轮一个 agent 进程（Engine） |
| 后端实例 = | 一个 `SharedCodexAppServer`（home + socket + 认证检查点 + 运行记录行） | 一份凭据档：`config_dir` + 固定版本二进制（`server/claude_planner/config.rs:28-35`） | 一条注册：command/args/env，其中 env 含 `GROK_HOME` 等（`server/acp_planner/config.rs:18-32`） |
| 凭据 | `CODEX_HOME/auth.json`（播种见 `provider/codex/shared/home.rs:54-70`） | `CLAUDE_CONFIG_DIR`，owner 在其中 `/login` | 由 agent 自行登录后缓存的 token（Grok 用 `cached_token`，`server/acp_planner/profile/grok.rs:1-7`） |
| 原生会话粘性的证据 | 线程属于该 daemon 的 home | `--resume <session>` 需要同一个 config dir〔待实测〕 | 注册摘要不一致时返回 `Changed`（`truth/db/sqlite/acp_submission.rs:102`） |
| 健康探测 | daemon readiness + `account/read` | `--version` + `auth status --json` + 模型目录（`server/claude_planner/availability.rs:58-117`） | `configured` + `check_ready`（`server/agent_providers.rs:261-295`） |
| 在途数来源 | 实例内的 native turns | Held/Observing execution 行 | 同 Claude |
| 限流证据 | `codexErrorInfo`〔待实测〕；已知的并发上限消息 | `rate_limit_event`（已有 allowed 样本；拒绝/耗尽语义待实测） | 尚无；只有 `StopReason::Refusal` |
| 多实例所需改动 | P2/P3：supervisor 多实例化与绑定 | 凭据档从一份变成列表，WA2 计划的 `ConfigSource::Dedicated(path)` 落地后按档取 | 去掉“每个 provider 只登记一次”，改为 `(provider, backend_id)` 唯一 |

**后续扩展多个 OpenCode 的已知改动。** `AgentProvider::OpenCode` 已存在，并且走 ACP（`types/runtime.rs:31`、`:70`）。以下是起点，仍须独立扫查绑定与调用者：

1. ACP 注册的唯一键从 `provider` 改为 `(provider, backend_id)`（`server/acp_planner/config.rs:131-134` 一处校验，
   `:269-280` 的 `configured` 改为按 `BackendRef` 查找）。
2. 注册摘要（`:47-82`）已覆盖 env；不同凭据范围应产生不同摘要，相同注册不能靠不同短名假装隔离。复用已有摘要机制，仍显式持久 BackendRef。
3. 健康 slot 的键改为 `BackendRef`（Codex 在 P3 先完成）。

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
      "credentials": { "kind": "codex_home", "path": "/srv/neige/data/codex-home" } },
    { "provider": "codex", "id": "team-b", "weight": 2, "max_inflight": 8, "accepts_new_sessions": true,
      "credentials": { "kind": "codex_home", "path": "/srv/neige/codex/team-b" } }
  ]
}
```

- `credentials` 是按协议的闭合枚举：`codex_home { path }`，后续增加 `claude_config_dir { path }` 和 `acp_registration { … }`。
  枚举随协议实现增加，不提供“任意 JSON extras”（ART §9）。
- 校验（`deny_unknown_fields`）：`id` 在 provider 内唯一，并且满足 `[a-z0-9-]{1,32}`（会用于路径和 socket 名）；路径是绝对路径；
  先创建/校验受控目录再 canonicalize；路径两两不重叠，也不位于另一后端的派生目录中；`weight >= 1`。
  注册后持久冻结规范化凭据范围（协议、canonical home，ACP 后续用其已定义摘要）；重启及任何 daemon 接管之前核对，
  同名换 home、路径别名/符号链接导致范围变化、删除后复用 id 均拒绝。轮换同一 home 内的登录凭据允许，不把秘密内容当身份。
- 每个后端的 socket、认证检查点和日志目录由池按 `BackendId` 从 `data_dir` 派生，例如 `run/codex-appserver-<id>.sock`，不可配置。
  这样少一个出错点。`default` 保持今天的路径，保证接管兼容（§7.3）。

### 6.2 生效时机与移除

只在启动时读取，无热重载。本期收窄为**拒绝移除任何持久登记的 BackendRef**，即使没有活跃会话，避免 server 停止后
遗留 daemon 无回收 owner。部署删除配置文件也须过同一登记一致性校验，不能切回隐式 default 绕过。保留登记并设置
`accepts_new_sessions=false` 可排空，但不代表已退役或删除资源。显式退役（停止/确认进程、清理运行记录、处理历史绑定、
保留不可复用身份墓碑）另立 L2；本期不提供不完整的 `BackendRetired` 流程。

### 6.3 缺省与播种（Q8 已定）

全新无配置部署只有隐式 `codex/default`，home 为 `data_dir/codex-home`，沿用既有 default 初始化。
升级时必须核对旧部署的 home 与 default 的规范化范围一致；示例 `/srv/neige/data` 是 data_dir，占位路径不能覆盖旧 home。
有历史登记的部署仍受 §6.2 一致性检查约束。

**显式配置的后端不从宿主 `~/.codex` 或 `$CODEX_HOME` 播种**，owner 在各 home 自行登录；也不从 default 复制。
这包括显式声明的 default。旧 default 已有文件原位保留，未配置的传统单实例首次初始化仍复用现有行为。
P2 不得为新 home 调用 `provider/codex/shared/home.rs:48-90` 的宿主播种入口。

## 7. 持久化与迁移

### 7.1 绑定的持久性与线性化（P3）

现有库只记录 provider/thread，无法支持离线时的后端归属（§2.7）。BackendRef 是会话持久类型的必填字段；
旧行可一次性真实回填为 default，但**不保留 SQL `DEFAULT 'default'`** 掩盖遗漏的新写者。SQLite 迁移具体用表重建或
等价的显式回填实现，沿用存储 owner 的迁移方法；新写者、镜像、fixture、operation 载荷和恢复读取必须全部显式带值。
不改已发布迁移，不改 snapshot/handle/transcript codec 来藏一个平行绑定。

线性化点在已有 session/carrier owner 的事务与 CAS：

1. 新会话准备时计算候选；创建行或首次固定时在事务中复检当前 carrier、operation claim 与未固定状态，持久固定 BackendRef。
   只有 CAS 胜者可继续；败者读回已固定值并复检权限，不使用自己选出的实例。没有已有行时必须先建立受同一 owner 保护的行。
2. 在该提交之前禁止任何原生 mint/resume/turn、TUI spawn 等可建立原生上下文的副作用。BackendRef 一旦固定，
   即使还没有原生 ID 也不可改选；发送结果 unknown、取消或进程崩溃均回到这一后端。沿用原 unknown/恢复政策，不新增重试机制。
3. 原生 ID 返回后，只能在 BackendRef 与当前 carrier/claim 仍匹配的事务中绑定 ID。SQL 不变守卫从固定起生效，
   不等到 ID 出现。reset 使用既有新 carrier 路径，不能原位改 backend。

生产入口包括 Planner `factory/codex.rs`、`factory/recovery.rs`，retained source admission，prompt 交互卡，
空 prompt 的 pending 登记/TUI spawn，以及 task worker 的显式 mint→persist→首 turn（adapter `:1161-1208`）。
`session_bind_attribution_tx` 是 ID 归属写入点，不能独自充当前置固定点。P3 必须扫清所有新建行和原生发送入口。

按线程查找必须将 BackendRef 与原生 thread ID 组合校验；不依赖 UUID 不碰撞来证明隔离。
对于只有 thread ID 的旧入口，由存储 owner 解析唯一已记录绑定；歧义、缺失或范围不一致失败关闭，不轮询各 daemon 猜归属。

### 7.2 登记、运行记录与恢复范围（P2 起）

P2 新增持久登记（BackendRef、规范化凭据范围；本期不删除），与可变运行记录 `codex_backend_daemons` 分开。
后者以 backend_id 为键，沿用现有运行记录的进程身份、环境签名和接管验证；所有读写带键。登记校验先于任一接管/启动副作用，
批量校验失败则不部分启动。升级把旧单行 `shared_codex_daemon` 迁为 default，核对其 `codex_home_path` 与原部署 home 的规范化范围；
旧记录尚未运行、无 home 值时只能使用原配置确定的 `data_dir/codex-home`，不可用新配置的任意路径补齐。不能仅因 socket 同名就接管。
旧表在同一新迁移中删除，0026/0027 字节冻结。daemon 存活而配置缺项时按 §6.2 拒绝启动，不遗弃进程。

| 恢复事件 | P2（仅 default 接流量） | P3 放流量前 |
|---|---|---|
| server 冷启、启动时接管存活 daemon | 旧库所有历史线程仅归 default；非 default 的 DB 回填为空，禁止 mint/外部 TUI 与 pending 归属 | 从 DB 按 BackendRef 筛选，复检 carrier、归档/休眠与权限 |
| 连接重建/热接管 | 仅重放该实例受控 cache；非 default 保持无产品线程，不从全库补齐 | cache 与 replay plan 均带实例范围，源连接代次一致 |
| crash/设置变更替换/自愈冷 resume | 原 supervisor 的锁、cold token 准备和授权复检不变；default 才有历史候选 | 只恢复绑定到本实例的候选；canonical recovery 继续由原 owner 精确恢复 |

`start_or_takeover` 在 `:847-853` 就回填并 resume，故 default-only 过滤和非 default 流量围栏属于 **P2**，不能推迟。
resume 的现有串行化、sleep exclusion、HotTakeover/ColdRespawn 与 token 生命周期继续复用
`shared_codex_appserver/resume_replay.rs:45-151`，不在池重写。每个实例独立 readiness 订阅与连接代次。

### 7.3 升级与恢复备份；不支持降级

`SqlxRepo::open`（`truth/db/sqlite/mod.rs:244-257`）在执行迁移前拒绝未知未来迁移。
因此 P2/P3 后旧二进制**不能打开新库**；保留旧表、停 daemon 或排空非 default 均不能使原库可降级。

升级前停 server 写入，并通过原生命周期 owner 停止/确认所有相关 daemon 与 TUI；备份一致的数据库及各 home/运行状态，
记录二进制与配置版本，再升级。失败恢复需先停止新版本及其全部 daemon/TUI，整体恢复升级前备份和匹配的 home、配置及二进制，
再按旧接管验证启动。恢复会丢失备份后的产品与原生历史，须明确接受；不得把新库交给旧二进制，或混用新 home 与旧库。
这是一条运维恢复路径，不新增双写兼容表。

## 8. 权限、隔离与凭据

- **进程级凭据（Q5 已定）**：多后端模式下，只要存在 `OPENAI_API_KEY`、`CODEX_API_KEY`、`CODEX_ACCESS_TOKEN`、
  `OPENAI_ORGANIZATION` 或 `OPENAI_PROJECT` 即启动拒绝（包括空值，不打印值）。完整集合来自
  `provider/codex/shared/environment.rs:36-42`；未来白名单认证键变动时同步所属校验和测试，不复制整张环境表。
  daemon 用原 `SpawnEnvironment` 白名单；TUI/worker 复用 supervisor 已有白名单（`crates/calm-proc-supervisor/src/child_environment.rs:5-63` 不含这五键），
  从绑定取 CODEX_HOME/remote URI，拒绝从 kernel/supervisor 环境或 operation 载荷再注入上述键。
  单后端传统模式保持原约定。P2 做启动拒绝，P3 验证全部 TUI 路径。
- **home**：0700 是新增要求，当前 `home.rs:55,328` 的 `create_dir_all` 不保证这一点；P2 在任何秘密写入、
  接管或 spawn 前创建/核验 home 所有权和权限，不满足则拒绝并给运维修复提示。config 0600 仍复用
  `write_config_0600`（`:429-449`）。规范化路径隔离和持久身份见 §6–7；不声称能隔离同一 OS 用户的恶意进程。
- **MCP 授权边界（Q10 待决，默认隔离）**：当前全局 daemon token 的 `ConnectionIdentity::DaemonTrust` 只要求
  `_meta.threadId`，在 `server/mcp_server/transport.rs:531-547,1771-1816` 按 provider/thread 解析任意活跃 session。
  把同一 token 复制到多个 home 会扩大信任域，不能声称“不扩大授权面”。默认设计为每 BackendRef 一个 daemon token，
  initialize 后身份带 BackendRef；tools/list、tools/call 及再次 admission 复检 threadId 的持久后端与连接范围一致，
  不一致/unknown 拒绝。CardBound 继续保留现有 session 校验，不因匹配 backend 而绕过 card 权限。
  token 生命周期与存储由现有 MCP wiring owner 扩展；连接身份由 `mcp_server/{handshake,registry}.rs`
  （`:33-63`、`:58-61`）扩展，池不实现第二套认证。旧 default token 升级须经该 owner
  绑定为 default（无法可靠证明则轮换并更新旧 daemon），不得保留无范围 fallback。
  用户尚未选择共享信任域；在 Q10 明确或默认隔离方案经独立 L2 评审落定之前，**P2 不得启动额外 daemon**。
- **MCP follower**：每个 `(home, daemon)` 启动一个原 `CodexMcpToolset`，在该实例 boot 前订阅 plugin events 与自身
  readiness（`server/state.rs:1728-1737`、`codex_mcp_toolset.rs:80-147`）。每次 refresh 仍由原 owner
  `approval_delegated_tools` → generation/grants 写入 → reload；插件读取失败写空 grants，写失败不 reload
  （`:150-200`）。不把 follower 换成池的 reload 广播；一个实例的失败不能阻断其他 follower。
- **删除 fence**：共享 ThreadSeals；strict quiesce、提交后清理与忘记 seal 的次序见 §10.4。
- **披露**：API/日志只给 BackendRef、状态与净化原因，不输出 token 或凭据内容；凭据路径仅 owner 启动诊断可见。
  TUI 只取得绑定实例的 URI，不跨账号迁移历史。

## 9. 与 Agent Runtime 迁移（M2 Codex 试点、W 系列）的关系

### 9.1 重叠文件

| 文件 | ART M2 / W 系列在改什么 | 本设计在改什么 |
|---|---|---|
| `server/harness/backend/factory.rs`、`factory/codex.rs`、`factory/recovery.rs` | M2：Codex managed 入口与恢复路径迁到 AgentRuntime 门面 | P3：工厂把唯一 daemon 换成 `pool.instance(BackendRef)` |
| `server/harness/backend.rs`（`Arm::Codex`） | M2：Controller 包装 | P3：实例来源改变，不改语义 |
| `server/shared_codex_appserver.rs` | M2：作为 SharedProcess 的原 owner | P2：构造、持久键、default-only 恢复；P3：绑定范围复检 |
| `server/provider_execution/*` | WA1/WA2：owner 层推广、worker turn 授权 | 本期不增加准入条件；后续另立 L2 |
| `server/claude_planner/role.rs`（WA2 计划路径/`ConfigSource`，未实现） | WA2：worker 已定 `UserHome` | 后续 Claude 片复用最终 owner：Planner `Dedicated(path)` 按凭据档取 |
| `server/agent_providers.rs` | 无 | P3：slot、认证 retry 与汇总按后端 |

### 9.2 冲突点

1. **ART §15/§16 与 P3**：R1 明确“不新增数据库迁移”，并规定“改变原生 ID 绑定须另立 L2 设计”。P3 正好新增绑定列，
   并改变 resume 的路由。所以 P3 **不能**算作 R1 或 M2 的一部分，必须单独做 L2 设计评审与实施。本文 §7 就是这份设计的草案，
   P3 开始前还要单独过评审。
2. **M2 与 P3 改同一条调用路径**：M2 迁移 Codex 的 open/resume/recovery，P3 在同一路径里插入后端解析。
   并行实施会在同一 owner 上产生两套改动，违反 ART §16.3“同一 owner 串行”的原则。
3. **ART §16.2 的基线**：M2 开始前要对 Codex 方法族做源码基线。如果 P2 在基线之后合入，会改变构造路径，基线需要重做。
4. **TWE 的 OQ10（`config_source = UserHome | Dedicated`）**：claude-worker 的凭据来源选项与本设计的“Claude 凭据档”是同一概念。
   **OQ10 已定 UserHome**（TWE §5.4）；`ConfigSource` 是 WA2 计划的类型化合同，不是本基线已实现的 API。
   后续 Claude 片复用落地后的同一 owner 合同，不预先再造一个；多凭据档只用于 Planner，worker 保持 owner HOME 登录。

**方向冲突（明确写出）**：ART 把“NativeBinding 的宿主配置范围”留作一个已存在的概念，但 R1 不允许改变它。
本设计的核心正是让这个范围从“唯一”变成“可选”。两者不矛盾的前提是：P3 排在 M2 之后，作为 M2 落地后的第一个独立 L2 行为切片，
与 ART §16.3 的 H 系列同级排序。**Q1 已定：P1/P2 先行，P3 在 M2 后独立 L2**；不把多账号流量并入 M2。

### 9.3 已定排序与串行约束

```text
P1 (L1, 只新增模块)   ──可与 M1/M2/W 并行──────────────┐
P2 (L2, 多实例 supervisor) ── 先行；M2 取其最终基线 ──────┤
M2 (ART Codex 试点)                                     │
P3 (独立 L2, 绑定与路由) ── 必须等 M2 合入 ─────────────┘
P4 (L1, 仅限额观测/选择信号) ── 等 P3
Claude/ACP 多凭据档 ── 等 M3 对应 backend 迁移；worker UserHome 不变
```

- 可以并行：P1 与 M1、M2、WA 系列（P1 只新增文件，挂接点只有配置解析和启动日志）。
- P2 的恢复范围、MCP 和身份校验也在原 owner，不能按“只改构造”安排并行写入。P2 先行，M2 以 P2 最终树重取基线；
  若 M2 已开始则串行协调并重定基线，不并入 P3，也不并行改同一 owner。
- P3、P4 必须等 M2 落地。

## 10. 新模块布局与窄挂接点

### 10.1 新模块与复用

```text
server/agent_backend_pool/
  mod.rs, config.rs       登记与配置（P1 类型；P2 身份校验）
  policy.rs, signals.rs   纯评分、类型化观测（P1；P4 信号接线）
  codex.rs                多实例组合、路径派生与原 follower 装配（P2）
  binding.rs              调用会话 owner 固定/解析绑定（P3，不复制 SQL/CAS）
  notifications.rs       带 BackendRef/连接代次的合流（P3）
  probe.rs                按持久绑定取得一次性探测快照的宿主适配（P3）
  对应 *_tests.rs         按职责测试，沿用现有 target
truth/db/sqlite/backend_registry.rs   持久身份登记（P2）
truth/db/sqlite/backend_daemon.rs     按键运行记录（P2）
crates/calm-truth/migrations/下一编号_*.sql       P2 登记/运行表，P3 会话绑定
```

新文件承载“多个”和“选择”；生命周期、认证、权限、replay、模型和 CAS 继续在现有 owner，窄挂接不等于少到可以漏掉调用者。
不新建 crate，不把宿主政策放进 provider。

### 10.2 挂接点重估

以下是手写增删合计的范围估计，**不含新文件、测试、迁移和生成物**；源码位置是本次基线，M2 后须重取。
按职责聚合，不再把换签名当作每处一行。实施中接口不匹配时先核实 owner，不能用平行包装逻辑维持此预算。

| 切片 | 现有文件/入口 | 必须改动 | 粗估行数 |
|---|---|---|---|
| P1 | `server/{config,state,lib}.rs` | CLI、类型解析和只读登记；不启动实例 | 25–50 |
| P2 | `shared_codex_appserver.rs:754-858,1854-2556,3166-3191`、`resume_replay.rs` | 构造注入 BackendRef/路径/seals；按键记录；default-only 回填与全部恢复入口范围复检 | 100–180 |
| P2 | `truth/db/mod.rs:403,590`、`sqlite/{out_of_domain,read}.rs` | 接口带键，委托新登记/运行记录模块 | 45–90 |
| P2 | `server/{state,state_clients,lib}.rs`、`types/runtime_paths.rs` | 全量身份校验先于 boot、组合多实例、逐实例 boot/follower；home 验证/禁止播种 | 100–180 |
| P2 | `shared_codex_home.rs`、`routes/settings.rs:111` | 每 home 注入本实例 token；代理变更所有实例标记重启 | 20–45 |
| P2 | `mcp_server/{wiring,handshake,registry,transport}.rs` 及 token 存储 owner | 默认方案：daemon token 范围、initialize、thread 归属及 admission 复检；复用原鉴权 | 100–200（Q10 未定） |
| P3 | `harness/backend/{factory.rs,factory/*}`、operation 的 start/retained/recovery/adapters、`operation/{mod,driver}.rs` | 首副作用前固定后端；按行构造实例；超时停止与恢复同绑定 | 160–280 |
| P3 | `truth` 的 session row/mirror/repo、server projection 与 pending | 必填 BackendRef、CAS/不可变守卫、所有写者与读取；pending 按范围分队列 | 120–220 |
| P3 | `provider/worker/codex.rs:62-164`、`server/provider_registry.rs`、reaper 调用链 | 改无参探测为一次性绑定快照，保留死亡裁决规则 | 80–140 |
| P3 | `state.rs` 各 state/组合、`harness/codex_events.rs`、`dispatcher/mod.rs`、`replay.rs` | 池注入和带范围通知/重放订阅 | 90–160 |
| P3 | §10.4 按线程生命周期调用者 | strict 删除、forget、休眠、release retry、sweeper、rollout 路由 | 180–320 |
| P3 | `agent_providers.rs`、`routes/{agent_providers,planner_recovery,models,planner_model}.rs`、`planner_model/*`、`harness/codex_selection.rs` | 后端 slot、认证 retry/recovery、catalog 查询与模型写入校验；API/绑定生成 | 180–300 |
| P4 | `provider/worker/failure.rs`、已有失败/原生事件消费点 | 证实后的分类与关联观测，无准入变化 | 60–120 |

现有文件粗估：P2 365–695 行，P3 810–1420 行，另有新实现/测试；旧估计显著低估范围。
P3 涉及同一个“副作用前固定且所有后续操作不越界”不变量，不能拆出一个先放流量、再补清理/模型的切片。
可以在实施内分小批接线，但直到完整退出条件满足都保持非 default 流量关闭；若超出可审查范围，重新设计分片门槛，不能降低 L2。

### 10.3 架构五点与永久成本

- **抽象边界**：池拥有登记、评分与试探租约；会话 owner 拥有 CAS；supervisor 拥有恢复；MCP owner 拥有认证授权。
- **重复逻辑**：复用原 follower、模型 acquisition、健康检查与 replay；新 probe 只选实例/冻结证据，不复制死亡裁决。
- **硬编码身份**：按 AgentProtocol 声明的闭集分派；default 是池的升级标识，不用 UUID 或 provider 名猜线程归属。
- **外部事实**：只用持久范围、协议报告和 Neige 路径；限流拒绝状态待实测，allowed 样本不证明限流。
- **永久成本**：P2 新登记表和运行表，P3 一个绑定字段；普通新增后端只改配置（会持久登记，不能随意删/复用）。
  普通新增路由须走 owner 的绑定入口并加所属行为用例，不维护固定文件/测试名 CI 清单。协议认证环境键变化修改其所属
  环境定义及池校验/测试；新增授权策略另立 L2。沿用既有 target，单元用例每次对应 CI target 执行一次，
  gate 额外一次聚焦 replay；调用者清单是本次评审证据，不加每个 workflow 重复运行的源码文本 pin。

### 10.4 P3 放流量前的路由清单

这是一轮源码扫查的职责清单；实施时对单例字段、类型注入及其下游做新鲜扫查，逐项归类，不能只搜索字段名。
三类入口均须完成；P2 的 default 兼容句柄在 P3 后不得被业务调用者用作隐式路由。

| 类别 | 调用者与当前证据 | 目标路由与验收 |
|---|---|---|
| 按线程/会话 | factory open、恢复、retained（`factory/{codex,recovery}.rs`、`operation/planner_harness_start_adapter/retained.rs:420-455`） | 固定 BackendRef 后才原生调用；CAS 竞争/unknown 不改选 |
| 按线程/会话 | `operation/codex_adapter/mod.rs:448-577,1146-1275`、`pending_codex_threads.rs:121,373` | prompt、空 prompt、worker mint/persist/首 turn、TUI env/remote、通知均使用同一绑定 |
| 按线程/会话 | `routes/planner_cards.rs:83-110`、`routes/tracks.rs:812,1020,3328,3456`、`harness/backend/factory/codex_worker.rs:206-247` | strict quiesce/超时与取消 interrupt 精确归属，不吞掉 strict 失败 |
| 按线程/会话 | `harness/registry.rs:603-690`、`routes/{planner_session,planner_recovery}.rs`、shutdown adapter | replacement/reset/recovery 使用前驱的 BackendRef，不把新 carrier 的选择用来清旧线程 |
| 按线程/会话 | `planner_hibernation.rs:86,158-180`、`worker_flow/codex_rollout.rs:114` | 休眠释放与 rollout thread_path 来自绑定实例；不从别的 home 探测 |
| 按线程/会话 | `provider/worker/codex.rs:119-161`、provider registry、reaper | 一次解析绑定，取得原子快照：宿主范围 BackendRef 与事实载荷（连接代次、running、connected_at、active turn、RPC facts）；RPC 跨代次则 Unknown |
| 按线程/会话 | `planner_model/{acquisition,child_selection,selection_advice}.rs`、`harness/codex_selection.rs`、`routes/planner_model.rs:130-143` | 已绑定 catalog、每轮解析及模型写入校验固定同一后端；创建前的查询显式带候选/范围且不冒称已固定 |
| 按后端 | `agent_providers.rs:189-207,339-370`、`routes/agent_providers.rs:125-145`、`planner_recovery.rs:232` | readiness、account/read、slot、expected_revision retry、恢复只针对指定 BackendRef；provider 汇总不能放行坏的已绑定后端 |
| 全实例广播 | `lib.rs:676`、`state.rs:1728-1737`、`routes/settings.rs:111` | 逐实例 boot；每实例完整 follower/独立 readiness；代理设置所有实例标记；各失败单独报告 |
| 全实例广播 | `terminal_sweeper.rs:149-156`、`thread_release.rs:82-120` | 所有实例的 hibernating release 重试、ended release；实例 owner 保留义务，即使 SQL carrier 已消失 |
| 全相关 owner | `routes/tracks.rs:3204-3216`（含 area 删除）、`shared_codex_appserver.rs:1235-1310` | 提交后在全部相关实例清 cache、native turn/replay/pending 与释放义务，再忘记共享 seal；不能只清 default |

删除在事务/移动 workspace 前收集并保存所有相关 `(BackendRef, thread)`，所有 owner strict quiesce 成功才继续。
提交失败按原 owner 补偿并保留 fence；提交后调用每个相关 owner 清理，待全部清理职责已完成或由实例 owner 明确保留重试义务、
且不能被 replay/pending 重新归属时才 `ThreadSeals::forget_deleted`。释放 RPC 不改成全局必须成功，复用原 best-effort/重试合同；
不能提前丢掉 owner 的义务再以 SQL 行已删为由跳过。所有者集合来自删除计划/登记，不靠删除后的库反查。

探测快照是宿主提供的协议事实，不将 BackendRef/SQL 政策塞入 provider 公共契约：provider 只消费同一次 probe 的事实与代次一致性。
无绑定、找不到实例或 RPC 跨代次报告 Unknown，不能把 default down 当作别的后端 Dead。
readiness 是每实例 watch；合流通知保留 BackendRef/连接代次，不能把广播源身份丢掉。

## 11. 切片计划与退出条件

### P1 配置与纯模型（L1）

只新增类型、配置解析、纯评分与观测模型，在 config/state/lib 留窄挂接，不启动额外实例、不写登记、
不启用选择、不改变既有授权。依赖无；可先于 M2。退出：非法/重复 id、未知字段、相对/重叠路径、权重校验，
加权最少在途与 ordered 的排序/同分规则、Unknown 与软上限场景通过；policy 不分配半开租约。
L1 理由：不跨持久/进程/权限边界。若路径创建/登记副作用被提前，应留在 P2，不能顺手扩大 P1。

### P2 多实例基础设施，非 default 禁止产品流量（L2）

范围为 §6–8：持久身份登记与 keyed runtime 迁移、default-only 历史恢复、home/env 隔离、每实例完整 MCP follower、
共享 seals、各实例 readiness 与生命周期。Q10 默认隔离方案必须先经 L2 落定，不能以基础设施无流量为由复制全局 token。
P1/P2 先行；M2 基线取 P2 最终树，按 §9 串行协调。

退出必须同时满足：

- 两 fixture 实例路径隔离、default 与旧 home 一致、未知/变化/删减/复用配置在接管前拒绝，升级后接管精确验证。
- 带历史线程的旧库启动/接管、连接重建、crash 自愈和设置替换时，只有 default 有产品 resume 候选；
  非 default 的 mint、TUI、pending 归属入口被拒绝。canonical recovery、sleep 与原授权复检保持。
- 每个实例的插件 grants generation 写入先于 reload；插件读取失败空 grants，home 写失败不 reload，
  lagged plugin event/独立 readiness/一个实例失效均不使其他实例漏更新。
- 五个进程级凭据键逐一验证启动拒绝，无值泄露；新 home 不播种且权限检查真实执行；MCP 跨 BackendRef threadId 拒绝，
  旧 token 不保留无范围兼容回落。真实 home 文件权限是新要求，不冒称已有实现。
- 新迁移与未知迁移拒绝测试、升级前备份/整体恢复演练具备证据；非 default 禁流量围栏保持到 P3 完整退出。

L2：身份持久性、凭据、MCP 授权与进程生命周期。新文件见 §10.1，既有挂接规模见 §10.2。
关键隔离断言的 mutation 在独占可恢复 worktree：预测完整失败集，分别单因素去掉恢复范围过滤或让 socket 复用，
跑整个所属 target、解释实际红集、恢复后跑绿；不在文档阶段伪报已执行。

### P3 绑定及全部路由，M2 后独立 L2

前置：P2、M2 最终基线、独立两路 L2 设计评审。范围包含 §7.1、§10.4 **三类全部调用者**，
而非先开放 mint/resume 再补维护路径。按 BackendRef 的 availability、认证 retry/recovery、catalog/每轮模型选择和模型写入校验
均在本片；相关 API/OpenAPI 与前端绑定用真实 generator 更新。

退出必须同时满足：

- 首副作用前固定与 CAS 竞争覆盖 Planner、retained、worker、prompt/空 prompt 卡；所有新写者显式必填 BackendRef，
  旧行仅一次性回填。无 ID、unknown、取消/重启均不重新选择；变更绑定拒绝。
- 两后端的冷启、热接管、重连、自愈只恢复自己线程；通知/pending/replay 保留实例及代次，TUI home/remote 与绑定一致。
- §10.4 每行具备真实生产入口的路由证据与针对两个实例的行为测试：strict deletion、post-commit forget 与 seal 顺序、
  replacement、超时/取消、休眠、release retry、sweeper、rollout thread_path 均无 default 遗漏。
- 探测一次性快照包括 running 与 connected_at，RPC 代次变化为 Unknown；default down、另一实例活跃不会误判后者 Dead；
  readiness 各自订阅，不用无参状态拼出跨实例事实。
- 已绑定模型目录、模型写入、认证 retry 的 expected_revision 和可用性均命中同一后端；provider 任一 Ready 的汇总
  不掩盖目标后端不可用；新会话查询显式范围，查询不能私自固定或重新选择。
- 审查所有单例注入及下游，无业务 default fallback；删除前相关 owner 集合留存到提交后，全部 owner 清理后才 forget seal。
  非 default 流量围栏最后一次性解除；没有清单、断言或实测的路径继续禁流量。

L2：原生归属、持久 CAS、删除/死亡裁决与授权路由。可以分提交评审，不能将放流量退出条件拆散。
mutation 选“去绑定过滤”“绕过不可变 CAS”及跨后端 probe 各一个单因素，预测并核对完整失败集；
tier 1 集成覆盖无需凭据，真实多账号 Codex E2E 仅专用宿主。

### P4 限额观测与选择信号（收窄为 L1）

依赖 P3。仅在固定版本原生证据核实后增加限流/耗尽分类、关联 BackendRef/连接代次的观测，
将其接入池软选择信号与原子半开租约；不增加或改变每轮 admission、authentication/retry 授权、持久容量。
退出：真实拒绝/耗尽 fixture 与 allowed 正例分开，allowed 不触发冷却；resetsAt 只在已证实拒绝语义下使用；
并发半开只有一个租约，取消/确定未发送/unknown/超时的释放和旧代次观测均覆盖；绑定会话不改选、软上限不冒称硬限制。
若原生状态无法证实，就不启用相应分类，只保留已证实信号，不猜错误文本。

L1 理由：纯观测与宿主内存选择，不跨新的权限或持久边界。任何 `BackendAdmission`、硬容量、认证授权调整
单独拆为 **P4-A（独立 L2，需新设计）**，不混入 P4。P1 的评分测试不能替代 P4 并发 owner 租约测试。

### 后续候选

Claude 多凭据档：M3/WA2 后复用 ConfigSource 计划的最终 owner，Planner 可 Dedicated，worker 保持已定 UserHome；L2。
ACP 多注册：依赖 M3，需检查注册摘要、绑定与持久调用者，不预判为 L1；触及绑定则独立 L2。
显式退役、持久硬容量、准入/认证授权扩展均独立 L2，不在本期四片范围。

本轮只修设计，无 Rust/原生测试；kernel gate 独占执行 doc-exists 与文本 ratchet。实施切片的 worker 跑聚焦验证，
gate 按仓库要求拥有 text/contract/quick 和一次拒绝空选择的聚焦 replay，CI 拥有广域套件。

## 12. 风险

| # | 风险 | 缓解/停止条件 |
|---|---|---|
| R1 | 多 daemon 资源成本未知 | 依次启动，资源估计待专用宿主实测；P2 不用流量证明基础设施隔离 |
| R2 | 单例调用者遗漏导致越 home、误判死亡、错误删除 | §10.4 三类完整扫查；B/C/J 全在 P3 退出前；不准先放流量再补 |
| R3 | 多账号服务条款或隐私风险 | 部署方确认；只均衡新会话，不提供跨账号上下文迁移 |
| R4 | allowed 误判限流、旧代次成功解除新冷却 | P4 实测拒绝语义；关联实例/连接/冷却代次，租约有界释放；max_inflight 仅软上限 |
| R5 | M2 同 owner 冲突及旧行数预算失真 | P1/P2 先行，M2 重定基线，P3 后置独立 L2；§10.2 重估，不用预算压掉接线 |
| R6 | 降级或备份/home 混用破坏历史归属 | 不支持原库降级；升级前一致备份，停全部进程后整体恢复；接受备份后数据丢失 |
| R7 | 环境凭据或路径变化使新后端变成旧账号 | 五键启动拒绝；daemon/TUI 环境同检查；规范化范围持久冻结且接管前全量校验；新 home 不播种 |
| R8 | 全局 daemon token 扩大信任域 | Q10 待决默认按 BackendRef 隔离；P2 额外实例启动前 L2 落定，否则维持单实例 |
| R9 | 配置移除遗留存活 daemon 或复用历史身份 | 本期拒绝移除登记；仅停止接新流量，显式退役另立 L2 |
| R10 | 删除后忘 seal 太早或失去释放义务 | 删除计划保存相关 owner，全部清理后才 forget；释放义务留在实例，sweeper 全实例重试 |

## 13. 问题与已定决策

| # | 状态 | 决策/默认与落实 |
|---|---|---|
| Q1 | **已定（用户）** | P1/P2 先行；P3 在 Agent Runtime M2 后作为独立 L2（§9、§11） |
| Q2 | **已定（用户）** | 默认加权最少在途，提供 ordered（§4.4） |
| Q3 | 待决 | 长期限流后跨后端新开：默认只提示既有 reset，不自动迁移（§4.4） |
| Q4 | 待决 | 硬容量：默认不做，max_inflight 仅软选择；需要时独立 L2（§4.6、§11 P4-A） |
| Q5 | **已定（用户）** | 多后端遇进程级凭据/账号范围键启动拒绝，完整五键与 TUI 覆盖见 §8 |
| Q6 | 已由仓库约束收敛 | 不保留兼容旧表，不支持原库降级；升级前备份、失败整体恢复（§7.3） |
| Q7 | 待决 | 默认手动权重，不按未知账号额度自动调整 |
| Q8 | **已定（用户）** | 显式配置 home 不播种宿主 ~/.codex；owner 自行登录；传统隐式 default 兼容边界见 §6.3 |
| Q9 | 已由 TWE OQ10 收敛 | OQ10 已定 UserHome；worker 保持用户登录，多 Claude 凭据档仅 Planner（§9.2） |
| Q10 | **待决，新增信任域取舍** | 多 home 是否共享全局 daemon 信任域？默认不共享：token 绑定 BackendRef 并校验 threadId 后端；若选共享须明确接受跨后端 threadId 授权面并重新 L2。决定/默认方案评审落定前不启动额外实例（§8、P2） |

## 14. 评审处理（第一轮）

全部先用当前源码核实；下表“成立”指设计缺口成立，不代表已实现修复。路径简写同文首，行号按复核基线。
无发现整条驳回；N 中有关“已实现/待决定”的表述按源码与上游定稿纠正。修订版仍交两路 L2 复审。

| 发现 id | 源码核实与处理方式 | 文档小节 |
|---|---|---|
| A / bd-B1 | 成立：`shared_codex_appserver.rs:847-853,3166-3191` 无条件回填再 resume；P2 default-only 围栏，列清冷启/接管/重连/自愈范围，复用原 replay | §3 S9、§7.2、§11 P2/P3 |
| B / bd-B2、ad-B3 | 成立：`routes/planner_cards.rs:83-110` strict interrupt、`routes/tracks.rs:3204-3216` post-commit forget 只用单例；`thread_release.rs:82-120`、`terminal_sweeper.rs:149-156`、`planner_hibernation.rs:86`、`worker_flow/codex_rollout.rs:114` 仍是单例。补全三类清单、所有 owner 清理后忘 seal，移除“后续逐步迁移” | §10.4、§11 P3、R2/R10 |
| C / bd-B3、ad-B2 | 成立：`provider/worker/codex.rs:62-71,119-161` 的 running/connected_at 无线程参数且分次读取；改为绑定的一次性快照、RPC 代次一致、每实例 readiness，不只在 pool 包一层转发 | §10.2、§10.4、§11 P3 |
| D / bd-B4、ad-B1 | 成立：单行记录有 codex_home_path 但无按 BackendRef 的永久范围登记（迁移 0026/0027），当前构造 `:765-771` 从 cfg 派生路径；新增冻结规范化范围的登记，先校验再接管，同名换范围/复用拒绝，升级核对 default home | §6.1–6.3、§7.2、§11 P2 |
| E / bd-B5、ad-B4 | 成立：`truth/db/sqlite/mod.rs:244-257` 显式拒绝未知迁移；删除旧库可直接降级/排空后回退说法，改一致备份整体恢复 | §7.3、R6、Q6 |
| F / bd-B6 | 成立：`codex_mcp_toolset.rs:80-200` 自有 readiness follower 与 delegation→grants→reload 顺序，写失败 return；每实例原样装配，删除仅广播 reload 的设计 | §8、§10.1–10.2、§11 P2 |
| G / ad-B5 | 成立：`mcp_server/transport.rs:531-547,1771-1816` 的 DaemonTrust 按 threadId 解析全局活跃 session；删除“不扩大授权面”，新增 Q10，默认 scope token/校验后端，额外实例启动受阻挡 | §8、§10.2、§11 P2、R8、Q10 |
| H / ad-B6 | 成立：纯评分不可能独占试探；现有在途为 native turns 内存事实（`shared_codex_appserver.rs` 的 `active_turn_id_for_thread`），无容量事务。policy 只评分，池原子租约及取消/unknown/超时规则，观测关联代次，软上限 | §4.3–4.5、§11 P1/P4 |
| I / ad-B7 | 成立：`shared_codex_appserver.rs:4290` 附近明确 daemon 留存供接管，`lib.rs:676` 只 boot 已构造实例。删配置不能回收旧进程；收窄为拒绝移除持久登记，显式退役另立 L2 | §6.2、§7.2、§11、R9 |
| J / ad-B8 | 成立：`routes/planner_model.rs:130-143`、`routes/models.rs:80-86`、`routes/agent_providers.rs:125-145`、`agent_providers.rs:189-207` 都用 default；目录、模型写校验、slot、认证 retry/recovery 全部提前 P3 | §3 S17–18、§10.2/10.4、§11 P3 |
| K / ad-B9、bd-N1 | 成立：adapter `:1161-1208` 原生 mint 在 `persist_shared_worker_runtime_fields` 前，retained `:420-455` 有 claim/admit 和 Unknown 分支；只在 ID 落库时绑定太晚。复用原 owner/CAS 在首副作用前固定，Unknown 不重选，新写必填且无永久 SQL DEFAULT | §4.4、§7.1、§11 P3 |
| L / ad-P2（凭据） | 成立：`provider/codex/shared/environment.rs:36-42` 实有五键；adapter `:1503-1538` 构造 TUI env，不能只改 daemon。home `:55,328` 只有 mkdir，0700 是新增约束；补 daemon/TUI 两路径验收 | §8、§11 P2/P3、Q5 |
| M / ad-P2（限流证据） | 成立：`provider/tests/fixtures/claude_planner_stream/pH.ndjson:11` 的 status 为 allowed，overageStatus rejected 不证明整次请求拒绝；不得用于拒绝冷却，拒绝/耗尽标待实测并保留 allowed 负例 | §4.4、§5、§11 P4、R4 |
| N / bd-N2、bd-N3 | 核实纠正：TWE §5.4 已定 UserHome、§7 WA2 仍是计划；worker 首 turn 的显式 mint/persist 见 adapter `:1161-1208`，FIFO 只属空 prompt 交互路径。删除待决 OQ10/已实现 ConfigSource 暗示；P4 删除 BackendAdmission，新增准入/认证授权只能独立 L2 | §2.3、§4.6、§9.2、§11、Q9 |
