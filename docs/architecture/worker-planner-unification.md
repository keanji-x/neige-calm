# Worker 与 Planner 协议会话底座统一设计

## 1. 背景与已拍板边界

当前 Neige 的 Planner 与 Agent Worker 运行在两套完全割裂的执行栈上：

- **Planner（及 System Assistant）** 走无头协议会话栈（`PlannerHarness` + `PlannerBackend` + `CatalogAccess`），原生支持 `codex`、`claude`、`antigravity`、`opencode`、`grok` 五类 Provider 的会话创建、模型目录校验、动态切模、流式事件（`LiveReplies`）与结构化对话转录（`conversation_v1`）。
- **Agent Worker** 走早期的任务适配器双栈：`CodexWorkerAdapter` 虽然借用了 `SharedCodexAppServer`，但写死了 `TurnModelSelection::inherit()` 并额外拉起 `OptionalViewer` PTY 终端进程；`ClaudeWorkerAdapter` 直接在交互式 PTY 终端内启动 TUI 进程、靠模拟括号粘贴发消息；两者的输出展示还依赖 `WorkerFlowDriver` 轮询磁盘上的工具私有会话文件。

针对本次架构收敛，已确认以下三条核心设计边界：

1. **V1 Provider 范围（可暂不支持 Claude Worker）**：V1 先将 `codex` 与全部三类 `ACP` Provider（`antigravity`、`opencode`、`grok`）统一到同一套无头协议会话底座；`claude` Worker 在 V1 明确标记为不支持（同步清理旧 PTY `ClaudeWorkerAdapter` 及尚未接通生产入口的 `crates/calm-server/src/claude_worker/` 实验代码，预留后续以轻量角色配置接回的扩展点）。
2. **Planner 可按任务指定模型**：Task 声明契约支持由 Planner 显式指定 `provider`、`model` 与 `reasoning_effort`（未指定时回退到默认规则），并在任务准备阶段复用 `CatalogAccess` 完成合法性校验。
3. **Agent Worker 彻底砍掉 PTY 终端**：仅 `kind: "terminal"` shell 任务保留 PTY 终端与终端卡片；所有 Agent Worker 不再创建 `terminals` 记录、不拉起任何本地 TUI/Viewer 进程，统一收口为无头结构化对话卡片，直接复用现有对话只读 API 与前端会话视图。

---

## 2. 代码库现状核查（锚点事实）

| 维度 | 当前实现位置 | 现状事实与痛点 |
| --- | --- | --- |
| 任务声明契约 | `crates/calm-types/src/report_blocks/tasks.rs`、`crates/calm-report/` | `TaskKind` 仅有 `codex` / `claude` / `terminal` / `compact`；无 `provider`、`model`、`reasoning_effort` 字段；子 Track 派生目前还硬编码限制 `kind == Codex`。 |
| 调度分发入口 | `crates/calm-server/src/scheduler/mod.rs`、`scheduler/running_worker.rs` | 按 `TaskKind` 硬分叉到 `CodexWorkerAdapter` 与 `ClaudeWorkerAdapter`；另靠 `WORKER_IDLE_TURN_GRACE`（300 秒轮询宽限期）兜底“Turn 已结束但未调用 `neige_task_done/fail`”的僵尸会话。 |
| Codex Worker | `crates/calm-server/src/operation/codex_adapter/{worker.rs, worker_spawn.rs}` | 硬编码 `TurnModelSelection::inherit()`，无法指定模型；额外创建 `Terminal` 行并拉起 `codex resume --remote` 终端视图进程。 |
| Claude Worker | `crates/calm-server/src/operation/claude_adapter/mod.rs`、`claude_worker/` | 生产路径跑交互式 PTY TUI，无 `--model` / `--effort` 参数；`claude_worker/`（WA2）处于 `#![allow(dead_code)]` 未接线状态，且未接入统一模型目录与卡片绑定。 |
| 私有日志抓取 | `crates/calm-server/src/worker_flow/mod.rs` | 旁路扫描 `~/.codex/sessions/**/rollout-*.jsonl` 与 `~/.claude/projects/**/*.jsonl` 还原 Worker 输出，直接耦合外部 CLI 私有目录布局。 |
| 运行期发信 | `crates/calm-server/src/terminal_interaction/message.rs` | 写 `planner_messages` 表后，靠 `TuiInput::BracketedPasteSubmit` 往 PTY 终端塞入提示文本，与终端强绑定。 |
| 可复用的协议与目录层 | `crates/calm-server/src/{harness/backend/, acp_planner/, planner_model/}` | `PlannerBackend` 已封装 `Codex` 与 `Acp` 的线程启动/恢复、逐 Turn 模型参数注入（`TurnModelSelection`）、流式 `PlannerEvent` 与原生 `interrupt`；`CatalogAccess` 已实现跨 Provider 模型/推理强度校验。 |
| 可复用的卡片与前端读链路 | `crates/calm-truth/src/planner_binding.rs`、`routes/conversation_items.rs`、`fe/web/src/systems/cards/builtins/` | 只要卡片能解析出 `PlannerBinding`，后端 `/api/cards/{id}/harness/items?contract=conversation_v1`、`/harness/live`、`/planner/run` 三个只读端点以及前端无头卡片（`headless: true`）对话组件即可零新建接口直接工作。 |

---

## 3. 核心架构设计：改哪里、怎么改最好

### 3.1 总体取舍：共享协议/转录底座 + 精简 `WorkerTurnRunner`（不污染 `PlannerHarness` 人类会话状态机）

在代码组织上，最关键的架构判断是**“下四层彻底统一，最上层保持薄角色驱动器分离”**：

- **为什么不直接把 Worker 塞进 `crates/calm-server/src/harness/run_loop.rs`（约 4,811 行）？**
  `PlannerHarness` 的主循环围绕“人类持久化交互会话”构建，重度耦合了消息撤回（withdraw）、历史回滚（rewind）、浏览器会话身份校验（`HumanInputSource`）、人工权限审批挂起队列（`HeldRequests`）以及 Track 事件水位线追赶（`push_watermark`）。而 Task Worker 是受内核租约（`workspace_lease`）约束的**无人值守单次 Attempt 执行器**：它不需要回滚与撤回，越界审批必须立即自动拒绝，且一旦 Turn 自然结束又没有提交任务结果，必须确定性收敛为失败并释放工作区锁。硬塞入 `run_loop.rs` 会产生大量角色分支判断并放大回归风险。
- **最佳实践切法（四层完全共享 + 约 500 行精简 `WorkerTurnRunner`）**：
  1. **共享协议后端层（`PlannerBackend` 泛化为 `AgentBackend`）**：直接复用 `Codex` 与 `Acp` 后端的会话生命周期、`TurnModelSelection` 注入、流式事件读取、`ProviderExecution` 进程监管与原生中断。
  2. **共享模型校验层（`CatalogAccess`）**：复用 `planner_model` 目录缓存与校验逻辑，统一解析 `(provider, model, reasoning_effort)`。
  3. **共享转录与实时流层（`LiveReplies` + `conversation_v1` 投影）**：复用相同的原生事件落库投影与内存流式广播，彻底淘汰 `WorkerFlowDriver` 磁盘 JSONL 轮询。
  4. **共享卡片绑定与 HTTP 读路由（`PlannerBinding`）**：扩展 `HarnessProfile::TaskWorker`，零新增 REST 读接口打通前后端对话展示。
  5. **薄执行循环（`AgentWorkerAdapter` + `WorkerTurnRunner`）**：外层原封不动保留 `workspace_lease::worker` 工作区租约、`CardDecisionSink::commit_worker_task_report`（`neige_task_done` / `neige_task_fail`）、Gate 验证机与 Git 候选提交交付；内层仅用约 250 行状态清晰的异步 Turn 循环替代原有的 PTY 管理与 300 秒轮询。

架构分层对比如下：

```text
Report Task 声明 (kind: "agent" | "codex", provider?, model?, reasoning_effort?)
        │
        ▼
   Scheduler ──► workspace_lease::worker (保留：干净工作区检查、head/upstream 检出与锁)
        │
        ▼
AgentWorkerAdapter + WorkerTurnRunner (新增精简执行器，无 Terminal / 无 PTY)
   ├── 模型解析：CatalogAccess 校验 (provider, model, reasoning_effort) -> TurnModelSelection
   ├── 协议执行：共享 AgentBackend (Arm::Codex | Arm::Acp) + 无人值守权限策略
   ├── 实时转录：共享 LiveReplyWriter + 标准对话转录投影 -> GET /api/cards/{id}/harness/{items,live}
   ├── 运行期通信：planner_messages + Codex turn/steer 或 ACP 下一 Turn 自动续跑队列
   └── 结算收敛：CardDecisionSink (done/fail -> Gate -> Git 交付) 或 Turn 结束即时 worker-turn-ended
```

---

### 3.2 六个核心子模块设计（D1–D6）

#### D1. 任务声明契约与存储演进（支持指定 Provider / Model / Reasoning Effort）

1. **Report Task Schema（向后兼容演进）**：
   - 新增标准任务类型 `kind: "agent"`，并允许在 Agent 任务上声明可选字段：
     - `provider?: "codex" | "antigravity" | "opencode" | "grok"`（V1 若声明 `"claude"` 或旧 `kind: "claude"`，在声明诊断或调度准备阶段返回明确的 `unsupported_worker_provider` 错误，不静默降级）。
     - `model?: string`（非空字符串，对应目标 Provider 模型目录中的合法 `model_id`）。
     - `reasoning_effort?: string`（可选推理档位，如 `"low"` / `"medium"` / `"high"` / `"xhigh"`；目标模型不支持时由 `CatalogAccess` 给出精确诊断）。
   - **兼容保留 `kind: "codex"`**：作为 `kind: "agent"` 且默认 `provider = "codex"` 的完全等价别名（同时支持显式附加 `model` 与 `reasoning_effort`），保证现有模板、提示词与历史报告无需迁移即可继续工作；若 `kind: "codex"` 同时传入非 `"codex"` 的 `provider`，直接报声明冲突诊断。
   - **默认回退规则**：
     - 当 `kind: "agent"` 未显式指定 `provider` 时，优先继承当前 Track Planner 卡片的 Provider（若 Planner 为 `claude` 等 V1 暂不支持作为 Worker 的 Provider，则回退至安装级默认 `codex`）。
     - 当 `model` / `reasoning_effort` 未指定时，复用 `CatalogAccess` 解析该 Provider 的安装默认模型与默认推理强度；对于 `spawn: "sub-track"` 子 Track 任务，同步放开原先仅限 `codex` 的限制，并将所选 `(provider, model, reasoning_effort)` 透传给子 Track 的初始卡片模型配置（复用已有的 `child_selection.rs`）。
2. **SQLite 增量迁移**：
   - 新增单文件前向迁移（位于唯一的 `crates/calm-truth/migrations/` 目录下），为 `tasks` 表增加可空列 `provider TEXT`、`model TEXT`、`reasoning_effort TEXT`，并在 `TaskKind` 枚举与 `worker_sessions` 类型约束中补充 `agent` 与 `managed-worker`（严守已发布迁移字节冻结规则，不触碰历史 `.sql`）。

#### D2. 卡片与会话身份统一（`HarnessProfile::TaskWorker`）

1. 在 `calm-types` 中为 `HarnessProfile` 新增 `TaskWorker`（序列化为 `"task_worker"`），为 `WorkerSessionKind` 新增 `ManagedWorker`（序列化为 `"managed-worker"`）。
2. `AgentWorkerAdapter` 在准备任务时创建：
   - **`Card`**：`kind = "codex"`（保持现有无头会话卡片家族约定）、`role = CardRole::Worker`，`payload` 写入 `harness_profile = "task_worker"`、`worker_provider = <AgentProvider>` 以及所选 `model` / `reasoning_effort` 元数据（**不再创建 `terminals` 记录，也不写入 `terminal_id`**）。
   - **`WorkerSession`**：`kind = ManagedWorker`、`provider = WorkerProviderKind::from(provider)`、`contract = WorkerContract::Executor`、`mode = SessionMode::Resumable`，签发会话绑定的 MCP Bearer Token（完全符合现有的 `session_get_by_active_token_hash` 鉴权门禁）。
3. 扩展 `crates/calm-truth/src/planner_binding.rs` 中的 `PlannerBinding::from_card`：当 `expected_role == CardRole::Worker` 且 `harness_profile == TaskWorker` 时，从 `worker_provider` 与卡片模型字段解析出完整 `PlannerBinding`。由此，现有的 `card_runs_headless_harness`、`/api/cards/{id}/harness/items`、`/api/cards/{id}/harness/live` 与 `/api/cards/{id}/planner/run` 自动对 Worker 卡片生效。

#### D3. 共享 `AgentBackend` 与无人值守权限边界

1. **泛化后端构造器**：将 `crates/calm-server/src/harness/backend/` 中的 `PlannerFactory::open` 扩展为接收角色配置（`Planner` / `Assistant` / `TaskWorker`）：
   - **指令装填**：对 `TaskWorker` 使用 `SeededCardRole::Worker` 渲染执行器系统提示词（`prompts/` 下的 Worker 契约说明），替代 Planner 的规划提示词；MCP 可见性继续由现有的 `mcp_server/tool_visibility.rs` 按 `CardRole::Worker` 严格裁剪（仅暴露 `neige_task_done`、`neige_task_fail`、`neige_message_read`、`neige_cat`、`neige_ls` 及读写权限允许的只读工具）。
   - **无人值守工具审批（ACP 与 Codex 对齐）**：
     - Codex 分支沿用现有的工作区沙箱配置（`ReadOnly` 任务对应只读沙箱，写任务对应 Track 工作区写权限 + `ApprovalPolicy::Never`）。
     - ACP 分支（`acp_planner/wiring.rs`）为 `TaskWorker` 提供专用的无人值守审批器：对允许范围内的工作区读写/命令（写任务）或只读操作（`access: "read_only"` 任务）及已授权 `neige` MCP 工具自动批准；对越界请求或交互式提问立即返回确定性拒绝（如 `NO_APPROVER` / 权限拒绝响应），绝不挂入 `HeldRequests` 等待人类点击。

#### D4. `AgentWorkerAdapter` 与 `WorkerTurnRunner` 执行与即时收敛循环

`WorkerTurnRunner` 承接单个任务 Attempt 的完整协议生命周期：

1. **启动与首轮 Turn**：调用 `AgentBackend` 打开原生会话（记录 `native_session_id` / `thread_id` 到 `worker_sessions`），将 `render_task_worker_prompt_tx` 生成的任务目标、验收标准与上下文连同解析好的 `TurnModelSelection` 一起提交为首个 Turn，并将 `TurnInput` 写入对话转录。
2. **流式事件落库**：消费 `PlannerEvents` 事件流，驱动共享的 `LiveReplyWriter`（实时推送思维链、正文增量与工具调用状态）并调用共享转录持久化函数写入 `conversation_v1` 条目。
3. **Turn 结束后的三分支确定性判定（彻底消除 300 秒轮询）**：
   - **分支 A（已交卷或已取消）**：Worker 在 Turn 内已调用 `neige_task_done` 或 `neige_task_fail`（任务状态已进入 `Done` / `Verifying` / `Failed`），或已被 `neige_task_cancel` 取消 —— 正常关闭后端会话并退出 Runner。
   - **分支 B（运行期收到 Planner 新消息）**：任务仍处于 `Running`，且在当前 Turn 期间收到了尚未投递的 `planner_messages` —— 在同一原生会话（同一 `thread_id`）上立即发起跟进 Turn，注入 `[neige] Planner message` 提示其调用 `neige_message_read` 并完成后续汇报。
   - **分支 C（未交卷即静默结束）**：任务仍处于 `Running` 且无待投递的 Planner 消息 —— **立即在单事务内将 Attempt 结算为失败**（`failure_code = "worker-turn-ended"`，释放 `workspace_lease` 并唤醒 Planner），不再需要 `scheduler/running_worker.rs` 的 300 秒空闲宽限期轮询。
4. **进程崩溃与重启恢复**：若后端进程异常退出则按协议错误立即失败并释放租约；服务重启时，启动恢复逻辑扫描未终结的 `ManagedWorker` 会话，统一按中断恢复策略结算失败并归还工作区锁。

#### D5. 运行期通信与取消控制（去终端化）

1. **Planner 向运行中 Worker 发信**：
   - 保留 `planner_messages` 数据表与 `CardDecisionSink` 中的“存在未读 Planner 消息时拒绝 `neige_task_done`”安全栅栏（`decision_sink.rs`）。
   - 移除 `TuiInput::BracketedPasteSubmit` 终端粘贴投递；改为内存通道直接通知活跃的 `WorkerTurnRunner`：
     - 若后端支持运行期插入（`backend.metadata().steer == true`，即 `Codex`），立即调用 `steer_delivery` 插入提醒；
     - 若后端不支持运行期插入（`ACP`），标记待续跑通知，待当前 Turn 一结束即自动走 **分支 B** 续跑下一 Turn。
   - **MCP 工具入口**：为避免突破 `crates/calm-server/src/mcp_server/tools/mod.rs` 的 Planner MCP 描述字节预算（上限 `42_097` 字节），可直接新增极简签名的 `neige_task_message`（参数仅 `attempt_id` 与 `text`），同时随着 Agent Worker 去终端化精简 `neige_terminal_*` 工具描述中面向 Worker 的冗余说明，保持总字节预算下降；对 Agent Worker 的 `attempt_id` 调用 `neige_terminal_read/show/control` 则返回清晰的“Agent Worker 使用结构化对话而非终端”的提示错误。
2. **任务取消（`neige_task_cancel`）**：直接调用 `AgentBackend::interrupt` 中断当前 Turn，并通过 `ProviderExecution::stop_session` 终止受管子进程组，回收干净利落。

#### D6. 前端无头 Worker 卡片与旧栈退役

1. **前端（`fe/`）无头卡片注册与会话抽屉打通**：
   - 在 `fe/web/src/systems/cards/builtins/` 新增 `worker.ts`（匹配 `role === "worker"` 且 `harness_profile === "task_worker"`，声明 `headless: true, component: () => null`），并在 `codex.ts` 中排除 `task_worker`，确保画布不渲染空白终端框。
   - 在 Report 任务块与卡片列表中，点击 Worker Attempt 直接打开现有的标准 `Conversation` 会话面板，展示实时流式输出、工具调用详情以及实际运行的 `provider / model` 徽标。
2. **旧栈集中清理（在切流稳定后删除）**：
   - 删除 `CodexWorkerAdapter` 的 PTY Viewer 派生逻辑（`worker_spawn.rs` 等）；
   - 删除 `ClaudeWorkerAdapter` 的 Worker PTY 路径与尚未接线的 `crates/calm-server/src/claude_worker/`（WA2）；
   - 删除 `crates/calm-server/src/worker_flow/`（磁盘私有 rollout/JSONL 抓取器）；
   - 删除 `terminal_interaction/message.rs` 的括号粘贴投递与 `scheduler/running_worker.rs` 的 300 秒空闲轮询。
   - *注：`TerminalAdapter`（`kind: "terminal"` shell 命令任务）与用户手动打开的交互式终端（`neige_terminal_open`）完整保留，不受影响。*

#### 附：后续恢复 Claude Worker 的预留路径（V1.1）

由于 `claude_planner` 已经实现了完整的无头 `stream-json` 双向协议会话与 `TurnModelSelection` 参数透传，V1 之后若需支持 Claude Worker，**无需重建适配器**，只需在 `crates/calm-server/src/claude_planner/role.rs` 中增加约 150 行的 `WorkerRole` 分支（放开工作区工具集与本地配置策略、挂载无人值守权限回调），即可直接接入 `AgentBackend::Claude`。

---

## 4. 修改范围清单与代码量评估（LOC 估算）

按生产代码（不含自动生成文件）统计，本次改造**预计新增/修改约 1,700 ~ 2,150 行 Rust/TS 生产代码**（补充单测与契约测试约 **+800 ~ +1,050 行**），同时在切流完成后**可净删约 2,550 ~ 3,200 行历史包袱代码**，最终代码库**净减少约 800 ~ 1,000 行**：

| 模块层级 | 主要涉及路径 | 改动内容摘要 | 新增/修改生产代码 | 可删除旧代码 |
| --- | --- | --- | --- | --- |
| **1. 类型、契约与数据库迁移** | `crates/calm-types/src/{harness.rs, worker.rs, report_blocks/tasks.rs}`<br>`crates/calm-report/`<br>`crates/calm-truth/src/{planner_binding.rs, db/sqlite/}`<br>`crates/calm-truth/migrations/02XX_*.sql` | `HarnessProfile::TaskWorker`、`WorkerSessionKind::ManagedWorker`；Task 声明新增 `kind: "agent"` 及 `provider` / `model` / `reasoning_effort` 校验；`tasks` 表增量迁移；`PlannerBinding` 支持 Worker 卡片。 | +320 ~ +400 行 | 0 行 |
| **2. 共享协议底座与无人值守策略** | `crates/calm-server/src/harness/{backend/, instructions.rs}`<br>`crates/calm-server/src/acp_planner/{profile/, wiring.rs}`<br>`crates/calm-server/prompts/` | 将 `PlannerBackend` / `PlannerFactory` 泛化支持 `TaskWorker`；装填 Worker 系统提示词；实现 ACP Worker 无人值守审批器（自动放行授权范围、立即拒绝越界请求）。 | +350 ~ +450 行 | 0 行 |
| **3. `AgentWorkerAdapter`、Runner 与通信** | `crates/calm-server/src/operation/agent_worker_adapter/`（新）<br>`crates/calm-server/src/scheduler/`<br>`crates/calm-server/src/mcp_server/tools/`<br>`crates/calm-server/src/routes/planner_cards.rs` | 无 PTY 任务准备与 `CatalogAccess` 模型解析；`WorkerTurnRunner` 事件流落库与 Turn 结束三分支即时收敛；`neige_task_message`（`steer`/续跑队列）与原生 `cancel`；子 Track 模型透传。 | +750 ~ +950 行 | 0 行 |
| **4. 前端无头 Worker 卡片与会话挂载** | `fe/core/domain/`、`fe/core/api/`<br>`fe/web/src/systems/cards/builtins/{worker.ts, codex.ts, register.ts}`<br>`fe/web/src/features/`（任务跳会话面板） | 生成新 OpenAPI 类型；注册 `headless: true` 的 Worker 卡片；从 Report 任务状态区一键打开标准对话流抽屉并展示 `provider / model`。 | +280 ~ +350 行 | -50 ~ -100 行 |
| **5. 旧双栈与私有文件抓取退役** | `crates/calm-server/src/operation/codex_adapter/worker*.rs`<br>`crates/calm-server/src/operation/claude_adapter/`<br>`crates/calm-server/src/claude_worker/`<br>`crates/calm-server/src/worker_flow/`<br>`crates/calm-server/src/terminal_interaction/message.rs` | 移除 Codex Worker PTY Viewer、Claude PTY Worker、未接线的 `claude_worker`（WA2）、磁盘 JSONL 轮询器 `WorkerFlowDriver`、括号粘贴发信及 300 秒空闲轮询。 | — | **-2,500 ~ -3,100 行** |
| **合计（生产代码）** | — | **统一 4 类 Provider + 动态选模 + 彻底无终端化** | **+1,700 ~ +2,150 行** | **-2,550 ~ -3,200 行** |

---

## 5. 推荐实施拆分顺序（4 个可独立合入的 PR）

为降低单次变更半径并满足仓库审查分级要求，建议按以下 4 个阶段推进：

1. **PR 1 — 任务契约、存储迁移与 Worker 卡片绑定（审查级别：L2，涉及数据库迁移与持久化契约）**
   - 落地 SQLite 迁移（`tasks` 新增 `provider` / `model` / `reasoning_effort`）、`TaskKind::Agent` 与声明校验诊断（含拒绝 `claude` Worker 的清晰诊断）、`HarnessProfile::TaskWorker`、`WorkerSessionKind::ManagedWorker` 与 `PlannerBinding` 扩展，同步更新 OpenAPI 与前端生成类型。
   - *阶段产物*：前后端契约与存储就绪，现有运行行为零变化。
2. **PR 2 — 共享 `AgentBackend` 泛化与 ACP/Codex 无人值守执行配置（审查级别：L2，涉及工具权限与隔离边界）**
   - 解耦 `PlannerFactory` 与 `acp_planner` 中的 Planner 专属假设，加入 `TaskWorker` 系统指令渲染与无人值守权限审批器（含只读任务与写任务的边界单测及变异验证）。
   - *阶段产物*：`Codex` 与三类 `ACP` Provider 均可通过统一后端接口以 `Executor` 角色跑通单测会话。
3. **PR 3 — `AgentWorkerAdapter`、`WorkerTurnRunner` 调度切流与运行期消息（审查级别：L2，涉及调度、租约释放与 MCP 工具面）**
   - 实现 `AgentWorkerAdapter` 与 `WorkerTurnRunner`，接管 `kind: "agent"` 与 `kind: "codex"` 调度分发；接入 `CatalogAccess` 模型校验、对话转录落库、Turn 结束即时收敛与 `neige_task_message`；同步放开子 Track 任务的 Provider/模型指定。
   - *阶段产物*：后端全链路跑通四类 Provider 的无终端 Worker 执行与模型选择。
4. **PR 4 — 前端无头 Worker 对话视图接入与旧栈彻底清理（审查级别：L1）**
   - 前端落地 `WORKER_CARD_ENTRY` 与任务对话抽屉跳转，完成真实浏览器预览验证；随后在同一或紧随的清理提交中整体移除 `worker_flow`、`claude_worker`、旧 PTY Worker 启动器与括号粘贴逻辑。
   - *阶段产物*：端到端体验闭环，净删约 2,500+ 行历史代码。

---

## 6. 关键风险与门禁检查点

实施各阶段 PR 时需重点看护以下仓库级不变量与门禁：

1. **只读任务与越界审批安全（L2 核心不变量）**：`access: "read_only"` 的 ACP Worker 必须在协议权限回调层硬性拦截一切写文件与非只读终端请求，需对权限拦截分支执行变异验证（Mutation Verification）。
2. **工作区租约零泄漏**：`WorkerTurnRunner` 的所有退出路径（正常交卷、Turn 提前结束、协议报错、取消、进程崩溃、重启恢复）都必须通过事务原子释放 `workspace_lease`，确保不会遗留阻塞后续任务的脏锁。
3. **Planner MCP 工具面字节预算**：修改或新增 `neige_task_message` 时，必须运行 `mcp_server` 契约测试（守护 `42,097` 字节上限与 MCP 目录 golden 快照）。
4. **OpenAPI 与前后端契约同步**：涉及 `calm-types` 路由模型变更时，运行 `scripts/local-rust-gates.sh --quick --base origin/main` 与 `(cd fe && npm run gen:api && npm run lint && npm run build)`。
5. **仓库文本棘轮门禁（`scripts/local-ratchet-gates.sh`）**：新增 Worker 提示词必须放入 `crates/calm-server/prompts/*.md`（避免触发 `gate-prose-ratchet.sh` 的 Rust 中文/长字符串基线）；全仓命名避免引入已退役历史术语（守护 `gate-1316-terminology-ratchet.sh` 与 `gate-1209-template-rename-residual.sh`）。
