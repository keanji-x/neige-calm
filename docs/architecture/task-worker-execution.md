# Task worker 执行模型：复用 Agent Runtime（A）还是 typed PTY（B）

状态：设计已定稿（第三版）：两路独立 L2 设计评审（boundary、adversarial）第 3 轮通过，用户已确认方向（ask 686425，结论见 §0 第 9 条与 §5.4）。
仍只是设计，只改文档，不改代码，不声称已实现。第一版与第二版各经两路评审（r1、r2）都判为阻塞；第三版按第二轮发现修订，
并把 turn 发行收窄为“一种持久行、每行一个消费点”（§3.6），处置见 §11。
核实基线为 main `6a53c9c05`。本文取代 [workspace-next-steps.md](workspace-next-steps.md) §3.3 的“设计起点”作为里程碑 W 的
重写设计（该节的 WR1–WR7 是本文的验收条件，逐条处置见 §6）。简写：`server/` = `crates/calm-server/src/`，
`truth/` = `crates/calm-truth/src/`，`provider/` = `crates/provider/src/`，`exec/` = `crates/calm-exec/src/`。
文中每条源码事实都带 `file:line`；没有源码证据、须在固定版本的 Claude CLI 上实测的事实标为 **〔待实测〕**，汇总在 §5.3。

## 0. 结论

1. **claude-worker 走路线 A**：迁成与 Claude Planner 共用 stream-json turn 驱动的无界面执行。每个 turn 一个 Engine execution，
   由已落地的 `server/provider_execution/` owner 层管理准入、释放凭据、停止、结算与失联恢复，与 Planner 相同。
   为此要在 owner 层做五处推广：consumer 名、alias、owner 注册、`stop_session` 的位置、**释放授权的调用者事务条件**（§3.2）。
   这样做去掉了 WR3（typed 终端重启）与 PTY 字节保真握手；WR1 收窄成 owner 完成写的既有精确所有权条件（§6）。
2. **释放授权围栏在 Held→Releasing 的同一写事务里**（修 r1 两路 B1）。task 条件只放在准入事务里不够：今天 owner 层的释放事务
   只检查 execution、Observing 与 Held（`server/provider_execution/release.rs:13-29`）。worker 的 task/绑定条件由调用者以事务条件的形式
   组合进这个事务，与 gate 的 `ReleaseAuthority` 先例相同（`server/operation/gate_execution.rs:124-183`）。见 §3.3。
3. **每个 turn（含首 turn）都由同一种持久行发行，每行只有一个消费点**（修 r1 两路 B2、r2 adversarial B1）。首 turn、消息、手动“继续”
   与中断后的续跑都是 `claude-worker-turn` 行；spawn op 只做准备，不准入。每行至多准入一个 execution，然后停在 parked；唯一消费点是
   worker owner 的 `complete`：它以精确 execution 所有权为条件终结该行，若 execution 被 kernel 一侧中断，在同一事务里写出以该 execution
   为键的续跑行。boot 只停 execution，不写行；`complete` 不准入、不发起 turn。见 §3.6。
4. **收尾分派的每一次 task 与租约写都在精确当前 session 围栏之内**（修 r2 boundary B1、adversarial B2）。`converge_dead_worker`
   改为单一写事务：card 当前 session 不是本 session 或本 session 已终结即无副作用返回；task 失败写改用 card 精确所有权，不再用绕过 card
   守卫的 `TaskReporter::Kernel`；首 turn 未 stamp 的窗口用既有的 op 目标证明认定绑定仍有效。见 §3.7。
5. **对称在 task 政策端口上，不在“TUI 降为 viewer”上**。Codex 的 viewer `codex resume <thread> --remote` 是 daemon 的另一个客户端
   （`server/operation/codex_adapter/mod.rs:1284-1288`）。Neige 今天的 Claude 集成里没有接到运行中 `-p` 会话的路径：既有的
   `claude --resume=<id>` 会起一个新的写者进程，所以 restart 在子进程还活着时拒绝（`server/operation/claude_restart_adapter.rs:248-255`、`:262-272`）。
   CLI 本身是否另有此类接口不是源码能证明的，列为外部合同（§5.2）。Claude worker 的“viewer”是 kernel 对 stream-json 与 transcript 的会话投影（只读）。
6. **terminal-worker 只用 Pipe execution**（按用户输入 2 改写）：与 gate 相同的后台 `/bin/sh` 包装脚本，stdout/stderr 进日志文件，
   退出码判定，不提供 PTY 变体，也不让 AI 选择模式。交互需求改由 terminal 卡（Workbench 中的常驻终端）或 agent task 承担。见 §5.5。
   用户提出的 WorkerBench 分层评估见 §5.6，列为 OQ12。
   **codex-worker** 的业务线程不动；它的可选 viewer 留在 Legacy 管线，直到 Legacy wind-down 或 OQ6 有结论。
7. **task 政策不搬家**：目标、租约、gate、超时、取消、重驱与退出判定各自已有 owner（§2.3），worker 和 Planner 的差别全在这些政策里。
   要改的只是它们今天直接碰 PTY 的七个接缝（§2.4）：这些接缝改为调用一个 provider 中立的 worker 会话端口，
   端口的 Claude 实现落到 owner 层的共享 stop/finish，Codex 实现落到既有 daemon 调用。生命周期只有 owner 层这一份。
8. **对 C5 的影响**：无界面 worker 的 execution 带 track（`server/provider_execution/launch.rs:44`），terminal-worker 的 Pipe execution
   同样带 track（gate 先例 `server/operation/gate_execution.rs:79`），都落在 `unsettled_executions(Track)` 之内
   （`truth/db/sqlite/workspace_execution.rs:183-201`），不需要新谓词。尚未准入的 turn 行不是 execution，须由 C1 的 clean 复查视为
   待发行（§3.6）。C5 对两种 worker 保留的 Legacy 检查只剩升级前的 Legacy 行；codex viewer 照旧保留，直到迁移。W 只依赖 C4（§7）。
9. **方向性问题：已定（用户，ask 686425：路线“确认，合并设计”；开放问题“全部按推荐”）**（§5.4）：
   路线 A（claude-worker 无界面）；terminal-worker 只用 Pipe execution；长时间或交互命令走 Workbench 常驻终端卡（OQ12）；codex-worker 业务线程不动；
   OQ1 选 (a) 停止 + 续跑行；OQ2 先只给中断；OQ3 拒绝回给模型；OQ4/OQ10 选 `UserHome`；OQ11 每 turn 一 execution（常驻待 E7）；
   OQ8 接受 Engine 为硬依赖。未被问到的 OQ5、OQ6、OQ7、OQ9 保持开放，按推荐推进，由对应切片定。

## 1. 现状（三种 task worker 都不是 Engine execution）

| | terminal-worker（W1） | claude-worker（W2） | codex-worker（W3） |
|---|---|---|---|
| 启动 | `TerminalWorkerAdapter::spawn_side_effect`（`server/operation/terminal_adapter.rs:754-892`）→ `spawn_task_terminal_with_parts`（`server/routes/terminal.rs:73-89`）→ Legacy `ensure_for_task` | `ClaudeWorkerAdapter` 同一管线（`server/operation/claude_adapter/mod.rs:1027-1047`） | 共享 daemon 的 `thread/start` + `turn/start`（`server/operation/codex_adapter/mod.rs:1143-1316`、`:1207-1256`） |
| 进程 | PTY 中 `/bin/sh -c "<PATH>\n<program>"`，80x24（`server/routes/terminal.rs:137-149`） | PTY 中长寿 TUI：`exec claude --permission-mode auto --settings … --session-id=… --append-system-prompt … --allowedTools mcp__neige --strict-mcp-config --mcp-config … -- '<prompt>'`（`claude_adapter/mod.rs:383-408`、`:269`） | 业务在 daemon 线程；PTY 只是可选 viewer（`codex_adapter/mod.rs:1259-1314`，失败吞为 NoOp `:1311-1314`） |
| 凭据 | 只有代理（`terminal_adapter.rs:968-980` 的 `terminal_worker_env`） | MCP token 只在 EnsureProc env（`server/operation/claude_adapter/worker_mcp.rs:57-63`）；`mcp.json` 已是 `${NEIGE_MCP_TOKEN}` 形式（`worker_mcp.rs:47-50` → `server/mcp_server/wiring.rs:22-43`） | 线程的 `shell_environment_policy.set`（`codex_adapter/mod.rs:1165-1166`）；viewer env 也带 token（`:1276-1281`） |
| 登录与配置 | 不适用 | 无 `CLAUDE_CONFIG_DIR`（`claude_adapter/mod.rs:351-381`）；登录与用户设置经 supervisor 白名单里的 HOME（`crates/calm-proc-supervisor/src/child_environment.rs:5-49`）；二进制来自 `claude_bin`（`claude_adapter/mod.rs:319`） | daemon 自己的配置 |
| 输入 | `KeysOnly`（`provider/worker/terminal.rs:38-41`） | bracketed paste + Enter，等 `UserPromptSubmit` hook（`provider/worker/claude.rs:34-43`；`server/terminal_interaction/message.rs:96-122`） | 经 viewer TUI 粘贴（`server/terminal_interaction/message.rs:133-143`） |
| 进度 | 无 | hook 报告的 `transcript_path` 尾随（`server/worker_flow/claude_transcript_lookup.rs:23-29`）→ `worker_flow_cursors`（`truth/db/sqlite/task_liveness.rs:47-70`） | daemon 通知（`liveness_feeder.rs`）+ 同一 cursor |
| 退出判定 | `on_terminal_exit` → `complete_terminal_task`（`server/scheduler/mod.rs:2129-2133`、`:2162-2280`）；sweep 的 running-terminal 臂读终端退出字段（`:1620-1622`、`:1879-1899`） | PTY 退出对 task 无作用（同 `:2129-2133`）；reaper 探测 `-1` 哨兵 → Failed（`provider/worker/supervisor.rs:35-40`、`server/reaper/mod.rs:327-341`、`:374-453`）；退出 0 且未自报时停在 running，直到 1 小时空闲超时 | `Resumable`，死亡由仲裁确认（`provider/worker/codex.rs:141-159`、`server/reaper/mod.rs:168-256`）；另有 300s“turn 结束未自报”（`server/scheduler/running_worker.rs:23`） |
| 超时与取消 | 无期限：running-terminal 臂排在 liveness 臂之前（`scheduler/mod.rs:1620-1623`）；不可取消（`crates/calm-server/prompts/planner.md:29`） | 1 小时空闲 / 8 小时上限；可取消 | 同 Claude，另有 300s 臂 |

三者都经 `calm-proc-supervisor` 的 Legacy PTY 运行。该 PTY 活过 kernel 重启，不活过 supervisor 重启
（`crates/calm-proc-supervisor/src/lib.rs:460-485`）。boot 时用 `Probe` 收敛（`server/lib.rs:48-125`）。

## 2. worker 与 Planner：相同的与不同的（问题 2）

### 2.1 已落地的 Agent Runtime（C4.1–C4.4）

**owner 层**（`server/provider_execution/`）

- 每个 session 至多一个 claim，存在 `worker_sessions.provider_execution_json`（`truth/migrations/0185_provider_execution.sql:2`）。
- SQL `admit` 的三道条件（`truth/db/sqlite/provider_execution.rs:140-185`）：
  - 拒绝绑定了终端的 session（`:147-157`，C4.2 R1）；
  - 只接受 card 当前 session（`cards.session_id = worker_sessions.id`）且状态为活跃的 session，且 claim 为空（`:168-173`）；
  - 准入不看 `contract`，所以 Executor 会话在 SQL 层可以准入。
- 生命周期操作：
  - `ProviderExecution::admit`：先登记驱动者，再在同一事务里写 claim 与 `engine.admit`，提交后才 drive（`server/provider_execution/mod.rs:186-213`）；
  - `stop`：Observing→Stopping CAS 与 Engine stop 请求同事务（`:151-173`、`:215-232`）；
  - `finish`：结算后在 10s 内完成 owner（`:246-256`），**之后**才在新事务中复核精确 claim、清 claim 并 retire（`:257-273`）；
  - `admission_gate`：前一个 claim 仍在即拒绝；若它已结算且无驱动者，先替它 `finish`（`:129-148`）；
  - 驱动者丢失时，`Cancellation` 发 `DriverLost` Stop（`:304-337`）。
- 恢复路径：
  - 30s durable sweep（`server/provider_execution/sweep.rs:9-91`、`:120-133`）对无驱动者的 claim 先 `stop(DriverLost)`，结算后再 `finish`（`:47-57`）；
  - boot pass A 把 Observing 的 claim 置为 `KernelRestarted` Stop（`sweep.rs:94-118`）。
- 凭据只走 Held stdin 载荷：等到 Spawned 之后另开事务，只核对同一 execution、Observing 与 Held，CAS 到 Releasing 并提交，
  写完再 CAS 到 Released（`server/provider_execution/release.rs:4-68`，其中 `:13-29`）。载荷编解码在 `server/operation/stdin_payload.rs:49-106`；
  代理值先经分类（`server/execution_environment.rs:20-31`）。
- I/O：`EngineStdio` 的读写两侧（`server/provider_execution/stdio.rs:76-231`）带缺口检测与 `StdinTorn`；stderr 有上限（`server/provider_execution/stderr.rs:5`、`:33-53`）。

**Claude 驱动**

- 每个 turn 起一个 `claude -p --input-format stream-json --output-format stream-json --verbose …` 进程（`server/claude_planner/spawn.rs:131-138`、`:186-256`）。
- 环境 `base_env`：`passthrough_keys` 加 PATH、**`CLAUDE_CONFIG_DIR`**、MCP socket、marker 等（`server/claude_planner/spawn.rs:260-267`、`:294-317`）；
  `CLAUDE_CONFIG_DIR` 取 Planner 配置的专用目录，owner 须在其中 `/login`（`server/claude_planner/config.rs:22-32`；`server/claude_planner/session/launch.rs:91-95`）。
- 首条 user line 在凭据 Released 之后写入（`server/claude_planner/session/launch.rs:314-339`）。
- `init_check` 校验版本固定、session、`interrupt_receipt_v1` 与 neige MCP 已连接（`server/claude_planner/driver.rs:124-174`）。
- 看到 `result` 后关闭 stdin（`driver.rs:615-623`），再停止并结算（`:589-721`）。
- 中断经 `control_request`（`server/claude_planner/session.rs:426-449`）。

### 2.2 相同的部分（可共用）

worker 与 Planner 在下列五件事上做的是同一件事，A 路线全部复用、不复制：

- 进程准入与唯一性：一个 session 同时只有一个 execution。
- 凭据传输：MCP token 走 stdin 载荷，`--mcp-config` 只引用 `${NEIGE_MCP_TOKEN}`（`server/mcp_server/wiring.rs:22-43`；worker 今天已用同一函数，`worker_mcp.rs:47-50`）。
- 协议驱动：解码、`init_check`、结果、中断、停止与结算。
- 失联恢复：sweep 与 boot。
- 占用：Engine 行带 track。

### 2.3 不同的部分：全是 task 政策，且各有 owner

| 政策 | 今天的 owner（不搬家） |
|---|---|
| 任务目标与 prompt | `render_task_worker_prompt_tx`（`server/operation/task_prompt.rs:19-75`），冻结在 card payload（`server/operation/claude_adapter/worker_preparation.rs:81-95`） |
| worktree 租约 | 获取：`prepare_worker_lease_tx`（`server/operation/workspace_lease/worker.rs:197`）、`acquire_workspace_lease_tx`（`workspace_lease/mod.rs:124`）；0166 观察围栏不变。释放：`release_workspace_lease_for_card_tx`（`workspace_lease/release.rs:56`） |
| 副作用授权 | `require_attempt_startable_tx`：当前 attempt 且状态 ∈ Dispatched/Running/Verifying（`server/task_recovery.rs:13-36`）；经 `TaskLaunch::authorize_tx`（`server/operation/task_launch.rs:57-59`） |
| gate 与内核代提交 | `task-gate-run` op（`server/operation/task_gate_run/mod.rs:42`）；checkpoint（`task_gate_run/checkpoint.rs:24-62`） |
| 自报 | `commit_worker_task_report`（`server/decision_sink.rs:60-223`）：同一事务做 task CAS（gated 时转 Verifying，`:152-166`）与租约释放（`:199-204`）；不停进程 |
| 1 小时无进展 / 8 小时上限 | `WorkerLiveness`（`server/scheduler/worker_liveness.rs:20-51`）；扫描臂在 `server/scheduler/mod.rs:1623-1653` → `fail_running_worker`（`server/scheduler/running_worker.rs:154-320`） |
| 取消 | `neige_task_cancel` → `cancel_running_in_tx`：按 card 的 task CAS 加清理标记（`server/mcp_server/tools/plan/cancel_running.rs:102-140`） |
| 清理执行 | `sweep_timeout_worker_cleanups`（`server/scheduler/mod.rs:1696-1775`）→ `fail_running_worker_card`（`server/operation/driver.rs:366-408`） |
| 自报后结束进程 | terminal sweeper 的 settled-worker 臂：结束后 60s，每 30s 一拍（`server/terminal_sweeper.rs:73-97`、`:224-304`） |
| 死亡 session 的租约兜底 | `converge_dead_worker`（`server/reaper/mod.rs:374-453`）：无 spawn op、无 task key、无 track 时释放 card 租约（`:381-391`），task CAS 竞争失败也释放（`:445-450`），经 `release_reaped_worker_workspace_lease`（`:455-470`） |
| 重驱 | scheduler `resume_dispatched` 重跑 `drive_spawn`（`server/scheduler/mod.rs:1815`）；op driver `recover_on_boot`（`server/operation/driver.rs:588-625`） |
| 退出判定 | provider `interpret_exit` / `session_mode`（`exec/provider.rs:101-133`、`provider/worker/claude.rs:30-70`）；reaper（`server/reaper/mod.rs:95-453`） |
| 消息 | `TerminalInteraction::message`：只有 Planner 能发，内容持久化，worker 只收到固定通知并去拉取（`server/terminal_interaction/message.rs:25-29`、`:202-322`；`neige_message_read`） |

这些政策只读 task、租约、session 与 attempt 绑定这几类事实，不关心进程是 PTY 还是 Engine。
问题 2 的答案是：**是的，不同只在政策，且政策已经有 owner**。不该做的是复制一套 Planner 那样的 turn 生命周期给 worker，
也不该把 task 政策搬进 owner 层；owner 层的完成端口只做 turn 级收尾（`server/provider_execution/mod.rs:35-48`）。
task 政策进入 owner 层生命周期的唯一方式，是调用者提供的事务条件（准入与释放两处，§3.3）。

### 2.4 政策直接碰 PTY 的接缝（A 路线要改的全部位置）

| # | 接缝 | 今天 | A 之后 |
|---|---|---|---|
| S1 | 启动 | `spawn_task_terminal`（`claude_adapter/mod.rs:1027-1047`） | spawn op 的准备事务写首 turn 行，首 turn 由该行发行（§3.6） |
| S2 | 消息 | 粘贴 + 等 hook + 要求 renderer 开着粘贴模式（`message.rs:96-143`） | 写一条 turn 行，由该行投递成下一 turn 的 user line（§3.6） |
| S3 | 停止（超时/取消） | `stop_timed_out_worker` 按 provider 分支，Claude 是空操作（`server/harness/backend/factory/codex_worker.rs:202-213`），随后按 pid 收割（`server/operation/driver.rs:379-396`） | worker 会话端口 `stop` → 共享 `stop_session` |
| S4 | 自报后结束 | 按 `ws.terminal_run_id` JOIN 终端（`server/terminal_sweeper.rs:85-97`），无界面 session 不在其中 | 端口 `stop`；未发行的 turn 行在授权条件下自然被拒（§3.6） |
| S5 | 死亡判定 | reaper 用 `terminal_run_id` 探测 supervisor（`provider/worker/claude.rs:45-56`） | 结算是 SQL 权威；无界面 session 的收尾由 reaper 的 SQL 臂负责（§3.7），不探测 supervisor |
| S6 | boot | `reconcile_supervisor_on_boot`（`server/lib.rs:48-125`） | worker owner 的 boot 通道只停 Observing 的 execution，不写行；续跑行由 owner `complete` 在终结记录该 execution 的 turn 行的同一事务里写出（§3.6） |
| S7 | 静默检测 | PTY 输出静默 60s 交给 watcher（`server/worker_quiet.rs:21-27`），watcher 可接受目录信任对话框（`crates/calm-server/prompts/terminal/worker-quiet-watch.md:5`） | 不适用：有精确的 turn 结束事件 |

S3 与 S4 今天在 harness factory 里对 provider 硬编码分支（`codex_worker.rs:170-176`、`:207-213`）。A 路线把它们收成
`exec::provider` 的 worker 端口方法，实现在 server（与 agent-runtime-design §3 的“带持久事务的实现留在 server 集成层”一致），
同时删除这两处 match。

## 3. 路线 A：claude-worker 复用 Agent Runtime

### 3.1 执行形状

- **每个 turn 一个 execution**，与 Planner 相同：Planner 驱动在 `result` 后关闭 stdin（`server/claude_planner/driver.rs:615-623`），
  每个 session 一个 claim 的规则正好表达了“turn 串行”（`truth/db/sqlite/provider_execution.rs:168-173`）。另一种形状“常驻 execution 跨多 turn”的对比见 §3.1.1，列为 OQ11。
- **首 turn**：user line 就是今天的位置参数 prompt（`claude_adapter/mod.rs:405-406`），用 `--session-id`（`server/claude_planner/spawn.rs:198-206`）。
- **后续 turn**：Planner 消息、续跑或手动“继续”都起 `--resume <session>`，即同一 Claude session 上的新 execution，由 §3.6 的 turn 行发行。
  alias 含 execution id（`server/provider_execution/sweep.rs:5-7`），没有 typed 终端的“一个 alias 只有一个执行”问题，WR3 因此消失。
- **mid-turn 消息排队到 turn 结束**：turn 行在当前 claim 清除前不会被准入（`admission_gate`，`mod.rs:129-148`）。Claude Planner 路径没有 steer
  （`server/claude_planner/` 中无 `steer`）；今天 TUI 对 turn 中途的粘贴同样是排队（`provider/worker/claude.rs:34-37` 的注释）。
- **turn 结束但未自报**：execution 结算后，owner 完成时 task 仍是 running。照今天 Stop hook 的语义唤醒 Planner
  （`server/dispatcher/mod.rs:156-160`、`:1114-1117`、`:1663-1673`），task 留给 1 小时空闲期限或 Planner 处置。
  这比今天精确：今天 Claude 没有 turn 结束检测（`server/harness/backend/factory/codex_worker.rs:106-109` 对 Claude 返回 None）。
  是否像 Codex 那样加 300s 宽限后失败，见 OQ5。

#### 3.1.1 每 turn 一 execution 与常驻 execution 的对比（用户输入 1）

stream-json 输入下，CLI 进程在 stdin 开着时可以跨多个 turn 常驻〔待实测 E7 一并确认〕；Planner 是有意每 turn 一进程（`result` 后关 stdin，
`driver.rs:615-623`）。turn 进行中再写一条 user 消息的实际行为（排队到 turn 结束，还是在工具边界注入）没有源码证据，列为 **E7**〔待实测〕。

| 维度 | 每 turn 一 execution（推荐首片） | 常驻 execution 跨多 turn |
|---|---|---|
| owner 层记账 | 一个 claim 对应一个 turn，`Purpose::WorkerTurn{turn_id}` 一一对应；`finish` 即 turn 收尾（`mod.rs:234-278`） | 一个 claim 跨多个 turn：turn 身份须在 execution 内另行持久，`complete` 只在会话结束时运行；owner 层要新增“execution 内 turn 结束”的记账面，Planner 不用它 |
| 凭据 | 每 turn 一次 Held 载荷，每次释放都经 §3.3 的授权事务 | 只在会话开始释放一次；之后的 turn 不再经过释放授权，task 条件须改到每次写 user line 前复核（新机制） |
| 消息延迟 | 每条消息要起一个进程并通过 `init_check`（`driver.rs:124-174`）；具体耗时未测 | 写入已在运行的进程，免去进程启动与 init；具体差值未测 |
| turn 中途消息 | 排队到 turn 结束（turn 行等 claim 清除） | 取决于 E7：若 CLI 在工具边界注入，就得到 steer 能力，也带来 turn 中途改变目标的语义 |
| Planner | 不受影响 | 不受影响（Planner 保持每 turn 一进程）；但共享驱动要多支持一种“result 后不关 stdin”的模式（`driver.rs:615-623` 处分叉） |
| 占用与 C5 | turn 之间没有未结算 execution，C5 的正向静默可在 worker 空闲时通过；待发行的 turn 行由 C1 复查覆盖（§3.6） | 会话期间 execution 一直未结算，`unsettled_executions(Track)` 一直非空，空闲的 worker 也让 recycle 等破坏性 effect 得到 Busy |
| kernel 重启（OQ1） | boot 停掉在跑的 turn，空闲 worker 不受影响；续跑由原 turn 行交给续跑行（§3.6） | boot pass A 照抄会停掉整个会话，空闲 worker 也被打断；要么做 OQ1(b) 的重新接管（Engine 持有 stdin，`stdio.rs:76-155`），要么每次重启都重建会话 |
| 停止与超时 | Stop 结束一个 turn；空闲期限读 cursor 不变 | Stop 结束整个会话；turn 级超时要在 execution 内另行判定 |
| 资源 | 空闲时无进程 | 空闲时进程常驻；内存数字未测 |

推荐：WA2 用每 turn 一 execution。常驻形状要先有 E7 的结论，并且要回答释放授权在会话内如何逐 turn 复核、C5 下空闲 worker 的占用、
以及 kernel 重启是否必须先做 WA4。列为 **OQ11**。

### 3.2 owner 层的推广（不改 Planner 行为）

下表列出 owner 层今天的 Planner 专用之处，每处都是 worker 消费者会撞上的：

| 处 | 证据 | 推广 |
|---|---|---|
| `KnownEnd` 是 Claude/ACP 变体的闭合枚举 | `server/provider_execution/mod.rs:28-34` | 加 `ClaudeWorkerTurn`。provider 协议结束是 owner 层定义的有意闭合集合，按 consumer 增长；它与既有 `ClaudeTurn` 的区别是携带 worker 的 turn sink 而不是 Planner 的 `Shared`（见下文驱动接缝）；成本见 §8 |
| `Purpose` 只有 Turn/Compaction/RewindCheck | `truth/db/sqlite/provider_execution.rs:28-32` | 加 `WorkerTurn{turn_id, intent_key}`（JSON 列，无 schema 迁移；旧二进制读到时按 sweep 的“未分类即保留”处理，`sweep.rs:18-21`） |
| alias 固定为 `planner:` 前缀 | `sweep.rs:5-7`、`launch.rs:42` | 改由 owner 提供前缀；Planner 两个 consumer 保持原字节，否则既有行的 sweep 匹配会断 |
| owner 列表写死两个 Planner host | `server/execution_boot.rs:10-18`；projection `:29-49` | 加 worker owner 与它的 no-op projection（`mod.rs:282-302`） |
| `stop_session` 在 Claude Planner 模块里，ACP 另有私有版本 | `server/claude_planner/lifecycle.rs:195-222`；`server/acp_planner/session.rs:424-457` | 移进 `provider_execution` 作为唯一的“Stop → 等结算 → 无驱动者则 finish”入口。Claude Planner 与 worker 共用；ACP 是否并入，单独核对它不 finish 的原因后再定 |
| `admit` 自己开事务 | `mod.rs:197` | 接受调用者的事务条件（准入授权），与 op claim 校验、`engine.admit` 同事务 |
| 释放事务只看 execution/Observing/Held | `release.rs:13-29` | **接受调用者的释放授权**：在同一个 Held→Releasing 写事务里先运行调用者条件，失败即同事务写 Stopping{ReleaseRefused} 与 Stop，不发字节。Planner 传空授权，字节不变。先例：`gate_execution.rs:151-183` |
| boot pass A 的 session 集合只含 Planner | `truth/db/sqlite/session_row.rs:128-133`（`contract IN ('planner','assistant')`） | worker owner 自己的 revoke + boot 通道（§3.6；只停，不写行） |
| 驱动者丢失时的完成依赖 harness | `server/claude_planner/execution.rs:199-265` | worker owner 的 `complete` 只做 turn 级收尾：以精确 execution 所有权为条件终结 turn 行，按 §3.6 写或不写续跑行；不触 harness，不准入，不写 task |

**共享协议驱动的接缝**（r1 boundary N1）。今天的驱动不只 argv 与 `init_check` 是 Planner 专用：`drive` 接受 `Arc<session::Shared>`
（`server/claude_planner/driver.rs:186`），从 Planner host 取 owner（`:200-209`），把事件发成 `PlannerEvent`（`:402-415`），
settle 构造带 `Shared`/`TurnSlot`/`Approvals` 的 `KnownEnd::ClaudeTurn`（`:589-721`；`execution.rs:50-61`）；`Shared` 还持有 harness
安装与 held-request 状态（`server/claude_planner/session.rs:89-100`）。WA1 先把驱动拆成“协议循环 + 角色化 turn sink”：协议循环只负责解码、
`init_check`、`result`、中断、stdin 关闭与结算；sink 是一个 trait，提供事件出口、turn 槽与完成 `End` 的构造。Planner sink 包装今天的
`Shared`/`TurnSlot`/`Approvals`，字节与行为不变；worker sink 只记录 turn 结果与会话投影，不引入 carrier、conversation 或 approvals。
WA2 只实现 worker sink，不复制 drive/settle，也不把 worker 政策塞进 Planner 的 `Shared`。

### 3.3 准入、凭据与 task 围栏

**授权条件（worker turn authority）**，由 task/worker 控制层拥有，不放进通用 owner 层。一个写事务内依次检查：

1. 发行 op 的 live claim：`require_live_claim_tx`（含 lease 未过期与 `completed_at_ms IS NULL`，`server/operation/repo_sqlite/writes.rs:109-117`）。
   发行 op 总是 §3.6 的 `claude-worker-turn` 行，首 turn 也是；spawn op 不准入 execution。
2. `require_attempt_startable_tx(task)`（`server/task_recovery.rs:13-36`）。
3. **worker 写者条件**（新增，同类缺陷的补全）：task 状态只能是 Dispatched 或 Running。`require_attempt_startable_tx` 也接受 Verifying
   （`task_recovery.rs:26-29`），因为 gate 需要它；但 gated 自报在同一事务里把 task 转为 Verifying 并释放租约（`server/decision_sink.rs:152-166`、`:199-204`），
   此后再起一个 worker 写者 turn 就会在代提交之后写 worktree。故 worker 在通用守卫之外再排除 Verifying。
4. 绑定：task 属于本 card，即 `task.worker_card_id` 等于本 card，或者为空且本 card 是该 task 的 worker-spawn op 的目标——后者是既有的
   未 stamp 窗口所有权证明 `worker_op_targets_card_tx`（`truth/db/sqlite/task.rs:312-330`；`complete_terminal_task` 已这样用，`server/scheduler/mod.rs:2190-2197`）。
   单独一个“为空”不算绑定（r2 adversarial B2 的同类补全）；新准备形状的 spawn op 因此保持 kind `claude-worker`、card target 与 KernelDispatcher actor（`task.rs:332-336`）。
   card 当前 session 等于本 session 且未终结（SQL `admit` 已查，`provider_execution.rs:168-173`，释放事务须重查）；card 最新租约处于持有状态
   （`latest_workspace_lease_for_card_tx`，`server/operation/workspace_lease/facts.rs:50-63`）。§3.7 的收尾谓词用同一个绑定判定。

这条授权在**两处**运行，两处都必须过：

- **准入事务**：`ProviderExecution::admit` 接受调用者条件（今天它自己开事务，`mod.rs:197`），与 SQL `admit`、`engine.admit` 同事务。
  失败则什么都不准入，不 mint token。token 的 mint 放在准入事务内、授权条件之后；mint 从不代替授权（同起点设计 workspace-next-steps §3.3 的释放授权围栏条目）。
- **释放事务**（修 r1 两路 B1）：`release::send` 先等 Spawned（`release.rs:13`），再开写事务。今天这个事务只核对同一 execution、Observing 与 Held
  （`:16-23`）。准入提交之后、等 Spawned 期间，op lease 可能过期、attempt 可能被取代或取消（`cancel_running.rs:114` 只改 task 并留清理标记，
  停止异步发生），旧驱动仍会把 Held 改成 Releasing 并发出 token 与代理值。所以把上面的授权条件组合进同一个 Held→Releasing 写事务；
  失败则同事务写 Stopping{ReleaseRefused} 与 Engine Stop，提交后不写任何字节。这与 gate 的 `transition(…, Some(ReleaseAuthority::Task))` 同形
  （`server/operation/gate_execution.rs:151-183`、`:196-210`），task 政策仍由 `task_launch`/`task_recovery` 与 worker 控制层持有。
- **Releasing→Released**：仍是精确 claim 的 CAS（`release.rs:55-59`）。失去所有权的驱动者停止写入、留下 Releasing。
  任何后继（boot、sweep）看到 Releasing 都按自己的原因（`KernelRestarted`、`DriverLost`）写 Stopping 与 Stop，永不重发：`Release` 没有回到 Held 的转移（`truth/db/sqlite/provider_execution.rs:34-39`），
  Releasing 形状本身就是“输入未送达”的持久证据（§3.6）。起点设计用的 `ReleaseInterrupted` 不是今天 `StopReason` 的变体（`:40-52`），本设计不新增它。
- **授权事务先赢时的语义**：授权提交之后才发生的取消、取代、自报或 lease 过期，不撤回已经授权的这一次释放：载荷写给它在授权时刻已被允许启动的那个进程。
  随后的停止由各自既有路径负责：取消与超时经清理标记 → 端口 `stop` → `stop_session`；自报之后 turn 自然在 `result` 结束，未结束则 S4 端口 `stop`。
  不会有第二个进程，也不会有第二次授权：后续 turn 行在它自己的授权事务里被拒（§3.6）。
- 同类路径一并覆盖：首 turn、消息 turn、续跑 turn、手动“继续”，以及 owner `complete` 写续跑行的条件（§3.6，以同一授权的 task 一侧条件为前提），
  全部经同一个授权函数，没有旁路。

其余：

- worker session 不绑定终端（`terminal_run_id` 为 NULL），否则 SQL `admit` 会拒绝（`provider_execution.rs:147-157`）。这要求新的准备形状不建终端行（§10）。
- 每个 turn 都经 stdin 载荷传 token：`mcp.json` 不用改，worker 今天已写 Planner 同一个 `${NEIGE_MCP_TOKEN}` 形式的配置（`worker_mcp.rs:47-50`）；
  改变的是 token 的传输与驻留：从 EnsureProc env（`worker_mcp.rs:57-63`）改为 Held 载荷。载荷解码后 CLI、shim 与 CLI 的子进程仍经环境拿到 token，
  所以“只在载荷里”是对控制面与持久记录的约束：operations 与 terminal 的 env、execution 持久 secrets、OutputLog、stderr 捕获与 argv 都不含明文（r1 adversarial N2）。
  用户 hooks 与 plugins 继承 CLI 环境是 OQ4 的既有信任面，不在此约束内。
- 秘密代理值进载荷（`server/execution_environment.rs:20-31`）；PTY 字节保真握手（原 Wd）不再需要。
- Q1（用户已决定：无原生回退）：Engine 不可用时 `provider_execution::engine` 返回 `ExecutionUnavailable`（`server/provider_execution/mod.rs:91-95`）。
  **新 attempt** 须在 mint token 与取得租约**之前**检查，失败即让 dispatch 失败，不回退 Legacy。这个检查只在新准备形状的入口；
  升级前的 Legacy 行的 attach、消息、取消与清理不经过它（r1 adversarial N3，§10）。

### 3.4 共享驱动的 worker 档

`spawn::argv` 今天只服务 Planner：

- 工具集写死：`--tools Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch`（`server/claude_planner/spawn.rs:20`、`:226-228`）；
- 只读项目设置：`--setting-sources project`（`:221`）；
- 禁用 slash 命令（`:220-225`）；
- 权限三档（`:233-250`）；
- `init_check` 拒绝 skills 与非内置 plugin（`driver.rs:157-172`）。

worker 档是一份**类型化的角色档**（Planner 档的字节由 golden 钉住不变），协议循环共用（§3.2 的接缝）。worker 档的初值照今天 worker 的命令行
（`claude_adapter/mod.rs:383-408`）：

- 权限：`--permission-mode auto`；
- 工具：不限工具集，加 `--allowedTools mcp__neige`；
- 设置：`--settings` 指向带 16 个 hook 的设置文件（`server/routes/claude_cards.rs:223-315`）；
- 系统提示：`--append-system-prompt`；
- 不加 `--setting-sources`（OQ4），不加 `--disable-slash-commands`；
- `init_check` 只检查 session、中断能力与 neige MCP；版本见 §3.4.1。

#### 3.4.1 登录凭据、用户配置与环境的来源（修 r1 boundary B4）

共用 Planner 的驱动**不**共用 Planner 的登录与配置。两者今天的来源不同：

| | Claude Planner | Legacy claude-worker |
|---|---|---|
| 二进制 | 配置文件里的固定版本二进制 `claude_binary`，要求 `--version` 等于 `claude_version`（`claude_planner/config.rs:22-32`） | `resolve_claude_bin(&codex.claude_bin)`（`claude_adapter/mod.rs:319`；`worker_preparation.rs:41`），无版本校验 |
| 配置与登录 | `CLAUDE_CONFIG_DIR` = 专用目录，owner 在其中 `/login`（`config.rs:30-31`；`spawn.rs:304-307`） | 不设 `CLAUDE_CONFIG_DIR`（`claude_adapter/mod.rs:351-381`）；登录、用户 settings、skills、plugins 来自 HOME |
| 环境来源 | kernel 环境的 `passthrough_keys`（`spawn.rs:260-267` 过滤 `provider/codex/shared/environment.rs:8-43`）：HOME、USER、SHELL、LANG、TZ、TMPDIR、`NO_PROXY`/`ALL_PROXY`、`SSL_CERT_FILE` 等 | supervisor 环境的白名单（`child_environment.rs:5-49`），另含 `SSH_AUTH_SOCK`、`XDG_*`、`DISPLAY`、`SSL_CERT_DIR`、`CURL_CA_BUNDLE`、`NODE_EXTRA_CA_CERTS` 等 Planner 没有的键 |
| 部署前提 | 无 `--claude-planner-config` 时 Claude Planner 不可用（`config.rs:22`） | 无此前提 |

只把 `--setting-sources` 去掉，并不能让 worker 继续用旧的登录与设置：若 worker 档沿用 `base_env`，`CLAUDE_CONFIG_DIR` 会把配置指向 Planner 的
专用目录〔待实测 E9：`CLAUDE_CONFIG_DIR` 对登录、settings、skills、plugins 的实际影响范围〕。因此 worker 档带一份 typed 的启动合同：

- `binary`：沿用今天的 `claude_bin` 解析（`claude_adapter/mod.rs:319`），不依赖 Claude Planner 配置。`init_check` 把 `init` 报告的版本记进 turn 记录，
  不要求等于 Planner 的固定版本；是否也给 worker 加版本固定是独立决定（K4）。
- `config_source`：闭合枚举 `UserHome | Dedicated(path)`，由 worker 档的类型化配置给出，不从环境推断。推荐默认 `UserHome`：
  环境里**不设** `CLAUDE_CONFIG_DIR`，登录与用户配置照今天经 HOME，不需要额外 `/login`；`-p` 下是否沿用 TUI 的同一登录〔待实测 E9〕。
  选 `Dedicated` 则要求 owner 在该目录登录，并且 OQ4 的“保留用户设置”不再成立，须改写 OQ4 与升级说明。列为 **OQ10**。
- `environment`：worker 档的环境白名单取 supervisor 的同一张表（`child_environment.rs:5-49`，改为可复用的单一来源），加 `build_claude_env` 的
  worker 键（`claude_adapter/mod.rs:351-381`），值取自 kernel 环境；Planner 的 `base_env` 不变。kernel 与 supervisor 的环境是否同源由实现核对，
  差异写进升级说明（与起点设计 Wa 的环境对等同一问题）。
- 只有 Codex Planner 的部署：worker 不读 Claude Planner 配置，所以不受影响；仍受 OQ8（Engine）约束。

验收：`w_claude_worker_env_uses_user_config`（`UserHome` 下环境无 `CLAUDE_CONFIG_DIR`、含 HOME 与白名单键）、
`w_claude_worker_env_matches_legacy_allowlist`、`w_planner_env_unchanged`（Planner 环境 golden 不变）、
`w_claude_worker_dedicated_config_sets_dir`（`Dedicated` 时只设该目录）。

### 3.5 停止与结算

- **停止**：S3、S4 与 Track 删除都调用共享 `stop_session`。Track/Area 删除走 `execution_quiesce`（`server/execution_quiesce.rs:42-77`），
  它直接请求 Engine Stop；owner 的 sweep 随后对已结算的 claim 做 `finish`（`sweep.rs:47-57`），与 Planner 相同。
- **session 状态写入的顺序**：claim 存在时，session 的终态写入被 `require_unclaimed` 拒绝（`truth/db/sqlite/session_row.rs:629`、`:677-679`）。
  所以 `fail_running_worker_card` 的“标 Failed”（`server/operation/driver.rs:403-406`）必须排在 `stop_session` 完成之后。失败时保留清理标记重试，与今天 Codex 中断失败的处理一致（`codex_worker.rs:228-231`）。
  supervisor 丢失后的 Unknown 不在“重试必然收敛”之内：Engine 对 Unknown 一直 Wait（`crates/workspace-runtime/src/execution/mod.rs:117-132`），
  claim、租约与占用按 Q2 保留到宿主重启（见 K6）。
- **自报后结束**：今天靠 60s 宽限给“报告 turn”收尾（`server/terminal_sweeper.rs:73`）。无界面 turn 在 `result` 后自然结算，宽限只在 turn 还没结束时需要。

### 3.6 turn 发行：一种持久行、每行一个消费点（修 r1 两路 B2、r2 adversarial B1）

**问题**。第一版让“旧 execution 完成时由 worker 会话端口以 `--resume` 发续跑 turn”。但 `finish` 先 `await owner.complete`（`mod.rs:246-256`），
返回之后才清 claim 并 retire（`:257-273`），而 SQL `admit` 要求 claim 为空（`provider_execution.rs:168-173`）、`admission_gate` 也拒绝旧 claim（`mod.rs:134-147`）。
在 `complete` 里同步发起会被拒；只排一个内存任务等 claim 清除，则崩溃后续跑永久丢失。§3.1 的消息同理：今天的消息投递是一次性写入，结果可以是 `Unknown`（`message.rs:309-313`）。

第二版改为“原因同事务写意图 + boot 另写 `restart:{execution}` 意图”，r2 adversarial B1 指出它有两个发行责任人：原意图 I 已准入 E 却未成功时崩溃，
boot 又为 E 写出 J，I 与 J 都是未终结 op，都会被 `recover_on_boot` 重驱（`server/operation/driver.rs:588-623`）；TxCommitted 的重驱会再跑副作用
（`:784-824`），claim 清除后也不再保留发行权（`server/provider_execution/mod.rs:257-273`）。spawn op 作为首 turn 的发行者是第三种行，同样没有交接规则。
这类竞态来自“同一个 turn 的责任可以落在多行上”，所以第三版不再加交接机制，而是收窄形状：**只有一种发行行，boot 不写行，每行只有一个消费点。**

**行**。kind `claude-worker-turn` 的 keyed `operations` 行，归 task/worker 控制层，不归通用 owner 层。idempotency key 为 `claude-worker-turn:{session}:{cause}`，
`cause` ∈ `first` | `message:{message_id}` | `manual:{request_key}` | `continue:{interrupted_execution_id}`。keyed 行是永久的，同 key 的重放直接返回原行
（`server/operation/driver.rs:190-199`），boot 时由 `recover_on_boot` 重驱（`:588-625`）。今天只有独立事务的插入（`server/operation/repo_sqlite.rs:162`），
WA2 要加一个在调用者事务内插入 keyed 行的变体。选它而不是新表：重驱、租约 claim、幂等、parked 与 boot 恢复都已存在，不加迁移；若实施时发现 op 相位不合用，
改为新表须按持久层边界单独 L2。

**谁写行**（都与原因同事务）：

- 首 turn：spawn op 的准备事务在建 card、session（`terminal_run_id` 为 NULL）与租约的同一事务里写 `first` 行。spawn op **不准入 execution**，副作用为空，
  直接成功，scheduler 照旧 stamp（`server/scheduler/mod.rs:1428-1441`）。所以从 session 存在的那一刻起，就有一条未终结的发行行（§3.7 依赖这一点）；
- 消息：在 `message` 持久化消息记录的同一事务里写 `message:{id}` 行（消息 id 与 attempt 在准入时已确定，`message.rs:265-277`）。无界面 session 不再走粘贴写入；
- 手动“继续”：route 以客户端 idempotency key 提交 `manual:{key}` 行；
- 续跑：只由**记录了被中断 execution 的那一行**在它自己的终结事务里写 `continue:{E}` 行（见“消费点”）。boot 与 sweep 不写行。

**一行的生命周期**（沿用 driver 既有相位：Pending → TxCommitted → SpawnStarted → Parked，`driver.rs:695-760`、`:928-976`）：

1. **Pending**，在 `preparation_track` 的 track 围栏下（`driver.rs:701-718`），与 C1 的 clean 串行。`prepare_external` 调 `admission_gate`（`mod.rs:125-148`）：
   旧 claim 已结算且无驱动者时替它 `finish`；旧 claim 尚未结算则本次不准入，沿用 driver 的“有限重试后留在 Pending、lease 过期后重驱”（`driver.rs:720-722`），不新建轮询。
2. **准入事务**（op 的 `prepare_tx`，Pending→TxCommitted 的那一次 CAS）：§3.3 的授权 + SQL `admit` + `engine.admit`，claim 的 `Purpose::WorkerTurn{turn_id, intent_key}`
   记下本行的 key，`tx_output` 记下 execution id E。一行**至多准入一个 execution**：相位只前进，准入只发生在这一次 CAS 里。授权失败则行以 `refused` 终结，不准入、不发字节。
3. **副作用**（SpawnStarted）：只有执行了第 2 步的那次驱动调用持有 E 的驱动者 guard（`ProviderDrivers::claim`，`mod.rs:52-60`、`:196`），它把 guard 带进同一调用的副作用步，
   做释放事务（§3.3）并驱动 E，然后返回 `Parked`。任何重驱调用（boot 后、或 op lease 过期后的再次 claim）都没有 guard，只返回 `Parked`，不对 E 做任何 I/O，
   不再准入，也不接管（与 `mod.rs:214` 的“绝不接管”一致）。失去驱动者的 E 由 owner 层既有路径停止：boot 的 KernelRestarted、sweep 的 DriverLost（`sweep.rs:47-57`、`:94-118`）。
4. **Parked**：行声明自有 parked 资源（`owns_parked_resource`，`server/operation/mod.rs:832-836`，同 gate 先例 `task_gate_run/mod.rs:322`）。它的 `recover_owned_parked`
   （`operation/mod.rs:846-857`）只读 SQL：E 的 claim 仍在就 `LeaveParked`。行不自己完成，也不停 E。parked deadline 是 driver 合同要求的字段（缺失即失败，`driver.rs:1226-1236`），
   取 task 的运行上限；turn 的实际期限由 task 的空闲/上限政策经端口 `stop` 执行。

**消费点（唯一）**：worker owner 的 `complete(session, record, evidence, known)`（端口 `mod.rs:35-48`）。它在一个写事务里，以精确 execution 所有权为条件（claim 仍是 E，
端口注释 `mod.rs:38-40`），用 `complete_parked_tx`（`server/operation/repo_sqlite/writes.rs:366-391`）把 `record` 中 `intent_key` 指向的行 I 从 Parked 终结，
并且**只在这次终结实际发生的同一事务里**按下面的规则写或不写 `continue:{E}`：

- `known` 带 `result`（turn 正常结束）：I 成功，不写续跑；未自报照 §3.1 唤醒 Planner；
- 无 `result`，且 claim 的停止原因是 kernel 一侧的中断——`KernelRestarted`、`Shutdown`、`DriverLost`，或 `ReleaseRefused`（`StopReason`，`truth/db/sqlite/provider_execution.rs:40-52`）——
  并且 §3.3 授权的 task 一侧条件（第 2–4 条）此刻仍成立：I 以 `interrupted` 终结，同事务写 `continue:{E}`。`ReleaseRefused` 而 task 一侧仍成立，说明拒绝只来自 op lease，输入未送达；
- 其余（用户中断 `Interrupted`、`TrackRetired`、`SetupFailed`、`OutputGap`、`StdinTorn`、无停止原因的自然退出，或 task 一侧授权已不成立）：I 以对应结果终结，不写续跑；
  turn 失败而 task 未自报时唤醒 Planner。确定性失败不续跑，避免循环。
- `complete_parked_tx` 对非 Parked 的行一律返回 `AlreadyResolved{phase}`（`writes.rs:379-383`），`complete` 按相位区分：终态相位是重复完成（`finish` 在清 claim 前失败后重试，
  `mod.rs:246-273`），什么都不写；非终态相位（例如 boot 后行还在 TxCommitted、`recover_on_boot` 还没把它推进到 Parked）则 `complete` 返回冲突，`finish` 保留 claim，
  下一拍重试（`mod.rs:246-256` 的 `??`），直到行被重驱到 Parked。

**为什么只续跑一次**：每行至多准入一个 execution（第 2 步的单次 CAS）；每个 execution 的 claim 只记录一行的 key；该行只在 `complete_parked_tx` 的 Parked→终态那一次事务里终结，
续跑行只在那次事务里写，键又是 E，重放返回原行（`driver.rs:190-199`）。boot 与 sweep 不写行。所以 I 的责任与 J 的产生是同一个提交，任何时刻一个 turn 的责任只在一行上。
`complete` 先于清 claim（`mod.rs:246-273`），而 claim 只经 `finish` 清除，所以“claim 已清而 I 仍 Parked”不会出现。

**续跑输入按 E 的持久释放形状决定**（r1 adversarial B2 的后半）：

- `Held` 或 `Releasing`：首条 user line 只在 Released 之后写入（`server/claude_planner/session/launch.rs:314-339`），输入一定未送达。J 重发 I 的**同一输入**；
  Releasing 的后继只按自己的原因写 Stop，`Release` 状态不能回到 Held，故永不重发凭据（`provider_execution.rs:34-39`）；
- `Released`：输入可能已送达。J 发固定的“kernel 已重启，继续当前任务”文本；
- 用 `--resume` 还是 `--session-id`：只有当 worker owner 已持久记录该 Claude session 由 `init` 报告（`driver.rs:137-142`，外部工具报告的值）时才 `--resume`；
  否则 J 重发首 turn，仍用原 `--session-id`。一个从未收到输入就被停掉的 `--session-id` 能否再次使用〔待实测 E8〕；E8 不成立则改用新的 session id，并在 J 的准入事务里更新 card 的 `claude_session_id`。

**崩溃点逐一**（全部经生产入口：op driver 的 `recover_on_boot`、worker owner 的 boot 通道与 owner 层 sweep）：

| 崩溃点 | 持久状态 | 恢复 | 结果 |
|---|---|---|---|
| 准入事务提交前 | 行 Pending | 重驱第 1–2 步 | 一次准入 |
| 准入后、SpawnStarted 前 | 行 TxCommitted，E Held | 重驱只 park；boot 停 E；sweep `finish` → `complete`（行未 Parked 则重试） | 一条 `continue:{E}`，同一输入 |
| 释放中（Releasing） | 行 SpawnStarted/Parked，E Releasing | 同上 | 一条续跑，同一输入，不重发凭据 |
| Released 后、`result` 前 | 行 Parked，E Released | 同上 | 一条续跑，固定续跑文本 |
| `result` 后、`complete` 前 | E 已结算 | sweep `finish` → `complete` | I 成功，无续跑 |
| `complete` 提交后、清 claim 前 | I 终结，J Pending | sweep 再 `finish`：`AlreadyResolved`，无写；清 claim；J 重驱经 `admission_gate` | J 一次准入 |
| 重复 boot | 同上任一 | boot 只做 Observing→Stopping CAS，不写行 | 不增加行 |

- **取消/取代/自报与待发行行竞争**：任何一方先提交，另一方在自己的授权事务里看到结果；行后到则被拒，取消后到则走既有停止路径（§3.3 的“先赢”语义）；
  被取消停止的 E 在消费点不满足 task 一侧授权，不续跑。
- **消息合并**：消息是拉取模型（worker 用 `neige_message_read` 读全部记录，`message.rs:25-29`），重复通知无害。准入事务可把同 session 已排队的 `message:*` 行
  一并标为由本 turn 消费，减少空 turn；不改变正确性。
- **与 C1/C5 的关系**（r1 adversarial C5 备注）：待发行的行不是 execution，`unsettled_executions(Track)` 为空不等于没有待发行的写者。
  C1 的 clean 写者复查须把 Track 上未终结的 `claude-worker-turn` 行视为待发行（与 Planner 的持久 Issuing 相同角色），发行又在同一 track 围栏下运行；
  验收见 §7 的 `w_turn_row_vs_clean_*`。

**kernel 重启的两种处置**（OQ1）：

- **(a) 停止 + 续跑行（推荐首片）**：上面的设计。半截工具调用的副作用由 worker 自己在续跑中处理（kernel 代提交，worker 不提交，`task_gate_run/checkpoint.rs:32-62`）。
  代价是重启时丢掉进行中的那一次工具调用。今天的 Legacy claude-worker 活过 kernel 重启；Codex 线程由 daemon 接管（`server/shared_codex_appserver.rs:1-2`），同样活过。
- **(b) 同 consumer 的重新接管**：boot 时 owner 为自己的 Observing claim 重新登记驱动者，从 Engine 输出日志的游标继续读（`stdio.rs:76-155`），
  stdin 仍由 executor 持有（`Engine::interact`）。这与 `stop` 注释里“绝不接管别的 execution”（`mod.rs:214`）不同，是新的 owner 层能力，须单独 L2 评审（WA4）。

### 3.7 收尾分派与租约兜底（修 r1 boundary B3 / WR4，r2 boundary B1、adversarial B2）

第一版把 WR4 判为“不触发”，理由是 `converge_dead_worker` 不变。但正因为无界面 session 不进探测路径，新 session 就不再经过它的兜底：
`converge_dead_worker` 对无 spawn op、无 task key、无 track 的已死 worker 释放 card 租约（`server/reaper/mod.rs:381-391`），task CAS 竞争失败时也释放
（`:445-450`，经 `:455-468`）；而通用 `finish` 只完成 owner、清 claim、retire（`mod.rs:234-278`），没有 task 或租约动作。

**今天的分派器本身不满足 WR1**（r2 boundary B1，核实成立）。reaper 在事务外读出 session 快照（`sessions_nonterminal`，`reaper/mod.rs:103`）、探测并解释之后才调
`converge_dead_worker`（`:276-283`）。分派器先以 `TaskReporter::Kernel` 失败 task（`:406-413`），Kernel 绕过 card 守卫（`truth/db/sqlite/task.rs:372-384`、`:655-668`），
条件只有 task id、track 与 Dispatched/Running；同一事务再按 card 释放“最新的活租约”（`:431-438`；`workspace_lease/release.rs:56-68` 按 card 选行）。
缺绑定的三种情形与竞争失败分支则在事务外释放（`:381-391`、`:445-450`）。交错：reaper 拿到旧 session S1 的死亡结果 → 暂停 → Legacy `claude-restart`
在同一 card 上终结 S1 并启动 S2（`server/operation/claude_restart_adapter.rs:248-254`、`:310-327`；`session_start_runtime_tx` 改指 `cards.session_id`，
`truth/db/sqlite/session_mirror.rs:318-342`）→ 旧结果进入分派 → 仍 Running 的同一 task 被标 Failed，S2 共用的 card 租约被释放。第二版只在租约释放上加围栏，
挡不住前面的 task 写，也挡不住事务外的兜底。

**设计：一个分派器、一个写事务、围栏先于一切写。** `converge_dead_worker` 改为在单个写事务内完成全部判定，Legacy 探测臂与无界面 SQL 臂共用：

1. **当前性围栏**（先于任何写）：本 session 未终结，且（session 有 card 时）`cards.session_id` 仍是本 session。不成立即 **Stale**：不写 task、不释放租约、
   不进入任何兜底，直接返回。Legacy 探测臂随后的 `session_commit_exit` 对已被重启终结的行本就是 Absorbed（`reaper/mod.rs:296-322`）。
2. **在同一事务内解析绑定**（spawn op、task key、track）。任一缺失 → 在本事务内 `release_workspace_lease_for_card_tx(…, CommitAsTaskEnded)`
   （`workspace_lease/release.rs:56`），删去事务外的 `release_reaped_worker_workspace_lease`（`reaper/mod.rs:455-468`）。Legacy 重启出的 session
   没有 spawn op（`claude_restart_adapter.rs:324`），它自己作为当前 session 死亡时仍走这一支，`reaper/tests.rs:670-728` 钉住的行为不变。
3. **按入口分支**：
   - **探测臂**（Legacy，已正面确认死亡）：task 失败写改用 card 的精确所有权 `TaskReporter::Card{card, owns_key: worker_op_targets_card_tx(..)}`，
     与 `complete_terminal_task` 同形（`server/scheduler/mod.rs:2190-2197`），不再用 Kernel；与 `Commit(Failed)` 的租约释放同事务。rows == 0（task 已终结或不属于本 card）
     → 同一事务内 `CommitAsTaskEnded` 释放本 card 的租约，取代今天事务外的竞争失败兜底（`:445-450`）。
   - **SQL 臂**（无界面）：在事务内重算下面的“会话结束”谓词；不成立 → **NotEnded**，无写返回。成立 → 只做租约收敛（`CommitAsTaskEnded`），不写 task：
     按谓词，task 已不是本 card 的活绑定 attempt，没有可失败的对象。
4. 只有结果为 Applied 时，调用者才终结 session（`require_unclaimed`，`session_row.rs:629`）。Stale 与 NotEnded 不终结。

**会话结束谓词**（无界面；同一个绑定判定也是 §3.3 授权的第 4 条）：下列持久事实同时成立：

- claim 为空；
- 本 session 没有未终结的 `claude-worker-turn` 行。首 turn 行与 session 同事务写入（§3.6），续跑行与被中断行的终结同事务写入，所以“准备已提交、首 turn 尚未准入”
  与“turn 被中断、续跑尚未准入”都不会让这一条变真（r2 adversarial B2 的第一个窗口）；
- 并且绑定失效：spawn op、task key 或 track 无法解析；或者 task 不是 Dispatched/Running；或者 task 不属于本 card。“属于”= `worker_card_id` 等于本 card，
  **或者为空且本 card 是 worker-spawn op 的目标**（既有证明 `worker_op_targets_card_tx`，`truth/db/sqlite/task.rs:312-330`）。所以首个 turn 已经完成、
  scheduler 尚未 stamp（`scheduler/mod.rs:1428-1441`）的窗口仍算绑定有效，不关闭、不失败 task、不释放租约（r2 adversarial B2 的第二个窗口）；
- 当前性围栏成立（不成立是 Stale，不是“结束”）。

task 仍 Running 而 worker 空闲（turn 结束未自报）不是结束，留给空闲期限（OQ5）。Verifying 时租约已在自报事务中释放（`decision_sink.rs:199-204`），收敛是无操作。

**发现**：reaper 既有的 30s tick 加一个 SQL 臂，按谓词列出候选无界面 claude-worker session；不探测 supervisor。发现只是提示，判定在分派器的写事务内重做。

**同类路径扫查**（r2 的修缺陷类要求：所有写 task 或租约的收尾路径）：

| 路径 | task/租约写的条件 | 结论 |
|---|---|---|
| `converge_dead_worker` 探测臂与 SQL 臂 | 本节第 1–3 步 | 本次修订 |
| 空闲/上限超时 `fail_task_liveness_timeout` | 按 task 判定，事务内重查 liveness 事实，竞争失败无写（`server/scheduler/running_worker.rs:217-320`）；租约经清理标记，同事务 | 是 task 自身的政策决定而非过时的 session 观察，不受 session 取代影响；不改 |
| 自报 `commit_worker_task_report` | card 守卫 + task CAS + 租约释放同事务（`decision_sink.rs:152-166`、`:199-204`） | 不改 |
| 取消 `cancel_running_in_tx` | 按 card 的 task CAS 加清理标记（`cancel_running.rs:102-140`） | 不改 |
| 清理执行 `fail_running_worker_card` | 只写 session 终态，不写 task 与租约（`server/operation/driver.rs:366-408`）；须排在 `stop_session` 之后（§3.5） | 不改 |
| owner `complete`（worker） | 不写 task，不动租约；只终结 turn 行、按 §3.6 写续跑行，以精确 execution 所有权为条件 | 新增，已带围栏 |
| WT 的 parked 完成 → `complete_terminal_task` | card 守卫 + op 目标证明（`scheduler/mod.rs:2190-2197`），在 op 的精确 parked claim 下 | 不改 |

发行入口的同类扫查见 §3.6：首 turn、消息、手动继续与续跑都是同一种行，都经同一个准入事务与授权；spawn op 与 boot 都不准入、不写发行行以外的东西。

验收（全部经 reaper 的真实入口 `sweep_all` 或新 SQL 臂进入，不直接调 helper）：

- WR1 交错（r2 boundary B1）：`w_reaper_stale_probe_after_legacy_restart_is_noop`——经真实 reaper 分派，在探测之后的新暂停点停住，运行真实 Legacy `claude-restart`，
  再放行旧结果；断言 task 状态、S2 与它的租约、事件流都不变（无 `TaskFailed`）。`w_reaper_stale_probe_after_attempt_replaced_spares_new_attempt`：新 attempt 的 task 与租约不变。
  `w_reaper_task_fail_requires_card_binding`：task 不属于本 card 时不写 task。`w_reaper_stale_binding_missing_releases_nothing`：缺绑定但 session 已被取代时不释放。
- WR4：`w_headless_closure_releases_lease_without_spawn_op`、`w_headless_closure_releases_lease_when_binding_missing`、`w_headless_closure_releases_lease_on_task_race_lost`、
  `w_headless_closure_spares_replacement_session`、`w_headless_idle_running_session_not_closed`、`w_headless_closure_waits_for_pending_turn_row`。
- 未 stamp 窗口（r2 adversarial B2）：`w_headless_closure_spares_unadmitted_first_turn`（准备提交后、首 turn 准入前暂停跨过一拍）、
  `w_headless_closure_spares_unstamped_after_fast_turn`（首 turn 完成、stamp 前暂停跨过一拍）；均断言不关闭、不写 task、不释放租约，随后准入或 stamp 照常。

### 3.8 进度与超时

- 1 小时空闲判定读 `worker_flow_cursors`（`truth/db/sqlite/task_liveness.rs:47-70`）。cursor 来自 hook 报告的 `transcript_path`，这是外部工具的文档化合同
  （`claude_transcript_lookup.rs:23-29`），kernel 不自己推导路径。
- 首片保留这条证据，只要 hook 在 `-p` 下照常触发〔待实测 E2〕，超时政策就字节不变。
- 改由 stream-json 事件喂进度是另一个政策变化，单独决定，不在本里程碑内。

### 3.9 UI 与用户介入

- 今天用户可以在 worker 卡上打开 TUI 并打字：
  - WebSocket 输入作用域是 `InteractiveUser`（`server/ws/terminal.rs:225`、`server/terminal_renderer/input_authority.rs:10-38`）；
  - 前端把 `onData` 作为 Input 发送（`fe/web/src/systems/terminal/xterm-view.tsx:706-710`）；
  - worker 卡渲染的是终端视图（`fe/web/src/systems/cards/builtins/claude-card.tsx:54`）。
- 无界面之后，卡片改为会话投影的只读视图，加一个“中断”控件（复用 Planner 的 `control_request` 中断，`server/claude_planner/session.rs:426-449`）。
- 用户能否给 worker 发消息（今天 message 只允许 Planner，`server/terminal_interaction/message.rs:218-224`）见 OQ2；若开放，它写的也是 §3.6 的 turn 行。

## 4. 路线 B：typed PTY execution（workspace-next-steps §3.3 的起点）

- **形状**：把三种 worker 都放进 E2b typed 终端。alias 用 `term:<id>`，复用 `TerminalProjection`（`server/operation/terminal_execution_create.rs:13-14`、`:217-241`），
  typed renderer 走 `attach_typed_terminal`（`server/routes/terminal.rs:187-220`）。
- **新机制**：
  - `terminal_launch` 的 `Admitted` 状态与释放授权围栏；
  - `TerminalObservation` 端口与 reaper 共享退出决策树（Wb）；
  - PTY 字节保真握手：raw 模式、布防/撤防标记、输入门（Wd）；
  - typed viewer（We）。
- **规模**：起点估计 Wa–We 合计 7750–11700 行（workspace-next-steps §3.3 切片表），且 WR1–WR5 尚未闭合。
- **保留的价值**：TUI 交互与用户打字、Claude 交互模式的全部能力、hook 与 transcript 依赖不变、kernel 重启不打断 worker。
- **固有问题**：
  - claude-worker 的进度与退出仍要从 PTY 推断：退出 0 而未自报只能交给期限，是 WR3 的根源；
  - 凭据要在 PTY 上做字节保真握手；
  - typed 终端重启要另设 alias 交接（WR3）；
  - worker 与 Planner 维持两套 Claude 驱动（TUI 与 stream-json）。
- r1 adversarial 的公平比较：B 不会自动解决释放授权、task 取代、租约与代提交顺序，同样要承担 §3.3 的授权事务；r1 的三项阻塞都能在 A 的 owner/task 框架内修补，
  不构成改选 B 的理由。

### 4.1 对比

| 维度 | A（无界面 + 共用驱动） | B（typed PTY） |
|---|---|---|
| 生命周期 owner | 复用 `provider_execution`，不新建 | 新的 worker 执行 owner、`Admitted` 记录、观察端口 |
| 凭据 | 既有 stdin 载荷 + 调用者释放授权 | 新 PTY 握手（含 8 条真实 PTY 测试与负面用例）+ 同样的释放授权 |
| 退出与进度 | `result` 事件 + SQL 结算；turn 结束精确 | PTY 退出 + 期限；退出 0 未自报只能等期限 |
| 重启 | 新 turn execution（`--resume`），经持久 turn 行，WR3 消失 | typed 重启的 alias 交接（WR3） |
| kernel 重启 | 默认中断 turn 并续跑（OQ1；(b) 可消除） | 不中断 |
| 用户介入 | 失去 TUI 打字（OQ2） | 保留 |
| Claude 交互能力 | 须逐项核对（§5.2） | 全部保留 |
| 登录与配置 | 须显式合同（§3.4.1） | 不变 |
| C5 | 由 `unsettled_executions` 覆盖；待发行的 turn 行由 C1 复查覆盖 | 同样覆盖（typed alias） |
| 与 Codex 对称 | 政策端口对称；viewer 不对称（§0.5） | viewer 层对称，业务层不对称 |
| 规模（粗估） | WA1–WA3 约 4300–6800 行（§7） | 7750–11700 行，外加 WR 处置 |

## 5. 失去与保留（问题 3）

### 5.1 失去与保留一览

| 项 | A 之后 | 证据 |
|---|---|---|
| 用户实时打字进 worker TUI | **失去**；改为只读会话视图 + 中断；用户消息通道待 OQ2 | `server/ws/terminal.rs:225`、`fe/web/src/systems/terminal/xterm-view.tsx:706-710` |
| Planner 消息 | **保留**：通知文本与拉取模型不变，传输从粘贴改为持久 turn 行 + user line；不再依赖 renderer 粘贴模式与 `UserPromptSubmit` 等待；投递不再有 `Unknown` | `server/terminal_interaction/message.rs:25-29`、`:96-143`、`:309-313` |
| Planner/watcher 发按键 | **失去**，也不再需要：按键只用于对话框与 Esc；Esc 由 `control_request` 中断取代 | `server/terminal_interaction/actions.rs:75-139`、`provider/worker/claude.rs:40` |
| transcript 与 hook | **保留**：hook 设置文件照传，transcript 路径仍取自 hook 载荷〔E2〕 | `server/routes/claude_cards.rs:223-315`、`claude_transcript_lookup.rs:103-151` |
| 进度与超时 | **更可靠**：turn 结束与进程结算来自协议和 SQL，不再用 `-1` 探测哨兵、pid 等待或 PTY 静默 | `provider/worker/supervisor.rs:35-40`、`server/operation/driver.rs:379-396`、`server/worker_quiet.rs:21-27` |
| 凭据 | **简化**：MCP token 与秘密代理走 Held stdin 载荷，不进 EnsureProc env，不要 PTY 握手 | `server/operation/claude_adapter/worker_mcp.rs:57-63` → `server/provider_execution/release.rs:4-68` |
| 登录与用户配置 | **保留**（OQ10 选 `UserHome`）：不设 `CLAUDE_CONFIG_DIR`，HOME 照旧；环境白名单与 Legacy 同源 | §3.4.1 |
| 手动 restart | **改形**：变成经 turn 行发行的 `--resume` 续跑 turn；不再有“子进程活着时拒绝”与 typed 终端重启问题 | `server/operation/claude_restart_adapter.rs:248-279` |
| kernel 重启连续性 | 默认**失去**（OQ1） | §3.6 |

### 5.2 Claude Code 交互模式能力逐项核对

“是否用到”一栏只说明 **Neige 的集成**是否依赖该能力；用户自己的 settings 或 TUI 习惯是否用到，源码无法证明（r1 boundary N2）。
CLI 自身有没有某种接口，也一律以外部合同或实测为准，不从 Neige 代码里没有对应路径推断。

| 能力 | Neige 集成是否用到 | 证据 | `-p` 下的处置 |
|---|---|---|---|
| 位置参数 prompt | 用 | `claude_adapter/mod.rs:405-406` | 首条 user line |
| bracketed paste 消息 | 用 | `message.rs:25-29`、`:65-74` | turn 行 + user line |
| 等 `UserPromptSubmit` 再投递 | 用 | `message.rs:96-122` | 不需要：首 turn 的 user line 就是 prompt |
| 目录信任对话框 + watcher 接受 | 用（watcher 可以按 Up/Down/Enter 接受） | `crates/calm-server/prompts/terminal/worker-quiet-watch.md:5` | Planner 的 `-p` 路径没有任何信任处理代码（`server/claude_planner/` 只有 `spawn.rs:45` 一处关于设置拒绝的注释）〔E3：确认 `-p` 不弹〕 |
| `--permission-mode auto` | 用 | `claude_adapter/mod.rs:266-269` | 〔E1：`-p` 下 auto 是否可用；会弹框的请求如何结束〕→ OQ3 |
| 权限弹框由人在 TUI 回答 | 间接用：watcher 报 `needs_owner`，人去 attach | `server/worker_watch/mod.rs:42-89` | 拒绝回给模型（`--permission-prompts none`，Planner 已在用，`spawn.rs:233-250`），或走 Planner 的 approvals 通道（`server/claude_planner/approvals.rs:1-10`）→ OQ3 |
| slash 命令 | kernel 不发；用户可以手打（worker 无 `--disable-slash-commands`） | `message.rs:23-24`；`claude_adapter/mod.rs:383-408` | 用户手打能力随 TUI 一起失去；自动压缩〔E4〕 |
| 按键（Esc、方向键、Ctrl） | Planner/watcher 可发（`BoundKeys::Accepted`） | `provider/worker/claude.rs:40`、`server/terminal_interaction/actions.rs:75-139` | 中断用 `control_request`；其余不需要 |
| plan mode、图片粘贴、statusLine | kernel 未集成；用户 settings 是否使用未知 | worker 路径中无相关代码；kernel 写的设置只有 hooks 与 attribution（`server/routes/claude_cards.rs:296-315`） | `UserHome` 下用户 settings 照常加载；交互专有的部分（图片粘贴、statusLine 显示）随 TUI 失去 |
| 接到运行中会话的客户端 | Neige 未集成 | Neige 的 resume 起新写者并拒绝活子进程（`claude_restart_adapter.rs:248-255`） | 不依赖；CLI 是否提供不影响 A |
| `claude_auto_resume` | 不适用于 task worker | `server/claude_auto_resume.rs:104-119` | 无 |
| 用户 settings、skills、plugins（经 HOME） | 用（worker 不传 `--setting-sources`，不设 `CLAUDE_CONFIG_DIR`） | `claude_adapter/mod.rs:351-408`；Planner 则限定 project 并拒绝 skills/plugins（`spawn.rs:220-222`、`driver.rs:157-172`） | worker 档默认保留今天的语义 → OQ4、OQ10、E9 |
| Stop hook 唤醒 Planner | 用 | `server/dispatcher/mod.rs:1663-1673` | 保留；turn 结束事件是同义的更强证据 |

### 5.3 实测清单（固定版本，专用宿主；结论写进 WA0 证据文档）

实测已完成：Claude Code CLI 2.1.280，本机隔离实测，证据见 [WA0 证据文档](task-worker-execution-wa0-evidence.md)
（命令、输入、输出摘要与限制逐项记录）。下面每项先写原问题，再写实测结论；本文其他位置的〔待实测〕以此处结论与限制为准。

- **E1**：`-p` 加 `--permission-mode auto` 是否被接受；会触发弹框的工具调用在 `--permission-prompts none` 下返回什么。
  **实测：支持，有效模式须检查 init。** Sonnet 的 init 报 `permissionMode=auto`；需要批准的 Bash 被拒绝回模型（`tool_result.is_error=true`，
  动作未执行），`result` 仍可为 success，拒绝记在 `permission_denials`。Haiku 接受 auto 参数，但 init 报 `permissionMode=default`。
  限制：只验证显式 ask 路径。[E1](task-worker-execution-wa0-evidence.md#e1auto-与无批准界面)
- **E2**：`--settings` 中的 16 个 hook 在 `-p` 下是否都触发，SessionStart 是否带 `transcript_path`。
  **实测：部分支持，16 个中 12 种实际触发；SessionStart 带 `transcript_path`；resume 时 SessionStart 记 `source=resume`。** PermissionDenied、Notification、
  Elicitation、StopFailure 未触发，不能依赖它们作为结算或拒绝信号。[E2](task-worker-execution-wa0-evidence.md#e216-个-hook-与-transcript_path)
- **E3**：在未被信任的 worktree 里，`-p` 是否跳过目录信任对话框。
  **实测：支持。** 新建的未信任 git 目录中 `-p` 直接完成，无信任确认；这不等于隔离不可信项目配置。[E3](task-worker-execution-wa0-evidence.md#e3未信任目录)
- **E4**：`-p` 的长 turn 是否会自动压缩，压缩后是否仍可 `--resume`。
  **实测：部分支持。** 手动 `/compact` 后新进程 `--resume` 成功并保留上下文；长 turn 自动压缩未验证，仍待测。[E4](task-worker-execution-wa0-evidence.md#e4压缩后恢复)
- **E5**：turn 中途被 Stop 之后，`--resume` 是否可以续跑，未完成的 tool_use 如何呈现。
  **实测：支持 control interrupt 与 SIGTERM 后的新进程 resume。** 两种停止都让未完成的 tool_use 与同 id 的错误 tool_result 配对；
  interrupt 有 `error_during_execution` result，SIGTERM 无 result。限制：未测 SIGKILL/断电下悬空的 tool_use。[E5](task-worker-execution-wa0-evidence.md#e5turn-中途停止后恢复)
- **E6**：在用户 settings 下，`init` 报告的 skills 与 plugins 是什么。
  **实测：用户 skills/plugins 被加载**，init 报 79 个 skills、7 个 plugins（含用户、插件与内置能力）。MCP 为空，不是 MCP 继承测试。
  [E6](task-worker-execution-wa0-evidence.md#e6用户-settings-下-init-的-skillsplugins)
- **E7**（用户输入 1）：stream-json 输入、stdin 保持打开时：进程是否在 `result` 之后继续等待下一条 user 消息；turn 进行中写入一条 user 消息，
  是排队到当前 turn 结束，还是在工具边界注入当前 turn；注入时 transcript 与 `result` 如何呈现。
  **实测：`result` 后进程继续等待下一输入；工具期间的消息在工具边界被当前 turn 吸收**，只产生一个 `result`，transcript 记为
  `queue-operation`/`queued_command`（`absorbed_mid_turn`），不是独立 user turn。限制：单次工具边界实测。[E7](task-worker-execution-wa0-evidence.md#e7常驻输入与-turn-中途消息)
- **E8**：以 `--session-id` 启动、尚未收到任何输入就被停掉的进程，其 session id 能否再次用于 `--session-id`；能否 `--resume`。
  **实测：同 ID 可再次 `--session-id`；首次 `--resume` 失败**（`No conversation found with session ID`）。[E8](task-worker-execution-wa0-evidence.md#e8没有输入的预分配-session)
- **E9**：不设与设置 `CLAUDE_CONFIG_DIR` 时，`-p` 使用的登录凭据、用户 settings、skills 与 plugins 分别来自哪里；与同一用户 TUI 的登录是否一致。
  **实测：凭据、settings、skills 与 plugins 随 `CLAUDE_CONFIG_DIR` 切换**；不设时来自默认配置根，与同用户 TUI 读同一凭据文件；
  指向空目录时未登录，不回退到用户凭据，仍有内置 skills。限制：仅本机 Linux 文件凭据后端。[E9](task-worker-execution-wa0-evidence.md#e9配置目录登录与-tui)

### 5.4 开放问题（须用户或 Planner 决定）

用户已确认方向（ask 686425：路线“确认，合并设计”；开放问题“全部按推荐”）。**已定（用户）**：路线 A（claude-worker 无界面，§3）；
terminal-worker 只用 Pipe execution（§5.5）；长时间或交互命令走 Workbench 常驻终端卡（OQ12）；codex-worker 的业务线程不动。
下列各项中标“已定（用户）”的按结论执行；未被问到的 OQ5、OQ6、OQ7、OQ9 保持开放，按推荐推进，由对应切片定。

- **OQ1 kernel 重启**（**已定（用户）：(a) 停止 + 续跑行**；(b) 留作可选的 WA4）：选 §3.6 的 (a) 停止 + 续跑行，还是 (b) 重新接管。推荐先 (a)，若 dogfood 部署频繁打断长任务再做 (b)。
  WA0 证据（不改结论）：E5 支持 control interrupt 与 SIGTERM 后以新进程 `--resume` 续跑，E4 支持手动压缩后 resume，(a) 方向可行；
  完成消息与进程退出须分别处理，resume 不保证原工具恰好执行一次。E8 表明只预分配 ID、未收到输入的会话不能 `--resume`，
  同 ID 再次 `--session-id` 则可用：续跑行要区分“仅预分配 ID”与“已持久化会话”（与 §3.6 首 turn 未 stamp 的窗口一起核对）。
  未验证：SIGKILL/断电、长 turn 自动压缩。见 [E4](task-worker-execution-wa0-evidence.md#e4压缩后恢复)、[E5](task-worker-execution-wa0-evidence.md#e5turn-中途停止后恢复)、[E8](task-worker-execution-wa0-evidence.md#e8没有输入的预分配-session)。
- **OQ2 用户介入**（**已定（用户）：先只给中断**，输入框另立需求）：是否接受失去 TUI 打字。可选做法：在会话视图加“发给 worker”输入框，经与 Planner 消息相同的绑定校验
  （`server/terminal_interaction/target.rs` 的 `resolve_target`/`check_binding`）写成一条 turn 行。推荐：先只给中断，输入框另立需求。
- **OQ3 权限请求**（**已定（用户）：拒绝回给模型**）：选拒绝回给模型（简单，与 Planner 的 `never` 一致），还是接入 approvals 通道让人批准（要求 worker 会话有 held-request 宿主，而它今天归 PlannerHarness）。推荐先拒绝。
  WA0 证据（不改结论）：E1 中需要批准的工具在 `--permission-prompts none` 下被拒绝回模型，动作未执行，进程不因此失败；
  拒绝记在 `permission_denials`，与 `result` 成功相互独立。init 报告的有效 `permissionMode` 可能不同于 argv（Haiku 报 `default`），
  须检查 init 而非只看参数。PermissionDenied hook 在该拒绝路径未出现（E2），不能把它当作拒绝信号。见 [E1](task-worker-execution-wa0-evidence.md#e1auto-与无批准界面)。
- **OQ4 用户设置**（**已定（用户）：保持加载，`UserHome`**）：worker 是否继续加载 HOME 下的 settings、skills 与 plugins（今天加载）。推荐保持，worker 档不套 Planner 的限制；前提是 OQ10 选 `UserHome`。
  WA0 证据（不改结论）：E6 中用户 settings 下 init 报 79 个 skills、7 个 plugins，worker 的 init 检查不能沿用 Planner 对非内置
  plugins/skills 的拒绝规则；E2 中 `--settings` 注册的 hook 被继承，SessionStart 带 `transcript_path`，但 16 个 hook 只有 12 种实际触发，
  应写成“注册 16 个、按条件触发”。MCP 继承未测。见 [E2](task-worker-execution-wa0-evidence.md#e216-个-hook-与-transcript_path)、[E6](task-worker-execution-wa0-evidence.md#e6用户-settings-下-init-的-skillsplugins)。
- **OQ5 turn 结束未自报**（保持开放：按推荐推进，由对应切片定）：只唤醒 Planner（今天 Stop hook 的语义），还是像 Codex 那样 300s 宽限后失败（`server/scheduler/running_worker.rs:23`）。推荐前者，行为不变。
- **OQ6 codex viewer**（保持开放：按推荐推进，由对应切片定）：viewer env 里的 MCP token（`codex_adapter/mod.rs:1276-1281`）是否真被 viewer 用到。若不用，viewer 可以 `NotRequired` 迁成 typed PTY；若用到，在 Legacy wind-down 之前保留 Legacy。
- **OQ7 terminal-worker**（按用户输入 2 改写；只用 Pipe 已定（用户），(i)(ii) 保持开放：按推荐推进，由对应切片定）：方向已定为只用 Pipe execution，不提供 PTY 变体（§5.5）。待确认的只剩两点：
  (i) 是否接受失去 TTY 语义与交互输入（§5.5 的“失去”一栏）；(ii) kernel 重启时在跑的 terminal-worker 是重新观察（推荐）还是照 gate 停止。
- **OQ8 无 Engine 的部署**（**已定（用户）：接受 Engine 为硬依赖**）：Claude worker 与 terminal-worker 都将要求 Engine（`server/execution_boot.rs:24-26` 无 key 文件即 None）。未配置 executor 的部署会失去这两种 worker 的新启动。是否接受，还是先确保 Engine 在所有部署上配置。
- **OQ9 worker 的 turn 发起者**（保持开放：按推荐推进，由对应切片定）：用一个小的 worker 会话控制器加 §3.6 的 turn 行（推荐），还是复用 PlannerHarness 的队列与快照（可以免费得到 approvals 与会话 UI，但要引入 carrier 与 conversation 语义）。
- **OQ10 worker 的登录与配置来源**（**已定（用户）：`UserHome`**）：`UserHome`（推荐，行为同今天，无需额外登录）还是 `Dedicated`（与 Planner 隔离，需 owner 登录，失去用户设置）。§3.4.1。
  WA0 证据（不改结论）：E9 中不设 `CLAUDE_CONFIG_DIR` 时 `-p` 与同用户 TUI 读同一凭据文件，无需额外登录；指向空目录时未登录且不回退到
  用户凭据，用户 settings/skills/plugins 也不再加载，但内置能力仍在。这支持 `UserHome`，也支持 §3.4.1 不共用 Planner Dedicated 配置。
  临时复制凭据只是定位来源的实验，不是产品设计建议。限制：仅本机 Linux 文件凭据后端。见 [E9](task-worker-execution-wa0-evidence.md#e9配置目录登录与-tui)。
- **OQ11 每 turn 一 execution 还是常驻 execution**（用户输入 1；**已定（用户）：每 turn 一 execution**，常驻待 E7）：推荐每 turn 一 execution；常驻待 E7 与 §3.1.1 列出的三个问题有答案后再议。
  WA0 证据（不改结论）：E7 已有答案，stdin 保持打开时进程在 `result` 后继续等待；工具期间写入的消息在工具边界被当前 turn 吸收
  （steer 语义，只有一个 `result`，transcript 记 `absorbed_mid_turn`），常驻形状技术上可行。§3.1.1 列出的三个问题（逐 turn 授权复核、
  C5 下空闲占用、重启接管）未由实测回答，所以仍保持每 turn 一 execution。见 [E7](task-worker-execution-wa0-evidence.md#e7常驻输入与-turn-中途消息)。
- **OQ12 WorkerBench 分层**（Planner 补充的用户输入；**已定（用户）：采纳**，长时间或交互命令走 Workbench 常驻终端卡，本设计不改提示词）：一次性命令走 terminal-worker 的 Pipe execution；长时间或交互命令改为对 Workbench 中常驻 terminal 卡的操作，
  不是 task worker。是否采纳，以及是否同时修改 Planner 指南，见 §5.6。已由用户决定的部分：共享终端的状态冲突接受为已知风险、处置为重试，
  不设计仲裁、加锁或独占；凭据不进共享终端环境，按安全边界处理。

### 5.5 terminal-worker：只用 Pipe execution（用户输入 2）

**今天是否真有路径给 terminal-worker 发按键或交互**。源码上这些路径都可达，但没有任何 kernel 内的调用方自动使用：

- Planner 与 Assistant 可以对 task worker 的终端发 `key`/`text`/`sequence`：`keys_refused` 只在任务绑定且 provider 拒绝按键时拒绝
  （`server/terminal_interaction/target.rs:267-276`），`KeysOnly` 返回 `BoundKeys::Accepted`（`exec/provider.rs:79-93`；`provider/worker/terminal.rs:38-41`）；
- 用户在卡片上打开终端打字，作用域 `InteractiveUser` 总被允许（`server/terminal_renderer/input_authority.rs:19-38`）；
- 自动方：watcher 的按键只用于 agent worker 的启动对话框（`crates/calm-server/prompts/terminal/worker-quiet-watch.md:5`）；Planner 指南说 terminal task 的
  `command` 原样交给 `/bin/sh -c`（`crates/calm-server/prompts/planner.md:29`），对 worker 的纠正用 `message`（`crates/calm-server/prompts/guides/terminal.md:6`），
  没有指示向 terminal task 发按键；
- 实际运行中是否有人或 Planner 这样做，源码不能回答〔待查：生产事件中对 terminal 类 task 终端的 input 回执〕。
- `TuiInput::KeysOnly` 本身不能删：managed provider 也返回它（`provider/worker/managed.rs:31-34`）。

**Pipe 形状**（复用 gate 的机制，不新建生命周期）：

- op 自有的 Pipe execution：`/bin/sh <wrapper>`，`IoMode::Pipe`（`server/operation/gate_execution.rs:93-99`；`crates/calm-session/src/control.rs:87-90`）；
- 包装脚本先 `read -r _go`，再把 stdout/stderr 重定向进日志文件，最后把退出码原子写入 exit 文件（`server/operation/gate_wrapper.snapshot.sh:3-13`）；
  `command` 作为一步原样交给 `/bin/sh -c`，保持 planner 合同；
- 释放：Held，载荷就是 `go\n`，经 `gate_execution::release` 的授权事务（`gate_execution.rs:196-233`）；授权是 §3.3 同一函数的 terminal 版本
  （terminal-worker 无租约，起点盘点 workspace-next-steps §2 W1 行），即 op live claim + `require_attempt_startable_tx` + 写者条件；
- 结果：parked op 在结算后读 Drained 状态的退出码（同 `task_gate_run/mod.rs:438-444`）与日志尾，调 `complete_terminal_task`
  （`server/scheduler/mod.rs:2162-2280`）；结果 JSON 的键保持不变（`"pty_output"`、`"pty_output_truncated"`、`"source": "terminal-exit"`，`:2216-2221`），内容改为日志尾，
  键名是否改是独立的合同变更，本设计不改；
- 日志的读取面：今天 gate 日志经 track 视图 `runs/<attempt_id>/gates/<N>.log` 读取（`server/mcp_server/registry.rs:229`），没有 HTTP 路由；
  terminal-worker 日志的读取面（track 视图与卡片只读日志）在 WT 中定义；
- 秘密代理：与 Q8 一致，在准入前拒绝。gate 今天把 `terminal_worker_env` 的代理值直接放进 intent（`gate_execution.rs:54-61`），不经分类，这是 gate 的既有问题，另立 issue，不在 W 内修。

**WR2 落到日志**：输出由子进程自己写进日志文件，不经 Engine OutputLog，所以没有“结算之后取尾、复制进 `pty_output`”这一步，也不涉及 `OutputEnd::Open`。
durable 义务是 parked op 本身：结算之后它仍未完成，boot 由 parked 恢复重驱（`server/operation/driver.rs:498-503`、`:588-625`），完成时重读日志尾，幂等。
测试：结算后、完成前崩溃；日志读取一次失败后重试；日志尾截断标记；均无 renderer、无用户操作。

**kernel 重启**：gate 在 boot 时把 Observing 的执行停为 KernelRestarted（`task_gate_run/mod.rs:398-406`）。今天的 Legacy terminal-worker 活过 kernel 重启。
terminal-worker 的全部证据（日志文件、exit 文件、结算）都是持久的，推荐 boot 时重新观察而不停止：Held 只能经同一授权事务释放，Releasing 按自己的原因停止、永不重发，
Released 继续观察。是否这样做是 OQ7(ii)。

**失去什么**：

- 交互：不能再对 terminal-worker 打字、发按键或 attach 终端；需要交互的工作改由 terminal 卡（§5.6）或 agent task 承担；
- TTY 语义：子进程的 stdin/stdout 不是终端，依赖 isatty 的程序会改变行为（颜色、分页器、交互提示、进度条）。命令需要 TTY 时应改写或改用 terminal 卡；
- 终端视图：卡片从 xterm 视图变为只读日志；
- 不变：terminal task 仍无期限、仍不可取消（`scheduler/mod.rs:1620-1623`；`prompts/planner.md:29`）。Pipe 让取消在技术上可行（op Stop），是否开放另议。

**与 PTY 的资源与机制对比**（只列源码事实，不估数字）：

| | Legacy PTY terminal-worker（今天） | Pipe execution（WT） |
|---|---|---|
| 进程 | supervisor 中的 PTY 子进程（`server/routes/terminal.rs:108-133`） | Engine 的 Pipe 子进程（`gate_execution.rs:93-99`） |
| kernel 侧 | 每个终端一个 renderer，缓冲 `replay_bytes = 1 << 20`（`server/routes/terminal.rs:117-125`、`:137-148`） | 无 renderer；Engine replay `8192` 字节（`gate_execution.rs:98`） |
| 持久行 | card、terminal 行、worker session、`tx_output.terminal_launch` | op 行与执行记录；日志与 exit 文件 |
| 输出 | renderer 在退出时写 `pty_output`（结算后取尾的义务即 WR2） | 子进程自己写日志文件 |
| 退出权威 | 终端退出字段（`scheduler/mod.rs:1879-1899`） | Engine 结算 + exit 文件 |
| 交互 | 可（KeysOnly、用户打字） | 不可 |
| kernel 重启 | 活过 | 按 OQ7(ii) |

### 5.6 WorkerBench：常驻终端的分层评估（Planner 补充的用户输入）

**源码中的 WorkerBench 是什么**。代码里没有一个叫 WorkerBench 的独立服务；对应的是 terminal 卡的 Workbench 放置：

- 放置只是展示字段：`TerminalPresentation { source, in_workbench }`，文件头写明“Never an authorization fact”（`crates/calm-types/src/card_presentation.rs:1`、`:21-35`）；
  手动创建的终端默认在 Workbench，Agent 创建的默认不在（`:30-35`）；
  唯一写者 `set_workbench_tx`（`crates/calm-truth/src/db/sqlite/terminal_presentation.rs:1-39`）；路由 `PATCH /api/cards/{id}/terminal-workbench`，
  响应说明“process and source unchanged”（`server/routes/terminal_cards.rs:31-34`、`:164-211`）；前端在 `fe/web/src/app/router/terminal-workbench*.tsx`。
- 常驻终端本身是 terminal 卡：
  - 人创建：`POST /api/tracks/{track_id}/terminal-cards`，program 缺省为 `$SHELL`，cwd 缺省为 track workspace，env 是继承集加请求给的键
    （`server/routes/terminal_cards.rs:40-59`、`:81-155`；`planner_hooks: false`，`:109-110`）；
  - AI 创建：`neige_terminal_open` 只给 Planner（`server/mcp_server/tools/terminal.rs:80-86`），来源 Agent、`planner_hooks: true`（`:416-421`），
    提交 Legacy kind `terminal-create`（`:425`）；
  - 所有权与生命周期：卡片属于 Track，由创建它的 op 建立；是否 Engine execution 取决于配置 `terminal_execution`：开启时人创建的走 typed
    （`server/operation/terminal_execution_create.rs:109-150`、`server/config.rs:54`），否则 Legacy（`terminal_execution_create.rs:144-148`）；
    AI 打开的总是 Legacy（`tools/terminal.rs:425`）。
- 输入仲裁：Planner 与 Assistant 是与人共用同一 renderer 和 PTY 的客户端（`server/terminal_interaction/mod.rs:1`、`:63-71`）。AI 写入前要 claim 控制权
  （claim-if-unowned，`server/terminal_interaction/input_control.rs:5-17`；被占用时 `CONTROL_HELD_BY_ANOTHER_CLIENT`，`mod.rs:7`）；
  人的作用域 `InteractiveUser` 的输入与控制检查恒为真（`server/terminal_renderer/input_authority.rs:19-38`），即人随时可以打字，不受 AI 的 claim 约束。
- 凭据与环境：Legacy 继承 supervisor 白名单（`child_environment.rs:5-49`）；typed 终端只有 TERM 三键加调用方 env（`server/routes/terminal.rs:151-167`），没有 HOME。
  AI 打开的终端带 Planner hooks 环境（`tools/terminal.rs:420`）。
- 今天的 Planner 指南已经让 Planner 用自己的终端卡跑需要复跑的命令（`crates/calm-server/prompts/guides/terminal.md:7`、`:17`）。

**与 task worker 语义的兼容性**：常驻共享终端不能承担 task worker。

| task 语义 | 常驻共享终端的情况 |
|---|---|
| 退出码判定 | shell 不随命令退出；要得到命令的退出码只能解析输出里的哨兵，不是 SQL 结算这样的 durable 事实 |
| gate | gate 在 attempt 自己的 checkout 中跑自己的 execution（`gate_execution.rs:37-121`），不经共享终端 |
| worktree 租约 | terminal-worker 本来就没有租约；agent worker 的租约属于其 card，共享终端不持有 |
| 取消与超时 | 只能发 Ctrl-C 等按键，没有 Stop 与结算；人的输入可能同时到达 |
| 输出持久化 | 只有屏幕与 renderer 缓冲（`routes/terminal.rs:125`），不是按命令的持久日志 |
| 状态串扰 | cwd、环境变量、shell 选项与后台作业在命令之间与人之间共享；并发命令交错。**用户决定：接受为已知风险，处置为重试**（见下） |
| 人机同时输入 | 人的输入不受 AI claim 约束（见上），AI 的命令可能被人的按键打断或混入。**用户决定：同上，接受，重试** |
| 凭据 | 写进共享 shell 的秘密值对人和其他 AI 写入可见，也会进 renderer 缓冲。**安全边界，不属于可接受的冲突**（见下） |

**用户决定（Planner 消息 c4b98977）：共享状态冲突接受为已知风险，处置为重试**。cwd、env、并发命令与人机同时输入的冲突不可避免，但属于长尾稀疏情况，
发生时由 AI 重试即可。本设计**不**为此加仲裁、加锁或独占机制，沿用今天的 claim-if-unowned 与人的输入恒可写（`input_control.rs:5-17`；`input_authority.rs:19-38`）。

**凭据仍是安全边界**：这一决定只覆盖状态冲突。MCP token、秘密代理值等凭据不得进入人机共用的终端环境：
今天 AI 打开的终端只注入非秘密的 hook 键（`server/terminal_hooks.rs:40-49`，经 `server/operation/terminal_adapter.rs:278-288`），
而终端 env 会持久写进 terminal 行与 spawn 输出（`terminal_adapter.rs:277`），所以任何凭据一旦进入就会被持久化并对共用者可见。
对 Workbench 终端的操作只经既有的输入通道写命令文本，不新增向共享终端注入凭据的路径；需要凭据的命令归 agent worker（载荷通道，§3.3）
或 terminal-worker（秘密代理在准入前拒绝，§5.5）。本设计的结论限于“W 不新增凭据路径”，不是“共享终端的最终环境绝无凭据”（r2 adversarial N2）：
Legacy 终端还继承 supervisor 白名单，其中含 `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`（`child_environment.rs:36-42`），带认证的代理值会原样进入；
请求 env 与 hook 键合并后也不经分类（`terminal_adapter.rs:278-288`）。这是既有路径，另立 issue，不在 W 内修。
验收：`w_workbench_terminal_env_has_no_credentials` 断言 W 的改动没有在 AI 打开的终端 env 中加入 `NEIGE_MCP_TOKEN` 或经 W 路径的代理值
（env 只含 `TERMINAL_HOOK_ENV_KEYS` 与请求键），不断言继承环境。

**资源**（只列源码事实）：每条一次性命令一个 Pipe execution：一个进程树、一个 op 行、一个日志文件，无 renderer；常驻终端：一个 renderer
（1 MiB 缓冲，`routes/terminal.rs:147`）和一个随卡片存活的 shell。具体内存与 CPU 数字未测。

**推荐分层**：

- 一次性命令（terminal task）= terminal-worker 走 Pipe execution（§5.5）：有退出码、持久日志、结算与 task 收敛；
- 长时间或交互命令 = 对 Workbench 中常驻 terminal 卡的操作，经既有 `neige_terminal_open`/`input`/`control`，**不是 task worker**，
  不参与 task 的退出判定、gate 与租约；需要结果进入 task 时，由 agent worker 或 Planner 读取后自报；
- agent task 仍是需要判断与多步操作的工作的承载者。

**影响**：

- terminal-worker 的 PTY 路径：WT 同片删除 terminal-worker 新启动的 Legacy 分支（`TerminalWorkerAdapter` → `spawn_task_terminal_with_parts`）；
  升级前的 Legacy 行的读者保留到 Legacy wind-down。terminal 卡的 PTY 路径（人与 Planner 的终端）不受影响，仍是 typed 或 Legacy。
- WT：从 typed PTY 改为 Pipe，复用 gate 的 owned execution，不再需要 `Admitted` 记录、typed 观察实现与 PTY 握手（§7）。
- C5：Pipe execution 带 track，由 `unsettled_executions(Track)` 覆盖；terminal-worker 的 Legacy 检查只为升级前的行保留；terminal 卡的检查照 C5 现有规则
  （owner 自建 card 的 Legacy 检查保留，typed 终端由谓词覆盖）。
- 是否把“长时间/交互命令用 Workbench 终端”写进 Planner 指南，是 OQ12 的一部分；本设计不改提示词。

## 6. 与 WR1–WR7 对比（问题 4）

| 需求 | 路线 A 下的处置 | 仍需要的片 |
|---|---|---|
| WR1 退出取代的原子围栏 | **收窄并修共享分派器**：无界面 worker 没有 claude-restart，turn 之间的竞争是两个准入的竞争，由 SQL `admit` 的单 claim 与“card 当前 session”条件串行化（`provider_execution.rs:168-173`）。owner `complete` 不写 task。kernel 一侧写 task 或租约的收尾只剩 `converge_dead_worker`：它改为单个写事务，精确当前 session 围栏先于 task 失败写与所有租约兜底，过时结果无副作用返回；task 失败写改用 card 精确所有权（§3.7）。这一修订同样作用于保留的 Legacy 探测臂 | WA2：§3.7 的 `w_reaper_stale_probe_after_legacy_restart_is_noop`、`w_reaper_stale_probe_after_attempt_replaced_spares_new_attempt`、`w_reaper_task_fail_requires_card_binding`、`w_headless_closure_spares_replacement_session`；`w_claude_worker_completion_races_new_admission` |
| WR2 输出的 durable 完成义务 | Claude 不适用：task 结果是自报，没有 `pty_output`。terminal-worker **仍需要**，落到日志文件与 parked op（§5.5） | WT |
| WR3 typed 终端重启 | **删除**（Claude）：重启是同 session 的新 execution，经持久 turn 行发行，alias 各自独立（`sweep.rs:5-7`） | 无；WA2 加一条真实的“turn 结束 → 续跑 → 自报”回归 |
| WR4 未绑定 session 的租约清理 | **触发**（r1 boundary B3）：无界面 session 由 reaper 的 SQL 臂发现，经 `converge_dead_worker` 这一权威分派器收尾；全部兜底在分派器的同一写事务与当前性围栏内；会话结束谓词以未终结 turn 行与 op 目标证明保留合法的首 turn 窗口（§3.7） | WA2；WT 的 Pipe 行没有 session 租约 |
| WR5 `ResumeEligible` | **删除**：没有决策树；没有 provider 产生它（`server/reaper/mod.rs:327-341`） | 无 |
| WR6 保留已收敛的机制 | 保留：释放授权围栏（task 条件在 Held→Releasing 的同一写事务里，§3.3；准入事务另有一道）；Releasing 不重发；SQL 结算是退出权威。删除：Claude 的 PTY 握手、输入门及其负面测试；terminal-worker 也不再需要（Pipe） | WA1–WA2、WT |
| WR7 Q1/Q2/Q8 对 worker 的效果 | Q1：新 attempt 在 mint 与租约之前因 `ExecutionUnavailable` 失败，不回退；Legacy 行不受此检查。Q2：supervisor 崩溃后，worker execution 与 Planner 一样 Unknown，失败关闭到宿主重启；task 由超时或取消收敛，claim 与租约保留到宿主重启（K6）。Q8：Claude 有载荷通道；terminal-worker 遇秘密代理在准入前拒绝 | WA2、WT |

原切片的处置：

| 原片 | 处置 |
|---|---|
| Wa（环境对等） | 拆分：claude-worker 的环境白名单进 WA2（§3.4.1）；terminal-worker 沿用 gate 的环境构造，差异在 WT 中核对 |
| Wb（观察端口与决策树） | 删除 |
| Wc（terminal-worker） | 改为 WT（Pipe） |
| Wd（claude-worker typed PTY） | 由 WA2 取代 |
| We（codex viewer） | 按 OQ6 决定 |

## 7. 推荐切片

规模口径同 workspace-next-steps §3.2：手写增删合计，粗估。gate 仍是父合同 §6 的公共 gate G，加一次拒绝空选择的聚焦 replay。
测试过滤器前缀用 `w_`。

| 片 | 结果 | 前置 | 验收 | 规模（粗估） | 等级 |
|---|---|---|---|---|---|
| WA0 | 实测证据文档（E1–E9），只改文档 | 无 | 每项给出版本、命令、输出摘要与结论；由结论定下 OQ1/OQ3/OQ4/OQ10/OQ11 的可行性 | 文档 | L1：不改代码，但定下方向 |
| WA1 | owner 层推广，Planner 行为不变：alias 前缀由 owner 提供（Planner 字节不变）、owner 注册、`stop_session` 移进 `provider_execution`、`admit` 与 `release::send` 接受调用者的事务条件、协议循环与角色化 turn sink 拆分（§3.2）、`spawn::argv` 与 `init_check` 拆成类型化角色档（Planner 档字节钉住） | C4 | Planner argv 与环境 golden 不变；`provider_execution` 既有用例与 C4.1–C4.4 gate 的 replay 全绿；新增 `w_owner_alias_preserves_planner_bytes`、`w_stop_session_is_shared`、`w_release_runs_caller_authority_in_release_tx`、`w_release_refused_writes_stop_without_bytes` | 600–1000 | L2：生命周期 owner 层 |
| WA2 | claude-worker 无界面：worker owner（consumer `claude-worker`）与 `Purpose::WorkerTurn`；worker turn authority（准入与释放两处）；载荷凭据；`claude-worker-turn` 行（含首 turn）与唯一消费点；消息改成 turn 行；boot 通道按 OQ1(a)，只停不写；收尾分派器的单事务围栏（Legacy 探测臂与无界面 SQL 臂，§3.7）；worker 档的登录/配置/环境合同（§3.4.1）；接缝 S3–S6 经 worker 会话端口（删 `codex_worker.rs:170-176`、`:207-213` 的 provider match）；新准备形状与升级分界（§10）；该 kind 新启动的 Legacy 分支同片删除 | WA0、WA1 | 见下方列表 | 3000–4700（超过 800，理由同原 Wc：准入、授权、发行、停止、结算与 task 收敛是一个不变量，“每条结束路径都结算并收敛 task，每个写者 turn 都经授权”） | L2 |
| WA3 | worker 卡的会话视图与中断控件；无界面 worker 不显示 restart | WA2 | 浏览器预览；`fe` lint、build、test；相关浏览器测试 | 300–600 | L1（只消费 WA2 已声明的合同） |
| WA4（可选） | OQ1(b) 同 consumer 重新接管 | WA2、OQ1 | kernel 重启前后同一 execution 持续输出，无重复 turn；负面用例：claim 已被替换时不接管 | 600–1000 | L2 |
| WT | terminal-worker 成为 op 自有的 Pipe execution（§5.5）：复用 gate 的 owned execution 与包装脚本；Held `go` 释放经 terminal 版授权；秘密代理准入前拒绝；parked op 结算后读日志尾与退出码经 `complete_terminal_task`；boot 按 OQ7(ii)；该 kind 新启动的 Legacy PTY 分支同片删除，升级前的行保留 Legacy | C4；OQ7 | `w_terminal_worker_exit_code_completes_task`、`w_terminal_worker_release_refused_when_superseded`、`w_terminal_worker_secret_proxy_refused_before_admit`、WR2 三条：`w_terminal_worker_crash_after_settle_before_complete`、`w_terminal_worker_log_read_failure_retries`、`w_terminal_worker_log_tail_truncated_flag`；`w_terminal_worker_kernel_restart_*`（按 OQ7(ii)）；`w_pre_upgrade_terminal_worker_stays_legacy` | 1200–2000 | L2：task 收敛与释放授权 |
| WV（按 OQ6） | codex viewer 成为 typed PTY（`NotRequired`），或留到 Legacy wind-down | WT 不再是前置（WT 不建 typed worker 终端）；C4 | 原 We 测试清单 | 600–1000 | L2 |

WA2 的验收：

- `w_claude_worker_refused_without_engine_before_mint`；反向：`w_legacy_claude_worker_row_recovers_without_engine`（无 Engine 时升级前的 Legacy 行仍可 attach、消息、取消与清理，新 attempt 在准备前拒绝且无新租约与 token）
- `w_claude_worker_admit_requires_startable_attempt`：三例，租约过期、attempt 被取代、task 已终结，均不准入、不发字节
- 准入之后、释放授权之前的交错（暂停在等 Spawned 处）：`w_release_refused_when_op_lease_expired_after_admit`、`w_release_refused_when_attempt_superseded_after_admit`、
  `w_release_refused_when_canceled_after_admit`、`w_release_refused_after_self_report`；各例断言同事务 Stopping{ReleaseRefused}、零字节
- 授权与停止两种顺序：`w_release_authorized_then_cancel_stops_process`、`w_cancel_then_release_refused`
- `w_claude_worker_token_only_in_payload`：operations 与 terminal 的 env、execution 持久 secrets、OutputLog、stderr 捕获与 argv 都不含 token 明文
- `w_claude_worker_turn_end_without_report_wakes_planner`
- `w_claude_worker_message_becomes_next_turn`、`w_message_mid_turn_waits_for_claim_clear`
- `w_claude_worker_resume_then_self_report`：真实回归
- `w_claude_worker_timeout_stops_then_fails_session`、`w_claude_worker_cancel_stops_execution`
- 首 turn 与发行入口：`w_first_turn_row_written_with_session`（spawn op 的准备事务写 `first` 行，spawn op 不准入）、`w_spawn_op_never_admits_execution`
- kernel 重启与续跑（§3.6 崩溃点表逐行，全部经 `recover_on_boot`、worker owner boot 通道与 owner sweep 的真实入口，每例断言该 session 的准入总数、
  `claude-worker-turn` 行数与续跑输入形式）：`w_claude_worker_kernel_restart_resumes`、`w_turn_crash_before_admit_admits_once`、
  `w_turn_crash_after_admit_before_release_continues_once`、`w_turn_crash_while_releasing_continues_once_same_input`、
  `w_turn_crash_after_released_continues_once_with_notice`、`w_turn_crash_after_result_before_complete_no_continuation`、
  `w_turn_crash_after_complete_before_claim_clear_admits_continuation_once`、`w_turn_repeated_boot_writes_no_row`
- 消费点：`w_finish_retried_completion_writes_one_continuation`、`w_complete_before_row_parked_retries`、`w_turn_redrive_without_guard_parks_without_io`
  （op lease 过期后的再次 claim 不释放、不驱动、不准入）、`w_turn_admission_finishes_settled_driverless_claim`（旧 claim 已结算且无驱动者，sweep 未运行，行在一次驱动内交接并准入）、
  `w_turn_no_continuation_after_cancel_stop`、`w_turn_no_continuation_after_deterministic_failure`
- 行与竞争：`w_turn_row_refused_after_cancel`、`w_turn_row_refused_after_self_report`、`w_turn_row_refused_when_superseded`、
  `w_turn_row_vs_clean_issuance_first`、`w_turn_row_vs_clean_clean_first`
- 授权函数的表测试 `w_worker_authority_table`：直接构造各条件组合（含“Verifying 且租约仍持有”“worker_card_id 为空而 card 不是 spawn op 目标”），隔离写者条件与绑定条件
- `w_claude_worker_completion_races_new_admission`（WR1）
- §3.4.1 的四条环境与配置用例；§3.7 的四条 WR1 交错、六条 WR4 收尾与两条未 stamp 窗口用例
- 升级：`w_pre_upgrade_tx_committed_claude_worker_stays_legacy`、`w_pre_upgrade_spawn_started_claude_worker_stays_legacy`、`w_pre_upgrade_pending_claude_worker_starts_headless`（§10）
- 既有 `track_worker_cwd*`、`planner_result_loop`、`worker_flow_claude_suite` 改写后全绿
- Track 删除时未结算的 worker execution 返回 Conflict

**mutation 预测**（每项先写出完整红集，只改生产代码，按 AGENTS.md 运行包含这些测试的整个 target）。下表是**实施前的假设**，不是 mutation 证明（r2 adversarial N1）：
测试落地后，每行须按实际 target 校准成按名列出的完整红集与断言依据，再执行；一道突变可能被另一道围栏吸收的，表中已写明，并另设隔离该条件的测试。

| 片 | 单因素突变 | 预测红集 | 预测保持绿 |
|---|---|---|---|
| WA1 | alias 前缀对 Planner 改变字节 | `w_owner_alias_preserves_planner_bytes`；既有 Planner sweep 用例中按 alias 匹配者须在 WA1 实施时按名补全，补全前本行不算完整预测 | 其余 |
| WA1 | `release::send` 不调用调用者授权 | `w_release_runs_caller_authority_in_release_tx`、`w_release_refused_writes_stop_without_bytes` | Planner 既有用例（Planner 传空授权） |
| WA2 | 释放事务去掉 worker 授权（准入仍有） | 四条 `w_release_refused_*_after_admit`、`w_cancel_then_release_refused` | `w_claude_worker_admit_requires_startable_attempt` |
| WA2 | 准入事务去掉 worker 授权（释放仍有） | `w_claude_worker_admit_requires_startable_attempt`（断言未准入） | 四条 `w_release_refused_*_after_admit` |
| WA2 | 写者条件放行 Verifying（只用 `require_attempt_startable_tx`） | `w_worker_authority_table` 的“Verifying 且租约仍持有”行 | `w_release_refused_after_self_report`、`w_turn_row_refused_after_self_report`（自报同事务释放租约，`decision_sink.rs:152-166`、`:199-204`，租约条件仍拒绝）；取消、取代与租约用例 |
| WA2 | owner `complete` 在 kernel 一侧中断时不写续跑行 | `w_claude_worker_kernel_restart_resumes`、`w_turn_crash_after_admit_before_release_continues_once`、`w_turn_crash_while_releasing_continues_once_same_input`、`w_turn_crash_after_released_continues_once_with_notice`、`w_finish_retried_completion_writes_one_continuation`（断言恰好一条） | `w_turn_crash_before_admit_admits_once`、`w_turn_crash_after_result_before_complete_no_continuation`、`w_turn_repeated_boot_writes_no_row` |
| WA2 | 续跑行不以 `complete_parked_tx` 的实际终结为条件（`AlreadyResolved` 也写，键改为每次新键） | `w_finish_retried_completion_writes_one_continuation`、`w_turn_crash_after_complete_before_claim_clear_admits_continuation_once` | 其余崩溃点用例 |
| WA2 | 重驱调用无 guard 也执行释放与驱动 | `w_turn_redrive_without_guard_parks_without_io` | 崩溃点用例（boot 后 E 已被停止，释放 CAS 失败） |
| WA2 | boot 通道也写续跑行 | `w_turn_repeated_boot_writes_no_row`、`w_turn_crash_after_admit_before_release_continues_once`（断言恰好一条） | `w_turn_crash_before_admit_admits_once` |
| WA2 | 发行行跳过 `admission_gate` 交接 | `w_turn_admission_finishes_settled_driverless_claim` | `w_message_mid_turn_waits_for_claim_clear`（SQL `admit` 的单 claim 仍拒绝，`provider_execution.rs:168-173`） |
| WA2 | 收尾臂不调分派器的租约兜底 | `w_headless_closure_releases_lease_without_spawn_op`、`…_when_binding_missing`、`…_on_task_race_lost` | `w_headless_closure_spares_replacement_session`、`w_headless_idle_running_session_not_closed` |
| WA2 | 分派器去掉第 1 步当前性围栏 | `w_reaper_stale_probe_after_legacy_restart_is_noop`、`w_reaper_stale_binding_missing_releases_nothing`、`w_headless_closure_spares_replacement_session` | 三条 WR4 释放用例、`w_reaper_task_fail_requires_card_binding` |
| WA2 | 探测臂的 task 失败写改回 `TaskReporter::Kernel` | `w_reaper_task_fail_requires_card_binding` | `w_reaper_stale_probe_after_legacy_restart_is_noop`（围栏先拒） |
| WA2 | 会话结束谓词把 `worker_card_id` 为空当成绑定失效（去掉 op 目标证明） | `w_headless_closure_spares_unstamped_after_fast_turn` | `w_headless_closure_spares_unadmitted_first_turn`（未终结的 `first` 行仍阻止关闭） |
| WA2 | 准备事务不写 `first` 行（首 turn 由 spawn op 副作用发行） | `w_first_turn_row_written_with_session`、`w_spawn_op_never_admits_execution`、`w_headless_closure_spares_unadmitted_first_turn` | `w_headless_closure_spares_unstamped_after_fast_turn` |
| WA2 | 升级分界只看相位不看准备形状 | `w_pre_upgrade_tx_committed_claude_worker_stays_legacy` | 另两条升级用例 |
| WA2 | worker 环境设置 `CLAUDE_CONFIG_DIR` | `w_claude_worker_env_uses_user_config` | `w_planner_env_unchanged` |
| WT | parked op 结算后不读日志尾即完成 | `w_terminal_worker_crash_after_settle_before_complete`、`w_terminal_worker_log_read_failure_retries` | `w_terminal_worker_exit_code_completes_task` 中不查输出的断言 |
| WT | 释放不带 terminal 授权 | `w_terminal_worker_release_refused_when_superseded` | 其余 |

预测与实际红集不符即作废该轮结果；上表中预测保持绿而实际变红的，先查是否突变不是单因素。

顺序：WA0 → WA1 → WA2 → WA3，WA4 视 OQ1。WT 与 WA 系列互不依赖，可以并行在不同 worktree。
W 不再依赖 C5（原依赖列为“C4、C5”）。如果 WA2 或 WT 先于 C5 合入，C5 对该 kind 的 Legacy 检查只为升级前的行保留。

## 8. 架构五点与永久成本

- **抽象边界**：task 政策留在 scheduler、decision_sink、租约、gate、sweeper 与 worker 控制层（turn authority、turn 行、收尾分派）。进程生命周期只在 `provider_execution`，
  它只多了两个“调用者事务条件”的插口，不读 task。协议驱动在 `claude_planner` 的共享协议循环（实施时可以按职责改名，但不另建 crate），角色差别在 turn sink。
  worker 会话端口在 `exec::provider`，实现在 server。
- **重复逻辑**：Claude worker 与 Planner 共用协议循环、载荷、owner、sweep 与释放事务；terminal-worker 与 gate 共用 owned execution、包装脚本与释放授权。
  删除的平行实现包括 worker 的 TUI 粘贴投递（对 Claude）、PTY 退出解释（对 Claude 与 terminal）、两处 provider match、以及 `stop_session` 的 Claude 私有版本。
  死亡 session 的租约兜底只有 `converge_dead_worker` 一份，Legacy 探测臂与无界面 SQL 臂共用它的单事务判定，不另写一份过时检查。
  turn 发行只有一种行与一个消费点，boot 通道与 spawn op 都不发行；续跑没有第二套交接机制。ACP 的私有 `stop_execution` 是否并入，在 WA1 中核对后决定。
- **硬编码**：`KnownEnd` 与 `Purpose` 是 owner 层的闭合协议集合，加一个 consumer 改一个变体；角色档是类型化的两档，`config_source` 是闭合的两值枚举，
  不按 card 身份特判。owner 注册由 host 提供，`execution_boot.rs:10-18` 不再列具体 Planner host 名。
- **外部合同事实**：transcript 路径取自 hook 载荷；Claude session 取自 `init`（`driver.rs:137-142`）；`--resume` 的前提是持久记录的 `init` session；
  `-p`、常驻 stdin、`CLAUDE_CONFIG_DIR` 的行为一律以 WA0 的实测为准（E1–E9），不从私有文件布局推导，也不同时接受两种形式作回退。
- **永久成本**：
  - 每个 30s tick 多一次 `consumer_inventory`（`sweep.rs:24-25`）与一次 claim 列表；reaper 每 tick 多一个无界面 session 的 SQL 查询；每次 boot 多一次 worker revoke 查询。
  - 每个 turn（首 turn、消息、手动继续、每次中断后的续跑）一行永久的 keyed op（keyed 行不清理，`driver.rs:190-191`）；行数与 turn 数同阶。
    这些行经 parked sweep 时，本 kind 的 `recover_owned_parked` 只读 claim 是否仍在，每 tick 每个 Parked 行一次读。
  - reaper 的分派器从“事务外读、事务内写”变成单个写事务：每次死亡判定多读 `cards.session_id` 与 session 状态各一次，次数与今天的分派次数相同。
  - 新增一种 worker provider 要实现端口的 `stop` 与“turn 结束”映射，由类型强制。
  - Planner argv/环境 golden 与 worker argv/环境 golden 各一份，Claude CLI 升级时一起核对。
  - 新测试只注册在既有 target 中，不加 CI 调用；验收名是设计建议，不变成手抄的测试注册清单。

## 9. 风险

- **K1 kernel 重启打断长 turn**（OQ1）：dogfood 部署频繁时尤甚。缓解：(a) 的续跑行，必要时 WA4。
- **K2 CLI 行为差异未实测**（E1–E9）：WA0 先行，结论不成立的项改设计，不带着假设进 WA2。
- **K3 Engine 成为 Claude worker 与 terminal-worker 的硬依赖**（OQ8）。
- **K4 版本**：worker 不继承 Planner 的固定版本（§3.4.1），沿用今天的 `claude_bin`，版本只记录不比对。若决定给 worker 也加固定，要有自己的配置项，
  不能借 Planner 的配置，否则只有 Codex Planner 的部署会失去 Claude worker。
- **K5 Planner 回归**：WA1 碰共享驱动与释放事务。缓解：argv/环境 golden 与 C4 gate replay；Planner 传空授权。
- **K6 session 终态被 claim 卡住**：`require_unclaimed`（`session_row.rs:629`）。暂态的 Stop 或完成失败保留清理标记，下一拍重试，须有测试证明可重试收敛；
  supervisor 丢失后的 Unknown 按 Q2 保留 claim、租约与占用到宿主 boot 改变后才收敛（`crates/workspace-runtime/src/execution/mod.rs:117-132`），
  测试断言这一保留，而不是要求无条件最终成功；不得为了绿测试绕过 claim 围栏（r1 adversarial N1）。
- **K7 过渡期两条 claude-worker 路径并存**：Legacy 读者要保留到升级前的行排空，见 §10。
- **K8 turn 行的增长**：见 §8 永久成本；若成为问题，清理须另行设计，不能对 keyed 行加保留期（`driver.rs:190-191`）。

## 10. 迁移与兼容（问题 5）

**分界依据是 op 的持久准备形状，不是相位，也不是版本号**（修 r1 adversarial B3）。今天的 Claude worker 准备在 `prepare_tx_and_advance`
（`server/operation/driver.rs:725-738`）里持久化：`card_with_claude_worker_create_tx` 建 card、terminal 与 session（`worker_preparation.rs:57-76`），
session 写入 `terminal_run_id = Some(term.id)`（`truth/db/sqlite/card_composite.rs:478-492`），并取得租约（`worker_preparation.rs:78-79`）；之后才转 SpawnStarted。
所以“升级前 Dispatched、尚未 SpawnStarted 的 op 一律走新路径”是错的：停在 TxCommitted 的行已有终端绑定，新路径的 SQL `admit` 必拒（`provider_execution.rs:147-157`）。

- **规则**：
  - op 的 `tx_output` 含 Legacy 准备形状（`terminal_id`，session 绑定终端）→ 整个生命周期按 Legacy：spawn、attach/NoOp（`claude_adapter/mod.rs:884-978`）、
    没有 launch 状态时只 attach（`server/operation/terminal_launch.rs:147-149`）、补偿、reaper 探测、settled-worker 臂、超时与取消的 pid 收割、消息粘贴；
  - op 尚未准备（无 `tx_output`）→ 新准备形状：不建终端行，session `terminal_run_id` 为 NULL，`tx_output` 带显式的无界面形状标记，
    同一事务写首 turn 的 `first` 行（§3.6）；spawn op 不准入；
  - 不做 Legacy 形状到无界面的原子转换：那要解绑已存在的终端，等于把既有终端变成新的准入权限。
  - 新 attempt（包括重试）总是新 op，走新形状。
  - 消息与收尾的路由：session 绑定终端 → Legacy；否则 → turn 行与 §3.7。与准备形状一致，因为 Legacy 形状总是绑定终端。
- **升级测试**：用旧版本真实写出的 TxCommitted 输出（Starting 绑定与租约），重驱后断言：继续 Legacy、无双启动、无遗留租约、终端未被解绑、
  新路径的 `admit` 未被调用（`w_pre_upgrade_tx_committed_claude_worker_stays_legacy`）；SpawnStarted 的行同理；Pending 的行走新形状。
- **terminal-worker**：同一规则。Legacy 形状（终端行）继续 Legacy；未准备的走 Pipe（`w_pre_upgrade_terminal_worker_stays_legacy`）。
- **无 Engine**：新形状的入口拒绝；Legacy 形状的行不经 Engine 检查（r1 adversarial N3）。
- C5 与 Legacy wind-down 对 Legacy 行的检查照 workspace-next-steps 的规则保留。
- **降级**：不支持。旧二进制没有 `claude-worker` owner，不会 sweep 这些 execution，`Purpose::WorkerTurn` 在旧 sweep 中是“未分类即保留”（`sweep.rs:18-21`）；
  旧二进制也不认识 `claude-worker-turn` op kind。PR 写明最低版本。
- **用户可见变化**：
  - worker 卡从终端变为只读会话视图加中断，不能直接打字（OQ2）；
  - restart 按钮对无界面 worker 消失，“继续”是续跑 turn；
  - kernel 重启时 worker 的当前 turn 被中断并续跑（OQ1）；
  - 会触发弹框的权限请求被拒绝并回给模型（OQ3），不再停在 TUI 上等人；
  - 不再出现目录信任对话框与 watcher 的“trust_accepted”〔E3〕；
  - 未自报的 turn 结束后 Planner 更早被唤醒；
  - terminal-worker 的卡片变为只读日志，命令不再有 TTY（OQ7）；
  - Legacy `claude-restart` 换 session 后，旧 session 迟到的死亡结果既不再让 task 失败，也不再释放 card 租约（§3.7 的当前性围栏）；
    Legacy 探测臂的 task 失败写改用 card 精确所有权，task 不属于本 card 时不再被失败（今天 `TaskReporter::Kernel` 会）。

## 11. 评审处置

### 11.1 第一轮（第一版 → 第二版）

本表记录第二版当时的处置；其中 §3.6 的意图与交接已在第三版收窄，见 §11.2。

| 发现 | 处置 |
|---|---|
| boundary B1、adversarial B1：准入事务里的 task 守卫不能替代 Held→Releasing 时的释放授权 | 核实成立（`release.rs:13-29`；`mod.rs:197-211`）。owner 层 `release::send` 接受调用者授权，与 Held→Releasing 同事务，失败同事务 Stop 不发字节（§3.2、§3.3），先例 `gate_execution.rs:151-183`。同类补全：worker 写者条件排除 Verifying；Releasing→Released 与 Releasing 永不重发；授权先赢的语义；所有发行路径共用一个授权函数。验收与 mutation 见 §7 |
| boundary B2、adversarial B2：boot 续跑与旧 claim 清除之间缺持久、幂等交接；完成回调内续跑会被旧 claim 阻塞 | 核实成立（`mod.rs:246-273`；`provider_execution.rs:168-173`；`sweep.rs:47-57`）。§3.6：keyed op 的 turn 意图与原因同事务写入，唯一发行者经 `admission_gate` 交接，准入与释放各经授权；`complete` 无意图副作用；boot 按 Held/Releasing/Released 与已记录的 `init` session 区分；与 C1 复查的关系。消息同改为意图。崩溃、重复完成、取消/取代竞争的验收见 §7 |
| boundary B3：WR4 兜底在不可达的 Legacy 路径 | 核实成立（`reaper/mod.rs:381-391`、`:445-470`；`mod.rs:234-278`）。§3.7：reaper 的 SQL 臂发现已结束的无界面 session，复用 `converge_dead_worker` 分派，加精确当前 session 条件；经新臂的六条验收；WR4 改为“触发” |
| boundary B4：共用 Planner 启动后的登录凭据与用户配置来源 | 核实成立（`spawn.rs:304-307`；`config.rs:30-31`；`claude_adapter/mod.rs:351-381`；`child_environment.rs:5-49`）。§3.4.1：typed 启动合同（binary、`config_source`、环境白名单），推荐 `UserHome`，只有 Codex Planner 的部署不受影响；新增 OQ10、E9、K4 改写与四条环境验收 |
| adversarial B3：升级分界遗漏已 prepare 的 TxCommitted 操作 | 核实成立（`driver.rs:725-738`；`worker_preparation.rs:57-79`；`card_composite.rs:478-492`）。§10：按持久准备形状划分，Legacy 形状整个生命周期走 Legacy，不做原子转换；新增 TxCommitted 升级用例与 mutation |
| boundary N1：共享协议驱动的接缝 | 核实成立（`driver.rs:186`、`:200-209`、`:402-415`；`session.rs:89-100`）。§3.2 定义协议循环与角色化 turn sink，WA1 实施 |
| boundary N2：外部能力的否定判断超出源码证据 | §0.5（第二版编号 §0.4）与 §5.2 限定为 Neige 集成范围，CLI 能力列为外部合同；plan mode 等改为“kernel 未集成，用户 settings 未知” |
| adversarial N1：K6 须限定 Unknown 残余 | K6 与 §3.5 改写：暂态可重试收敛，Unknown 保留到宿主 boot 改变 |
| adversarial N2：`mcp.json` 现状描述有误 | 核实成立（`worker_mcp.rs:47-50`）。§1、§3.3 改正：改变的是 token 传输与驻留；`token_only_in_payload` 明确覆盖的位置 |
| adversarial N3：无 Engine 与 Legacy 共存的反向验收 | §3.3、§10 限定 Engine 检查只在新形状入口；新增 `w_legacy_claude_worker_row_recovers_without_engine` |
| 用户输入 1：`-p` 中途插话与常驻 | 新增 E7、§3.1.1 两种形状对比、OQ11；推荐每 turn 一 execution |
| 用户输入 2：terminal-worker 执行方式 | §5.5：只用 Pipe，不提供 PTY 变体；核实 KeysOnly 的可达路径与无自动调用方；失去项；资源与机制对比；WT、OQ7、WR2、切片表改写 |
| Planner 消息 f45487bb（WorkerBench） | §5.6：源码中只有 terminal 卡的 Workbench 放置，无独立服务，逐项带出处；兼容性、资源与推荐分层；新增 OQ12；对 WT 与 C5 的影响 |
| Planner 消息 c4b98977（用户决定：共享终端冲突） | §5.6、OQ12：状态冲突接受为已知风险、处置为重试，标为用户决定，不加仲裁/加锁/独占；凭据不进共享环境，写明依据（`terminal_hooks.rs:40-49`、`terminal_adapter.rs:277-288`）与验收 |

### 11.2 第二轮（第二版 → 第三版）

| 发现 | 处置 |
|---|---|
| boundary B1：共享收尾分派的当前 session 围栏只围住租约释放，没围住先发生的 Kernel task 失败写 | 核实成立：分派器先以 `TaskReporter::Kernel` 失败 task（`reaper/mod.rs:406-413`），Kernel 绕过 card 守卫（`task.rs:372-384`、`:655-668`），之后才同事务释放租约（`:431-438`）；缺绑定与竞争失败的兜底在事务外（`:381-391`、`:445-450`）；Legacy restart 终结旧 session 并改指 `cards.session_id`（`claude_restart_adapter.rs:248-254`、`:310-327`；`session_mirror.rs:318-342`）。§3.7：分派器改为单写事务，当前性围栏先于 task 写与所有租约兜底，过时结果无副作用返回；task 失败写改用 card 精确所有权；兜底移进同一事务。新增经真实 reaper 的“探测后暂停 → Legacy restart → 旧结果返回”与“attempt 被替换”用例；§6 WR1、§10 与 workspace-next-steps WR1 摘要同步 |
| adversarial B1：boot 续跑意图与原发行 op（含首 spawn op）之间无持久消费点与责任移交 | 核实成立（`driver.rs:588-623`、`:784-824`；`provider_execution/mod.rs:257-273`）。按 AGENTS.md 的“收窄而非叠加”处理：§3.6 改为一种持久行、每行一个消费点。首 turn 也是 turn 行，在 spawn op 的准备事务里写，spawn op 不准入；boot 只停不写行；每行至多准入一个 execution（Pending→TxCommitted 的单次 CAS），重驱调用无驱动者 guard 只 park；唯一消费点是 worker owner `complete` 以精确 execution 所有权为条件的事务，它用 `complete_parked_tx`（`writes.rs:366-391`）终结记录该 execution 的行，并只在实际终结的那次事务里写以该 execution 为键的续跑行。崩溃点逐一列表，验收全部经 `recover_on_boot`、boot 通道与 sweep 的生产入口 |
| adversarial B2：收尾谓词把合法的首 turn 未 stamp 窗口当成绑定失效 | 核实成立（`worker_preparation.rs:57-79`；`scheduler/mod.rs:1428-1441`；`reaper/mod.rs:402-439`；`task.rs:372-384`）。§3.7：首 turn 行与 session 同事务写入，未终结的行阻止关闭；绑定判定复用既有的 op 目标证明 `worker_op_targets_card_tx`（`task.rs:312-330`），单独的 NULL 不算绑定也不算失效；§3.3 授权第 4 条同步收紧。新增两个窗口的用例与对应 mutation |
| 同类扫查：所有写 task 或租约的收尾路径、所有发行入口 | §3.7 的扫查表（超时、自报、取消、清理执行、owner complete、WT 完成）与 §3.6 的发行入口（首 turn、消息、手动、续跑同一种行；spawn op 与 boot 不发行）。另发现并修正：§3.3 引用的 `ReleaseInterrupted` 不是今天的 `StopReason` 变体（`provider_execution.rs:40-52`），改为“按自己的原因停止、以 Releasing 形状判定未送达” |
| adversarial N1：mutation 表不能作为完整红集预测 | §7 把表标为实施前假设，须在测试落地后按名校准；修正被其他围栏吸收的两行（跳过 `admission_gate`、放行 Verifying），并新增隔离条件的 `w_turn_admission_finishes_settled_driverless_claim` 与 `w_worker_authority_table` |
| adversarial N2：WorkerBench 凭据验收只查持久 env | §5.6 把结论限定为“W 不新增凭据路径”，写明继承白名单含代理键（`child_environment.rs:36-42`）这一既有路径另立 issue；验收断言相应收窄 |
| 用户确认（ask 686425，第三版经两路 L2 设计评审第 3 轮通过之后） | 路线“确认，合并设计”；开放问题“全部按推荐”。状态行改为定稿；§0 第 9 条与 §5.4 把路线 A、terminal-worker 只用 Pipe、OQ12、codex-worker 不动、OQ1、OQ2、OQ3、OQ4/OQ10、OQ11、OQ8 标为已定（用户）；OQ5、OQ6、OQ7、OQ9 保持开放，按推荐推进，由对应切片定。仍只是设计，未实现 |
