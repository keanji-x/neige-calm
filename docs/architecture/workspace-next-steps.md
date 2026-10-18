# Workspace 下一步：S2a / M1 之后的 resource cutover 与隔离等级（#2666 re-plan）

状态：重新规划，只改文档，不改代码，不声称已实现。核实基线为 main `91e004a83`（S2a E1a–E2b 已合入 #2795–#2861，
M1 W1–W4 已合入 #2866–#2871）。父合同为 [track-workspace.md](track-workspace.md)；本文盘点
[workspace-launch-admission.md](workspace-launch-admission.md)（S2a 之前写成，仍标注待 fresh 评审）在
[workspace-execution.md](workspace-execution.md) 与 [workspace-service-migration.md](workspace-service-migration.md) 之后的剩余部分。
标 **「待接受」** 的条目收窄了父合同或 launch-admission，须 Planner/用户接受后才成为合同。后续实现切片为 L2（I2 除外），
由 Planner 安排两路独立评审。`server/` 简写 `crates/calm-server/src/`，`truth/` 简写 `crates/calm-truth/src/`，
`runtime/` 简写 `crates/workspace-runtime/src/`。本版按两路计划评审（boundary 与 adversarial）及 Planner 决定修订，处置见 §7。

## 0. 结论

1. launch-admission 的执行半边已由 S2a 以另一种结构交付（执行私有 registration，不签发 PreparedLaunch）；政策归属由 M1
   收进 `workspace-runtime`。资源半边**一行都没有交付**，其中真正未闭合的生产缺口只有两类：关闭后清理的
   sample-then-effect 竞态，以及 Legacy 启动路径不经 executor、占用只能靠 DB 代理事实。
2. 推荐删除或推迟 launch-admission 的大部分资源机制（kernel 持 FD 与反向导出、CanonicalWorkspaceId 别名归一、
   borrowed/Legacy adoption、独立的 resource task-admission cutover）。删除的前提是一条可执行的**非重叠合同**：
   Neige 自有位置之间、自有位置与 `none` 选择之间互不重叠（§3.1、§4.2）。剩余 cutover 收窄为 C0–C6。
3. `none` 隔离等级**不依赖** resource cutover 的任何部分，但**依赖新的准入规则**：按被操作 track 自身的行跳过破坏性路径
   并不够，因为另一个 owner 的回收、清理或删除可以作用到同一物理目录。`none` 的准入拒绝位于、等于或包含任何
   Neige 自有位置的 cwd（§4.2 的保证 G）。有了这条规则，`none` 可以在当前模型上落地（I1a/I1b/I2，无 migration）。
4. 推荐顺序：先 I1a→I1b→I2，同时做 C0；然后 C1；再 C2–C5（Engine 消费者与正向静默）；C6 可选；之后 S3→S2b（view）。
   既有 worktree 准入的自有/自有重叠（§3.1 O1）是既有缺陷，另立 issue，在 C5 之前修复。

## 1. 当前事实（main `91e004a83`）

| 事实 | 证据 |
|---|---|
| 选择是四列：`kind`（Managed/Attached）、`path`、`frozen_at`、`worktree: Option`，单写者同句写入并同步 0166 binding | `crates/calm-types/src/model.rs:189-213`、`truth/db/sqlite/track_workspace.rs:12-54` |
| 创建计划是闭合枚举（五个变体）：显式 cwd → `AttachedWithTrackWorktree`；省略 cwd → `ManagedUnder`；子 track → `ManagedFrozenUnder` / `InheritAttachedFrozen`；`AttachedFromCwd`（attached、无 worktree、在 `path` 运行）目前只给 fixture 与 `Repo::track_create` 用 | `truth/db/sqlite/track.rs:17-40,139-175`、`server/routes/tracks/preparation.rs:200-210` |
| 显式 cwd 的准入只查绝对路径、存在、是目录、`rev-parse` 成功与 `refs/heads/neige` 命名空间；**不排除与 managed 根或其他 track worktree 根的重叠**。`AttachedFromCwd` 不经 `AttachedInheritedPath` | `runtime/selection.rs:24-80`、`server/routes/tracks/preparation.rs:155-161` |
| 子 track：managed 父 → 自己的 frozen managed 目录；attached 父 → 经 `AttachedInheritedPath` 继承父的 `path`（用户来源目录，不是父的 worktree），`worktree` 为 None。`AttachedInheritedPath` 以 lexical + canonical 两次检查拒绝 managed 根内的路径，注释写明“recycling is by DIRECTORY”的 collateral 风险。#1830 之前的 attached track 同样无 worktree（0120 不回填） | `server/operation/child_track_adapter.rs:83-96`、`truth/db/sqlite/track.rs:41-77`、`truth/migrations/0120_track_worktree.sql:1-5` |
| `worker_checkout` 对 Attached 只接受 `Some(worktree)`：无 worktree 的 attached track 拒绝 `track-without-worktree`；Managed（worktree 为 None）照常可以取得 worker lease | `server/operation/workspace_lease/worker.rs:128-137,256-259,545-550` |
| 破坏性路径的守卫都按**被操作 track 自身的行**：recycle 只对 Managed（且 canonical 在根下、带 marker、非 system area）；`remove_track_worktree`、post-commit sweep、10 分钟 clean 只对该行 `worktree` 非空；area 目录清理只在 managed 根；trash GC 只扫 `<root>/.trash`。这些守卫不阻止另一个 owner 的 effect 到达别的行选中的同一目录 | `runtime/{recycle.rs,sweep.rs:36-47,track_worktree.rs:46-63,worktree_clean.rs:56-110}`、`server/workspace_service/ports.rs:65-71`、`crates/workspace/src/teardown.rs:12-71` |
| candidate refs：删除时 sweep 读取该 track **所有** lease 行的 `git_common_dir`（不按 state、不看 `worktree`），删除在 `Some(worktree)` 分支之外执行，前缀 `refs/neige/candidates/<track>/` 与 `refs/neige/gate-runs/<track>/`，`update-ref -d` 会运行 reference-transaction hook | `server/workspace_service/sweep.rs:7-38`、`runtime/sweep.rs:41-46`、`server/git_candidate/refs.rs:18-60`、`server/operation/task_gate_run/mod.rs:155-157` |
| 0166：每 track 一个逻辑 selection 与 binding，记录 dev/inode 观察。终端：`decode` 把 `Attached ∧ worktree = None` 解成 `ExternalDir` 并**跳过观察**，只有 `WorkspaceDefault`（managed 与有 worktree 的 attached）调 `observe_for_task`，已观察目录消失 → `WorkspaceRecoveryRequired`。worker：无 worker checkout 时 `observe_worker_checkout` 直接返回；有 checkout 时，准备事务把观察 identity 相等、`is_current`、canonical 路径一致与 managed 观察相等 + `consume_materialized` 作为提交前的 **Stale/CAS 围栏** | `truth/migrations/0166_workspace_resources.sql`、`server/operation/terminal_adapter/target.rs:27-68`、`server/operation/workspace_lease/worker.rs:139-157`、`server/operation/workspace_lease/worker/preparation.rs:228-262` |
| 执行：`workspace_executions` 表、`Engine`、typed opt-in Terminal；`unsettled_executions` 接入 `require_safe_tx`、删除调用者 quiesce、clean 跳过 | `workspace-execution.md` §7、§11；`server/operation/terminal_disposal.rs:110-123` |
| executor 的共享 registration 请求（`Attach` 作 Worktree registration、`Export/ExportAck`、`ObserveDirectories`、`ObserveTransferredDirectories`、`RetireRegistration`）在 calm-server 没有生产发送者；生产适配器 `session_executor` 只发 `Prepare(Private)` 及 Launch/Revoke/Inspect/Control/Watch/Retire | `crates/calm-session/src/control/execution.rs`、`crates/calm-proc-supervisor/src/execution/mod.rs:346-390`、`runtime/execution/session_executor.rs` |
| executor 帧用 bincode（变体按序号编码）；journal 与 registration receipt 用 JSON 持久化；boot 时要求持久 receipt 的 `contract == EXECUTION_ENVELOPE_VERSION`（现为 4），journal 另有 `JOURNAL_VERSION`（4） | `crates/calm-session/src/lib.rs:281-286`、`crates/calm-session/src/control/execution.rs:29`、`crates/calm-proc-supervisor/src/execution/{mod.rs:250-257,directory.rs:14-22,journal.rs:14,236-246}` |
| 锁：每卡 `planner_recovery_locks` → 全局 operation drive mutex（`OperationRuntime::lock_for_track_delete`，所有 operation 驱动的公共漏斗）→ 每 track `track_delete_locks`（绕过 OperationRuntime 的直接恢复/重连），不得反向 | `server/state.rs:112-141`、`server/operation/driver.rs:75-78`、`server/harness/mod.rs:245`、`server/routes/planner_recovery.rs:47` |
| 幂等形状：`create_request_digest` 对后加的可选字段在省略时不写入载荷，以保留历史 binding 的 SHA（`admin` 的 false 与省略同形） | `server/routes/tracks/create.rs:427-465` |

## 2. launch-admission 盘点

| launch-admission 章节 | 状态 | 依据 / 去向 |
|---|---|---|
| Outcome：真实 PreparedLaunch 到达生产调用者 | 未交付；结构被取代 | S2a 用执行私有 registration 与 `ExecutionIntent`（execution §7.1），不签发 PreparedLaunch。typed `resolve`/LaunchHandle 推迟到 S2b（§3.1 D5，待接受） |
| executor checkpoint `2a68f5c…` 与 `/tmp` 合同引用 | 被取代 | E1a-1/E1a-2/E1b 已合入（#2795、#2820、#2826）；`/tmp` 文件不是可复核证据，删除引用 |
| Existing owners：create/ensure、track_worktree、materialize、recycle/clean/sweep | 政策已迁移，义务未交付 | M1 把政策移入 `workspace-runtime`，行为不变；intent/incarnation 义务随 D1 删除，B pin 移到 S3（D7） |
| worktree_clean 计时器 sample-then-effect 竞态 | **未闭合** | W4 移入 `ClosedWorktreeFacts`，E2b 加了 `executions_settled`，但采样与 `git clean` 之间仍无围栏 → C1 |
| Authority（消费 C1 结论）与 typed purpose | 不变 / 推迟 | 授权仍在 auth owner；purpose 随 `resolve` 推迟（D5） |
| Identity 表：Selection / Canonical / Binding / SourceIncarnation / generation / epoch | 部分存在，其余建议删除 | 0166 有 selection/binding 与 worker 的观察围栏（保留）；Canonical、incarnation、generation 删除（D1）；source epoch 属 S3 |
| Current-object proof：kernel 持 FD、SCM_RIGHTS 双向转移、活索引 | 建议删除 | executor 在 Prepare 时打开并持有每个执行自己的 root（E1a 已交付）；跨执行连续性无消费者（D1） |
| Owned materialization / borrowed adoption | owned marker 沿用；adoption 建议删除 | borrowed 选择改为显式 `none`（D2） |
| Crash/restart 表 | 执行侧已交付；资源侧随 D1 删除 | executor journal、tombstone 与 `Engine::recover`（E1a、E2a） |
| Required kernel states（Planned…Ready…Releasing） | 建议删除 | 只保留 C1 的清理围栏与 C6 的释放作业 |
| Frozen executor seam 必要改动 1–3（Canonical WorkspaceId、连续持有报告、描述符导出） | 建议删除 | 随 D1；无调用者的共享 registration 请求在 C0 删除 |
| 必要改动 4（Linux pidfd/subreaper 与非 Linux 拒绝） | 属执行协议 | 按 E1a 合同处理，本文不复核 |
| ExternalDir、legacy executor、provider/直接进程 | 部分交付 | 私有 registration 复用 ExternalDir wire 形状；v2/v3 协商不变；provider/直接进程 → C2–C4（execution §11 E3） |
| RuntimeSafety closure（跨 alias、全部 ingress 声明） | 收窄（以非重叠合同为条件） | 只有 §3.1 的非重叠合同成立（`none` 准入 + O1）时才不需要跨 alias 闭合；保留清理竞态的围栏（C1）与破坏性 effect 的正向静默（C5） |
| One task-admission cutover | 建议删除 | D3（同样以非重叠合同为条件） |
| Independent release/cancellation | 收窄，可选 | C6：删除事务内写释放作业，提交后与启动时消费 |
| Six C4 seams | 拆分 | WorkspaceRef/resolve/WorkspaceContent → S2b；占用 = `unsettled_executions`（E2b 已用于 typed 终端，C2–C5 扩展）；Revision/GitFacts → S3 |
| Smallest slices 1–3 | 被本文 §3.2 取代 | — |

M1 的非目标仍然有效：`identity` 留在 calm-server；两份 `RecycleRefusal` 的重复按 follow-up issue 处理。

## 3. 剩余 resource cutover

### 3.1 收窄依据与建议删除（待接受）

**非重叠合同。** Neige 自有位置（§4.2 定义：managed 根整体，含 system area 与 `.trash`；每个 track worktree 根）是
kernel 创建、回收、清理、删除 effect 的全部目标。D1–D3 的前提是：(a) `none` 选择与任何自有位置互不重叠，由 §4.2 的准入
保证；(b) 自有位置之间按 track 一一对应、互不嵌套；(c) `none` 不参与 task admission（worker 被拒）。今天 (b) 有一个既有
缺口：

- **O1（既有缺陷，另立 issue，待接受）** 显式 cwd 的 worktree 准入不排除 managed 根或他人 worktree 根：cwd 选在 managed
  track A 的目录内（managed 目录是 Git 仓库）或他人 worktree 根内时，新 worktree 落在 A 之下，删除 A 时被整体移进 `.trash`
  或被 `worktree remove --force` 带走。修复：worktree 准入复用 §4.2 的同一检查，但只拒绝“位于或等于”自有位置（包含 worktree 根
  的仓库根正是 worktree 的正常用法）。这是对既有创建行为的收窄，须 Planner 接受；在 C5 依赖按 track scope 的占用之前完成。

在 (a)–(c) 成立后：

- **D1 删除** kernel 持 FD、SCM_RIGHTS 反向导出、`SourceObjectLease`/`RegisteredObjectWitness`、CanonicalWorkspaceId
  活索引与别名归一、runtime generation。理由：每个执行已在 executor 内持有自己 Prepare 时打开的 root；自有资源互不重叠，
  资源键就是 track_id；符号链接别名在准入时由 canonical 检查拒绝；准入之后由外部进程改名或换链接不在防御范围内（与
  `AttachedInheritedPath` 一致）；交付需要的内容身份由 S3 的 Revision 按内容证明，不用 inode。worker 准备的 0166 观察围栏
  （§1）不属于 D1，保留。
- **D2 删除** borrowed/Legacy adoption。理由：borrowed 选择就是 `none`；`none` 只承诺保证 G，不是资源，不需要收养。
- **D3 删除** 独立的 resource task-admission cutover（`Legacy → Converting → ResourceAdmissionV1`）。理由：`none` 不进入
  task admission，自有资源与 track 一一对应，按 track 的 `checkout_admission` 与按资源聚合等价，不存在需要切换的第二个调度器。
- **D4 推迟** 历史 operation 的 owner-codec sidecar：只有引入 typed WorkspaceRef 时才需要，随 D5 一起留给 S2b。历史 operation
  仍由原 owner 以冻结字节驱动，不重读当前 path。
- **D5 推迟** worktree 上的 typed `resolve`/LaunchHandle/PreparedLaunch。第一个真正需要非路径启动目标的是 view 的
  namespace 进入（S2b）；worktree 由 `ExecutionIntent` 冻结的 root + executor 在 Prepare 时打开即可。这一条收窄了父合同 §3.2。
- **D6 删除** 无调用者的 executor 共享 registration 请求及其独占代码；Prepare(Private) 依赖的私有路径保留（C0 的保留清单）。
- **D7 移到 S3** materialization 时的 B pin（基线证据）。**D8 移到 S2b** WorkspaceContent 与 FE 文件端口迁移。

### 3.2 有序切片

每片独立可合入、有真实生产消费者，按父合同 §6 的公共 gate G 验证（text ratchet、contract、
`scripts/local-rust-gates.sh --quick --base origin/main` 各一次，加一次拒绝空选择的聚焦 replay），等级 L2。**规模口径**（所有
切片统一）：`git diff --numstat` 中手写文件的增删合计不超过约 800 行；OpenAPI 与 `fe/core/api/generated/*` 等生成产物不计入，
在 PR 中单列。纯删除片（除删除外只有不超过约 60 行的兼容改动）按请求组切，不为凑预算把一个请求的处理与其测试拆到两片；
超出时在 PR 中说明。下表的测试名是实现时注册的新过滤器，不是已存在的名字；行数是源文件盘点（`wc -l`），不是删除 diff。

| # | 结果 | 范围与验收 | 依赖 |
|---|---|---|---|
| C0a | 删除 `ObserveTransferredDirectories`：session `control/execution/transferred_directory.rs`（117）、supervisor 分派分支、`tests/execution_registration/transferred_directory.rs`（285）。同片引入兼容规则（见下） | 约 −420；保留清单全绿；`-p calm-proc-supervisor`、`-p calm-session` 聚焦 | D1/D6 被接受 |
| C0b | 删除 `ObserveDirectories` 及只为它存在的 helper 角色：supervisor `execution/directory_relation.rs`（190）、`directory_probe.rs`（338）与其测试（154）、`scope_helper.rs` 的 `ObserveDirectories` 角色、session `directory_relation.rs`（94）、`tests/execution_registration/directory_relation.rs`（359） | 约 −1150，纯删除（半删会留下 clippy 拒绝的死代码，故不再拆） | C0a |
| C0c | 删除 `Export`/`ExportAck` 的请求、处理分支与测试 | 净删除；保留清单全绿 | C0a |
| C0d | 删除作 Worktree registration 的共享 `Attach`、`RetireRegistration`（`execution/attachment.rs` 的 `attach_objects`、`retirement.rs`、session `attachment_client.rs` 与 `tests/execution_attachment.rs` 的对应部分），以及只服务共享 registration 的 `execution_registration*` 测试 | 净删除；覆盖保留不变量的测试断言改挂到 Private 路径而不是删除 | C0c |
| C1 | 关闭后清理的围栏，复用既有两把锁、不加第二套 lifecycle 锁（持有区间见下） | 进程内围栏，不加表。验收：clean 在 SQL 采样后暂停 → reopen 并启动 → clean 拒绝且不写 cleaned；clean 先赢时，worker 准备、终端创建、typed admit、Planner 启动/恢复各自得到 Busy 或等待且 clean 结束后正常启动（每个入口一个测试，不只测 reopen）。约 300 行 | 无 |
| C2a | 直接 pipe 进程改为 Engine 消费者，同片定义 close-stdin/half-close（今天只有 `WriteStdin`，`calm-session/src/control/execution.rs`） | 一个 `ExecutionIntent` 构造点 + 真实入口测试；删除被替换的 Legacy spawn 分支 | E2b（已合入） |
| C2b | gate shell（`server/operation/gate_process.rs:263` 的 `spawn_held`）改为 Engine 消费者；保留其持久恢复、stdin 释放与 leader reaping 合同 | 同上，一个构造点或一个恢复机制 | C2a |
| C3a / C3b | forge action / probe 改为 Engine 消费者，各对应一个现有 spawn 点（`server/operation/forge_action_adapter/mod.rs:1009`、`:1296`） | 同上；`cwd_lease` 冻结语义不变；不按工具名单注册 | C2a |
| C4 | Planner 本地 provider 进程（Claude session/rewind/readiness、ACP）与 worker 启动改为 Engine 消费者，每个 provider 的一个启动或恢复入口一片，沿最新 main 的 provider Factory 归属 | 同上；provider 外部协议仍由 provider 层拥有 | C2a |
| C5 | 破坏性 effect 的正向静默：recycle、`remove_track_worktree`、sweep、area 补偿在 fence 下要求 `unsettled_executions(scope)` 为空，否则 Busy 并保留目录；已迁移类型的 Legacy 代理判断同片删除，未迁移类型的 Legacy 检查保留 | 删除路径保持“先 quiesce、后写者复查”的现有时序；scope 只在非重叠合同（含 O1）成立后才等于 owning track。验收：未结算执行时 Track/Area 删除返回 Conflict 且目录与 branch 保留 | C2a–C4、O1 |
| C6（可选） | 释放作业：删除事务内写入待回收的 worktree/refs 描述，提交后 sweep 消费，启动时补跑 | 修复“提交后崩溃 → worktree 与 branch 永久泄漏”；不改 7 天 trash 与回收顺序。若接受泄漏只记 follow-up，则不做，也不声称已有 crash-safe release | C5 |

**C0 保留清单。** 删除边界按私有路径的依赖划定，不按类型整块删：`stage_private`（`execution/preparation.rs:198-201`）、
唯一的 `release_private`（`execution/private.rs:8-37`）与 transfer 身份/nonce/root 资格检查（同文件 65-132）、
`RegisteredRoot`（`execution/directory.rs`）、`PreparedSource::Private(Box<AttachRegistration>)` 及其 ExternalDir 形状、
`HeldSourceObjects` 的 FD 接收、不可变 replay、先持久化再发布、独占 ExecutionId、durable settle 之后才释放、Control/Stop、
Inspect/Watch/Retire 与 tombstone。`tests/execution_private.rs` 与覆盖这些不变量的 registration 测试保留。

**C0 兼容规则。** 删除 wire 枚举变体会改变 bincode 序号，所以每个删变体的片都把 `EXECUTION_ENVELOPE_VERSION` +1，session 与
supervisor 同片。被接管的升级前 supervisor 握手不匹配时按 E2a 的既有合同处理：`Engine::available()` 为假，typed 创建得到
`ExecutionUnavailable`，启用需重启 supervisor（升级说明照写）。持久事实不跟随 wire 版本：C0a 把 receipt 的持久合同从 wire
envelope 拆出（`execution/mod.rs:250-257`、`directory.rs:19-22` 改为比较持久合同常量，仍为 4），`JOURNAL_VERSION` 不变，
持久 JSON 形状不改；某片若必须改持久形状，同片提升持久合同并带旧形状解码测试。验收夹具：升级前 v4 状态目录中一个已持久化的
Private 执行（运行中与已 durable settle 各一）在新 supervisor 下恢复、结算、释放并留下 tombstone。若 Planner 希望只要求一次
supervisor 重启，C0a–C0d 在同一发布中落地。

**C1 锁与持有区间。** 两把既有锁：全局 drive mutex（`OperationRuntime::lock_for_track_delete`）与每 track 的
`track_delete_locks`，顺序 card → drive → track（`server/state.rs:112-130`），任何入口都不持 track 去等 drive 或 card。

| 入口 | 持有区间 |
|---|---|
| clean 计时器 | 无锁 SQL 采样 → 取 drive → 取 track → 在两者下复查 `closed_at` 未变、checkout Free、执行已结算、无 Starting/活动的 session 或终端 → 释放 drive（只持 track）→ 在 `spawn_blocking` 中 `git clean -fdX` 并记入 `cleaned` → 释放 track。drive 只覆盖复查这几次 DB 读取，`git clean` 期间不阻塞全局 operation 驱动 |
| 经 OperationRuntime 的启动（终端创建与 `terminal_adapter::prepare`、worker 准备、planner-harness-start/restart、typed admit） | 已在 drive 下运行；对 track 键只用 try-lock，从自身检查开始持有到同事务写下 clean 可见的占用事实（Starting session、未结算 intent、lease）提交为止；争用时返回 Busy 由驱动重试，不在 drive 下等待 `git clean` |
| 直接恢复/重连（`harness/mod.rs:245`、`routes/planner_recovery.rs:47`、`claude_update`） | 照旧在不持 drive 的情况下等待 track，覆盖检查到 spawn |
| reopen 路由 | 不持 drive，等待 track，覆盖 `closed_at` 写入；clean 的复查因此只会看到“仍关闭”或“已重开” |
| 不经 operation 的 Legacy 启动（WS 终端 ensure、`claude_auto_resume` 等，实现时按 spawn 点调用者扫描，清单写进 PR） | 不持 drive，等待 track，覆盖检查到写下占用事实；boot reconcile 早于计时器首轮（计时器跳过启动 tick） |

残余风险写进 PR 并由 Planner 接受：锁不跨进程崩溃；server 崩溃后孤儿 `git clean` 子进程可能继续运行，时长没有上界，期间
重启后的启动不受围栏保护。不改 clean 的 SQL 采样。

之后的 S3（Revision、B pin、7/90）与 S2b（view、WorkspaceRef、WorkspaceContent）沿用各自协议，不在本表。

## 4. 隔离等级

### 4.1 依赖分析

| 等级 | 依赖 cutover 的哪部分 | 结论 |
|---|---|---|
| `worktree` | 无（就是今天的行为） | 只需让它成为显式选择 |
| `none` | 无。保证 G（§4.2）由“每个 kernel effect 只以自有位置为目标”的既有守卫加新的非重叠准入共同给出；它不要互斥、占用、证据或交付，因此不需要 C1–C6 | **可以先在当前模型上落地** |
| `view` | S3 的 Revision/RuleView/FUSE 与 S2b 机制；推迟到 S2b 的 typed 启动目标与 WorkspaceContent（D5、D8）；回收私有层前的正向静默（C5），也就是 view 内允许的每种启动都要先成为 Engine 消费者（C2–C4） | 在 C5 与 S3 之后，作为第三个后端加入 |

typed 等级选择本身（请求字段、枚举、wire 投影、UI 选择）只依赖创建路由与模型，与 cutover 无关。

### 4.2 `none` 的设计

**自有位置与 kernel effect。** Neige 自有位置是：(1) managed 根 `workspace_root` 整体，含 system area 与 `<root>/.trash`；
(2) 每个 `tracks.workspace_worktree_path` 非空的行的 track worktree 根 `<repo>/.claude/worktrees/track-<id>`。自有 Git 元数据是
该 worktree 的管理目录、分支 `neige/track-<id>`、`refs/neige/candidates/<track>/` 与 `refs/neige/gate-runs/<track>/`。kernel 的
创建、回收、清理、删除 effect（materialize、`ensure_track_worktree`、recycle、trash GC、`remove_track_worktree` 及其目录兜底、
`git clean -fdX`、candidate refs 删除、空 area 目录删除）的目标都是某个自有位置或自有 refs（§1）。

**保证 G。** 对一个经准入的 `none` 目录 D：kernel 不创建、不回收、不 clean、不删除 D，也不删除 D 内任何不属于自有位置的文件。
依据：每个 kernel effect 只作用于自有位置；准入时 D 与全部自有位置互不重叠；`none` 行本身不是自有位置，其所有破坏性入口是
显式 no-op。准入保持自有选择与 `none` 选择不重叠，这是 G 的唯一新增前提。

**准入（I1b）。** 拒绝位于、等于或包含任何自有位置的 cwd，否则 400 并说明冲突的自有位置。实现复用 `AttachedInheritedPath`
的 lexical + canonical 双检查（`truth/db/sqlite/track.rs:41-77`）：把它的比较抽成一个函数，`AttachedInheritedPath::new` 继续只
检查“位于 managed 根内”（子 track 行为不变），`none` 的证明类型检查全部自有位置与三种关系。worktree 根集合在创建事务内读取
（SQLite 单写者，并发创建的 worktree track 无法插在检查与写入之间）；候选路径与已存在的根做 canonical 比较，不存在的根按
lexical 比较，与 `AttachedInheritedPath` 相同。其余准入不变：绝对路径、存在、是目录、位于 Git 工作树内（不顺带放开非 Git
目录）；跳过 `refs/heads/neige` 命名空间检查与 `git_repo_root_for_track_cwd`，两者只服务 worktree。准入只作用于新的 HTTP 创建，
不作用于旧行，也不作用于 keyed 重放。直接后果：只要仓库 R 上还有 worktree track，`none` 就不能选 R 本身（R 包含其 worktree 根），
拒绝信息说明原因。

**G 之外（明确不承诺）。**

- 准入之后，同一仓库的 attached-worktree track 仍在该仓库下创建自己的 worktree（可能落在 D 内）；删除它时移除那个 worktree
  根、管理目录与分支。这些只作用于新的自有位置，D 的其余文件不变。worktree 父的子 track 继承的来源目录本来就包含父的
  worktree，属同一情形。
- 历史 lease 的 Neige 自有 refs（见下“历史事实例外”）。
- Planner 请求的 forge/`git_commit` 在 `agent_cwd` 执行 `git add -A` 与提交（`server/mcp_server/transport.rs:1086-1110`、
  `crates/calm-types/src/forge_git.rs:26`）：这是 agent 发起的写入，不是 kernel 生命周期 effect；UI 文案因此不写“Neige 不触碰”。
- 准入之后外部进程把 D 改名、换成指向自有位置的链接：不防御。
- 旧行（#1830 之前的 attached 行与子 track）不重新准入：子 track 只经过 managed 根检查，#1830 之前的行没有重叠检查。它们得到
  行级 no-op 与创建时已有的保护；I1a 的 PR 在一份真实数据目录上统计与自有位置重叠的旧行数量作为证据，有则另立 issue，
  本计划不加启动检查。

**历史事实例外（显式）。** #1830 之前的 attached track 可能留有已释放的 `workspace_leases` 行（`git_common_dir` 非空，0115），
它们解码为 `none`。Track/Area 删除时照旧按这些冻结 lease 记录删除该 track 前缀下的 candidate 与 gate-run refs：这会写用户仓库
的 `.git` 并运行 reference-transaction hook，但只删除 Neige 自有 refs，是 G 的明确例外；`none` 分支不对 refs 清理做 no-op，否则
历史 refs 从“会清理”变为泄漏。I1a 同时审计历史未完成 operation/交付的重放：列出持久化后会对 track cwd 做 Git 或文件 effect 的
operation kind（task gate run、交付/合并、catch-up、checkpoint、worker 准备），逐一写明对 `Attached ∧ worktree = None` 行的重放
结果（在 effect 前被拒，或由原 owner 以冻结字节驱动），结论写进 PR，不以“新 worker 被拒”代替。

**数据模型：不加 migration。** `none` ≡ `kind = Attached ∧ worktree = None`，这正是 `AttachedFromCwd` 写出的行形状。
calm-types 新增闭合枚举 `WorkspaceIsolation { Managed, Worktree, None }` 和唯一解码函数 `TrackWorkspace::isolation()`；
`Managed` 表示 kernel 独占目录，不是用户可选等级。不加 `workspace_isolation` 列：它与 `worktree` 是否为空是同一事实，
加列会形成两份编码再用 CHECK 绑定。S2b 引入 view 描述符时同片扩展解码（worktree 与 view 互斥由那次 migration 的 CHECK 保证）。
现有的 #1830 之前 attached 行与子 track 自动解码为 `none`：它们今天就直接在来源目录运行。

**API（I1b）。** `CreateTrackRequest` 增加可省略的 `isolation: "worktree" | "none"`，路由在一处解码成必填的 Rust 枚举
`AttachedIsolation`，下游不再出现 `Option`、默认值或回填：

- 显式 cwd 且省略 `isolation` → `worktree`。这是现有“显式 cwd 即 worktree”合同的唯一兼容规则（Planner 已接受），OpenAPI 写明
  条件默认，PR 中作为有意例外说明；
- 省略 cwd（managed）却给出 `isolation` → 400；
- `none` → 带非重叠证明的 attached 计划（行形状同 `AttachedFromCwd`；`AttachedFromCwd` 本身仍只给 fixture 与
  `Repo::track_create`）；`worktree` → 现有 `AttachedWithTrackWorktree`；
- 幂等：`isolation` 进入 `CreateRequestShape`。digest 只在解码结果为 `none` 时写入 `"isolation": "none"`；省略与显式 `worktree`
  是同一个创建，都不写字段，因此升级前 binding 的载荷字节不变（与 `admin` 的先例相同，`create.rs:427-465`）。keyed 重放沿用
  原选择，不重新准入、不物化、不改选择；
- wire：`Track.workspace` 只在一个投影点输出必填的 `isolation`（取 `isolation()` 的结果），ToSchema/TS 把它标为响应必填；DB 与
  任何持久载荷不写该字段，反序列化不读它，因此嵌入 `TrackWorkspace` 的持久 operation/事件载荷形状不变。若某个持久载荷直接
  序列化 `TrackWorkspace`，投影放在 HTTP/事件 wire 类型上而不是 `TrackWorkspace` 本身。重新生成并提交 OpenAPI 与
  `fe/core/api/generated/wire.ts`，同片扫 Rust/FE 手写的 `TrackWorkspace` 夹具。

**运行行为。** Planner、对话与终端的 cwd = `path`（`agent_cwd()` 不变）。终端行为不变：`decode` 今天已把 `none` 形状解成
`ExternalDir` 并跳过 0166 观察；I1a 只把这条判断改写为对 `isolation()` 的穷举 `match`。旧的非 Git attached 行照旧能启动终端。
binding 行照旧作为选择审计写入。worker 的 0166 观察围栏只对有 worker checkout 的 owned 准备生效，保持不变。worker task 继续以
既有拒绝字 `track-without-worktree` 拒绝，消息改为说明“该 track 隔离等级为 none，task worker 需要 worktree”。publish 照旧要求
worktree。两个 `none` track 可以指向同一目录，kernel 不加互斥。area 的 `area_folders` 认领是 area 级路径命名空间，不是 track
互斥，保持不变。

**每条 kernel 破坏性路径如何跳过 `none`（I1a）。** workspace-runtime 各入口改为对 `isolation()` 做穷举 `match`，`None` 分支是
显式 no-op 并注明保证 G。入口今天只收字符串（`remove_track_worktree(track_id, &str)`、`WorkspaceTrackSweep.track_worktree:
Option<String>`、`ClosedWorktreeFacts::closed_idle_worktrees` 返回 `(String, String, i64)`），所以 I1a 改为传递 typed 数据：
sweep 捕获在删除事务内用既有行解码器读出该行的 `TrackWorkspace` 并携带解码后的等级与 worktree 目标；clean 端口返回 typed 行；
`remove_track_worktree` 接收只能由 `Worktree` 分支构造的目标。不在适配器里从路径字符串推断等级，也不复制解码。今天的 SQL
守卫语义等价，保留不动。

| 路径 | 今天的守卫 | `none` 的结果 |
|---|---|---|
| `recycle_track_workspace`（Track/Area 删除、area 补偿）、`workspace_allows_runtime_recovery` | 非 Managed → `Refused(NotManaged)` / 允许 | `match`：只有 `Managed` 回收；`none` 目录不在 managed 根内（准入保证），其他 track 的回收也到不了 |
| `remove_track_worktree`、post-commit sweep 的 worktree 部分 | 该行 `worktree` 为 Some | `match`：只有 `Worktree` 删除；`none` 目录不在任何 worktree 根内 |
| 10 分钟 clean | `CLOSED_TRACK_WORKTREES_SQL` 要求 `workspace_worktree_path IS NOT NULL` | 不入候选；runtime 侧对 typed 行 `match`；`git clean` 只在 worktree 根内 |
| candidate / gate-run refs 删除 | 按该 track 全部 lease 行的 common dir | 新 `none` 行没有 lease（worker 被拒）→ 空；历史 lease 照旧删除 Neige 自有 refs（显式例外） |
| area 删除 | 逐 track recycle + sweep；只删 managed 根下的空 area 目录 | 同上；managed 根之外不动 |
| materialize / `ensure_track_worktree` | 只对 Managed / `worktree` 为 Some | 不创建任何目录 |
| lease、`ensure_clean_tree`、catch-up `reset --keep` | 需要 worker checkout | 在任何 effect 之前被拒 |
| repoint | attached 在创建时冻结 | Conflict |
| trash GC | 只扫 `<root>/.trash` | `none` 目录不在 managed 根内，无关 |

I1a 的 PR 另外按 `agent_cwd`/`path` 消费者扫一遍 kernel 侧写入（附件已是 managed-only，track 文件端口只读），结果写进 PR，
不做成永久名单。

**UI（I2）。** 选择文件夹后，新建 track 的 composer 出现两个选项：`Worktree (isolated)`（默认）与 `Directly in this folder`。
后者与 track 头部/信息里的徽标都标为 **“Not isolated · no guarantees”**，说明 agent 直接改动这个目录（包括 Planner 的提交落在
当前分支），Neige 不加锁、不监控、不记录证据、不交付；Neige 不会创建、清理或删除这个目录；task worker 不可用。准入被拒时
显示服务端给出的冲突位置。复用现有 chip/选项组件与图标，遵循 `fe/AGENTS.md`；FE 只读 wire 的 `isolation` 字段，不再从
`worktree` 是否存在推断。

**开放问题（待用户决定，不阻塞 I1）。** ① `none` 是否最终允许 worker task（需要任务系统新增“无交付管理”的模式，建议另立）；
② 是否放开非 Git 目录；③ 子 track 继承父的来源目录（worktree 父的子 track 今天也是 `none`），是否改为各自建 worktree，
这是既有行为，建议单独立 issue；④ “包含 worktree 根即拒绝”会让仓库根在仍有 worktree track 时不能选 `none`，worktree 的
effect 本身只限于其根，是否对 worktree 根放宽为只拒绝“位于或等于”。

### 4.3 隔离切片

| # | 结果 | 验收 | 等级 |
|---|---|---|---|
| I1a | calm-types `WorkspaceIsolation` 与 `isolation()`；workspace-runtime 破坏性入口穷举 `match` 并改收 typed 数据；终端 decode 改为 `match`（行为不变）；worker 拒绝消息；历史 replay 审计与旧行重叠统计写进 PR。用户可见变化只有 worker 拒绝消息 | ① `AttachedFromCwd` 夹具：Track 删除、Area 删除（含补偿）、关闭后 clean 计时器之后，来源目录及其 ignored/未跟踪文件逐字节不变，且没有 `.claude/worktrees`；② 升级前夹具：#1830 之前的 attached 行带已释放 lease（`git_common_dir` 非空）与该 track 的 candidate/gate-run refs，删除后恰好这些 refs 被删、来源目录逐字节不变；③ 旧的非 Git attached 行仍能启动终端且不触发观察；④ worker task 被拒。`-p workspace-runtime -p calm-server` 聚焦。约 400 行 | L2：破坏性路径 |
| I1b | 请求字段、路由映射、非重叠准入、幂等形状、wire 投影与生成产物 | HTTP 创建 `none` → 无 worktree、无 branch、Planner cwd = `path`；反例：cwd 位于、等于或包含 managed 根、system area、`.trash`、managed track 目录、他人 worktree 根，以及经符号链接指向它们 → 400；准入后 owner 不可触及：`none` 在 R 的子目录，同仓库的 worktree track 与 managed track 依次关闭（clean 计时器运行）、删除，Area 删除、trash GC 之后，`none` 目录逐字节不变；G 之外的情形：`none` 在 R，之后建并删除 R 上的 worktree track，只有那个 worktree 根与其分支消失；managed 带 `isolation` → 400；省略字段 = worktree；幂等：升级前 binding（带消息与无消息各一）以省略字段重放成功且 digest 字节不变，显式 `worktree` 同键重放成功，`none` 同键冲突，原目录已消失时重放不重新准入、不物化、不改选择；生成器产物无漂移。约 500 行（生成文件另列） | L2：API/持久选择 |
| I2 | FE 选择、徽标与准入拒绝提示 | `cd fe && npm ci && npm run lint && npm run build && npm test`，真实浏览器预览（docs/development-preview.md）与相关 browser 测试 | L1：纯 UI，只消费已评审的 wire 字段 |
| O1（另立 issue，待接受） | worktree 准入复用同一检查，拒绝位于或等于自有位置的 cwd | cwd 在 managed track 目录内或他人 worktree 根内 → 400；仓库根上已有 worktree 时新 worktree 照常创建。约 150 行 | L2：创建准入 |
| （S2b） | `view` 成为 `WorkspaceIsolation` 的第三个变体，随其后端描述符 migration 与创建/关闭/删除/恢复同片 | 按 [workspace-view-backend.md](workspace-view-backend.md) | L2 |

## 5. 推荐顺序与理由

1. **I1a → I1b → I2 先行。** 不依赖任何 cutover；直接满足用户的 typed 选择；把 borrowed 选择变成显式且标注“无保证”的
   非资源目标，并以准入规则保证它与自有位置不重叠，这是 D1–D3 能删除的前提之一。I1a 的穷举 `match` 也让以后加入 `view`
   时每个破坏性入口都必须表态。
2. **C0a–C0d 与 I1 并行**（D1/D6 被接受后）：按请求组的纯删除，缩小 executor 协议面；兼容规则在 C0a 落地。
3. **C1** 关闭唯一已知的清理竞态，规模小，与其余切片无依赖。
4. **O1**（接受后）在 C5 之前。**C2a → C2b/C3a/C3b/C4 → C5**：先让各种启动成为 executor 事实，再让破坏性 effect 依赖这些
   事实；顺序反过来，C5 只能继续依赖 DB 代理事实。
5. **C6 可选**，然后 **S3 → S2b**：view 需要 S3 的证据与 C5 的正向静默，作为第三个后端最后接入。

## 6. AGENTS.md 架构五点与永久成本

- **抽象边界**：隔离词汇在 calm-types；保证 G 的破坏性政策在 `workspace-runtime` 的入口（M1 之后的唯一 owner），入口收 typed
  数据；非重叠准入与 `AttachedInheritedPath` 同在 calm-truth 的创建计划模块（它拥有 `TrackWorkspacePlan` 与创建事务）；FE 消费
  wire 字段，不推断领域政策；进程事实只来自 executor（C2–C5）。
- **重复逻辑**：一个解码函数 `isolation()`、一个 wire 投影点、一个 lexical + canonical 重叠比较（子 track、`none` 与 O1 共用），
  没有并行的守卫列表；C1 复用既有两把锁；C2–C4 每接入一种启动就删除对应的 Legacy spawn 分支，C5 删除已迁移类型的 Legacy
  占用代理；C0 删除无调用者的协议。
- **硬编码假设**：`WorkspaceIsolation` 是本层有意闭合的集合，`view` 作为新变体加入；自有位置是 kernel 自己的两类目录
  （managed 根与 worktree 根），由配置与 DB 行给出，不按 provider、模板或路径字符串（如 `.claude`）推断所有权。
- **外部合同事实**：Git 根只取 `rev-parse --show-toplevel`；`none` 只要求它成功；worktree 根取 DB 中 kernel 写入的
  `workspace_worktree_path`，不从路径布局推断；executor 的版本与持久形状取自其协议常量。
- **永久成本**：没有 migration、新表或名单。新增一个等级要改：枚举、解码函数、workspace-runtime 中约 6 处穷举 `match`、
  请求枚举、FE 标签映射，每处一次，由编译器强制。新增一类自有位置要在重叠检查的位置集合中加一处。准入检查每次 attached
  创建运行一次（一次 SQL 读与少量 canonicalize）。新测试放在既有 target 中，每个 CI workflow 运行一次；增删测试不改任何名单。
  C1 是进程内围栏，不新增持久状态；C0 每片 envelope +1，持久合同常量只在持久形状改变时变化；C6 若实施会新增一类
  operation kind。删除 D1–D3、D6 会显著减少 launch-admission 计划中的永久面。

## 7. 评审处置（计划评审第一轮）

| 发现 | 处置 |
|---|---|
| boundary B1 / adversarial B1：按行跳过不能阻止另一 owner 的回收、清理、删除到达 `none` 目录 | §4.2 定义自有位置与保证 G；`none` 准入拒绝位于、等于或包含自有位置的 cwd，复用 `AttachedInheritedPath` 的双检查；列出 G 之外的情形；I1b 加入 owner 关闭/删除的反例验收；§3.1 以非重叠合同重新论证 D1–D3，并把既有的自有/自有重叠列为 O1 |
| adversarial B2：#1830 之前的行带历史 lease，删除时仍删 candidate refs | 保留为显式例外（只删 Neige 自有 refs）；I1a 审计历史未完成 operation/交付的重放；I1a 验收加入升级前 lease/refs 夹具 |
| boundary B2 / adversarial N1：0166 盘点错误，I1a 的终端行为变化不存在 | §1 改正：`none` 形状今天就解成 `ExternalDir` 并跳过观察，无 checkout 的 worker 也跳过；owned worker 准备以观察 identity 作 Stale/CAS 围栏并保留。I1a 不再声称终端行为变化；旧的非 Git attached 行照旧启动终端，新的 Git 准入不作用于旧行与 keyed 重放 |
| boundary B3 / adversarial N4：C0 规模低估，C2 合并两个消费者，规模口径不一 | C0 按请求组切为 C0a–C0d 并给出源文件盘点、保留清单与 envelope/持久合同兼容规则；C2 拆为 C2a（pipe + half-close）与 C2b（gate），C3 按 spawn 点拆分，C4 每个启动或恢复入口一片；§3.2 统一规模口径（手写行，生成产物单列） |
| boundary N3：入口只收字符串 | I1a 改为传递 typed 数据给 remove/sweep/clean，不从路径推断 |
| boundary N1 / adversarial N3：省略 isolation 的幂等字节与历史 binding | 省略与显式 `worktree` 同形且不写字段，只有 `none` 写入；I1b 加升级前 binding 的重放矩阵 |
| adversarial N5：`isolation()` 不会自动成为 wire 字段 | 单一 wire 投影点，不持久化，不改持久载荷形状 |
| boundary N2 / adversarial N2：C1 未命名锁与持有区间 | §3.2 命名 drive mutex 与 `track_delete_locks`，逐入口给出持有区间，不加第二套锁；残余崩溃风险无上界，交 Planner 接受 |
| boundary N1（I1b 默认值） | Planner 接受省略 → worktree 作为唯一记录在案的兼容规则 |
| adversarial coverage：Planner forge/`git_commit` 写入来源目录 | 列入 G 之外，UI 文案不写“Neige 不触碰” |
