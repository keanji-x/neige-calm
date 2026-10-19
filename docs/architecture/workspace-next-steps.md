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
| 锁：每卡 `planner_recovery_locks` → 全局 operation drive mutex（`OperationRuntime::lock_for_track_delete`；`drive()` 持有它；boot 的 `apply_recovery_item` 在 Recover 与 Compensate 取 claim 前也持有它，`driver.rs:1299-1302`、`:1326-1328`，早先基线不持有）→ 每 track `track_delete_locks`（绕过 OperationRuntime 的直接恢复/重连），不得反向。创建持目标 area 的 `area_delete_locks`，先于以上各锁；单 Track DELETE 不取 area 锁 | `server/state.rs:112-141`、`server/operation/driver.rs:75-78,400-408,444-446,1204-1214`、`server/harness/mod.rs:245`、`server/routes/planner_recovery.rs:47`、`server/routes/tracks.rs:722-727,3123-3126` |
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
| ExternalDir、legacy executor、provider/直接进程 | 部分交付 | 私有 registration 复用 ExternalDir wire 形状；v2/v3 协商不变；provider/直接进程 → C2–C4（execution §11 E3） |
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
| C2b.0 | 行为不变的准备片，缩小 C2b.1 | ① `tests/execution_hooks.rs:42-75` 的 supervisor 装配移入 `tests/support`（移动，不复制），`execution_hooks` 改用它；② 通用写者对 `terminal_launch` 的三处特判（`writes.rs:155-158`、`:250-253`、`:306-315`）改由该层一个 owner-state 键常量生成，此时常量只有 `terminal_launch`；③ owned 留存守卫（见设计）：`owns_parked_resource()` 改为返回 owner-state 键，四个离开写者带必需的键参数并在事务内留存、返回 `Retained`，driver 对 `Retained` 记错误并在 lease 到期后再 drive；④ owned Parked reconcile 的错误留存；⑤ 新迁移放行带记录的无 artifacts parked 行（④⑤ 见设计）。生产适配器都不声明键，现有行都满足原 CHECK 分支，故行为不变；迁移使本片为 L2。新测试：lib `claim_tests::owned_key_retains_terminal_writes`、`owned_parked::tests::error_keeps_lease_and_writes_last_error`，calm-truth `typed_gate_park_migration_tests::{upgrade_preserves_rows_refs_indexes_and_keyed_fence, parked_check_admits_only_recorded_rows}`；replay `execution_hooks` 全部、lib `repo_sqlite::claim_tests`、`terminal_adapter::tests::launch_cleanup_tests` 与 calm-truth `operations_keyed_rows_permanent_tests`。估算 +52/−48，守卫与 `Retained` +40，`mod.rs` +4/−2，`driver.rs` 调用点与延迟 drive +25/−4，28 个测试调用点改参数 +28/−28，新测试 +40；错误留存 +12/−3 与其测试 +40；迁移 +90（表定义取自 0152，只改一支）、`head_schema_fixture.rs` 迁移清单 +1、迁移测试 +90；合计约 +422/−85 ≈ 510 | C2a |
| C2b.1 | worker gate run（`task-gate-run`）改为 Engine 消费者；同片引入两种 gate 共用的唯一构造点 `server/operation/gate_execution.rs`、必需记录 `gate_execution` v1、typed parked 关联与 typed 占用读者（设计见下） | 范围：Pipe intent 工厂（沿用现有环境白名单；consumer `gate` 注册显式 no-op projection）、记录 CAS（SQL 在 calm-truth）、owner-state 常量加入 `gate_execution`、owned 适配器 park 的 Rust 条件不再要求 `spawn_artifacts_json`（表约束由 C2b.0 迁移放行）、owned 的 Boot 恢复先于 deadline 分流、释放顺序、完成事务内复核 decision、Stop 型 timeout、gate 适配器声明 `gate_execution` 键（留存不变量由 C2b.0 的守卫执行）、boot 矩阵的 gate run 列（SpawnStarted 收窄为 infra）、`gate_ops` 的混合占用合同、`finalize_run` 改收结算 status、`ExecutionUnavailable` 拒绝、pre-C2b 原生 `task-gate-run` op 不推进（task verify 到 C2b.2 才适用）。测试世界接 C2b.0 移出的真实 executor support，`launch_kernel` 重启测试的 supervisor 留在测试进程、活过 kernel kill。删除：`task_gate_run/mod.rs:300-400,448-527` 的重驱 `stop_group`、`spawn_held_identified`/`record_spawn_artifacts`、原生 `recover_parked`、Observer 的 stdin 写入/`observe_verdict`/reap 及原生补偿分支。新测试 `c2b_gate_run_has_one_engine_construction`、`c2b_gate_run_refuses_unavailable_executor`、`c2b_release_authorizes_before_first_byte`、`c2b_refused_release_writes_no_byte`、`c2b_phase_writes_preserve_gate_record`、`c2b_typed_park_needs_no_spawn_artifacts`、`c2b_redrive_of_held_ends_infra_without_second_execution`、`c2b_boot_held_stops_without_token`、`c2b_boot_after_deadline_keeps_boot_origin`、`c2b_changed_decision_aborts_completion`、`c2b_timeout_retries_while_probe_holds_claim`、`c2b_stop_decision_survives_unavailable_and_restart`、`c2b_cancel_parked_leaves_typed_op_parked`、`c2b_typed_gate_op_occupies_until_settled`（新 run 准入、regate、reuse、delivery 各一断言）、`c2b_lost_close_ack_does_not_repeat_steps`、`c2b_leader_exit_drains_descendants_before_verdict`、`c2b_unknown_execution_never_completes_op`、`c2b_pre_c2b_native_op_is_never_advanced`、`c2b_park_write_failure_retains_owned_execution`、`c2b_recorded_op_never_enters_compensation`、`c2b_malformed_record_is_retained_with_backoff`。replay：mcp_integration_suite `task_gate_run::{a_gate_run_commits_new_files_before_its_steps_run, a_red_step_returns_its_name_and_tail, an_edit_during_the_run_discards_its_result, done_during_a_run_delivers_after_the_run, a_cancel_during_a_run_keeps_the_checkout_busy_until_it_ends, more::the_run_releases_its_wrapper_only_after_parked, restart::*, commit_message::done_before_checkpoint_release_accepts_the_admitted_input, reuse::an_unchanged_candidate_reuses_the_passing_run}`；kernel_process_suite `parked_operations::{recovery_mode_plumbing_distinguishes_boot_probe_and_past_deadline, boot_recovery_past_deadline_ignores_abandoned_parked_lease}`；lib `task_gate_run::tests::run_recovery_never_reads_the_exit_file`、`execution_boot::port_contract::*`、`repo_sqlite::claim_tests` 与 `terminal_adapter::tests::launch_cleanup_tests`（pin `terminal_launch` 保留）。`restart::a_restart_before_the_park_reruns_nothing`（R8a）保留名字，断言由“重驱后 Succeeded”改为“step 未执行、run 以 infra 结束”（见 boot 矩阵）。估算（增/删）：`gate_execution.rs` +215；calm-truth 记录 CAS 与结算读取 +45；`writes.rs` 常量 +3；`driver.rs` +30/−18；`gate_ops.rs` +30/−6；`task_gate_run/mod.rs` +110/−200；`finalize.rs` +20/−15；重启 support +35；21 个新测试 +360；R8a 等既有用例改写 +20/−15。合计约 +868/−254 ≈ 1120，超出约 800：每个生产消费者与其不变量测试保持原子（typed op 从第一次合入起就必须被准入、park、boot、完成与占用读者一致读取），可前置的行为不变整理（含留存守卫）已移入 C2b.0，PR 按上列文件组说明 | C2b.0 |
| C2b.2 | task verification（`task-verify`）改为同一构造点的 Engine 消费者：释放移到 park 之后，Released 的 boot 只重试 CloseStdin 并重新挂 observer，after-sample 只认 Drained | 范围：observer 释放、`Exited` 后 Stop 收尾子孙、两个计时器（见下）、boot 矩阵的 task verify 列、新 attempt 前要求前一 attempt 的 gate execution 已结算（替代 kill prior）、恢复判决取 executor status 而非 exit file、`gate_ops` 对无记录的原生 op 改判 Unproven（此后不再产生原生 op），无记录的原生 `task-verify` op 自此不推进（boot 矩阵末行）；scheduler 测试世界接 C2b.1 的 executor support。删除：`task_verify_adapter/mod.rs` 的 `spawn_side_effect`（`:368-558`）中的 kill-prior（`:383-417`）、`spawn_held_identified`（`:423`）、`tasks.gate_pid*` 写入与 park 前释放（`:433-495`）、原生 observer/reap（`:511-558`），`recover_parked`（`:560-664`）的 PID 存活与 exit-file 恢复，补偿 `kill_gate_group`（`:720-741`）；verify 对 `target::stop_group`（`target.rs:691`）的调用改为结算事实，函数本身留给 C2b.3。列保留，读者改为 execution 事实。新测试 `c2b_verify_releases_only_after_park`、`c2b_boot_released_retries_only_close_then_reattaches`、`c2b_crash_before_token_is_infra_not_relaunch`、`c2b_timeout_probe_preserves_durable_decision`、`c2b_after_sample_requires_drained`、`c2b_new_attempt_requires_prior_execution_settled`、`c2b_recovered_verdict_uses_executor_status`、`c2b_settled_verify_after_deadline_keeps_status`、`c2b_native_terminal_gate_op_is_unproven`。replay：runtime_suite `scheduler::` 中 `green_gate_flips_verifying_to_done`、`red_gate_fails_with_failing_step_and_log_tail`、`gate_timeout_*`、`gate_spawn_kills_prior_recorded_group`、`forged_exit_file_and_group_kill_cannot_flip_gate_green`、`step_exit_ends_step_and_still_writes_exit_file`、`gate_step_env_is_minimal_and_evidence_paths_scrubbed`、`closed_track_still_drives_verifying_gate`、`stale_gate_before_first_attempt_fails_without_running_shell`、`parked_gate_dead_*`、`boot_reattach_live_gate_lands_verdict_after_exit`、`aborted_observer_leaves_gate_group_alive_and_reattach_lands_verdict`；mcp_integration_suite `gate_binding::{gate_result_discarded_when_tree_changed_during_steps, reattached_gate_result_discarded_when_tree_changed, recovered_gate_stops_the_group_before_sampling, live_reattached_gate_result_discarded_when_tree_changed, gated_candidate_happy_path_wakes_planner_once, legacy_lease_gate_runs_without_target_check, pre_upgrade_frozen_gate_boots_as_unbound}`、`task_regate::regate_reruns_gate_on_same_candidate`；lib `task_verify_adapter::verdict_tests::*`；加 C2b.1 的 `c2b_` 过滤器。改写 PID/exit-file 证据的用例保留名字与断言意图，`parked_gate_dead_with_exit_file_recovers_real_verdict` 与 `pre_upgrade_frozen_gate_boots_as_unbound` 改以 settlement status 为恢复证据（后者用新的 typed 记录构造，不得借此恢复真正无记录的 pre-C2b op）。估算（增/删）：`task_verify_adapter/mod.rs` +150/−290；`target.rs` +15/−20；`gate_execution.rs` 的 Released boot、重挂与授权计时 +60；`gate_ops.rs` +8/−10；`tasks.gate_pid` 读者 +20/−15；scheduler support +40；9 个新测试 +150；PID/exit-file 证据用例改写 +60/−90。合计约 +503/−425 ≈ 930，超出原因同 C2b.1：task verify 这一生产消费者与其不变量测试保持原子（同一 op 的 spawn、park、boot、恢复与占用读者必须同片切换） | C2b.1 |
| C2b.3 | 删除已无调用者的共享原生 gate 机器（纯删除片） | `gate_process.rs:263-324` 的 `spawn_held`、`:328-426` 的 `GateWait`/`GateObservation`/WNOWAIT `observe_verdict`、`:429-541` 的 group/marker 存活、sweep 与 `kill`（`kill` 先做 caller sweep）及其测试模块 `:543-677`，`gate_lifecycle.rs:137` 的 `spawn_held_identified` 与直接调用它的 `gate_lifecycle/unsupported_gate_tests.rs` 用例（`:149`），`target.rs:691` 的 `stop_group`，以及 caller sweep 证明只服务它们的 `target.rs` kill 与 `proc_identity` marker helper；`marked_group_stopped` 的最后一个生产调用者 `gate_ops.rs:107` 已在 C2b.2 改判，故此片无行为变化。Forge/provider/terminal 仍在用的 helper、`verdict_from_exit_code`、wrapper 与 step/exit 文件保留。无新测试；replay `verdict_tests::*` 与 C2b.1/C2b.2 全部 `c2b_` 过滤器，caller sweep 证据写入 PR。估算约 −470（`gate_process.rs` 约 −410、`gate_lifecycle.rs` 约 −45、`target.rs`/`proc_identity` 约 −60，减去测试用例删除后的 import 调整），兼容改动不超过 60 行 | C2b.2 |
| C3a / C3b | forge action / probe 改为 Engine 消费者，各对应一个现有 spawn 点（`server/operation/forge_action_adapter/mod.rs:1009`、`:1296`） | 同上；`cwd_lease` 冻结语义不变；不按工具名单注册 | C2a |
| C4 | Planner 本地 provider 进程（Claude session/rewind/readiness、ACP）与 worker 启动改为 Engine 消费者，每个 provider 的一个启动或恢复入口一片，沿最新 main 的 provider Factory 归属 | 同上；provider 外部协议仍由 provider 层拥有 | C2a |
| C5 | 破坏性 effect 的正向静默：recycle、`remove_track_worktree`、sweep、area 补偿在 fence 下要求 `unsettled_executions(scope)` 为空，否则 Busy 并保留目录；已迁移类型的 Legacy 代理判断同片删除，未迁移类型的 Legacy 检查保留 | 删除路径保持“先 quiesce、后写者复查”的现有时序；scope 只在非重叠合同（含 O1）成立后才等于 owning track。验收：未结算执行时 Track/Area 删除返回 Conflict 且目录与 branch 保留 | C2a–C4、O1 |
| C6（可选） | 释放作业：删除事务内写入待回收的 worktree/refs 描述，提交后 sweep 消费，启动时补跑 | 修复“提交后崩溃 → worktree 与 branch 永久泄漏”；不改 7 天 trash 与回收顺序。未消费的作业行同片并入 `none` 准入的自有位置集合（§4.2 进行中的 teardown 目标），验收加“作业未消费时准入其目标 → 400”。若接受泄漏只记 follow-up，则不做，也不声称已有 crash-safe release | C5 |

**C2a 范围决定。** 只读盘点未发现 C2b/C3/C4 之外独立的双向 track pipe 消费者。有界短 Git/CLI 读取
（checkpoint、sampling、lease reads）仅在进程内由 deadline + process-group reap 与 C1 fences/leases 覆盖。
kernel 死亡后这些 deadline/析构与进程内 fence 不再生效；遗留的短读 process group（包括 Git filter/hook 的副作用）
不在 C5 的 empty-execution predicate 覆盖内，属于 #2862 的同一残留类别（child lifetime / PDEATHSIG）。
这里接受较窄的保证，不把短读迁入 Engine；C5 不得据执行集合为空声称已解决此跨崩溃静默缺口。
迁移还需 Engine 当前没有的 stdout/stderr 分离与
每次 sample 的归属，成本不值得；不为名义消费者而扩大本片。C2a 仅补齐后续迁移的 Pipe half-close。
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
- **记录。** 必需的 `operations.tx_output_json.data.gate_execution`，`version` 为 JSON 整数 1：`{execution_id, release: Held |
  Released{authorized_at_ms}, decision: Observing | Stopping{reason: Timeout | KernelRestarted | ReleaseRefused}}`（不设 Canceled，见留存不变量）。
  `intent_json` 不可变（`0170:19-27` 的 trigger），不能放可变位，记录放 `tx_output_json`；park 的表约束需要新迁移（见下）。记录有三种写入：① 准入，在 SpawnStarted
  的原 claim 下，与 `require_live_claim_tx`、`Engine::admit` 同一写事务，仅当记录缺失时写 Held；提交后才 `drive`
  （`runtime/execution/engine.rs:43-60`，ticket 类型见 `mod.rs:166-182`）。② Held→Released。③ Observing→Stopping
  与 `Engine::request(Stop)` 同一事务（Store 的 Tx 就是 operation 的 SQLite 事务，同终端的 `admit(tx, …)`）。
  CAS 条件固定 op id、`execution_id`、当前 `release` 与 `decision`；条件 SQL 放 calm-truth（`server/execution_store.rs:9`）。
  只有这三种写入，没有把记录换绑到另一个 execution 的写入（见 boot 矩阵的 SpawnStarted）。②③ 的写事务都先
  `require_live_claim_tx`（`writes.rs:73-80`）：live observer、deadline sweep 与 boot 都先取 claim 再写，同一时刻只有一个 release/decision 写者。
  取不到 claim（`claim_parked` 返回 None，`server/operation/mod.rs:939-962`）不消耗一次性的计时器到期或 ReleaseRefused：
  写者重读 op 并重试，直到 op 终结或该 decision 已 durable（`c2b_timeout_retries_while_probe_holds_claim`）。
  通用写者今天只特判 `terminal_launch`（`server/operation/repo_sqlite/writes.rs:155-158`、`:250-253`、`output_value` `:306-315`），
  否则补偿写会因为 `settlement_output_matches_tx` 不匹配而被拒绝。C2b.0 先把特判改为该层一个 owner-state 键常量，
  两个 UPDATE 和比较都从它生成；C2b.1 只在常量里加 `gate_execution`。`complete_parked_tx` 在事务内重读行，本身保留记录
  （`writes.rs:318-372`），由 `c2b_phase_writes_preserve_gate_record` pin 住。永久成本：C2b.0 的键参数是一次性的签名与调用点迁移
  （四个写者、28 个测试调用点，编译检查），不留调用者或测试清单。日后另一个 owned 资源要做 owner 注册：适配器声明键、owner 的
  记录编解码与结算、常量加键，park 不带 artifacts 时还要一条新迁移分支；只有既有 owner 多保留一个字段才只改常量。新增的 `c2b_`
  与迁移测试随所在 suite 每个 CI workflow 各跑一次；mutation 只在实现 PR 内做一次，不进 CI。
- **释放顺序（两种 gate 相同，只在 park 提交后由 observer 执行）。** ① durable authorize：Held→Released 与
  `require_attempt_startable_tx` 在同一写事务，扩展 `TaskLaunch`。它今天先检查、再在事务外运行 effect
  （`server/operation/task_launch.rs:71-91`）。之后先等 execution 有可交互的 Spawned receipt（`interact` 先 inspect，无 receipt
  即拒绝，`runtime/execution/engine.rs:301`、`:353-358`）：Prepared/Forking 只是等待，不是 ReleaseRefused，计入 60s 上限。② `Engine::interact(WriteStdin "go\n")`，同一调用内按 `StdinProgress.accepted_bytes`
  只重试剩余后缀。③ `Engine::close_stdin`。① 输了，或 ②③ 失败、超过 60s，就走 ③ 型 Stopping{ReleaseRefused}；
  永不用关闭未授权的 stdin 来取消。崩溃后看到 Released 不再写字节，只重试 CloseStdin。token 未完整送达时，
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
  已结算时 decision 为 Stopping{r} 取 r，否则取 `Drained{status}` 的 status（NoChild 判 infra）。after-sample 只在 Drained 之后；
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
    lease，每个 lease 周期重试一次；boot 的 `claim_parked_for_boot` 不看 lease，每次 boot 重试一次并留下自己的 60s lease。围栏
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
  `last_error` 持久、lease 期内不重取、到期只重试一次、不准入或终结 execution）；C2b.0 的 lib
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
  `<NNNN>_typed_gate_parked_receipts.sql`（编号合入时取下一个空号）。谓词只改第一支：`kind <> 'codex-isolated-worker' AND
  (spawn_artifacts_json IS NOT NULL OR COALESCE(json_extract(tx_output_json, '$.data.gate_execution.version') = 1 AND
  json_type(tx_output_json, '$.data.gate_execution.execution_id') = 'text', 0))`（`COALESCE` 同 isolated 支：CHECK 对 NULL 放行），
  isolated 支逐字不变；用 0152 DDL 加此谓词实测：带记录的两种 kind 可 park，无记录无 artifacts、`execution_id` 非文本与 isolated 无 receipt 均被拒。按记录而不按 kind：无记录的行
  仍必须有 artifacts，原生行照旧受检，这是最弱的约束，schema 里不再列一份 gate kind；记录只由 `gate_execution.rs` 构造。混合期
  C2b.1 的 task verify 仍写原生行（有 artifacts、无记录），走原分支；C2b.2 起写记录，迁移不再随片改。重建沿用 0152 的步骤：迁移
  事务内外键始终开启（`truth/db/sqlite/infra/database.rs:43`；实测重建被引用的父表在 `defer_foreign_keys` 下同样提交失败），
  所以先把 HEAD 唯一的引用列 `worker_sessions.spawn_op_id`（0163 定义；0103 的引用表已由 0125 删除）存入临时表并置 NULL；
  新表的列、默认值与其余 CHECK 同 0152，`INSERT … SELECT *`、DROP、RENAME，恢复引用，再逐字重建三个索引与 keyed 行 DELETE
  trigger（`0152:89-103`）。测试（C2b.0，calm-truth）：`upgrade_preserves_rows_refs_indexes_and_keyed_fence` 以 `migrator_through`
  （`track_merge_policy_ask_migration_tests.rs:8`）建前一版本库，灌原生 parked、isolated parked、awaiting_retry、keyed 行与引用它们的
  `spawn_op_id`，迁移后逐列相等、引用恢复、`foreign_key_check` 为空、索引与 trigger 的 `sqlite_master` SQL 不变、keyed 行仍不可删；
  `parked_check_admits_only_recorded_rows` 在正常迁移的库上：带 v1 记录、无 artifacts 的两种 gate kind 行可 park，无记录无 artifacts
  的、无 `COALESCE` 时会漏放的缺键行仍被拒，isolated 分支不变。C2b.1 的 `c2b_typed_park_needs_no_spawn_artifacts` 在正常迁移的库上经生产 `set_parked`，两种 kind 各一行。
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
  - Parked 且 Released：worker gate run 走 Stopping{KernelRestarted}，判 infra，保持今天的重启合同
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
  （`runtime/execution/mod.rs:106-127`）。Engine 抢先 Launch 的 Held intent 只是一个持有中的 shell，而 boot 从不做 Held→Released，
  所以先后顺序只决定是否 fork，不影响安全。(c) 接受，合并：记录与 typed park 的唯一消费者就是 cutover，typed park 只是上述三处改动。
  (d) 接受，共四片：行为不变的 C2b.0 之后三片。gate run 先行，因为它从不恢复判决，能先验证构造、释放、Stop 与 park；task verify 再加 Released 的重挂
  与 after-sample；最后一片是纯删除。mutation 验证只针对“授权先于首字节”、“boot Held 必 Stop”、timeout 围栏（含完成事务内复核）、typed 占用读者与 owned 留存守卫五条不变量。

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

之后的 S3（Revision、B pin、7/90）与 S2b（view、WorkspaceRef、WorkspaceContent）沿用各自协议，不在本表。

## 4. 隔离等级

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
  请求枚举、FE 标签映射，每处一次，由编译器强制。新增一类自有位置要在重叠检查的位置集合中加一处；进行中的 teardown 目标是一个进程内登记表，只有删除路径登记、只有 `none` 准入
  读取，新增删除路径时必须同样登记（由 sweep 的入参类型要求守卫来强制，不靠名单）。它只保护同一进程内的窗口，
  已启动子进程跨崩溃存活留作 G 的显式例外（#2862），不加持久登记。准入检查每次 attached
  创建运行一次（一次 SQL 读与少量 canonicalize）。新测试放在既有 target 中，每个 CI workflow 运行一次；增删测试不改任何名单。
  C1 是进程内围栏，不新增持久状态，每次 Planner turn issuance 多一次通常无争用的 track 锁；C0 每片 envelope +1，持久合同常量只在持久形状改变时变化；C6 若实施会新增一类
  operation kind。删除 D1–D3、D6 会显著减少 launch-admission 计划中的永久面。

## 7. 评审处置（计划评审第一、二轮与 C2b 设计评审）

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
| 第二轮 N4：`apply_recovery_item` 不持 drive | §1 改正锁事实（第四轮 adversarial N1：HEAD 已在取 claim 前持 drive，`driver.rs:1299-1302`、`:1326-1328`，§1 改为现状）；C1 加 boot 恢复一行，围栏在 adapter 而非 drive mutex 上，Busy 可重试；不以计时器宽限作顺序保证 |
| C2b 设计评审 boundary B1 / adversarial B1：SpawnStarted 重驱的“Stop、结算、再准入”没有合法的记录与别名转换 | 收窄而非替换：有记录的 SpawnStarted 写 Stopping{KernelRestarted} 加 Stop，typed park 同一 execution，结算后判 infra；记录仍只有三种写入。唯一需要替换的是 R8a 测试，不是生产消费者；R8a 改断言 |
| C2b 设计评审 boundary B2：`gate_ops` 读 `spawn_artifacts_json` 并调用 `marked_group_stopped`，不在任何切片里 | C2b.1 引入混合占用合同（有记录看 execution 结算），C2b.2 把无记录的原生 op 改判 Unproven，C2b.3 因此仍是纯删除；计入两片预算，`c2b_typed_gate_op_occupies_until_settled` 覆盖四个调用者 |
| C2b 设计评审 adversarial B2：过期的 owned op 在 boot 时以 PastDeadline 进入，丢失 Boot | owned 适配器在 deadline 分流之前走 Boot 分支，`op` 带 deadline；原生顺序不变；回归 `c2b_boot_after_deadline_keeps_boot_origin` |
| C2b 设计评审 adversarial B3：reconcile 在完成事务前算好 outcome，期间写入的 timeout/cancel 判决会丢 | release/decision 写者都先 `require_live_claim_tx`；gate 的 `complete_owned_parked_tx` 在同一事务内复核 execution id、release、decision，不同则回滚重算；结算是判决前提，Stopping 不单独授权完成 |
| C2b 设计评审第二轮 adversarial B1：Stop 型 cancel/补偿走既有 saga，结算前 `mark_failed` 或未结算即 Stuck | 有记录的 typed op 不进入补偿：`cancel_parked` 无生产调用者，对 owned op 返回 false，decision 去掉 Canceled；记录前失败无 execution，补偿无进程步；记录后一律 typed park，Stop 只写 decision 并留在 Parked，由同一 reconcile 结算后完成与 Retire；回归 `c2b_stop_decision_survives_unavailable_and_restart`、`c2b_cancel_parked_leaves_typed_op_parked` |
| C2b 设计评审第二轮非阻塞项 | claim 竞争不消耗一次性计时器或 ReleaseRefused（`c2b_timeout_retries_while_probe_holds_claim`）；首个 WriteStdin 前等可交互 receipt；不推进原生 op 按 kind 分片生效；parked deadline 是独立安全上限；新增行为不变的 C2b.0，超预算理由改为“生产消费者与其不变量测试保持原子”；boundary N1 的 603 是加法错误（逐项和为 +503），C2b.2 合计不变 |
| C2b 设计评审第三轮 adversarial B1：准入后 `set_parked` 失败，普通与 boot 外层 `mark_stuck` 终结有记录的 op，execution 无人 Stop、别名不 Retire；boundary N1：“共 3 片” | 三轮阻断同属一类，改为一条类不变量：有记录的 op 只经 reconcile 完成事务离开 SpawnStarted/Parked。四个通用离开写者在事务内按适配器声明的 owner-state 键留存（只写 `last_error`、lease 即退避、到期再 drive），附全部写者穷举表；删去 `plan_compensation` Conflict 等逐例外规则；守卫放进行为不变的 C2b.0（约 270），C2b.1 加两条回归（约 1100）；(d) 改为四片 |
| C2b 设计评审第四轮 adversarial B1：0152 的 parked CHECK 只许 isolated 无 artifacts，typed gate park 必败，留存后永久重试 | 删去“不加迁移”；C2b.0 加新迁移，第一支改为“有 artifacts 或有 v1 `gate_execution` 记录”（`COALESCE` 防 NULL 放行），按 0152 步骤重建并逐字保留索引、keyed 行 trigger 与 `worker_sessions.spawn_op_id`；calm-truth 升级与正常迁移两个测试，C2b.1 经生产 `set_parked` 覆盖两种 kind；C2b.0 约 510，改为 L2 |
| 第四轮 boundary B1：owned Parked reconcile 出错时先清 lease、不写 `last_error`，sweep 立即重取 | C2b.0 在 `owned_parked.rs` 加与 owner 无关的错误留存：以本 claim 为围栏的 UPDATE 只写 `last_error`，保留 lease 作退避；回归 C2b.0 lib `error_keeps_lease_and_writes_last_error` 与 C2b.1 `c2b_malformed_record_is_retained_with_backoff`（SpawnStarted/Parked × boot/稳态） |
| 第四轮非阻塞：boundary N1 写者表缺 `write_phase_and_tx_output`；N2 永久成本；adversarial N1 锁基线 | 表加该行，保证写为现有生产调用路径并约束新调用者；区分一次性键参数迁移与日后 owner 注册，写明 CI 频率；§1 与第二轮 N4 改为现状 |
| C2b 设计评审非阻塞项：行号、同宿主 Unknown、计时语义、预算 | 改正 `gate_process.rs` 与 `task_*` 删除范围；明确同一 OS boot 内 Unknown 不可恢复、不承诺结算；区分授权计时与 parked 安全网，已结算 verify 过期后取 status；两片预算按文件重估，均超出约 800 并写明无安全切点 |
