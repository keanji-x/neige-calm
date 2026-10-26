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
   Neige 自有位置之间互不重叠（O1），新的 `none` 选择在准入时与全部自有位置不重叠（§3.1、§4.2）。剩余 cutover 收窄为 C0–C6。
3. `none` 隔离等级**不依赖** resource cutover 的任何部分，但**依赖新的准入规则**：按被操作 track 自身的行跳过破坏性路径
   并不够，因为另一个 owner 的回收、清理或删除可以作用到同一物理目录。`none` 的准入拒绝位于、等于或包含任何
   Neige 自有位置的 cwd（§4.2 的保证 G，含已启动 teardown 子进程跨崩溃存活的显式例外，待 #2862 闭合）。在此收窄下，
   `none` 可以在当前模型上落地（I1a/I1b/I2，无 migration）。
4. 推荐顺序：先 I1a→I1b→I2，同时做 C0；然后 C1；再 C2–C5（Engine 消费者与正向静默；C4 只迁 Planner，C4.0d 先行）；task worker 的迁移是独立里程碑 W（§3.3；重写设计 [task-worker-execution.md](task-worker-execution.md) 推荐 claude-worker 复用 Agent Runtime、terminal-worker 只用 gate 那样的 Pipe execution，只依赖 C4），
   C5 不等待它；C6 可选；之后 S3→S2b（view）。
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
| 锁：每卡 `planner_recovery_locks` → 全局 operation drive mutex（`OperationRuntime::lock_for_track_delete`；`drive()` 持有它；boot 的 `apply_recovery_item` 在 Recover 与 Compensate 取 claim 前也持有它，`driver.rs:1474-1475`、`:1502-1503`，早先基线不持有）→ 每 track `track_delete_locks`（绕过 OperationRuntime 的直接恢复/重连），不得反向。创建持目标 area 的 `area_delete_locks`，先于以上各锁；单 Track DELETE 不取 area 锁 | `server/state.rs:112-141`、`server/operation/driver.rs:75-78,400-408,444-446,1204-1214`、`server/harness/mod.rs:245`、`server/routes/planner_recovery.rs:47`、`server/routes/tracks.rs:722-727,3123-3126` |
| Planner turn issuance：既有 runtime 从 Idle/TurnCompleted 发下一 turn 只取自身的 `inner.issuance`，复查 archive、shutdown、live carrier 后转 Issuing、持久化 snapshot 再发送，不取 drive 或 track 锁；clean 的 SQL 只排除 `starting/running/turn_pending`，注释写明 idle Planner 不阻止 clean | `server/harness/run_loop.rs:3074,3287-3314,3351-3371,3533-3547`、`server/harness/state.rs:59-68`、`server/workspace_service/ports.rs:65-73` |
| 删除的 post-commit 窗口：删除事务内捕获 `WorkspaceTrackSweep`（worktree 字符串），行删除提交后经若干 await 才在 `spawn_blocking` 中 `worktree remove --force` 并兜底删目录；Area 删除同样在提交与 `finalize_area_recycle` 之后 sweep | `server/workspace_service/sweep.rs:11-34`、`server/routes/tracks.rs:2939-2997`、`server/routes/areas.rs:643-716`、`runtime/sweep.rs:19-45`、`crates/workspace/src/teardown.rs:56-61,100` |
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
| ExternalDir、legacy executor、provider/直接进程 | 部分交付 | 私有 registration 复用 ExternalDir wire 形状；v2/v3 协商不变；provider/直接进程 → C2–C4（Planner）与 W（task worker，§3.3）（execution §11 E3） |
| RuntimeSafety closure（跨 alias、全部 ingress 声明） | 收窄（以非重叠合同为条件） | 只有 §3.1 的非重叠合同成立（`none` 准入 + O1）时才不需要跨 alias 闭合；保留清理竞态的围栏（C1）与破坏性 effect 的正向静默（C5） |
| One task-admission cutover | 建议删除 | D3（同样以非重叠合同为条件） |
| Independent release/cancellation | 收窄，可选 | C6：删除事务内写释放作业，提交后与启动时消费 |
| Six C4 seams | 拆分 | WorkspaceRef/resolve/WorkspaceContent → S2b；占用 = `unsettled_executions`（E2b 已用于 typed 终端，C2–C5 扩展）；Revision/GitFacts → S3 |
| Smallest slices 1–3 | 被本文 §3.2 取代 | — |

M1 的非目标仍然有效：`identity` 留在 calm-server；两份 `RecycleRefusal` 的重复按 follow-up issue 处理。

## 3. 剩余 resource cutover

### 3.1 收窄依据与建议删除（待接受）

**非重叠合同。** Neige 自有位置（§4.2 定义：managed 根整体，含 system area 与 `.trash`；每个 live track worktree 根；
进行中的 teardown 目标）是 kernel 创建、回收、清理、删除 effect 的全部目标。这不是一条持续成立的全局“无 alias”不变量，
D1–D3 的前提是三条更窄的事实：(a) **准入时排除**：每个新准入的 `none` 选择在准入那一刻与全部自有位置互不重叠（§4.2）；
之后只有一种被明确接受的重叠——同仓库后来创建的 worktree 落进 `none` 目录，删除时只移除那个后来创建的自有子树（§4.2
“G 之外”），旧行不重新准入；已启动 teardown 子进程跨崩溃存活不受进程内登记保护，是 G 的显式例外（#2862）；
(b) **owned/owned 不重叠**：自有位置之间按 track 一一对应、互不嵌套，由 O1 保障；
(c) `none` 不参与 task admission（worker 被拒）。今天 (b) 有一个既有缺口：

- **O1（既有缺陷，另立 issue，待接受）** 显式 cwd 的 worktree 准入不排除 managed 根或他人 worktree 根：cwd 选在 managed
  track A 的目录内（managed 目录是 Git 仓库）或他人 worktree 根内时，新 worktree 落在 A 之下，删除 A 时被整体移进 `.trash`
  或被 `worktree remove --force` 带走。修复：worktree 准入复用 §4.2 的同一检查，但只拒绝“位于或等于”自有位置（包含 worktree 根
  的仓库根正是 worktree 的正常用法）。这是对既有创建行为的收窄，须 Planner 接受；在 C5 依赖按 track scope 的占用之前完成。

在 (a)–(c) 成立后：

- **D1 删除** kernel 持 FD、SCM_RIGHTS 反向导出、`SourceObjectLease`/`RegisteredObjectWitness`、CanonicalWorkspaceId
  活索引与别名归一、runtime generation。理由：每个执行已在 executor 内持有自己 Prepare 时打开的 root；自有资源互不重叠，
  资源键就是 track_id；准入时已存在的静态符号链接别名由 canonical 检查拒绝；准入**期间与之后**由外部进程改名或换链接
  都不在防御范围内（canonical 检查只是一次观察，不是原子租约；与 `AttachedInheritedPath` 一致，§4.2）；交付需要的内容身份由 S3 的 Revision 按内容证明，不用 inode。worker 准备的 0166 观察围栏
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

每片独立可合入；除 C2a 协议前置片外有真实生产消费者，按父合同 §6 的公共 gate G 验证（text ratchet、contract、
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
| C1 | 关闭后清理的围栏，复用既有两把锁、不加第二套 lifecycle 锁（持有区间见下） | 进程内围栏，不加表。验收：clean 在 SQL 采样后暂停 → reopen 并启动 → clean 拒绝且不写 cleaned；clean 先赢时，worker 准备、终端创建、typed admit、Planner 启动/恢复、boot 恢复项各自得到 Busy（可重试，不 Stuck）或等待且 clean 结束后正常启动（每个入口一个测试，不只测 reopen）。既有 idle Planner 双向：① issuance 先赢——clean 采样后、取 track 前暂停，向 idle Planner 投入排队消息使其持久化 Issuing，恢复 clean → 复查看到 `turn_pending`，拒绝且不写 cleaned；② clean 先赢——clean 持 track 停在 `git clean` 之前，分别投入排队消息、自动观察与 pending rewind → 不持久化 Issuing、不发送 turn/start，clean 结束后各自照常发出；③ 删除持 track 时有 issuance 在等待 → 删除完成且该 turn 不发出、无死锁。约 350 行 | 无 |
| C2a | typed Pipe `CloseStdin` 前置能力：Interactive 精确 capability，关闭 writer、保留输出与生命周期 | 真实 client→wire→executor→child EOF 与 Engine 入口；幂等 close、关闭后 write 拒绝；PTY/未授权/Unknown 拒绝；envelope 9，持久合同与 journal 仍为 4 | E2b（已合入） |
| C2b.0 | 行为不变的准备片，缩小 C2b.1 | ① `tests/execution_hooks.rs:42-75` 的 supervisor 装配移入 `tests/support`（移动，不复制），`execution_hooks` 改用它；② 通用写者对 `terminal_launch` 的三处特判（`writes.rs:155-158`、`:250-253`、`:306-315`）改由该层一个 owner-state 键常量生成，此时常量只有 `terminal_launch`；③ owned 留存守卫（见设计）：`owns_parked_resource()` 改为返回 owner-state 键，四个离开写者带必需的键参数并在事务内留存、返回 `Retained`，driver 对 `Retained` 记错误并在 lease 到期后再 drive；④ owned Parked reconcile 的错误留存；⑤ 新迁移去掉 parked CHECK 对非 isolated 行的 artifacts 要求，该要求只留在 Rust（④⑤ 见设计）。生产适配器都不声明键，CHECK 只放宽，Rust 的 artifacts 围栏不变，故行为不变；迁移使本片为 L2。新测试：lib `claim_tests::owned_key_retains_terminal_writes`、`owned_parked::tests::error_keeps_lease_and_writes_last_error`，calm-truth `typed_gate_park_migration_tests::{upgrade_preserves_rows_refs_indexes_and_keyed_fence, parked_check_keeps_only_owner_neutral_facts}`；replay `execution_hooks` 全部、lib `repo_sqlite::claim_tests`、`operation::tests::{set_parked_requires_lease_and_artifacts_record_rejects_stale_lease, parked_return_without_artifacts_fails_and_drops_observer}`、`terminal_adapter::tests::launch_cleanup_tests` 与 calm-truth `operations_keyed_rows_permanent_tests`。估算 +52/−48，守卫与 `Retained` +40，`mod.rs` +4/−2，`driver.rs` 调用点与延迟 drive +25/−4，28 个测试调用点改参数 +28/−28，新测试 +40；错误留存 +12/−3 与其测试 +40；迁移 +90（表定义取自 0152，只改一支）、`head_schema_fixture.rs` 迁移清单 +1、迁移测试 +90；合计约 +422/−85 ≈ 510 | C2a |
| C2b.1 | worker gate run（`task-gate-run`）改为 Engine 消费者；同片引入两种 gate 共用的唯一构造点 `server/operation/gate_execution.rs`、必需记录 `gate_execution` v1、typed parked 关联与 typed 占用读者（设计见下） | 范围：Pipe intent 工厂（沿用现有环境白名单；consumer `gate` 注册显式 no-op projection）、记录 CAS（SQL 在 calm-truth）、owner-state 常量加入 `gate_execution`、owned 适配器 park 的 Rust 条件不再要求 `spawn_artifacts_json`（表约束由 C2b.0 迁移放行）、owned 的 Boot 恢复先于 deadline 分流、释放顺序、完成事务内复核 decision、Stop 型 timeout、gate 适配器声明 `gate_execution` 键（留存不变量由 C2b.0 的守卫执行）、boot 矩阵的 gate run 列（SpawnStarted 收窄为 infra）、`gate_ops` 的混合占用合同、`finalize_run` 改收结算 status、`ExecutionUnavailable` 拒绝、pre-C2b 原生 `task-gate-run` op 不推进（task verify 到 C2b.2 才适用）。测试世界接 C2b.0 移出的真实 executor support，`launch_kernel` 重启测试的 supervisor 留在测试进程、活过 kernel kill。删除：`task_gate_run/mod.rs:300-400,448-527` 的重驱 `stop_group`、`spawn_held_identified`/`record_spawn_artifacts`、原生 `recover_parked`、Observer 的 stdin 写入/`observe_verdict`/reap 及原生补偿分支。新测试 `c2b_gate_run_has_one_engine_construction`、`c2b_gate_run_refuses_unavailable_executor`、`c2b_release_authorizes_before_first_byte`、`c2b_refused_release_writes_no_byte`、`c2b_phase_writes_preserve_gate_record`、`c2b_typed_park_needs_no_spawn_artifacts`、`c2b_redrive_of_held_ends_infra_without_second_execution`、`c2b_boot_held_stops_without_token`、`c2b_boot_after_deadline_keeps_boot_origin`、`c2b_changed_decision_aborts_completion`、`c2b_timeout_retries_while_probe_holds_claim`、`c2b_stop_decision_survives_unavailable_and_restart`、`c2b_cancel_parked_leaves_typed_op_parked`、`c2b_typed_gate_op_occupies_until_settled`（新 run 准入、regate、reuse、delivery 各一断言）、`c2b_lost_close_ack_does_not_repeat_steps`、`c2b_leader_exit_drains_descendants_before_verdict`、`c2b_unknown_execution_never_completes_op`、`c2b_pre_c2b_native_op_is_never_advanced`、`c2b_park_write_failure_retains_owned_execution`、`c2b_recorded_op_never_enters_compensation`、`c2b_malformed_record_is_retained_with_backoff`、`c2b_owned_park_requires_decodable_record`。replay：mcp_integration_suite `task_gate_run::{a_gate_run_commits_new_files_before_its_steps_run, a_red_step_returns_its_name_and_tail, an_edit_during_the_run_discards_its_result, done_during_a_run_delivers_after_the_run, a_cancel_during_a_run_keeps_the_checkout_busy_until_it_ends, more::the_run_releases_its_wrapper_only_after_parked, restart::*, commit_message::done_before_checkpoint_release_accepts_the_admitted_input, reuse::an_unchanged_candidate_reuses_the_passing_run}`；kernel_process_suite `parked_operations::{recovery_mode_plumbing_distinguishes_boot_probe_and_past_deadline, boot_recovery_past_deadline_ignores_abandoned_parked_lease}`；lib `task_gate_run::tests::run_recovery_never_reads_the_exit_file`、`execution_boot::port_contract::*`、`repo_sqlite::claim_tests`、C2b.0 的两条 `operation::tests` 原生 artifacts 用例与 `terminal_adapter::tests::launch_cleanup_tests`（pin `terminal_launch` 保留）。`restart::a_restart_before_the_park_reruns_nothing`（R8a）保留名字，断言由“重驱后 Succeeded”改为“step 未执行、run 以 infra 结束”（见 boot 矩阵）。估算（增/删）：`gate_execution.rs` +215；calm-truth 记录 CAS 与结算读取 +45；`writes.rs` 常量与 `set_parked` 的键条件 +8/−1；`driver.rs` +30/−18；`gate_ops.rs` +30/−6；`task_gate_run/mod.rs` +110/−200；`finalize.rs` +20/−15；重启 support +35；22 个新测试 +380；R8a 等既有用例改写 +20/−15。合计约 +893/−255 ≈ 1150，超出约 800：每个生产消费者与其不变量测试保持原子（typed op 从第一次合入起就必须被准入、park、boot、完成与占用读者一致读取），可前置的行为不变整理（含留存守卫）已移入 C2b.0，PR 按上列文件组说明 | C2b.0 |
| C2b.2 | task verification（`task-verify`）改为同一构造点的 Engine 消费者：释放移到 park 之后，Released 的 boot 只重试 CloseStdin 并重新挂 observer，after-sample 只认 Drained | 范围：observer 释放、`Exited` 后 Stop 收尾子孙、两个计时器（见下）、boot 矩阵的 task verify 列、新 attempt 前要求前一 attempt 的 gate execution 已结算（替代 kill prior）、恢复判决取 executor status 而非 exit file、`gate_ops` 对无记录的原生 op 改判 Unproven（此后不再产生原生 op），无记录的原生 `task-verify` op 自此不推进（boot 矩阵末行）；scheduler 测试世界接 C2b.1 的 executor support。删除：`task_verify_adapter/mod.rs` 的 `spawn_side_effect`（`:368-558`）中的 kill-prior（`:383-417`）、`spawn_held_identified`（`:423`）、`tasks.gate_pid*` 写入与 park 前释放（`:433-495`）、原生 observer/reap（`:511-558`），`recover_parked`（`:560-664`）的 PID 存活与 exit-file 恢复，补偿 `kill_gate_group`（`:720-741`）；verify 对 `target::stop_group`（`target.rs:691`）的调用改为结算事实，函数本身留给 C2b.3。列保留，读者改为 execution 事实。新测试 `c2b_verify_releases_only_after_park`、`c2b_boot_released_retries_only_close_then_reattaches`、`c2b_crash_before_token_is_infra_not_relaunch`、`c2b_timeout_probe_preserves_durable_decision`、`c2b_after_sample_requires_drained`、`c2b_new_attempt_requires_prior_execution_settled`、`c2b_recovered_verdict_uses_executor_status`、`c2b_settled_verify_after_deadline_keeps_status`、`c2b_native_terminal_gate_op_is_unproven`。replay：runtime_suite `scheduler::` 中 `green_gate_flips_verifying_to_done`、`red_gate_fails_with_failing_step_and_log_tail`、`gate_timeout_*`、`gate_spawn_kills_prior_recorded_group`、`forged_exit_file_and_group_kill_cannot_flip_gate_green`、`step_exit_ends_step_and_still_writes_exit_file`、`gate_step_env_is_minimal_and_evidence_paths_scrubbed`、`closed_track_still_drives_verifying_gate`、`stale_gate_before_first_attempt_fails_without_running_shell`、`parked_gate_dead_*`、`boot_reattach_live_gate_lands_verdict_after_exit`、`aborted_observer_leaves_gate_group_alive_and_reattach_lands_verdict`；mcp_integration_suite `gate_binding::{gate_result_discarded_when_tree_changed_during_steps, reattached_gate_result_discarded_when_tree_changed, recovered_gate_stops_the_group_before_sampling, live_reattached_gate_result_discarded_when_tree_changed, gated_candidate_happy_path_wakes_planner_once, legacy_lease_gate_runs_without_target_check, pre_upgrade_frozen_gate_boots_as_unbound}`、`task_regate::regate_reruns_gate_on_same_candidate`；lib `task_verify_adapter::verdict_tests::*`；加 C2b.1 的 `c2b_` 过滤器。改写 PID/exit-file 证据的用例保留名字与断言意图，`parked_gate_dead_with_exit_file_recovers_real_verdict` 与 `pre_upgrade_frozen_gate_boots_as_unbound` 改以 settlement status 为恢复证据（后者用新的 typed 记录构造，不得借此恢复真正无记录的 pre-C2b op）。估算（增/删）：`task_verify_adapter/mod.rs` +150/−290；`target.rs` +15/−20；`gate_execution.rs` 的 Released boot、重挂与授权计时 +60；`gate_ops.rs` +8/−10；`tasks.gate_pid` 读者 +20/−15；scheduler support +40；9 个新测试 +150；PID/exit-file 证据用例改写 +60/−90。合计约 +503/−425 ≈ 930，超出原因同 C2b.1：task verify 这一生产消费者与其不变量测试保持原子（同一 op 的 spawn、park、boot、恢复与占用读者必须同片切换） | C2b.1 |
| C2b.3 | 删除已无调用者的共享原生 gate 机器（纯删除片） | `gate_process.rs:263-324` 的 `spawn_held`、`:328-426` 的 `GateWait`/`GateObservation`/WNOWAIT `observe_verdict`、`:429-541` 的 group/marker 存活、sweep 与 `kill`（`kill` 先做 caller sweep）及其测试模块 `:543-677`，`gate_lifecycle.rs:137` 的 `spawn_held_identified` 与直接调用它的 `gate_lifecycle/unsupported_gate_tests.rs` 用例（`:149`），`target.rs:691` 的 `stop_group`，以及 caller sweep 证明只服务它们的 `target.rs` kill 与 `proc_identity` marker helper；`marked_group_stopped` 的最后一个生产调用者 `gate_ops.rs:107` 已在 C2b.2 改判，故此片无行为变化。Forge/provider/terminal 仍在用的 helper、`verdict_from_exit_code`、wrapper 与 step/exit 文件保留。无新测试；replay `verdict_tests::*` 与 C2b.1/C2b.2 全部 `c2b_` 过滤器，caller sweep 证据写入 PR。估算约 −470（`gate_process.rs` 约 −410、`gate_lifecycle.rs` 约 −45、`target.rs`/`proc_identity` 约 −60，减去测试用例删除后的 import 调整），兼容改动不超过 60 行 | C2b.2 |
| C3.0 | 行为不变的准备片：① 把 `server/operation/gate_execution.rs` 中与 owner 无关的机制抽到 `server/operation/owned_execution.rs`，calm-truth 的记录 SQL 按 owner 键参数化（清单见 C3 设计的“共享 helper”）；② `ParkedRecovery::Complete` 携带 owner 已算好的 `Option<OwnerEvent>`，由 `owned_parked::reconcile` 的完成事务 append、提交后发布（gate/verify 传 None）；③ forge 的测试世界（target forge_action_adapter 与 forge_suite）接 C2b.0 移出的真实 executor support，`forge_suite` 重启测试的 supervisor 留在测试进程、活过 kernel kill | 生产消费者：worker gate run 与 task verify 改用抽出的 helper 与新的 `Complete` 形状，gate 记录形状、CAS 语义与全部 `c2b_` 行为不变；forge 仍是原生，只换测试 support。删除 `truth/db/sqlite/gate_execution.rs` 中写死 `$.data.gate_execution` 的 `insert`/`compare_exchange`/`require_record`，由按键参数化的同名函数取代（键须在 `OWNER_STATE_KEYS`）。新测试 lib `owned_execution::tests::c3_keyed_record_cas_never_touches_another_key`、`owned_parked::tests::c3_complete_appends_owner_event_in_completion_tx`（测试适配器带 event：与完成同事务，回滚则无 event，提交后才发布）；replay mcp_integration_suite 与 runtime_suite 的 `c2b_` 过滤器、lib `task_gate_run::tests`、`task_verify_adapter::verdict_tests`、`repo_sqlite::claim_tests`、`owned_parked::tests`、`execution_boot::port_contract`，以及 forge_action_adapter 与 forge_suite 全部（断言不变）。规模（设计值）：抽取 +130/−120（移动按增删双计）、`Complete` 携带 event +30/−10 与测试 +35、support +80/−40，合计 +275/−170 ≈ 445；按 C3 设计的规模校准预计约 750–1150 | C2b.3（已合入） |
| C3.2a | 已关闭 worktree 清理器消费 forge-owned 的占用检查，forge 声明 `preparation_track`：修复 main 上已有、与 Engine 无关的缺口（设计见 C3 设计的“清理栅栏”） | 生产消费者：10 分钟 clean 的 fence（`server/workspace_service/ports.rs:109-135`）与每个 `forge-action` 的 Pending 准备（`driver.rs:658-670`）。范围：fence 在既有锁下调用 `track_has_active_forge_action`（`server/operation/workspace_lease/mod.rs:73-110`），不在 `checkout_occupancy` 或 runtime 复制其 SQL；`ForgeActionAdapter::preparation_track` 返回 payload 的 `track_id`，复用 C1 的通用 Busy 重试，通用代码无 forge 特例。新测试 track_suite `track_delete_forge_fence::c3_closed_worktree_clean_refuses_active_forge_action`、`track_worktree::c1_fence::c3_forge_action_after_clean_check_waits_for_clean`（`c1-clean-fenced` 暂停后提交并驱动 forge action，clean 结束前无准入、无释放，之后照常完成）；replay track_suite `track_delete_forge_fence::` 与 `track_worktree::c1_fence::`、forge_action_adapter 全部。规模（设计值）：fence 与端口 +30、`preparation_track` +10、测试 +100，合计约 +140/−5 ≈ 145，预计约 250–380 | C1（已合入） |
| C3.2b | 原生 forge action 的凭据改走释放 stdin 载荷，判决与 event 构造改为纯函数；Engine 之前、C3.2c 原样复用的两项（设计见 C3 设计的“构造与环境”） | 生产消费者：今天的原生 action（`spawn_side_effect`，`server/operation/forge_action_adapter/mod.rs:1271-1494`）。范围：① `render_forge_wrapper`（`:182-230`）首行 `exec >/dev/null 2>&1`，按固定键序 `IFS= read -r` 读取凭据载荷与终止标记，不符或 EOF 即 `exit 75`；kernel 侧载荷渲染与校验（换行、NUL、超长在写第一个字节之前拒绝，走今天的释放失败路径）；原生 observer 以载荷取代 `go\n` 写入 child stdin（`:1353-1362`），action 的 spawn 环境不再含 `FORGE_CREDENTIAL_ENV_KEYS`；probe 仍经 `apply_forge_subprocess_env`（`:336-351`）继承凭据，到 C3.2c 才随迁移改走载荷。② 把 `complete_forge_op_with_result`/`_from_live_result`/`_succeeded`（`:829-989`）中的判决、artifact 与 event 构造抽成返回 `(ParkedOutcome, Option<OwnerEvent>)` 的函数，原生路径仍以 `complete_parked_tx` 提交，判决不变。新测试 target forge_action_adapter `c3_credential_payload_round_trips_shell_values`（未设、空、空白、反斜杠、shell 元字符）、`c3_partial_credential_payload_never_runs_action`、`c3_refused_release_writes_no_byte`（换行、NUL、超长值）；`forge_action_recovery_probe_env_passes_auth_and_strips_forbidden`（`tests/forge_action_adapter.rs:2193`）中 action 的断言改为载荷送达、probe 的断言不变；replay forge_action_adapter 全部、lib `operation::forge_action_adapter::tests`、forge_suite、mcp_plugin_forge_action 全部。规模（设计值）：wrapper +25、载荷渲染与校验 +45、observer +15/−8、判决函数 +70/−60、新测试 +120、既有用例改写 +30/−30，合计 +305/−98 ≈ 400，预计约 700–1050 | 无 |
| C3.2c | forge action 与它的两种 probe（verdict 与 output，今天的 `run_probe`，`mod.rs:999-1023`）一起改为 Engine 消费者：唯一构造点 `server/operation/forge_execution.rs`、必需记录 `forge_execution` v1，probe 是同一记录拥有的 execution，durable 排斥（设计见下） | 生产消费者：MCP plugin forge 工具经 `server/mcp_server/transport.rs:920` 提交的每个 `forge-action` op。范围：action 与 probe 共用的 Pipe intent 工厂（凭据只走 C3.2b 的 stdin 载荷）、键加入 `OWNER_STATE_KEYS`、consumer `forge` 的 no-op projection（`server/execution_boot.rs:18-28`）、`owns_parked_resource()` 返回该键、owned park、park 后释放、boot 矩阵（含 probe 行）、probe 的 create-if-absent 准入与释放、计时与 Stop、经 `owned_parked::reconcile` 完成（decision event 经 C3.0 的 `Complete`，与 action 及全部 probe 的 Retire 同事务）、`ExecutionUnavailable` 拒绝、pre-C3 原生 op 不推进且 forge-owned 占用检查保留其 Stuck 行。删除：`kill_artifacts_group`（`:299-303`）与重驱 kill-prior（`:1278-1280`）、原生 `Command`/`setsid`/`SpawnArtifacts`（`:1295-1333`）、observer 的 stdin 写入、释放失败直接判 infra 与 `child.wait`（`:1342-1488`）、原生 probe 与 dead 恢复漏斗 `run_probe`/`complete_from_probe`/`resolve_post_release_via_probe`/`resolve_dead_outcome`（`:999-1201`，`verdict_from_exit_code` `:991-997` 保留为 `.code` 映射）、原生 `recover_parked` 的 PID 存活轮询（`:1496-1589`）、补偿 `kill_forge_action_group` 的 kill（`:1591-1638`，改为拒绝）、直接写 `complete_parked_tx` 的 `complete_forge_op_failed` 与 C3.2b 判决函数的原生提交外壳（`:800-827`、`:933-989`）；`apply_forge_subprocess_env` 改为构造 intent 环境向量（同一白名单，无凭据键）。新测试、replay 与 mutation 见 C3 设计。规模（设计值）：构造点与记录（含 probe 子记录、准入、释放、计时）+315、adapter +260/−590、注册、键与原生 Stuck 检查 +20/−3、24+1 个新测试 +750、原生 PID/EOF/probe 用例改写 +280/−300，合计 +1625/−893 ≈ 2520，预计约 4300–6500。远超 800：一个生产消费者与其不变量测试必须同片切换（见 C3 设计的“切片与规模”），可前置的部分已移入 C3.0、C3.2a、C3.2b，其中约 −590 是删除后无调用者、clippy 不允许留下的原生代码 | C3.0、C3.2a、C3.2b |
| C4.0d | **已由 #2999（`36b22516c`）交付；C4 的第一片，独立可部署**：全局未完成 execution 的周期恢复。Engine 的组装 owner（`server/execution_boot.rs`）在 boot `recover` 之后起唯一的 30s `Engine::recover` 节拍，同片加 pending 部分索引；生产消费者是既有 gate、forge 与 typed 终端，它同时修复它们今天的缺口（`StoreError` 或提交后 drive 丢失后要等重启） | StoreError 与丢失 drive 的回归（gate、forge、typed 终端），见 C4 设计“全局周期恢复” | 无 |
| C4.0a | 行为不变：forge 的 stdin 载荷编码与 wrapper 读取前缀抽成按键表参数化的共享模块（设计见下“C4 设计决定”） | forge 全部 replay 不变 | 无 |
| C4.0b | 行为不变：Claude/ACP Planner 测试世界接真实 executor support，重启测试的 supervisor 活过 kernel | 断言不变 | 无 |
| C4.0c | `worker_sessions.provider_execution_json` 迁移与 calm-truth 记录 SQL，无消费者 | 迁移与 CAS 测试 | 无 |
| C4.1 | Claude rewind 预检成为第一个 Engine 消费者，同片引入 Planner provider execution 的 owner 层（记录、owner 完成端口、同 session 的唯一准入规则、durable sweep、boot pass、`EngineStdio`、有界 stderr、代理分类函数与 release wrapper）；rewind 的秘密代理经 Held stdin 载荷，Engine 进度依赖 C4.0d | 见 C4 设计 | C4.0a、C4.0b、C4.0c、C4.0d（Q2 若选另一支，另加同宿主正向排空片） |
| C4.2 | Claude Planner turn 与 compaction 改为 Engine 消费者；MCP token 走 release stdin 载荷；复用 C4.1 的代理分类函数（`server/execution_environment.rs`） | 见 C4 设计 | C4.1 |
| C4.3 | ACP Planner turn 改为同一 owner 层的消费者；运营者 env 值走载荷 | 见 C4 设计 | C4.2 |
| C4.4 | 删除 Planner marker 的运行期机制（保留 boot 时的 pre-C4 sweep）；Claude readiness 加进程组回收，ACP catalog 加总时限 | readiness/catalog 回收测试；replay C4.2/C4.3 | C4.2、C4.3 |
| C5 | 破坏性 effect 的正向静默：recycle、`remove_track_worktree`、sweep、area 补偿在 fence 下要求 `unsettled_executions(scope)` 为空，否则 Busy 并保留目录；已迁移类型的 Legacy 代理判断同片删除，未迁移类型的 Legacy 检查保留；forge action 与 probe 自 C3.2c 起是 execution、由该谓词覆盖，pre-C3 原生 forge 行（含 Stuck）没有 execution，其 forge-owned 检查保留；C4 之后仍不是 execution 的还有：task worker（terminal-worker、claude-worker、codex viewer）在 W 之前仍是 Legacy PTY，与 owner 自建 card 的 Legacy PTY、升级前的 Legacy worker 行一样保留 Legacy 占用与静默检查（见 C4 设计 Q6 与 §3.3）；共享 Codex app-server 下的 Codex 线程保留线程级检查（Q7）。C5 不等待 W。W 按 [task-worker-execution.md](task-worker-execution.md) 的路线 A 合入后，新启动的 claude-worker 是带 track 的 provider execution（`server/provider_execution/launch.rs:44`），terminal-worker 是带 track 的 Pipe execution（同 gate，`server/operation/gate_execution.rs:79`），都落在同一 `unsettled_executions(Track)` 谓词之内，不加新谓词；尚未准入的 `claude-worker-turn` 行由 C1 的 clean 复查视为待发行（该文 §3.6）；这两个 kind 只为升级前的 Legacy 行保留 Legacy 检查；codex viewer 的 Legacy 检查保留到迁移 | 拆为 C5a–C5c，合同 C5-D1…C5-D7 与各片验收见下“C5 设计决定”。占用不只按 owning track：O1 之后显式 cwd 的 typed 终端仍可运行在另一 track 的目录内，占用 = owning track 的未结算执行 ∪ 根位于目标之内的未结算执行（C5-D1，源根解码失败即关闭）；显式 cwd 准入对目标的 owner 锁 try-lock，owner 按 managed 布局与进行中的 teardown 登记求得，提交之后、sweep 或 rmdir 之前也挡得住；forge probe 准入用同一围栏（C5-D6）；删除路径保持“先 quiesce、后写者复查”，并在既有围栏下、第一次移动之前加一次正向证明，restore 不做新证明、永不因证明失败而跳过，restore 失败的条目在 track 行仍在时不被 trash GC 回收（C5-D2、C5-D5）；Area 的证明包含 Area 容器目录（C5-D5）；repoint 保持“不 Stop 即拒绝”（C5-D3），其目标经与 track 创建相同的 O1 准入（C5-D7）；retired trash 与一次性 objective checkout 的豁免，以二者都在 O1 的自有位置集合内、且显式 cwd 被拒绝为前提（C5-D4）。验收：未结算执行（含他 track 拥有、根在目标内者）时 Track/Area 删除、repoint、clean 返回 Conflict/延后且目录、branch 与行保留 | C2a–C4、O1（已在 main，`truth/db/sqlite/track.rs:123`；不依赖 W） |
| C6（可选） | 释放作业：删除事务内写入待回收的 worktree/refs 描述，提交后 sweep 消费，启动时补跑 | 修复“提交后崩溃 → worktree 与 branch 永久泄漏”；不改 7 天 trash 与回收顺序。未消费的作业行同片并入 `none` 准入的自有位置集合（§4.2 进行中的 teardown 目标），验收加“作业未消费时准入其目标 → 400”。若接受泄漏只记 follow-up，则不做，也不声称已有 crash-safe release | C5 |
| W | task worker 迁入 Engine：claude-worker 走无界面的 Agent Runtime（路线 A），terminal-worker 只走 Pipe execution（不提供 PTY 变体），codex viewer 按 OQ6；重写设计见 [task-worker-execution.md](task-worker-execution.md)（第三版，设计已定稿：两路 L2 设计评审第 3 轮通过，用户已确认方向，ask 686425；仍只是设计，未实现），WR1–WR7 的处置见 §3.3 | 见 [task-worker-execution.md](task-worker-execution.md) §7 | C4（与 C5 无硬依赖：先于 C5 合入时，C5 对 claude-worker 的 Legacy 检查只为升级前的行保留） |
| Legacy wind-down | W 之后迁移 owner 自建 card；升级时排空 pre-upgrade Legacy 行；随后删除剩余 Legacy 检查与 EnsureProc；保留 Codex thread 检查 | 见下文 Legacy wind-down；动工前 L2 设计 | W |

**C2a 范围决定。** 只读盘点未发现 C2b/C3/C4 之外独立的双向 track pipe 消费者。有界短 Git/CLI 读取
（checkpoint、sampling、lease reads）仅在进程内由 deadline + process-group reap 与 C1 fences/leases 覆盖。
kernel 死亡后这些 deadline/析构与进程内 fence 不再生效；遗留的短读 process group（包括 Git filter/hook 的副作用）
不在 C5 的 empty-execution predicate 覆盖内，属于 #2862 的同一残留类别（child lifetime / PDEATHSIG）。
这里接受较窄的保证，不把短读迁入 Engine；C5 不得据执行集合为空声称已解决此跨崩溃静默缺口。
迁移还需 Engine 当前没有的 stdout/stderr 分离与
每次 sample 的归属，成本不值得；不为名义消费者而扩大本片。C2a 仅补齐后续迁移的 Pipe half-close。
forge probe 不属于此类：它运行 plugin argv、继承凭据，今天只回收直接子进程，C3.2c 起与 action 一样是 Engine execution（见 C3 设计），
在 C5 的谓词之内；此类不为 probe 留例外。
升级到 envelope 9 必须重启 supervisor；严格拒绝 envelope 8，不回退 Legacy；持久 receipt 合同和 journal 版本保持 4。

**C2b 设计决定。** 路径前缀：`server/`、`supervisor/`、`runtime/`、`truth/` 分别指 calm-server、calm-proc-supervisor、
workspace-runtime、calm-truth 的 `src/`（迁移为 `truth/migrations/`）。四片串行（C2b.0 行为不变），每种 gate 只有一条路线，不保留可选的原生回退。

- **构造。** `gate_execution.rs` 是唯一构造点：由冻结 cwd、GateFiles 和现有环境白名单（`server/operation/gate_process.rs:285-309`
  的 env_clear、五个继承键、`terminal_worker_env`、证据路径与 `NEIGE_GATE_OP`）构造 Pipe `ExecutionIntent`，owner 为 op，
  alias 每个 gate op 一个（`workspace_executions_current_alias` 唯一索引，`truth/migrations/0170_workspace_executions.sql:14-15`）。
  Pipe 把 stdout/stderr 并入 executor 自己的输出流（`supervisor/execution/process.rs:177-184`），而判决证据是 gate log，
  所以 wrapper 在握手之后执行 `exec >>"$log" 2>&1`，log 路径经环境传入后 unset（与 exit/step 路径一样）。脚本、log、step
  与 exit 路径在准入事务之前写好，不启动一个之后才写脚本的 intent。缺 Engine 或 `!available()` 时，gate run 准入和
  spawn 前都返回 `CalmError::ExecutionUnavailable`（同终端，`server/operation/terminal_execution_create.rs:27-32`），
  不建 execution 行，不 fork；envelope 8 已由 C2a 拒绝。
- **记录。** 必需的 `operations.tx_output_json.data.gate_execution`，`version` 为 JSON 整数 1：`{execution_id, release: Held | Releasing{authorized_at_ms} |
  Released{authorized_at_ms}, decision: Observing | Stopping{reason: Timeout | KernelRestarted | ReleaseRefused}}`（Released 表示 CloseStdin 已确认；不设 Canceled，见留存不变量）。
  `intent_json` 不可变（`0170:19-27` 的 trigger），不能放可变位，记录放 `tx_output_json`；park 的表约束需要新迁移（见下）。记录有三种写入：① 准入，在 SpawnStarted
  的原 claim 下，与 `require_live_claim_tx`、`Engine::admit` 同一写事务，仅当记录缺失时写 Held；提交后才 `drive`
  （`runtime/execution/engine.rs:43-60`，ticket 类型见 `mod.rs:166-182`）。② Held→Releasing→Released（确认后）。③ Observing→Stopping
  与 `Engine::request(Stop)` 同一事务（Store 的 Tx 就是 operation 的 SQLite 事务，同终端的 `admit(tx, …)`）。
  CAS 条件固定 op id、`execution_id`、当前 `release` 与 `decision`；条件 SQL 放 calm-truth（`server/execution_store.rs:9`）。
  只有这三种写入，没有把记录换绑到另一个 execution 的写入（见 boot 矩阵的 SpawnStarted）。②③ 的写事务都先
  `require_live_claim_tx`（`writes.rs:73-80`）：live observer、deadline sweep 与 boot 都先取 claim 再写，同一时刻只有一个 release/decision 写者。重读只刷新 owner state，必须保留 claim_parked 返回的原 token；token 不匹配就停止并重新取 claim。
  取不到 claim（`claim_parked` 返回 None，`server/operation/mod.rs:939-962`）不消耗一次性的计时器到期或 ReleaseRefused：
  写者重读 op 并重试，直到 op 终结或该 decision 已 durable（`c2b_timeout_retries_while_probe_holds_claim`）。
  通用写者今天只特判 `terminal_launch`（`server/operation/repo_sqlite/writes.rs:155-158`、`:250-253`、`output_value` `:306-315`），
  否则补偿写会因为 `settlement_output_matches_tx` 不匹配而被拒绝。C2b.0 先把特判改为该层一个 owner-state 键常量，
  两个 UPDATE 和比较都从它生成；C2b.1 只在常量里加 `gate_execution`。`complete_parked_tx` 在事务内重读行，本身保留记录
  （`writes.rs:318-372`），由 `c2b_phase_writes_preserve_gate_record` pin 住。永久成本：C2b.0 的键参数是一次性的签名与调用点迁移
  （四个写者、28 个测试调用点，编译检查），不留调用者或测试清单。日后另一个 owned 资源要做 owner 注册：适配器声明键、owner 的
  记录编解码与结算、常量加键；parked CHECK 不读 owner 记录，不随 owner 加迁移；只有既有 owner 多保留一个字段才只改常量。新增的 `c2b_`
  与迁移测试随所在 suite 每个 CI workflow 各跑一次；mutation 只在实现 PR 内做一次，不进 CI。
- **释放顺序（两种 gate 相同，只在 park 提交后由 observer 执行）。** ① durable authorize：Held→Releasing 与
  `require_attempt_startable_tx` 在同一写事务，扩展 `TaskLaunch`。它今天先检查、再在事务外运行 effect
  （`server/operation/task_launch.rs:71-91`）。之后先等 execution 有可交互的 Spawned receipt（`interact` 先 inspect，无 receipt
  即拒绝，`runtime/execution/engine.rs:301`、`:353-358`）：Prepared/Forking 只是等待，不是 ReleaseRefused，计入 60s 上限。② `Engine::interact(WriteStdin "go\n")`，同一调用内按 `StdinProgress.accepted_bytes`
  只重试剩余后缀。③ `Engine::close_stdin` 确认后，在原 claim 下将 Releasing→Released。① 输了，或 ②③ 失败、超过 60s，就走 ③ 型 Stopping{ReleaseRefused}；
  永不用关闭未授权的 stdin 来取消。Releasing 是 durable 的未确认释放；任何 claimant 看到它已结算，都先写 Stopping{ReleaseRefused}，不能取退出码判绿。
  崩溃后看到 Releasing 不再写字节，按未确认释放保守判 infra；Released 不再写字节。token 未完整送达时，
  `read -r _go || exit 75`（`gate_process.rs:19`）让 shell 不执行任何 step，没有已开始的 step 就判 infra（`:201-207`）。
  task verify 今天在 park 前释放（`task_verify_adapter/mod.rs:433-495`），SpawnStarted 重驱又先 kill 再重启（`:383-416`），
  所以释放后、park 前崩溃会让 step 跑第二遍；改为 park 后释放，SpawnStarted 就只可能是 Held。这是有意的行为变化。
- **Timeout 与 Stop。** Timeout、ReleaseRefused、KernelRestarted 都走 decision ③ 加 `Desired::Stop`，op 留在 Parked。Desired 只增（`runtime/execution/mod.rs:79-84`，
  `0170:29-35` trigger），executor 先 TERM、再 KILL（`supervisor/execution/shutdown.rs:7-38`）。完成事务里同时请求
  `Desired::Retire`，由 executor 收尾输出与回收（`retire_waits_output_open`）。两个计时器不同：授权计时
  `authorized_at_ms + timeout` 才是 gate 的 timeout，live observer 与 boot 重挂都按它的剩余时间等待，到期写 ③ Stopping{Timeout}；
  `parked_deadline_ms` 是 park 时刻 + timeout + 120s 余量（`gate_lifecycle.rs:31`、`:62-65`）的 sweep 安全网。observer 在 park 后立即授权时，释放上限
  60s 小于余量，授权计时先到期；但 park 到授权之间的延迟没有上界（`task_gate_run/mod.rs:173` 的 `before_release` seam 也能
  停住它），所以 parked deadline 是独立的安全上限，不依赖这一先后。PastDeadline 时未结算且仍 Observing 就写 ③ Stopping{Timeout}，已结算则走下面的结算规则：
  已结算、无 decision 的 Released verify 不论在 boot 还是过期 sweep 被发现，都取 settlement status，同今天“死且有 exit 证据先于
  mode 判断”（`task_verify_adapter/mod.rs:576-599`）。settlement 不带退出时间，kernel 停机期间不承诺严格的墙钟 timeout。
- **回收与 timeout/probe 围栏。** gate 不再持有 Child，删除 WNOWAIT 僵尸锁（`gate_process.rs:352-426` 的 `observe_verdict`、
  `task_gate_run/mod.rs:489-496`）。observer 经 `Engine::output` 观察状态：`Exited{status}` 时请求 Stop 收尾子孙（对应今天的
  无条件 `kill`，`gate_process.rs:398`），再在有界的 `wait_settled` 内等到 `Drained{status}`。status 是 helper 报告的 leader 退出码
  （`supervisor/execution/scope_helper.rs:168-178`），drain 证据是 subreaper（`:84` 设置）的 ECHILD（`:176-178` 之后才报 Drained），
  不读 /proc 环境，不可读的子孙不再削弱 drain。observer 和 probe 都经 `owned_parked::reconcile` 的 claim 围栏完成
  （`server/operation/owned_parked.rs:11-53`），判决规则先要求权威结算：未结算一律 LeaveParked，Stopping 本身从不授权完成；
  已结算时 decision 为 Stopping{r} 取 r，否则仅在 release == Released 时取 `Drained{status}` 的 status（NoChild 判 infra）；Held/Releasing 先持久化 ReleaseRefused。after-sample 只在 Drained 之后；
  没有结算证据时 op 保持 parked，检出保持 busy。reconcile 在完成事务之前算好 outcome（`owned_parked.rs:21`），事务内只验 claim
  （`:26-28`），`complete_parked_tx` 重读行只为保留字段、不复核判决（`writes.rs:318-372`）。所以适配器只从 claim 时 `op` 快照里的
  记录推导 outcome；gate 适配器在同一事务内既有的 `complete_owned_parked_tx`（`:29-30`）重读记录，`execution_id`、`release`、
  `decision` 任一与快照不同就返回 Conflict，整个事务回滚，reconcile 按下文的错误留存写 `last_error`、保留 lease，lease 到期后
  下一轮（observer 重试或 sweep）以新快照重算，Stopping 优先。②③ 必须持 claim 已排除这一竞争；复核是同一 owner 内的防线，不加通用 hook。
- **owned execution 留存不变量（一条类不变量，取代逐个例外）。** 有记录的 op 只能停在 SpawnStarted 或 Parked，离开它们的唯一写入是
  上面 reconcile 的完成事务（结算后判决，`complete_owned_parked_tx` 同事务复核记录并请求 Retire）。前三轮阻断同属一类：通用路径
  （saga 补偿、`mark_failed`、普通与 boot 外层的 `mark_stuck`）只验输入、phase 与 claim（`writes.rs:55-64`），在 Stop、结算与
  Retire 之前终结 op。
  - 唯一守卫点：通用 repo 的四个离开写者 `set_phase`（`repo_sqlite.rs:344-388`）、`set_compensating`（`:409-456`）、`mark_failed`
    （`:490-536`）、`mark_stuck`（`:538-581`）在同一写事务、`has_live_claim_tx` 之后调用 `writes.rs` 的一个 helper：行的
    `tx_output_json.data.<key>` 存在就不写目标 phase。key 问适配器：`owns_parked_resource()`（`mod.rs:810`）改为返回
    `Option<&'static str>`（必须是 C2b.0 owner-state 常量里的键，`is_some()` 即原 bool），四个写者以必需参数接收，漏传编译失败。
    守卫读事务内的行而不读 claim 快照：B1 中 driver 的 `op` 是准入前取的快照，没有记录。记录只在 SpawnStarted 的 claim 下写入，
    claim 又校验 phase，所以 Pending/TxCommitted/AppServerInteract 来源的写者看到的行必无记录，照旧终结。保证范围是现有生产
    调用路径：`write_phase_and_tx_output` 不带守卫、不限来源 phase（见表）；新增从 SpawnStarted/Parked 调用它的生产路径须先接同一守卫。
  - 留存：同一事务只写 `last_error`（“owned execution retained: …”），phase 与 lease 不变，返回区别于丢 claim 的 `Retained`；
    driver 记 error 日志，并在 lease 到期后安排一次 detached `drive()`（同 `retry_after_track`，`driver.rs:524-552`；稳态没有周期
    drive）。Parked 行由 parked sweep 在 lease 过期后重取（`mod.rs:939-962`）。退避就是 lease（60s，`mod.rs:76`）：
    `claim_drive_batch` 跳过未过期 lease（`repo_sqlite.rs:186-210`），持续错误每个 lease 周期重试一次、不热循环；op 非终态，
    `gate_ops` 报 Unfinished，准入、regate 与 delivery 都能看到该 op 与 `last_error`。
  - 重试路线（稳态与 boot 相同）：重驱 SpawnStarted 时 `spawn_side_effect` 入口已有记录，typed park 同一 execution，observer
    不释放，写 ③ Stopping{KernelRestarted} 加 Stop，结算后 infra 加 Retire（boot 矩阵的 SpawnStarted 行）。这解决第三轮 B1：
    `set_parked`（`driver.rs:790`）失败后外层 `mark_stuck`（`:464-468`、`:1309-1316`）被留存，下一 lease 周期重驱完成上述路线。
  - Parked 的错误留存：owned Parked 路径不经四个写者。`owned_parked::reconcile` 把 `recover_owned_parked` 的错误（含记录解码）、
    `Fail` 与完成事务的错误都上抛（`owned_parked.rs:21-24`、`:43-46`），`:50-53` 却先无条件清 lease、不写 `last_error`；driver 只记
    日志（`driver.rs:984-998`），下一次 sweep 立即重取（`mod.rs:939-962`）。C2b.0 把 `:50-53` 按结果分开：完成已提交或 LeaveParked
    照旧清 lease；任何 Err 改为一条同样以 `lease_owner = 本 claim AND phase = 'parked'` 为围栏的 UPDATE，只写 `last_error`
    （“owned execution retained: …”）与 `updated_at_ms`，lease 不动，再返回该错误。lease 即退避：稳态 `claim_parked` 跳过未过期
    lease，每个 lease 周期重试一次。boot 不看 lease，一次 boot 对起始即 parked 的行至多两次立即尝试：`apply_recovery` 先以 Boot
    claim 处理 VerifyParked 项（`driver.rs:489`、`:1371`），再无条件 `sweep_parked_for_boot`（`:510`、`:984`），其
    `claim_parked_for_boot` 不看 lease（`mod.rs:965-976`）；之后由稳态 lease 退避。保留末次 sweep，它也发现恢复中新 park 的行。围栏
    0 行（claim 已被替换）就不写。它在通用层、与 owner 无关，形状同四写者的 `Retained`；Stop、结算与 Retire 仍归 owner。
  - 损坏的记录：守卫只看键存在、不解码，照样留存；SpawnStarted 重驱与 Parked reconcile 的解码错误分别经外层守卫与上一条留存，
    每个 lease 周期记一次错误。这是不变量违背，
    不自动终结，因为终结会丢掉 Stop 该 execution 的唯一句柄；op 与检出保持 busy 并带 `last_error`，直到人工处理。
  - 不选“准入、记录与 park 同事务”：它要把通用 phase 写入搬进适配器的准入事务，而 park 之后的写者（缺 deadline、补偿入口、
    外层 Stuck）仍需同一守卫，不能单独闭合这一类。由此删去两条逐例外规则：gate 的 `plan_compensation` 读到记录返回 Conflict，
    以及“记录之后 `spawn_side_effect` 不返回 Err”。`cancel_parked` 没有生产调用者（只有 `operation/tests.rs:443`），task cancel
    也不停 gate（`a_cancel_during_a_run_keeps_the_checkout_busy_until_it_ends`）；它对 owned 适配器仍在 claim 前返回 `Ok(false)`，
    免得占住 observer 的 parked claim；日后的取消只能在 claim 下写 ③ 并保持 Parked。executor 不可用或未结算时 ③ 只写 DB，
    reconcile 一律 LeaveParked，boot 不等 executor。Stuck 只留给无记录的 op 与 pre-C2b 原生补偿。

  | 终结或补偿写者（HEAD 行号） | 有记录时 |
  |---|---|
  | Pending 失败 `mark_failed`（`driver.rs:596-614`）；TxCommitted `set_phase(Succeeded)` 与补偿（`:649`、`:658-676`）；AppServerInteract 补偿（`:746-754`） | 来源先于 SpawnStarted，行必无记录，照旧终结 |
  | SpawnStarted 的 `Ready` → `set_phase(SpawnSucceeded)`（`:768`）及 SpawnSucceeded → `Succeeded`（`:834`） | 留存，重驱走 typed park |
  | SpawnStarted 补偿入口：未声明 Parked、缺 artifacts、Err（`:779-787`、`:807-815`、`:820-828`，经 `fail_with_compensation` `:873-888`） | `set_compensating` 留存；无记录时照旧补偿（无进程步）后 Failed |
  | 普通外层 `mark_stuck`（`:458-475`）与 boot 外层 `mark_stuck`（`:1296-1324`） | 留存（B1） |
  | 补偿收尾 `resume_compensation` 的 `mark_failed`/`mark_stuck`（`:890-925`，经 Compensating drive `:843-852` 与 boot Compensate `:1326-1348`） | 有记录的 op 进不了 Compensating；到达也留存 |
  | `cancel_parked`（`:413-428`） | claim 前返回 `Ok(false)`；到达 `set_compensating` 也留存 |
  | 缺 deadline 的 `fail_claimed_parked`（`:1019-1027`、`:1241-1255`） | 留存；typed park 的 deadline 必需（`mod.rs:387-390`），不可达 |
  | 原生 parked：`kill_recheck_then_fail_parked`（`:1201-1239`）、缺 artifacts（`:1125-1131`、`:1386-1396`）、`complete_parked_and_publish`（`:1181-1199`） | owned 分支先返回（`:1054`、`:1117` 与移到 deadline 分流前的 boot owned 分支）；其中 `mark_failed` 也留存 |
  | `write_phase_and_tx_output`（`writes.rs:197-278`），经 `prepare_tx_and_advance`（`repo_sqlite.rs:296-341`，来源 Pending）、`set_phase_and_tx_output`（`:390-407`；生产调用 `driver.rs:685`、`:710`、`:725`、`:739`，来源 TxCommitted/AppServerInteract）与 `PhaseWrite::Inserted`（`tx_only.rs:196`、`terminal_execution_create.rs:185`，新插入的 Pending） | 不带守卫；现有生产来源都先于 SpawnStarted，行必无记录 |
  | `owned_parked::reconcile`（`owned_parked.rs:11-53`） | 唯一终结路线；LeaveParked 释放 claim；任何 Err（解码、Fail、完成事务 Conflict）按 Parked 的错误留存写 `last_error`、保留 lease |

  回归：C2b.0 的 lib `claim_tests::owned_key_retains_terminal_writes`（测试适配器声明键，四个写者均留存且只改 `last_error`）；
  C2b.1 的 `c2b_park_write_failure_retains_owned_execution`（普通与 boot 两个入口，各在准入提交后、park 提交前注入一次失败：一个
  execution、零个 step、结算前不终结、最终 infra 加 Retire、无永久占用）、`c2b_recorded_op_never_enters_compensation`（准入后
  `spawn_side_effect` 报错、`plan_compensation` 报错两例）、`c2b_stop_decision_survives_unavailable_and_restart`（Timeout 写入，
  executor 不可用期间 op 保持 Parked，kernel 重启，executor 恢复后结算，完成与 Retire 同事务）、`c2b_cancel_parked_leaves_typed_op_parked`
  与 `c2b_malformed_record_is_retained_with_backoff`（SpawnStarted 与 Parked 各一行坏记录，boot 与稳态两个入口：phase 不变、
  `last_error` 持久、稳态 lease 期内不重取、到期只重试一次，一次 boot 恰两次尝试、不再多，不准入或终结 execution）；C2b.0 的 lib
  `owned_parked::tests::error_keeps_lease_and_writes_last_error`（测试适配器的 recover 报错、Fail、完成事务 Conflict 三例，另验
  claim 被替换后旧 reconcile 不写）。
- **Typed parked 关联。** gate 适配器声明 owner-state 键 `gate_execution`（改型后的 `owns_parked_resource()`）。driver 的 owned 路径（`server/operation/driver.rs:1031-1060`、
  `:1117-1125`、`:1368-1380`）目前没有实现者（只有 trait 默认值，`server/operation/mod.rs:810`），缺三处：`set_parked` 的
  `spawn_artifacts_json IS NOT NULL` 条件（`writes.rs:113-117`）和 `driver.rs:796-812` 的缺 artifact 检查，对 owned 适配器改为
  由适配器解码必需记录，不伪造 PID 三元组；以及 boot 来源：`VerifyParked` 先按 deadline 分流（`:1360-1366`），过期的 owned op
  经 `apply_parked_past_deadline_with_claim` 以 `RecoveryMode::PastDeadline` 进入（`:1117-1124`），Boot 丢失。改为 owned 适配器
  在 deadline 判断之前走 Boot 分支（仍以 `ParkedClaimMode::Boot` 取 claim），以 `RecoveryMode::Boot` 调用；`op` 带着
  `parked_deadline_ms`，来源与 deadline 都到 owner，由它按矩阵与上面的计时规则决定。不加新的 RecoveryMode；原生适配器顺序不变
  （`calm-server/tests/cases/parked_operations.rs:406-419` 的原生断言照旧）。
- **park 的表约束（新迁移，C2b.0）。** 0152 的 parked CHECK（`truth/migrations/0152_operations_awaiting_retry.sql:57-75`）要求非
  `codex-isolated-worker` 行 `spawn_artifacts_json IS NOT NULL`（`:61`），无 artifacts 的例外只给 isolated（`:62-73`），改 Rust 条件
  也躲不过它；第四轮 adversarial 用 0152 原 DDL 复现两种 gate kind 的 park 都被拒。已发布迁移字节冻结，C2b.0 加
  `<NNNN>_parked_artifacts_in_rust.sql`（编号合入时取下一个空号）。谓词只改第一支：去掉其 artifacts 要求，成为
  `kind <> 'codex-isolated-worker'`；parked 的 `parked_at_ms`/`parked_deadline_ms` 事实与 isolated 支逐字不变。schema 不读任何
  owner 记录：在通用表 CHECK 里解析 `gate_execution` 的字段是 owner 私有政策，且每个日后的 owned 资源都要再加一支迁移（第五轮
  boundary B1）。不选“artifacts 或 `tx_output_json IS NOT NULL`”：parked 行都经过 TxCommitted，`prepare_tx_and_advance`
  （`repo_sqlite.rs:296-341`）已写入 tx output，原生与 owned 行都满足，它区分不了任何行，不是 parked 的不变量。artifacts 要求
  留在今天就执行它的 Rust：非 owned 适配器由 `set_parked` 的 `spawn_artifacts_json IS NOT NULL`（`writes.rs:104-117`）与 driver
  的缺 artifact 检查（`driver.rs:796-815`）拒绝，C2b.0 起这是原生行唯一的围栏，由 replay 的 `operation::tests::{set_parked_requires_lease_and_artifacts_record_rejects_stale_lease,
  parked_return_without_artifacts_fails_and_drops_observer}`（`tests.rs:280`、`:1645`，后者经生产 driver 与 `set_parked`）pin 住，
  生产中只有专用 `set_parked` 可进入 Parked；通用 phase 写者与直接 SQL 不在这个围栏内。
  实现 PR 分别做 mutation：只删去 `writes.rs:117` 的条件时两条用例应变红；只删去 driver 的缺 artifact 检查时，`parked_return_without_artifacts_fails_and_drops_observer` 应变红（driver 检查在 `set_parked` 返回未写入之后）。C2b.1 起 `set_parked` 以 owner-state 键参数分支：未声明键照旧要求
  artifacts，声明键则要求同一事务内该键存在（与留存守卫同一个通用事实），不看 artifacts。记录的完整有效性只由 owner 解码器负责：
  gate 适配器在 `spawn_side_effect` 返回 Parked 前以 `gate_execution.rs` 解码本 claim 下的行（记录只在本 claim 下写入），失败即
  返回 Err，经补偿入口被守卫留存并写 `last_error`；键缺失则不 park、守卫不留存，照旧补偿（无进程步）后 Failed，因为准入与记录
  同事务，无记录即无 execution。混合期 C2b.1 的 task verify 仍是原生，走未声明键的分支；迁移不随片或 owner 改。重建沿用 0152 的步骤：迁移
  事务内外键始终开启（`truth/db/sqlite/infra/database.rs:43`；实测重建被引用的父表在 `defer_foreign_keys` 下同样提交失败），
  所以先把 HEAD 唯一的引用列 `worker_sessions.spawn_op_id`（0163 定义；0103 的引用表已由 0125 删除）存入临时表并置 NULL；
  新表的列、默认值与其余 CHECK 同 0152，`INSERT … SELECT *`、DROP、RENAME，恢复引用，再逐字重建三个索引与 keyed 行 DELETE
  trigger（`0152:89-103`）。测试（C2b.0，calm-truth）：`upgrade_preserves_rows_refs_indexes_and_keyed_fence` 以 `migrator_through`
  （`track_merge_policy_ask_migration_tests.rs:8`）建前一版本库，灌原生 parked、isolated parked、awaiting_retry、keyed 行与引用它们的
  `spawn_op_id`，迁移后逐列相等、引用恢复、`foreign_key_check` 为空、索引与 trigger 的 `sqlite_master` SQL 不变、keyed 行仍不可删；
  `parked_check_keeps_only_owner_neutral_facts` 在正常迁移的库上：无 artifacts 的非 isolated parked 行可写入，缺 `parked_at_ms` 或
  `parked_deadline_ms` 的仍被拒，isolated 分支不变（无 receipt 与缺键行被拒）。C2b.1 经生产 driver 与 `set_parked`：
  `c2b_typed_park_needs_no_spawn_artifacts` 两种 kind 各一行；`c2b_owned_park_requires_decodable_record` 键缺失 → 不 park、
  Failed、零 execution，记录损坏 → 不 park、留存、`last_error` 持久。
- **占用读者。** `gate_ops.rs:54-92` 只读 `spawn_artifacts_json`，`:101-108` 对终态 op 以 `is_none_or` 把“无 artifacts”当作已停；
  新 run 准入（`task_gate_run/admission.rs:103-138`）、regate（`regate.rs:148-160`）、reuse（`reuse.rs:141`、`:173`）与 delivery
  （`git_candidate/delivery.rs:394-398`）都经它。C2b.1 起 `GateOp` 带一个闭合的进程来源：有记录 → 在同一事务内经 calm-truth
  读取该 execution 是否已结算，未结算为 Unproven；只有 artifacts → 照旧 `marked_group_stopped`（C2b.1 的 task verify 仍是原生）；
  两者都没有 → 已停（准入与记录同事务，无记录即未 fork）。C2b.2 之后不再产生原生 op，只有 artifacts 的 op 改判 Unproven，与
  pre-C2b 的 unresolved 政策一致；代价：升级时在途 attempt 若已有原生 gate op，升级后其新 run 与 regate 被拒（既有 Unproven
  提示），reuse 不复用，delivery 不受影响（只看 Unfinished）。调用者与枚举不变，gate 政策留在 calm-server。
- **Boot 矩阵。** 在 `Engine::recover` 只排队 drive（`server/state.rs:1584`、`engine.rs:81-101`）之后，由既有 operation 恢复
  （`server/main.rs:73`）逐 op 处理。boot 本身只写 DB（decision 加 Desired）和 drive，不等 executor：
  - SpawnStarted 且无记录、无 artifacts：准入事务未提交，没有 execution，重驱按 ① 准入。
  - SpawnStarted 且有记录（必为 Held；boot 与留存后的稳态重驱相同）：两种 gate 相同。以 typed park 停泊同一 execution，其 observer 不释放，在 parked
    claim 下写 ③ Stopping{KernelRestarted} 加 Stop（按上面的重试规则），结算后按“已记 Stopping”判 infra；不准入第二个 execution，step 一次也没跑，worker 可再跑、Planner 可 regate。
    这是有意收窄：旧行 Stop 并结算后仍占别名（`0170:14-15` 的唯一索引只排除 `retire`），Engine 对已结算、非 Retire 的行
    不再推进（`runtime/execution/engine.rs:206-213`），而 Retire 只在完成事务里请求；替换需要 Retire、准入与记录换绑同事务
    及其崩溃重放，唯一要它的是 R8a 测试（`calm-server/tests/cases/task_gate_run/restart.rs:125-171` 断言重驱后 Succeeded），不是生产消费者。
  - Parked 且 Held：两种 gate 相同。Stopping{KernelRestarted} 加 Stop，永不发 token，结算后判 infra。
  - Parked 且 Releasing：未确认释放，不重发 token；worker gate run 写 Stopping{KernelRestarted} 加 Stop，结算后判 infra。
  - Parked 且 Released（已确认）：worker gate run 走 Stopping{KernelRestarted}，判 infra，保持今天的重启合同
    （`task_gate_run/mod.rs:365-395`），不顺带改成恢复判决。task verify 只重试 CloseStdin，再按授权计时的剩余时间
    重挂 observer；已过期且未结算就写 ③ Stopping{Timeout}。
  - 已记 Stopping{r}：用同一 execution 完成 r，永不重启。
  - 已结算但无 decision：task verify 取 settlement status（NoChild 判 infra，不做 after-sample）；
    worker gate run 判 infra。
  - executor 不可用：保留 Desired 与占用，op 留在 parked；drive 等待可用（`runtime/execution/session_executor.rs:198-210`），
    新 gate 被拒。
  - supervisor 在同一 OS boot 内重启：活动行变为 Unknown，Prepared 变为 Revoked，Reserved 变为 NoChild
    （`supervisor/execution/mod.rs:186-188`）。Unknown 拒绝 CloseStdin 与交互（`supervisor/execution/controls.rs:133-141`），
    `next_step` 对 Stop/Retire 也只会 Wait。gate 不 close、不重启、不采样、不完成。supervisor 退出已关闭 writer，Held 的 shell
    读到 EOF 后以 75 退出、什么也没执行，但这不产生 durable 的 drain 证据：C2b 在同一 OS boot 内无法恢复这种 execution，
    op 与检出一直 busy，直到主机重启（NoChild）或另立的 executor 证明/对账功能。C2b 不承诺最终结算，也不加 PID 回退。
  - OS 重启：活动行变为 NoChild `host_rebooted`（`mod.rs:183-184`），结算后判 infra。
  - pre-C2b 原生 op（有 `spawn_artifacts_json`、无记录；`task-gate-run` 自 C2b.1、`task-verify` 自 C2b.2）：不发信号、不重启、不完成。parked 的 op 由 reconcile 报 unresolved，
    检出保持 busy；其余 op 与旧的 `stop_run_group`/`kill_gate_group` 补偿步骤拒绝后进入 Stuck（既有补偿失败路径，
    `driver.rs:916-923`），原因写明“在旧 kernel 下排空或手工停止 pgid N”。升级前须排空 gate。
- **Planner 质询。** (a) 接受，删去 prelude。writer 是 supervisor 的 `Process.input`（`process.rs:177-184`、`:257`）：
  kernel 死亡时 writer 不关闭，held shell 停在 `read`，什么也不执行，boot 会 Stop 它；supervisor 死亡时 writer 关闭，
  shell 读到 EOF 以 75 退出；半个 token 加 EOF 同样以 75 退出。prelude 只多给“无 kernel 时持有自行过期”，那只是占用，
  不是执行。备忘录要求“EOF 才是最终释放”，这里不采纳：token 只在 durable 授权之后才写，读到 token 就等于已授权。
  未找到具体风险。(b) 接受，不加 pre-drive hook。`next_step` 中 Absent/Prepared 加 Stop 得 Revoke，Spawned 加 Stop 得 Stop
  （`runtime/execution/mod.rs:106-127`）。Engine 抢先 Launch 的 Held intent 只是一个持有中的 shell，而 boot 从不做 Held→Releasing，
  所以先后顺序只决定是否 fork，不影响安全。(c) 接受，合并：记录与 typed park 的唯一消费者就是 cutover，typed park 只是上述三处改动。
  (d) 接受，共四片：行为不变的 C2b.0 之后三片。gate run 先行，因为它从不恢复判决，能先验证构造、释放、Stop 与 park；task verify 再加 Released 的重挂
  与 after-sample；最后一片是纯删除。mutation 验证只针对“授权先于首字节”、“boot Held 必 Stop”、timeout 围栏（含完成事务内复核）、typed 占用读者与 owned 留存守卫五条不变量。

**C3 设计决定。** 路径前缀同 C2b。Planner 决定（C3 设计评审第六轮之后）：forge action 与它的 probe（verdict 与 output 两种用途，今天的
`run_probe`，`server/operation/forge_action_adapter/mod.rs:999-1023`）都迁入 Engine，probe 是同一 `forge_execution` 记录拥有的 execution，
不再给 probe 硬化一套原生生命周期。第四至六轮 adversarial 各在那套原生 probe 生命周期里找到一个新阻断（R4-B1 预约与续租的顺序、
R5-B1 续租前被取消泄漏预约、R6-B1 tokio 的 spawn 在 OS spawn 之后仍可返回 Err，子进程逃出守卫）；按 AGENTS.md，三轮之后改方法、
删机制：Engine 已拥有进程生命周期，forge 里的第二套是重复逻辑。由此从计划删去：进程内 probe 预约与其身份守卫、继任与过渡期回收任务、
每次调用前的 claim 续租预算、probe 的 `kill_on_drop` 过渡期，以及 C3.1 片。顺序 C3.0 → C3.2a、C3.2b（两者互不依赖）→ C3.2c。等级 L2。
已收敛的 action 设计（凭据 stdin 握手、boot 矩阵、占用、`preparation_track`、清理栅栏、完成与 Retire）不变，probe 只复用它们。

- **两个 spawn 点的归类。** action 运行任意 plugin argv（`calm-types/src/forge_action.rs`），held wrapper 停泊数分钟（默认 300/900s，
  `server/mcp_server/transport.rs:872`、`:1185-1192`），释放后跨 kernel 重启靠原生 PID 恢复继续（`forge_action_adapter/mod.rs:1509-1545`），
  在冻结 `cwd_lease` 中运行（`:1288-1299`）：真实的 C5 占用，迁移。probe 是有界读（每次 60s，`:44`、`:1016`），但同样运行 plugin
  argv、继承凭据（`apply_forge_subprocess_env`，`:336-351`），今天只对直接子进程 `kill_on_drop`（`:1009-1018`），没有 session、进程组或
  子孙回收，超时后 shell 拉起的子孙可以在 kernel 存活时继续使用 worktree，不满足 C2a 例外的前提（deadline + process-group reap）。
  C3.2c 起它也迁移：spawn、deadline 的 `Desired::Stop`、先 TERM 后 KILL（`supervisor/execution/shutdown.rs:7-38`）、helper 作为 subreaper
  收养子孙并经 pidfd 转发信号（`supervisor/execution/scope_helper.rs:84`、`:259-292`）、以 ECHILD 证明的 Drained（`:176-178`）、结算与
  Retire 都归 Engine。probe 行的 track 是 `FrozenForge.track_id`，在 `unsettled_executions` 内，C5 的谓词覆盖它，C2a 的短读例外不再含 forge probe。
- **共享 helper（C3.0，只抽 owner 无关的部分）。** 抽出：`engine()` 可用性检查（`gate_execution.rs:52-58`）、`refresh_claim`
  （`:275-285`，保留原 claim token）、`settlement` 读取（`:287-296`）、`drain_exited`（`:298-320`）、释放的发送段（等 Spawned receipt、
  按 `accepted_bytes` 写 owner 传入的字节、`close_stdin`，`:212-260`）、`Release`/`Decision`/`StopReason` 三个枚举（serde 形状不变），以及
  calm-truth 记录 SQL 按键参数化、比较整个记录对象（gate 记录只有四个字段，与今天逐字段比较等价）。留在 gate：`admit`
  （GateFiles、`render_gate_wrapper`、`NEIGE_GATE_*`、consumer `gate`）、`release` 中的 `TaskLaunch::authorize_tx`（`:162-164`）、
  gate 的 `Record`/`KEY`/`Projection`、授权计时与 `finalize_run`。不建泛型 owner 框架：每个 owner 自有 Record、构造点与判决。
  同片的 `ParkedRecovery::Complete` 携带 `Option<OwnerEvent>`（这是 `ParkedRecovery` 唯一的通用改动，gate/verify 传 None）；
  完成载荷整体装箱，`reconcile` 也返回装箱 future，避免 `Event` 放大每个调用方恢复 future 的栈占用。
  `OwnerEvent { actor: ActorId, scope: EventScope, event: Event }` 全由 owner 算好，通用层不推断 actor 或 scope；
  `owned_parked::reconcile` 在完成事务内用 `append_decision_event_in_tx`（correlation None）append，提交后发布。
  forge 测试世界只换成真实 executor support，forge 本身仍是原生。
- **构造与环境（凭据载荷 C3.2b，其余 C3.2c）。** `forge_execution.rs` 是 action 与两种 probe 唯一的 Pipe intent 构造点，三者只差 argv、
  alias 与结果路径：program `/bin/sh`，args `-c` 加 `render_forge_wrapper(argv)`（`mod.rs:182-230`，握手与文件捕获），root 为
  `canonicalize(cwd_lease)`（Private root 要求，同 gate），owner 为 op，track 取 `FrozenForge.track_id`；alias 为 `forge:<op_id>`、
  `forge:<op_id>:verdict`、`forge:<op_id>:output`（`workspace_executions_current_alias` 唯一索引在 Retire 之前另外拒绝同 alias 的第二行，
  `truth/migrations/0170_workspace_executions.sql:14-15`）。结果路径：action 用 `FrozenForge.result_path`；probe 用
  `<result_path>.probe.<execution_id>`，wrapper 把 probe 的 stdout 与退出码写成它的 `.stdout`/`.code`（tmp+rename）。execution id 每次新生成，
  probe 文件不可能是旧文件，不做删除，也就没有旧 claimant 的删除能与新 probe 竞争；action 只在首次准入前 `remove_stale_result_files`（`:1287`），
  重驱不删。环境沿用 `apply_forge_subprocess_env`（`:336-351`）的同一白名单，改为 intent 的环境向量，`NEIGE_FORGE_RESULT_PATH` 照旧传入后 unset。
  **凭据只走释放握手的 stdin，不落盘。** `ExecutionSpecification.environment` 写入不可变的
  `workspace_executions.intent_json`（`0170:19-27`），supervisor journal 也持久化整个
  `PrepareExecution`（`supervisor/execution/journal.rs:18-27`），所以 `FORGE_CREDENTIAL_ENV_KEYS`（`calm-types/src/forge_env.rs:3-10`）
  不进 action 或 probe 的 intent、不建文件、C3 不扩展 `ExecutionSecrets`（它只有控制 token，且随行持久化）。源码核实：WriteStdin 字节经
  `Engine::interact`（`runtime/execution/engine.rs:274-288`）、已认证的 Interactive capability 到 `supervisor/execution/controls.rs:175-178`，直接
  `process.write`（`supervisor/execution/process.rs:60-77`）；journal 只记 request、phase 与撤销位，回复 `StdinProgress` 只带字节数，receipt 只有身份与
  slot，均无字节；唯一的落盘面是 Pipe 输出日志（`supervisor/execution/output.rs:22-31`）。因此 wrapper 第一行就
  `exec >/dev/null 2>&1`（早于读取载荷，保持今天的丢弃，`mod.rs:1301-1302`），从不 `set -x`、不回显。载荷在释放时从 kernel 进程环境
  现取：按 `FORGE_CREDENTIAL_ENV_KEYS` 的固定顺序每键一行，`0`（未设）或 `1` 加原值，再以终止标记 `go` 结束；值含换行或 NUL、总长超过
  应用层载荷上限（如 16 KiB，低于 `process.write` 单次请求的 1 MiB 上限；不依赖管道容量，续写靠 `accepted_bytes`）则在 Releasing 之前拒绝 → Stopping{ReleaseRefused}，一个字节都不写。
  wrapper 用固定键名的 `IFS= read -r` 逐行读取并 `export`，不 eval；行数、前缀或标记不符或读到 EOF 就 `exit 75`，凭完整载荷加标记
  之后才 exec argv。C3.2b 先在原生路径落地同一载荷与 wrapper（原生 observer 写 child stdin），C3.2c 只把写入换成 `Engine::interact`，
  并让 probe 也走它。非凭据键与基础白名单照旧进 intent（同 gate）。Pipe 合并 stdout/stderr（`supervisor/execution/process.rs:177-184`），
  输出日志按窗口上限保留（`output.rs:39` 夹在 1 KiB–4 MiB），所以应用输出不经 Pipe：action 与 probe 的 stdout 都由 wrapper 写 tmp 再 rename，
  probe 的 stderr 仍丢弃（同今天的 `Stdio::null()`，`:1013`）；action 的 `.code`/`.stdout` 与 artifact 发布（`:736-798`）不变。文件是应用证据，
  结算是物理寿命证据，不解析 Pipe 输出。action 的准入与记录插入同一写事务（`require_live_claim_tx`、`Engine::admit`、按键 insert），提交后
  `drive`。缺 Engine 或 `!available()` → `CalmError::ExecutionUnavailable`，不建行、不 fork：action 无记录的 op 经补偿（无进程步）进入
  Failed，不回退原生；probe 见下“probe 的释放与计时”。
- **记录。** `operations.tx_output_json.data.forge_execution`，`version` 为 JSON 整数 1：`{execution_id, release: Held |
  Releasing{authorized_at_ms} | Released{authorized_at_ms}, decision: Observing | Stopping{reason: Timeout | KernelRestarted |
  ReleaseRefused}, verdict: Unprobed | Landed, probes: {verdict: Probe | null, output: Probe | null}}`，`Probe = {execution_id, release,
  decision}` 复用同样的枚举（probe 不写 KernelRestarted）。键加入 `OWNER_STATE_KEYS`（`server/operation/repo_sqlite/writes.rs:18`）并由
  `owns_parked_resource()` 返回，于是 C2b.0 的留存守卫、`set_parked` 的键条件（`writes.rs:142-160`）与 owned boot 分流
  （`driver.rs:1192`、`:1213`、`:1276`）直接适用。`FrozenForge::from_output` 解整个 `data`（`mod.rs:107-111`）且忽略未知字段，冻结载荷不变。
  写入都在原 claim 下、与 `require_live_claim_tx` 同一写事务 CAS 整个记录：① action 准入写 Held；② action Held→Releasing→Released；
  ③ action Observing→Stopping 与 `Desired::Stop` 同事务；④ probe 准入：`probes.<kind>` 由 null 变为 `{新 execution_id, Held, Observing}`，
  与该 probe 的 `Engine::admit` 同一事务（create-if-absent），output probe 的准入同事务把 `verdict` 由 Unprobed 改为 Landed；⑤ probe
  Held→Releasing→Released；⑥ probe Observing→Stopping 与该 probe 的 `Desired::Stop` 同事务。没有把记录换绑到另一个 execution 的写入，
  也没有删除 probe 的写入：每个 op 每种 probe 至多一个 execution。
- **action 的释放顺序（同 C2b，只由 park 提交后的 observer 执行）。** ① durable Held→Releasing，先于首字节（forge 无 task 授权，只有 claim 与 CAS）；
  ② 等 Spawned receipt，按 `accepted_bytes` 写完整凭据载荷与终止标记（单次存活发送内续写后缀）；③ `close_stdin` 确认后 Releasing→Released。上限沿用 `RELEASE_TIMEOUT` 60s
  （`mod.rs:42`）；失败或超时写 Stopping{ReleaseRefused}，取代今天的直接 kill 并判 `gate-infra`（`:1363-1388`）。probe 的 ⑤ 用同一个
  共享发送段，见下。
- **probe 的排斥与所有权（C3.2c）。** 排斥就是 durable 记录，不是进程内表：
  - 只有 ④ 能产生 probe 进程。`Engine::admit` 只在调用者的事务里插入行，`drive` 只接受提交之后才构造的 ticket
    （`runtime/execution/engine.rs:43-60`，类型见 `mod.rs:166-182`），executor 只 Prepare/Launch 已提交的行；kernel 不再持有 probe 的 Child，
    也就没有 R6-B1 那种“OS spawn 成功、调用方拿到 Err”的缝隙。④ 的事务先 `require_live_claim_tx`（`writes.rs:93-118`：phase、原 token、
    租约未过期、未完成），再以整记录 CAS 要求 `probes.<kind>` 为 null。SQLite 单写者让它与竞争者取 claim 串行：竞争者 B（boot 无条件
    替换，`server/operation/mod.rs:989-1011`；过期后的 sweep，`:963-986`）先提交 claim，A 的 ④ 因 token 已换而回滚，不留行、不 fork；
    A 先提交，B 在 claim 之后读出的行（`fetch_claimed_parked`，`server/operation/repo_sqlite.rs:655`）已含 A 的 probe，B 只观察它。完成事务先验 claim（`owned_parked.rs:27`）
    并复核整个记录，所以终结提交之后不会再有 ④，也不会有人在不知道某个 probe 的情况下终结 op。
  - 后来的 claimant 观察已存在的 probe execution，从不准入第二个：未结算就按下面的计时与 drain 规则推进并 LeaveParked，已结算才读它的
    文件判决。claimant 对记录里每个未结算的 execution 调用幂等的 `drive`；提交后崩溃由 boot 的 `Engine::recover`（`server/execution_boot.rs:29`）
    重新排队。
  - kernel 崩溃或被 kill 时，probe 属于 supervisor，继续运行并受 Desired 驱动，boot 重新观察同一行；这取代了旧设计中“kernel 崩溃丢失
    进程内预约”的限制。同一 OS boot 内 supervisor 重启使未结算的 probe 成为 Unknown（`supervisor/execution/mod.rs:186-188`），
    `next_step` 对它只 Wait（`runtime/execution/mod.rs:106-127`），op 不完成、检出 busy，接受为限制（同 action 与 C2b）。
  - 不需要 claim 续租：一次 claim 内只做一次 durable 转换、一次完成，或一次有界的释放发送（不超过 `RELEASE_TIMEOUT`，与 action 释放同一
    约束），不在 claim 内等待 probe 进程。claim 过期只让当前 claimant 的下一次写失败并退出，接手者从记录继续：probe 不重启，计时不重置
    （以 durable 的 `authorized_at_ms` 为准），probe 自身的 60s 与 claim 租约（60s，`server/operation/mod.rs:77`，`claim_parked` 不续租）
    不再有关系。
  - sweep（以及 boot/recover）的一次 claim 只执行一次 durable 步骤并返回 LeaveParked，不等待 Stop、结算或 probe；
    **parked sweep 保证活性，observer 只是加速器。** 所有 recovery mode（PreDeadlineProbe、PastDeadline、Boot）在一步提交后，
    只要返回 LeaveParked，就从 adapter 的同一个调用点执行幂等 `ensure_observer`，不再按 mode 分散挂载；observer 丢失最多损失一个
    sweep 周期，不会丢失 operation。sweep 返回后的完成由 observer 异步推进。原生测试只将返回后的终态即时断言改为有界等待，phase、失败原因、事件和 payload 均保持不变；
    sweep 前仍须断言 parked 且无事件。未结算的 action 仍为 Observing 且 `now >= parked_deadline_ms` 时，observer 在取 claim 之前退出，
    将 claim 让给下一次 sweep；一次 sweep 同事务写 Stopping{Timeout} 与 Stop 后，post-step 重挂 observer，此时 Stopping 不再让出 claim，
    不需第二次 sweep 即可收尾、推进 probe 与完成。已结算的 action 没有剩余 action 计时，observer 可继续推进其 probe。
    每个进程每个 op 最多一个 observer：forge adapter 自有按 op id 索引的内存注册表，所有挂载点统一经过
    `ensure_observer(ctx, op_id)`；未注册时先注册再 spawn，已注册时只标记 restart 请求。observer 因 deadline yield、
    不再 Parked 或 operation 不存在而退出时，在同一把锁内检查并消费 restart：有请求则继续循环，否则注销，避免 sweep 与退出竞争丢失唤醒。
    observer 的 `get_operation`、`claim_parked` 临时仓库错误不退出，以 50ms 起、翻倍至最多 2s 的退避重试；完整读与 claim 成功后重置退避。
    RAII 在 panic/drop 时同样清理注册；boot 在新进程重新挂载，不把该表当 durable probe 排斥。
    deadline 测试只调用一次 `sweep_parked`，之后只作有界终态等待，不以重复 sweep 掩盖 claim 饥饿。
    回归另验证重复成功的 PastDeadline sweep 在 probe 未结算时只保留一个 observer，结算后仍完成；
    deadline yield 到注销之间暂停 observer，此时 sweep 已推进并请求 restart，恢复后无需第二次 sweep 即完成。
    这些 observer 回归只轮询 `OperationRepo::operation_result` 等待终态，不调用会继续推进 deadline 的 `OperationRuntime::wait`。
- **probe 的释放与计时（C3.2c）。** probe 只在 action 已结算、op 为 Parked、判决需要它时准入（见判决）。⑤ 与 action 释放三段相同（durable
  Held→Releasing 先于首字节，按 `accepted_bytes` 写凭据载荷与标记，`close_stdin` 确认后 Released）；与 action 一样只由 observer 在 parked claim
  下发送（park 时挂上的 observer，或 boot 在 owned 分支重挂的 observer，同 gate，`task_gate_run/mod.rs:396-401`），sweep 与 boot 本身不写字节。
  差别只有一处：probe 准入时 op 已 Parked，Held 只表示尚未授权，所以 boot 之后重挂的 observer 照常释放 Held 的 probe 一次，不写
  KernelRestarted。Releasing 是未确认释放，永不重发载荷：发送失败、超过 60s，或 boot 时看到 Releasing，都写 ⑥ Stopping{ReleaseRefused}
  加 Stop；稳态下其他 claimant 看到未结算的 Releasing 只按计时与 drain 规则推进。计时：`authorized_at_ms + PROBE_TIMEOUT`（60s，`mod.rs:44`），未结算、仍 Observing 且到期 → ⑥ Stopping{Timeout} 加
  `Desired::Stop`，由 executor TERM 后 KILL 并收尾全部子孙；`Exited` 只经共享 `drain_exited` 请求 Stop，`Drained`/`NoChild` 才是结算。
  准入或释放时 executor 不可用：不建行或不写字节，返回 Err，经 owned Parked 的错误留存写 `last_error`、以 lease 退避，可用后重试，不判
  infra、不回退原生。probe 不重试：结算后没有判决证据（Stopping{r}、NoChild、未 Released、缺 `.code`）的 probe 判 `gate-infra`，同今天
  probe 自身 Err 判 infra（`:1037-1044`）。这比今天窄：今天 kernel 重启后 probe 在新进程里从头再跑；迁移后已释放的 probe 在 supervisor
  内继续、无需重跑，但 Releasing 时崩溃或主机重启会以 infra 结束。重试需要 attempt 计数与它自己的排斥，没有消费者要求，不做。
- **Boot 矩阵**（`Engine::recover`（`execution_boot.rs:29`）之后逐 op；boot 只写 DB 与 drive，不等 executor）：
  - SpawnStarted 无记录：准入未提交，重驱按 ① 准入。
  - SpawnStarted 有记录（必为 Held）与 Parked 且 Held：typed park 同一 execution，不释放，Stopping{KernelRestarted} 加 Stop，结算后
    Failed `gate-infra`，不跑 probe（token 从未写出）。`tests/forge_action_adapter.rs:2279` 的“重驱后跑一次”保留名字，断言改为零次执行、
    infra；理由同 C2b 的 R8a：换绑需要 Retire、准入与换绑同事务，没有生产消费者。
  - Parked 且 Releasing：载荷可能部分送达，永不重放（凭据也不再取第二次）；wrapper 缺标记就不会执行 action；Stopping{ReleaseRefused} 加 Stop，结算后判 `gate-infra`，不读 action 文件、不准入 probe。
  - Parked 且 Released：不重发载荷，只重试幂等 CloseStdin，重挂 observer，不 Stop，保留释放后的 action 跨重启继续并交付结果的合同
    （`tests/forge_action_adapter.rs:2881`、`:2967`）。
  - 已记 Stopping{r}：同一 execution，永不重启，结算后按 r。已结算：按判决；OS 重启的 NoChild 同此（已释放的 action 可能已落地）。
  - 记录里有 probe（只可能在 action 已结算之后）：Held → 由重挂的 observer 照常释放；Releasing → ⑥ Stopping{ReleaseRefused} 加 Stop，结算后 infra；
    Released → 不重发载荷，只重试幂等 CloseStdin，按 `authorized_at_ms` 的剩余时间继续计时；已记 Stopping 或已结算 → 按判决。
    不准入第二个同种 probe。
  - Unknown（同 OS boot 内 supervisor 重启，action 或 probe）：不 close、不完成，op 与检出 busy，限制同 C2b。executor 不可用：保留 Desired，op parked，新 action 被拒。
  - pre-C3 原生 op（有 `spawn_artifacts_json`、无记录）：不读 PID、不发信号、不重启、不完成。`recover_owned_parked` 报“在旧 kernel 下
    排空或手工停止 pgid N”，经 owned 错误留存写 `last_error`，op 保持 parked、phase 栅栏保持 busy；补偿步骤 `kill_forge_action_group`
    改为拒绝 → Stuck（同 `task_gate_run/mod.rs:499-511`）。今天的 forge 占用检查只认 `RECOVERABLE_OPERATION_PHASES`（不含 Stuck，
    `workspace_lease/mod.rs:63-71`），所以 C3.2c 在同一检查里加入“有 `spawn_artifacts_json`、无 `forge_execution` 记录”的未决原生行，
    含 Stuck，track/area 删除照旧被拒，C3.2a 起的清理器也读到它；这些行没有 execution，C5 必须保留该 forge-owned 检查。升级前须排空
    forge action，升级说明照写；已结算的历史行照常可读。
- **清理栅栏（C3.2a；原生 Stuck 行随 C3.2c）。** 已关闭 worktree 清理器的 fence（`workspace_service/ports.rs:109-135`）今天只读 closed-idle 行、`checkout_occupancy`（无 forge 项，
  `truth/db/sqlite/track_occupancy.rs:47-94`）与 `unsettled_executions`，随后 `clean_ignored`（`runtime/worktree_clean.rs:86-104`）；原生 forge op
  没有 execution，所以活跃 forge op 挡不住 `git clean`。这是 main 上已有的缺口（自关闭不经
  `mcp_server/tools/track_state.rs:386-395` 的 forge 预检）。C3.2a 让该 fence 在既有锁下同时调用 forge-owned 的
  `track_has_active_forge_action`（`workspace_lease/mod.rs:73-110`，C3.2c 按上项扩展），不在 `checkout_occupancy` 或 runtime 复制其 SQL；已结算的历史终态行照旧不挡。
  检查之后 fence 放下 drive、只持 track 跨 `git clean`（`ports.rs:112-135`），而 forge 不覆盖 `preparation_track`（默认 None，
  `operation/mod.rs:767-772`），检查后提交的 forge op 会在 clean 期间准入、释放。所以 C3.2a 让 `ForgeActionAdapter::preparation_track`
  返回 payload 的 `track_id`（即冻结后的 `FrozenForge.track_id`），复用 C1 的通用 Busy 重试（`driver.rs:658-670`、`retry_after_track`）：
  Busy 则留 Pending、track 释放后重驱；track 排斥持到 TxCommitted 提交，此后该行在 forge 查询的 `RECOVERABLE_OPERATION_PHASES` 内对
  clean 可见。锁序不变：drive → try-track，持 drive 时不 await track，clean I/O 期间只持 track；通用代码无 forge 特例。C3.2c 之后
  action 与 probe 的 execution 另外经 `unsettled_executions` 挡住 clean。
- **leader 退出与 Drained；timeout。** observer 经 `Engine::output` 看到 `Exited` 时只调用共享 `drain_exited` 请求 Stop 收尾子孙
  （如 checks 等待脚本，`crates/plugin/src/builtin/gitforge/git_actions/checks.rs:196-230`），只有 `Drained`/`NoChild` 结算才允许判决；
  probe 同此。action 的 timeout 只有一个计时器：`parked_deadline_ms`（提交时刻加 300/900s，`transport.rs:872`）。只有 parked sweep/driver 的 `recover_parked(PastDeadline)`
  发现已过期、未结算且仍 Observing 才写 Stopping{Timeout} 加 Stop；observer 不写 action Timeout，但在到期且 action 未结算、仍 Observing 时退出取 claim 的循环，
  给 sweep 让路；sweep 完成一步后重挂 observer，Stopping 或 action 已结算时继续异步推进，不需后续 sweep。其他 recovery mode 不应用 action deadline 判决。probe 的计时见上，独立于 action deadline，保持不变。
- **结算后判决（只读 claim 快照里的记录与持久证据）。** action 或记录中任一 probe 未结算 → 一律 LeaveParked（必要时 `drain_exited`、
  写 Timeout）。action 已结算：Held 或 Stopping{KernelRestarted | ReleaseRefused} → `gate-infra`；结果文件可读（`read_result_file`，tmp+rename 后才出现）→
  今天的 live-result 规则（`:875-931`，非零或提取失败仍交 probe）；Stopping{Timeout} 且无文件 → 冻结 action 声明了 probe 就进入 probe 流程（先 verdict、再按需 output），
  按 probe 结果完成；未声明 probe 才判 `action-timeout`。这与今天 driver kill 后经 `recover_parked(alive=false, PastDeadline)`
  进入 `resolve_post_release_via_probe` 的行为一致，保留 deadline snapshot 合同；其余无文件（signal、NoChild）→ 有 probe 进入 probe 流程，无 probe
  判 `gate-infra`，PastDeadline 时 `action-timeout`（`:1573`）。仅未确认的释放发送失败（Releasing）写 ReleaseRefused，永不重发，结算后判 `gate-infra`。
  action 或 probe 一旦 durable Released，观察与幂等 CloseStdin 的 transport 错误不改变 decision 或 Desired，op 保持 parked，后续 step 重试；不能把已确认释放改判 ReleaseRefused。probe 流程（C3.2c 起，所有调用方——observer、pre/past-deadline sweep、boot、
  dead-parked——都经同一个 forge-owned 的步进函数，每次 claim 推进一步）：verdict probe 未准入 → ④；已结算且 Observing、Released、`Drained` 三条件同时成立 →
  读其 `.code` 经 `verdict_from_exit_code`（`:991-997`）：0 → Landed，1 → `action-not-landed`（原因文本同今天，`:1120-1123`），其他或缺失 →
  `gate-infra`（`:1077-1080`）；其余结算 → `gate-infra`。Landed 且 event 需要从 JSON 重新提取字段（`:1047`）：无 output probe argv →
  `gate-infra`（`:1048-1054`）；否则 ④ 准入 output probe 并同事务写 Landed；output probe 同样必须 Observing、Released、`Drained` 且 `.code` 为 0 → 读 `.stdout` 交今天的
  完成规则（artifact 与 event 构造，C3.2b 的判决函数），否则 `gate-infra`（`:1056-1068`）。Landed 且不需要 JSON → 以空 stdout 完成
  （`:1070-1073`）。判决只来自已结算 probe 的文件；`verdict: Landed` 写入后，之后的 claimant 不再读 verdict probe 的文件。PastDeadline
  不跳过已准入或需要的 probe（今天 dead 时不论 deadline 都跑 probe，`:1569`）。probe 按 plugin 合同是读，且每种至多一个 execution，不再重复读。
- **完成与 post-commit drive。** forge 的 decision event 今天在 `complete_forge_op_succeeded` 的同一事务内 append（`:942-972`）。改为
  经 C3.0 的 `ParkedRecovery::Complete` 携带 owner 已算好 actor、scope 与 event 的 `Option<OwnerEvent>`；
  `complete_owned_parked_tx` 在 `owned_parked::reconcile` 的完成事务内按键复核整个记录（与快照不同 → Conflict、回滚、错误留存），
  并对 action 与记录里的每个 probe execution 请求 `Desired::Retire`；`reconcile` 同事务用 `append_decision_event_in_tx`（correlation None）append event，提交后发布（`owned_parked.rs:31-40`），crash seam `forge-pre-fence-commit`
  移到该事务提交之前。artifact 发布在事务前、幂等。每条终结路径（infra、not-landed、unknown、timeout、无 probe、AlreadyResolved）
  都经同一 reconcile：提交后 `owned_parked_completed` drive 这些 execution（同 `task_gate_run/mod.rs:462-466`），observer 每轮看到终态
  phase 也 drive；Retire 是同事务的 durable Desired，提交后崩溃由 boot 的 `Engine::recover` 重新排队。
- **测试（C3.2c；C3.0、C3.2a、C3.2b 的测试见切片表）。** 新测试，target forge_action_adapter：`c3_action_has_one_engine_construction`（action 与两种
  probe 都出自同一构造点）、`c3_action_refuses_unavailable_executor`、`c3_action_releases_only_after_park`、
  `c3_credentials_never_enter_intent`（action 与两种 probe 的 `intent_json` 与 journal 都无凭据键）、`c3_phase_writes_preserve_forge_record`、
  `c3_redrive_of_held_ends_infra_without_second_execution`、`c3_boot_held_stops_without_token`、`c3_boot_releasing_never_repeats_token`、
  `c3_boot_released_retries_only_close_then_reattaches`、`c3_leader_exit_drains_descendants_before_verdict`、
  `c3_timeout_stops_then_completes_after_settlement`（经 sweep 触发）、`c3_observer_alone_never_times_out_expired_action`、`c3_unknown_execution_never_completes_op`（action 与 probe 两个变体）、
  `c3_completion_rechecks_record_and_retires_in_same_tx`（action 与已准入的 probe 都 Retire）、`c3_every_terminal_outcome_drives_retire`、
  `c3_park_write_failure_retains_owned_execution`、`c3_pre_c3_native_forge_op_is_never_advanced`（含补偿 → Stuck 后 track 与 area 删除仍被拒）；
  probe：`c3_probe_admission_is_durably_exclusive`（A 取 claim 后在 ④ 之前确定性暂停，boot 替换与过期 sweep 两个变体：A 的 ④ 回滚、零
  fork，每种 probe 恰一个 execution 行；直接再 admit 同 alias 被唯一索引拒绝）、`c3_boot_steal_never_double_spawns_probe`（target forge_suite，用 C3.0 的 support：probe 运行中
  kill kernel、supervisor 存活：重启后 boot 观察同一 execution_id，不准入第二个，按它的文件判决；Held 的 probe 重启后恰释放一次）、
  `c3_probe_settlement_precedes_verdict`（probe leader 写好 `.code` 后退出、后台子孙仍在 worktree：Drained 之前不判决、不完成，`Exited` 只
  触发 Stop drain）、`c3_probe_timeout_stops_through_engine`（probe 与子孙超过 60s：写 Stopping{Timeout} 与 `Desired::Stop`，executor 收尾
  全部子孙后才判 `gate-infra`；等待期间 claim 过期不重置计时、不重启 probe）、`c3_no_completion_while_probe_unsettled`（probe 未结算时
  past-deadline sweep、observer 与 boot 都不终结；完成快照之后记录多了 probe → 完成事务 Conflict、回滚）、
  `c3_landed_verdict_persists_before_output_probe`（Landed 与 output probe 同事务；output probe 期间重启不重跑 verdict probe）、
  `c3_boot_releasing_probe_is_refused_not_resent`、`c3_probe_waits_for_available_executor`（不建行、不判 infra，`last_error` 与 lease 退避，
  可用后恰准入一次）；track_suite `track_delete_forge_fence::c3_closed_worktree_clean_refuses_unresolved_native_stuck_forge_row`。replay：
  forge_action_adapter 全部（只验已删机制的用例，如 `forge_action_recovery_rejects_wrong_start_time`，随机制删除，清单写进 PR；原生 probe
  结果用例如 `forge_action_probe_not_landed_fails_dead_parked_without_rerun`、`forge_action_probe_unknown_fails_dead_parked_without_rerun`、
  `forge_action_crash_then_probe_reextracts_probe_typed_output_fields`、`forge_action_dead_past_deadline_uses_probe_and_no_probe_times_out`
  保留名字与判决断言，证据改为 probe execution 与其文件）；forge_suite `forge_merge_crash_reboot::` 两条（EOF 与 PID 预言改为 typed 记录与
  结算，保留“零或一次合并”断言）；forge_template_e2e `issue_comment_recovery::`；track_suite `track_delete_forge_fence::` 与
  `track_worktree::c1_fence::`；mcp_plugin_forge_action 全部；lib `operation::forge_action_adapter::tests`（含
  `landed_probe_completion_tx_error_leaves_operation_parked`）、`repo_sqlite::claim_tests`、`owned_parked::tests`、`execution_boot::port_contract`；
  C3.0 的 `c2b_` 过滤器。mutation 只在实现 PR 内各做一次，每次先写出预测变红的测试名：park 前释放、boot Held 不 Stop、凭据进 intent
  （action 或 probe）、wrapper 不校验终止标记（C3.2b）、占用检查漏掉原生 Stuck、清理器不读 forge 检查与 forge 不声明 `preparation_track`
  （C3.2a）、probe 准入不在 live-claim 事务内或不要求 `probes.<kind>` 为 null（`c3_probe_admission_is_durably_exclusive`、
  `c3_boot_steal_never_double_spawns_probe`）、`Exited` 即读 probe 文件（`c3_probe_settlement_precedes_verdict`）、probe 到期不写 Stop
  （`c3_probe_timeout_stops_through_engine`）、存在未结算 probe 时仍允许完成（`c3_no_completion_while_probe_unsettled`）、output probe 准入
  不同事务写 Landed（`c3_landed_verdict_persists_before_output_probe`）、完成事务不复核记录、省略 Retire drive。
  永久成本：`OWNER_STATE_KEYS` 多一个键，`execution_boot.rs` 多一条 consumer 注册；每个需要 probe 的 op 至多多两行 execution，完成时 Retire；
  `c3_` 测试随所在 suite 每个 CI workflow 各跑一次；不加调用者或测试清单。
- **切片与规模。** 校准：C2b 四片的设计值与实际手写增删（`git show --numstat`，不计生成产物与 Markdown）分别为 C2b.0 510 → 1092、
  C2b.1 1150 → 2572、C2b.2 930 → 2397、C2b.3 470 → 821，即 1.7–2.6×，切片表的“预计”按此区间给出。C3.2c 的设计值约 2520、预计
  4300–6500，远超约 800 的口径；能保持行为或独立成立的部分已经前移（C3.0 的 helper、`Complete` 携带 event 与 support，C3.2a 的清理栅栏，
  C3.2b 的凭据载荷与判决函数）。剩下的不能再切：① 先迁 action、probe 暂留原生：owned reconcile 下 probe 在 claim 内运行，单次 probe
  可达 60s 而 claim 租约也是 60s、不续租，完成总因 claim 过期失败、下一个 claimant 重跑 probe，要么活锁，要么重新引入已删的续租预算；
  ② 先迁 action、ambiguous 结果暂判 infra：在两次合入之间把今天由 probe 判定的 op 退化为 `gate-infra`，是可部署的回归；③ 先迁 probe：
  probe 记录要求 forge 声明 owner-state 键，键一经声明，driver 就把全部 forge parked op 交给 owned 路径（`driver.rs:1192`），原生 action 的
  PID 恢复要在 claim 下重写，即旧版 C3 设计明确不做的原生 claim 管线；④ 原生代码约 −590 行在切换后没有调用者，clippy 不允许留到下一片。
  PR 按上列文件组说明。

**C4.3 落地决定（#2666，A–D）。** ACP Planner turn 注册为 `acp-planner` owner/projection，经同一 provider admission fence、Held 凭据释放与 `EngineStdio`，运营者环境值只进 stdin 载荷；原生 readiness/catalog 留到 C4.4。A：保留已知结果的 45s receipt-only 持久化预算，driver 先持久化再调用 owner；owner 完成端口幂等、单次 10s，失败保留 claim 供重试。B：沿用已合入 C4.2 的 `KnownEnd`；丢失完成时记录 interrupted/unconfirmed，复用 durable outcome，只有活动 slot 保留现场 session 事实。C：保留 #3044 receipt fence；丢失 driver 只把 receipt 结为 unknown，不制造正常 join 的 cleanup 证据，session 继续 held，绝不重发。D：修正 executor scope helper 的 Pipe stdin 所有权：target spawn 完成后用 `fstat(0)` 识别 pipe/FIFO，以 `/dev/null` `dup2` 替换 helper 的 fd 0；target 关闭 stdin 后写入得到 EPIPE，进入既有 StdinTorn/Transport 路径，不新增 ACP response timeout。PTY、非 pipe stdin 与 stdout/stderr 不变；supervisor 回归用仍存活且已关闭 fd 0 的 target 验证拒写。

**C4 设计决定。** 核实基线 main `c70201bcb`（C3.2c #2983 已合入）；路径前缀同 C2b，`tests/` 指 `crates/calm-server/tests/`。只是设计，不声称已实现。
结论先行：(1) **row C4 的前提要改**：Claude/ACP Planner 今天是“每个 turn 一个原生子进程”（`server/claude_planner/session.rs:1-2` 的模块说明，
ACP `server/acp_planner/session/driver.rs:73` turn 末停进程），不是长寿会话；task worker 都**不是** kernel 原生子进程，而是经 Legacy
`EnsureProc` 由 calm-proc-supervisor 托管的 PTY（`server/terminal_renderer/mod.rs:819-831`）；isolated Codex worker 已由 #1908 删除，
`calm-worker-runtime` 的 bwrap launcher 未接入任何生产启动，现已退役。(2) C4.0a–C4.0d 与 C4.1–C4.4 不需要改 Engine 合同；三类入口需要改合同才能成为消费者，
本设计**不迁移**它们并给出更窄保证：共享 Codex app-server（需要无 track 的宿主级 execution 与原地重启）、Claude/ACP readiness 类
一次性命令（无 track）、以及同宿主 supervisor 崩溃后的 Unknown 排空（需要 executor 同宿主正向排空，workspace-execution.md §6.4、§11 E3）。
(3) 两项待用户决定（见“开放问题”Q1、Q2，两支都已定规模）：executor 不可用时（macOS 有限构建、升级前 supervisor）Planner turn
被拒；supervisor 崩溃后该 card 被拒到宿主重启。Q3–Q8 已由 Planner 决定并写入下文（有界 stderr、接受 200ms STOP_GRACE、ACP catalog
留在项目 cwd、owner 自建 Legacy PTY 与共享 Codex daemon 不在 C4、认证代理值不进 intent），只陈述它们对 C4 各片的效果。(4) 第一轮两路 L2 设计评审的阻断项
（sweep 不保证 Engine 进度、丢失驱动者后 owner 未完成、C4.1 单独部署不安全、worker 释放缺 durable 授权围栏、正常收尾不停住 leader、
PTY 载荷非字节保真、环境键插入 shell 源码、Claude worker 退出到 task 的收敛依赖 projection）的处置见 §7 中“C4 设计评审第一轮”各行。
(5) 第二轮的阻断项（C4.5 独立部署缺周期恢复、task 义务扫描绕过退出政策与 resumable 死亡确认、只读结算的 `TerminalObservation`）的处置见
§7 中“C4 设计评审第二轮”各行：周期恢复改为最早的共享片 C4.0d，worker 退出改为两条发现路径共用的一棵决策树，观察端口改取 executor 的权威事实并驱动 leader 退出排空。
(6) **范围收窄（第三轮之后，Planner 决定）。** 第三轮两路评审确认周期恢复与 Planner 各片成立，阻断项全部落在 worker 迁移（退出取代未进入
task 与 lease 的同一事务、worker 输出取尾缺 durable 恢复、Legacy `claude-restart` 无法重启 typed worker 终端、未绑定 session 丢失 lease 释放），
且第二轮以来的阻断也都在 worker 一侧。按 AGENTS.md 三轮仍阻断即收窄的规则，C4 只保留 C4.0a–C4.0d 与 C4.1–C4.4，C4.0d 是第一片且独立可部署；
worker 迁移（原 C4.5a–C4.7）移出 C4，成为 C5 之后的独立里程碑 W（§3.3），第三轮的 worker 设计作为起点、全部开放发现作为需求随之移入。
C4 各片不依赖 W，也不为 worker 预置只有 worker 需要的东西；在 W 之前 task worker 仍是 Legacy PTY，C5 保留它们的 Legacy 检查。下文的 Planner 机制与
第三轮评审时相同，只改了第三轮指出的 stderr 溢出判定与恢复节拍测试的 mutation 预测红集。

- **盘点（源码，main `c70201bcb`）。** 仍是原生子进程或 Legacy supervisor 进程、尚未成为 Engine execution 的每个启动或恢复入口：

  | # | 入口与调用路径 | owner 层 / 协议 owner | 生命周期 | stdin/stdout/stderr | 凭据与环境 | 崩溃/重启 | 钉住的既有测试 | C4 处置 |
  |---|---|---|---|---|---|---|---|---|
  | N1 | Claude Planner turn 与 compaction：`server/claude_planner/session.rs:491`（`start_spawn` `:295`）。turn：`harness/run_loop/issuance.rs:494` → `harness/backend/managed.rs:149` → `claude_planner/session/invocation.rs:11`（`start_spawn` `:26`）；canonical：`harness/backend/dispatch.rs:80` → `invocation.rs:38`（`:48`）；compaction：`managed.rs:203` → `claude_planner/compact.rs:61`（历史，`:67`）、`:32`（canonical，`:46`）；rewind 的 cut 随下一次 spawn 走 argv（`spawn.rs:151-156`） | 进程归 calm-server；stream-json 编解码归 provider（`provider/src/claude/protocol.rs:312`、`line_writer.rs:23`），读行循环在 calm-server（`claude_planner/driver.rs:206`、`:333`） | 每 turn 一个 `claude -p`，每个 worker session 至多一个（`session.rs:309`、`:322`、`:335`），无总时限；收尾 `driver.rs:643-694`：关 stdin → 等 5s → marker `stop_by` → `start_kill` + reap → `TurnCompleted` | 三路 pipe，`kill_on_drop`，无 pgid/setsid；stdin 先写一行 user line（`session.rs:543`，5s），之后写 control response（`driver.rs:564-581`）与 interrupt（`session.rs:741-753`）；stdout 是协议；stderr 取尾行（`driver.rs:552-560`） | `env_clear` + `base_env`（`spawn.rs:295-317`；白名单 `provider/src/codex/shared/environment.rs:8-43` 去掉 `OPENAI_`/`CODEX_`/`RUST_`，`spawn.rs:260-267`）、`CLAUDE_CONFIG_DIR`、marker `NEIGE_CLAUDE_PLANNER`（`stop.rs:11`）；**秘密** `NEIGE_MCP_TOKEN` 只在 env（`session.rs:495`）；instructions 文件 0600（`spawn.rs:319-390`）；cwd = `agent_cwd()`（`claude_planner/wiring.rs:56`） | spawn 前 marker stop（`session.rs:399`）；boot 先 revoke 再 sweep（`claude_planner/lifecycle.rs:27`，`state.rs:1389`，在 MCP listener 之前），写 interrupted（`claude_planner/recovery.rs:12-47`），不重启进程；续接靠 `--resume <thread>`（`worker_sessions.agent_session_id`，`driver.rs:494-532`）；破坏性步骤前 `sweep_track`（`lifecycle.rs:60`；`routes/tracks.rs:2095`、`:2656`，`routes/areas.rs:473`）；关闭 `claude_planner/session/execution_end.rs:185-220`；PID 不入库 | planner_harness_suite `claude_planner_session::*`、`claude_planner_session_lifecycle::*`、`claude_planner_credentials::*`、`claude_planner_retirement::*`、`claude_planner_compact::*` 等（清单见受影响测试）；lib `claude_planner::{spawn_tests,stop_tests,driver_tests}`、`harness::backend::claude_dispatch_tests`、`planner_harness_start_adapter::canonical_lifecycle_tests::claude_recovery` | C4.2 |
  | N2 | Claude rewind 预检：`server/claude_planner/rewind.rs:119`（`run` `:111`，`check` `:86-109`）；`routes/planner_input_send.rs:74` → `harness/run_loop/replace_command.rs:106` → `managed.rs:52` → `claude_planner/session/invocation.rs:63-80`（持 session `issue` 锁，turn 活动或关闭中时拒绝） | calm-server | 一次性，`CHECK_TIMEOUT` 30s（`:21`、`:135`） | stdin null；stdout 与 stderr 都读，判词取 result `errors`、否则 stderr 尾行（`:160-187`）；只 `kill_on_drop` 直接子进程，超时后无显式 reap | `session_env`（无 MCP token），cwd = `agent_cwd()` | 无恢复；崩溃时子进程无人回收（marker 会被下一次 stop 扫到） | lib `claude_planner::rewind::mapping_tests::m1_mapping_edit_dry_run_accept_refuse_and_failure_table`（`rewind.rs:190-247`）、`claude_planner::rewind_tests::the_dry_run_and_the_next_spawn_carry_the_cut`；planner_harness_suite `claude_planner_replace::*` | C4.1 |
  | N3 | Claude readiness：`server/claude_planner/readiness_command.rs:79`（`run_inner` `:76`）。`--version`（`config.rs:53-92`，10s；每次 turn 前 `session.rs:373`；`check_ready` `config.rs:180-183`）、`auth status --json`（`auth_status.rs:17-30`）、catalog `initialize`（`catalog_fetch.rs:45`，8s，cwd 为 host 的 instructions 目录 `availability.rs:362`） | calm-server；解码归 provider（`provider/src/claude/discovery.rs:9`、`:82`、`:128`） | 一次性、有界 | stdin null 或写一次后关闭；stdout 上限 64KiB；stderr null；`process_group(0)`、`kill_on_drop`，成功、超时与取消均回收进程组（SIGKILL） | `readiness_env`（无 marker、无 token）；无 track cwd | 无 | lib `claude_planner::{auth_status_tests,catalog_fetch_tests}`；planner_harness_suite `claude_planner_availability::*`、`claude_planner_catalog::*`、`claude_planner_models::*` | 不迁移（见“不能成为消费者”） |
  | N4 | ACP Planner turn：`server/acp_planner/process.rs:55`（`spawn_control` `:42`）；`managed.rs:91-97` → `acp_planner/session.rs:111` → `issue` `:125` → `:270` | 进程归 calm-server 与 `plugin::child_process`（`GroupChild`/`spawn_within`，`crates/plugin/src/child_process.rs:65-149`）；JSON-RPC 帧归 provider（`provider/src/acp/transport.rs:160`，NDJSON，帧上限 8MiB `:9`） | 每 turn 一个进程（setup → `session/load` 或 `session/new` → prompt，最长 3600s，`session/driver.rs:17`），turn 末 `process.stop` | 三路 pipe；stdin/stdout 双向 JSON-RPC，含 agent 发起的权限请求（`provider/src/acp/approvals.rs:212`）；stderr 前 256KiB 写 `<data_dir>/acp-agent-logs/<worker>.stderr.log`（`process.rs:127-186`）；`setsid`（`:82`），`kill_on_drop` 加组 SIGKILL（`child_process.rs:118-124`） | `env_clear` + 运营者注册的 `config.env`（**可能含凭据**，校验 `acp_planner/config.rs:145-164`）+ PATH + marker `NEIGE_ACP_PLANNER` + `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN`（`process.rs:73-78`）+ profile launch env；token 也在 stdin 上的 `session/new`/`session/load` 载荷里（`session.rs:289-301`）；cwd = `agent_cwd()`（`acp_planner/wiring.rs:38`） | spawn 前 marker stop（`process.rs:50`）；boot revoke + sweep（`acp_planner/config.rs:312`，`state.rs:1406`）；receipt 恢复（`acp_planner/recovery.rs:82`、`:98`）；不重启进程，续接靠 `session/load`；停止 `process.rs:116-122` + marker；`sweep_track` `config.rs:331` | runtime_suite `acp_planner_provider::*`（假 agent `tests/fixtures/acp_planner/agent.py`）；lib `acp_planner::process::{setup_tests,pump_tests}`、`acp_planner::boot_tests::*`、`acp_planner::session::control_tests` | C4.3 |
  | N5 | ACP readiness 与 catalog：`acp_planner/config.rs:295-306`（cwd `std::env::temp_dir()`）、`acp_planner/catalog.rs:66`（cwd 为 card workspace 或 scratch，`:61-64`；30s 缓存） | 同 N4 | 一次性；`spawn_within` 10s；catalog 整体 30s | 原生 `GroupChild`：取消/超时/drop 组 SIGKILL | `config.env` 凭据，无 MCP token | 无 marker | runtime_suite `acp_planner_provider::acp_planner_catalog::*`（含 `acp_catalog_cancellation_*kills_process_group*`）；lib `acp_planner::boot_tests::*` | 不迁移；C4.4 删除运行期 marker 并约束 catalog 总时限 |
  | N6 | 共享 Codex app-server：`server/shared_codex_appserver.rs:2358`（`launch_spawned_process` `:2328`，`spawn_process_transition` `:2285`；boot `:839`/`:844`；`restart_after_crash` `:2783`；heal `:2917-2984`；takeover `:1718`） | calm-server；WebSocket JSON-RPC 经 unix socket，归 provider（`provider/src/codex/mod.rs:289`） | 每个 data_dir 一个，服务全部 Codex Planner、Assistant、worker 线程；刻意活过 kernel（`:4420-4421`） | stdin null；stdout/stderr 写日志文件（`:2351-2367`）；`process_group(0)`；刻意不 `kill_on_drop`（`:2359-2361`） | `env_clear` + `SPAWN_ENV_PASSTHROUGH`（含 `OPENAI_API_KEY` 等）+ `CODEX_HOME`；每 card token 走线程参数（`mcp_server/wiring.rs:60-100`） | `shared_codex_daemon` 行持久化 pid/pgid/start_time/boot_id（0026）；崩溃重启带退避与 generation 围栏；boot takeover 或回收后重启 | target `shared_codex_appserver`（79）、`no_double_spawn`（runtime_suite）、`codex_worker_shared_daemon`、`codex_user_prompt_shared_daemon`、`worker_exec_shell_env` | 不迁移（见“不能成为消费者”） |
  | W1 | terminal-worker：`server/operation/terminal_adapter.rs:754` → `:824` `spawn_task_terminal` → `routes/terminal.rs:73-88` → `terminal_renderer/mod.rs:434`、`:453`、`:757` → `EnsureProc`（`:819-831`；supervisor `calm-proc-supervisor/src/lib.rs:675-700`、`:1432-1560`）；调度 `scheduler/mod.rs:856`、`:1084`、`:1139` | 启动归 calm-server operation 适配器；进程归 Legacy supervisor（不是 Engine）；存活/退出解释归 provider（`provider/src/worker/terminal.rs`） | 通常一次性；退出经 `TerminalTaskHook::on_terminal_exit`（`scheduler/mod.rs:2066`）→ `complete_terminal_task`（`:2143`） | PTY（`/bin/sh -c`，`routes/terminal.rs:139-151`），portable-pty 的 setsid | `terminal_worker_env`（`terminal_adapter.rs:968-980`，只有代理）+ supervisor 的环境白名单（`calm-proc-supervisor/src/child_environment.rs:5-49`）；cwd 为 payload cwd 或 track workspace；无 lease | `terminals.pid`、`tx_output.terminal_launch`（`terminal_launch.rs:10-32`、`:102-179`）；活过 kernel 重启、不活过 supervisor 重启（`calm-proc-supervisor/src/lib.rs:460-475`）；boot `Probe`（`server/lib.rs:43-118`）；重驱只 attach（`terminal_launch.rs:147-149`）；停止 `terminal_renderer/mod.rs:691-740`，裸 PID 兜底 `terminal_sweeper.rs:489-544`、`:670-750` | runtime_suite `scheduler::*`、`kernel_process_suite::reconcile_supervisor_on_boot::*`、`no_double_spawn::*`；api_suite `terminal_ws_suite::*`；lib `terminal_adapter::tests::launch_cleanup_tests::*` | W 的 WT：op 自有的 Pipe execution，复用 gate 机制（不在 C4；重写设计见 [task-worker-execution.md](task-worker-execution.md) §5.5、§7） |
  | W2 | claude-worker：`operation/claude_adapter/mod.rs:867` → `:1045`，同 W1 管线；程序 `exec claude … --session-id … --mcp-config`（`:382-407`） | 同 W1；协议走 hook 与 transcript tail（`worker_flow/`），不解析 stdout | 长寿交互 PTY | PTY；消息经 bracketed paste（`provider/src/worker/claude.rs:38-43`） | `build_claude_env`（`:350-380`）+ **秘密** MCP token 只在 `EnsureProc` 的 env（`claude_adapter/worker_mcp.rs:39` 的 `wire`）；Claude 登录靠 supervisor 白名单里的 HOME；cwd 为 worker lease 的 worktree | 重驱幂等（`:889-977`）、快速退出保留（`worker_cleanup.rs:13-24`）、补偿（`:1134-1223`）；`claude_auto_resume` 排除 task worker（`claude_auto_resume.rs:104-119`） | lib `claude_adapter::tests::*`（含 `worker_mcp_tests`）；runtime_suite `scheduler::live_dispatch_claude_does_not_reconcile_recorded_pty_exit`、`worker_flow_claude_suite`；mcp_integration_suite `track_worker_cwd::*`、`track_worker_cwd_ends::*` | W 的 WA1–WA3：无界面、与 Claude Planner 共用 stream-json turn 驱动与 `provider_execution` owner 层（不在 C4；[task-worker-execution.md](task-worker-execution.md) §3、§7） |
  | W3 | codex-worker：`operation/codex_adapter/mod.rs:860`；业务是共享 daemon 中的线程（`:1142-1315`，即 N6）；PTY 只是可选 viewer `codex resume <thread> --remote`（`:1283-1297`，`OptionalViewer`） | 同 W1 | viewer 失败被吞为 NoOp（`:1310-1313`） | PTY | viewer env 带 MCP token（`:1276-1280`） | 同 W1 | target `codex_worker_shared_daemon`；lib `codex_adapter::tests::viewer_cleanup_tests::*` | 业务线程不变；viewer 按 [task-worker-execution.md](task-worker-execution.md) OQ6 迁成 typed PTY（WV）或留到 Legacy wind-down（不在 C4） |

  不在 row C4 内、但走同一 Legacy PTY 管线的 owner 自建 card：`claude-create`（`claude_adapter/mod.rs:584`）、`codex-create`（`codex_adapter/mod.rs:547`）、
  Legacy 模式的 `terminal-create`（`terminal_adapter.rs:378`；typed 替代已由 E2b opt-in 交付）、`claude-restart`（`claude_restart_adapter.rs:389` → `:461`/`:465`）与 boot 的
  `claude_auto_resume`（`application_boot.rs:70`）。已不存在、不设切片：isolated Codex worker（#1908 删除，只剩 `truth/migrations/0174_parked_artifacts_in_rust.sql:60-71`
  的 CHECK 支）；`calm-worker-runtime` 的 bwrap `Runtime` 未接生产，现已退役；`proc_entry_vanished` 已归属 calm-server 的 `proc_identity.rs`，进程 fixture 仅供测试。若将来重新设计此边界，provider 的父进程必须是
  bwrap 内的 init，不能是 supervisor 的直接子进程。W1–W3 的行留作盘点证据，但它们已不在 C4：迁移移到 W（§3.3，重写设计为 [task-worker-execution.md](task-worker-execution.md)），在 W 之前照今天的 Legacy 管线运行。

- **不能成为消费者的入口与更窄保证。** Engine 每个 execution 必有 `track: TrackId`、Private root 与 operation 形状的 owner
  （`runtime/execution/mod.rs:46-56`），占用谓词按 track 聚合（workspace-execution.md §7.2）。
  - N6 共享 Codex app-server：不属于任何 track，跨 track 服务多个 card，要求原地重启（同一 socket 与 `CODEX_HOME`）与跨 kernel takeover。
    给它伪造 track 会让 `unsettled_executions(track)` 失真，属于硬编码假设；正确做法是宿主级 execution，这是 Engine 合同变更，按任务要求
    不在 C4 设计。保留 `SharedCodexAppServer` 原生生命周期与身份校验。更窄保证：Codex Planner/Assistant/worker 的 agent 工具进程是 daemon
    的子孙、在 track cwd 内运行，**`unsettled_executions` 为空不证明 Codex 已静默**；C5 必须保留 Codex 的线程级检查（活动 turn 与 interrupt），
    不得据执行集合为空删除它们。
  - N3、N5 readiness 类：宿主级、无 track，Claude 的三条在 kernel cwd 或 host 的 instructions 目录运行，ACP readiness 在 temp 目录，都不在
    Neige 自有位置，不是 C5 占用。保持原生、有界。C4.4 给 Claude `readiness_command.rs` 加 `process_group(0)` 与超时/丢弃时的组 SIGKILL，
    使它满足 C2a 短读例外的前提（deadline + process-group reap）；ACP 已有 `GroupChild` 组回收。两者的 control probes 不再携带 marker 或调用 marker stop。ACP catalog 以 card workspace 为 cwd 时属于 C2a
    的有界短读例外，跨崩溃残留同 #2862，不在 C5 谓词内。Q5 已决定：保持项目 cwd，以保留 agent 读到的项目配置；今天只有逐步时限
    （spawn 10s、`setup_request` 10s，`acp_planner/process.rs:85`、`:253`；停止 3s，`:235`），C4.4 给整个 discovery 加一个 30s 总时限，到期或取消即丢弃
    `GroupChild` 触发组 SIGKILL（`plugin/src/child_process.rs:118-124`），并以测试钉住；改用 scratch 目录是另一项用户可见行为变化，不在 C4。
  - 同宿主 supervisor 崩溃后的 Unknown：见开放问题 Q2。优雅重启（SIGTERM）走 executor 的 stop 例程并持久化 Drained（workspace-execution.md
    §6.4），只有崩溃或 SIGKILL 才留下 Unknown。

- **执行形状（Planner，C4.1–C4.3）。** 一个 provider 进程一个 execution，与今天“每 turn 一个进程”一致；不做长寿会话 execution，也不在 kernel
  重启后接回进行中的协议流（kernel 重启时今天就杀掉并记 interrupted，boot 矩阵保持这一合同）。consumer `claude-planner` 与 `acp-planner`
  各自显式注册 no-op projection（`server/execution_boot.rs:18-28`）；owner 为 `OperationRef("worker-session:<id>")`（不透明字符串，SQL 不读
  它，`server/execution_store.rs`），track 取 `PlannerRow.track_id`（必填，`harness/wiring.rs:14-20`），root 为 `canonicalize(agent_cwd)`；
  alias 为每个 execution 唯一的 `planner:<worker_session_id>:<execution_id>`，使用本次准入的 execution_id；同一 execution 重放仍用同一 alias。
  executor 的 alias slot 一经占用不释放，Retire 后 tombstone 仍保留它，重启也重建它（workspace-execution.md §7 的 alias slot）；
  **保留成本**：每次 execution 新增一个永久 alias slot 与 Retire tombstone；Retire 释放输出和秘密，不回收 slot 或 tombstone，
  supervisor 重启须重载它们。因此长期空间、启动读取与内存随历史执行数线性增长（每次 rewind/turn/compaction 各一份）；SQL 也保留历史行。
  这不是并发量上界。历史压缩/回收需 executor owner 的合同与安全重放设计，另立后续片，不以复用旧 alias 绕过。
  `SessionExecutor` 保持 `handoff: None`，不加交接、不改 Engine 合同。`workspace_executions_current_alias` 唯一索引只约束同 alias 的行，
  不负责同 session 排斥；该排斥由下述与 `Engine::admit` 同事务的 `provider_execution_json` NULL→记录 CAS 和 `admission_gate` 共同保证。
- **owner 记录（durable release/decision，C4.0c 迁移）。** 没有每 turn 的 operation，记录放在 owner 行上：新迁移给 `worker_sessions` 加可空列
  `provider_execution_json`（编号合入时取下一个空号），删行即删记录，孤儿由 sweep 处理。`version` 为 JSON 整数 1：`{execution_id, purpose:
  Turn{turn_id} | Compaction{turn_id} | RewindCheck, release: NotRequired | Held | Releasing{authorized_at_ms} | Released{authorized_at_ms},
  decision: Observing | Stopping{reason, at_ms}}`，reason ∈ `Finished | Interrupted | Shutdown | TrackRetired | KernelRestarted | DriverLost |
  ReleaseRefused | SetupFailed | OutputGap | StdinTorn`（`Finished` 是正常收尾的 Stop，不是失败；判决只看协议结果与结算，不看 reason）。
  SQL 全在 calm-truth `truth/db/sqlite/provider_execution.rs`，整记录 CAS（同 C3.0 的按键比较整个对象）。只有四种写入：
  ① 准入：记录为 NULL 时写 `{新 id, Held|NotRequired, Observing}`，与 `Engine::admit` 同一写事务（token mint 仍按今天在其之前的事务）；
  ② release 的 Held→Releasing→Released；③ Observing→Stopping 与 `Engine::request(Stop)` 同事务；④ 完成：记录指向的 execution **已结算**、
  且 **owner 完成已确认**（下述入口返回 Ok）时清空记录并 `request(Retire)` 同事务，提交后 drive。**Retire 只在结算之后请求**：
  保留先确认旧进程已停止写 transcript、再完成并清空记录的顺序；未结算就清空会允许新进程与旧进程操作同一 Claude session。
  Retire 负责输出与秘密回收，不释放 executor alias slot，也不是下一次准入的前提；④ 提交之后，下一次准入无需等待 executor 实际 Retire。
  **记录只在 owner 完成之后清空**：它是丢失驱动者
  之后唯一的恢复依据（purpose 里的 turn_id 与 execution_id），先清空就无从完成 owner。下一次准入要求记录为 NULL，见“同 session 的唯一准入规则”。
  不选 `operations.tx_output_json`：turn 不是 operation；不选新表：每个 session 只有一个当前记录，历史已在 `workspace_executions`。
  `workspace_executions` 的 pending 部分索引归 C4.0d（见“全局周期恢复”），C4.0c 的迁移只加这一列。
- **claim 与重读。** 进程内驱动者的 claim 是 `(execution_id, 它最后写入的记录)`。每次写入整记录 CAS；不匹配就重读：`execution_id` 不同或记录
  为空 → 失去所有权、停止写入，永不接管另一个 execution；id 相同 → 只采纳更新的 decision（Stopping 优先于 Observing），release 只由本驱动者
  写，继续走向结算。sweep 只写 ③ 与 ④，从不写 release 或字节。单 kernel 进程内不需要 lease；跨 kernel 由 boot 全部 Stop。
- **进程内驱动表（只是加速器）。** `ProviderDrivers`：execution_id → 驱动句柄，在准入事务**提交之前**插入、完成 ④ 或 drop 时由 RAII 移除
  （驱动 task panic 或被 abort 时 guard 随 task 栈释放）；sweep 只用它判断“本进程内有没有驱动者”，从不把它当排斥。
- **owner 完成入口（provider owner 拥有，修第一轮 adversarial B2 / boundary B5）。** `server/provider_execution/mod.rs` 声明 owner 端口
  `ProviderExecutionOwner { fn consumer(&self) -> &'static str; async fn complete(&self, worker_session_id, record, evidence: Settled(Settlement) | Unconfirmed, known: Option<KnownEnd>) -> Result<()> }`，
  claude_planner 与 acp_planner 各实现一个，与 consumer 键在 `server/execution_boot.rs` 同处注册；通用 sweep 只按 consumer 键查表并调度，
  不含任何 provider 规则。`complete` 幂等、以 `(worker_session_id, execution_id, purpose.turn_id)` 为精确身份；sweep 只以 `Settled` 调用它，
  `Unconfirmed` 只由驱动者在结算超出界限时使用（见“结算后判决”），且从不授权 ④。**驱动者的
  正常收尾与 sweep 调同一个函数**：驱动者传 `known = Some(协议结果)`，sweep 对丢失的驱动者传 `None`（结果未知或 interrupted）；今天各驱动者
  尾部的这段逻辑（Claude `driver.rs:700-720` 的 `finish_turn`，ACP `session/driver.rs:65-157` 的 revoke、`settle_receipt`、projection、`active = None`）
  原样移进 `complete`，不留第二份实现。各 purpose 的语义：
  - Claude `Turn{turn_id}`：durable 写 `turn_outcome::record`（`harness/turn_outcome.rs:30`）；`None` 时与 `record_interrupted_turn`
    （`claude_planner/recovery.rs:12-47`）同形的 interrupted、`Confirmation::Unconfirmed`，该写入对已记录的终态幂等、不覆盖驱动者先写的结果。
    进程内：若 `HarnessRegistry`（`harness/registry.rs:47`）有该 worker session 的 Live handle，且其 `Shared.active` 指向同一 turn，则经
    `finish_turn`（`claude_planner/session.rs:120-140`，`state.active = None`）发布 `TurnCompleted`——比较 turn 身份与清除 `active` 在同一状态锁内原子完成（今天 `finish_turn` 无条件清空，C4.2 改为比较后清除，第二轮 adversarial R2-N3）；`DriverSlot` 记下自己的 execution_id，
    `complete` 只清除指向同一 execution 的 `Finished(Err)`，使 `join_driver`（`session/execution_end.rs:77-82`）不再永久返回 Conflict。
  - Claude `Compaction{turn_id}`：维护失败（`None` 时），清 `active` 与同一 execution 的 `DriverSlot`，等待者得到失败；不伪造 transcript turn。
  - `RewindCheck`：请求已不在，无 outcome，直接确认。
  - ACP `Turn{turn_id}`：按今天驱动者尾部的顺序：关闭 approvals 围栏、`revoke`（`acp_planner/session.rs:557`）、`settle_receipt`
    （`session/driver.rs:176`，CAS 不覆盖已完成的胜者）写 unknown、`turn_outcome::record`、`active = None`；ACP `DriverSlot`
    （`acp_planner/session/execution_end.rs:9-40`）同样按 execution_id 清除。
  确认 = `complete` 返回 Ok（durable 写入已提交、进程内围栏已释放；session 不在内存时只做 durable 部分，下次安装从 durable 状态重建）；
  返回 Err 或超时（每次调用 10s 上限）→ 记录保留，下一轮重试。`complete` 与 ④ 之间崩溃 → 下一轮重跑 `complete`（幂等）再 ④。
- **全局周期恢复（C4.0d，已由 #2999（`36b22516c`）交付，C4 的第一片，修第二轮 adversarial R2-B1 / boundary B3）。** 以下为交付前的缺口与该片的修复设计：此前 `Engine::recover` 只在 boot 调用一次（`server/execution_boot.rs:33`，
  源码中没有别的生产调用点）；Engine 周期遇 `StoreError` 时记错并移除 slot（`workspace-runtime/src/execution/engine.rs:109-133`），之后只有新的
  `drive` 才会重建它，executor 的 generation 变化唤醒不了已移除的 slot；SQL 中 Desired 改变也不唤醒已 park 的 Wait（`engine.rs:136-143`）。
  **既有消费者有同一缺口**（源码核实）：gate 的 parked reconcile 在未结算时只经 `drain_exited` drive，而它只在 leader 为 `Exited` 时请求 Stop
  （`operation/owned_execution.rs:110-134`、`task_gate_run/mod.rs:407-418`），所以 `store.settle` 在 Drained 之后遇一次 `StoreError` 的 gate 要等到 parked
  期限的 `execution::stop` 才结算；gate 与 forge 完成后的 Retire 只在完成时 drive 一次（`task_gate_run/mod.rs:466-470`、`:557-560`，forge observer
  `forge_action_adapter/lifecycle.rs:344-349`；forge 只在 Parked 期间每轮 `drive`，`lifecycle.rs:54`），Retire 遇 `StoreError` 后秘密留在库中直到重启；
  typed 终端只在创建与删除时 drive（`terminal_execution_create.rs:212`、`terminal_adapter.rs:605`），renderer 只读不 drive（`terminal_renderer/typed.rs:243-345`），
  `store.spawned` 遇 `StoreError` 后进程退出也无人结算，projection 不写退出、占用不结束。所以周期恢复不是 Planner 专属。C4.0d 是 C4 的**第一片**，无前置、**独立可部署**：
  它的生产消费者就是上述既有 gate、forge 与 typed 终端，单独合入即修复它们今天在 `StoreError` 或提交后 drive 丢失之后要等重启的缺口，
  不依赖 C4 的其余各片，也不依赖 W。C4.0d 在 Engine 的组装 owner `server/execution_boot.rs` 中，boot `recover` 成功后起**唯一**的周期任务，每 30s 调用一次既有 `Engine::recover()`
  （`engine.rs:81-97`，经 `Store::pending` drive 每个未结算行与 `desired = 'retire'` 但秘密未清的行）；`RecoverReport.error` 记日志、下一轮重试；
  进程内一个 kernel 一个节拍，没有第二个全局 recover 计时器，Engine 内不加任何 consumer 或 provider 分支。已有 slot 时 `drive` 只置 dirty 并 notify
  （`engine.rs:60-66`），所以重复调用幂等；它不是第二个状态机，进度规则仍全在 Engine 的 `next_step`。不改 Engine 合同（只是周期调用既有公开方法）。
  每个消费者——gate、forge、typed 终端，以及 C4.1 起的 Planner——都依赖这一节拍保证 Stop、结算、Retire 与秘密清除最终发生，
  无需重启或用户操作；各 owner 的 sweep 只处理自己的记录（Planner 的 (a)–(c)、gate/forge 的 parked reconcile），不再各自调用 `recover`。
  C4.0d 同片加 calm-truth 的 pending 部分索引（谓词与 `pending` 查询逐字相同：`settled_at_ms IS NULL OR (desired = 'retire' AND secrets_json IS NOT NULL)`，
  `truth/db/sqlite/workspace_execution.rs:103-110`），使每轮只扫未完成的行；C4.0c 的迁移因此只含 owner 记录列。故障注入用 `fixtures` feature 下的一次性
  store 故障接缝（`server/execution_store.rs` 的 `spawned`/`settle`/`finish` 与 executor `retire` 返回一次 `StoreError`，与既有 `test_seams::crash_point` 同一模块），
  生产构建不含。回归（C4.0d，均无重启、无用户操作）：`c4_recovery_tick_settles_drained_gate_before_deadline`（gate Drained 后 `settle` 失败一次：
  下一轮结算，gate 在 parked 期限之前完成）、`c4_recovery_tick_redrives_gate_retire_after_store_error`、`c4_recovery_tick_redrives_forge_retire_after_store_error`
  （完成后的 Retire 失败一次：秘密清除、tombstone 写入）、`c4_recovery_tick_redrives_typed_terminal_after_store_error`（`spawned` 失败一次后进程退出：
  一轮内结算，projection 写退出）、`c4_recovery_tick_is_idempotent_with_live_slots`（先断言前提：live slot 存在期间节拍至少运行两轮，轮数取自 `fixtures` 接缝记录的 `recover` 调用次数；
  再断言连续多轮不产生第二个驱动循环、不重复 Prepare/Launch。没有前提断言时，去掉节拍会让它空洞地通过）、
  `c4_recovery_tick_retries_after_pending_read_error`。
- **Planner 的 durable 周期 sweep（`server/provider_execution/sweep.rs`，修第一轮 adversarial B1）。** boot 一次（下述 pass B），之后每 30s 一次
  （与 terminal sweeper 同节拍，`server/state.rs:1646` 附近起任务）。Engine 进度——提交后 drive 丢失（future 在 COMMIT 确认前被取消）、瞬时
  `StoreError`、Stopping 中与 ④ 已提交的 C4 行——由 C4.0d 的全局节拍保证，本 sweep 不再调用 `recover`。每轮按序：
  (a) C4 consumer 的未 Retire 行，若 owner 记录缺失或指向别的 id：未结算 → `request(Stop)`，已结算 → `request(Retire)`（card/track/worker
  session 删除留下的孤儿；被删除的 session 已不能再启动，不依赖 alias 索引排斥）；(b) 记录指向它、本进程无驱动者、仍 Observing → ③
  Stopping{DriverLost}；(c) 记录指向它、已结算、无驱动者 → owner 的 `complete(…, None)`，Ok 后 ④。记录为 Stopping 且未结算时无需额外动作：
  ③ 已在同一事务写了 `Desired::Stop`，由 C4.0d 的全局节拍保证推进。每步一个事务、提交后 drive；(a)–(c) 每行一个事务，`complete` 有 10s 上限，一行卡住不拖住整轮。
  **#3020 inventory 合同**：每行 Err 属于未分类，永不作为 owner 缺失的证据，也不据此 Stop、Retire 或清记录；其他健康行继续。
  `consumer_inventory` 用真实 `SqliteExecutionStore::insert` 的行验证序列化兼容并测量扫描耗时；本查询返回全部未 Retire 行，
  按 consumer 解码筛选，每个注册 consumer 每 30s 一次。当前 `ORDER BY execution_id` 的 SQL 计划可沿主键扫描全部保留历史，
  因而 SQL 扫描成本随历史总数增长，JSON 解码成本随全部未 Retire 行数增长，均不只随 Planner 行数增长；实现交付报告须记录实测与此保留成本。
  execution_id 列由 typed writer 写入；损坏身份与 `pending()` 统一：整个扫描返回 Err，下一轮重试，不跳过、不采取生命周期动作。
- **C4.3 implementation decisions A/B/C.** ACP keeps its existing 45s receipt-only known-result persistence budget in the driver, before calling the completion owner. Owner completion makes one idempotent attempt inside the shared 10s bound; failure retains the claim for retry (A). `KnownEnd` carries the driver's own end; recovery reuses an existing durable outcome and otherwise records unknown/unconfirmed, retaining only live facts the driver actually observed (B). The #3044 receipt fence is unchanged: a lost, aborted, or panicked driver revokes its token, clears the settled execution claim, and settles the receipt unknown without `kept_cleanup` evidence. It is never a `StoppedRevokedNormallyJoined` observation; the session stays held (C).
- **C4.2 carrier 围栏（R1–R4，收窄合同）。** 持有 `provider_execution_json` 的 worker session 必须保持 conversation 的当前 carrier，且记录清空之前不离开 active/failed。R1：`admit` 在同一 SQL 事务拒绝 terminal-bound session（typed Conflict），因此 typed terminal projection 与 Planner claim 不相交，`terminal_settled_tx` 不改。R2：请求路径的 idle carrier replacement、`cards.session_id` repoint、显式 close/reset 先在旧 session 的 issue 锁内以注册 owner 调 `admission_gate`（已结算 claim 可 inline 完成，否则 Conflict），再在写事务里检查 claim 为空。R3：在其它 writer 事务内运行、不能做 owner I/O 的 writer 只加 SQL 围栏，claim 非空即跳过该行或拒绝；不嵌套 owner 完成，等 30s sweep 后重试。R4：track/card 删行先经 provider `sweep_track` Stop；owner 记录随 worker row 删除，孤儿由 owner sweep 回收。outcome 写入仍走 `harness_turn_outcome_put` 的当前 carrier 检查，不绕过、不跳写。
- **同 session 的唯一准入规则（自 C4.1 起，修第一轮 adversarial B3）。** 同一 worker session 的每个启动——C4 的 Engine 准入 ① 与仍原生的
  启动（C4.1 时 Claude 的 turn、canonical 与 compaction 都经 `start_spawn`，`claude_planner/session.rs:295`；rewind 经 `prepare_edit`，
  `session/invocation.rs:63-80`）——都在既有 `issue` 锁内（`session.rs:309`）先调用同一个 `provider_execution::admission_gate(worker_session_id)`：
  记录为 NULL → 继续（Engine 准入仍须在 ① 的同一事务以 NULL→记录 CAS 排斥竞争者）；非空、已结算、本进程无驱动者 → 内联做 owner `complete` 与 ④ 后继续；其余 → `Conflict`（失败关闭、可重试，对应今天
  “下一次 spawn 的 stop 失败即关闭”，`driver.rs:690`）。读语句在 calm-truth，原生路径的检查与 Engine 路径共用这一函数。由此 C4.1 单独部署即安全：
  被取消的 rewind 请求在 `issue` 锁释放后仍留着非空记录，直到结算与 ④，期间同 session 的原生 turn 或 compaction 被拒，不会与它并发操作同一
  Claude session；反方向由既有 `active.is_some()` 检查（`session.rs:322`，同一锁内）排斥 rewind。与删除和 boot 的交互：删 worker session 行即删记录，
  该 session 已不能再启动，旧 execution 由 sweep (a) Stop；boot pass A 写 Stop 后，原生启动在结算前一律被拒；Unknown 时被拒到宿主重启（Q2）。
- **Boot 矩阵（Planner）。** pass A 在 MCP listener 之前、revoke 之后，放进既有 `claude_planner::lifecycle::boot`（`lifecycle.rs:27`）与
  `acp_planner::config::boot`（`config.rs:312`）：一个事务内对该 consumer 的每个非空记录写 ③ 并经 calm-truth 的同一 request 语句写
  `Desired::Stop`（不需要 Engine 实例）；boot 的 `Engine::recover`（`server/state.rs:1638`）或 C4.0d 节拍的下一轮 drive 这些行（与二者先后无关）。pass B 是 sweep 的首轮。
  listener 之前**只保证**两件事：旧凭据已撤销（revoke 失败照今天使 boot 失败，`lifecycle.rs:21-28`）与 Stop 请求已 durable；实际 Stop、结算、
  pass B 与 owner 完成都在之后。pass A 写入失败只记日志、boot 继续（今天 marker sweep 失败同样只记日志）：被撤销的 token 不能认证，
  `admission_gate` 对非空未结算记录失败关闭，pass B 的 (b) 再以 DriverLost 写 Stop。新测试 `c4_boot_pass_a_precedes_listener_and_failure_is_covered_by_sweep`
  断言顺序（revoke → pass A → listener）与 pass A 失败时的这条退路；既有源码扫描只作辅证，不当作物理静默的证据。

  | 记录状态 | boot 动作 |
  |---|---|
  | 无记录 | 无 |
  | Held 或 Releasing（载荷未发或可能部分送达） | Stopping{KernelRestarted} + Stop，永不发载荷；结算后 owner `complete(None)`（interrupted，与既有 `record_interrupted_turn` 幂等一致）再 ④ |
  | Released、Observing（turn 进行中） | Stopping{KernelRestarted} + Stop；结算后 owner `complete(None)` 再 ④；同今天 boot sweep + interrupted |
  | 已记 Stopping | 保持原因，确保 Desired 为 Stop；结算后 owner `complete(None)` 再 ④（驱动者若已记下结果，幂等写入不覆盖） |
  | 已结算 | owner `complete(None)` 再 ④ |
  | executor 不可用 | Desired 与记录保留；新 turn 得 `ExecutionUnavailable`；可用后由 C4.0d 节拍与 sweep 收尾 |
  | 同宿主 supervisor 崩溃（Unknown） | 不结算、不 Retire；该 card 下一 turn 被拒直到宿主重启（Q2） |
  | 宿主重启（NoChild `host_rebooted`） | 结算 → ④ |
  | pre-C4 原生进程（无 execution） | boot 仍按今天做 marker sweep（Engine 启动的进程不再带 marker，sweep 只命中升级前的原生进程）。用户可能跳过中间版本，所以 C4.4 **保留**这条只在 boot 运行的 pre-C4 sweep（只读 `/proc` 扫描加信号，约 120 行），只删除运行期的 marker 路径。Claude boot 在 session ids 中加入 `readiness`，ACP boot 在 session ids 与 `catalog` 中加入 `readiness`，各用同一次 sweep，覆盖升级前的 readiness/auth/catalog probes。readiness 没有 session 或 MCP token，不单建证明；失败仍 warn，Claude 把全部 session 记为 unproven；它的最终删除另立 issue，以项目的最低升级路径政策为前提，不靠升级说明 |

- **凭据与 release（复用 C3.2b 载荷）。** `ExecutionSpecification.environment` 写入不可变 `intent_json` 并进 supervisor journal（C3 设计“构造与环境”），
  所以秘密只走释放握手的 stdin：Claude 为 `NEIGE_MCP_TOKEN`；ACP 为 `NEIGE_MCP_TOKEN` 加运营者 `config.env` 各键的值（键名与顺序取自注册，不是秘密）。
  非秘密键（白名单、PATH、`CLAUDE_CONFIG_DIR`、`NEIGE_MCP_SOCKET`、profile launch env）进 intent。kernel 先按今天的覆盖顺序合并最终环境
  （ACP 为 `config.env` → PATH → `NEIGE_MCP_*` → profile launch env，`acp_planner/process.rs:60-80`，后者覆盖前者），再按每个键最终值的来源
  分区：来源是秘密的进载荷，其余进 intent；同名键只出现在一侧，构造时断言，所以载荷导出不会改变今天的覆盖结果。marker 键不再进入 Engine 启动的 env。
  - **代理值（Q8 已决定）。** Planner 最终环境中的代理键（`HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` 大小写两组中实际出现的，如白名单里的 `ALL_PROXY`，
    `provider/src/codex/shared/environment.rs:24-28`）先经一个分类函数 `proxy_value_is_secret`（`server/execution_environment.rs`，C4.1 引入，C4.3 复用）：
    URL 代理值只要不是有效 UTF-8、无法解析、没有 host、解析后含非空 username 或任意 password、或原始值含任意 `@` → 秘密，走 Planner 的释放载荷；其余进 intent。
    `NO_PROXY`/`no_proxy` 是 host pattern 排除列表，不需要 URL 解析：不是有效 UTF-8 或含任意 `@` → 秘密；普通 domain 列表进 intent。C4 的每个
    带秘密的 consumer（rewind、Claude/ACP Planner turn 与 compaction）都有释放载荷。rewind 的 `session_env` 同样可携带含 userinfo 的代理值，不能视为无秘密。
    worker 的代理（`child_environment.rs` 白名单与 `terminal_worker_env`）及没有载荷通道时的处置属 W（§3.3 WR7），W 复用同一分类函数。
    gate 先例（`gate_execution.rs:50-56` 把代理写进 intent）是既有暴露，不作为本设计的依据，另立 issue 处理，不在 C4 扩大。
  - **载荷键合同（修第一轮 boundary B3）。** C4.0a 的共享模块 `server/operation/stdin_payload.rs` 只接受 `PayloadKeys::new` 校验过的键：
    每个键都须是 POSIX 可移植标识符 `^[A-Za-z_][A-Za-z0-9_]*$`，不是 shell 特殊名（`IFS`、`ENV`、`PS1`/`PS2`/`PS4`、`OPTIND`、`OPTARG`），也不在
    wrapper 内部变量的命名空间 `neige_*`（今天的 `neige_forge_credential` 等，`forge_action_adapter/mod.rs:183-194`）；运营者键另外不得是 `PATH`
    或 `NEIGE_*`（kernel 凭据与 wrapper 输入，`acp_planner/config.rs:145-151` 今天已拒这两类）。kernel 固定键（`NEIGE_MCP_TOKEN`、代理键、forge 键）
    是常量，由一条常量测试证明满足同一谓词。键只有校验通过后才插入 wrapper 源码的 `export KEY=…`，值只经
    `IFS= read -r` 读进内部变量，从不进入源码，也不 eval。同一个 `validate_key` 是唯一来源：ACP 注册校验（`config.rs:145`）改为调用它，新注册以
    `BadRequest` 拒绝；已存在且不合格的注册在 turn 准入时、mint 之前以同样的显式错误拒绝，不静默跳过、不回退到直接 `Command::env`。
    forge 的键表满足该谓词，行为不变。这是对今天“任意不含 `=`/NUL 的名字”的收窄（`X-Y` 一类名字今天能传给子进程，
    但 shell 无法引用），写进 PR 与升级说明。新测试：`c4_payload_key_rejects_non_identifier`（含空格、`-`、`$(…)`、`;`）、
    `c4_payload_key_rejects_reserved_and_internal_names`（`IFS`、`neige_forge_credential`、`NEIGE_X`）、`c4_existing_bad_acp_key_refuses_before_mint`。
  wrapper：program `/bin/sh -c`，先建立下述有界 stderr 汇点并把 fd 2 指向它，然后按固定键序 `IFS= read -r` 读取，不符或 EOF → `exit 75`，
  最后 `exec` provider argv。载荷之后同一 stdin 继续承载协议：POSIX `read` 在 pipe 上逐字节读到换行，不多读，后续字节到达 provider
  （新测试 `c4_payload_leaves_protocol_bytes_for_provider` 以真实 `/bin/sh` 钉住）。C4.0a 把 forge 的载荷编码与校验（`operation/forge_action_adapter/credentials.rs:9-54`）
  与 wrapper 的读取前缀（`forge_action_adapter/mod.rs:171-194`）抽成按键表参数化的共享模块，forge 行为不变。顺序同 C2b/C3：① durable Held→Releasing
  先于首字节；等可交互的 Spawned receipt（上限 10s，同 ACP `spawn_within`；Claude 今天无上限，此为新上界）；② `Engine::interact(WriteStdin)` 按
  `accepted_bytes` 在同一次存活调用内续写后缀；③ Releasing→Released；之后才写协议的第一行。失败或超时 → ③ Stopping{ReleaseRefused}；任何
  claimant 看到 Releasing 都永不重发。首个 user line 的 `BeforeWrite`/`CrossedWrite` 分类（`session.rs:543-562`）改按该行的 `accepted_bytes`
  （0 → BeforeWrite，部分 → CrossedWrite）。rewind 预检也用 Held→Releasing→Released：秘密代理只经 C4.0a stdin 载荷；非秘密代理进 intent。wrapper 读完载荷后关闭 stdin，不再读取协议。
- **stdout 与 stderr、输出窗口。** Pipe 把 stdout/stderr 并入同一输出（`supervisor/execution/process.rs:177-184`），协议只能占 stdout，所以 stderr
  由 wrapper 导向每个 execution 一个**有界**汇点（Q3 已决定，不再有无界文件）。wrapper 在 owner 给出的 0700 目录（Claude
  `claude-planner/tmp/<execution_id>/`，完成 ④ 时删除；ACP 改为 `acp-agent-logs/<worker>/` 目录内的同名文件并跨 turn 保留，今天是 `<worker>.stderr.log`，路径变化写进 PR）里 `umask 077; mkfifo err.fifo`，后台起
  汇点 `{ head -c 262144 >stderr; if [ "$(dd bs=1 count=1 2>/dev/null | wc -c)" -gt 0 ]; then : >stderr.overflow; wc -c >stderr.dropped.tmp && mv stderr.dropped.tmp stderr.dropped; fi; : >stderr.closed; } <err.fifo &`，
  再 `exec 2>err.fifo` 并删除 fifo 名；`head` 保留前 256KiB（与今天 ACP 的上限相同，`acp_planner/process.rs:125-186`），之后读一个字节探测是否溢出：
  读到字节就**立即**创建 `stderr.overflow`（溢出事实在越过上限的那一刻持久化，不等 EOF），再由 `wc -c` 把剩余字节读掉并只记数，计数先写临时文件再
  `mv`，读者不会看到半写的数；provider 永不因 stderr 反压。汇点只有在读到 EOF 之后才创建 `stderr.closed`：不溢出时 `dd` 读到 0 字节即 EOF，
  溢出时 `wc` 读完全部剩余字节；这是“汇点完整看到了 stderr”的唯一正面证据。汇点的各进程都在同一 execution 的进程组内，Stop 与结算一并覆盖，
  而正常收尾也会 Stop（见“结算后判决”）。Stop 可能在任何一步打断汇点：`wc` 只在 EOF 时输出（第二轮两路评审以 `/bin/sh` 实测：provider 写 1MiB
  后不退出，组 TERM 后 `stderr` 为 256KiB、计数为空）；探测读到溢出字节与创建标记是两个可被杀死的步骤，排队在 fifo 中的字节也可能随 Stop 一起丢弃
  （第三轮 adversarial 以 `/bin/sh` 实测：写 262145 字节、保留文件到达 262144 时立即组 TERM，30 次都只留 262144 字节而无标记）。所以计数与溢出
  标记都可能缺失，**没有 `stderr.closed` 就不能断言未截断**。mkfifo 或重定向失败 → `exit 75`（release 之前，ReleaseRefused）。结算后 kernel 读 `stderr`
  的尾行作判词，并给 turn 的诊断加显式结果：有 `stderr.closed` 而无 `stderr.overflow` → 未截断（恰好 256KiB 而无后续字节也不算溢出）；有标记且有计数 N
  → `StderrTruncated{dropped_bytes: Exact(N + 1)}`；有标记而无计数（`wc` 被 Stop 中断）→ `StderrTruncated{dropped_bytes: AtLeast(1)}`，信息写明“前 256KiB
  之后丢弃了字节，计数因停止而中断”；既无标记也无 `stderr.closed`（汇点在 EOF 之前被 Stop 打断）→ `StderrCaptureInterrupted`，信息写明“汇点被停止，
  保留内容之后是否丢弃了字节未知”，即可能截断，从不报告为未截断。
  缺失或无法解析的计数从不读作 0，也从不阻塞 owner `complete`：汇点文件只是诊断，读取失败记为 `StderrUnreadable`，不当作 turn 失败、不使 `complete` 返回 Err。
  磁盘上界是每个 execution 256KiB 加三个空或很小的文件；驱动者丢失或 owner 行删除后，目录由 owner `complete` 与 ④ 删除，孤儿目录由
  boot 按今天清 instructions 目录的方式清空（`lifecycle.rs:36-55`）。新测试 `c4_stderr_sink_is_bounded_and_reports_overflow`（真实 `/bin/sh` 写
  1MiB stderr 后正常退出：文件恰为 256KiB、计数为余数、协议不受影响）、`c4_stderr_overflow_survives_stop`（写 1MiB 后不退出，经 Stop 结算：
  结果为 `AtLeast`、`Exact` 或 `StderrCaptureInterrupted`，从不报告“未截断”）、`c4_stderr_stop_at_cap_is_never_untruncated`（写 262145 字节，保留文件到达
  262144 字节时立即 Stop：结果为 `AtLeast(1)` 或 `StderrCaptureInterrupted`，即第三轮 adversarial R3-N1 的情形）、`c4_stderr_overflow_survives_descendant_holding_fifo`
  （setsid 子孙持有 stderr，Stop 排空后同上）、`c4_stderr_exact_cap_is_not_overflow`（恰写 262144 字节后正常退出：有 `stderr.closed`、无标记，报告未截断）、`c4_stderr_unreadable_never_blocks_complete` 与 `c4_stderr_sink_cleared_after_driver_loss`。
  `replay_bytes` 取上限 4MiB（`supervisor/execution/output.rs:39`）。OutputLog 只追加、按 execution 保留到 Retire（`output.rs:58-66`），`snapshot`
  只返回最后 4MiB（`:48-57`）且把落后的 `from` 夹到窗口起点，所以读者必须自检缺口：回复的 `cursor - output.len()` 大于请求的 cursor 即缺口 →
  `io::ErrorKind::InvalidData` → ③ Stopping{OutputGap}，turn 失败。这比今天窄：今天 pipe 满时 CLI 被反压，Engine 的读者线程从不反压
  （`output.rs:68-100`），kernel 落后超过 4MiB 即失败；ACP 协议帧上限 8MiB（`provider/src/acp/transport.rs:9`），单帧超过 4MiB 而 kernel 未及时
  读取同样以 OutputGap 失败，测试覆盖完整与不完整的末尾输出两种边界。**stdout 的 OutputLog 不是有界磁盘**：`replay_bytes` 只是快照窗口，
  文件在一个 execution 内无上界（Claude turn 无总时限），到 Retire 才释放；今天 stdout 只经 pipe、不落盘，这是新增的磁盘占用。界定它需要
  executor owner 的 OutputLog 上限或轮转（Engine/executor 合同变更），不在 C4：写进 PR 作为已接受的残余，另立 issue；C4 只保证每个 turn 结束即
  Retire（含 owner 完成后的 ④），不让 OutputLog 跨 turn 累积。
- **协议留在 provider 层。** C4.1 引入 `server/provider_execution/stdio.rs` 的 `EngineStdio`：`AsyncRead` 基于 `Engine::output_generation` +
  `Engine::output` 的 cursor（含缺口检测），C4.2 加 `AsyncWrite` 基于 `Engine::interact(WriteStdin)`（部分写后 Transport → `StdinTorn`，不自动重发，
  workspace-execution.md §4 细则 → ③ Stopping{StdinTorn}）。Claude 的 `BufReader::lines`（`driver.rs:206`）、`TurnSlot`（`turn_slot.rs:104`）与 ACP 的
  `Connection::new(reader, writer)`（`provider/src/acp/transport.rs:160`）只换掉 `ChildStdout`/`ChildStdin`，provider 的编解码一行不搬。
- **结算后判决（取代 marker 停止证明，修第一轮 boundary B1）。** Claude 的 `Confirmation::ProviderTerminal`（`driver.rs:665-668`）今天以 marker
  扫描为空为证据；改为该 execution 已结算（Drained/NoChild）。**规则：协议结束后一律 durable 请求 Stop，结算之后才有 outcome 与 Retire**，
  与 leader 是否已 `Exited` 无关——今天 `driver.rs:649-664` 也是等完 `EXIT_WAIT` 后无条件 `stop_by`，`claude_planner_session.rs:344` 钉住
  “结果之后不退出的 CLI 被停住且 turn 正常完成”。Claude 收尾：result 行 → `close_stdin` → 至多 `EXIT_WAIT` 5s 等 `Exited`（只是给 CLI 自行
  退出的机会）→ 无论观察到 `Exited` 与否，③ Stopping{Finished} 与 `Engine::request(Stop)` 同事务、提交后 drive → 在 `SETTLE_AFTER_STOP` 12s 内
  等结算 → owner `complete(Some(结果), Settled)` → ④ → `TurnCompleted`。**不复用 `drain_exited`**（`server/operation/owned_execution.rs:110-134`）：
  它只在 `Exited` 时请求 Stop，且要求 operation 的 live claim；与 owner 无关的部分只有“`Engine::request(tx, id, Stop)`，提交后 drive”，Planner 以 ③
  的记录 CAS 作围栏直接调用它，不为 Planner session 伪造 Operation。未在界内结算 → 驱动者以 `complete(Some(结果), Unconfirmed)` 按今天的方式
  durable 记下 `Unconfirmed` 结果并释放进程内 `active`（`driver.rs:672-716`），**不做 ④**；记录保留，`admission_gate` 拒绝下一次启动；结算后 sweep (c)
  以 `complete(None, Settled)` 补做（幂等，不改写已记的结果）再 ④。`Exited` 从不单独授权判决或 Retire。同一规则覆盖每条结束路径：
  - ACP turn：prompt 响应或取消窗口结束 → 关闭 approvals 围栏 → ③ Stopping{Finished|Interrupted} + Stop（今天 turn 末本就无条件 `process.stop`，
    `session/driver.rs:73`）→ 结算 → `complete` → ④；以结算取代 `reap` + 组 sweep（`process.rs:233-240`）。
  - 准入后的 setup 与拒绝：Claude 的 `abort_before_ok`（`session.rs:644-664`）、release 失败、ACP `initialize`/`session/new`/`session/load` 失败
    （`process.rs:245-260`）→ ③ Stopping{ReleaseRefused|SetupFailed} + Stop → 结算 → `complete(None…)` → ④，从不在 `Exited` 时直接返回。
  - rewind 预检：result 或 leader 退出 → ③ Stopping{Finished} + Stop → 结算后才读判词（`c4_rewind_verdict_waits_for_settlement`）。
  新测试 `c4_lingering_cli_after_result_is_stopped_and_completes`（假 CLI 输出 result 后忽略 EOF：Stop 在 `EXIT_WAIT` 后发出、turn 以已知结果完成、
  Retire 在结算之后）、`c4_acp_setup_failure_settles_before_retire`、`c4_refusal_before_ok_settles_before_retire`。
- **计时、中断、关闭与破坏性步骤（Q4 已决定：接受 Engine 的 200ms STOP_GRACE）。** interrupt 保留协议宽限：照旧先写 control_request
  （`session.rs:741-753`），`STOP_TIMER` 10s 到期才 ③ Stopping{Interrupted}；ACP cancel 窗口（3s/5s，`session/driver.rs:44-52`）不变。关闭与 hibernate →
  ③ Stopping{Shutdown}，至多等 20s（`claude_planner/session/execution_end.rs:185-220`）。executor 的 `STOP_GRACE` 200ms 之后 KILL，取代 marker 的 5s
  TERM 宽限（`planner_process.rs:16`）：interrupt 路径只在 CLI 无视 control_request 10s 之后才走到它；关闭、hibernate 与 track stop 则立即 TERM、200ms
  后 KILL，这是写进 PR 与升级说明的行为变化，不恢复 provider 自有的第二个 killer；若实测 provider 需要更长的 TERM 清理，只能经 executor 的 typed
  stop 政策统一延长（合同变更，另立）。端到端时限以真实 executor 与真实进程树测试：`c4_shutdown_deadline_includes_provider_cleanup`（假 CLI 在 TERM
  后 50ms 内 flush 一行 transcript：flush 落盘、结算在关闭预算内；另一例忽略 TERM：200ms 后 KILL、仍在 20s 内结算）、`c4_track_stop_settles_within_quiesce_deadline`、
  `c4_interrupt_keeps_protocol_grace`（8s 内响应 control_request 的 CLI 不被 Stop）；真实 Claude/ACP CLI 的 TERM 清理耗时只在专用宿主测一次，结果写进 PR。
  `lifecycle::sweep_track` 与 ACP `sweep_track` 保留 revoke，marker 部分改为 owner 专用的 stop：同一事务内对该 track 的 C4 记录写 ③
  Stopping{TrackRetired} + Stop，提交后 drive 并在 `QUIESCE_DEADLINE` 内等结算，未结算 → Err，破坏性步骤照今天中止。不用通用
  `execution_quiesce::quiesce(Track)`：repoint（`routes/tracks.rs:2095`）也调用它，通用 quiesce 会停掉同 track 的 gate 与 forge，是行为变化。
- **executor 不可用。** 缺 Engine 或 `!available()` → `CalmError::ExecutionUnavailable`，在 mint 与准入之前返回，不建行、不 fork、不回退原生（同 C2b/C3）。
- **切片（每个启动或恢复入口一片；规模口径同 §3.2，“预计”按 C2b/C3 实测比：C2b 1.7–2.6×，C3.2a 0.9×、C3.2b 1.0×、C3.2c 2520 → 5887 即 2.3×）。**

  | # | 结果 | 前置 | owner 记录 | boot 矩阵 | release / 凭据 | 活性 | 删除的原生路径 | 规模（设计 → 预计） | 等级 |
  |---|---|---|---|---|---|---|---|---|---|
  | C4.0d | **已由 #2999（`36b22516c`）交付；C4 的第一片，独立可部署**；全局周期恢复：`server/execution_boot.rs` 在 boot `recover` 成功后起唯一的 30s `Engine::recover` 节拍（`RecoverReport.error` 记日志、下一轮重试），`state.rs` 接线；`workspace_executions` 的 pending 部分索引迁移；`fixtures` 下的一次性 store 故障接缝；生产消费者是既有 gate、forge 与 typed 终端 | 无 | 部分索引 | boot `recover` 不变，节拍在其后 | 无 | 节拍保证每个未完成行（含 Stopping、已请求 Retire 而秘密未清）推进 | 无 | 节拍与接线 +50、迁移 +15、故障接缝 +40、新测试 +240、`execution_boot::port_contract` 改写 +20 ≈ 365 → 450–800 | L2：生命周期进度与迁移 |
  | C4.0a | 行为不变：forge 的载荷编码/校验与 wrapper 读取前缀抽成按键表参数化的 `server/operation/stdin_payload.rs`，含载荷键合同（`PayloadKeys::new`/`validate_key`，见“载荷键合同”），forge 改用它 | 无 | 无 | 无 | 共享载荷（换行、NUL、超长在首字节前拒绝；键须为非保留的可移植标识符） | 无 | 无（移动） | +190/−110，测试 +110 ≈ 410 → 410–650 | L2：凭据边界 |
  | C4.0b | 行为不变：Claude（`tests/cases/claude_planner_session_fixture.rs`、`claude_planner_stack_fixture.rs`）与 ACP（`acp_planner_provider` 的 boot 世界）测试世界接 `tests/support/execution.rs` 的真实 executor，重启类测试的 supervisor 留在测试进程、活过 kernel kill；`ClaudePlannerHost`/`AcpPlannerHost` 的会话参数带上 Engine 句柄（未使用） | 无 | 无 | 无 | 无 | 无 | 无 | +200/−40 ≈ 240 → 300–550 | L1：只动测试支撑与未用参数 |
  | C4.0c | `worker_sessions.provider_execution_json` 迁移与 calm-truth 记录 SQL（①–④ 的 CAS、sweep 与 `admission_gate` 读取），无消费者（同 E2a 先例：store 是迁移的写者） | 无 | 列与编解码 | 无 | 无 | 无 | 无 | 迁移 +20、SQL +150、迁移与 CAS 测试 +150 ≈ 320 → 320–520 | L2：迁移 |
  | C4.1 | Claude rewind 预检（N2）成为第一个 Engine 消费者，同片引入 owner 层：`server/provider_execution/{mod.rs,sweep.rs,stdio.rs}`（记录、驱动表、owner 完成端口、`admission_gate`、durable sweep、boot pass A、`EngineStdio` 读侧、有界 stderr 汇点、`execution_environment.rs` 代理分类与 wrapper；Engine 进度由 C4.0d 节拍保证）与 consumer `claude-planner`；仍原生的 turn、canonical 与 compaction 启动接入同一 `admission_gate` | C4.0a、C4.0b、C4.0c、C4.0d | RewindCheck；同 session 的全部启动经 `admission_gate` | pass A：未结算 → Stopping{KernelRestarted}；已结算 → `complete` 后 ④ | Held；秘密代理值只经 stdin 载荷，Released 后 `close_stdin` | 请求 future 被丢弃 → RAII 写 ③；sweep (b)(c) 与 C4.0d 节拍兜底；30s 由 Stop 取代 `kill_on_drop` | `rewind.rs:111-157` 的原生 `Command`/`wait_with_output`；判词改读 Pipe 输出、stderr 汇点与结算 status | owner 层 +360、owner 端口与 Claude rewind 实现 +60、`admission_gate` 与原生路径接入 +60、stdio 读侧 +90、stderr 汇点（溢出与结束标记、四种结果）+75、rewind +80/−50、新测试 +440（通用的 StoreError 用例移到 C4.0d，Planner 的两条丢失 drive 用例保留，新增五条 stderr 用例）、改写 +40/−40、代理分类函数与表测试 +80、wrapper/release +100、rewind 凭据测试 +80 ≈ 1535 → 2500–3600 | L2 |
  | C4.2 | Claude Planner turn 与 compaction（N1）改为 Engine 消费者；驱动者尾部移进 owner `complete`；复用 C4.1 的代理分类函数与 wrapper | C4.1 | Turn/Compaction | 见 Planner boot 矩阵 | `NEIGE_MCP_TOKEN` 与秘密代理值走载荷；首个 user line 在 Released 之后 | 驱动者 + sweep；结算判决；协议结束后无条件 Stop | `session.rs:491-532` 的原生 spawn 与 `abort_before_ok` 的 kill（`:644-664`）、`session.rs:399` 的 spawn 前 marker stop、`driver.rs:243-252`（`child.wait`）与 `:649-694`（marker `stop_by`、`start_kill`、reap）、`claude_planner/session/execution_end.rs` 的 marker stop、`recovery::quiesce_canonical`（`recovery.rs:52-57`）的 marker stop、`lifecycle::sweep_track` 与 `shutdown_replay` 的 marker 部分；Engine 进程不再带 marker | stdio 写侧 +80、`start_spawn` +180/−120、driver 收尾与 `complete`（Turn/Compaction）+170/−110、中断/关闭/track stop +90/−60、boot pass +30、新测试 +640、改写 +260/−210 ≈ 1950 → 3350–5100 | L2 |
  | C4.3 | ACP Planner turn（N4）改为同一 owner 层的消费者 `acp-planner`；ACP 驱动者尾部（revoke、receipt、projection）移进 owner `complete` | C4.2 | Turn | 同上；receipt 恢复不变 | `NEIGE_MCP_TOKEN`、`config.env` 值与秘密代理值走载荷；`session/new`/`session/load` 中的 token 仍走协议 stdin（字节不落盘）；不合格的既有注册键在 mint 前拒绝 | 同上；setup 失败同样先结算 | `process.rs:42-125` 中 turn 用的原生 spawn、`spawn_within` 与 `stop`/`reap`（readiness/catalog 仍用原生 `Process`，见 N5）、ACP turn 的 marker stop | +1150/−380、`complete` 与环境分区 +120 ≈ 1650 → 2800–4200 | L2 |
  | C4.4 | marker 运行期机制删除与 readiness 生命周期的有界化：删 `claude_planner/stop.rs`、ACP readiness/catalog 的 marker stop、不再用于 boot 的 fixtures 接缝与 `planner_process.rs` 的运行期部分；保留 Claude `lifecycle::{sweep_track,sweep_card,sweep_scope}` 与 ACP `config::sweep_track` 的 durable Stop/settle；四个 ACP runtime caller 改走 ACP owner 与共享 provider session-stop helper，保持严格/仅日志错误策略；**保留** boot 时的 pre-C4 sweep（含 `readiness` marker，E5）；Claude readiness 加 `process_group(0)` 与超时/取消时的组 kill；ACP catalog 加 30s 总时限（Q5） | C4.2、C4.3 | 无 | 保留 pre-C4 boot sweep（见 boot 矩阵） | 无 | 无 | 同左（boot sweep 除外） | ≈ −700/+140，测试 +120 | L2：删除生命周期机制 |

  **C4.4 accepted containment limit (N1).** Native readiness/catalog process-group cleanup does not catch a descendant that escapes with `setsid` and redirects its descriptors. It also cannot reclaim a probe that outlives an abruptly killed kernel. These probes have no session/MCP token; stronger containment remains outside C4.4. Preserve this limit in the PR.

  **C4.4 F/G corrections.** Pipe stdin controls (including Held credential release) wait for the helper's `Started` report, after the helper releases its inherited pipe reader, using #3153's authenticated startup notification and existing Stop deadline without holding the scope gate needed by Stop or lifecycle reports. Pre-Started writes wait and then proceed; they do not permanently tear the pipe. ACP and Claude share the boot-owned pre-C4 absence proof; a failed sweep keeps boot running but fences strict ACP session stops for every swept session, including a missing durable claim. Runtime cleanup still never scans markers.


  超过约 800 的片是 C4.1–C4.3，各自理由如下；可前置、行为不变或有既有消费者的部分已拆出（C4.0a–C4.0d），
  断言不与它们的生产不变量分开，生成产物不计入手写预算。C4.1：rewind 加 owner 层（记录、owner 完成、`admission_gate`、boot、sweep）是原子核心，
  每片须有真实生产消费者（§3.2）；先选 rewind：一次性；秘密代理由同片的 Held wrapper 保护，能先验证记录、sweep、owner 完成、boot 与结算判决；它单独部署也安全，前提是同一片把仍原生的
  turn、canonical 与 compaction 启动接到同一个 `admission_gate`（第一轮 adversarial B3），只靠 `issue` 锁不够，因为被取消的 rewind 在锁释放后仍可能未结算；
  周期恢复已移到 C4.0d。另一种切法“把 rewind 推迟到 C4.2 与 turn 同片”会让 C4.2 超过 5000 预计行，所以不选。C4.2：turn 与 compaction 共用 `start_spawn`，
  必须同片切换；可先抽出的“唯一完成函数”在 live 原生路径上没有第二个调用者，提前抽出只是无消费者的移动，不拆；Planner 的代理分类函数先有 C4.1 rewind 这一个
  首个消费者，随它引入，不再依赖 worker 片。C4.3：ACP 的传输、release 与 receipt 是同一消费者。
  依赖与顺序：**C4.0d 是 C4 的第一片，已由 #2999（`36b22516c`）交付**，无前置、独立可部署，先于其余各片合入；它的生产消费者是既有 gate、forge 与 typed 终端，合入即修复它们的恢复缺口。
  C4.0a–C4.0c 无前置，可在 C4.0d 之后并行；之后 C4.1（依赖 C4.0a–C4.0d）→ C4.2（依赖 C4.1）→ C4.3 → C4.4。C4 各片不依赖 W 的任何片，
  W 各片也不放进 C4（§3.3）。规模比第三轮设计变化：C4.1 +25（stderr 结束标记与第五条 stderr 用例）、C4.1 再 +260（代理分类、表测试与 wrapper/release、凭据回归从 C4.2 提前）；C4.2 相应减少 120；
  原 C4.5a–C4.7 移到 W。若 Q2 选另一支，另加一片 executor 同宿主正向排空（见开放问题 Q2），位置在 C4.0a–C4.0d 之后、C4.1 之前。

- **受影响的既有测试（隐含的时序或同步完成假设会变，实现者不应因此停下；保留名字与断言意图，PR 列出改写清单）。**
  - C4.1：lib `claude_planner::rewind::mapping_tests::m1_mapping_edit_dry_run_accept_refuse_and_failure_table`（直接经原生 `run` 跑 `/bin/sh`，改为纯判词映射加一例真实 executor）、
    `claude_planner::rewind_tests::the_dry_run_and_the_next_spawn_carry_the_cut`；planner_harness_suite `claude_planner_replace::{the_cut_is_checked_then_rides_the_replacing_spawn_and_survives_a_restart,
    a_cut_the_cli_refuses_is_a_409_with_its_reason_and_nothing_changes}`（409 在结算之后才返回；stderr 判词改读汇点文件；假 CLI 的 check 分支 `tests/fixtures/claude_planner_fake/claude.sh:60-73` 不变）。
    原生启动接入 `admission_gate` 后：planner_harness_suite `claude_planner_session::*`、`claude_planner_compact::*`、`claude_planner_replace::*` 与 lib
    `harness::backend::claude_dispatch_tests::*`（记录为 NULL 时行为不变，只多一次 SQL 读；夹具 DB 需要 C4.0c 的列）。C4.0b 行为不变，但改动 ACP 的 boot
    世界，所以 runtime_suite `acp_planner_provider::*` 也在它的 replay 内。
  - C4.2（planner_harness_suite）：`claude_planner_session::assert_refused_before_ok`（`:410-441`，“无 marked PID”改为“无未结算 execution 且记录为空”）、
    `a_stalled_write_is_refused_before_ok`（`:444`，停顿改在 `accepted_bytes`）、`an_immediate_exit_is_refused_before_ok_with_no_outcome`（`:463`，退出经 Engine 观察，release 前退出即 ReleaseRefused）、
    `a_spawn_failure_leaves_no_instructions_file`（`:481`，Prepare 被拒）、`a_seal_after_spawn_stops_the_cli_before_any_input`（`:493`，fixtures 的 `after_spawn` 钩子 `session.rs:513-519` 移到“Spawned receipt 之后、release 之前”）、
    `a_wrong_version_receives_no_user_input`（`:521`）、`the_private_instructions_never_reach_a_cmdline`（`:581`，另验 intent 与 journal）、`assert_env_is_the_allowlist`（`:178`，可见 env 只少 marker）、
    `a_surviving_setsid_child_of_a_successful_exit_is_gone_before_turn_completed`（`:241`，证据改为 Drained）、`a_killed_cli_settles_failed_with_its_exit_status`（`:276`，status 取结算）、
    `a_cli_lingering_after_its_result_is_stopped_and_the_turn_completes`（`:344`）、`shutdown_interrupts_the_running_turn_and_refuses_the_next`（`:704`）；marker 用例 `the_stop_seam_fails_without_signalling_until_cleared`（`:625`）、
    `the_boot_sweep_ignores_the_seam`（`:679`）、`a_hung_proc_scan_fails_later_stops_at_once_until_it_returns`（`:801`）在 C4.2 改为只针对 boot 的 pre-C4 sweep，C4.4 随机制删除。
    `claude_planner_session_lifecycle::*`：`an_ignored_interrupt_is_ended_by_the_stop_timer_within_budget`（`:31`）、`a_cli_ignoring_sigterm_still_settles_by_settle_by`（`:424`，KILL 提前到 200ms）、
    `an_interrupted_cli_that_stopped_reading_stdin_settles_within_budget`（`:240`，写入停顿 → StdinTorn）、`a_cli_that_never_stops_writing_is_stopped_within_budget`（`:277`，不再反压，可能以 OutputGap 结束，失败原因断言可变）、
    `the_stop_timer_runs_from_the_interrupt_not_from_its_write`（`:390`）、`the_first_init_binds_the_row_and_a_reopened_session_resumes`（`:94`）、`a_failed_bind_is_retried_on_the_next_turn`（`:515`，6.5s 睡眠）、
    `a_shutdown_or_seal_during_the_version_check_mints_nothing`（`:572`，version 检查仍原生）。`claude_planner_credentials::{boot_after_a_crash_records_interrupted_or_re_drains_and_keeps_settled_outcomes,
    the_codex_daemon_down_at_boot_still_recovers_claude, a_superseded_claude_rows_live_process_is_gone_after_boot_and_delete}`（CLI 今天随 kernel 被扫掉，改为活在 supervisor 中直到 boot Stop 与结算，
    “boot 后已消失”改为有界等待结算；需要 C4.0b 的 supervisor 活过 kernel；`:194`、`:342` 的 1s 睡眠保留）；`claude_planner_retirement::*`（`:212`、`:307`、`:434` 的 2s 睡眠，改为 owner 专用 stop）；
    `claude_planner_{compact,shutdown,hibernation,approvals,live_replies,session_results,create_retry,wiring,availability}::*`、`planner_turn_budget`、`planner_timeout`、`track_mail_claude`、`planner_harness_boot_recovery`。
    lib：`claude_planner::spawn_tests::{argv_is_exactly_the_spawn_contract, the_ambient_allowlist_drops_codex_openai_and_rust_keys, the_spawn_env_disables_claude_auto_memory}`（argv 前加 wrapper，env 分为 intent 与载荷）、
    `claude_planner::driver_tests::settle_by_fits_the_harness_interrupt_budget`、`claude_planner::turn_slot` 的 `bounded_gives_up_at_settle_by_armed_while_pending`、`claude_planner::session::interrupt_tests`（`/bin/cat` 子进程改为 `EngineStdio` 测试替身）、
    `harness::backend::claude_dispatch_tests::*`（含 canonical 的 `canonical_claude_start_checkpoints_before_first_line`、`canonical_claude_stop_waits_for_exact_driver_end`、`canonical_claude_flush_failure_holds_one_attempt_without_second_spawn`）、
    `operation::planner_harness_start_adapter::canonical_lifecycle_tests::claude_recovery::*`（8 条）。源码扫描 `server/lib.rs:790-800` 的 `claude_planner_boot_revokes_before_the_mcp_listener_starts` 依赖
    `lifecycle::boot(…)` 字符串：pass A 放进该函数，字符串不变，只改其说明。`harness_turn_start_invariant`（`tests/cases/harness_turn_start_invariant.rs:44-71`）的 `.start_turn(` 文件名单不变。
  - C4.3：runtime_suite `acp_planner_provider::*` 全部，尤其 `acp_planner_lifecycle::*`（20s/100ms/6s/10s 窗口，spawn 经 Engine 更慢）、`acp_planner_approvals::acp_process_exit_withdraws_a_held_request_after_opening_it`（退出改经输出终止）、
    `acp_grok_provider::grok_runs_two_planner_turns_and_loads_the_same_native_session_after_restart`（进程活过 kernel，boot Stop）、`acp_settlement::*`（写者竞争改在 WriteStdin）、`acp_planner_{prompt_error,start,transcript,checkpoint,authentication,permission_creation,cli}::*`；
    lib `acp_planner::process::{setup_tests, pump_tests::*}`（python3 真实 pipe 改为 `EngineStdio`）、`acp_planner::boot_tests::*`（含 `authentication_tests`、`receipt_tests` 的 25ms 轮询；macOS 专用
    `acp_boot_turn_refuses_before_spawn_and_revokes_minted_token` 的拒绝原因改为 `ExecutionUnavailable` 且在 mint 之前）、`acp_planner::session::control_tests`；planner_harness_suite `planner_harness_boot_recovery`；
    History 恢复专属测试已退役，不再作为 replay 目标；api_suite `domain_api_suite::track_workspace_recycle`。
  - C4.4（E1–E5）：随机制删除 lib `claude_planner::stop_tests::*`、`planner_process` 的运行期单元测试与上列 C4.2 的三条 marker 用例；ACP macOS 专用 `acp_boot_readiness_refuses_before_spawn`、`acp_boot_catalog_refuses_before_spawn`
    （不再因缺 `/proc` 拒绝，macOS 上的 readiness 行为变化写进 PR）；保留 boot-owned `planner_process` 的 exact marker、recycled PID identity、unlistable `/proc` fail-closed 与 scan deadline 小型测试集；`acp_planner_catalog::acp_catalog_cancellation_stops_marked_descendants_*`（`:179-235`）改为按进程组断言。macOS readiness/catalog 不再因缺 `/proc` 在 spawn 前拒绝；按真实 ACP 协议结果判定。
  - C4.0d：lib `execution_boot::port_contract`（boot `recover` 不变，节拍在其后起）；全局 recover 会周期重驱 gate、forge 与 typed 终端的未完成行，所以 replay
    runtime_suite `execution_hooks::*` 与 `c2b_`、mcp_integration_suite `c2b_`、lib `task_gate_run::tests`、`task_verify_adapter::verdict_tests`、`owned_parked::tests`，
    forge_action_adapter 与 forge_suite 全部（断言不变；重复 `drive` 只置 dirty，不应改变任何时序断言，若有用例依赖“无人重驱”即为它钉住的缺口，PR 列出）。
- **新测试与 mutation（`c4_` 过滤器，名字是实现时注册的新过滤器）。** 上文各处已点名的测试按所在片注册；此处汇总。
  C4.0a：`c4_payload_key_rejects_non_identifier`、`c4_payload_key_rejects_reserved_and_internal_names`、常量键表测试。
  C4.0d：`c4_recovery_tick_settles_drained_gate_before_deadline`、`c4_recovery_tick_redrives_gate_retire_after_store_error`、`c4_recovery_tick_redrives_forge_retire_after_store_error`、
  `c4_recovery_tick_redrives_typed_terminal_after_store_error`、`c4_recovery_tick_is_idempotent_with_live_slots`、`c4_recovery_tick_retries_after_pending_read_error`。
  C4.1：`c4_rewind_has_one_engine_construction`、`c4_rewind_refuses_unavailable_executor`、`c4_rewind_verdict_waits_for_settlement`
  （leader 退出、setsid 子孙留在 cwd：Drained 之前无判词）、`c4_rewind_timeout_stops_through_engine`、`c4_dropped_rewind_request_is_stopped_by_sweep`、
  `c4_boot_stops_recorded_provider_execution`、`c4_boot_pass_a_precedes_listener_and_failure_is_covered_by_sweep`、`c4_orphan_execution_without_record_is_retired`、
  `c4_provider_admission_is_exclusive_per_session`（同事务 NULL→记录 CAS 与 `admission_gate`，竞争者用不同 execution alias 仍被拒）、`c4_retire_only_after_settlement`、
  `c4_canceled_rewind_blocks_native_turn_and_compaction_until_settled`（C4.1 边界上的“取消 rewind 后立即发原生 turn/compaction”，第一轮 adversarial B3）、
  `c4_planner_lost_post_commit_drive_recovers_by_tick`（③ 提交后 drive 丢失，无 executor 事件、无重启：C4.0d 节拍使 Stop、结算、④ 与秘密清除最终都发生）、
  `c4_planner_committed_retire_recovers_by_tick`（④ 已提交而 drive 丢失：Retire 与秘密清除完成）、`c4_stderr_sink_is_bounded_and_reports_overflow`、
  `c4_stderr_overflow_survives_stop`、`c4_stderr_stop_at_cap_is_never_untruncated`、`c4_stderr_overflow_survives_descendant_holding_fifo`、`c4_stderr_exact_cap_is_not_overflow`、
  `c4_stderr_unreadable_never_blocks_complete`、`c4_stderr_sink_cleared_after_driver_loss`、
  `c4_repeated_rewind_launches_with_distinct_execution_aliases`（同 session 连续两次 rewind，真实启动并完成第二次）、`c4_proxy_value_classification_table`（无 userinfo、含 userinfo、无法解析、空值）、`c4_rewind_proxy_override_order_is_preserved`（覆盖顺序在分类之前解析）、`c4_rewind_credentials_never_enter_intent_or_journal`（真实 wrapper 收到秘密代理，intent 与 supervisor journal 均无凭据）、`c4_inventory_err_never_drives_stop_retire_or_clear`、`c4_consumer_inventory_real_store_unclassified_rows_and_scan_cost`。
  C4.2：`c4_rewind_then_turn_launches_with_distinct_execution_aliases`、`c4_turn_compaction_turn_launches_with_distinct_execution_aliases`
  （同 session 的 rewind→turn、turn→compaction→turn，每一步都真实启动并完成）、`c4_claude_turn_has_one_engine_construction`、`c4_claude_turn_refuses_unavailable_executor`、`c4_credentials_never_enter_intent_or_journal`
  （含带 userinfo 的代理值）、`c4_release_precedes_first_protocol_byte`、`c4_payload_leaves_protocol_bytes_for_provider`、`c4_boot_never_resends_payload`
  （Held、Releasing 两例）、`c4_boot_stops_running_turn_and_records_interrupted`、`c4_turn_verdict_requires_drained`、`c4_lingering_cli_after_result_is_stopped_and_completes`、
  `c4_refusal_before_ok_settles_before_retire`、`c4_output_gap_fails_the_turn`（完整与不完整末尾输出两例）、`c4_torn_stdin_stops_the_turn`、
  `c4_next_turn_refused_while_previous_unsettled`、`c4_lost_driver_completes_owner_turn`（驱动 task panic、abort 各一例，分别发生在 release 前、turn 中、
  结果之后：sweep 结算后调用 owner `complete`，`active` 清空、`TurnCompleted` 发布、`join_driver` 不再 Conflict、下一 turn 无需重启即可启动）、
  `c4_lost_compaction_driver_fails_maintenance_without_fabricated_turn`、`c4_restart_between_settlement_complete_and_retire`（三个崩溃点各一例：
  `complete` 幂等重跑、结果不被改写、最终 ④）、`c4_outcome_write_failure_keeps_record`（`complete` 返回 Err：记录保留、下一轮重试、无用户操作也收敛）、
  `c4_track_stop_leaves_gate_and_forge_running`（repoint 期间同 track 的 gate 不受影响）、`c4_every_turn_end_drives_retire`、`c4_unknown_execution_keeps_card_refused`、
  `c4_shutdown_deadline_includes_provider_cleanup`、`c4_track_stop_settles_within_quiesce_deadline`、`c4_interrupt_keeps_protocol_grace`、
  `c4_concurrent_complete_same_turn_is_idempotent`（同一 turn 一路经 `admission_gate` 内联完成、一路经 sweep）、`c4_complete_retry_after_active_cleared_before_record_clear`、
  `c4_unconfirmed_then_settled_with_new_turn_attempt`（Unconfirmed 之后发起新 turn 被拒，结算后补做 `complete` 与 ④ 再放行）。
  C4.3：`c4_consecutive_acp_turns_launch_with_distinct_execution_aliases`（同 session 连续两次 ACP turn，真实启动并完成第二次）；ACP 的同名对应物，加 `c4_acp_registration_env_only_in_payload`、`c4_acp_env_partition_preserves_override_order`、`c4_existing_bad_acp_key_refuses_before_mint`、
  `c4_acp_setup_failure_settles_before_retire`、`c4_lost_acp_driver_revokes_and_settles_receipt`（driver 丢失后 credential 撤销、receipt 以 unknown 结算、
  approvals 围栏释放、下一 turn 可启动）。C4.4：`c4_claude_readiness_kills_group_on_timeout_and_cancel`（成功 leader 留下子孙、超时、取消三例）、
  `c4_acp_catalog_total_deadline_kills_group`、`c4_pre_c4_boot_sweep_still_runs`（两种 boot 均覆盖 readiness marker）；四条 ACP runtime 路径（shutdown replay、historical predecessor、displaced historical、compensation）必须返回前结算 session 的 Engine execution。mutation 验证 readiness 组 kill、catalog 总时限与一条 ACP runtime Stop。
  上述 C4.1–C4.3 的四条连续启动回归必须走真实 **Engine→SessionExecutor→executor**（可用假 provider CLI，不以 mock executor 或仅 SQL 准入代替）。
  每条都覆盖下一次准入发生在前驱已结算、owner 完成且 ④ 提交之后、executor 实际 Retire **之前**与 **之后**两种时序；前者延迟前驱 Retire，
  后者确认 tombstone 已写入。两种时序各加跨 kernel 重启的例子（supervisor 活过 kernel），并加已结算前驱跨 supervisor 重启的例子，
  验证热记录或 tombstone 重建的旧 alias slot 都不阻塞新 execution。断言同 session 的各 execution_id 与 alias 不同、确有下一次 Launch 与 provider 运行证据、
  前驱未结算时仍被 CAS + `admission_gate` 拒绝；不把成功写入 execution 行当作启动成功。
  mutation 只在实现 PR 内各做一次，先按名字写出完整的预测红集，再运行容纳它们的整个 target 或过滤器，并与实际红集逐一比对：准入提交前释放、凭据进 intent、
  boot 不 Stop Observing、`Exited` 即判 `ProviderTerminal`、结算前请求 Retire、前一 execution 未结算仍准入、原生启动绕过 `admission_gate`、
  sweep 在 owner `complete` 之前做 ④、`EXIT_WAIT` 后只在 `Exited` 时 Stop、`validate_key` 放行 `neige_*`、stderr 汇点去掉溢出标记、
  结果判定不看 `stderr.closed`（预测 `c4_stderr_stop_at_cap_is_never_untruncated`、`c4_stderr_overflow_survives_stop`、`c4_stderr_overflow_survives_descendant_holding_fifo` 与 `c4_stderr_missing_closed_never_means_complete` 变红；物理用例通过真实 wrapper 的受控前缀中断 EOF 发布者，稳定复现缺少 closed 的窗口）、track stop 改用通用 quiesce、忽略输出缺口。
  **恢复节拍的突变（修第三轮 boundary 的 mutation 说明）**：在 C4.0d 的 PR 里去掉周期 `Engine::recover`（只保留 boot 的一次调用），预测红集恰为 C4.0d 的
  六条：五条推进用例（`c4_recovery_tick_settles_drained_gate_before_deadline`、`c4_recovery_tick_redrives_gate_retire_after_store_error`、
  `c4_recovery_tick_redrives_forge_retire_after_store_error`、`c4_recovery_tick_redrives_typed_terminal_after_store_error`、`c4_recovery_tick_retries_after_pending_read_error`）
  断言的推进不再发生；`c4_recovery_tick_is_idempotent_with_live_slots` 只因它的前提断言（节拍至少运行两轮）失败而变红，它的“无第二驱动循环”断言不由这一突变
  证明，而由 Engine 既有的单 slot 合同（`engine.rs:60-66`）承担，C4.0d 不另做 Engine 突变。C4.1 的 PR 若重做同一突变，预测红集在这六条之外再加
  `c4_planner_lost_post_commit_drive_recovers_by_tick` 与 `c4_planner_committed_retire_recovers_by_tick` 两条，不含任何 worker 用例（worker 用例属 W）。
- **gate 目标（每片必须列出驱动被迁移路径的全部 target；“加”表示在前一片列表之上追加，前一片的全部 target 继续 replay）。**
  公共 gate G 不变（ratchet、contract、`--quick` 各一次，加一次拒绝空选择的聚焦 replay）。replay 列表：
  C4.0a：`forge_action_adapter`、`forge_suite`、`mcp_plugin_forge_action` 全部与 lib `operation::forge_action_adapter::tests`、lib `operation::stdin_payload`；
  C4.0b：`planner_harness_suite` 全部、`planner_harness_adapters`、`planner_card_reset`、`runtime_suite acp_planner_provider::`、`api_suite domain_api_suite::`，
  lib `claude_planner::`、`acp_planner::`、`harness::backend::claude_dispatch_tests`、`operation::planner_harness_start_adapter::canonical_lifecycle_tests`；
  C4.0c：`-p calm-truth` 迁移与 CAS 测试、`runtime_suite migration_suite::`；
  C4.0d：`-p calm-truth` 迁移测试、`runtime_suite migration_suite::`、lib `execution_boot::`、`runtime_suite execution_hooks::` 与 `c2b_`、`mcp_integration_suite c2b_`、
  lib `task_gate_run::tests`、`task_verify_adapter::verdict_tests`、`owned_parked::tests`、`forge_action_adapter`、`forge_suite` 全部与 `c4_recovery_tick` 过滤器；
  C4.1：C4.0b 的全部（ACP 除外），加 lib `provider_execution::`、`execution_boot::port_contract` 与 `c4_` 过滤器（C4.0d 的列表由它自己的片 replay，C4.1 不重复）；
  C4.2：C4.1 的全部（`planner_harness_suite` 全部已含 `track_mail_claude`、`planner_turn_budget`、`planner_timeout`）；
  C4.3：C4.2 的全部，加 `runtime_suite acp_planner_provider::`、`planner_harness_suite planner_harness_boot_recovery::`、
  `api_suite domain_api_suite::track_workspace_recycle`、lib `acp_planner::`；C4.4：C4.3 的全部。History 恢复专属测试已退役，不再 replay。W 各片的 gate 目标见 §3.3。
  实现 PR 以本片改动的**生产符号**做调用者审计，不只按构造器名：C4.1/C4.2 `start_spawn|prepare_edit|join_driver|finish_turn|ClaudePlannerHost|claude_planner_session_fixture|claude_planner_stack_fixture`，
  C4.3 `AcpPlannerHost|acp_planner|settle_receipt`，C4.0d `ExecutionEngine|execution_boot::recover|Engine::recover|pending`，C4.1 起另加 `proxy_value_is_secret|base_env`；搜索范围
  `crates/calm-server/{src,tests}`、`crates/provider`、`crates/calm-exec`、`crates/calm-truth-test-harness`。命中的 target 都必须在该片列表里，结果写进 PR，
  不做成永久名单。
- **开放问题。** Q1、Q2 已由用户决定：无原生回退；Unknown 失败关闭到宿主重启。Q3–Q8 已由 Planner 决定，已写入上文对应处。下文只陈述它们对 C4 各片（C4.0a–C4.0d、C4.1–C4.4）的效果；对 task worker 的效果属 W，由 W 设计单独陈述（§3.3 WR7）。
  - **Q1 executor 不可用（用户已接受）**（macOS 有限构建，README `:131-133` 已写明 registered execution 只在 Linux；或升级前的 supervisor）。推荐支（设计已按此写）：
    失败关闭，不留原生回退。Claude/ACP Planner turn、rewind、compaction 在 mint 与准入之前返回 `ExecutionUnavailable`；
    readiness 也据此报告不可用，UI 与 API 错误写明原因与出路（macOS：此构建不支持 Planner；升级前 supervisor：重启 supervisor 或升级）。C4.0d 不改变这一点：
    没有 Engine（未配置 kernel client key，`execution_boot.rs:12-14`）时不起节拍；Engine 存在而 executor 不可用时，节拍的 `recover` 记日志、下一轮重试，
    既有 gate、forge、typed 终端的 `ExecutionUnavailable` 拒绝不变。task worker 在 W 之前仍是 Legacy PTY，C4 不改变它们。
    这是 macOS 上现有 Planner 功能的回退，需要用户明确接受，不能以“文档已写只支持 Linux”代替。另一支（保留原生回退）会保留 marker 机制与
    第二套生命周期，C4.4 无法删除、C5 无法以执行集合为准，规模与风险都不在本设计内；若选它，C4 需要重新设计，本片序不成立。
  - **Q2 同宿主 supervisor 崩溃（Unknown，用户已接受到宿主重启前失败关闭）。** 推荐支（设计已按此写）：接受“到宿主重启前不可恢复”。Unknown 保持失败关闭，永不按 PID 推断成功、
    永不自动 Retire；该 card 的下一次启动（自 C4.1 起包括 rewind，经 `admission_gate`）、以及依赖 C5 正向静默的该 track/area 破坏性
    操作（删除、回收、repoint）都被拒到宿主重启；UI 以单独的 Unknown 状态与“重启宿主”出路展示，不与普通 Busy 混同。今天 marker sweep 能在
    同宿主清理，所以这是用户可见的退化，需要用户明确接受。
    另一支（不接受时）：**executor 同宿主正向排空**，另立一片，Engine/executor 合同变更，不在 C4 之内设计。范围：supervisor 为每个 execution 建一个
    cgroup v2 子组作为新的 `scope_mechanism`（今天是 subreaper helper，`supervisor/execution/scope_helper.rs:84`；Drained 证明绑定该机制，
    `execution/mod.rs:145-149`），journal 记录 cgroup 路径；同宿主重启的 `open`（`execution/mod.rs:166-183`）对这类记录不再一律给 Unknown，而是
    `cgroup.kill` 后等 `cgroup.events` 的 `populated 0`，得到带 cgroup 证明的 Drained（executor 协议、journal 版本与 workspace-execution.md §6.4
    的 boot 规则都要改）；cgroup 委派不可用的宿主仍留在 Unknown，失败关闭。规模：设计约 1300 手写行（supervisor cgroup 放置与委派探测 +450、
    重启对账 +250、journal 与协议版本 +150、Engine 侧事实映射 +100、真实 cgroup 测试 +350），按 C2b/C3 实测比外推为 2200–3400，L2；这只是范围轮廓的估计，
    不是经过验证的交付估计。**这一支在被接受为排空证明之前，还须由独立的 L2 executor owner 设计回答以下各点**（第二轮 boundary N3）：
    ① 原子成员关系：目标在 fork 之前就属于该 cgroup（`clone3` 的 `CLONE_INTO_CGROUP`，或 helper 先加入 cgroup 再 exec 目标），不存在“先 fork 后迁移”的逃逸窗口；
    ② 逃逸与委派权限：provider 与 supervisor 同 uid 时可写同一委派子树的 `cgroup.procs`，能把自己迁到兄弟组；须以 `nsdelegate` 与 cgroup namespace、
    或把证明范围扩到整个委派子树来封住，否则 `populated 0` 不证明全部子孙已退出；③ durable 且不复用的身份：只记路径不够（同名可重建），journal 须记 cgroup 的
    inode/`cgroup.id`（经文件句柄比较）并在重启时核对；④ `cgroup.kill` 失败、`populated` 读取错误、委派探测失败与重启竞争（mkdir 与 journal 写入之间崩溃、
    孤儿 cgroup、重启时组已被外部删除）各自的结论，任何不确定都留在 Unknown；⑤ supervisor 丢失后输出不完整：读者线程随 supervisor 退出，
    `output_end` 照今天记 `Incomplete{executor_restarted}`（`supervisor/execution/mod.rs:189-200`），排空证明不证明输出完整，消费者照既有规则处理不完整输出；
    ⑥ 旧记录：subreaper helper 机制下的既有记录与升级前记录**保持 Unknown**，只有新机制且通过 ①–④ 的记录可以得到带 cgroup 证明的 Drained，不按 PID 推断，
    不把 provider 政策带进 Engine。片序：它不依赖 C4.0a–C4.0d 的代码，可与它们并行开发；**合入**须在 C4.1 之前（自 C4.1 起 Unknown 的 rewind
    就会经 `admission_gate` 封住整张 card）；C2b/C3 的 gate 与 forge 消费者同时受益。C4.0d 不受 Q2 影响：Unknown 行在每轮 `recover` 中照 Engine 规则
    等待，节拍不推断、不 Retire。Q2 对 typed worker 终端的效果属 W。
  - **Q3（已决定）**：有界 stderr 汇点与显式溢出结果，见“stdout 与 stderr、输出窗口”；stdout OutputLog 的磁盘上界是已接受的残余并另立 issue。
  - **Q4（已决定）**：接受 Engine 的 200ms STOP_GRACE（关闭、hibernate、track stop），保留 provider 协议的 interrupt 宽限，以真实进程清理测端到端时限，见“计时”。
  - **Q5（已决定）**：ACP catalog 留在项目 cwd，作为限定范围的 C2a 短读例外，加 30s 总时限与组回收，见“不能成为消费者的入口”。
  - **Q6（已决定）**：owner 自建 card 的 Legacy PTY（`claude-create`、`codex-create`、Legacy `terminal-create`、`claude-restart`、`claude_auto_resume`）
    不在 C4；C5 保留它们的 Legacy 检查（含升级前的 worker 行），不以执行集合为空证明静默。第三轮之后 task worker 本身（terminal-worker、claude-worker、
    codex viewer）也不在 C4：它们在 W 之前同样是 Legacy PTY，C5 以同样方式保留其检查（§3.3）。
  - **Q7（已决定）**：共享 Codex daemon 留在 Engine 之外，C5 保留 Codex 线程级检查，永不伪造 track。
  - **Q8（已决定）**：只有非秘密代理值进 intent；带认证的代理值走 Planner 的释放载荷，见“凭据与 release”。C4 的带秘密 consumer 都有载荷通道；
    没有载荷通道时在准入前拒绝的规则只作用于 worker，属 W。
- **架构五点与永久成本。** 抽象边界：进程生命周期只归 Engine，周期恢复只归 Engine 的组装 owner（`execution_boot.rs`，C4.0d），Engine 内无 consumer 或 provider
  分支；记录、sweep 与 `admission_gate` 的 SQL 只在 calm-truth；协议编解码留在 provider，calm-server 只换字节流；
  owner 规则（判决、outcome、receipt、撤销）只在各 provider owner 的 `complete` 中，通用 sweep 只按 consumer 键调度它。重复逻辑：载荷编解码与键合同一处（C4.0a）；
  全局恢复一个节拍（C4.0d），各 owner 不各自调用 `recover`；驱动者收尾与丢失驱动者的恢复是同一个 `complete`；Planner 两个 provider 共用一个 owner 层、
  `admission_gate`、`EngineStdio` 与代理分类函数（`execution_environment.rs`，C4.1）；Engine 进度只由既有 `recover`/`pending` 推进，不另建状态机；
  C4.4 删除 marker 的运行期第二套生命周期。
  硬编码：consumer 键、alias 前缀与 owner 完成实现由各 owner 注册，通用层不按 provider 名字分支；
  不给 daemon 或 readiness 伪造 track。外部合同事实：进程事实取自 executor 回复与 calm-truth 的结算，不加持久化的阶段镜像；
  provider session 取自 init 记录（`driver.rs:494-532`），不读 `/proc`（唯一例外是保留的 boot 时 pre-C4 sweep，它只针对升级前没有 execution 的原生进程）。
  永久成本：一列（`worker_sessions.provider_execution_json`）与一个 pending 部分索引；
  `execution_boot.rs` 两条 consumer 注册与两个 owner 完成实现（新增 Planner provider 要加 consumer 注册与 owner `complete`）；一个全局 30s 恢复节拍
  （每轮一次走部分索引的 SQL 读加每个未完成行一次 executor inspect，不随历史增长）；一个 Planner 30s sweep（每轮一次记录读）；
  每个 provider turn 一行 execution 并在完成时 Retire；`c4_` 测试随所在 suite 每个 CI workflow 各跑一次；新增或改名测试不改任何名单，
  不加调用者或测试名单。worker 一侧（决策树、观察端口、排空、reaper 的两次新查询）的边界与成本随 W 移到 §3.3。

**C0 保留清单。** 删除边界按私有路径的依赖划定，不按类型整块删：`stage_private`（`execution/preparation.rs:198-201`）、
唯一的 `release_private`（`execution/private.rs:8-37`）与 transfer 身份/nonce/root 资格检查（同文件 65-132）、
`RegisteredRoot`（`execution/directory.rs`）、`PreparedSource::Private(Box<AttachRegistration>)` 及其 ExternalDir 形状、
`HeldSourceObjects` 的 FD 接收、不可变 replay、先持久化再发布、独占 ExecutionId、durable settle 之后才释放、Control/Stop、
Inspect/Watch/Retire 与 tombstone。`tests/execution_private.rs` 与覆盖这些不变量的 registration 测试保留。

**C0 兼容规则。** 删除 wire 枚举变体会改变 bincode 序号，所以每个删变体的片都把 `EXECUTION_ENVELOPE_VERSION` +1，session 与
supervisor 同片。被接管的升级前 supervisor 握手不匹配时按 E2a 的既有合同处理：`Engine::available()` 为假，typed 创建得到
`ExecutionUnavailable`，启用需重启 supervisor（升级说明照写）。持久事实不跟随 wire 版本：C0a 把 receipt 的持久合同从 wire
envelope 拆出，新增持久合同常量（仍为 4），`JOURNAL_VERSION` 不变，持久 JSON 形状不改；某片若必须改持久形状，同片提升持久
合同并带旧形状解码测试。C0a 同片扫完 `EXECUTION_ENVELOPE_VERSION` 的全部持久用途，逐处改为持久合同常量，不能只改两处比较
而让写者写入新的 envelope 号：写者 supervisor `execution/private.rs:124`、`execution/attachment.rs:84`；读者/比较
`execution/mod.rs:250-257`、`execution/directory.rs:19-22`、session `control/execution/attachment_client.rs:36,74` 与
`control/execution/directory_relation.rs:48`（后两者在 C0b/C0d 删除前的中间片仍在）；session `control/execution.rs:392`
的 `required_contract` 按其语义归属（持久 receipt 合同还是 wire 握手）逐处判定，结论写进 PR；握手 `envelope_version`
（`execution/mod.rs:362`、`lib.rs:595-601`、`control/execution.rs:410`）继续跟随 wire 版本。PR 附 `rg EXECUTION_ENVELOPE_VERSION`
的前后结果。验收夹具按当前恢复合同拆分（`calm-proc-supervisor/src/execution/mod.rs:186-193`、workspace-execution.md
§6 的 boot 规则与“同宿主残留 Unknown”），以升级前 v4 状态目录启动新 supervisor：① 同宿主重启（boot id 不变）时已 Spawned、未结算的 Private
执行恢复为 Unknown，保持失败关闭，不可 Retire、不释放；② 宿主 boot 改变时同一执行判为 NoChild → settle → 释放 → tombstone；
③ 已 durable settle 的执行正常重放、释放并留下 tombstone。不把同宿主重启后直接结算当作成功标准，也不新增人工 NoChild 出口。
若 Planner 希望只要求一次 supervisor 重启，C0a–C0d 在同一发布中落地。

**C1 锁与持有区间。** 两把既有锁：全局 drive mutex（`OperationRuntime::lock_for_track_delete`）与每 track 的
`track_delete_locks`，顺序 card → drive → track → Planner runtime 的 `inner.issuance`（`server/state.rs:112-130`；track 持有者在 track 下停止
runtime 或做生命周期写入时取 issuance：shutdown 先置 `shutting_down` 并发 shutdown 信号再取 issuance，`run_loop.rs:841-848`；
`lock_if_idle` 持 issuance 穿过生命周期写入，`run_loop/hibernation.rs:9-10`），任何入口都不持 track 去等 drive 或 card，也不持
issuance 去等 track。围栏放在每个入口自己的“检查 → 写下 clean 可见占用事实”区间上，不依赖调用者是否持有 drive。
“clean 可见占用事实”指 clean 在 track 下复查时读取的同一批事实：`worker_sessions.state ∈ {starting, running,
turn_pending}`、未结算 intent/执行、checkout lease。

| 入口 | 持有区间 |
|---|---|
| clean 计时器 | 无锁 SQL 采样 → 取 drive → 取 track → 在两者下复查 `closed_at` 未变、checkout Free、执行已结算、无 clean 可见占用事实 → 释放 drive（只持 track）→ 在 `spawn_blocking` 中 `git clean -fdX` 并记入 `cleaned` → 释放 track。drive 只覆盖复查这几次 DB 读取，`git clean` 期间不阻塞全局 operation 驱动 |
| 经 OperationRuntime 的启动（终端创建与 `terminal_adapter::prepare`、worker 准备、planner-harness-start/restart、typed admit） | 由 `drive()` 在 drive 下驱动时：对 track 键只用 try-lock，从自身检查开始持有到同事务写下占用事实（Starting session、未结算 intent、lease）提交为止；争用时返回 Busy 由驱动重试，不在 drive 下等待 `git clean` |
| boot 恢复（`apply_recovery_item` → `drive_one`，持 drive） | 同一 adapter 的同一 track 围栏与同一持有区间；在 drive 下只 try-lock track。争用时的 Busy 保持可重试，由拥有 track 等待任务的 `retry_after_track` 在锁释放后唤醒驱动，不得记为 Stuck。不以“计时器跳过启动 tick / 十分钟宽限”作为 boot 恢复先于 clean 的顺序保证 |
| 既有 Planner runtime 的下一 turn（Idle/TurnCompleted → Issuing：排队消息、自动观察、pending rewind 的重新 issuance、`retry_issuance_now`，全部经 `maybe_issue_turn`，`run_loop.rs:3074`；同样在 issuance 下向 provider 发命令的 `run_loop/{compact_command,durable_command,replace_command}.rs` 逐个判定是否会在 cwd 内引起 agent 写入，会则加同一围栏；实现时扫描绕过它们直接发送 turn/start 的调用点，清单写进 PR） | 在取 issuance 之前、不持 card/drive/issuance 时等待 track（与该 runtime 的 shutdown 信号 `select`，shutdown 时放弃并保留队列）→ 取 issuance → 既有 archive/shutdown/live-carrier 复查 → 转 Issuing 并持久化 snapshot（落为 `turn_pending`，即 clean 复查排除的状态；若实现发现持久形状不同，就在同一处写 clean 可见事实）→ 释放 track → 发送 turn/start。clean 先赢时 issuance 等到 clean 结束再照常发出；在 track 下等待 runtime 退出的持有者（删除的 quiesce、`claude_update` 的 stop 等，实现时逐个列出）必须先发 shutdown，等待者因此退出，不死锁 |
| 直接恢复/重连（`harness/mod.rs:245`、`routes/planner_recovery.rs:47`、`claude_update`） | 照旧在不持 drive 的情况下等待 track，覆盖检查到 spawn |
| reopen 路由 | 不持 drive，等待 track，覆盖 `closed_at` 写入；clean 的复查因此只会看到“仍关闭”或“已重开” |
| 不经 operation 的 Legacy 启动（WS 终端 ensure、`claude_auto_resume` 等，实现时按 spawn 点调用者扫描，清单写进 PR） | 不持 drive，等待 track，覆盖检查到写下占用事实；各自持 track，因此不依赖 boot reconcile 与计时器首轮的先后 |

保留“idle Planner 不阻止 clean”的既有政策（`ports.rs:65-67`）：不选“把所有 Idle 活 owner 都当作阻止 clean 的事实”，
因为那会让每个留有 idle Planner 的关闭 track 在 hibernation 移除 owner 之前都不被清理，是政策变化；围栏只要求 idle → Issuing
这一跳与 clean 互斥。

残余风险写进 PR 并由 Planner 接受：锁不跨进程崩溃；server 崩溃后孤儿 `git clean` 子进程可能继续运行，时长没有上界，期间
重启后的启动不受围栏保护。这与 §4.2 已启动 teardown 子进程跨崩溃存活属同一残余风险类，留给 follow-up #2862
（PDEATHSIG / child lifetime semantics）闭合，本计划不新增机制。不改 clean 的 SQL 采样。

**C5 设计决定。** 核实基线 main `0a172a409`（只读盘点 c5-inventory 的同一基线）；路径前缀同 C2b，另 `workspace/` 指
`crates/workspace/src/`、`tests/` 指 `crates/calm-server/tests/`。只是设计，不声称已实现。C5 行原文假设 O1 成立后按 owning track
的占用即覆盖全部破坏性 effect；盘点在代码中找到与之矛盾的五处（C5-D1…C5-D5），第一轮设计评审又找到 forge probe 的准入缺口（C5-D6），第二轮找到 repoint 目标绕过 O1 的缺口（C5-D7），逐条决定如下。为不与 §3.1 的 D1–D8 重名，编号加 `C5-` 前缀。
O1 已在 main 实现（`truth/db/sqlite/track.rs:123` 的 `admit_attached_workspace_tx`，`server/routes/tracks/allocation.rs:101`
在创建写者内调用）；§0 第 4 条与 §3.1 中“待接受、在 C5 之前修复”的写法是历史记录，不再是前置条件。

- **破坏性 effect 的范围。** 移走或删除执行可能在用的目录或其中文件的 effect：Track 删除的 recycle（`server/routes/tracks.rs:2992`
  → `runtime/recycle.rs:83`）、Area 删除的批量 recycle（`server/routes/areas.rs:542` → `runtime/recycle.rs:201`）、提交后的
  worktree/branch/refs sweep（`server/routes/tracks.rs:3126`、`server/routes/areas.rs:740` → `runtime/sweep.rs:113`）、repoint 回收
  旧 managed 目录（`server/routes/tracks.rs:2296`）、Area 容器的空目录 rmdir（`runtime/recycle.rs:251-253`）、已关闭 worktree 的 `git clean -fdX`
  （`server/workspace_service/ports.rs:113-138`）。
  restore 与自补偿、只删行的 Card/plugin 删除与启动补偿、materialization staging 不属于此类（C5-D2、C5-D4、C5c）。
- **C5c 删除入口审计（main `b14763934`）。** `server/routes/cards.rs::delete_card` 与
  `server/plugin_host/callbacks.rs::card_delete` 只删除 card/session/terminal 行、结算任务与释放 lease；REST 保留既有
  owner quiesce、Legacy launch 复查和 provider 生命周期，plugin callback 保留 manifest 权限检查与 terminal 清理。
  它们不回收 track workspace，不授予停止他 track 执行的权限。
  `plugin/src/host/lifecycle.rs::uninstall` 经 `server/plugin_host/adapter.rs::plugin_delete` 删除 plugin 行与关联状态，
  保留 plugin 自有 stop；后续只允许 `plugin/src/host/managed.rs::remove_managed_tree` 删除内核生成的 plugin tree，
  不触碰 operator tree 或 track workspace。启动补偿的 `terminal_adapter.rs::compensate_step`、
  `worker_cleanup.rs::compensate_worker_rows` 与 `planner_harness_start_adapter.rs::compensate_step` 只按原 operation
  的持久输出清理自己的 runtime/card/terminal（含 boot recovery 的同一路径）；child-track adapter 的补偿为空操作。
  这些入口不是 C5 workspace effect；本片不扩大其授权或生命周期。今后若增加 workspace 移动/删除，须另行决定围栏、
  正向证明与 owner 授权，不能从这里的行删除继承权限。
  **Planner 代理保留。** 本片只切换 typed 终端；Claude/ACP Planner 仍按 main 的 Starting/Running/TurnPending session
  代理阻塞 clean。issuance 在 `server/harness/run_loop/issuance.rs:369` 释放 Track 后才在 `:507-510` 调用 turn_start；
  canonical 首轮同样在 `server/harness/run_loop/canonical.rs:526` 释放后于 `:528` dispatch，compact 在
  `server/harness/run_loop/compact_command.rs:163` 释放后于 `:164-168` 准入；`server/harness/run_loop/issuance.rs:664-665` 明确由 durable
  turn_pending 排除 clean。独立后续切片负责延长 Track guard 到 Engine 占用提交（含 C5-D6.5 的 RewindCheck），在此前不删 Planner 代理。
  typed 终端没有此窗口：`server/operation/terminal_execution_create.rs:205` 取得同一 Track try-lock，`:235` prepare_tx
  同事务调用 `terminal_adapter.rs:309` 创建 session（`truth/db/sqlite/card_composite.rs:117`），再在
  `terminal_execution_create.rs:93-94` admit；`:248-249` 提交后才释放。clean 在 `server/workspace_service/ports.rs:146-150`
  取得同一 Track 后复核 execution 占用，守卫覆盖 Git effect。
- **正向证明。** 在该 effect 既有围栏（全局 drive 锁与 track 锁，Area 另加 area 锁）持有区间内的一次新读，得到：(i) scope 内
  `unsettled_executions` 为空（`truth/db/sqlite/workspace_execution.rs:174-199`：只看 `settled_at_ms IS NULL`，Held、Stopping、
  Unknown 一律算占用；不得改用 `pending`，`:103`）；(ii) C5-D1 的位置占用为空；(iii) 未迁移类型的 Legacy 检查照旧通过。今天
  Track 删除只有 quiesce 的快照（`server/execution_quiesce.rs:42-77` 只等它快照到的 id，不再读一次）与 recycle **之后**的写者复查
  （`server/routes/tracks.rs:2859`），recycle 之前没有直接证明；Area 同样（`server/routes/areas.rs:469`、`:615`）。C5 在第一次移动
  之前加这次读，写者复查保留。
- **C5 保证的不变量及其已接受的例外。** 经受围栏的入口准入的未结算执行，只要其根位于或等于目标，任何破坏性 effect 都不会
  运行，无论哪个 track 拥有它；回滚不丢失 live 目录（C5-D2：restore 失败的条目也不被 trash GC 删除）。不是绝对保证，下列例外
  逐条写明并写进 PR，由 Planner 接受：准入之后外部改名或替换链接（§3.1 D1）与 shell 自行 `cd`（C5-D1.4）；没有 execution 根的
  Legacy PTY 只有按 owning track 的 Legacy 检查（C5-D1.4）；C5 之前已准入、根在 `.trash` 或 objective checkout 内的执行与
  attached track，含 C5 之前已 repoint 到其中的 track（C5-D4、C5-D7）；进程内锁与 teardown 登记不跨 server 崩溃，崩溃后存活的孤儿子进程（`git clean`、teardown）属 #2862。

- **C5-D1（已决定）：占用按位置覆盖，不只按 owning track。** 事实：显式 cwd 与 track 的选择无关（`server/operation/terminal_adapter/target.rs:22-35`，
  显式值直接成为 `ExternalDir`），经 `terminal_cwd_or_track_workspace`（`server/operation/terminal_adapter.rs:221-228`，调用于 `:292`）
  写进终端行；typed 终端以请求的 track 作 `metadata.track`、以该 cwd 作 `source.root`（`server/operation/terminal_execution_create.rs:64`、
  `:66`）。track A 的 typed 终端因此可以在 track B 的 managed 目录或 worktree 内运行，而 `unsettled_executions(Track(B))` 看不到它；
  O1 只约束 track 选择的准入，不约束终端 cwd。准入时序也不闭合：typed 终端创建只 try-lock 请求 track 的锁
  （`server/operation/terminal_execution_create.rs:166` → `server/operation/driver.rs:79-85`），不取全局 drive 锁，B 的删除持有的锁挡不住它。
  决定：
  1. **位置占用。** 破坏性 effect 的占用 = owning scope 的未结算执行 ∪ 位置集合：`source.root`（`runtime/execution/mod.rs:36-56`，
     原样序列化在 `workspace_executions.intent_json`，写入见 `truth/db/sqlite/workspace_execution.rs:29-50`）**位于或等于**任一 effect
     目标根（managed 目录、track worktree 根；Area 为 Area 容器目录与全部成员的 worktree 根，C5-D5）的未结算执行，与它的 owning track 无关。比较复用 calm-truth 唯一的
     重叠函数 `paths_overlap`（`truth/db/sqlite/track.rs:104-117`：lexical 与 physical 各比一次，解析失败是错误，不当作不相交），
     `containing = false`。根在目标之上的执行（例如仓库根上 attached track 自己的终端或 Planner）不算占用：worktree 位于仓库根之内
     （`workspace/worktree.rs:205-210`），O1 对 worktree 也只拒绝“位于或等于”（`truth/db/sqlite/track.rs:119-122`），与 §4.2 接受的同仓库
     重叠是同一规则。SQL 在 calm-truth：新读者经部分索引 `workspace_executions_unsettled`（`truth/migrations/0170_workspace_executions.sql:16-17`）
     取**全部**未结算行的 `intent_json`，在同一 owner 模块逐行解码 `source.root` 后用同一个 `paths_overlap` 过滤；该函数改为
     calm-truth 内共享（O1 与新读者共用），不复制第二份路径比较，也不在 server 或 runtime 重写 SQL。
     **解码失败关闭。** SQL 不按 `json_extract(intent_json, '$.source.root')` 过滤，不写 `IS NOT NULL`，不用默认值、不跳过任何行；
     Rust 侧以只含 `source.root: PathBuf` 的必填类型逐行解码，缺失、`null`、非字符串或 JSON 结构不符都是错误：删除与 repoint 在
     任何 effect 之前返回 Conflict（带 execution id），clean 延后并记录，不当作“不在目标内”。该行的根未知，因此它阻塞**所有** scope
     的破坏性 effect，直到其 owner 结算它。`json_valid` CHECK（`truth/migrations/0170_workspace_executions.sql:6`）只保证 JSON 合法，
     不是 source 的模式保证，所以这条规则要写成合同。升级：`PrivateSource.root` 是必填字段，早于 execution 的持久化出现（#2826 引入该类型，
     #2831 才以 0170 建表），每个写者都原样序列化 `ExecutionMetadata`（`truth/db/sqlite/workspace_execution.rs:29-50`），intent 又不可变
     （0170 的 `workspace_executions_immutable` 触发器），所以已发布版本写不出缺 `source.root` 的 intent。这条规则在升级时没有代价，
     不需要回填或排空；手工改坏的行按上面的规则失败关闭。
  2. **他 track 的占用者只拒绝、不 Stop。** owning scope 内的执行照旧由删除的 quiesce Stop；位置集合中 owning track 不在 scope 内的执行，
     删除、repoint、clean 都在任何 effect 之前返回 Conflict（clean 为延后），目录、branch 与行保留。删除 B 不授予停止 A 的 card 的权限；
     该终端由其 owner 关闭后重试即可。
  3. **准入围栏，不是准入拒绝。** 显式 cwd 的终端准入（typed 与 Legacy 共用 `target::decode`，`target.rs:28`）另外 try-lock cwd 所在
     每个自有位置的 owner 锁，冲突即 `WorkspaceBusy`（可重试，同 C1 持有区间表中 typed admit 的规则）。**owner 集合**（同一
     `paths_overlap`，`containing = false`，cwd 的 lexical 与 physical 位置各求一次、取并集）在行删除之后、effect 完成之前也必须
     找得到 owner，否则提交之后、sweep 之前的窗口里 cwd 看不到 owner，准入照常通过（评审 B2：Track 删除在写者内删行，
     `server/routes/tracks.rs:2879-2884`，提交后才 sweep，`:3125-3131`；Area 同样，`server/routes/areas.rs:641-648`、`:738-744`）：
     - managed 根之下：按 Neige 自有布局 `<managed root>/<area_id>/<track_id>`（`workspace/materialize.rs:23`；recycle 自己也只认这一深度，
       `workspace/recycle.rs:13`、`:128`）求 owner，与行是否存在无关。cwd 位于或等于 `<root>/<area_id>` 时取该 area 的
       `area_delete_locks`（空 Area 也一样）；位于或等于 `<root>/<area_id>/<track_id>` 时再取该 track 的 `track_delete_locks`。
       repoint 提交绑定（`server/routes/tracks.rs:2270`）之后、回收旧 managed 目录（`:2296`）之前，行不再指向旧目录，但布局仍给出
       原 track id，repoint 一直持有该 track 锁，所以这段窗口同样被挡住。
       Planner 建议的“repoint 旧根也登记为 teardown 目标”实质采纳为“保留原 track 锁直到回收”，但不另做登记：布局已经给出身份，
       另加一份登记还得管它的释放。`.trash` 已由 C5-D4 拒绝，`<managed root>` 本身没有 owner（不是任何 effect 的目标）。
     - managed 根之外的 worktree 根：live 行的 `workspace_worktree_path` ∪ 进行中的 teardown 目标，后者沿用既有登记表
       `TeardownTargets`（`runtime/sweep.rs:9-10` 以 `(track_id, path)` 为键，删除提交前登记，`:90`，最后一个守卫随阻塞 sweep 结束才释放，
       `:124`）。今天的 `snapshot()`（`:13-20`）丢掉了 track id；C5a 加一个带 track id 的读法，O1 继续用不带身份的投影，
       不建并行登记表。worktree 不能按布局推断 owner（§6“外部合同事实”：取 DB 中 kernel 写入的路径）。Track 删除持 track 锁到 sweep 结束；
       Area 删除在 `server/routes/areas.rs:738` 释放 area 锁，但成员 track 锁保留到 sweep 结束，所以登记项所对应的锁在整个窗口内都被持有。
     **锁的生命周期**（N1）：在请求 track 自己的围栏（`terminal_execution_create.rs:166` 已持有）之外，对 owner 集合去重，跳过已持有的
     请求 track；其余 owner 锁只 try-lock，任何一把失败就释放本次已取得的全部守卫、不开写事务、返回 `WorkspaceBusy`；成功取得的
     守卫持有到准入写者提交为止。在准入写者内重新求 owner 集合（布局部分是确定的；live 行在事务内读；登记表取当时快照），
     与已锁集合不一致也返回 `WorkspaceBusy`。从不等待锁，所以不与 area → drive → 按序 track（`server/routes/areas.rs:874-892`）
     或 typed admit 的 track → writer 路径形成等待环；单次调用必定终止。不声称公平：与持续重试的删除同步争用时可能反复 Busy，
     这只影响活性，不影响安全，由用户重试解决。不在破坏性生命周期内时这些锁空闲，终端照常创建。
     Planner 偏好的“不新增终端创建拒绝”因此成立：用户在其他目录（含他 track 的目录）开终端是正当用法，只有与该位置的破坏性 effect
     并发时才 Busy。有了这一围栏，B 的证明在 B 的锁下与 effect 之间、直到 sweep 或 rmdir 完成，都不会有新的位置占用者。
  4. **不覆盖（记录，不声称）。** shell 在准入之后自行 `cd`（与 §3.1 D1 中准入之后外部改名属同一类，不在防御范围内）；Legacy PTY
     （配置关闭时的 Legacy terminal-create，`terminal_execution_create.rs:144-152`，以及 owner 自建 card）没有 execution 根，保留按 owning
     track 的 Legacy 检查，其跨 track 位置缺口是既有缺陷，单独立 issue，交给 Legacy wind-down。
  否决：只按 owning track（上面的反例）；终端创建时拒绝落在他 track 目录内的 cwd（拒绝正当用法）；删除时 Stop 他 track 的执行（越权）。

- **C5-D2（已决定）：restore 与回滚不做新证明，证明失败永不删除、永不削弱回滚。** restore 是同一围栏下对已完成移动的逆操作：只检查原路径
  不存在后把 trash 条目改名回去（`workspace/recycle.rs:51-79`），不删除任何东西。各分支照旧无条件执行其既有条件：Track 提交失败且行仍在
  （`server/routes/tracks.rs:3072`、`:3080`）、Track panic 且行仍在（`:3160`、`:3165`）、Area 提交失败（`server/routes/areas.rs:692`）、
  Area panic 且 area 仍在（`:767`）、批量中途失败（`runtime/recycle.rs:218-224`，反序 `:237-249`）、落点校验失败的自补偿
  （`workspace/recycle.rs:253`）。若在 restore 前要求新证明并在失败时跳过，live 目录会留在 `.trash`、7 天后被 GC，等于把已回滚的删除变成
  延期删除；此时发现的占用者恰恰需要目录回到原处。正向证明前移到第一次移动之前（上文），写者复查失败于是只剩“输给未受围栏的入口”这一窄
  分支，它保持现状：restore，再返回 Conflict（`server/operation/terminal_disposal.rs:110-120`）；restore 自身失败时保留 trash 条目与 seal，
  返回 Internal（`server/routes/tracks.rs:3082-3086`、`server/routes/areas.rs:692-696`），由既有 owner 路径（重试删除或运营者恢复）处理。
  **失败回滚的条目不被 trash GC 删除（评审 B3）。** 保留的 seal 是线程 seal，不是 GC 豁免；今天的 GC 只看条目名中的时间戳与 7 天
  保留期（`workspace/recycle.rs:352-361`），不看 track 或 Area 行是否还在，所以失败的 restore 只是把删除推迟 7 天。决定：GC 跳过
  **其 track 行仍存在且该行的 workspace 仍是 managed** 的条目。条目名是 recycle 自己写的 `<track_id>-<stamp>`（`workspace/recycle.rs:203`），
  track id 用 `trash_entry_timestamp`（`:384-388`）旁边的同一解析函数取出，不另解析第二种格式。保留集合由 `collect_once` 在它已持有、
  用于采样 `now` 的 drive 锁下读取（`server/workspace_service/trash.rs:21-24`），作为参数交给 `gc_trash`；filesystem 原语不读 DB。
  删除持 drive 直到提交或补偿，所以 GC 看到的要么是仍在的行（保留），要么是已提交的删除（条目照常按 7 天回收）。
  覆盖：原路径被占用与改名失败两种 restore 失败（行都还在）、Area 提交失败（成员行都在）、panic 分支，以及 recycle 之后、提交之前
  崩溃（行仍在、目录仍在 trash，今天 7 天后同样丢失）；行是 durable 的，所以跨重启成立。repoint 的旧目录条目不受影响：
  repoint 提交时行已改为 attached（`server/routes/tracks.rs:2299-2301` 的注释），条目照常过期。保留期限没有上限，直到运营者把条目改名回原处，
  或用户再次删除该 track，这是明确的最终删除：行被删除后，条目按其原时间戳回收，可能立即回收。不采用 Planner 给出的另一判据“原路径
  仍存在”：改名失败时原路径并不存在，这个判据挡不住；而行判据已覆盖原路径被占用的情况。不写新的 pin 文件或 pin 行：
  失败分支上的额外写入本身也可能失败；行判据不需要任何写入，也不需要迁移。
  提交后的 sweep 与 area 空目录 `remove_dir`（非递归，`workspace/recycle.rs:294`）继承提交前的证明：drive 与 track 锁随 prepared deletion
  保持到 sweep 结束（Area 的 area 锁保持到 `finalize_area_recycle` 之后、在 `server/routes/areas.rs:738` 释放，成员 track 锁保留到
  sweep 结束），C5-D1.3 的准入围栏按布局与 teardown 登记找到这些锁、挡住新的位置占用者；Area 容器目录本身也在 C5-D5 的全 Area
  证明之内。提交后不再查询，尤其不以已为空的 Area 成员关系作证明。

- **C5-D3（已决定）：repoint 保持既有“不 Stop 即拒绝”，不套用删除的 quiesce。** repoint 是对 live track 的绑定变更，不是删除，没有停止
  用户在跑工作的权限；`tests/execution_hooks.rs:468`（`repoint_rejects_unsettled_typed_without_stop`）断言 Conflict 且 desired 仍为 `run`，保留。
  时序不变：drive（`server/routes/tracks.rs:2041`）与 track（`:2043`）锁下，`require_safe_tx` 先于 session 取代与 harness 关停（`:2088`
  先于 `:2132`、`:2162`），Claude/ACP owner sweep（`:2187`、`:2206`）之后在绑定写事务内再查一次（`:2265`，写入 `:2270`），提交后回收旧
  目录（`:2296`），锁一直持有。repoint 的**目标**另由 C5-D7 的同一 O1 准入把关，与本条的占用检查互相独立。
  C5 只把两次检查都扩到 C5-D1 的位置占用（目标为旧 managed 目录）；`:2265` 那次就是 `:2296` 的正向证明，
  因为其后到 effect 之间围栏未释放，且新位置占用者被 C5-D1 的准入围栏挡住。删除路径的“先 quiesce、后写者复查”只适用于删除。

- **C5-D4（已决定）：retired trash 与一次性 objective checkout 豁免 Track/Area 证明，前提是两者都是 O1 的自有位置、且显式 cwd 被拒绝。**
  Planner 偏好的理由是“二者是 Neige 独占资源、在任何执行根之外”，今天并不成立：显式 cwd 不受限制（`target.rs:33-34`），可以指向
  `<managed root>/.trash/…` 或 objective checkout 根（`server/state.rs:567`，`objective-checks/<track>/<seq>-<ordinal>-<op>`，
  `server/operation/objective_check_run/mod.rs:155-158`）。只拒绝显式 cwd 也不够（评审 boundary）：O1 的自有位置集合只有 managed 根、
  live worktree 根与 teardown 目标（`truth/db/sqlite/track.rs:154-156`，`server/routes/tracks/allocation.rs:101-108`），objective checkout
  是 managed 根之外的 Git worktree，`none` 可以直接 attach 它（`runtime/selection.rs:29-35`）；之后该 track 的默认 cwd 终端
  （`target.rs:33-47`）与 provider execution（`server/provider_execution/launch.rs:23`、`:44-47`）都把它当作 track workspace 使用，
  绕过显式 cwd 的拒绝，而清理（`checkout.rs:62-86`）只证明了 op 自己的执行已结算。决定：
  1. **O1 的自有位置集合加入 objective checkout 根。** `admit_attached_workspace_tx` 的 `managed_root` 参数改为自有根列表：managed 根
     （`.trash` 在其下，已覆盖）与 objective checkout 根，规则不变（`none` 双向，`containing = true`；worktree 的 cwd、仓库根与派生目标父目录
     位于或等于即拒绝）。这样 track 创建的两种 attached 选择都不能落在 checkout 内，默认 cwd 终端与 provider execution 也就无从使用它。
     根只有一个来源：今天它只在 `server/state.rs:567` 以 `gate_logs_dir.with_file_name("objective-checks")` 算出一次，然后交给
     `ObjectiveCheckRunAdapter::new`（`objective_check_run/mod.rs:74`）。C5a 把这个值提为状态构造中的一个 typed 字段，adapter、track 创建
     写者与终端 cwd 解码共用它，不在终端或 allocation 代码里再拼一次 `objective-checks`。`admit_attached_workspace_tx` 的全部生产调用者
     都改传同一自有根列表：track 创建写者（`server/routes/tracks/allocation.rs:101`）、historical 创建在 Area 缺失时的拒绝路径
     （`server/routes/tracks.rs:884`）、子 track（`server/operation/child_track_adapter.rs:147`），以及 C5-D7 新增的 repoint 两处调用。
  2. **显式 cwd 同样拒绝。** 终端 cwd 解码（`target.rs:28`，typed 与 Legacy、`claude_restart_adapter.rs:286` 共用）拒绝位于或等于这两个
     内部根（`.trash` 与 objective checkout 根）的显式 cwd（BadRequest，同一 `paths_overlap`）。这不是 C5-D1 否决的那种拒绝：二者不是
     用户目录，§4.2 已把 `.trash` 列为自有位置；他 track 的 workspace 与任意目录照旧允许。
  3. **升级。** C5 之前已准入、workspace 或执行根落在 objective checkout 内的 track，与 C5 之前根在 trash 内的执行属于同一类残留：
     checkout 只在一个 op 运行期间存在，残留窗口限于升级时正在运行的 objective 检查；接受并写进 PR，不加永久检查。
  在此前提下：
  - trash GC（`server/workspace_service/trash.rs:19-28` → `workspace/recycle.rs:311-370`）不做 Track/Area 证明或位置证明，围栏只用于采样
    时间与读取 C5-D2 的保留集合（`trash.rs:21-24`），照旧在删除前释放。理由：条目只在 C5 证明（含位置）之后进入 trash，进入后
    O1 与显式 cwd 都不再准入新根；回滚由 7 天保留期与 C5-D2 的保留集合保护。残留：C5 之前已准入、根在 trash 内的执行，接受并写进 PR，
    不加永久检查。
  - objective checkout 的 owner 是 operation；有了 1 与 2，唯一准入的占用者是它自己的 gate execution。局部条件照旧：重建前只在 op 尚无
    execution 记录时删除（`server/operation/objective_check_run/mod.rs:197-199` → `checkout.rs:9-11`），清理只在 owned 恢复读到结算
    （`mod.rs:265`）并完成之后进行（`checkout.rs:62-86`）。不套 Track 证明：同 track 的无关执行会无谓阻塞清理；Track 删除照旧经该
    gate execution 的 track（`server/operation/gate_execution.rs`）保守地覆盖它。
  - claim staging（`workspace/materialize.rs:378`、`:413`）是发布前、按尝试命名、按年龄回收的目录，不是已准入 workspace 的拆除，不加执行生命周期。

- **C5-D5（已决定）：Area 在第一次移动之前做一次全 Area 证明；ACP owner 撤销先于通用 Stop。** 今天 Area 的顺序是 Legacy 检查
  （`server/routes/areas.rs:453`）→ Claude sweep（`:462`）→ 通用 quiesce（`:469`）→ 每个成员的 Codex（`:484`）、Legacy 终端（`:489`）、
  **ACP sweep（`:499`）**、harness 关停（`:505`）→ 批量 recycle（`:542`）→ 写者复查（`:615`）→ `area_delete_tx`（`:648`）。ACP sweep 落在
  通用 Stop 之后，与 Track（`server/routes/tracks.rs:2748`、`:2758` 先于 `:2768`）和 `areas.rs:459-460` 的注释都不一致。决定：
  1. ACP 的 `sweep_track` 移到 Claude sweep 的成员循环旁，全部成员的 provider 撤销与 Stop 都先于 `:469`；
  2. 所有成员的 Codex、Legacy 终端与 harness 步骤之后、`recycle_area_workspaces` 移动第一个目录之前，做一次全 Area 正向证明：scope 为
     prepared 时捕获的成员 track id（area 锁 `:875` 与成员 track 锁 `:892` 下成员关系不变，与 `Scope::Area` 等价），位置目标集合为
     **Area 容器目录 `<managed root>/<area_id>`**（`finalize_area_recycle` 删除的正是它，`runtime/recycle.rs:251-253` →
     `workspace/recycle.rs:275-294`）与全部成员的 worktree 根，`containing = false`。成员 managed 目录都在容器之内，由容器目标覆盖。
     根位于或等于容器的他 track 执行（例如 A 的终端以 `<root>/<area_id>` 为显式 cwd，它在每个成员 managed 目录之上，按成员目标比较
     会被 `containing = false` 排除；评审 boundary）因此是占用。空 Area 没有成员，scope 为空，但位置目标仍是容器，证明照做，不跳过。
     任何占用都在移动之前返回 Conflict，不出现“部分移动再回滚”。不做逐成员证明与移动交错。证明到 rmdir 之间，容器的新占用者被
     C5-D1.3 挡住：其 owner 是该 area 锁，按布局求得，与 Area 行是否已删无关；area 锁持有到 `finalize_area_recycle` 之后
     （`server/routes/areas.rs:731-738`）。
  3. 写者复查 `:615` 保留 `Scope::Area`（提交前成员关系完整），位置目标与 2 相同；提交后的 sweep 与空目录清理按 C5-D2 继承证明。
  4. **panic 分支不删除容器目录。** `run_recycled_area_deletion` 在 area 行已删时的 panic 分支（`server/routes/areas.rs:771-780`）调用
     `finalize_area_recycle`，此时 area 守卫已随 saga future 丢弃，rmdir 不在围栏内。该 rmdir 只是收尾（`runtime/recycle.rs:251` 的注释），
     C5b 在这一分支删去它：留下的空目录记录日志，不做任何其他处理。不把守卫移出 saga：那样成功路径要持 area 锁直到 sweep 结束，
     而 `:736-737` 有意在 sweep 之前释放它。Track 的 panic 分支（`server/routes/tracks.rs:3157-3177`）只在行仍在时 restore，不做破坏性 effect，
     不受影响。

- **C5-D6（已决定）：forge 的 probe 准入取同一准入围栏；C5b 删除阶段代理之前必须先有它（评审 B1）。** 事实：forge execution 只有两个准入点，
  `admit_action`（`server/operation/forge_execution.rs:195-224`，唯一调用者是 TxCommitted 的 spawn，`forge_action_adapter/mod.rs:558`）与
  `admit_probe`（`:227-266`，verdict 在 `forge_action_adapter/lifecycle.rs:164`，output 在 `:230`）。`admit_probe` 经 `recover` 到达，
  `recover` 有两个入口：observer 循环（`lifecycle.rs:384`）的 `observer_step`（`:251`）→ `owned_parked::reconcile`（`:267`、`:301`，`owned_parked.rs:17-25`），
  以及 boot/sweep 恢复的 `recover_owned_parked`（`forge_action_adapter/mod.rs:571-583`）。两个准入点都只要求 live claim（`forge_execution.rs:215`、
  `:261`），不取 drive 锁，也不取 track 锁。probe 的根是 canonical 化的 `cwd_lease`（`:160`）。今天挡住它的只有阶段代理
  （`server/operation/workspace_lease/mod.rs:63-71` 含 Parked）。C5b 删掉代理之后，一个 Parked、action 在证明时已结算的 op，可以在证明与
  recycle 之间，或在 clean 只持 track 锁执行 `git clean` 期间（`server/workspace_service/ports.rs:113-143`），由 observer 或恢复准入一个 probe。
  决定：
  1. `admit_probe` 在开写事务之前取 C5-D1.3 的同一准入围栏：一个共享函数（calm-server，与 `try_launch_fence` 同层，`server/operation/driver.rs:79-85`），
     终端创建与 forge 共用，不复制。它 try-lock `FrozenForge.track_id` 的 track 锁，以及 `cwd_lease` 根的 owner 集合，去重；任何一把失败就
     全部释放，持有到准入事务提交为止。准入事务内（同一 `require_live_claim_tx` 事务）另外要求 frozen track 行仍存在。
  2. 围栏返回 Busy 时，`probe_step` 与 verdict 分支返回 `LeaveParked`，不写 `compare_exchange`，probe 槽位仍为空；下一次 observer 步骤
     或 boot/sweep 恢复会重试，`recover_owned_parked` 在 `LeaveParked` 时重新确保 observer（`mod.rs:578-581`）。Busy 不延长 deadline：
     过了 deadline，由既有的 PastDeadline 判决决定。track 行已不存在时，op 经既有 `ParkedRecovery::Complete` 以 gate-infra 失败结束，
     不准入任何 execution。
  3. `admit_action` 不加围栏，论证如下：它只在 op 尚无 `forge_execution` 记录时运行，记录正是由它写入的（`forge_execution.rs:216`）；
     C5b 只对**带** `forge_execution` 的 op 删除阶段代理，无记录且处于可恢复阶段的 op 照旧阻塞删除与 clean。删除在自己的锁下读这个代理；
     新 op 只能经 Pending 准备进入 TxCommitted，而 Pending 准备在 `try_launch_fence` 下（`driver.rs:701-716`），删除期间会得到 Busy。
     所以一个无记录的 op 要么被代理看到，要么根本启动不了。
  4. **同类扫描：所有 `Engine::admit` 调用者。** 证明成立的前提是，证明之后不会有经受围栏的入口在目标内准入新执行，所以 C5a 的 PR 逐个
     列出调用者及其证据：typed 终端（`server/operation/terminal_execution_create.rs:93`，自身 track 的 try-lock 加 C5-D1.3）；Planner 与
     provider 启动（`server/provider_execution/mod.rs:278`、`server/claude_planner/session/launch.rs:263`、`server/claude_planner/rewind.rs:123`、
     `server/acp_planner/session/cancellation.rs:58`），按 C1 持有区间表应在 track 锁下从检查持有到占用事实提交，PR 逐个核实；owned Pipe
     （`server/operation/owned_execution/pipe.rs:105`）里，task gate 的根是 lease checkout，由保留的 `checkout_occupancy` 覆盖（lease 行先于准入存在），
     objective 检查的根是它自己的 checkout（C5-D4）；forge 按本条处理。不属于以上任何一类的调用者，在发现它的那一片接入同一个共享围栏，
     不靠代理。

- **C5-D6.5（Planner 决定）：历史 edit 的 RewindCheck 也受 track 围栏。** 实现盘点发现 C5-D6.4 的既有前提不成立：
  `routes/planner_input_send.rs` → `harness/run_loop/replace_command.rs::handle_replace_admitted` 只持有 `inner.issuance`，
  经 `claude_planner/session/invocation.rs::prepare_edit` → `claude_planner/rewind.rs::check` 准入 RewindCheck 时没有 track 锁。
  决定：`handle_replace_admitted` 在 `inner.issuance` **之前**取得以 `inner.track_id` 为键的 `track_delete_locks`，保持
  track → issuance 顺序；复用 `lock_track_for_issuance` 的 shutdown-select 等锁步骤，但不复用它的 `can_seek_issuance`
  预检。ShuttingDown 与 Superseded 使用既有 edit refusal。守卫保持到 `prepare_rewind` 返回且 RewindCheck 已结算，
  或 clean 可见的占用事实已提交。生产 edit 入口的回归测试证明持有 track 锁时不能准入 RewindCheck。
  同时核实 C5-D6.4 的其余 provider 准入：没有在 track 锁下保持到占用提交的调用者遵守同样的锁序；若既有锁序使其无法
  取得 track 锁，则停止并提交带源码位置的冲突，不倒置锁序。

- **C5-D7（已决定）：repoint 的目标经同一 O1 准入，在任何生命周期变化之前一次、在绑定写者内再一次（评审 R2-B1）。** 事实：repoint 对目标
  只做 Git 校验（`server/routes/tracks.rs:2032` 的 `validate_attached_workspace`：绝对路径、存在、是目录、Git 工作树、slice 分支命名空间空闲）；
  两次 folder claim（`:2128` 的 `ScanOnly`、`:2262` 的 `Authoritative`）只执行 Area 文件夹归属，不排除内部根；绑定写者（`:2270`）
  写入 `attached`、`worktree = None`、冻结，不调用 `admit_attached_workspace_tx`。managed 目录是 Git 仓库（§3.1），`.trash` 下的条目
  `<managed root>/.trash/<track>-<stamp>` 与 objective checkout 根（Git worktree，C5-D4）都能通过 Git 校验，所以一个 pristine 的 managed track
  今天可以 repoint 进 objective checkout 或 `.trash` 中的仓库。之后该 track 的默认 cwd 终端与 provider execution 都以它为根
  （`server/operation/terminal_adapter/target.rs:33-47`，`server/provider_execution/launch.rs:23`），而 objective 清理（`checkout.rs:62-86`）
  与 trash GC（`workspace/recycle.rs:311-370`）按 C5-D4 不做位置证明，会删除这些根。这绕过了 C5-D4.1 对创建的限制与 C5-D4.2 对显式 cwd 的限制。
  决定：
  1. **复用同一准入，不加第二种检查。** repoint 调用 calm-truth 的 `admit_attached_workspace_tx`，参数与创建相同：C5-D4.1 扩展后的自有根列表
     （managed 根，含 system area 与 `.trash`；objective checkout 根，取状态构造中的同一 typed 字段）、事务内读取的 live worktree 根、
     `TeardownTargets` 的快照（`teardown_targets.snapshot()`）。plan 取 `TrackWorkspacePlan::AttachedFromCwd`，因为 repoint 写入的绑定
     （`attached`、`worktree = None`）就是 `none` 隔离（`target.rs:43-47` 对它按 `ExternalDir` 解码），规则与 `none` 创建相同：双向，
     `containing = true`；目标的 lexical 位置与 physical 位置（`PathLocation::existing` 取 canonical，自有根经 `paths_overlap` 的 physical
     比较，`truth/db/sqlite/track.rs:104-117`）各比一次，解析失败是错误。所以经符号链接指向 objective checkout 或 `.trash` 仓库的目标、
     以及自有根经符号链接祖先到达的情形都被拒绝。不在 server 里另写路径比较或内部根名单；拒绝为 BadRequest，信息说明冲突的自有位置。
  2. **第一次：任何生命周期变化之前。** 在 `:2032` 的 Git 校验之后、取 drive 与 track 锁（`:2041`、`:2043`）与 owner preflight、R2 transition
     gate（`:2045-2070`）之前，在一个只做准入的写事务里调用（与 `:880-894` 的 historical 拒绝路径同一形状，不写任何行）。因此被拒的请求
     不进入 fence 事务（`:2080`），不 supersede session（`:2140`），不关停 harness（`:2162`），不 sweep Claude/ACP owner（`:2187`、`:2206`），
     旧绑定、carrier 与旧目录都不变。
  3. **第二次：绑定写者内，权威。** 在 `:2246` 的写事务内、`:2270` 的 `track_workspace_write_tx` 之前，与 `Authoritative` folder claim
     （`:2262`）同一 `BEGIN IMMEDIATE`，再调用一次同一函数。两次之间可能出现新的自有根（例如在目标内新建 worktree track、objective
     checkout 被物化、Track 删除登记 teardown 目标），只有写者内的这次与写入原子；它失败时走既有的写失败分支（`:2285-2292`）：
     不写入、不回收，在旧路径重启 Planner harness（`restart_planner_harness_at`），旧绑定与旧目录保留。
  4. **直接后果，与 `none` 创建一致。** 目标位于、等于或包含任何自有位置都拒绝：objective checkout 根及其子目录、`.trash` 下的仓库、
     任一 managed 目录（含本 track 自己的旧 managed 目录，否则提交后的回收会把新 workspace 一并移进 `.trash`）、live worktree 根与
     进行中的 teardown 目标；仓库 R 上仍有 worktree track 时，R 本身也不能作 repoint 目标（R 包含其 worktree 根），这是 §4.2 的同一规则，
     §4.2 开放问题④若放宽，创建与 repoint 一起放宽。否决：按 worktree 规则（`containing = false`）准入，它与写入的 `none` 绑定不符。
     没有任何自有位置的普通外部仓库照常 repoint。
  5. **写入 track workspace 或执行根的全部路径（同类扫描）。** `track_workspace_write_tx`（`truth/db/sqlite/track_workspace.rs:12`）是
     唯一的 workspace 写者，生产调用者三个；执行根另有两类来源：
     - track 创建（`truth/db/sqlite/track.rs:270`，经 `server/routes/tracks/allocation.rs:101`）：attached 两种 plan 经 O1 准入；managed plan
       的路径由 area 与 track id 推导、位于 managed 根内，函数自身对它直接返回（`truth/db/sqlite/track.rs:133-135`）。新建（含 fork 与 recipe）都经这一个写者；
       keyed 重放不重新准入（§4.2）。historical 创建在 Area 缺失时（`server/routes/tracks.rs:884`）只做同一准入后拒绝，不写行。
     - 子 track（`server/operation/child_track_adapter.rs:147`，同一创建写者）：managed 父 → `ManagedFrozenUnder`，推导路径；attached 父 →
       `AttachedWithTrackWorktree(父路径)`，父路径、仓库根与派生 worktree 父目录经 O1 准入。
     - repoint（`server/routes/tracks.rs:2270`）：今天没有准入，由本条补上。
     - system area 的 launchpad（`server/routes/today.rs:309`）：路径由 `launchpad_workspace`（`:227`）从 managed 根、area 与 track id 推导，
       kind 为 managed，不接受任何外部输入；与 managed 创建同理不经 O1，不变。
     - 显式 cwd（`target.rs:28`，typed、Legacy 与 `claude_restart_adapter.rs:286` 共用）：按 C5-D4.2 拒绝内部根，按 C5-D1.3 取准入围栏；
       有意不套用完整的 O1（C5-D1 否决了拒绝他 track 目录的做法）。
     - 默认 cwd（`target.rs:33-47` 的 `agent_cwd`；provider execution 的 `launch.rs:23` canonical 化同一 workspace）：只消费已准入的绑定，
       由上面四个写入路径的准入传递覆盖，不加第二次检查。owned Pipe 的根（task gate 的 lease checkout、objective 检查自己的 checkout）是
       kernel 自有位置，归 C5-D6.4 的调用者归类。
     测试夹具直接写行（例如 `truth/db/sqlite/task_projection.rs` 的测试 setup）不是生产路径。PR 附这份扫描的结果，不做成永久名单。
  6. **残留。** 准入是一次文件系统观察：准入之后外部改名或替换链接属 §3.1 D1 的同一例外；C5 之前已 repoint 进内部根的 track 属 C5-D4.3
     的同一残留，接受并写进 PR，不加永久检查。

**C5 切片。** 每片 L2（workspace 隔离与生命周期边界），gate 同 §3.2 公共 gate G：worker 跑聚焦测试与格式化；gate 跑 quick、文本与
contract 检查各一次，加一次拒绝空选择的聚焦 replay；广泛套件归 CI。下表测试为实现时注册的新过滤器。三片都**保留**：Legacy worker 启动行
（`server/operation/terminal_disposal.rs:37` 的 terminal-worker、claude-worker、codex-worker，与 `:110-141` 的两个检查；W 的 WT2/WA2 不在
main）、Legacy 终端静默（`server/terminal_sweeper.rs:535` 起，typed 已跳过）、共享 Codex 线程检查（`server/routes/planner_cards.rs:82`、
`server/routes/tracks.rs:2735`、`server/routes/areas.rs:484`）、pre-C4 Planner 的 boot 证明与 Claude/ACP owner 生命周期
（`server/claude_planner/lifecycle.rs:91`、`:106`，`server/acp_planner/config.rs:371`、`:381`；后者不是 Legacy 代理）、`checkout_occupancy`
（`truth/db/sqlite/track_occupancy.rs:47`）、repoint 的规范 carrier 与 pristine 检查。

| # | 结果 | 范围、删除与保留 | 验收 | 依赖 |
|---|---|---|---|---|
| C5a | 位置占用与 Track 侧正向证明 | calm-truth：共享 `paths_overlap` 与按位置的未结算读者，逐行解码、失败关闭（C5-D1.1）；O1 自有根列表加入 objective checkout 根，该根提为状态构造中的 typed 字段（C5-D4.1）；终端 cwd 解码的准入围栏，含按布局求得的 area/track owner、`TeardownTargets` 带 track id 的读法与 N1 的锁生命周期（C5-D1.3），以及内部根拒绝（C5-D4.2）；该围栏是终端与 forge 共用的一个函数（C5-D6.1）；trash GC 的保留集合（C5-D2）；repoint 目标经同一 O1 准入，一次在任何生命周期变化之前、一次在绑定写者内，O1 全部调用者改传扩展后的自有根列表（C5-D7）；生产消费者：Track 删除在 recycle 之前的正向证明与写者复查、repoint 两次检查（C5-D3）、clean 的 fence（`server/workspace_service/ports.rs:113-143`）；PR 附 `Engine::admit` 调用者的逐个归类（C5-D6.4）与 workspace 写入路径的扫描（C5-D7.5）。不删除 Legacy 检查 | A 的 typed 终端根在 B 的 managed 目录或 worktree 内：B 的删除与 repoint 返回 Conflict、clean 延后，目录、branch 与行保留，A 的执行 desired 仍为 `run`；B 删除持锁期间 A 在 B 目录内的显式 cwd 创建得到 `WorkspaceBusy`，锁释放后成功；Track 删除在提交之后、worktree sweep 之前暂停（既有夹具钩子 `server/workspace_service/sweep.rs:51` 的 `before_sweep`），经生产终端创建端点以 A 为 track、B 的 worktree 内为 cwd，得到 `WorkspaceBusy` 且没有新的 execution 行，恢复后 worktree 与 branch 被拆除；repoint 在绑定提交之后、回收旧目录之前暂停，cwd 在旧 managed 目录内同样 `WorkspaceBusy`；删除提交失败回滚之后登记释放、同一请求成功；own cwd 终端不因自身锁而 Busy；A 进 B 与 B 进 A 的并发请求都在有限时间内以成功或 Busy 结束；根在仓库根（B 的 worktree 之上）的执行不阻塞；显式 cwd 位于 `.trash` 或 objective checkout 根时 BadRequest；objective checkout 存在时，以它或其子目录作 `none` 的 attached track 创建、以它作仓库根的 worktree 创建都 400；经生产 `PATCH /api/tracks/{id}`（`update_track` → `server/routes/tracks.rs:2525`）对 pristine managed track 做 repoint，目标等于或位于 objective checkout 根、等于或位于 `.trash` 下的 Git 仓库，以及经符号链接指向这两者的物理别名，都 400，且没有 session 被 supersede、harness 未关停、旧 managed 绑定（kind、path、未冻结）、card 的 carrier 与旧目录逐项不变、`.trash` 中没有新条目；repoint 在 `wait_at_workspace_repoint_race_hook`（`server/routes/tracks.rs:2181`）暂停时于目标内物化一个自有根（例如在目标仓库上建 worktree track），放行后绑定写者拒绝、旧绑定与旧目录保留、Planner harness 在旧路径重启；没有自有位置的普通外部 Git 仓库照常 repoint 成功并回收旧目录；未结算行的 intent 缺 `source.root`、为 `null` 或为数字时（夹具直接插入行），任一 track 的删除与 repoint Conflict、clean 延后；restore 失败（原路径被占用；注入改名失败）后把 GC 时钟推过 7 天，条目保留，重启后仍保留；再次删除该 track 之后条目被回收；repoint 留下的条目照常在 7 天后回收；quiesce 之后、recycle 之前准入的执行使删除在移动之前 Conflict（不经 restore）；`tests/execution_hooks.rs:468` 照旧通过 | C2a–C4、O1 |
| C5b | Area 全量证明与 forge 代理切换 | C5-D5 的 ACP 顺序与全 Area 证明，位置目标为 Area 容器目录与成员 worktree 根；panic 分支删去容器 rmdir（C5-D5.4）；Area 补偿矩阵按 C5-D2 只扩测试、不改 restore。forge 的 probe 准入接入 C5a 的共享围栏，事务内复查 frozen track 行（C5-D6.1–2），**同一片**删除 forge 已迁移类型的阶段代理：三个破坏性调用者（`server/routes/tracks.rs:3317`、`server/routes/areas.rs:902`、`server/workspace_service/ports.rs:125`）不再因带 `forge_execution` 的 op 处于可恢复阶段而阻塞，由执行谓词与 probe 围栏覆盖；无 `forge_execution` 记录的可恢复阶段 op 照旧阻塞（C5-D6.3）；**保留** pre-C3 原生行与原生 Stuck（`server/operation/workspace_lease/mod.rs:102-110` 中 `forge_execution` 缺失的分支），分类留在 forge owner；`track_close`（`server/mcp_server/tools/track_state.rs:390`）的逻辑活动策略不变 | 任一成员（含他 track 拥有、根在成员目录内的执行）未结算时第一个目录不移动，Conflict，全部目录与 branch 保留；A 拥有的 typed 终端以空 Area 和非空 Area 的容器目录为 cwd 时，Area 删除 Conflict、容器保留、A 的 desired 仍为 `run`；Area 删除持 area 锁期间以容器为 cwd 的创建 `WorkspaceBusy`；Area 删除在提交之后、sweep 之前暂停，cwd 在成员 worktree 内 `WorkspaceBusy`；ACP 撤销先于通用 Stop 的顺序测试；提交失败与 panic 照旧恢复全部目录；带 `forge_execution` 且执行已结算、op 仍在可恢复阶段时 Track/Area 删除与 clean 不再被阻塞，执行未结算时被阻塞；action 已结算的 Parked op，在删除的证明与 recycle 之间、以及 clean 持 track 锁期间，驱动 observer 与 boot/sweep 恢复：不准入 verdict 或 output probe（没有新 execution 行），op 仍 Parked；锁释放后 track 仍在则准入 probe，track 已删则 op 以 gate-infra 失败、不准入；原生 Stuck 行照旧 Conflict 且被忽略文件保留（既有 `tests/cases/track_delete_forge_fence.rs` 的原生用例保留，只伪造阶段的用例改为带真实 execution） | C5a |
| C5c | clean 的 typed 终端 session 代理切换与其余删除入口审计 | `CLOSED_TRACK_WORKTREES_SQL`（`truth/db/sqlite/track_workspace.rs`）只剔除有规范 terminal alias 历史的 typed 终端，含 settled/retired 历史；占用由 C5a 执行谓词覆盖。**保留** Claude/ACP Planner 的 session/TurnPending 代理：issuance 释放 Track 早于 Engine 准入（`server/harness/run_loop/issuance.rs:369,507-510`）；独立后续切片延长 guard（C5-D6.4/D6.5）。Legacy task worker、owner 自建 PTY、升级前行与 Codex 线程状态照旧；审计只删行的 Card、plugin 与启动补偿入口，它们不是 C5 effect | typed 终端 stale Running + settled execution 时 clean 进行；Exited/Idle + unsettled execution、无 renderer 时延后；Legacy 无 execution 仍延后；Claude/ACP Planner stale Starting/Running/TurnPending + settled execution 仍延后。保留 `tests/cases/track_worktree.rs`、`tests/cases/c1_planner_fence.rs`、`tests/cases/c1_fence.rs` 断言；仅对 no-silent-miss 断言做变异验证 | C5b |
| C5d.1 | Planner 准入 fence 延长（#2666，PR1） | Claude/ACP 的 historical turn、canonical first/resume/retry 与两种 compaction 路径将 typed Track fence 交给 ProviderExecution；admit 写者核对 intent track，occupancy commit 后释放，错误关闭；Codex 在原生调用前释放，rewind 保持 Held 到结算（D6.5）。保留 CLOSED_TRACK_WORKTREES_SQL 的 Planner session 代理，删除留给 PR2。L2：跨生命周期锁顺序与准入边界 | 生产入口在 occupancy commit 前暂停时 clean/delete 等待；提交后 clean 延后、delete 停止执行且无死锁；错 Track 与空 handoff 返回 Conflict；resume/retry 不重复 dispatch；保留 C1 与 rewind 回归。新增路径由必需参数强制携带 fence，无增长列表；测试每次 CI nextest 运行一次 | C5c |

之后的 S3（Revision、B pin、7/90）与 S2b（view、WorkspaceRef、WorkspaceContent）沿用各自协议，不在本表。

### 3.3 W：task worker 迁入 Engine（独立里程碑；重写设计见 task-worker-execution.md）

**重写设计（第三版，设计已定稿：两路 L2 设计评审第 3 轮通过，用户已确认方向，ask 686425；仍只是设计，不声称已实现）。** [task-worker-execution.md](task-worker-execution.md) 是 W 的重写设计，比较了路线 A（claude-worker 复用已落地的 Agent Runtime：无界面、每 turn 一个 Engine execution、与 Claude Planner 共用 stream-json turn 驱动与 `provider_execution` owner 层）与路线 B（下文起点的 typed PTY），推荐 A 用于 claude-worker、terminal-worker 只用 gate 那样的 Pipe execution（不再是 B 的收窄版）、codex viewer 按其 OQ6。切片改为 WA0–WA4、WT、WV（该文 §7）；W 只依赖 C4，与 C5 的先后不再是硬约束。第一版、第二版各经 W 设计评审一轮（boundary、adversarial）判为阻塞。第二版的修订是：释放授权的 task 条件进入 Held→Releasing 的同一写事务（该文 §3.3）；无界面 session 的收尾与租约兜底经 reaper 的 SQL 臂复用 `converge_dead_worker`（§3.7）；worker 的登录、配置与环境有 typed 合同，不借用 Planner 的 `CLAUDE_CONFIG_DIR`（§3.4.1）；升级分界按 op 的持久准备形状（§10）。第三版按第二轮收窄：每个 turn（含首 turn）都由同一种 `claude-worker-turn` 行发行，spawn op 与 boot 都不发行，每行至多准入一个 execution，唯一消费点是 worker owner 以精确 execution 所有权为条件的 `complete` 事务，续跑行只在该事务终结原行时写出、以被中断的 execution 为键（§3.6）；`converge_dead_worker` 改为单个写事务，精确当前 session 围栏先于 task 失败写与所有租约兜底，过时结果无副作用返回，task 失败写改用 card 精确所有权，首 turn 未 stamp 的窗口按既有 op 目标证明仍算绑定（§3.7）。逐条处置见该文 §11。下列 WR1–WR7 仍是验收条件，每条后的“路线 A 处置”是该文 §6 的摘要；本节其余的起点原文保留为路线 B 的记录。用户已确认路线 A 与开放问题的结论（路线 A、terminal-worker 只用 Pipe、长时间或交互命令走 Workbench 常驻终端卡、codex-worker 不动；OQ1 (a)、OQ2 先只给中断、OQ3 拒绝回给模型、OQ4/OQ10 `UserHome`、OQ11 每 turn 一 execution、OQ8 接受 Engine 为硬依赖；OQ5、OQ6、OQ7、OQ9 保持开放，按推荐推进，由对应切片定），见该文 §0 第 9 条与 §5.4；路线 B 不实施。

状态：**未收敛的设计起点，不可直接实施。** C4 设计评审第三轮（head `22472d737`）两路都在 worker 迁移上留下阻断项，第二轮以来的阻断也都落在
worker 一侧。按 AGENTS.md，三轮之后仍有阻断就要改方法或收窄范围，Planner 因此决定把 worker 迁移移出 C4，成为 C5 之后的独立里程碑 W。
C4 只保留 C4.0a–C4.0d 与 C4.1–C4.4。C4 与 W 之间没有交叉依赖：C4 各片不依赖 W 的代码或决定，也不为 worker 预置只有 worker 才需要的东西；
W 动工时 C4 与 C5 都已合入，它只复用其中已交付的部分，即 C4.0a 的载荷模块与键合同、C4.0d 的全局恢复节拍，以及 C4.1 在
`server/execution_environment.rs` 引入的代理分类函数。

W 合入之前，terminal-worker、claude-worker 与 codex-worker 的 viewer（盘点 W1–W3）照今天经 Legacy `EnsureProc` 运行，reaper、scheduler 与
terminal sweeper 的既有退出收敛不变。C5 对它们保留 Legacy 占用与静默检查，和 owner 自建 card、共享 Codex daemon 的处理相同（见 §3.2 的 C5 行）；
C5 不等待 W。W 的每一片迁移一种 worker 时，只删除该 kind 新启动的 Legacy 代理判断，升级前的 Legacy 行与 owner 自建 card 的检查照 C5 的规则保留。

W 动工前，须先按下列需求重写设计，作为新的 L2 设计经两路独立评审收敛。需求是 W 设计的验收条件；后面保留的第三轮设计只是起点，被需求否定的部分
以 WR 编号标出。片名改为 Wa–We（依次是原 C4.5a、C4.5b、C4.5c、C4.6、C4.7），新测试过滤器前缀改为 `w_`。

**需求（第二、三轮仍开放的 worker 发现）。**

- **WR1 退出取代的原子围栏（C4 第三轮 adversarial R3-B1）。** 起点的 D5 在读取时检查“同一 card 有更新的未终结 session”，再把失败交给既有
  `converge_dead_worker`。后者经 `task_fail_from_worker_tx`（`TaskReporter::Kernel`）在同一事务里释放 card 的 lease，守卫只有 task id、track 与状态
  （`reaper/mod.rs:402-437`、`truth/db/sqlite/task.rs:626-657`），不复核 session 是否仍是当前的。若在 D5 与 D6 之间手动 `claude-restart` 建出替代 session
  （`claude_restart_adapter.rs:248-255,310-327`），或有新 attempt 被准入，D6 仍会让健康替代者的 task 失败，并释放它的 lease。要求：精确的当前
  attempt/card/session 与“无取代 session”必须是授权 D6 与 lease 释放的**同一个** calm-truth 写事务的条件；过时的结果不做任何 task 或 lease 写入，
  也不走 `converge_dead_worker` 在竞争失败时的 lease 兜底；以“D5 之后暂停、随即重启”与“当前 attempt 被替换”两个交错测试钉住。
  **路线 A 处置：收窄并修共享分派器。** 无界面 worker 的重启是同 session 的新 execution，与新准入的竞争由 SQL `admit` 的单 claim 与“card 当前 session”条件串行化；owner `complete` 不写 task。kernel 一侧写 task 或租约的收尾只剩 `converge_dead_worker`，它改为单个写事务：精确当前 attempt/card/session（本 session 未终结且仍是 `cards.session_id`）是 task 失败写与**所有**租约兜底（缺绑定、竞争失败）的同一事务条件，过时结果不写 task、不释放租约、不进任何兜底；task 失败写改用 card 精确所有权，不再用 `TaskReporter::Kernel`。保留的 Legacy 探测臂同样经过它；以经真实 reaper 的“探测后暂停 → Legacy restart → 旧结果返回”与“当前 attempt 被替换”两个交错测试钉住（该文 §3.7，WA2）。
- **WR2 worker 输出的 durable 完成义务（R3-B2）。** 起点让 worker 执行 owner 在结算后取 OutputLog 的尾部写进 `terminals.pty_output`，并要求 scheduler
  的 Terminal 臂等它写入之后再完成 task。但结算事务不写 `pty_output`（`truth/db/sqlite/workspace_execution.rs:194-219`）；已结算的行不在
  `unsettled_worker_executions` 中，Terminal 类 task 也不在 P2 内，C4.0d 的 pending 谓词同样不含已结算的 Run/Stop 行。所以在结算之后、取尾之前崩溃，
  task 要么永不完成，要么按今天“退出字段一出现就完成”的规则（`scheduler/mod.rs:1875-1890`）发布空的或过时的结果。Drained 还可能带
  `OutputEnd::Open`（workspace-execution.md §6.4），结算并不证明最后的输出尾部已经落盘。要求：输出准备是一项 durable 义务，结算之后仍可发现，
  直到完成成功为止；倾向由既有 scheduler running-terminal 臂在 `complete_terminal_task` 之前调用一个幂等的 worker owner helper 取尾并持久化；
  显式处理 `OutputEnd` 的 Complete 与 Open/Incomplete，以及读写失败后的重试；测试覆盖结算后取尾前崩溃、输出读者暂停与一次性复制失败，均无 renderer、
  无用户操作。
  **路线 A 处置：** Claude 不适用（task 结果是自报，无 `pty_output`）；terminal-worker 仍需要，落到子进程自己写的日志文件与 parked op：结算后未完成的 op 由 parked 恢复重驱，完成时重读日志尾（该文 §5.5，WT）。
- **WR3 typed worker 终端的重启（boundary 第三轮 B1）。** 起点保留“Claude 退出 0 而未自报就交给 liveness 期限”，理由是手动 `claude-restart`
  可以继续同一 task。但 `claude-restart` 复用 card 的既有 terminal（`claude_restart_adapter.rs:282-325`），清掉退出字段、丢弃 renderer 后经
  `spawn_terminal` 走到 `renderer.ensure`，而后者明确拒绝 typed terminal（`terminal_renderer/mod.rs:459-469`）。typed 身份按 execution alias 是否存在判定，
  结算或 Retire 之后也不变（`truth/model.rs:225-240`），所以迁移后的 Claude worker 终端无法重启。要求：迁移 claude-worker（Wd）之前给出重启消费者
  的合同，例如明确设计的新终端生命周期，或单独评审的 typed 重启（workspace-execution.md 的“alias slot”规定每个 `term:<id>` 只有一个执行，
  终端重启须另行给出路由、授权、幂等键、renderer 切换到新 ExecutionId 与 alias 交接，并单独评审）；或者由用户/Planner 明确接受删除“可手动重启继续”的承诺，并相应修改 Claude 退出 0 的政策。不得去掉 typed
  围栏，不得对同一 alias 静默回退 `EnsureProc`，也不得把 alias 交接说成 Engine 合同不变。须加一条真实的“worker 退出 → 重启 → 自报”回归，
  不能只用 Legacy 夹具。WR1 的取代竞争正发生在这条重启路径上，二者须一起设计。
  **路线 A 处置：删除（Claude）。** 无 typed 终端；重启是经持久 `claude-worker-turn` 行发行的 `--resume` 新 turn execution，alias 含 execution id；保留一条真实的“turn 结束 → 续跑 → 自报”回归（WA2）。
- **WR4 未绑定 session 的 lease 清理（boundary 第三轮 B2）。** 起点的 D5 让没有绑定 attempt 的 session 直接走 D7 的 `session_commit_exit`，而后者只更新
  session（`truth/db/sqlite/session_row.rs:613-658`）。今天的 `converge_dead_worker` 在 spawn op、task key 或 track 无法解析时有意释放 worker 的 lease
  （`reaper/mod.rs:381-391`），task 竞争失败时还有 `ReleaseDelivery::CommitAsTaskEnded` 兜底（`:443-467`），`reaper/tests.rs:670-728` 钉住了无 spawn op
  而仍持 lease 的情形。决策树会丢掉这条清理，所以 Wb 声称的“唯一行为差异”不成立。要求：task 写入授权与已死 session 的资源清理分开；对正面确认
  已死、没有绑定的 session，在终结 session 之前保留既有幂等的 lease/交付兜底，经它的权威 helper 执行，不在 D7 复制 lease 政策；Verifying 与
  被取代的活 card session 仍不被触及，这一点与 WR1 的围栏一致：被取代时不释放替代者的 lease。须加经 P1/共享 `exit_policy` 入口的测试，覆盖既有的
  无 spawn op 夹具与缺绑定、竞争失败两个兄弟情形；直接调用不变的 helper 不算新分派器的覆盖。
  **路线 A 处置：触发。** 无界面 session 不进探测路径，正因如此须另有发现入口：reaper 的 SQL 臂在 claim 为空、无未终结 `claude-worker-turn` 行（首 turn 行与 session 同事务写入）且绑定失效时发现已结束的无界面 session；绑定按既有 op 目标证明判定，首 turn 未 stamp 的窗口不算失效。它复用 `converge_dead_worker` 这一权威分派器（`reaper/mod.rs:381-391`、`:445-468`），全部兜底都在分派器同一写事务的当前性围栏之内；验收经该臂进入，覆盖无 spawn op、缺绑定、竞争失败、替代者不受影响与两个未 stamp 窗口（该文 §3.7，WA2）。terminal-worker 的 Pipe 行没有 session 租约。
- **WR5 `ResumeEligible` 的 D7（boundary 第三轮 N1，非阻塞）。** 起点的 D6 把 `ResumeEligible` 留给恢复路径，而 D7 对 P1 无条件终结 session，
  与 `calm-types/src/worker.rs:188-189` 的定义（线程仍活、可重新接上）矛盾。要求：`ResumeEligible` 保持 session 可重新接上，不写终态；表驱动测试
  逐个判词写明 D7 的状态与解释。
  **路线 A 处置：删除。** 没有决策树，也没有 provider 产生 `ResumeEligible`。
- **WR6 保留第二轮已收敛的机制。** 第三轮两路都确认下列机制在设计层面成立，W 重写时保留，除非上述需求要求改动：P1/P2 共用、原样保留 resumable
  死亡闸门的一棵退出决策树（第二轮 boundary B1 / R2-B2）；`TerminalObservation` 的事实分工，即 SQL 结算是退出权威、运行期阶段经 `Engine::output`
  Observe 取得、不从“未结算”推断 Running（第二轮 boundary B2 / R2-N2）；worker 执行 owner 在 `LeaderExited` 时请求 Stop 排空，leader 退出不授权
  task 写入；projection 只终结 Ephemeral session；释放授权围栏（第一轮 adversarial B4）、PTY 字节保真握手与输入门（第一轮 boundary B2）及其负面测试
  （第二轮 R2-N3）；只靠 C4.0d 节拍恢复的两条 worker 用例（第二轮 R2-B1）。
  **路线 A 处置：** 释放授权围栏保留在 Held→Releasing 的同一写事务里：owner 层 `release::send` 接受调用者授权（`require_live_claim_tx` + `require_attempt_startable_tx` + 排除 Verifying 的 worker 写者条件 + card/session/租约绑定），失败同事务 Stop、不发字节；准入事务另有同一道检查；Releasing 永不重发（该文 §3.3，先例 `operation/gate_execution.rs:151-183`）。SQL 结算仍是退出权威；Claude 的 PTY 握手与输入门删除；terminal-worker 改用 gate 的 Held `go` 释放，不需要 PTY 握手。
- **WR7 开放问题对 worker 的效果。** 用户对 C4 的 Q1、Q2 作出决定后，W 设计须单独写明它们对 worker 的效果：executor 不可用时 worker 是否在 mint
  与准入之前被拒（Q1）；同宿主 supervisor 崩溃后 typed worker 终端是否占用到宿主重启（Q2；若选 executor 同宿主正向排空，该片须先于迁移 worker
  的片合入）。没有载荷通道的 terminal-worker 遇到带认证的代理值时在准入前拒绝，这一处置（Q8 在 worker 上的部分）也属 W。
  **路线 A 处置：** Q1 新 attempt 在 mint 与取得租约之前因 `ExecutionUnavailable` 拒绝、不回退 Legacy，升级前的 Legacy 行不受此检查；Q2 与 Planner 相同，Unknown 失败关闭到宿主重启，task 由超时或取消收敛，claim 与租约保留到宿主重启；Q8 Claude 走载荷，terminal-worker 在准入前拒绝。

**设计起点（第三轮评审时的 worker 设计，按上列需求重写；原文保留，以免丢失已收敛的部分）。**

- **共同决定。** 迁为 typed PTY execution，复用 E2b 的 typed 终端：alias `term:<terminal_id>`、consumer 与 projection 复用
  `terminal_execution_create::CONSUMER`/`TerminalProjection`（`server/execution_boot.rs:26-28`；settle 写 `terminals` 退出字段与会话），模式在创建时冻结
  （`Terminal::is_typed`，`truth/model.rs:227-232`），typed renderer 经 `attach_typed_terminal`（`routes/terminal.rs:187`）。
  - 记录：`terminal_launch` 的新 typed 状态 `Admitted{execution_id, terminal_id, release, decision}`（扩展 `RequestState`，`terminal_launch.rs:10-32`），
    release ∈ `NotRequired | Held | Releasing{authorized_at_ms} | Released{authorized_at_ms}`，decision ∈ `Observing | Stopping{reason, at_ms}`，
    reason ∈ `ReleaseRefused | ReleaseInterrupted | Superseded | KernelRestarted`。所有写入是整记录 CAS，SQL 在 operation repo 的既有
    `tx_output` 写入路径中（同 C2b/C3），Stopping 与 `Engine::request(Stop)` 同事务。
  - 准入：worker op 的 `spawn_side_effect` 在 SpawnStarted 的原 claim 下，一个写事务内 `require_live_claim_tx`（`operation/repo_sqlite/writes.rs:115-122`）+
    `require_attempt_startable_tx`（`task_recovery.rs:14-36`，经 `task_launch.rs:58`）+ `Engine::admit` + `Admitted{…, Held|NotRequired, Observing}`；
    提交后 drive。op 在 Spawned receipt 与 release 完成后才返回 Ready，`drive_spawn` 等待 op（`scheduler/mod.rs:1139`）的合同不变，只是更慢。
    op 不 park，execution 活过 op（同 typed 终端活过 `terminal-execution-create`）。
  - **释放授权围栏（修第一轮 adversarial B4）。** Held→Releasing 是一个写事务内的 CAS：`require_live_claim_tx(op)`（含 lease 未过期与
    `completed_at_ms IS NULL`）+ `require_attempt_startable_tx(task)`（仍是该 key 的当前 attempt 且状态为 Dispatched/Running/Verifying）+ 记录精确等于
    `Admitted{同一 execution_id, terminal_id, Held, Observing}`，且该 terminal 仍绑定同一 card 与 worker session；三者都过才写 Releasing，提交之后才可能
    发出载荷的第一个字节（同 gate 先例 `operation/gate_execution.rs:174-205`）。card token 已撤销时，重新 mint 放在**同一事务**、两道守卫之后：
    mint 从不代替 task 授权，不可启动的 attempt 不会得到新 token。守卫失败 → 同事务写 Stopping{Superseded} + Stop，结算后走既有补偿。
    Releasing→Released 同样要求 live claim；失去 claim 的驱动者停止写入、留下 Releasing。**任何后继（boot 重驱、lease 过期后的新 claimant）
    看到 Releasing 都写 Stopping{ReleaseInterrupted} + Stop，永不重发**；看到 Held 只能经上面同一个 CAS 释放，不经别的路径。lease 在 Releasing
    提交后过期的竞争：原驱动者可能仍把载荷写给它在授权时刻已被允许启动的那个进程，而后继随即 Stop 它；不会有第二个进程或第二次授权。
  - 环境对等：Legacy PTY 继承 supervisor 的环境白名单（`child_environment.rs:5-49`），Engine helper 只用 intent 的环境（`supervisor/execution/scope_helper.rs:113-121`），
    今天的 typed 终端只有 `TERM` 三键加调用方 env（`routes/terminal.rs:153-171`），没有 HOME，Claude 登录、`~/.claude` transcript 与 git/ssh 都会坏。
    Wa 让 `launch_specification` 按 supervisor 拥有的同一张白名单（改为 pub，单一来源）从 kernel 环境取值写入 intent；这同时修复 opt-in typed
    终端的缺口（行为变化，PR 说明）。值来自 kernel 而非 supervisor 的环境；二者是否同源（neige-app 分别启动它们）由实现核实，差异写进升级说明。
    这部分是独立片 Wa：白名单取值放进 C4.1 已建立、含代理分类函数的 `server/execution_environment.rs`，生产消费者是已交付的 opt-in typed 终端
    （`terminal_execution_create.rs`）；opt-in typed 终端今天没有任何代理键，Wa 给它非秘密代理值，带认证的代理值照今天不传（逐键显式分类并记结构化警告，
    不是静默降级：它从未有过这些值），worker 的“准入前拒绝”政策在 Wc 随 worker 消费者生效。代理分类函数 `proxy_value_is_secret` 由 C4.1 引入，Wa 只加白名单取值并复用它。
    新测试 `w_typed_terminal_env_matches_legacy_allowlist`、`w_typed_terminal_omits_authenticated_proxy_explicitly`。
    代理值按 C4 设计 Q8 的分类：非秘密进 intent；带 userinfo 的在 Wc 的 terminal-worker 上准入前拒绝，Wd 起 terminal-worker 遇到秘密代理时与
    claude-worker 走同一 PTY 握手（release 为 Held），否则 NotRequired。
  - **凭据的 PTY 传输（修第一轮 boundary B2）。** claude-worker 与 codex viewer 的 MCP token（以及秘密代理值）不进 intent。PTY 的 line discipline
    不是字节保真的：canonical 模式行长上限 4095，`VERASE`/`VKILL`/`VWERASE`/`VLNEXT` 做行编辑，`ICRNL` 把 CR 改为 NL，`ISIG` 把 ^C/^Z/^\ 变为信号，
    `IXON` 吞掉 ^S/^Q（评审以 `/bin/sh`、`stty -echo` 与 `IFS= read -r` 实测：5001 字节行变为 4095 字节，`1abc\x15def` 变为 `def`，`1abc\rdef` 变为 `1abc`）。
    只关回显不够，所以 wrapper 进入字节保真模式并精确恢复：(1) `neige_tty=$(stty -g) || exit 75`，`stty raw -echo -iexten || exit 75`（raw 即
    `-icanon -isig -ixon -icrnl -inlcr -igncr -istrip -opost`、`min 1 time 0`；非 canonical 时 MAX_CANON 不适用，编辑、信号与流控字符都只是普通字节）；
    (2) 两条都成功后才输出布防标记（实现选一个 renderer 与 model view 都忽略的 OSC 序列，以测试钉住其不可见）；(3) 按共享载荷合同逐键
    `IFS= read -r`（合同仍禁止 NL 与 NUL、总长 ≤16KiB，在首字节前拒绝；载荷编码与 Pipe 相同，不另设 PTY 编码）；(4) `stty "$neige_tty" || exit 75`
    精确恢复原 termios，再输出撤防标记，然后 `exec` provider。worker wrapper 不重定向 stderr（PTY 上它本就是屏幕）。kernel 侧：Held→Releasing
    提交后，在 Engine 输出中跨块增量匹配布防标记（上限 10s），看到后才经 `interact(WriteStdin)` 按 `accepted_bytes` 续写载荷，再等撤防标记
    （上限 10s），然后才 Releasing→Released；任何输出缺口、EOF、超时、部分或不确定的写入 → Stopping{ReleaseRefused} + Stop，不重发。N_TTY 读缓冲为
    4KiB，raw 模式下缓冲满时主端写入被反压而不丢字节，部分写由 `accepted_bytes` 续写覆盖；这一点由下列测试证明，不靠推断。
    **输入门**：typed renderer 的唯一 writer（浏览器按键、MCP 终端输入工具、renderer 为终端查询生成的应答都经它）把 E2b 的“观察到 Spawned
    之后才建立 writer”（workspace-execution.md §6.4 终端交互）收紧为“release 为 Released 或 NotRequired 之后”。Released 只在撤防标记之后写入，所以
    用户字节只会在 termios 恢复之后到达，进入恢复后的模式（或 provider 随后自设的模式），既不会在 raw 窗口内被当成载荷读走，也不会在回显关闭时丢失。
    载荷从不回显、从不进入 OutputLog、renderer 屏幕或日志。terminal-worker 无秘密时 release 为 NotRequired，不走握手。
    真实 executor PTY 测试：`w_pty_payload_round_trips_every_byte`（0x01–0xFF 除 0x0A 的全部字节，含 CR、^C、^Z、^\、^S、^Q、^U、^W、^V、^H、0x7F、^D）、
    `w_pty_payload_boundary_lengths`（单值 4095、4096、4097 与 16KiB；16KiB+1 在首字节前拒绝）、`w_pty_termios_restored_exactly`（exec 后的 `stty -g`
    与布防前逐字相同，之后的交互输入照常回显并按行送达）、`w_pty_stty_failure_fails_closed_before_marker`、`w_pty_marker_split_across_chunks`、
    `w_pty_marker_timeout_stops_without_payload`、`w_pty_input_gate_covers_every_writer`、`w_pty_payload_is_never_echoed`（OutputLog 与屏幕都不含载荷）。
  - **boot 与重驱矩阵（修第一轮 boundary N5）。** typed 行跳过 `reconcile_supervisor_on_boot` 的 Probe（E2b 已有）。

    | 记录 / 事实 | 动作 |
    |---|---|
    | Held，attempt 仍可启动 | 在新 claim 下只经上面的 CAS 释放一次（worker 没有“已执行的 step”，今天 maybe-issued 的重驱也是 attach 继续） |
    | Held，attempt 已终结、被取代或取消 | Stopping{Superseded} + Stop，不 mint；结算后既有补偿（`compensate_worker_rows` 只在结算之后执行） |
    | Releasing | Stopping{ReleaseInterrupted 或 KernelRestarted} + Stop，永不重发；结算后补偿，attempt 判 infra 失败 |
    | Released 或 NotRequired，Spawned | attach，SpawnSucceeded（幂等） |
    | 已结算（Drained/NoChild），Released 之前 | 视为启动失败：补偿 |
    | 已结算，Released 之后 | 快速退出保留（`worker_cleanup.rs:13-24`）改读结算；SpawnSucceeded，退出由下面的退出收敛处理 |
    | executor 不可用 | op 留在 SpawnStarted，按 C2b.0 的 owner 无关错误留存只写 `last_error`、保留 lease 作退避；不回退 Legacy |
    | Unknown（同宿主 supervisor 崩溃） | 不结算、不 Retire、不补偿；观察端口报 Unknown，task 由既有 liveness 期限失败，终端占用到宿主重启（Q2） |
    | 记录损坏或无法解码 | 同 C2b.0 留存 `last_error` 加退避，不猜测、不重发 |
    | op 被取消或 lease 丢失 | 由两道守卫与 CAS 处理：已 Held → 后继按前几行；正在 Releasing → Stop |
    | codex viewer（We）准入后失败 | 今天失败被吞为 NoOp（`codex_adapter/mod.rs:1310-1313`）；改为在记录 NoOp 的同一事务里对已准入的 viewer execution 写 Stopping{ReleaseRefused} + Stop，再吞掉错误；Retire 仍归 `terminal_delete_tx`（`w_viewer_failure_after_admission_stops_viewer`） |
  - **存活观察 `TerminalObservation`（修第二轮 boundary B2 / adversarial R2-N2）。** `provider/src/worker/{claude.rs:45,terminal.rs:40,codex.rs:108}`
    今天直接用 Legacy `Probe`，而 supervisor 对 typed alias 拒绝该访问（`calm-proc-supervisor/src/lib.rs:661`、`execution/mod.rs:465`），结果恒为 Unknown
    （`provider/src/worker/supervisor.rs:89`）。只读 SQL 也不够：`workspace_executions` 只持久化 desired、intent 与结算（`truth/migrations/0170_workspace_executions.sql:1-13`），
    运行中、未 spawn 与同宿主 supervisor 崩溃后的 Unknown 在 SQL 中完全相同；Unknown 是 executor journal 的事实（`calm-proc-supervisor/src/execution/mod.rs:182-188`），
    leader 退出（`LeaderExited`）与排空（`Drained`）也由 scope helper 分别报告（`execution/scope_helper.rs:169-191`）。所以端口按事实来源分工：
    **SQL 结算是退出的 durable 权威，executor 观察是运行期阶段的权威**，不加持久化的运行期阶段镜像。Wb 在 provider 合同 crate 的 `SpawnCtx`
    （`calm-exec/src/provider.rs:26`）旁声明 `TerminalObservation::observe(terminal_run_id) → Starting | Running | LeaderExited{status} | Settled{evidence: ExitEvidence} |
    Unknown{reason} | Unavailable{reason}`，calm-server 在 `server/worker_observation.rs` 实现，Wb 只有 Legacy 实现（仍经 supervisor `Probe`：Running → Running，
    Exited → `Settled`，证据保持今天的 `ExitSource::Probe` 与 `-1` 哨兵，Unknown → Unknown，观察结果与今天逐一相同），Wc 加 typed 实现：
    (1) 先在 calm-truth 读该 terminal 的当前 alias execution 及其结算；已结算 → `Settled`，证据取自 `Settlement`：`Drained{status: Some(c)}` → 退出码 c；
    `Drained{status: None}` → 信号退出（scope helper 的 status 是 leader 的 `ExitStatus::code()`，信号时为 None，与 `terminal_settled_tx` 的
    `signal_killed = exit_code IS NULL` 同一规则，`truth/db/sqlite/workspace_execution.rs:197-208`）；`NoChild`、`Revoked`、tombstone → 无退出码、非信号（结果未知）；
    来源 `ExitSource::Daemon`。(2) 未结算 → 经既有 Engine 端口 `Engine::output(id, Some(u64::MAX))` 取运行期状态（supervisor 把 cursor 夹到当前末尾，
    `supervisor/execution/output.rs:48-57`，所以不取字节；与 typed renderer、`drain_exited` 走同一 Observe 路径，`engine.rs:283-317`）：无 receipt、Prepared 或
    Forking → `Starting`；Spawned → `Running`；`Exited{status}` → `LeaderExited{status}`；`Unknown{reason}` → `Unknown`；Drained/NoChild/Revoked 而 SQL 尚未结算 →
    `Unavailable{"settlement pending"}`（等 Engine 写下结算，下一轮再看）；缺 Engine、`!available()`、Transport/Refused/Missing → `Unavailable`。
    `Running` 只来自正面的 Spawned 观察，从不从“没有结算”推断；Unknown 与 Unavailable 都是未证明。provider 只换取证来源，解释不搬：`Running` 照今天的
    存活解释（Claude 的 Alive、Codex 的线程状态），`Settled` → `Liveness::Exited{evidence}`，`Starting`、`LeaderExited`、`Unknown`、`Unavailable` → `Liveness::Unknown`
    （保守不收割；`LeaderExited` 表示仍在排空，从不是 Exited）。不改 Engine 合同、不加 Engine 政策。
  - **leader 退出排空（worker 执行 owner，Wc）。** `Desired::Run` 加 `Exited` 时 Engine 只 Wait（`workspace-runtime/src/execution/mod.rs:114-125`），
    今天的 typed renderer 也只在 Drained 之后发布退出、从不请求 Stop（`terminal_renderer/typed.rs:303-330`），`terminal_sweeper.rs:115-164` 只处理孤儿与已结算的 task；
    所以 leader 退出而 setsid 子孙仍在、又没有 renderer 时，execution 永不结算，task 义务也永不出现。worker 的政策归 worker 执行 owner
    （`server/operation/worker_execution.rs`，Wc 的 typed worker 路径所在模块）：**worker execution 的业务是它的 leader，leader 退出即该 execution 结束，
    子孙只是待排空的残余**（codex viewer 同理：viewer 退出结束的是 viewer execution，业务线程不在其中）。发现：calm-truth 查询 `unsettled_worker_executions`
    （consumer 为终端、alias 绑定的 terminal 属于 worker session、该 worker op 已完成、执行未结算；Held/Releasing 的记录仍归 spawn op 与重驱矩阵），
    reaper 每轮 tick 先调用 owner 的 `drain_exited_workers`，再做下述退出收敛；typed renderer 观察到 `Exited` 时调用同一函数，只是加速器。对每行 `observe`：
    `LeaderExited` → 一个写事务内经 calm-truth 复核该 execution 仍未结算、仍是该 terminal 的当前 alias 且 desired 为 run，`Engine::request(Stop)`，提交后 drive；
    Desired 单调取 max，renderer、tick 与终端删除并发写入无害；不需要 op claim：op 在 Ready 时已结束，leader 退出后的 Stop 只授权排空，不触及释放围栏。
    `Unknown` 不请求 Stop（Engine 对 Unknown 恒 Wait）、不动 task。结算后 `TerminalProjection` 在同一事务写退出字段与 session，task 由下述决策树处理；
    **leader 退出本身从不授权 task 的任何写入**，只有结算（`Drained{status}` 携带 leader 的 status）才是退出证据。
    同片修正 projection 的 session 规则：`terminal_settled_tx` 今天把绑定的每个 starting/running/idle/turn_pending session 都写成 `exited`
    （`truth/db/sqlite/workspace_execution.rs:209-219`），而 Legacy 只终结 Ephemeral session（`terminal_sweeper.rs:38-58`）；Wc 让它在 calm-truth 中采用同一规则
    （只终结 `mode = 'ephemeral'`），Resumable session（codex viewer 所绑定的业务 session）的终结仍只归死亡仲裁。E2b 的 opt-in typed 终端不绑定 Resumable session，行为不变。
  - **退出决策树（两条发现路径共用，修第二轮 boundary B1 / adversarial R2-B2）。** 实现一次，放在今天拥有 worker 死亡收敛的 reaper 中
    （`server/reaper/exit_policy.rs`，取代 `reaper/mod.rs:140-358` 的内联分支）；Wb 先让既有发现路径 P1（`sweep_all` 扫 `sessions_nonterminal`，
    `reaper/mod.rs:103`）改用它，Wc 加第二条发现路径 P2 并调用同一函数。输入：worker session、其绑定 attempt（task id、状态、kind、worker card）与
    `TerminalObservation` 的物理事实。没有 provider 名字分支：会话模式取自 provider 声明的 `session_mode()`，解释取自 `interpret_exit`，task 的结果通道取自
    scheduler 拥有的 task kind 规则。

    | 步骤 | 条件 | 动作 |
    |---|---|---|
    | D1 归属 | session 为 Starting，或其 spawn op 未完成 | 只记观察；spawn op 与 boot/重驱矩阵拥有（今天 `reaper/mod.rs:138-160`） |
    | D2 物理事实 | Starting、Running、LeaderExited、Unknown、Unavailable | 不判；LeaderExited 交上面的排空；只记观察 |
    | D3 死亡确认 | provider 声明 Resumable | 原样搬入既有闸门：busy 状态或 `deadline_ms` 内有活动 → 不收割；无 `thread_id` → 不收割；`confirm_durable_death` 不是 `Dead` → 不收割（`reaper/mod.rs:167-255`）；都只记观察、不写 task |
    | | Ephemeral | typed 的结算或 Legacy 的 Probe Exited（今天的语义）即死亡证据 |
    | D4 解释 | 通过 D1–D3 | `provider.interpret_exit(session, evidence)`，唯一的退出权威 |
    | D5 task 守卫（WR1、WR4 须重设计） | 无绑定 attempt，或 attempt 不是 Dispatched/Running（终态；Verifying 由 gate 拥有） | 不写 task；Verifying 的 lease 已在自报事务中释放（`decision_sink.rs:183-201`），无 lease 动作；只做 D7 |
    | | 同一 card 有更新的未终结 worker session（如手动 `claude-restart`，`claude_restart_adapter.rs:60` 让 task worker 的 card 带着 kernel MCP 重启） | 旧退出已被取代，不写 task |
    | D6 按结果通道与判词 | task kind 为 Terminal（结果通道是退出码，scheduler 拥有，`scheduler/mod.rs:2110-2113`） | Completed 与 Failed 都调用 scheduler 的 `complete_terminal_task`（`scheduler/mod.rs:2140-2200`，与 live hook 和 sweep 臂同一守卫事务：退出 0 成功、gated → verifying；非零、信号、Probe 的 −1 → failed）；从不调用 `converge_dead_worker` |
    | | 其它 kind，`Failed{reason}` | `converge_dead_worker`（`reaper/mod.rs:374`：失败加 lease 释放，守卫只许 dispatched/running，`truth/db/sqlite/task.rs:626-647`） |
    | | 其它 kind，`Completed` | 按 `ExitInterpretation` 合同“不发回退事件”（`calm-types/src/worker.rs:182-183`）：不写 task，task 留给自报或 running liveness 期限 |
    | | `PreserveCard` | 不写 task，保留 card，记下判词（今天的 reaper 同样只记观察，`reaper/mod.rs:327-356`） |
    | | `ResumeEligible` | 不写 task，留给恢复路径 |
    | D7 session 终结（WR4、WR5） | D6 写入提交之后，或 D5/D6 不写 task 时 | P1 照今天“先收敛、后终结”调用 `session_commit_exit`（`reaper/mod.rs:296-330`）；P2 的 Ephemeral session 已由 projection 在结算事务内终结 |

    今天 reaper 对 `Completed`/`PreserveCard`/`ResumeEligible` 只记观察，因为 Probe 的 −1 哨兵产生不了它们（`reaper/mod.rs:327-337`）；typed 结算带真实退出码之后它们
    都会出现，上表逐一给出处置。P1 的唯一行为差异（WR4 证明它不是唯一的）是 Terminal 类 task 改经 `complete_terminal_task`（今天经 `converge_dead_worker`，status detail 不同），写进 PR。
    **Claude 退出 0 而未自报的政策（WR3 须先解决重启；owner：`ClaudeProvider`，`provider/src/worker/claude.rs:58-70`）。** Claude task worker 没有退出码结果通道：
    `claude_interpret_exit` 保持退出 0 → `Completed`，即不发回退事件，task 留在 Running，由 running liveness 期限（idle 1h、上限 8h，`scheduler/worker_liveness.rs:22-25`）
    以 `worker-timeout` 收敛；不选“退出 0 即失败”，因为 task worker 的 Claude card 可以手动重启并继续对同一 task 自报（`claude_restart_adapter.rs:60`），立即失败会抢掉
    这条路径。这就是今天的行为（`scheduler::live_dispatch_claude_does_not_reconcile_recorded_pty_exit` 钉住），Wd 保持并以无 renderer 的新测试钉住。非零与信号
    退出仍是 `Failed` → `converge_dead_worker`，符合合同；今天 Legacy 的 attach reader 先终结 session、reaper 看不到，所以这对 Claude 是“更早失败”的行为变化，写进 PR 与升级说明。
    政策只在 Claude 的 `interpret_exit` 里，决策树不按 provider 名字分支。
  - **第二条发现路径 P2：按 task 义务（修第一轮 boundary B4）。** `TerminalProjection` 在结算事务内终结 Ephemeral session（`truth/db/sqlite/workspace_execution.rs:209-219`，
    规则见上），而 P1 只扫未终结的 session，scheduler 只对 `TaskKind::Terminal` 读已持久化的退出（`reconcile_running_terminal`，`scheduler/mod.rs:1838-1898`），
    Claude task 只能等 liveness 期限。Wc 加 calm-truth 查询 `worker_exit_obligations`：attempt 为 Dispatched/Running（Verifying 不在其中）、kind 不是 Terminal
    （Terminal 类已有 scheduler 的 durable 臂）、spawn op 已完成、绑定 session 已被 projection 终结且其 typed execution 已结算。键是**未完成的 task 义务加 SQL 结算**，
    不是 session 状态；每行以 `TerminalObservation` 的 `Settled` 证据进入同一决策树（D3 对 Resumable 照样生效，但 Resumable session 不会被 projection 终结，
    始终留在 P1）。task 只由 D6 的守卫写入改变，所以结算与 task 写入之间任何一步崩溃，下一轮 30s reaper 都会再次看到同一义务；判为不写 task 的行（如 Claude 的 `Completed`）每轮重判、无写入、幂等，行数以活 task 为界，直到自报或 liveness 期限使 attempt 离开 Running。（WR2 须重设计）typed 行的 `pty_output`
    （`complete_terminal_task` 的结果字段）由 worker 执行 owner 在结算后经 `Engine::output` 取尾写入 terminal 行（OutputLog 保留到 Retire，`output.rs:58-66`），
    scheduler 的 Terminal 臂在它写入之后才完成 task。
  - **测试。** Wb（P1 与端口，Legacy）：lib `reaper::exit_policy` 的 `w_exit_tree_table`（每个 `ExitInterpretation` × task kind × task 状态 × 会话模式，P1 与 P2
    的输入得到同一动作）、`w_exit_tree_resumable_requires_durable_death`（busy、近期活动、无 thread、仲裁 Alive/Unknown 各一例，都不写 task）、
    `w_exit_tree_terminal_kind_uses_complete_terminal_task`、`w_exit_tree_leaves_verifying_untouched`、`w_legacy_observation_is_unchanged`。
    Wc（terminal-worker，无 renderer，真实 executor）：`w_worker_exit_reaches_task_without_renderer`（退出 0、退出 1、信号、NoChild 与 kernel 重启后各一例，
    一轮内收敛）、`w_terminal_success_completes_through_scheduler_owner`（gated 时进入 verifying）、`w_leader_exit_with_setsid_descendant_drains_without_renderer`
    （leader 退出后 setsid 子孙仍在：排空请求 Stop、Drained 携带 leader 的 status、task 按决策树收敛）、`w_leader_exit_alone_never_changes_task`（LeaderExited 期间
    task 保持 Running）、`w_worker_observation_reports_unknown_after_supervisor_crash`（supervisor 被 SIGKILL 后重启：观察为 Unknown、不排空、不写 task、终端占用）、
    `w_worker_observation_unavailable_is_unproven`、`w_typed_worker_liveness_is_not_unknown`、`w_exit_convergence_survives_crash_between_projection_and_task`、
    `w_verifying_with_live_gate_is_not_touched_by_exit_scan`（worker 自报成功进入 verifying、gate 运行中、worker 随后退出：task 与 gate 都不受影响），以及只靠 C4.0d 节拍恢复的
    `w_worker_store_error_after_ready_recovers_by_tick` 与 `w_worker_lost_retire_drive_recovers_by_tick`（无重启、无用户操作）。
    Wd：`w_claude_zero_exit_without_renderer_keeps_task_running`（未自报：无 task 事件，liveness 期限后以 `worker-timeout` 失败，期间手动重启的 session 仍可自报）、
    `w_claude_nonzero_exit_fails_task_without_renderer`、`w_claude_self_report_then_exit_is_noop`。We：`w_live_codex_thread_with_exited_viewer_keeps_task`
    （业务线程活跃，viewer 退出 1、退出 0、信号、NoChild 与 viewer release 失败各一例，无 renderer 且含重启后：task 不变、session 不终结）、
    `w_viewer_settlement_keeps_codex_session_nonterminal`、`w_dead_codex_thread_after_viewer_exit_follows_verdict`（仲裁 Dead 后按判词处置）。
  - 停止：`stop_child`、`reap_terminal_pid_only`、`wait_for_pid_exit` 与 `try_reap_terminal_artifacts_with_renderer` 对 typed 行改走
    `execution_quiesce::quiesce(Terminal)`（E2b 的 settled-worker 分支已如此）；裸 PID 信号永不作用于 typed 行。
  - Retire：仍只有 `terminal_delete_tx` 一个写者（E2b）。worker 的“完成”是结算，占用在结算时结束；卡片保留到删除，删除时 Retire。这是终端
    类 consumer 对“每条完成路径驱动 Retire”的既有收窄，W 不改；C4.0d 的全局节拍同样保证这些行的 Stop 与 Retire 推进。
  - 删除的原生路径：该 worker kind 新启动的 Legacy `EnsureProc` 分支与 `terminal_launch` 的 Requested/HandedOff 写入；升级前的 Legacy 行仍由
    既有读者处理直到排空，C5 保留它们的 Legacy 检查。

- **切片起点（规模口径同 §3.2，“预计”按 C2b/C3 实测比；前置列已按 W 在 C5 之后改写，WR 列出该片动工前须先满足的需求）。**

  | # | 结果 | 前置 | owner 记录 | boot 矩阵 | release / 凭据 | 活性 | 删除的原生路径 | 规模（设计 → 预计） | 等级 |
  |---|---|---|---|---|---|---|---|---|---|
  | Wa | typed 终端的环境对等：在 C4.1 建立的 `server/execution_environment.rs` 中加白名单取值（supervisor 白名单改为 pub 的单一来源、从 kernel 环境取值），复用其 `proxy_value_is_secret`，`launch_specification` 改用它；生产消费者是已交付的 opt-in typed 终端 | C4 已合入（C4.1 的分类函数） | 无 | 无 | 非秘密代理进 intent，带认证的代理照今天不传（显式分类） | 无 | 无 | 环境模块 +60/−20（分类函数已在 C4.1）、`launch_specification` +20/−10、新测试 +90、`execution_hooks` 环境用例改写 +30/−20 ≈ 250 → 350–600 | L2：子进程环境白名单 |
  | Wb | `TerminalObservation` 端口（provider 合同 crate）与 Legacy 实现（仍经 Probe，观察不变），provider 构造注入端口；reaper 既有发现路径 P1 改用共享退出决策树 `server/reaper/exit_policy.rs`（唯一行为差异：Terminal 类 task 改经 `complete_terminal_task`） | C4 已合入；WR4 先行 | 无 | 无 | 无 | 既有 reaper 30s tick | 无（重构） | 端口与 Legacy 实现 +110/−40、provider 改接 +60/−40、决策树抽取 +200/−150、新测试 +230、provider/reaper 用例改写 +60/−60 ≈ 950 → 1300–1900 | L2：task 收敛政策 |
  | Wc | terminal-worker（W1）改为 typed PTY Engine 消费者，同片引入 worker typed 启动路径、`Admitted` 记录与释放授权围栏、代理分类（秘密代理准入前拒绝）、typed 停止分支、`TerminalObservation` 的 typed 实现、worker 执行 owner 的 leader 退出排空、projection 只终结 Ephemeral session、P2 按 task 义务发现 | Wa、Wb（C4.0d 已合入；PTY 握手随 Wd 引入）；WR1、WR2 | `terminal_launch` 的 `Admitted` 状态 | 见 worker boot 与重驱矩阵 | NotRequired | C4.0d 节拍 + leader 退出排空 + 决策树（P1/P2）+ scheduler 周期 sweep；renderer 退出加速 | terminal-worker 的 Legacy `EnsureProc` 新启动分支 | typed 启动路径 +560/−130、typed 观察实现 +70、排空与 `unsettled_worker_executions` +130、P2 查询与扫描 +90、projection 规则与 `pty_output` 写入 +40、新测试 +560、改写 +160/−130 ≈ 1870 → 3100–4700 | L2 |
  | Wd | claude-worker（W2）改为同一路径；MCP token 与秘密代理值走 PTY 字节保真握手；terminal-worker 遇秘密代理时也走它 | Wc（C4.0a 已合入）；WR3 | 同上 | 同上（Held 只经释放授权 CAS 释放） | token 载荷（raw 模式、布防/撤防标记、输入门） | 同上 | claude-worker 的 Legacy 新启动分支、EnsureProc env 中的 token | +480/−150、真实 PTY 测试 +460、Claude 退出政策测试 +70 ≈ 1160 → 2000–2950 | L2 |
  | We | codex-worker 的 viewer（W3）改为 typed PTY；业务线程不变（N6）；viewer 准入后失败先 Stop 再吞 | Wd | 同上 | 同上 | token 载荷 | 同上 | viewer 的 Legacy 新启动分支 | 迁移 ≈ 480、viewer 退出与活跃线程测试 +130 ≈ 610 → 1000–1550 | L2 |

  超过约 800 的片是 Wb、Wc 与 Wd。Wb：端口注入与 P1 决策树统一是一个 task 收敛政策变化，决策树的表驱动测试必须与它同片；它有既有生产消费者
  （reaper P1），可独立部署，但须先满足 WR4。Wc：typed worker 的准入、释放围栏、恢复、停止、leader 退出排空与 task 收敛是同一个不变量（每条结束路径
  都结算并收敛 task），任一部分缺失都会让首个 typed worker 卡住或绕过围栏，所以保持原子；WR1、WR2 会改变它的范围与规模。Wd：raw 握手、输入门与
  真实 PTY 测试必须同片，否则首个 claude-worker 的 token 不是字节保真的；WR3 未定之前它的范围也未定。规模在 W 重写设计时重估。

- **受影响的既有测试（起点清单，W 重写设计时按新范围更新）。**
  - Wa：runtime_suite `execution_hooks::{terminal_create,terminal_io,terminal_recovery}`（typed 终端的可见环境多出白名单键）；`calm-proc-supervisor` 的白名单
    改为 pub、内容不变（今天它没有单元测试，新测试放在 `w_typed_terminal_*`）。
  - Wb：lib `reaper::tests::*`（含 `converge_dead_worker_without_spawn_op_releases_workspace_lease`，`reaper/tests.rs:670`；Terminal 类 task 的期望改为
    `complete_terminal_task` 的结果）、`provider_registry` 的构造；`-p provider worker::`（`supervisor.rs:140`、`:194` 的 Probe 用例改为 Legacy 实现的用例）；
    `-p calm-exec truth_conformance`；api_suite `provider_conformance::*`（`ClaudeProvider::new(sock)` 等构造改为注入观察端口，“无 supervisor 时 Unknown”的断言意图保留）；
    runtime_suite `scheduler::{scheduler_worker_liveness, live_dispatch_claude_does_not_reconcile_recorded_pty_exit}`（断言不变）。
  - Wc：runtime_suite `scheduler::*`（`TerminalWorkerAdapter::new_with_spawn_hook`，`tests/scheduler.rs:954`，钩子须留在同一点或改用真实 executor）、尤其 `spawn_failure_marks_failed_and_emits_kernel_task_failed`、
    `boot_sweep_resolves_dispatched_terminal_with_recorded_exit_in_one_pass`、`sweep_marks_running_when_op_succeeded_before_crash`、`stale_spawn_started_worker_is_recovered_without_rechecking_context` 与嵌套的 `long_task_reliability`、
    `scheduler_running_worker`、`scheduler_worker_liveness`、`scheduler_runs_canceled`；`kernel_process_suite::{reconcile_supervisor_on_boot::*, kernel_reboot_harness::kernel_reboot_preserves_snapshot_and_reclaims_lease_without_duplicate_dispatch}`；
    `no_double_spawn::*`（计数的 spawn 钩子 `:393-793`）；`execution_hooks::{terminal_create,terminal_io,terminal_recovery}`（共用 projection；projection 只终结 Ephemeral session）；api_suite `terminal_ws_suite::{task_terminal::*, terminal_signals::*（含 worker_reap、worker_reap_attach）, terminal_sweeper::*, terminal_lifecycle_task_delete::*}`；
    lib `terminal_adapter::tests::launch_cleanup_tests::{launch_abort_ui_and_boot_never_resend_ensure, spawn_failed_definite_ack_compensates_worker_rows, spawn_failed_uncertain_receipts_retain_ownership, launch_successful_fast_exit_preserves_worker_rows}`
    （`operation/launch_cleanup_test_support.rs` 的 EnsureProc 夹具需要 typed 对应物）、`terminal_adapter::tests::{disposal_tests, pty_cwd_tests}`；mcp_integration_suite `task_recovery`。时序：op 要等 Spawned receipt；快速退出保留
    （`worker_cleanup.rs:13-24`）改读结算，不再依赖 attach reader 先写退出；leader 退出后经排空才结算，`pty_output` 在结算后写入。lib `reaper::tests::*` 加 P2 与排空。
  - Wd：lib `claude_adapter::tests::{claude_worker_recovery_already_exited_returns_noop_without_respawn, claude_worker_spawn_env_carries_raw_card_token_and_socket}` 与 already-live、fast-exit 两组（token 断言改为送达的 env，且不在 intent）、
    `claude_adapter::tests::{worker_mcp_tests::*（含 :468、:508 的重启路径）, reader_head_tests::*}`；runtime_suite `scheduler::live_dispatch_claude_does_not_reconcile_recorded_pty_exit`、`worker_flow_claude_suite::*`（transcript tail 依赖 HOME 对等）、
    `worker_flow_driver_suite::*`；mcp_integration_suite `track_worker_cwd::*`、`track_worker_cwd_ends::{an_exited_worker_is_released_by_its_timeout_flip, a_lease_from_an_older_boot_fails_its_attempt_and_is_released}`、`planner_result_loop`；
    api_suite `claude_card_endpoint::resume`（`seed_task_worker_card`）、`terminal_ws_suite::task_terminal::reassigned_task_worker_and_manual_restart_do_not_bypass_execution_binding`、`provider_conformance`。
  - We：target `codex_worker_shared_daemon`（含 `worker_optional_viewer_failure_preserves_business_and_owned_cleanup`）；lib `codex_adapter::tests::{viewer_cleanup_tests::*, reader_head_tests::*}`；`codex_e2e_suite` 只在 CI/专用宿主。
- **新测试（起点清单；名字是实现时注册的新过滤器，W 重写时按 WR1–WR5 增补）。**
  Wa：`w_typed_terminal_env_matches_legacy_allowlist`、`w_typed_terminal_omits_authenticated_proxy_explicitly`。
  Wb：`w_exit_tree_table`、`w_exit_tree_resumable_requires_durable_death`、`w_exit_tree_terminal_kind_uses_complete_terminal_task`、
  `w_exit_tree_leaves_verifying_untouched`、`w_legacy_observation_is_unchanged`。
  Wc：`w_worker_has_one_engine_construction`、`w_worker_env_matches_legacy_allowlist`、`w_worker_release_requires_live_claim_and_startable_attempt`
  （lease 在授权前过期、attempt 被取代、task 已终结三例：均不发字节、不 mint）、`w_two_claimants_release_once`、`w_worker_boot_after_cancel_never_releases`、
  `w_terminal_worker_refuses_authenticated_proxy_before_admission`、`w_typed_worker_is_never_signalled_by_pid`，以及“退出决策树”下的测试段列出的 Wc 用例
  （无 renderer 的退出收敛、Terminal 成功经 scheduler owner、setsid 子孙的 leader 退出排空、leader 退出不改 task、supervisor 崩溃后 Unknown、Unavailable 未证明、
  projection 与 task 之间崩溃、Verifying 加活 gate、只靠 C4.0d 节拍的两条恢复）。
  Wd：`w_worker_boot_held_releases_once`、`w_worker_boot_releasing_never_resends`、“凭据的 PTY 传输”下列出的 8 条真实 PTY 测试，加负面用例
  `w_pty_termios_restore_failure_fails_closed`、`w_pty_markers_in_provider_output_after_exec_are_ignored`、`w_pty_signal_and_resize_during_arming`（均不提前开门、任何输出不含载荷），以及
  `w_claude_zero_exit_without_renderer_keeps_task_running`、`w_claude_nonzero_exit_fails_task_without_renderer`、`w_claude_self_report_then_exit_is_noop`。
  We：`w_viewer_failure_after_admission_stops_viewer`、`w_live_codex_thread_with_exited_viewer_keeps_task`、`w_viewer_settlement_keeps_codex_session_nonterminal`、
  `w_dead_codex_thread_after_viewer_exit_follows_verdict`。
  mutation 起点：worker 的 Held→Releasing 去掉 `require_attempt_startable_tx`、PTY wrapper 去掉 `raw`（只留 `-echo`）、布防标记出现前写载荷、
  reaper 去掉 P2 义务扫描、决策树去掉 D3 的 Resumable 闸门（预测 `w_exit_tree_resumable_requires_durable_death` 与
  `w_live_codex_thread_with_exited_viewer_keeps_task` 变红）、D6 把 `Completed` 送进 `converge_dead_worker`、观察端口把 `LeaderExited` 映射为 `Settled`、
  排空去掉 `Engine::request(Stop)`、projection 恢复终结 Resumable session；W 重写时为 WR1 的事务围栏、WR2 的输出义务与 WR4 的 lease 清理各加一项，
  每项先按名字写出完整的预测红集。
- **gate 目标（起点；“加”的含义同 C4）。**
  Wa：`runtime_suite execution_hooks::`、`-p calm-proc-supervisor`（白名单改为 pub，supervisor 的 Legacy 环境用例不变）与 `w_typed_terminal` 过滤器；
  Wb：lib `reaper::`、`provider_registry`、`-p provider worker::`、`-p calm-exec truth_conformance`、`api_suite provider_conformance::`、
  `runtime_suite scheduler::`（`scheduler_worker_liveness` 与 Terminal 类收敛）与 `w_exit_tree`、`w_legacy_observation` 过滤器；
  Wc：Wa 与 Wb 的全部，加 `runtime_suite`（`scheduler`、`kernel_process_suite`、`no_double_spawn`、`execution_hooks`）、`api_suite terminal_ws_suite::`、
  `mcp_integration_suite task_recovery`、lib `operation::terminal_adapter::` 与 `w_` 过滤器；
  Wd：Wc 的全部，加 `mcp_integration_suite track_worker_cwd`、`mcp_integration_suite planner_result_loop`、`runtime_suite worker_flow_`、`api_suite claude_card_endpoint::`、
  lib `operation::claude_adapter::`；We：Wd 的全部，加 `codex_worker_shared_daemon`、lib `operation::codex_adapter::`。
  调用者审计的生产符号：Wa `launch_specification|child_environment|proxy_value_is_secret`，Wb `TerminalProvider|ClaudeProvider|CodexProvider|probe_terminal_liveness|converge_dead_worker|interpret_exit|sessions_nonterminal|complete_terminal_task`，
  Wc `TerminalWorkerAdapter|RequestState|terminal_settled_tx|TerminalProjection`，Wd `ClaudeWorkerAdapter|worker_mcp|seed_task_worker_card|seed_non_root_session_with_provider|claude_restart`，
  We `CodexWorkerAdapter|OptionalViewer`；搜索范围同 C4。
- **架构五点与永久成本（起点）。** worker 退出的含义只在 provider 的 `interpret_exit` 与 `session_mode`（含 Claude 退出 0 的政策），task 的结果通道只在
  scheduler（Terminal 类经 `complete_terminal_task`），死亡收敛只在 reaper 的一棵决策树，leader 退出排空只在 worker 执行 owner；取证经注入的
  `TerminalObservation`：结算取自 SQL，运行期阶段取自 executor。worker 复用 E2b 的 typed 终端与 projection，Terminal 类 task 复用 scheduler 的唯一完成函数，
  失败复用 reaper 的 `converge_dead_worker`；环境白名单与代理分类同在 `execution_environment.rs`，白名单的单一来源是 supervisor 的表。
  `unsettled_worker_executions` 与 `worker_exit_obligations` 的 SQL 只在 calm-truth。永久成本：reaper 每 30s 多两次查询（未结算 worker execution 与
  task 义务），每个未结算 worker execution 一次 Observe；新增 worker provider 要实现 `interpret_exit`、`session_mode` 与存活观察的映射，由端口类型强制。
  WR2 若引入新的完成义务，须在 W 设计中另行列出它的查询与频率。

## 4. 隔离等级

### Legacy wind-down（W 之后的独立里程碑）

W 只迁移 task worker，尚不意味着 Legacy 清零。此里程碑先将 owner 自建 card 的启动迁移到已声明的 Engine owner；
升级入口在 listener/新准入之前排空升级前 Legacy 行，不能只等待用户逐个重启。覆盖 owner card 与 pre-upgrade 行的升级、
失败关闭和重试测试，记录最低升级路径后，才删除剩余 Legacy 占用/静默分支与 `EnsureProc` 启动协议。
Codex 的业务线程与 viewer 生命周期仍不同，Codex thread 检查保留，不以 PTY 或 execution 静默取代。
本片另做设计与 L2 评审，不扩大 C4.1/W，也不把兼容检查永久保留下来。

### 4.1 依赖分析

| 等级 | 依赖 cutover 的哪部分 | 结论 |
|---|---|---|
| `worktree` | 无（就是今天的行为） | 只需让它成为显式选择 |
| `none` | 无。保证 G（§4.2，含已启动 teardown 子进程跨崩溃存活的显式例外，#2862）由“每个 kernel effect 只以自有位置为目标”的既有守卫加新的非重叠准入共同给出；它不要互斥、占用、证据或交付，因此不需要 C1–C6 | **可以先在当前模型上落地** |
| `view` | S3 的 Revision/RuleView/FUSE 与 S2b 机制；推迟到 S2b 的 typed 启动目标与 WorkspaceContent（D5、D8）；回收私有层前的正向静默（C5），也就是 view 内允许的每种启动都要先成为 Engine 消费者（C2–C4） | 在 C5 与 S3 之后，作为第三个后端加入 |

typed 等级选择本身（请求字段、枚举、wire 投影、UI 选择）只依赖创建路由与模型，与 cutover 无关。

### 4.2 `none` 的设计

**自有位置与 kernel effect。** Neige 自有位置是：(1) managed 根 `workspace_root` 整体，含 system area 与 `<root>/.trash`；
(2) 每个 `tracks.workspace_worktree_path` 非空的 live 行的 track worktree 根 `<repo>/.claude/worktrees/track-<id>`；(3) 进行中的
teardown 目标：行已（或即将）随删除事务提交而消失、但 post-commit sweep 尚未执行完的 worktree 根（见下“进行中的 teardown
目标”）。自有 Git 元数据是该 worktree 的管理目录、分支 `neige/track-<id>`、`refs/neige/candidates/<track>/` 与 `refs/neige/gate-runs/<track>/`。kernel 的
创建、回收、清理、删除 effect（materialize、`ensure_track_worktree`、recycle、trash GC、`remove_track_worktree` 及其目录兜底、
`git clean -fdX`、candidate refs 删除、空 area 目录删除）的目标都是某个自有位置或自有 refs（§1）。

**保证 G（以下文显式例外为限）。** 对一个经准入的 `none` 目录 D：kernel 不创建、不回收、不 clean、不删除 D，也不删除 D 内任何不属于自有位置的文件。
依据：每个 kernel effect 只作用于自有位置；准入时 D 与全部自有位置（含进行中的 teardown 目标）互不重叠；`none` 行本身不是
自有位置，其所有破坏性入口是显式 no-op。进程内登记只能覆盖同一 server 进程内的 teardown 窗口，不能排除已启动子进程跨崩溃
存活后删除重启时新准入的 D；这是 G 的显式例外，留给 #2862 闭合。G 的新增前提只是这条准入时排除，不是持续的全局不重叠：准入之后唯一被接受的重叠是
下文“G 之外”的后来创建的 worktree，owned/owned 不重叠由 O1 保障（§3.1）。

**准入（I1b）。** 拒绝位于、等于或包含任何自有位置的 cwd，否则 400 并说明冲突的自有位置。实现复用 `AttachedInheritedPath`
的 lexical + canonical 双检查（`truth/db/sqlite/track.rs:41-77`）：把它的比较抽成一个函数，`AttachedInheritedPath::new` 继续只
检查“位于 managed 根内”（子 track 行为不变），`none` 的证明类型检查全部自有位置与三种关系。
自有位置集合在创建事务内、已持有 SQLite 写锁之后读取（`BEGIN IMMEDIATE`，与 `area_folder_create_checked` 同，`truth/db/mod.rs:697`；若创建事务今天不是 immediate，`none` 分支先取
写锁再读），包括 live worktree 行与进行中的 teardown 目标快照；SQLite 单写者保证并发创建的 worktree track 无法插在检查与写入
之间。比较用物理位置：存在的路径取 canonical；不存在的自有根取其最近的已存在祖先的 canonical 路径再接上剩余的 lexical 段，
不把整路径 canonicalize 失败当作“不重叠”，也不对已存在的符号链接祖先只做 lexical 比较；候选 cwd 本身必须存在，canonicalize
失败即拒绝。这一检查是一次文件系统观察，不是原子租约：复用 `AttachedInheritedPath` 的比较函数不构成对并发换链的证明。
其余准入不变：绝对路径、存在、是目录、位于 Git 工作树内（不顺带放开非 Git 目录）；跳过 `refs/heads/neige` 命名空间检查与 `git_repo_root_for_track_cwd`，两者只服务 worktree。准入只作用于新的 HTTP 创建，
不作用于旧行，也不作用于 keyed 重放。直接后果：只要仓库 R 上还有 worktree track，`none` 就不能选 R 本身（R 包含其 worktree 根），
拒绝信息说明原因。

**进行中的 teardown 目标（B1 窗口）。** 删除事务提交后、post-commit sweep 执行 `worktree remove --force` 之前，worktree 根 W
已不在任何 `tracks` 行中，但仍会被删除（§1）；只扫 live 行的准入会在这一窗口准入 cwd = W 或 W 的 Git 内子目录，随后被 sweep 连同
删除。因此：

- **Owner 与类型。** workspace-runtime 的 sweep 模块拥有一个进程内登记表 `sweep::TeardownTargets`（`Arc<std::sync::Mutex<…>>`
  的目标多重集，按 track_id 与 worktree 根记录），server 状态持有唯一实例并交给 Track/Area 删除与 `none` 准入。登记返回一个
  RAII 守卫；没有别的写者。
- **登记与移除（锁与持有区间）。** 删除路径在删除事务内捕获 `WorkspaceTrackSweep` 后、**COMMIT 之前**登记全部捕获目标（只在
  这个 std mutex 的短临界区内插入，不跨 await）；事务回滚时守卫移除登记（行仍是 live，live 行检查接管）；提交后守卫随 sweep
  一起移进 `spawn_blocking` 闭包，只在该目标的 `remove_track_worktree` 与 refs 删除执行完（含失败与 panic 展开）之后才 drop。
  上层 future 被取消不提前移除登记，因为阻塞任务仍会执行。
- **准入读取。** `none` 准入在持有 SQLite 写锁的创建事务内，于同一 std mutex 的短临界区内取登记表快照，与 live 行一起作为自有
  位置集合。正确性：若删除先提交，其登记早于提交、提交早于准入事务取得写锁，快照必含 W；若准入事务先取得写锁，删除无法在其
  之前提交，A 行仍 live 而被拒。两种顺序都不需要 area 锁（单 Track DELETE 不取 area 锁，跨 area 的 cwd 也成立）。
- **Area 删除与补偿。** Area 删除对其全部 `WorkspaceTrackSweep` 同样在 COMMIT 前登记、在 `finalize_area_recycle` 之后的 sweep
  执行完再移除；回滚或 panic 恢复（`restore_area_recycle_report`、Track 删除的 `restore_recycled_workspace`）路径在补偿执行完之后
  才移除登记。被回收与恢复的 managed 目录始终在 managed 根内（自有位置 (1)），不需要登记。worktree 创建发生在行提交之后
  （`routes/tracks.rs:1517`、`create.rs:661`），失败不删除目录，没有提交前的创建窗口需要登记。
- **崩溃。** 登记表不持久化：今天尚未启动的 post-commit sweep 在重启后永远不会补跑，这一部分只泄漏不删除。
  但已经通过 `std::process` 启动的 `git worktree remove --force` 没有 parent-death 或 crash drain 保证
  （`crates/workspace/src/teardown.rs:56-62`、`crates/workspace/src/command.rs:35-40`）：server 崩溃后子进程可继续存活，
  live 行已删除、登记表已丢失，重启后的 `none` 准入可放行仍存在的 W 或其 Git 内子目录，随后被原有 Git 子进程删除。这个窗口没有时长上界，
  Track/Area 删除同样受影响，是 G 的显式例外，与 C1 的孤儿 `git clean` 属同一残余风险类；留给 follow-up #2862
  （PDEATHSIG / child lifetime semantics）闭合。进程内登记只足以关闭同一进程内的窗口，本计划不新增持久登记或子进程机制。
  若 C6 引入持久释放作业，那些作业行就是持久的进行中 teardown 目标，同片并入准入的自有位置集合（启动补跑之前
  准入也必须看见它们）；这本身不证明旧子进程已退出或排空，不关闭上述例外。

**G 之外（明确不承诺）。**

- 已启动的 teardown 子进程在 server 崩溃后存活：重启时登记丢失，`none` 可准入其尚未删除的目标，随后仍被删除，风险时长无上界。
  这是 G 的显式例外，与 C1 孤儿 `git clean` 同类，待 #2862（PDEATHSIG / child lifetime semantics）闭合；见上“崩溃”。
- 准入之后，同一仓库的 attached-worktree track 仍在该仓库下创建自己的 worktree（可能落在 D 内）；删除它时移除那个 worktree
  根、管理目录与分支。这些只作用于新的自有位置，D 的其余文件不变。worktree 父的子 track 继承的来源目录本来就包含父的
  worktree，属同一情形。
- 历史 lease 的 Neige 自有 refs（见下“历史事实例外”）。
- Planner 请求的 forge/`git_commit` 在 `agent_cwd` 执行 `git add -A` 与提交（`server/mcp_server/transport.rs:1086-1110`、
  `crates/calm-types/src/forge_git.rs:26`）：这是 agent 发起的写入，不是 kernel 生命周期 effect；UI 文案因此不写“Neige 不触碰”。
- 外部进程在准入**期间与之后**改名、换链接（包括 canonicalize 之后、事务插入之前把 D 或其祖先换成指向自有位置的链接）：
  不防御，也不做身份重验；SQLite 锁不约束文件系统。这是有意的收窄，与 D1 删除 kernel FD 一致。
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
| `remove_track_worktree`、post-commit sweep 的 worktree 部分 | 该行 `worktree` 为 Some | `match`：只有 `Worktree` 删除；准入排除 live 根与进程内登记的 teardown 目标；已启动子进程跨崩溃存活为 G 的显式例外（#2862） |
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
当前分支），Neige 不加锁、不监控、不记录证据、不交付；在保证 G 的范围内，Neige 不会创建、清理或删除这个目录；
明确提示已启动的删除子进程可在 server 崩溃后继续运行，重启后选择的目录仍可能被其删除（G 的例外，待 #2862 闭合）；task worker 不可用。准入被拒时
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
| I1b | 请求字段、路由映射、非重叠准入、幂等形状、wire 投影与生成产物 | HTTP 创建 `none` → 无 worktree、无 branch、Planner cwd = `path`；反例：cwd 位于、等于或包含 managed 根、system area、`.trash`、managed track 目录、他人 worktree 根，以及经符号链接指向它们 → 400；缺失的自有根位于已存在的符号链接祖先之下时按物理位置比较仍拒绝；进行中的 teardown 目标：attached-worktree track A 的 Track 删除（以及含 A 的 Area 删除，各一）在删除提交之后、sweep 之前由稳定暂停点停住，在同 area 与另一 area 各以 cwd = A 的 worktree 根、以及其 Git 内子目录创建 `none` → 400；放行 sweep 并等它结束后同一 cwd（目录若被重建）可准入；删除事务回滚后登记被移除、A 行仍 live 而照旧拒绝；准入后 owner 不可触及：`none` 在 R 的子目录，同仓库的 worktree track 与 managed track 依次关闭（clean 计时器运行）、删除，Area 删除、trash GC 之后，`none` 目录逐字节不变；G 之外的情形：`none` 在 R，之后建并删除 R 上的 worktree track，只有那个 worktree 根与其分支消失；managed 带 `isolation` → 400；省略字段 = worktree；幂等：升级前 binding（带消息与无消息各一）以省略字段重放成功且 digest 字节不变，显式 `worktree` 同键重放成功，`none` 同键冲突，原目录已消失时重放不重新准入、不物化、不改选择；生成器产物无漂移。约 500 行（生成文件另列） | L2：API/持久选择 |
| I2 | FE 选择、徽标与准入拒绝提示 | `cd fe && npm ci && npm run lint && npm run build && npm test`，真实浏览器预览（docs/development-preview.md）与相关 browser 测试 | L1：纯 UI，只消费已评审的 wire 字段 |
| O1（另立 issue，待接受） | worktree 准入复用同一检查，拒绝位于或等于自有位置的 cwd | cwd 在 managed track 目录内或他人 worktree 根内 → 400；仓库根上已有 worktree 时新 worktree 照常创建。约 150 行 | L2：创建准入 |
| （S2b） | `view` 成为 `WorkspaceIsolation` 的第三个变体，随其后端描述符 migration 与创建/关闭/删除/恢复同片 | 按 [workspace-view-backend.md](workspace-view-backend.md) | L2 |

## 5. 推荐顺序与理由

1. **I1a → I1b → I2 先行。** 不依赖任何 cutover；直接满足用户的 typed 选择；把 borrowed 选择变成显式且标注“无保证”的
   非资源目标，并以准入规则保证它与自有位置不重叠，这是 D1–D3 能删除的前提之一。I1a 的穷举 `match` 也让以后加入 `view`
   时每个破坏性入口都必须表态。
2. **C0a–C0d 与 I1 并行**（D1/D6 被接受后）：按请求组的纯删除，缩小 executor 协议面；兼容规则在 C0a 落地。
3. **C1** 关闭唯一已知的清理竞态，规模小，与其余切片无依赖。
4. **O1**（接受后）在 C5 之前。**C2a → C2b/C3.0、C3.2a、C3.2b → C3.2c/C4（C4.0d 是第一片，已由 #2999 交付，独立可部署；C4.0a–C4.0c 在其后并行；之后 C4.1 → C4.2 → C4.3 → C4.4）→ C5**：先让长寿启动成为 executor 事实，再让破坏性 effect
   依赖这些事实；顺序反过来，C5 只能继续依赖 DB 代理事实。forge probe 随 C3.2c 成为 execution，不再是例外。C5 之后仍不是 executor 事实、
   C5 不得删除其占用检查的有：pre-C3 原生 forge 行（含 Stuck，由 forge-owned 检查覆盖）、共享 Codex daemon 下的线程（线程级检查）、
   owner 自建 card 与升级前的 Legacy worker 行，以及在 W 之前仍是 Legacy PTY 的 task worker（Legacy 检查）。若用户对 C4 的 Q2 选另一支，
   executor 同宿主正向排空片可与 C4.0a–C4.0d 并行开发，合入在 C4.1 之前。
5. **C6 可选**，然后 **S3 → S2b**：view 需要 S3 的证据与 C5 的正向静默，作为第三个后端最后接入。
6. **W（task worker 迁入 Engine）在 C5 之后**，与 C6、S3、S2b 互不依赖：它须先按 §3.3 的需求重写设计并经两路 L2 评审收敛；
   第三轮评审表明 worker 迁移的阻断集中在退出取代、输出完成义务、typed 终端重启与 lease 清理四个所有权边界上，与 Planner 各片无关，
   所以不让它阻塞 C4 与 C5。W 落地前 C5 对 task worker 保留 Legacy 检查。

## 6. AGENTS.md 架构五点与永久成本

- **抽象边界**：隔离词汇在 calm-types；保证 G 的破坏性政策在 `workspace-runtime` 的入口（M1 之后的唯一 owner），入口收 typed
  数据；非重叠准入与 `AttachedInheritedPath` 同在 calm-truth 的创建计划模块（它拥有 `TrackWorkspacePlan` 与创建事务）；FE 消费
  wire 字段，不推断领域政策；进程事实只来自 executor（C2–C5）。
- **重复逻辑**：一个解码函数 `isolation()`、一个 wire 投影点、一个 lexical + canonical 重叠比较（子 track、`none` 与 O1 共用），
  没有并行的守卫列表；C1 复用既有两把锁；C2–C4 每接入一种启动就删除对应的 Legacy spawn 分支，C5 删除已迁移类型的 Legacy
  占用代理（不含上述 pre-C3 原生 forge 例外）；forge probe 迁入 Engine，删去原生 probe 生命周期，不另建一套；C0 删除无调用者的协议。
- **硬编码假设**：`WorkspaceIsolation` 是本层有意闭合的集合，`view` 作为新变体加入；自有位置是 kernel 自己的三类目录
  （managed 根、worktree 根，以及自 C5-D4 起的 objective checkout 根），由配置与 DB 行给出，不按 provider、模板或路径字符串（如 `.claude`）
  推断所有权。C5-D1.3 在 managed 根之下按 Neige 自己写入的 `<area_id>/<track_id>` 布局求 owner，这是 Neige 自有标识；worktree 根仍只取 DB 路径。
- **外部合同事实**：Git 根只取 `rev-parse --show-toplevel`；`none` 只要求它成功；worktree 根取 DB 中 kernel 写入的
  `workspace_worktree_path`，不从路径布局推断；executor 的版本与持久形状取自其协议常量。
- **永久成本**：I1/I2 没有 migration、新表或名单；C2b.0 另有 parked CHECK 重建迁移。新增一个等级要改：枚举、解码函数、workspace-runtime 中约 6 处穷举 `match`、
  请求枚举、FE 标签映射，每处一次，由编译器强制。新增一类自有位置要在重叠检查的位置集合中加一处；进行中的 teardown 目标是一个进程内登记表，只有删除路径登记、只有 `none` 准入
  读取，新增删除路径时必须同样登记（由 sweep 的入参类型要求守卫来强制，不靠名单）。它只保护同一进程内的窗口，
  已启动子进程跨崩溃存活留作 G 的显式例外（#2862），不加持久登记。准入检查每次 attached
  创建运行一次（一次 SQL 读与少量 canonicalize）。新测试放在既有 target 中，每个 CI workflow 运行一次；增删测试不改任何名单。
  C1 是进程内围栏，不新增持久状态，每次 Planner turn issuance 多一次通常无争用的 track 锁；C0 每片 envelope +1，持久合同常量只在持久形状改变时变化；C6 若实施会新增一类
  operation kind。删除 D1–D3、D6 会显著减少 launch-admission 计划中的永久面。

## 7. 评审处置（计划评审第一、二轮，C2b、C3、C4 与 C5 设计评审）

| 发现 | 处置 |
|---|---|
| boundary B1 / adversarial B1：按行跳过不能阻止另一 owner 的回收、清理、删除到达 `none` 目录 | §4.2 定义自有位置与保证 G；`none` 准入拒绝位于、等于或包含自有位置的 cwd，复用 `AttachedInheritedPath` 的双检查；列出 G 之外的情形；I1b 加入 owner 关闭/删除的反例验收；§3.1 以非重叠合同重新论证 D1–D3，并把既有的自有/自有重叠列为 O1 |
| adversarial B2：#1830 之前的行带历史 lease，删除时仍删 candidate refs | 保留为显式例外（只删 Neige 自有 refs）；I1a 审计历史未完成 operation/交付的重放；I1a 验收加入升级前 lease/refs 夹具 |
| boundary B2 / adversarial N1：0166 盘点错误，I1a 的终端行为变化不存在 | §1 改正：`none` 形状今天就解成 `ExternalDir` 并跳过观察，无 checkout 的 worker 也跳过；owned worker 准备以观察 identity 作 Stale/CAS 围栏并保留。I1a 不再声称终端行为变化；旧的非 Git attached 行照旧启动终端，新的 Git 准入不作用于旧行与 keyed 重放 |
| boundary B3 / adversarial N4：C0 规模低估，C2 合并两个消费者，规模口径不一 | C0 按请求组切为 C0a–C0d 并给出源文件盘点、保留清单与 envelope/持久合同兼容规则；C2 拆为 C2a（typed Pipe half-close 前置能力）与 C2b（gate），C3 按 spawn 点拆分，C4 每个启动或恢复入口一片；§3.2 统一规模口径（手写行，生成产物单列） |
| boundary N3：入口只收字符串 | I1a 改为传递 typed 数据给 remove/sweep/clean，不从路径推断 |
| boundary N1 / adversarial N3：省略 isolation 的幂等字节与历史 binding | 省略与显式 `worktree` 同形且不写字段，只有 `none` 写入；I1b 加升级前 binding 的重放矩阵 |
| adversarial N5：`isolation()` 不会自动成为 wire 字段 | 单一 wire 投影点，不持久化，不改持久载荷形状 |
| boundary N2 / adversarial N2：C1 未命名锁与持有区间 | §3.2 命名 drive mutex 与 `track_delete_locks`，逐入口给出持有区间，不加第二套锁；残余崩溃风险无上界，交 Planner 接受 |
| boundary N1（I1b 默认值） | Planner 接受省略 → worktree 作为唯一记录在案的兼容规则 |
| adversarial coverage：Planner forge/`git_commit` 写入来源目录 | 列入 G 之外，UI 文案不写“Neige 不触碰” |
| 计划评审第二轮 adversarial B1：删除提交后、post-commit sweep 前，live 行已消失而 worktree 仍会被删除 | §4.2 把进行中的 teardown 目标列为自有位置 (3)：workspace-runtime `sweep::TeardownTargets` 进程内登记表，删除在 COMMIT 前登记、sweep 执行完才移除，`none` 准入在持 SQLite 写锁的事务内读取快照；Area 删除与回滚补偿同理；未启动的 sweep 崩溃后不补跑、只泄漏，已启动子进程跨崩溃存活是 G 的显式例外（#2862），C6 的作业行若引入则并入集合但不证明旧子进程已排空；I1b 加“提交后、sweep 前准入 → 400，sweep 后可准入”的稳定反例 |
| 第二轮 adversarial B2：既有 idle Planner runtime 的下一 turn 不经任何启动入口 | C1 表加入 `maybe_issue_turn` 一行（排队消息、自动观察、pending rewind、issuance 下的命令）：issuance 之前等待 track，持有到持久化 `turn_pending`，锁序 card → drive → track → issuance，shutdown 可中止等待；保留 idle 可清理政策并说明不选“Idle 即阻止”的代价；C1 验收加双向竞争与删除不死锁 |
| 第二轮 N1：canonical 检查只是一次观察 | 外部路径/链接在准入期间与之后的变化都明确不防御；缺失根按最近已存在祖先的物理位置比较，不把 canonicalize 失败当不重叠 |
| 第二轮 N2：`none` 与自有位置不重叠不是全局不变量 | §3.1/§4.2 改写为准入时排除 + 后来创建 worktree 的已接受例外，owned/owned 由 O1 保障 |
| 第二轮 N3：同宿主 Unknown 与 envelope 写者 | C0 夹具拆为同宿主重启保持 Unknown、boot 改变 → NoChild、已 durable settle 重放；列出 `EXECUTION_ENVELOPE_VERSION` 的全部持久写者与读者 |
| 第二轮 N4：`apply_recovery_item` 不持 drive | §1 改正锁事实（第四轮 adversarial N1：HEAD 已在取 claim 前持 drive，`driver.rs:1474-1475`、`:1502-1503`，§1 改为现状）；C1 加 boot 恢复一行，围栏在 adapter 而非 drive mutex 上，Busy 可重试；不以计时器宽限作顺序保证 |
| C2b 设计评审 boundary B1 / adversarial B1：SpawnStarted 重驱的“Stop、结算、再准入”没有合法的记录与别名转换 | 收窄而非替换：有记录的 SpawnStarted 写 Stopping{KernelRestarted} 加 Stop，typed park 同一 execution，结算后判 infra；记录仍只有三种写入。唯一需要替换的是 R8a 测试，不是生产消费者；R8a 改断言 |
| C2b 设计评审 boundary B2：`gate_ops` 读 `spawn_artifacts_json` 并调用 `marked_group_stopped`，不在任何切片里 | C2b.1 引入混合占用合同（有记录看 execution 结算），C2b.2 把无记录的原生 op 改判 Unproven，C2b.3 因此仍是纯删除；计入两片预算，`c2b_typed_gate_op_occupies_until_settled` 覆盖四个调用者 |
| C2b 设计评审 adversarial B2：过期的 owned op 在 boot 时以 PastDeadline 进入，丢失 Boot | owned 适配器在 deadline 分流之前走 Boot 分支，`op` 带 deadline；原生顺序不变；回归 `c2b_boot_after_deadline_keeps_boot_origin` |
| C2b 设计评审 adversarial B3：reconcile 在完成事务前算好 outcome，期间写入的 timeout/cancel 判决会丢 | release/decision 写者都先 `require_live_claim_tx`；gate 的 `complete_owned_parked_tx` 在同一事务内复核 execution id、release、decision，不同则回滚重算；结算是判决前提，Stopping 不单独授权完成 |
| C2b 设计评审第二轮 adversarial B1：Stop 型 cancel/补偿走既有 saga，结算前 `mark_failed` 或未结算即 Stuck | 有记录的 typed op 不进入补偿：`cancel_parked` 无生产调用者，对 owned op 返回 false，decision 去掉 Canceled；记录前失败无 execution，补偿无进程步；记录后一律 typed park，Stop 只写 decision 并留在 Parked，由同一 reconcile 结算后完成与 Retire；回归 `c2b_stop_decision_survives_unavailable_and_restart`、`c2b_cancel_parked_leaves_typed_op_parked` |
| C2b 设计评审第二轮非阻塞项 | claim 竞争不消耗一次性计时器或 ReleaseRefused（`c2b_timeout_retries_while_probe_holds_claim`）；首个 WriteStdin 前等可交互 receipt；不推进原生 op 按 kind 分片生效；parked deadline 是独立安全上限；新增行为不变的 C2b.0，超预算理由改为“生产消费者与其不变量测试保持原子”；boundary N1 的 603 是加法错误（逐项和为 +503），C2b.2 合计不变 |
| C2b 设计评审第三轮 adversarial B1：准入后 `set_parked` 失败，普通与 boot 外层 `mark_stuck` 终结有记录的 op，execution 无人 Stop、别名不 Retire；boundary N1：“共 3 片” | 三轮阻断同属一类，改为一条类不变量：有记录的 op 只经 reconcile 完成事务离开 SpawnStarted/Parked。四个通用离开写者在事务内按适配器声明的 owner-state 键留存（只写 `last_error`、lease 即退避、到期再 drive），附全部写者穷举表；删去 `plan_compensation` Conflict 等逐例外规则；守卫放进行为不变的 C2b.0（约 270），C2b.1 加两条回归（约 1100）；(d) 改为四片 |
| C2b 设计评审第四轮 adversarial B1：0152 的 parked CHECK 只许 isolated 无 artifacts，typed gate park 必败，留存后永久重试 | 删去“不加迁移”；C2b.0 加新迁移，第一支改为“有 artifacts 或有 v1 `gate_execution` 记录”（`COALESCE` 防 NULL 放行；谓词已由第五轮 B1 改为只去掉 artifacts 要求），按 0152 步骤重建并逐字保留索引、keyed 行 trigger 与 `worker_sessions.spawn_op_id`；calm-truth 升级与正常迁移两个测试，C2b.1 经生产 `set_parked` 覆盖两种 kind；C2b.0 约 510，改为 L2 |
| 第四轮 boundary B1：owned Parked reconcile 出错时先清 lease、不写 `last_error`，sweep 立即重取 | C2b.0 在 `owned_parked.rs` 加与 owner 无关的错误留存：以本 claim 为围栏的 UPDATE 只写 `last_error`，保留 lease 作退避；回归 C2b.0 lib `error_keeps_lease_and_writes_last_error` 与 C2b.1 `c2b_malformed_record_is_retained_with_backoff`（SpawnStarted/Parked × boot/稳态） |
| 第四轮非阻塞：boundary N1 写者表缺 `write_phase_and_tx_output`；N2 永久成本；adversarial N1 锁基线 | 表加该行，保证写为现有生产调用路径并约束新调用者；区分一次性键参数迁移与日后 owner 注册，写明 CI 频率；§1 与第二轮 N4 改为现状 |
| 第五轮 boundary B1：通用 CHECK 解析 `gate_execution` 的 version/execution_id，是 owner 私有政策，每个 owned 资源再加一支迁移；adversarial N1：boot 有两次立即尝试；N2：CHECK 只是浅判别 | 新迁移第一支去掉 artifacts 要求，只留 parked 时间事实与原 isolated 支；不选“或 `tx_output_json IS NOT NULL`”（parked 行必有 tx output，区分不了）；原生 artifacts 围栏留在 Rust（`writes.rs:104-117`、`driver.rs:796-815`），C2b.0 replay 两条既有用例；owned 键只要求键存在，记录有效性只归 `gate_execution.rs` 解码器，`c2b_owned_park_requires_decodable_record` 覆盖缺失与损坏；迁移测试改名 `parked_check_keeps_only_owner_neutral_facts`；写明并断言一次 boot 两次尝试；C2b.1 约 1150 |
| C2b 设计评审非阻塞项：行号、同宿主 Unknown、计时语义、预算 | 改正 `gate_process.rs` 与 `task_*` 删除范围；明确同一 OS boot 内 Unknown 不可恢复、不承诺结算；区分授权计时与 parked 安全网，已结算 verify 过期后取 status；两片预算按文件重估，均超出约 800 并写明无安全切点 |
| C3 设计评审第四至六轮 adversarial 阻断：R4-B1（probe 预约与 claim 续租的顺序允许旧 claimant 在终结后启动 probe）、R5-B1（续租前被取消泄漏进程内预约，op 永久 Parked）、R6-B1（tokio `Command::spawn` 在 OS spawn 之后仍可返回 Err，子进程逃出预约守卫） | 三轮阻断都来自给 probe 手写的原生生命周期，按 AGENTS.md 改方法、删机制：probe 改为同一 `forge_execution` 记录拥有的 Engine execution，以 live claim 下与 `Engine::admit` 同事务的 create-if-absent（④）作 durable 排斥，后来的 claimant 只观察已存在的 probe；spawn、计时 Stop、子孙收尾、结算与 Retire 归 Engine，判决只读已结算 probe 的文件，Landed 与 output probe 准入同事务。删去进程内预约与身份守卫、继任与回收任务、每次调用前的续租预算、probe 的 `kill_on_drop` 过渡期与 C3.1 片，C2a/C5 不再为 probe 留例外；接受同一 OS boot 内 supervisor 重启后的 Unknown 不完成，以及 probe 不重试（Releasing 时崩溃或主机重启判 infra）。C3.2 拆为 C3.2a（清理栅栏）、C3.2b（原生凭据载荷与判决函数）、C3.2c（切换），按 C2b 实测比重估各片；测试删去续租与预约用例，新增 durable 排斥、boot 抢占不重复 spawn、结算先于判决、经 Engine Stop 的 probe 超时等；切换仍远超 800，理由与被否定的切法写在“切片与规模” |
| C4 设计评审第一轮 adversarial B1：sweep 只处理 Observing 与已结算，提交后 drive 丢失或 Engine 遇瞬时 `StoreError` 后，Stopping 与已请求 Retire 的行永不推进 | sweep 每轮先调用既有 `Engine::recover()`（`pending` = 未结算或 Retire 而秘密未清），覆盖 Stopping 与 ④ 已提交的行，不另建状态机；C4.0c 加与 `pending` 谓词相同的部分索引（第二轮把节拍与索引移到共享片 C4.0d，见下）；回归 `c4_sweep_redrives_after_lost_post_commit_drive`、`c4_sweep_redrives_after_transient_store_error`、`c4_sweep_redrives_committed_retire` |
| adversarial B2 / boundary B5：丢失驱动者后 sweep 停住进程并清空记录，但 owner turn 未完成（`active` 不清、`join_driver` 永久 Conflict、ACP receipt 与凭据未处理） | provider owner 注册的幂等 `complete` 端口，驱动者收尾与 sweep 共用同一函数；Turn、Compaction、RewindCheck、ACP（撤销、receipt、projection）逐一定义；④ 要求结算且 `complete` 已确认，记录在此之前保留；通用 sweep 只按 consumer 键调度；回归覆盖 panic/abort 各阶段、outcome 写入失败、无用户操作、结算/完成/Retire 之间的重启 |
| adversarial B3：C4.1 单独部署时，被取消的 rewind 在 `issue` 锁释放后仍未结算，原生 turn/compaction 不读记录 | 自 C4.1 起同 session 的全部启动（Engine 准入与仍原生的 turn、canonical、compaction、rewind）在 `issue` 锁内经同一个 `admission_gate`，非空未结算即 Conflict；写明与删除、boot、Unknown 的交互；回归 `c4_canceled_rewind_blocks_native_turn_and_compaction_until_settled` |
| adversarial B4：worker Held 恢复缺 durable 释放授权围栏，重新 mint 代替了 task 授权 | Held→Releasing 是 `require_live_claim_tx` + `require_attempt_startable_tx` + 精确记录 CAS 的同一事务，先于任何字节；mint 只在守卫之后、同一事务内；后继看到 Releasing 一律 Stop、永不重发；回归 lease 过期、取代、终结、双 claimant、取消后 boot |
| boundary B1：正常收尾只在 `Exited` 时 Stop，结果之后不退出的 CLI 一直 Spawned | `EXIT_WAIT` 之后无条件 ③ Stopping{Finished} + Stop，结算之后才有 outcome 与 Retire；ACP 收尾、setup 失败与准入后拒绝同一规则；不复用要求 operation claim 的 `drain_exited`，只用其与 owner 无关的 `Engine::request(Stop)`；回归 `c4_lingering_cli_after_result_is_stopped_and_completes` 等 |
| boundary B2：`stty -echo` 不使 PTY 载荷字节保真（4095 行长、行编辑、CR 转换、信号与流控字符） | wrapper 以 `stty -g` 保存、`stty raw -echo -iexten` 进入非 canonical 字节保真模式、读完后精确恢复，布防/撤防两个标记界定窗口，Released 在撤防之后才写，输入门覆盖全部 writer；真实 PTY 测试覆盖全部字节值、4095/4096/4097/16KiB 边界、termios 精确恢复与失败关闭 |
| boundary B3：通用 shell 解码器假定 ACP 环境键是 shell 标识符 | 载荷键合同：可移植标识符、shell 特殊名与 `neige_*` 内部命名空间保留、运营者键另禁 `PATH`/`NEIGE_*`；单一 `validate_key` 供注册与准入使用，不合格的既有注册在 mint 前显式拒绝；kernel 先按今天的覆盖顺序合并再按来源分区，同名键只在一侧 |
| boundary B4：复用 `TerminalProjection` 与 reaper 不能把 Claude worker 的退出送到 task；Legacy Probe 对 typed 行恒为 Unknown | reaper 加以 task 义务为键的扫描（`exited` session 且 attempt 仍活）→ provider `interpret_exit` → 既有 `converge_dead_worker`；provider 合同 crate 声明注入的 `TerminalObservation`，typed 读结算、Legacy 仍 Probe；回归无 renderer 的快速/信号/NoChild/重启退出与 projection 与 task 之间崩溃（第二轮改为两条发现路径共用的退出决策树与 executor 取证的观察端口，见下） |
| C4 第一轮非阻塞项（boundary N1–N5，adversarial N1–N4） | 受影响测试与 gate replay 对齐（C4.0b 加 `acp_planner_provider`，C4.6 加 `planner_result_loop` 与 `provider_conformance`，C4.5 加 reaper/provider），“加”表示继承前片，审计改按生产符号；Q3 改为有界汇点并把 stdout OutputLog 的磁盘上界列为残余；Q8 代理分类；`readiness_command.rs` 范围改为 `:126-129`，C4.4 改称运行期删除加 readiness 有界化并保留 pre-C4 boot sweep；worker boot/重驱矩阵含已结算、不可用、Unknown、损坏、取消与 viewer 准入后失败；PTY 握手的失败与分块测试；listener 之前只保证撤销与 Stop 已 durable，并测顺序与 pass A 失败；Q1/Q2 待用户，Q2 另一支给出范围、规模与片序 |
| C4 设计评审第二轮 adversarial R2-B1 / boundary B3：C4.5 允许先于 C4.1 部署，而周期 `Engine::recover` 只在 C4.1 的 Planner sweep 里；今天 recover 只在 boot 运行，`StoreError` 会移除 slot | 周期恢复改为最早的共享片 C4.0d：Engine 的组装 owner `execution_boot.rs` 在 boot `recover` 之后起唯一的 30s 节拍，pending 部分索引随它迁移；C4.1 与 C4.5c 都依赖 C4.0d，Planner sweep 不再调用 `recover`，不建第二个计时器或状态机。源码核实既有消费者有同一缺口：gate 的 Drained 后 `settle` 失败要等 parked 期限、gate/forge 完成后的 Retire 与 typed 终端在 `StoreError` 后要等重启；C4.0d 一并覆盖，回归 `c4_recovery_tick_*` 六条，C4.5c 加只部署到 worker 分支时的两条节拍恢复用例 |
| adversarial R2-B2 / boundary B1：task 义务扫描把每个已退出 session 都送进 `interpret_exit` 与失败专用的 `converge_dead_worker`，绕过 resumable 死亡确认，`Completed`/`PreserveCard`/`ResumeEligible` 与 Terminal 成功、Verifying 未定义 | reaper 中一棵退出决策树（`server/reaper/exit_policy.rs`），既有发现路径（C4.5b）与按 task 义务的发现路径（C4.5c）共用：归属 → 物理事实 → provider 声明 Resumable 时原样保留 busy/inactivity/thread/`confirm_durable_death` 闸门 → `interpret_exit` → task 守卫（只 Dispatched/Running，Verifying 归 gate，被取代的 session 不写）→ Terminal 类经 scheduler 的 `complete_terminal_task`，其它 kind 的 Failed 经 `converge_dead_worker`，Completed 不发回退事件，PreserveCard/ResumeEligible 不写 task；Claude 退出 0 而未自报的政策写在 `ClaudeProvider` 中（保持今天：交 liveness 期限，因手动重启可继续自报）；projection 只终结 Ephemeral session，Resumable session 留给仲裁；无 provider 名字分支；测试覆盖活 Codex 线程加已退出 viewer、全部判词、Terminal 成功、无 renderer 的 Claude 退出 0、Verifying 加活 gate |
| boundary B2 / adversarial R2-N2：只读结算的 `TerminalObservation` 区分不了运行与同宿主 Unknown，也看不到 leader 退出、驱动不了排空 | 端口按事实来源分工：SQL 结算是 durable 退出权威，运行期阶段经既有 `Engine::output` Observe 取自 executor（`Starting/Running/LeaderExited/Settled/Unknown/Unavailable`），不从“未结算”推断 Running，不加阶段镜像；worker 执行 owner 每轮先发现未结算的 worker execution，`LeaderExited` 即 durable 请求 Stop 排空，leader 退出从不授权 task 写入；回归覆盖无 renderer 的 setsid 子孙排空与 supervisor 崩溃后 Unknown；Engine 合同不变 |
| C4 第二轮非阻塞项（boundary N1–N3，adversarial R2-N1、R2-N3） | stderr 汇点在越过上限时立即写溢出标记，计数缺失记为 `AtLeast(1)`，从不读作 0、从不阻塞 `complete`，加 Stop 中溢出、子孙持有 fifo、恰好上限三类测试；C4.5 拆为 C4.5a（环境对等，消费者为 opt-in typed 终端）、C4.5b（观察端口与 P1 决策树）、C4.5c（typed worker 原子核心），超 800 的片逐一写明理由；Q2 另一支列出原子成员关系、委派逃逸、durable cgroup 身份、kill/populated 错误与重启竞争、supervisor 丢失后的输出、旧记录保持 Unknown 六项缺口，片序改为“可并行开发、合入在 C4.1 与 C4.5c 之前”；R2-N3 的完成竞争与 PTY 负面测试已在上文清单中保留 |
| C4 设计评审第三轮 adversarial R3-B1（D5 的取代检查不在 D6 的 task 与 lease 事务内）、R3-B2（worker 输出取尾缺 durable 恢复与完成边界）；boundary B1（Legacy `claude-restart` 无法重启 typed worker 终端）、B2（D5 丢失未绑定 session 的 lease 释放） | 不在 C4 内逐个修补。四项阻断都在 worker 迁移，第二轮以来的阻断也都在这一侧；按 AGENTS.md 三轮仍阻断即收窄范围，Planner 决定把 worker 迁移（原 C4.5a–C4.7）移出 C4，成为 C5 之后的独立里程碑 W（§3.3）。四项阻断逐一写成 W 的需求 WR1–WR4，另有 boundary N1（WR5）、须保留的第二轮已收敛机制（WR6）与 Q1/Q2/Q8 对 worker 的效果（WR7）；第三轮的 worker 设计原文保留为 W 的起点，被否定处标注 WR 编号，W 动工前须重写并经两路 L2 评审。C4 收窄为 C4.0a–C4.0d 与 C4.1–C4.4：C4.0d 是第一片且独立可部署，单独合入即修复既有 gate、forge 与 typed 终端在 `StoreError` 之后的缺口；C4.1 随 rewind 引入 Planner 的代理分类函数，不再依赖 C4.5a；C4 各片不依赖 W。C5 对 task worker 保留 Legacy 占用与静默检查，不等待 W。Planner 各片已收敛的机制不变。第一、二轮的 worker 处置（第一轮 adversarial B4、boundary B2/B4，第二轮 R2-B2、boundary B1/B2、R2-N2 以及 R2-B1 的 worker 部分）随起点移入 W，其中的片名 C4.5a–C4.7 依次对应 Wa–We |
| C4 第三轮非阻塞项：adversarial R3-N1（stderr 溢出标记在 Stop 下仍会丢，缺标记被读成未截断）；boundary 的 mutation 说明（去掉节拍时预测六条全红，但幂等用例可能空洞通过）；boundary 对节拍的确认 | 汇点只在读到 EOF 之后写 `stderr.closed`，没有它就不报告未截断：新增 `StderrCaptureInterrupted` 结果与 `c4_stderr_stop_at_cap_is_never_untruncated`（写 262145 字节、到达上限即 Stop），`c4_stderr_overflow_survives_stop` 允许该结果；幂等用例先断言节拍至少运行两轮，去掉节拍的预测红集逐条写明六条及各自变红的原因，“无第二驱动循环”由 Engine 既有单 slot 合同承担；C4.1 重做同一突变时只另加两条 Planner 用例，不含 worker 用例。boundary 第三轮确认节拍与物理观察成立、既有缺口真实，C4.0d 的机制不变 |
| C5 设计评审第一轮（failure-modes 事件 730922，boundary 事件 730930）：B1 C5b 删去 forge 阶段代理后，action 已结算的 Parked op 可经 observer 或恢复在证明之后准入 probe；B2 / boundary R1 准入 owner 集合只看 live 行，提交之后、sweep 之前找不到 owner；B3 restore 失败的目录 7 天后被 trash GC 删除；B4 未规定 `source.root` 缺失、null、类型错误与升级；boundary R2 objective checkout 根不在 O1 内，attached track 可绕过显式 cwd 拒绝；boundary R3 Area 容器的 rmdir 不在证明目标内（含空 Area）；N1 锁生命周期；N2 不变量的例外 | B1：新增 C5-D6，`admit_probe` 取与终端共用的准入围栏，事务内复查 frozen track 行，Busy 时 `LeaveParked`；`admit_action` 由保留的无记录 op 代理覆盖；扫描全部 `Engine::admit` 调用者，C5b 中代理删除与 probe 围栏同片。B2：owner 在 managed 根下按 Neige 布局求（覆盖 repoint 旧根与 Area 容器，保留原 track 锁直到回收；Planner 建议的 repoint 登记实质采纳、不另建），worktree 根取 live 行 ∪ 带 track id 的 `TeardownTargets`，持有到 sweep 结束。B3：GC 跳过 track 行仍在且仍为 managed 的条目，保留集合在 GC 已持有的 drive 锁下读取，不写 pin；否决“原路径仍存在”判据（挡不住改名失败）。B4：逐行解码、不过滤 NULL，失败即 Conflict 并阻塞所有 scope；`source.root` 早于持久化存在，升级无代价。R2：O1 自有根列表加入 objective checkout 根，该根提为 typed 字段；C5 之前的占用者是已接受残留。R3：Area 证明的位置目标改为容器目录与成员 worktree 根，容器 owner 为 area 锁，panic 分支删去容器 rmdir。N1：去重、Busy 时释放全部守卫、持有到写者提交、不声称公平。N2：C5 设计开头列出不变量及其已接受的例外。C5a/C5b 的范围与验收随之更新 |
| C5 设计评审第二轮（boundary 事件 731210 无发现；failure-modes 事件 731226）：R2-B1 repoint 目标只做 Git 校验（`server/routes/tracks.rs:2032`），folder claim（`:2128`、`:2262`）只执行 Area 归属，绑定写者（`:2270`）不经 `admit_attached_workspace_tx`，pristine managed track 可 repoint 进 objective checkout 或 `.trash` 中的仓库，之后默认 cwd 终端与 provider execution 在那里运行，objective 清理或 trash GC 可删除其根 | 新增 C5-D7：repoint 目标经同一 `admit_attached_workspace_tx`（C5-D4.1 扩展后的自有根列表、live worktree、teardown 快照，`AttachedFromCwd` 即 `none` 规则，lexical 与 physical），一次在 Git 校验之后、任何锁内生命周期变化之前的只读准入事务内，一次在绑定写者内与写入原子；不加第二种检查。扫描全部 workspace 写入路径与执行根来源（创建、historical 拒绝、子 track、repoint、launchpad、显式 cwd、默认 cwd）。C5a 范围加入 C5-D7，验收加入生产 PATCH 的 objective checkout、`.trash` 仓库及其物理别名的拒绝（旧绑定、carrier、目录不变）、竞态窗口内的写者拒绝与普通外部 repoint 成功 |
