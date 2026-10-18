# Workspace 下一步：S2a / M1 之后的 resource cutover 与隔离等级（#2666 re-plan）

状态：重新规划，只改文档，不改代码，不声称已实现。核实基线为 main `91e004a83`（S2a E1a–E2b 已合入 #2795–#2861，
M1 W1–W4 已合入 #2866–#2871）。父合同为 [track-workspace.md](track-workspace.md)；本文盘点
[workspace-launch-admission.md](workspace-launch-admission.md)（S2a 之前写成，仍标注待 fresh 评审）在
[workspace-execution.md](workspace-execution.md) 与 [workspace-service-migration.md](workspace-service-migration.md) 之后的剩余部分。
标 **「待接受」** 的条目收窄了父合同或 launch-admission，须 Planner/用户接受后才成为合同。后续实现切片为 L2，
由 Planner 安排两路独立评审。`server/` 简写 `crates/calm-server/src/`，`truth/` 简写 `crates/calm-truth/src/`。

## 0. 结论

1. launch-admission 的执行半边已由 S2a 以另一种结构交付（执行私有 registration，不签发 PreparedLaunch）；政策归属由 M1
   收进 `workspace-runtime`。资源半边**一行都没有交付**，其中真正未闭合的生产缺口只有两类：关闭后清理的
   sample-then-effect 竞态，以及 Legacy 启动路径不经 executor、占用只能靠 DB 代理事实。
2. 推荐删除或推迟 launch-admission 的大部分资源机制（kernel 持 FD 与反向导出、CanonicalWorkspaceId 别名归一、
   borrowed/Legacy adoption、独立的 resource task-admission cutover），理由见 §3.1。剩余 cutover 收窄为 C0–C6。
3. `none` 隔离等级**不依赖** resource cutover 的任何部分：现有模型里已有“attached 且无 track worktree、agent 直接在来源
   目录运行”的状态，所有破坏性路径都已跳过它。`none` 可以先在当前模型上落地（I1a/I1b/I2，无 migration）。
4. 推荐顺序：先 I1a→I1b→I2，同时做 C0；然后 C1；再 C2–C5（Engine 消费者与正向静默）；C6 可选；之后 S3→S2b（view）。
   `none` 先行会把 borrowed 选择变成显式声明“无保证”的非资源目标，这正是 §3.1 能删掉 adoption 与别名归一的前提。

## 1. 当前事实（main `91e004a83`）

| 事实 | 证据 |
|---|---|
| 选择是四列：`kind`（Managed/Attached）、`path`、`frozen_at`、`worktree: Option`，单写者同句写入并同步 0166 binding | `crates/calm-types/src/model.rs:160-213`、`truth/db/sqlite/track_workspace.rs:12-54` |
| 创建计划是闭合枚举：显式 cwd → `AttachedWithTrackWorktree`；省略 cwd → `ManagedUnder`；`AttachedFromCwd`（attached、无 worktree、在 `path` 运行）目前只给 fixture 与 `Repo::track_create` 用 | `truth/db/sqlite/track.rs:17-37,139-175`、`server/routes/tracks/preparation.rs:203-210` |
| attached 父的子 track 继承父的 `path`（用户来源目录），`worktree` 为 None；#1830 之前的 attached track 同样无 worktree | `server/operation/child_track_adapter.rs:83-96` |
| 无 worktree 的 attached track 不能取得 worker lease：`refused: track-without-worktree` | `server/operation/workspace_lease/worker.rs:129-137,256-259,545-550` |
| 破坏性路径的守卫：recycle 只对 Managed；`remove_track_worktree`、post-commit sweep、10 分钟 clean 只对 `worktree` 非空；candidate refs 按 lease 的 common dir；area 目录清理只在 managed 根 | `crates/workspace-runtime/src/{recycle.rs,sweep.rs,track_worktree.rs,worktree_clean.rs}`、`server/workspace_service/{ports.rs:65-71,sweep.rs}` |
| 0166 foundation：每 track 一个逻辑 selection 与 binding，可记录 dev/inode 观察；观察只作审计/缺失围栏（已观察目录消失 → `WorkspaceRecoveryRequired`），终端 WorkspaceDefault 与 worker 准备会触发 | `truth/migrations/0166_workspace_resources.sql`、`server/workspace_service/identity.rs`、`server/operation/terminal_adapter/target.rs:59-68` |
| 执行：`workspace_executions` 表、`Engine`、typed opt-in Terminal；`unsettled_executions` 接入 `require_safe_tx`、删除调用者 quiesce、clean 跳过 | `workspace-execution.md` §7、§11；`server/operation/terminal_disposal.rs:110-123` |
| executor 的共享 registration 请求（`Attach` 作 Worktree registration、`Export/ExportAck`、`ObserveDirectories`、`ObserveTransferredDirectories`、`RetireRegistration`）在 calm-server 没有调用者 | `crates/calm-session/src/control/execution.rs`、`crates/calm-proc-supervisor/src/execution/` |

## 2. launch-admission 盘点

| launch-admission 章节 | 状态 | 依据 / 去向 |
|---|---|---|
| Outcome：真实 PreparedLaunch 到达生产调用者 | 未交付；结构被取代 | S2a 用执行私有 registration 与 `ExecutionIntent`（execution §7.1），不签发 PreparedLaunch。typed `resolve`/LaunchHandle 推迟到 S2b（§3.1 D5，待接受） |
| executor checkpoint `2a68f5c…` 与 `/tmp` 合同引用 | 被取代 | E1a-1/E1a-2/E1b 已合入（#2795、#2820、#2826）；`/tmp` 文件不是可复核证据，删除引用 |
| Existing owners：create/ensure、track_worktree、materialize、recycle/clean/sweep | 政策已迁移，义务未交付 | M1 把政策移入 `workspace-runtime`，行为不变；intent/incarnation 义务随 D1 删除，B pin 移到 S3（D7） |
| worktree_clean 计时器 sample-then-effect 竞态 | **未闭合** | W4 移入 `ClosedWorktreeFacts`，E2b 加了 `executions_settled`，但采样与 `git clean` 之间仍无围栏 → C1 |
| Authority（消费 C1 结论）与 typed purpose | 不变 / 推迟 | 授权仍在 auth owner；purpose 随 `resolve` 推迟（D5） |
| Identity 表：Selection / Canonical / Binding / SourceIncarnation / generation / epoch | 部分存在，其余建议删除 | 0166 有 selection/binding；Canonical、incarnation、generation 删除（D1）；source epoch 属 S3 |
| Current-object proof：kernel 持 FD、SCM_RIGHTS 双向转移、活索引 | 建议删除 | executor 在 Prepare 时打开并持有每个执行自己的 root（E1a 已交付）；跨执行连续性无消费者（D1） |
| Owned materialization / borrowed adoption | owned marker 沿用；adoption 建议删除 | borrowed 选择改为显式 `none`（D2） |
| Crash/restart 表 | 执行侧已交付；资源侧随 D1 删除 | executor journal、tombstone 与 `Engine::recover`（E1a、E2a） |
| Required kernel states（Planned…Ready…Releasing） | 建议删除 | 只保留 C1 的维护预留与 C6 的释放作业 |
| Frozen executor seam 必要改动 1–3（Canonical WorkspaceId、连续持有报告、描述符导出） | 建议删除 | 随 D1；无调用者的共享 registration 请求在 C0 删除 |
| 必要改动 4（Linux pidfd/subreaper 与非 Linux 拒绝） | 属执行协议 | 按 E1a 合同处理，本文不复核 |
| ExternalDir、legacy executor、provider/直接进程 | 部分交付 | 私有 registration 复用 ExternalDir wire 形状；v2/v3 协商不变；provider/直接进程 → C2–C4（execution §11 E3） |
| RuntimeSafety closure（跨 alias、全部 ingress 声明） | 收窄 | 无别名后不需要跨 alias 闭合；只保留清理竞态的围栏（C1）与破坏性 effect 的正向静默（C5） |
| One task-admission cutover | 建议删除 | D3 |
| Independent release/cancellation | 收窄，可选 | C6：删除事务内写释放作业，提交后与启动时消费 |
| Six C4 seams | 拆分 | WorkspaceRef/resolve/WorkspaceContent → S2b；占用 = `unsettled_executions`（E2b 已用于 typed 终端，C2–C5 扩展）；Revision/GitFacts → S3 |
| Smallest slices 1–3 | 被本文 §3.2 取代 | — |

M1 的非目标仍然有效：`identity` 留在 calm-server；两份 `RecycleRefusal` 的重复按 follow-up issue 处理。

## 3. 剩余 resource cutover

### 3.1 收窄依据与建议删除（待接受）

关键事实：owned 目录与 track 一一对应（managed `<root>/<area>/<track>`、worktree `track-<id>`），别名只来自 borrowed
选择（无 worktree 的 attached：#1830 之前的 track 与子 track）。§4 把它们声明为 `none`，即“不承诺任何保证的非资源目标”，
与 launch-admission 已有的 “workspace-free / ExternalDir 目标保持独立形状” 同类。这之后：

- **D1 删除** kernel 持 FD、SCM_RIGHTS 反向导出、`SourceObjectLease`/`RegisteredObjectWitness`、CanonicalWorkspaceId
  活索引与别名归一、runtime generation。理由：每个执行已在 executor 内持有自己 Prepare 时打开的 root；资源键就是 track_id；
  交付需要的内容身份由 S3 的 Revision 按内容证明，不用 inode。
- **D2 删除** borrowed/Legacy adoption。理由：borrowed 选择就是 `none`，`none` 不承诺任何事，不需要收养。
- **D3 删除** 独立的 resource task-admission cutover（`Legacy → Converting → ResourceAdmissionV1`）。理由：没有别名时，
  按 track 的 `checkout_admission` 与按资源聚合等价，不存在需要切换的第二个调度器。
- **D4 推迟** 历史 operation 的 owner-codec sidecar：只有引入 typed WorkspaceRef 时才需要，随 D5 一起留给 S2b。
- **D5 推迟** worktree 上的 typed `resolve`/LaunchHandle/PreparedLaunch。第一个真正需要非路径启动目标的是 view 的
  namespace 进入（S2b）；worktree 由 `ExecutionIntent` 冻结的 root + executor 在 Prepare 时打开即可。这一条收窄了父合同 §3.2。
- **D6 删除** 无调用者的 executor 共享 registration 请求及其独占代码；Prepare(Private) 内部复用的 attach 核心保留（C0）。
- **D7 移到 S3** materialization 时的 B pin（基线证据）。**D8 移到 S2b** WorkspaceContent 与 FE 文件端口迁移。

### 3.2 有序切片

每片独立可合入、有真实生产消费者，按父合同 §6 的公共 gate G 验证（text ratchet、contract、
`scripts/local-rust-gates.sh --quick --base origin/main` 各一次，加一次拒绝空选择的聚焦 replay），等级 L2，规模按
`git diff --numstat` 不超过约 800 行。下表的测试名是实现时注册的新过滤器，不是已存在的名字。

| # | 结果 | 范围与验收 | 依赖 |
|---|---|---|---|
| C0 | 删除 D6 的 executor 请求变体、处理分支与测试；envelope 版本 +1，calm-session 与 supervisor 同片 | 净删除；保留的 Prepare(Private)/Launch/Revoke/Inspect/Watch/Retire 合同测试全绿；`-p calm-proc-supervisor`、`-p calm-session` 聚焦 replay。约 −800/+60，超出则按请求拆两片 | D1/D6 被接受 |
| C1 | 关闭后清理的围栏：clean 在既有 track 级 fence（父合同 §5.1 锁序中的 track lifecycle fence，实现时确认具体锁）内重新检查 `closed_at` 未变、checkout Free、执行已结算、无活动 session，并在持有 fence 期间执行 `git clean`；reopen、Planner 唤醒、终端创建与 typed admit 在同一 fence 下检查 | 进程内围栏，不加表：timer 与所有启动入口都在 server 进程内。残余风险（server 崩溃时 `git clean` 子进程短暂存活）写进 PR。验收沿用 launch-admission 首片：clean 在 SQL 采样后暂停 → reopen 并启动 → clean 拒绝且不写 cleaned；clean 先赢 → 启动等待或 Busy。约 300 行 | 无 |
| C2 | 直接 pipe 进程与 gate shell 改为 Engine 消费者（同片定义 close-stdin/half-close） | 一个 `ExecutionIntent` 构造点 + 真实入口测试；删除被替换的 Legacy spawn 分支 | E2b（已合入） |
| C3 | forge action/probe 改为 Engine 消费者 | 同上；`cwd_lease` 冻结语义不变 | C2 |
| C4 | Planner 本地 provider 进程（Claude session/rewind/readiness、ACP）与 worker 启动改为 Engine 消费者，每个 provider 一片 | 同上；provider 外部协议仍由 provider 层拥有 | C2 |
| C5 | 破坏性 effect 的正向静默：recycle、`remove_track_worktree`、sweep、area 补偿在 fence 下要求 `unsettled_executions(scope)` 为空，否则 Busy 并保留目录；已迁移类型的 Legacy 代理判断同片删除 | 删除路径已有 quiesce，按 Legacy 时点不变；验收：未结算执行时 Track/Area 删除返回 Conflict 且目录与 branch 保留 | C2–C4 |
| C6（可选） | 释放作业：删除事务内写入待回收的 worktree/refs 描述，提交后 sweep 消费，启动时补跑 | 修复“提交后崩溃 → worktree 与 branch 永久泄漏”；不改 7 天 trash 与回收顺序。若接受泄漏只记 follow-up，则不做 | C5 |

之后的 S3（Revision、B pin、7/90）与 S2b（view、WorkspaceRef、WorkspaceContent）沿用各自协议，不在本表。

## 4. 隔离等级

### 4.1 依赖分析

| 等级 | 依赖 cutover 的哪部分 | 结论 |
|---|---|---|
| `worktree` | 无（就是今天的行为） | 只需让它成为显式选择 |
| `none` | 无。它的唯一规则“kernel 不创建、不回收、不删除该目录”今天已由各破坏性守卫保证（§1 第 5 行）；它不要互斥、占用、证据或交付，因此不需要 C1–C6 | **可以先在当前模型上落地** |
| `view` | S3 的 Revision/RuleView/FUSE 与 S2b 机制；推迟到 S2b 的 typed 启动目标与 WorkspaceContent（D5、D8）；回收私有层前的正向静默（C5），也就是 view 内允许的每种启动都要先成为 Engine 消费者（C2–C4） | 在 C5 与 S3 之后，作为第三个后端加入 |

typed 等级选择本身（请求字段、枚举、wire 投影、UI 选择）只依赖创建路由与模型，与 cutover 无关。

### 4.2 `none` 的设计

**数据模型：不加 migration。** `none` ≡ `kind = Attached ∧ worktree = None`，这正是现有 `AttachedFromCwd` 计划写出的行。
calm-types 新增闭合枚举 `WorkspaceIsolation { Managed, Worktree, None }` 和唯一解码函数 `TrackWorkspace::isolation()`；
`Managed` 表示 kernel 独占目录，不是用户可选等级。不加 `workspace_isolation` 列：它与 `worktree` 是否为空是同一事实，
加列会形成两份编码再用 CHECK 绑定。S2b 引入 view 描述符时同片扩展解码（worktree 与 view 互斥由那次 migration 的 CHECK 保证）。
现有的 #1830 之前 attached 行与子 track 自动解码为 `none`。这是如实标注：它们今天就直接在来源目录运行。

**API。** `CreateTrackRequest` 增加 `isolation: "worktree" | "none"`，路由解码成必填的 Rust 枚举 `AttachedIsolation`：

- 显式 cwd 且省略 `isolation` → `worktree`。这是现有“显式 cwd 即 worktree”合同的单点解码，不是新增 fallback，在 PR 中作为有意例外说明；
- 省略 cwd（managed）却给出 `isolation` → 400；
- `none` → `TrackWorkspacePlan::AttachedFromCwd`（从 fixture 专用变为生产）；`worktree` → 现有 `AttachedWithTrackWorktree`；
- `isolation` 进入 `CreateRequestShape`，参与 Idempotency-Key 的请求形状比较；keyed 重放沿用原选择；
- `none` 的准入：绝对路径、存在、是目录、位于 Git 工作树内（保持现状，不顺带放开非 Git 目录）；跳过 `refs/heads/neige`
  命名空间检查与 `git_repo_root_for_track_cwd`，因为两者只服务 worktree；
- `Track.workspace` 的 wire 增加必填投影字段 `isolation`（ToSchema/TS），重新生成 OpenAPI 与 `fe/core/api/generated/wire.ts`。

**运行行为。** Planner、对话与终端的 cwd = `path`（`agent_cwd()` 不变）。终端 WorkspaceDefault 对 `none` 跳过 0166 观察，不产生
`WorkspaceRecoveryRequired`（`none` 无监控）；binding 行照旧作为选择审计写入。worker task 继续以既有拒绝字
`track-without-worktree` 拒绝，消息改为说明“该 track 隔离等级为 none，task worker 需要 worktree”。Planner 的 forge/git_commit
照旧在 `agent_cwd` 执行，即落在用户当前分支（无保证）；publish 照旧要求 worktree。两个 `none` track 可以指向同一目录，
kernel 不加互斥。area 的 `area_folders` 认领是 area 级路径命名空间，不是 track 互斥，保持不变。

**每条 kernel 破坏性路径如何跳过 `none`。** workspace-runtime 各入口改为对 `isolation()` 做穷举 `match`，`None` 分支是显式的
no-op 并注明唯一规则；今天的 SQL 守卫在语义上等价，保留不动：

| 路径 | 今天的守卫 | `none` 的结果 |
|---|---|---|
| `recycle_track_workspace`（Track/Area 删除、area 补偿）、`workspace_allows_runtime_recovery` | 非 Managed → `Refused(NotManaged)` / 允许 | `match`：只有 `Managed` 回收 |
| `remove_track_worktree`、post-commit sweep | `worktree` 为 Some | `match`：只有 `Worktree` 删除 |
| 10 分钟 clean | `CLOSED_TRACK_WORKTREES_SQL` 要求 `workspace_worktree_path IS NOT NULL` | 不入候选；runtime 侧对解码结果 `match` |
| candidate refs 删除 | 按 lease 的 common dir | `none` 没有 lease（worker 被拒）→ 空 |
| area 删除 | 逐 track recycle + sweep；只删 managed 根下的空 area 目录 | 同上两行；managed 根之外不动 |
| materialize / `ensure_track_worktree` | 只对 Managed / `worktree` 为 Some | 不创建任何目录 |
| lease、`ensure_clean_tree`、catch-up `reset --keep` | 需要 worker checkout | 在任何 effect 之前被拒 |
| repoint | attached 在创建时冻结 | Conflict |
| trash GC | 只扫 `<root>/.trash` | 无关 |

I1a 的 PR 另外按 `agent_cwd`/`path` 消费者扫一遍 kernel 侧写入（附件已是 managed-only），结果写进 PR，不做成永久名单。

**UI。** 选择文件夹后，新建 track 的 composer 出现两个选项：`Worktree (isolated)`（默认）与 `Directly in this folder`。
后者与 track 头部/信息里的徽标都标为 **“Not isolated · no guarantees”**，说明 agent 直接改动这个目录，Neige 不加锁、不监控、
不记录证据、不交付；Neige 也不会创建或删除它；task worker 不可用。复用现有 chip/选项组件与图标，遵循 `fe/AGENTS.md`；
FE 只读 wire 的 `isolation` 字段，不再从 `worktree` 是否存在推断。

**开放问题（待用户决定，不阻塞 I1）。** ① `none` 是否最终允许 worker task（需要任务系统新增“无交付管理”的模式，建议另立）；
② 是否放开非 Git 目录；③ 子 track 继承父的来源目录（worktree 父的子 track 今天也是 `none`），是否改为各自建 worktree，
这是既有行为，建议单独立 issue。

### 4.3 隔离切片

| # | 结果 | 验收 | 等级 |
|---|---|---|---|
| I1a | calm-types `WorkspaceIsolation` 与 `isolation()`；workspace-runtime 破坏性入口穷举 `match`；终端观察跳过 `none`；worker 拒绝消息。用户可见行为只变一处：旧 borrowed 行不再有终端缺失围栏，在 PR 中写明 | 用 `AttachedFromCwd` 夹具：Track 删除、Area 删除（含补偿）、关闭后 clean 计时器之后，来源目录及其 ignored/未跟踪文件逐字节不变，且没有 `.claude/worktrees`；worker task 被拒；`-p workspace-runtime -p calm-server` 聚焦。约 300 行 | L2：破坏性路径 |
| I1b | 请求字段、路由映射、幂等形状、`none` 准入、wire 投影与生成产物 | HTTP 创建 `none` → 无 worktree、无 branch、Planner cwd = `path`；managed 带 `isolation` → 400；省略字段 = worktree；keyed 重放不改选择；生成器产物无漂移。约 400 行（不含生成文件） | L2：API/持久选择 |
| I2 | FE 选择与徽标 | `cd fe && npm ci && npm run lint && npm run build && npm test`，真实浏览器预览（docs/development-preview.md）与相关 browser 测试 | L1：纯 UI，只消费已评审的 wire 字段 |
| （S2b） | `view` 成为 `WorkspaceIsolation` 的第三个变体，随其后端描述符 migration 与创建/关闭/删除/恢复同片 | 按 [workspace-view-backend.md](workspace-view-backend.md) | L2 |

## 5. 推荐顺序与理由

1. **I1a → I1b → I2 先行。** 不依赖任何 cutover；直接满足用户的 typed 选择；把 borrowed 选择变成显式且标注“无保证”的
   非资源目标，这正是 D1–D3 能删除的前提，cutover 因此变小。I1a 的穷举 `match` 也让以后加入 `view` 时每个破坏性入口都必须表态。
2. **C0 与 I1 并行**（D1/D6 被接受后）：纯删除，缩小 executor 协议面。
3. **C1** 关闭唯一已知的清理竞态，规模小，与其余切片无依赖。
4. **C2–C4 → C5**：先让各种启动成为 executor 事实，再让破坏性 effect 依赖这些事实；顺序反过来，C5 只能继续依赖 DB 代理事实。
5. **C6 可选**，然后 **S3 → S2b**：view 需要 S3 的证据与 C5 的正向静默，作为第三个后端最后接入。

## 6. AGENTS.md 架构五点与永久成本

- **抽象边界**：隔离词汇在 calm-types；“kernel 不触碰 `none`”的政策在 `workspace-runtime` 的破坏性入口（M1 之后的唯一 owner）；
  calm-truth 只拥有行与 SQL；FE 消费 wire 字段，不推断领域政策；进程事实只来自 executor（C2–C5）。
- **重复逻辑**：一个解码函数 `isolation()`，没有并行的守卫列表；C2–C4 每接入一种启动就删除对应的 Legacy spawn 分支，
  C5 删除已迁移类型的 Legacy 占用代理；C0 删除无调用者的协议。
- **硬编码假设**：`WorkspaceIsolation` 是本层有意闭合的集合，`view` 作为新变体加入；不按 provider、模板或路径字符串分支。
- **外部合同事实**：Git 根只取 `rev-parse --show-toplevel`；`none` 只要求它成功；不从路径布局推断 worktree 或所有权。
- **永久成本**：没有 migration、新表或名单。新增一个等级要改：枚举、解码函数、workspace-runtime 中约 6 处穷举 `match`、
  请求枚举、FE 标签映射，每处一次，由编译器强制。新测试放在既有 target 中，每个 CI workflow 运行一次；增删测试不改任何名单。
  C1 是进程内围栏，不新增持久状态；C6 若实施会新增一类 operation kind。删除 D1–D3、D6 会显著减少 launch-admission 计划中的永久面。
