# 按需 track worktree 设计草案（第二版）

状态：已被 [track-cow-view.md](track-cow-view.md)（第三版）取代，保留作历史。基于 `22b530cce`。背景调研见
[research-track-workspace-cow-optional.md](research-track-workspace-cow-optional.md)。

## 为什么改版

第一版让 Lazy track 的 agent 直接在用户 checkout 里运行，再用沙箱、准入等栅栏保证只读。
独立源码评审（`lazy-design-review`）给出 7 个阻塞发现，几乎都是同一类：**能碰到用户 checkout 的执行通道太多，
逐个设栅栏堵不完**。

| # | 漏洞 | 证据 |
|---|---|---|
| B1 | Codex 每轮按卡片权限模式重设 `workspaceWrite`/`dangerFullAccess`，只改 `thread/start` 无效；PlainChat、ACP（无 OS 隔离）、Claude ask/full 模式未覆盖 | `provider/src/codex/approvals.rs:38-63`、`acp_planner/process.rs:33-56`、`claude_planner/spawn.rs:48-59` |
| B2 | 现有只读 worker 进程本身仍可写；分支/clean-tree 规则不适用于用户的 main 和脏 checkout | `shared_codex_appserver.rs:959-962`、`workspace_lease/worker.rs:223-249` |
| B3 | 显式 cwd、terminal 任务、Claude 卡片重启、gate/verify 的 `/bin/sh` 都能落到用户 checkout；裸 PTY 可 `cd` 出去 | `terminal_adapter.rs:222-226`、`task_verify_adapter/mod.rs:373-416` |
| B4 | Worker 的 forge 动作由内核代跑 `git add -A && git commit`，绕过进程沙箱 | `mcp_server/transport.rs:1117-1147`、`calm-types/src/forge_git.rs:26` |
| B5–B7 | 旧行迁移与写闩、删除竞态与失败状态、前端 `worktree: string` 契约与文件预览根 | 见评审原文 |

用户的两个追问指向同一问题：Planner 只想在 main 上部署而不改代码；git 快照保护不了被 ignore 的文件、
仓库外文件和外部副作用，并发时也分不清是谁改的。按 AGENTS.md，阻塞发现反复出现说明**方案结构有问题**，
应改结构，而不是继续加栅栏。

## 新的不变量

**agent 启动的任何进程，都不以用户 checkout 为 cwd 或可写根。用户 checkout 只有用户自己碰。**
其余目录都归 kernel 所有，可丢弃、可重置。

这样“保护用户 checkout”靠位置保证，不再依赖各 provider、各权限模式的沙箱是否完备；
栅栏退化为 kernel 自有目录的卫生问题，出了错可以自动恢复。

## 设计

### D1 两层工作区

| 层 | 是什么 | 谁用 | 写了怎么办 |
|---|---|---|---|
| 参考副本（每仓库一份） | kernel 维护的 detached worktree，跟随上游 main，带入本地环境 | Lazy track 的 Planner/assistant、只读任务 | kernel 每轮开始前 `git status` 检查，脏了就重置——这份目录没有人类编辑，重置无归属问题 |
| track worktree（按需） | 现有的 `neige/track-<id>` worktree，从上游 main 建出 | 任何会“运行或写”的东西：读写任务、terminal 任务、Planner 驱动的终端、部署 | 现有交付流程 |

- 参考副本取代第一版的“在用户 checkout 里只读”。代价：看不到用户未提交的本地改动；需要时由用户提交或用户自己操作。
- 参考副本数量与仓库数相同，不随 track 增长，缓解 #1815。

### D2 本地环境带入（两层共用）

仓库提交一份声明（形式可参考 Claude Code 的 `.worktreeinclude`），每项选 `copy`（小文件如 `.env`）、
`link`（大依赖，只读共享）或 `setup`（命令重建）。只接受被 `.gitignore` 忽略的路径。
仓库外 `../` 依赖需要把 kernel 目录放到与仓库同级同深度的位置，单独决定。

### D3 部署与“只运行不修改”

Planner 想“基于 main 部署一个东西”时，在 track worktree 里运行：它从上游 main 建出、带入了本地环境，
在里面部署**就是**部署 main，且构建产物、端口、进程与其他 track 隔离。代价是首次建 worktree 的几秒和约 70 MB。
要部署**用户 checkout 的当前状态**（含未提交改动）是用户自己的动作，在用户自己的终端里做。

### D4 文件以外的副作用

部署到服务器、改数据库、调用外部服务，任何文件快照都保护不了，也不是工作区设计能解决的。
这类动作走已有的逐次用户批准机制（#2617），不在本设计范围。

### D5 终端

- 用户自己打开的终端：落在用户指定的位置，包括自己的 checkout（用户本人的 shell）。
- agent 驱动的终端、terminal 任务、gate：落在 track worktree（按需创建）。参考副本上不跑终端。

### D6 按需创建（沿用第一版并按评审补全）

- worktree 路径照旧在创建时写入并冻结；另加只允许 `NULL → 时间戳` 的 `materialized_at`，由 calm-truth 的单独
  CAS 写入，只有真正把它从 NULL 改成值的那次发事件。旧行迁移时按实际注册情况补写（B5）。
- 升级入口在调度器 claim 之后、提交 op 之前：持删除 fence 并重读 track/task 后再 ensure；
  临时失败保留可重驱状态，永久失败明确终态；catch-up 任务先建成再刷新上游（B6）。
- Planner 的 cwd 终身是参考副本（Codex 线程 cwd 不变的约束），通过路径读取 track worktree 的产物。
- 前端 schema、事件载荷、文件预览根随类型一起改，并定义旧事件兼容（B7）。

## 待验证

- 共享的 Codex app-server 是否允许按 track 给不同 cwd 与沙箱（`shared_codex_appserver.rs`）。
- 参考副本的刷新时机：每轮前快进，还是固定在 track 创建时的 commit（可复现性 vs 新鲜度）。
- 参考副本被重置时，正在读它的其他 track 会看到中间状态；需要“检查—重置”与读者之间的锁或版本切换（例如两份交替）。

## 分片（L2）

1. 环境带入声明（独立、收益最大，也让现有 worktree 立即受益）。
2. 参考副本：创建、刷新、检查与重置。
3. Lazy 模式：类型/迁移/创建参数，Planner 落参考副本。
4. 按需升级与终端落点。
5. 前端开关与模板默认值。
