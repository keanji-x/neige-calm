# Track Workspace 框架

状态：S1–S2a 合同已定；S3 的交付/验证实施协议见 [workspace-delivery-protocol.md](workspace-delivery-protocol.md) 与 [workspace-verification-decision.md](workspace-verification-decision.md)，不声称已实现。S2b 的机制修订见 [workspace-view-backend.md](workspace-view-backend.md)，待 fresh 双路完整 L2 设计评审。方向为 Workspace 框架 + git worktree / Linux 同路径 COW 视图两种实现；[track-cow-view.md](track-cow-view.md) 的 overlay 实验只作历史证据。

## Outcome 与初版边界

kernel 统一拥有选择、授权、交付、恢复和回收政策；workspace 实现只做文件系统与进程机制。Linux attached track 最终推荐视图：同 track 进程共享私有写入，能读取来源 checkout 的 dirty/ignored 文件，仓库内写入不落到用户 checkout。现存 track 不自动切换 backend；不引入 Lazy 升级、用户 checkout 备份或自研 Codex exec 协议。

初版 opt-in，默认 worktree；仅开放通过合同的 Linux Git 入口，创建与关闭/删除/恢复、证据和文件 API 同片。未完成入口拒绝，不回落来源目录；推荐默认、非 Git 交付和非 Linux 新实现后置。仓库外写入仍由 provider 沙箱约束，网络/端口不隔离，外部副作用仍走 #2617 批准。

S2b 选择直接 COW mediator，复用 S3 的 FUSE 基础与权威 Git 分类；不在可变来源树上挂 kernel overlay。官方 overlay 合同将挂载中 lower 变化定义为不允许/未定义，空闲重挂与成功压力测试不能补成保证。活动视图使用稳定 VFS inode/alias-group 与 first-write 私有 backing；不承诺 per-open immutable 图像。fresh lookup 复核来源名称/身份，原地更新的 bytes 可受已有 page cache 影响，须全静默 sync 重建挂载才保证消除旧缓存；旧 RO FD/mmap 可观察同 inode 后续私有写入，不取得写权限。不冒称严格 close-to-open 或全 POSIX cache 等价。

保留同路径及 dirty/ignored/热缓存可见目标，不承诺全树同一时间点。交付/验证只消费独立受控 D/W 与 S3 双闭包，copy + stat/hash 不能证明来源树原子快照。Git owner 注册的规则输入是 live 来源的显式例外：同一 S3 RuleView 控制可见规则 bytes/absence、private index/config、classifier、D/W 与 snapshot/gate；外部 dirty 规则保持 pending，view 内规则/分类变更经 typed RuleChange + SourceInstallPlan，全 drain 后原子开放一个版本，owned cache/source 的重分类须显式处置，不静默丢源码或纳入历史产物。

新增 VFS、COW 和 mmap/cache 复杂度须按 S2b 协议独立验收；当前宿主缺 `/dev/fuse` 不能声称 qualified，也不把整个 ext4 源存储类判为 Unsupported。未来原子来源快照可注册为独立能力，不是初版依赖。metadata lookup/read-only open 不全量复制文件，首次私有写才复制同一 inode-group 一份；typed 容量预算先预留，耗尽不回落来源。

后续实现等级 L2（隔离、授权和持久化边界），每片由 Planner 安排两路独立评审；本文不启动实现评审代理。本文只改文档，只跑文档 gate，不运行 Rust 或真实 Codex/Claude。

## 1. Facts：当前生产路径

核实 HEAD `9adf4d99cf426af51fcf2878d86555b7f474bac8`。完整阅读第四轮两路评审后，以源码核实；不是把评审建议当成已实现事实。以下位置均相对仓库根，`server/` 简写 `crates/calm-server/src/`，`truth/` 简写 `crates/calm-truth/src/`。

### 1.1 穷举口径

前轮已搜索 crates 的 `agent_cwd(`、`workspace.path/worktree`、`workspace_worktree_path`，沿持久 cwd、Command/current_dir、FS/Git/spawn/重驱追踪，补查 FE。排除注释、测试/fixture 和无关字段，不按文件名排除。本轮复核 lease/catch-up、checkpoint/gate、重驱、provider、operations、FS 与排除规则；下表是生产链证据，不是永久允许名单。

| 链路 | 当前行为与源码证据 |
|---|---|
| 模型/投影 | `calm-types/src/model.rs:192,210,244`：kind/path/frozen_at/worktree；accessor 优先 worktree；cwd 为 wire alias。`truth/db/rows.rs:67,74,106,135`、`truth/db/sqlite/track.rs:204` 投影；0120 migration 增 nullable worktree，不回填 |
| 单写者/冻结 | `truth/db/sqlite/track_workspace.rs:12,58,81` 原子写四列，frozen_at 闩；system launchpad 例外。首次 terminal 与 lease 关闭闩：`truth/db/sqlite/card.rs:624`、`server/operation/workspace_lease/mod.rs:235` |
| 创建/重驱/repoint | `server/routes/tracks.rs:1755,1825,2245`、`routes/tracks/create.rs:596,660,713`：提交后 ensure、Replay 保留 cwd；repoint 读 pristine 状态并重启。`workspace_materialize.rs:55,76,109` 建 managed 目录/仓库。创建事务并不知道实际 worktree 基线 |
| 其余 Planner 入口 | `server/routes/{planner_cards.rs:696,track_conversations.rs:217,today.rs:312}`；today 在 `:541,553` 以 path 建 key/启动。child `operation/child_track_adapter.rs:220,240,402` 复制 kind/path、worktree None，将 cwd 写 tx_output；`scheduler/mod.rs:1212,1227` 回放 |
| provider 配置/会话 | `server/{claude_planner/wiring.rs:61,acp_planner/wiring.rs:45,harness/run_loop.rs:2881,routes/models.rs:459}` 取 accessor；`operation/planner_harness_start_adapter.rs:1048,1174` 持久化 cwd；`crates/provider/src/codex/mod.rs:378` 编 RPC |
| 本地启动 | `server/claude_planner/{session.rs:471,rewind.rs:107,readiness_command.rs:79}` 与 `acp_planner/process.rs:33` 构造 Command/current_dir；只改 session 字符串不足以进 namespace |
| worker/terminal | `server/operation/{codex_adapter/mod.rs:980,1216,claude_adapter/mod.rs:478,828,terminal_adapter.rs:217,393,790}` 传 cwd；显式 terminal cwd 优先；terminal task 没有 worker lease |
| 持久化 cwd 回放 | `server/operation/task_verify_adapter/mod.rs:373` 按 gate.cwd → lease.path → terminal-worker tx_output.cwd → task.cwd/accessor 选择；`claude_restart_adapter.rs:211,271,348,387,447` 与 `routes/terminal.rs:40,128` 回放 terminal.cwd；forge 的 FrozenForge.cwd_lease 在 `forge_action_adapter/mod.rs:1288` |
| 独立 supervisor | `crates/calm-session/src/control.rs:17` EnsureProcRequest.cwd 为 String；`crates/calm-proc-supervisor/src/lib.rs:1162,1259,1283` 分别 pipe/PTY fork；当前 PTY 先在宿主检查目录再设置 cwd，不在 kernel 的 TrackedChild 中 |
| forge/gate/checkpoint | `server/operation/forge_action_adapter/mod.rs:1009,1296` probe/action 都构造 Command；`gate_process.rs:279` gate shell；`task_verify_adapter/target.rs:326,433,456,567` 采样 HEAD/status/provenance；`task_gate_run/checkpoint.rs:43,72` checkpoint 脚本和 refs_digest Git 读 |
| lease/准入 | `server/operation/workspace_lease/{worker.rs:137,211,417,515,facts.rs:102}` 读四列、要求 clean tree、采样 HEAD、检查只读 head。`crates/calm-truth/migrations/0130_read_only_tasks.sql:16` writer 唯一索引按 path；`truth/db/sqlite/track_occupancy.rs:42,83` 合并 task/lease/未结算 delivery，不证明进程退出 |
| worktree materialization | `server/operation/workspace_lease/track_worktree.rs:65,96,99,105` ensure 先 fetch，复用已有 branch，否则此时选 base/upstream；upstream 先写 branch config，部分失败可留下 branch。`:124` choose_lease_start 保留 ahead/behind/diverged 语义 |
| catch-up/交付/publish | `server/operation/workspace_lease/worker.rs:105,230,333` 的 T 是最新 done candidate，reset --keep U。`crates/calm-types/src/forge_git.rs:31,93,108` 检查真实 worktree/branch，add -A 会执行 clean filter，checkpoint soft reset 到 lease base。`server/git_candidate/delivery.rs:191,538` 冻结参数并事务写 settlement/candidate；`staleness.rs:51` 独立判断 |
| Git 事实消费者 | `server/{mcp_server/transport.rs:1111,mcp_server/tools/plan.rs:632,scheduler/mod.rs:151,builtin_plugins/dev/publish.rs:71,169}`；publish 要 worktree Some，从 branch config 读 upstream。`crates/plugin/src/builtin/gitforge/publish.rs:227` check_tip 核对 candidate 链 |
| 工作区文件 | `server/routes/fs.rs:129–375` 有通用绝对路径 FS 端口，也已有 track read/raw `:288,318`（`:298,328` 用 accessor）；`planner_attachments/mod.rs:52,60,121` 与 `routes.rs:70,110` 宿主读写附件，当前有 managed-only guard，不是现存 attached 越权写 |
| managed 回收/恢复 | `server/workspace_recycle.rs:18,31,137,191,485,544` marker 恢复闸门、.trash rename/补偿、7 天 GC；`harness/mod.rs:186`、`planner_hibernation.rs:228`、`routes/planner_recovery.rs:53` 消费恢复闸门 |
| close/delete/area | `server/operation/workspace_lease/{track_worktree_clean.rs:22,27,117,mod.rs:352,391,track_worktree.rs:143}`：10 分钟 clean ignored，删除会丢 dirty tree/branch，失败 warn。`routes/tracks.rs:3113,3120,3257` 与 `routes/areas.rs:470,488,614,653,658` 回收/补偿/最终 sweep；不能旁挂第二套 reaper |
| 静默与锁 | `server/planner_hibernation.rs:1,12,95` 已有 close 静默判定和显式唤醒，锁序 card → drive → track → resume replay；`calm-types/src/worker.rs:267` idle 仍可能有权限，terminal exit_code None 不证明已退出 |
| API/FE | `calm-types/src/event.rs:81,342` TrackUpdated flatten Track；`fe/core/{api/schemas.ts:76,domain/track.ts:29,89}`、generated wire/OpenAPI 保留字段。`fe/web/src/app/router/public.tsx:1066,1391,1436`、`systems/fs-viewers/public.tsx:152` 从 agentCwd 调绝对路径 FS API；旧 worktree.* invalidation 不是 backend 判别器 |
| 宿主服务与排除项 | `server/shared_codex_appserver.rs:2528` 启 app-server，工具需另路由；`plugin_host/child_process.rs:71` 插件宿主不是 track launcher，受其独立权限合同约束，若代跑仓库动作则进入 forge 链。github_preview 从 `/` 调 gh；funnel、rollout zstd、binary probe 与 supervisor socket/config 的 current_dir 用于 kernel 自有服务/产物，不是 track cwd |

已有 `crates/sandbox/src/lib.rs` 是 typed bwrap policy（最小宿主挂载、进程 namespace、环境配置），desktop 在用；不是可接管的常驻视图 holder。`external/codex` 不在 checkout，历史行号不能证明部署版本 experimental 协议或宿主项目读取行为。

## 2. S-A：一条源码证据路径

### 2.1 Revision、Tree 与运行视图

分开三个对象：活动视图观察可变来源；私有写入记录是 agent 相对已安装源码的修改；`Revision` 是持有内容的不可变交付证据。只给私有层计数不构成源码证据。

`Revision { workspace_id, install_epoch, content_digest, tree_oid, materialized_source, changes, overlap, normalization }` 由框架 snapshot 产生，绑定规范化 Git tree 与必需的 MaterializedSource 工作字节 manifest。manifest 保存路径、原始 bytes/mode/symlink 与规则身份，不把 filter/LFS 指针当工作源码；同一 content owner 持久化两类闭包。changes 含 tracked（即使被 ignore）、新增非 ignored、删除、mode/symlink；不含内部 Git 元数据和新增 ignored 产物。不支持类型或无法取得必需工作字节时显式拒绝。已 commit/status clean 不替代完整源码证据，非 Git 的 DirectoryRevision 初版不进入 Git candidate/gate。

创建基线 B 永久不变；当前安装基线 D 初始为 B，catch-up 准备后为 U 的 tree，agent replay 的结果属于 W。视图将 kernel 安装状态 D 与 agent 写入状态 W 分开记录（独立安装清单/代次，不能把整个私有层当 W）；snapshot 合成 D ⊕ W。未触碰的活动来源脏源码不自动进入交付。first-write capture 可能含用户改动时标 overlap；归属无法证明也保守标记，确认绑定 revision，未经确认拒绝交付。snapshot 只读完整独立的受控 D/W；并发 manifest 变化或未决来源复制重试/Busy，不能把活动来源采样当全树原子快照。

worktree 同样返回 Revision/GitFacts，只在实现内解析真实 checkout。kernel 的 lease、gate、只读 head pin、publish、catch-up、archive 只消费这组证据，不按 same_path/live_source 分支。Capabilities 布尔只做展示；动作以类型化 Result/Unsupported 和准备好的 plan 为准。

### 2.2 被测树 = 交付树

`prepare_verification(revision, candidate)` 返回证据句柄：开始/结束观察到的 tracked tree 都等于 candidate tree，期间无 install_epoch 变化和源码写入；新增非 ignored 文件同样使证据失效。gate 结果绑定 `(revision, tree_oid, candidate_oid, environment_digest)`，缺证据不能 bound。此为不变量，不规定统一物化方式，不新增 GateSpec 字段。

worktree 验证保留相同逻辑 cwd、用户 GateSpec 步骤和 Git 权威分类的 ignored 热输出/缓存位置，改由 immutable C 对应的冻结工作字节 manifest + 逐路径受控缓存投影提供；物理 inode/mount 语义明确改变，不要求新输出目录或 GateSpec 字段。tracked 优先于 ignore，源码实际写入始终被阻止；到达 mediator 的源码写请求拒绝并 latch，VFS 提前拒绝且未改源码的调用不虚构回执，调用者处理错误后可按普通 gate 结果 bound。没有完整能力时保留未转换 Legacy 或拒绝准备，不静默降为 unbound。view 钉住同一源码证据，活动 lower 的其他修改不影响被测内容。只读 head pin 同样消费冻结源码证据，不凭工作 HEAD 猜身份。

Git 内容规范化属于 snapshot：worktree 保留现有 Git clean/smudge、eol、fileMode 语义，在 launcher 内执行可能运行仓库代码的转换，记录规则和规范化 tree；验证使用同一规则。normalization 同时记录 ignored 分类的规则来源/摘要（仓库 ignore、有效 Git exclude 配置、kernel `.neige` 排除项）；view 私有 info/exclude 安装同一规则，snapshot 与 agent Git 观察一致，tracked 文件不因 ignore 消失。宿主可信写入器只接收已规范化 blob/tree，禁 hooks/filter/fsmonitor，不能用“禁 filter”代替规范化。初版 view 对 custom clean/smudge、LFS、working-tree-encoding、eol/autocrlf 等转换仓库明确 Unsupported（创建及 snapshot 重检 attributes/config）；不能把指针或工作树原字节静默当交付 blob。放开必须新增正反向物化等价合同。

### 2.3 attempt、交付与 Git facts

交付 tip 的唯一权威是资源账本。所有 S3 delivery/candidate/run/archive/retained baseline pins 与可写 best-effort 分支投影放在私有层之外、按资源授权的 kernel-owned durable content repo；refs 只作 GC pin/intent，不承担 tip 权威。用户 common-dir refs 仅观察，不靠 worktree-list + CAS 或 cooperative lock 写删：人可在采样与 CAS 间 checkout 同 OID 分支。kernel 的 owned 工作 HEAD/index 是 detached 执行投影，HEAD 写一律 update-ref --no-deref，不移动附着用户分支。须将 forge_git/lease/spawn 的 attached-branch 校验改为资源身份/源码证据，spawn 校验失败仍为 spawn-failed；upstream 全消费链与 detach 同片切账本 receipt，无 branch-config fallback。

lease 记录 identity、开始 Revision 与 `LeaseBase` 起点 P：普通 attempt 只取已结算 tip，catch-up 取已结算的 U，不从工作 HEAD 或上个 candidate 推断。dirty 判定是 Revision 相对已结算 tip 的差异，并拒绝未完成 Git 操作/冲突和未确认 overlap，不看工作 HEAD/status 或活动 lower 的 dirty。准备持排他准入，确认 Revision 为空后由 kernel 安装工作 HEAD/index 为 P；私有 commit 的相同 tree 不算 dirty，其历史不进入父链。Planner 私有改动先由 git_commit 结算或撤销。

C 只有一种构造方式：launcher 内 snapshot → 规范化 tree → 可信 plumbing 提交（父为冻结 P）→ pin → 安装工作 HEAD/index；gated checkpoint、ungated 和 Planner git_commit 共用，不走 git commit/hooks/签名，agent 私有历史只是源码输入。同 attempt 各 run 固定 P，未交付 run 不改变 GitFacts.delivery_tip。Planner git_commit 经 gitforge Services 的类型化方法进入 kernel（参照 builtin_plugins/dev/publish.rs 的 Services），先走 checkout_admission，非 Free 返回 Busy，Free 时登记排他 owner 并持有至结算；forge 适配器不得按插件/动作名截获。Planner 提交推进已结算 tip，publish 仍区分 Planner commit 与 done candidate。

完整 C/父/tree/message、规范化与工作字节闭包、预期 refs 与 source-effect 计划先入现有 operation intent。最终 candidate pin + delivery ref P→C 在独占 kernel repo 一次 update-ref 事务，随后私有投影/执行元数据 receipt，再由 settlement/candidate 同一 DB 事务推进账本。唯一 effect owner 签 sealed ProvenSameSourceProjection 或 SourceInstallPlan：前者证明 raw bytes/mode/link、tracked/nonignored 分类、normalization/ignore/config、权限与 mount/root 全不变，只改私有 HEAD/index/refs 和 baseline bookkeeping，保留 source epoch/generation，独立推进 metadata/baseline sequence。后者先固定双闭包 target；若来自 provider 同步调用，durably Accepted 后原调用必须实际结束/cancel，再全 scope 正向 drain、唯一 install/epoch receipt、新 ExecutionId/alias readiness/resume，以新 capability 取原结果。未完旧调用 Busy；按同一 intent 恢复，不按工具名/已有 owner 豁免。此分类覆盖 checkpoint/settlement/lease restore/catch-up/recovery。自有 pins 无法对账才 DeliveryRecoveryRequired，区分物理 WorkspaceRecoveryRequired；修复保全内容并重驱原 C/settlement，不采纳偏离 ref。

`GitFacts` 返回资源 repo identity、受保护基线、账本 tip、固定 upstream、候选及 ancestry/内容 receipts。Gitforge check_tip 仍拥有 done-candidate 政策；typed PublishTarget 从授权 scoped export 推冻结精确 OID 到 frozen remote ref，不依赖用户本地分支或私有展示投影。catch-up 冻结 H=账本 tip、T=最新 done candidate、fresh U、M 后先备齐 U 工作字节/规范化闭包，再由唯一 effect owner 分类；真正源码安装执行全 scope drain 后 CAS 私有 pin H→U、安装并结算 U/发布 P=U lease。保留 agent 回放 M..T，kernel 不 replay、不 reset 用户 checkout。

归档消费同一 Revision/content owner，A 父为账本 tip，不推进交付；ArchiveReceipt 必需 pin A 的规范化闭包与 raw 工作字节 manifest，一次 ContentReceipt 覆盖两者。B/tip/run/candidate/install 的实际源码消费者也必须有 MaterializedSource，HistoryRoot 不能当 launch/restore 内容。保留期仅按引用释放 kernel repo 自有 pin/私有投影，用户 common-dir 分支/旧 refs 不自动删；candidate、未结算操作、open/unknown PR 继续保护其精确对象。remote branch 删除仍为独立授权 Gitforge effect。

## 3. S-B：类型化启动与文件能力

### 3.1 所有权与持久化身份

新增 `workspace` crate，不加 calm- 前缀。它拥有路径解析、Backend、句柄/代次、launch 与内容；`calm-truth` 拥有事务/冻结/CAS；kernel workspace service 拥有授权、选择、生命周期、Git 交付政策并复用 operations；provider 拥有外部协议与 ViewSupport 资格声明。实现不读 tasks/approval，不判断模板/插件，不决定保留期或准入。复用 checkout_admission，不另造并发任务政策。

持久身份只存 `WorkspaceRef { workspace_id, cwd: WorkspaceCwd(RelativePath) }`，track 归属由领域记录关联，不把运行代次写进启动身份。需证明源码身份的 lease、gate target、冻结 forge/checkpoint/交付 intent 另存 install_epoch/Revision；重驱核对它们，不能重绑成当前源码。覆盖 lease、terminal、operation input/tx_output、FrozenForge、child output、session/handle、task.cwd、gate.cwd 和恢复/重启记录。writer 唯一索引与 supersede 以 workspace_id 为键；跨 track 不共享 workspace 的 view 不因 same_path 冲突，legacy 同一真实 checkout 仍映射同一锁域。

新增 migration，不改旧 migration/事件字节/hash。旧 cwd 只由显式 LegacyWorktree decoder 核对原身份和根后转换；未能核实拒绝恢复。历史事件不回填；已冻结旧 operation 的 payload 不原地改写，新增 sidecar binding 由 op id CAS 保存并供重驱消费。新 operation 必须用新类型；不能通用“缺身份就读 cwd”fallback。显式绝对 cwd 在 API 边界验证后转换为根内相对路径；旧 worktree 根外显式 cwd 使用独立类型化 `ExternalDir` 启动目标，无 workspace 账本/生命周期/源码证据；根外 gate.cwd 明确 unbound。保留既有行为，不伪装为 workspace。view 初版拒绝根外 cwd，不能借 ExternalDir 绕开来源保护。

### 3.2 LaunchHandle 与运行期闸门

S2a 下一实际启动/准入桥接见 [workspace-launch-admission.md](workspace-launch-admission.md)。foundation 的 logical selection 与 Pending/Observed 只作绑定观察，不生成 WorkspaceRef 或运行能力；同一 workspace owner 取得连续 live-FD 对象证明、materialization/adoption receipt 后才归一 CanonicalWorkspaceId 并 Ready。dev/inode/path/btime 和可复制 marker 都不是跨生命期证明；旧任务/租约/operation codec 的 source binding 闭合后才一次切换 resource admission，不把内部 executor checkpoint 当已交付接口。

唯一解析入口 `resolve(principal, purpose, WorkspaceRef) → Ready(LaunchHandle) / WorkspaceRecoveryRequired / Unsupported(reason)` 先核对资源、授权和恢复状态，再生成当前代次句柄；恢复只解析身份，源码证据仍须单独匹配。generation 只进入 LaunchHandle、单次 ticket 和 environment id（资源运行状态保留计数），不进入持久启动身份或 Revision。track spawn 只接受不可伪造的 LaunchHandle，宿主 cwd 构造器在 crate 内私有。gate shell/采样/checkpoint、forge action/probe、Claude session/restart/rewind/readiness、ACP、普通/任务 terminal、child/system/today 都经它。展示路径不是启动权限；扫描发现回归，不证明隔离或维护文件名单。

每次 spawn（包括重复调用旧 handle）在同一 lifecycle fence 下原子校验 generation、install_epoch、closing/admission 状态与 permit，登记 launch intent，再将可消费的单次 launch ticket 交给执行器。supervisor 在 fork 前核销 ticket；关闭/恢复必须等未决 ticket 被确认或撤销，不能检查后释放 fence 让迟到的 fork 穿过关闭。失败/未知启动不能当空闲。被封装的宿主 spawn/read/write 原语若命中已登记的 view 来源根却没有相应内容/管理能力，运行期拒绝；裸 std::fs/Command 只能靠评审/扫描发现，此闸门不声称覆盖任意宿主代码或单独证明隔离。使用目录身份/受控 fd 防 symlink 和路径换名竞态，不仅比较字符串前缀。

PTY 由 calm-proc-supervisor 实际 fork：扩展 calm-session 协议的版本握手与 launch envelope，传受控 namespace fd/身份、uid 映射、workspace generation 和 ticket；supervisor 先进入视图再检查逻辑 cwd、fork，用真实 uid/gid。kernel 内的类型限制由序列化能力票据延续到这个进程，不能发送任意 PID/ns 路径。旧 supervisor 不懂 envelope 时，拒绝 view；升级先 drain，不能为了升级杀掉未知旧 PTY。worktree 旧协议只在显式协商的 legacy 模式保留，不能服务 view。

### 3.3 三类宿主文件能力

| 能力 | 授权范围和消费者 |
|---|---|
| WorkspaceContent | 通过 workspace handle 的 read/list/status/diff/write/verification；工作区文件、Report 相对链接、HTML/image/raw 预览、provider 项目配置和附件均在此类。保留附件 managed-only 准入，既有 `.neige/attachments` 也由受控 write/open 访问，不新增宿主直写通道 |
| KernelArtifact | kernel 自有日志、operation/terminal 产物、runtime identity 与资源账本 storage；不接受工作区路径，不将 artifact 路径当用户 checkout。服务自有产物用此能力；view 内 provider 服务启动仍须 LaunchHandle，不能据此执行 track 仓库脚本 |
| SourceGitAdmin | 只允许框架提供的 repo identity 与自有 ref CAS、对象 pin/写入等固定 plumbing；不接受任意 argv，不执行仓库代码。hooks/filter/credential-sensitive 动作走 launcher 与既有凭据拆分。不能让 forge probe 冒充可信 Git |

复用 `/api/tracks/{track_id}/workspace/readfile{,-raw}`，内部 opener 改用 WorkspaceContent；补齐 track list/status/diff 并迁移 FE 绝对路径消费者，不新建平行 read/raw API。输入相对路径，保留鉴权、越界、symlink、MIME 和 revision 冲突规则。旧绝对路径通用路由对 view 来源根拒绝；不能让 FE 留在旧 agentCwd 规则上。checkpoint refs_digest、lease facts、publish/staleness 都用 GitFacts，不自行开 cwd。

### 3.4 namespace 与 provider

私有 backing/manifest/runtime 只在 backend 内定位；活动来源仅由 mediator 持只读能力，`.git` 从 S3 授权独立 export 初始化，绝不 union/overlay 原 `.git`。linked gitdir/common dir/alternates 的路径只取 Git 报告；初版外部 gitdir/alternates 拒绝，不能写穿真实 common dir。launcher 映射回真实 uid/gid，环境显式 allowlist，继承 provider 的仓库外沙箱。复用 sandbox crate 的 typed 挂载/环境与子树约束；常驻 holder 的 open/recover/join 属 workspace 新机制，bootstrap/join 消费 S2a 同一 journal/ExecutionId/ticket/control 的显式 envelope 扩展，不另建进程账本/driver。现有 bwrap policy 是一次性进程沙箱，不另抄一套通用沙箱政策。

S4 改用受控 view 内的整个 provider 服务，实施协议见 [workspace-provider-environments.md](workspace-provider-environments.md)，须经两路 L2 评审后实现。已实测共享 app-server 的远端 exec 路由不能隔离其宿主 config/文件/搜索，断连后 thread/start 仍可成功，原“共享 app-server + 每 view exec-server”不足以满足本合同。provider 按 workspace、generation、授权 binding 和 ExecutionId 管理独立服务、可变状态及控制通道；kernel 只验证类型化能力，不按 provider 名称特判。Codex 初版在该 scope 中使用 app-server 官方本地工具，不登记远端 exec-server；不猜 RPC 字段、注销/drain 方法或私有状态布局。

注册/readiness、每次调用、重连/恢复和所有控制均绑定授权、generation、install_epoch 及精确执行实例。S3 owner 区分有证明的 SameSourceProjection 与真实 SourceInstall：前者只推进 owned Git 执行投影/账本序列，不改源码 bytes/mode/link/分类/规则/挂载身份或 source epoch，不 drain 正在等该结果的 provider；后者先持久结束原调用，再 drain/install/新执行恢复并用新能力取原结果，未完调用始终 Busy。具体 S3 接缝由实施协议提出、须与 S3 owner 统一后实现；不能按工具名或既有 owner 豁免。重启、真实换安装代次或回收须核实旧服务、工具和后台 helper 全部 drain；父进程退出、连接关闭和 DB idle 不构成静止证明。私有 provider state 与显式认证输入分离，不挂载成员私人 HOME、kernel 全局对象库或其他 Area 状态。未具部署版本/平台/认证 profile 的完整资格时拒绝，不 fallback 到共享 daemon 或来源 checkout。

view worker 采用类型化受控 console，保留输出/结果观察、继续 turn、approve 和 stop；每次输入经同一 ExecutionControl，不把终端字节写进 app-server stdio。view 不再启动直连 `codex resume --remote` 的原生 TUI，原 worktree 交互不变；这是显式产品合同修订，须双审。view 服务只用受控匿名 stdio，工具子进程的 PID/proc/挂载与 FD 边界隔离服务控制和私有 state，不能凭隐藏 URL 或同 uid 宣称安全；通用 UI 按 console 能力渲染，不按应用/provider 名称特判。

provider 所在层声明类型化 `ViewSupport = Supported / Unsupported(reason)` 并提供合同测试；kernel 只消费声明，不按 provider 身份特判。Supported 必须证明项目读取、配置、Git/ghost/undo/搜索及按 cwd 的状态均隔离或显式禁用，整个服务进入 view 的元数据实验也不等于完整资格。S2b 负责本地 provider 合同：Claude 的共享 CLAUDE_CONFIG_DIR 不能证明 project 状态隔离，须按官方合同建立独立作用域；未证明前 Unsupported。任意配置 ACP 默认 Unsupported，只有具体适配层通过合同才能声明支持。Codex 由 S4 的部署版本/平台合同解除。可共享显式授权的推理/鉴权输入，不共享同一 app-server 进程、可变 home 或 trust 状态；Codex TUI `!` 不支持。

### 3.5 Area 协作的授权接缝（S2a / S3）

Area 成员共用同一 Track 的 WorkspaceRef，不按成员建立另一份工作区；占用按
workspace_id 汇聚。不同 view 即使展示同一路径也不是同一资源；legacy 同一实际
checkout 的身份归一由 workspace 框架负责，协作层不另按路径建锁或账本。

`resolve` 的 principal 是授权内核验证过的主体上下文，不是消息作者、展示名称、
`ActorId::User` 或 `X-Calm-Actor` 声明。读入口带真实访问者的成员/资源访问授权；
已接受的共同执行带协作授权层签发的执行授权上下文，引用被授予的
AreaEnvironmentBinding 及其版本/权限范围，作者仅作消息来源和审计。具体主体与
授权凭据类型由授权层拥有，kernel workspace service 验证并消费；workspace crate
只消费已授权的计划/能力，不解释 AreaMember、成员策略或凭据选择。

purpose 是 kernel workspace service 拥有的类型化用途，至少区分内容读取、
内容写入/执行及生命周期控制。调用入口声明用途不等于取得权限；不能由 adapter
自行把普通读取升级成执行或管理。读取者的授权不能充当共同执行授权，也不能
借消息作者的私人 HOME、Git/终端凭据或 Provider 配置补齐共同身份。共同授权上下文
必须传到所有启动和内容入口，配置/项目读取也使用同一绑定；不支持时明确拒绝。

持久 Turn 固定协作授权绑定引用和 WorkspaceRef；新准入、恢复和重启重新核验
该授权及冻结绑定，不静默改用当前成员、另一授权绑定或另一工作区。授权变化对
已接受批次的政策由协作授权层决定；需要停止时进入同一 kernel 生命周期入口。
AreaEnvironmentBinding 的授权身份与 workspace generation、namespace、ticket、
Provider environment id 是不同概念；后者不能替代前者。现有共享推理/鉴权合同
不自动证明多用户凭据隔离，隔离与成员授权未经集成验证不得开放共同执行。

停止请求已发出不构成静止证明；kernel 资源协调消费 executor 的实际执行回执，
未决 ticket、未知进程继续阻塞破坏性回收。旧执行的停止/清理不能重新指向重启后的
新执行。任务 admission 与破坏性 quiescence 分开，普通终端存在不自动新增 worker
排斥规则；协作层拥有消息批次和顺序，不复制租约、停止证明或资源生命周期。

上述身份/用途、授权上下文、占用与启动/内容接缝归 S2a；Revision、GitFacts 和
交付 tip 的唯一权威归 S3，精确交付协议仍须按 §6 先评审。需源码身份的执行另存
Revision/install_epoch，不能用 WorkspaceRef 或运行 generation 代替。协作层只引用
框架证据，不另建 candidate/tip/交付状态。此节定义实施合同，精确 Rust 类型和
可消费提交在对应切片实现时确认；不向 S1 增加预留 API、授权行为或 schema。

## 4. S-C：资源账本与两段式持久化

### 4.1 selection 与 materialization

资源账本由 kernel 拥有，生存期独立于 tracks；track/area 删除不能 cascade 它。保存 workspace_id、versioned descriptor、repo/storage identity、selection version、materialization receipt、B 的保护 ref、当前 D/tip/upstream、运行 generation/source install_epoch、projection/baseline sequence、独立 durable content repo/双闭包 receipts、runtime identity、close 截止时间及 tombstone。账本只保存资源状态；机制操作注册为现有 operations kind，复用其 idempotency_key、payload_hash、tx_output、阶段/probe/补偿，保存不可变 input、expected CAS version 与 FS/ref receipt，不建子账本，也不依赖已删除 track 回查。

selection 是 backend/source/storage，由现有唯一工作区写者与四列一起写，未冻结时 create/repoint/today launchpad 都可更新；与现有 frozen_at 闩一起冻结，不提前到创建事务。system 特例仍只在 kernel，不启用 view。materialization 是 Pending → Ready(receipt) 的显式枚举；baseline/upstream 在 ensure 或 view create 的实际成功点以 NULL→值 CAS 安装，不从创建请求猜值。LegacyWorktree 为显式变体，保留 attached None、managed、child 和旧 cwd/refusal；不凭默认值补虚假 baseline，unborn 是独立类型。

worktree ensure 保留 fetch/choose_lease_start 时点，部分失败后探测已建 branch/实际 base；重试只能记录实际 receipt。S3 切换前 branch config 是 worktree upstream 唯一权威；S3 与 detach 同片验证一致后切换到账本 receipt，后续消费者统一读 GitFacts，不双读 fallback。冻结约束 selection，不阻止首次 materialization 或后续授权 install；恢复不能重选 backend/source。

### 4.2 机制合同

机制返回强类型 receipt；kernel 只传 immutable plan/能力，不拼 upper/mount/gitdir 或调用 backend helper：

| 接口族 | 输入与输出合同 |
|---|---|
| create / open / recover | selection + op id / ledger descriptor + runtime identity；返回句柄、实际 materialization receipt 或 WorkspaceRecoveryRequired。相同请求幂等，不同资源拒绝覆盖 |
| snapshot / prepare_verification | handle + exclusive/read permit → Revision/SnapshotHandle；固定 candidate tree → VerificationHandle/环境证据；内容受保护到所有消费者释放 |
| plan_source_effect / stage_install / install / inspect_install | 唯一 owner 验证 footprint 后签 sealed ProvenSameSourceProjection / SourceInstallPlan；固定双闭包 target、before identity 与序列。前者保 source epoch，仅推进 metadata/baseline receipt；后者按 Accepted/原调用结束/drain/install/new execution/result 协议安装，拒绝未知旧调用 |
| install_delivered | 先经同一 sealed effect 分类；证明源码/环境不变时只设 owned HEAD/index、逻辑 D/W 与 projection/baseline sequence，source epoch/generation 不变。未证明则 SourceInstallPlan，不得把 no-byte 或 owner 身份当豁免 |
| git_facts / pin / ref_cas | repo capability + owned refs + expected oid → GitFacts/receipt；Git 配置/网络政策仍由 kernel 决定 |
| launcher / environment | handle + 准入 permit → LaunchHandle / typed provider 环境；每次执行仍核销 fence |
| content / sync / reclaim | read/list/status/diff/授权 write；IdlePermit 刷新；DestroyPlan 回收/恢复/最终删除 owned storage，不连带删 branch |

错误区分 Unsupported、Busy、Unavailable、StaleGeneration、InvalidPath、WorkspaceRecoveryRequired。Backend 有意闭合（Worktree/LinuxCowView）；名称对应实际机制，已发布 descriptor 不重新解释。新增 variant 改 enum/decoder/持久化版本、registry、声明与共享合同，kernel 不增实现分支。APFS 另立任务。

### 4.3 崩溃边界与恢复

外部写入前持久化 intent，结果可探测并绑定 op id；DB 不跨慢 FS/网络。rename 限同一 owned 文件系统，文件/父目录按耐久合同 fsync。

| 操作/写入边界 | 崩溃后的唯一恢复动作与线性化点 |
|---|---|
| selection DB → materialize intent DB | 未提交无动作；提交后按同一 selection/op id ensure，不能重新选 backend |
| 选 B → Git 保护 ref CAS → Ready DB | 先持久化所选 oid/预期 ref；保护 ref 存在才可 Ready。写 ref 后崩溃按 input/实际 branch 校验并补 receipt；保护前对象丢失显式失败，不能换 B；view 未 Ready 不开放 |
| 创建 FS/staging → rename → receipt DB | owned marker/storage identity 绑定 op id，逐项探测；完成后补 receipt，半成品只清理本 op 所有资源，不覆盖用户目录；Ready CAS 是可使用的提交点 |
| source-effect intent → 私有 metadata 或真正 install → receipt/DB | ProvenSameSourceProjection 按预期 metadata 序列幂等重驱，不改 source epoch、不等待同步调用者自 drain。SourceInstallPlan 先固定双闭包并完成 Accepted/旧调用结束与 scope drain，再安装/epoch CAS；中断按同一 manifest/intent 恢复，未结束调用保持 Busy |
| run object → run ref pin → run receipt | OID/元数据先入 intent；pin 已为目标补 receipt，否则按 expected CAS；不推进 delivery tip，同 attempt 多 run 不串成父链 |
| kernel repo candidate + delivery pin CAS → classified effect → settlement DB | 多 ref 全旧重试、全目标补齐，不从用户 branch/工作 HEAD 猜父。私有展示投影可 stale；按 sealed plan 补 metadata/baseline 或 source-install receipt，再原子推进账本与 candidate/settlement，不造第二 C |
| catch-up intent → 固定 U 双闭包 → classified effect → U/lease DB | 冻结 H/U/M/T，先完成必要调用交接/drain，再私有 ref H→U、按工作字节 manifest 安装；ref 为 U 仅补原 install/receipt/lease，不 refetch/重选 T，不依赖 smudge 重驱 |
| archive intent → durable Git + raw manifest pins → receipt DB | 私有层外同一 content store 验证/fsync 两类闭包及 pins，补完整 ContentReceipt/ArchiveReceipt 才允许 destroy；pointer-only 或 filter/supply 缺失不能 Ready，失败保留私有层 |
| tombstone/reclaim intent DB → .trash rename/unmount/remove → receipt DB | 先保留完整 descriptor/refs/截止时间再删领域行；失败重驱。area 事务回滚按原 recycle report 补偿，不恢复到已占用路径；保留 restoration gate |
| 到期 intent DB → refs CAS 释放/FS 删除 → final DB | 每项检查引用与静默证明；缺失已属于预期删除可补 receipt，未知 identity/外部引用则延期。旧 GC 不能越过账本释放资源 |

generation 在 holder 重建/恢复时单调增加；source install_epoch 仅随真正 SourceInstallPlan 变化，证明同源 metadata 效果只推进 projection/baseline sequence。PID 不作身份，未知进程/未决调用保持 Busy/recovery。所有可恢复源码由独立 durable store 的规范化与 raw manifest refs 保护，最后内容引用释放后才可 GC。

## 5. S-D：一个生命周期状态机

### 5.1 静默、启动与刷新

分层复用现有事实：hibernation 保持 Planner session 静默语义，不因 PTY/Readers 新增睡眠阻断。workspace 全静默供真正 SourceInstallPlan、sync/close/reclaim 使用；ProvenSameSourceProjection 仅需既有 owner/reservation/稳定性/CAS，不把等结果的 provider 伪装为 Drained。全静默组合现有 session 静默 ∧ occupancy ∧ 进程证据：occupancy 必须 Free（Readers 也阻塞）；旧执行 claim 由领域 owner 显式 suspend/结算，transition intent 保持调度排他，不能按 caller 身份忽略未完租约，无 starting/running/turn_pending session、未决 launch ticket、gate/forge/内容快照读取、远端工具调用；proc-supervisor 确认 PTY 和整棵后代退出。terminal shell 等输入、dev server、setsid 后代、未知 terminal/daemon、stuck owner 残留均阻塞。idle session 仅在无执行/工具且调用准入关闭后才可通过。

remote environment 从开放工具准入到 drain 完成一直持运行许可；若协议不能证明工具/后台后代已 drain，就持续 Busy，不由 DB idle 猜空闲。排他准备窗口由当前 owner 持有，不能以 except_attempt 排除其他工作。claim 前刷新，或 owner 在该窗口执行；并发 reader 不可忽略。

锁序沿用 card start fence（含 per_card_lock）→ operation drive → track lifecycle fence → resume replay；短 BEGIN IMMEDIATE 在所需 fence 后获取，不在事务内等待 card/drive/进程 drain。多 card 按稳定 id 顺序获取，后台维护不能持 track 锁再回取 card。检查、准入关闭、登记与安装受同一 track fence 串行化；耗时动作依 durable intent 重驱，不靠长 DB 锁。

空闲点无条件 sync，worktree no-op；view 保留 W，复核来源身份/HEAD 并刷新 mediator 的来源观察，不依赖 inotify 指纹，不重挂可变 lower overlay。常驻 Claude shell、terminal、Codex viewer PTY 可以无限期推迟，明确显示阻塞进程；不承诺两轮间必刷新，不强杀。来源 HEAD 变化在下一次可静默点提示并暂停新启动，用户确认继续只更新 observed HEAD，不改 B；交互式 PTY 活跃期间不承诺暂停下一条命令。重建先归档再新 track。

### 5.2 状态与现有机制的替换点

状态为 Active → Quiescing(reason) → ClosedRetained / DeletePending → Reclaimed → Expired；另有 WorkspaceRecoveryRequired。关闭先停止新准入，未静默显示 snapshot_pending；成功 snapshot 才进入可回收状态。显式唤醒 ClosedRetained 是授权的状态转换：取消待执行旧回收 intent、恢复/验证资源、增加代次再开放准入；不是绕过 closing 的 spawn。领域 track 仍关闭时，显式活动结束后再次 Quiescing（沿用现有 session 睡眠触发），重新 snapshot 并冻结本次截止时间；PTY 仅推迟资源静默，不阻止 Planner 睡眠。

复用 workspace_recycle 的 managed .trash、owner marker、恢复闸门和 area saga 补偿；managed destroy 的第一阶段就是 recycle，不另建一个 trash service。worktree_clean 的 ignored 清理由同一状态机调度；S1 保持旧 10 分钟行为，S3 切换到新归档/保留政策。track/area delete、Admin close、hibernation 后的资源回收、planner recovery 和旧 worktree sweep 全部接入这个入口；新政策启用时旧 timer/GC 对账本资源停止独立决策，不能并行双删。

私有层保留 **7 天**，关闭源码保留 **90 天**，为 typed settings 且每次 close 冻结。回收前必须在私有层/.git/trash 之外的资源 scoped KernelArtifact repo 导入、验证/fsync 完整 Git history 与所有必需 raw 工作字节 manifest 闭包，CAS pins 并提交覆盖当前引用版本的 ContentReceipt；不依赖 alternates/硬链/缺失祖先/私有 filter cache。第 8 天删整个 managed .git 后，reopen 从 A 冻结 bytes/mode/link 直接安装，无 smudge/LFS/网络；逻辑 D 保账本 tip，源码 W 保 A。缺闭包/到期拒绝，不造空目录或透传 pointer。同一 lifecycle driver 拥有 import/reclaim/reopen，子进程只获授权 export，不见其他 workspace/Area 对象。

删除先保存独立 tombstone 和逐资源作业再删 track/area；恢复 area 时同时恢复其 resource/archive 引用和恢复闸门。单项失败不吞掉兄弟项。用户来源路径永不在 DestroyPlan 中。

初版拒绝 view 父 track 创建子 track（selection 前拒绝，重驱同样拒绝），不产生 attached None 子行。以后若支持共享父视图或派生视图，需独立定义 lease/证据关系；不能沿用当前复制 path 的实现。

## 6. S-E：可独立合并的切片

依赖 **S1 → S2a → S3 → S2b → S4 → S5**：证据/生命周期先于 view，只有合同齐备入口可用。以下为未来验收，非本次测试。

公共实现 gate G：kernel gate 各跑一次 text ratchet、contract、`scripts/local-rust-gates.sh --quick --base origin/main`，加一次聚焦 nextest replay 并拒绝空选择；worker 跑聚焦测试/formatter 修复，CI 才跑广套件。命令模板 `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p <package> <filter> --test-threads 8`。新增合同过滤器在实现时注册，不把未存在的名字写成已验证；只有实际需要才加 codex-e2e feature。真实模型 E2E 不在共享生产主机跑。

| 切片/结果 | 模块与不变量 | 聚焦验证、gate、等级/依赖 |
|---|---|---|
| S1：现有 worktree/managed 机制封装，只有真实生产调用者 | 新 workspace crate/service；track_worktree、materialize、clean、workspace_recycle 与 area/recovery 调用链迁入机制入口。cwd、选择时点、refusal、branch config、wire/事件、7 天 trash 与 10 分钟 clean 均不变；无新归档/retention/schema 闲置机制 | 现有 track_worktree、回收/area 补偿和恢复 tests；对生产 ensure/delete/recovery 路径作前后等价验证。G；L2（回收所有权），无依赖 |
| S2a：只有 worktree 的类型化收口 + WorkspaceRef 迁移 | §1 全入口、truth 新 migration/sidecar binding、lease 索引、session/control/supervisor、FE track 文件端口。按持久身份/句柄、supervisor、FS/FE 三组收口，建立三类能力、票据/握手和资源账本；不预加 namespace fd。此片开始为实际 materialization 写 B pin（显式新增 Git ref 副作用），不前移 ensure 时点；无 view 可选 | 旧行与事件字节、create→ensure 来源变化、已有 branch 的失败重试、repoint/today、持久化重驱、PTY pipe/attach/restart/ACP/forge/gate/FS 生产入口。G；L2，依赖 S1 |
| S3：worktree 的统一 Revision/Tree、交付与生命周期 | 依 [delivery protocol](workspace-delivery-protocol.md) 与 [verification decision](workspace-verification-decision.md)，本次 parent 最小映射待第三轮 fresh 双路完整 L2 设计评审。统一 content/effect owner、raw manifest + normalized closure、kernel 私有 pins/投影、精确 OID publish、Free 转换与 upstream receipts、受控同 cwd 验证/ignored 热缓存、唯一生命周期 7/90；same-source metadata 保 source epoch，真 install 走 Accepted/full drain/new execution，不改 S1 | 多 run/P、同步 gate/commit 不自等待、raw/filter/source/规则身份、每个 ref/FS/DB/crash、整 .git 删除后 raw 恢复、PR/area rollback；G + 机制/崩溃 replay，未支持不得 skip 算通过；L2，依赖 S2a |
| S2b：Linux 同路径 COW 视图，创建与关闭/删除/恢复同片 | 依 [view backend protocol](workspace-view-backend.md)：直接 COW mediator、D/W、sandbox、holder 与 S2a namespace envelope 扩展；共享 S3 FUSE/分类/双闭包，不挂活 lower overlay。同片承担显式 view 选择的 API/wire/OpenAPI 字段及真实生成器产物。已过机制合同的 terminal、gate、受控 forge/FS 可用；Planner/conversation/worker 另须 provider ViewSupport（§3.4）。无 Supported Planner 时只开放显式 API/terminal 路径，两 track 生产链隔离用无模型测试，不宣称 Planner 端到端可用；子 track 拒绝 | 两 track 隔离、来源原地/原子替换与并发句柄、first-write overlap、whiteout/opaque/rename/hardlink group、LOOKUP/OPEN/FORGET 与 mmap/cache、RuleView/规则漂移/重分类/容量、后台后代/uid、holder 崩溃恢复、旧 ticket 竞态、外部 gitdir 拒绝、gate 冻结 C、create 后立即 close/delete、7/90 到期。G + 实际 FUSE/userns 的真实 Linux 机制合同；不能 skip 当通过。L2，依赖 S3 |
| S4：provider 受控执行环境接入 | 按 [实施协议](workspace-provider-environments.md) 建 provider runtime scope、独立 state/授权认证输入和精确执行控制；Planner/conversation/worker 的 start/turn/resume/recovery 均接入。Codex 整个 app-server 进入 view 并用官方本地工具；所有宿主项目读取受 §3.4 合同约束，未证明不开放 | 实际部署 binary 官方 schema、无模型生产编码/真实 Linux 服务合同；断连/重注册、epoch/binding/旧执行控制拒绝、服务与后台 helper drain、共同授权及独立 state。G + profile 协议/平台资格；L2，依赖 S2b |
| S5：完整产品预览/设置与推荐默认 | 产品设置及相关 types/OpenAPI、wire/zod、FE domain/ports/events、创建/恢复/保留期 UI。view 选择字段/生成器属 S2b；S2a 已封住绝对路径，S2b 已提供 view 预览；此片完善体验，能力齐备才推荐 Linux view，设置不换冻结 backend | 真生成器、schema/事件兼容、Report/image/raw/HTML、路径越界、白洞 404、设置和恢复浏览器测试；G + FE lint/build/test、真实浏览器预览与相关 browser E2E，遵循 development-preview。L2，依赖 S4 |

实施时须定（仅归属与约束，不在本文展开）：

- S2a / NB3-6、NB3-8、NB3-9：resolve 增 principal/purpose；按 backend 标明 QuiescenceEvidence 覆盖范围，定 supervisor 核销/撤销与 fork 线性化；删除/私有化 `agent_cwd()`，`DisplayPath` 不实现 `AsRef<Path>`，定 view 四列；扫描用 clippy disallowed-methods（Command::current_dir、std::fs）与逐处 `#[expect(reason)]`，不建文件名单。
- S3：以上旧实施待定项由 delivery/verification 两协议及唯一 sealed effect owner 统一收口，待第三轮 fresh 双路完整评审；不维护工具名/caller/test 名单。
- S2b / NB3-2、NB3-3：依 view backend protocol 统一 mediator、S3 RuleView/原子规则版本与 D/W manifest；same-source proof 不得因清 W 暴露不同来源或分类，必要时走真正 SourceInstallPlan；owned source/cache 重分类须显式处置。
- S2b / NB3-7：view backend protocol 定显式 no-provider 创建与 Unsupported Planner 拒绝、kernel 按 Revision/授权确认 overlap；无 Supported provider 时交付链无生产 provider 调用者是有意范围。

关键 invariant 的 mutation 只在独占可恢复 worktree 完成：移除 generation/ticket 校验、让 gate 读 live lower、archive 失败仍回收等，单次只改一个生产因素，先预测完整红集再跑整个相关 target/filter，安全恢复并重绿。本轮只执行 doc-exists 与 ratchet gate。

## 7. 架构五点与维护成本

- 抽象边界：kernel 决策、truth 事务、workspace 机制、provider 外部编码；共享 Revision/Tree 和能力阻止 adapter 猜实现。验证以证据不变量为准，worktree 用同逻辑 cwd 的冻结源码/动态 ignored 投影；采样不冒充无写入证明。
- 重复逻辑：只保留 checkout admission、分层组合的静默事实与一台资源生命周期状态机；复用 recycle/hibernation/sandbox，旧清理入口不旁挂第二套政策。
- 硬编码假设：backend 有意闭合，ref 命名是 Neige 自有协议，7/90 为 typed settings；不按模板、插件或路径字符串决定执行隔离。
- 外部事实：Git 路径由 Git 返回；进程身份由自有 nonce 与监督事实证明；Codex 只认部署版本协议。实验记录不足以解除发布阻断。
- 永久成本：新启动/文件调用只用既有能力，不加名单；backend 扩展改 §4.2 owner/合同。增改测试不改扫描名单/阈值；G text/contract/quick 各一次，focused worker + gate 各一次（有意 replay），CI 广套件按现有 workflow。平台/协议合同仅对应实现/升级运行；Facts 不转成永久检查。

## 8. Known gaps 与发布阻断

| 阻断 / 初版范围 | 解除片与准入条件 |
|---|---|
| 活来源的 rename/delete/切分支/git gc、mmap/cache、复杂构建及 tracked 一致性 | S2b 直接 mediator 的真实 Linux/FUSE 合同通过才开放对应宿主，不用可变 lower overlay 压测替代；不能 skip 算通过 |
| 本地 provider 按 cwd 状态/项目读取 | S2b provider 层 ViewSupport 合同；Claude 依官方状态作用域，ACP 默认拒绝；未通过仍 Unsupported，不猜 HOME 私有布局 |
| Codex 宿主项目文档/config/git info/ghost/undo/搜索、trust 状态 | S4 整个服务 scope、独立 state/认证输入、部署版本协议和平台合同；部分 namespace 元数据实验不能放行，未过拒绝 Codex view |
| 转换仓库、非 Git 交付、子 track、根外 cwd | S2b 在创建/准备和重驱边界拒绝（转换在 snapshot 重检）；解除另立扩展片，非 Linux 继续 worktree |
| gate 写源码、ignored 环境/依赖缓存 | S3 的冻结 raw 源码 + 按 Git 分类受控缓存投影；防实际源码写而非审计所有失败 syscall；S2b 消费同一冻结 C 而非活动来源。无完整机制时拒绝 bound，不新增 GateSpec/outputdir |
| 常驻 PTY/远端工具无法 drain、活动轮 HEAD 检测 | S3 资源静默与 S2b/S4 执行器证明；未 drain 持续 Busy、用户显式停止，不强杀，不承诺活动轮即时检测 |

这是实现发布门槛，不是本次已实现/验证的能力；网络/端口和远端副作用仍遵循既有权限边界。

## 9. 第一轮评审处置（保留索引）

A/B 指 design-review-a/b；“已修”为设计处置，非代码已实现。

| id | 处置 / 位置 |
|---|---|
| A:R1、B:B2、B:N4 | §1/§3 全调用链、三类能力、类型化启动、既有 read/raw 复用 |
| A:R2、B:B1、B:B7、B:N1 | §2 共享源码证据/父链/验证；worktree 规范化，view 转换仓库拒绝；能力布尔仅展示 |
| A:R3、A:R4、A:R5、B:B3、A:N1 | §4 机制合同、独立资源账本、现有 operations、selection/materialization、闭合 enum 成本 |
| B:B4、B:B5、B:N3、B:N5、B:N8、A:N2 | §3.2/§5 ticket、drain/静默、单生命周期、7/90、PR 引用保护；view 子 track 拒绝 |
| B:B6、B:N6 | §6 S1 无闲置机制，view 创建/回收同片 |
| B:N2、B:N7 | §3.4/§8 sandbox 复用、provider 隔离门槛 |

## 10. 第二、三、四轮评审处置

A 指 design-review-a2，N-B*/NB-* 指 design-review-b2。关键证据成立：worker.rs:236–241,490–505、task_prompt.rs:125–147、forge_git.rs:27,79–87,108–115；GateSpec/采样见 gate_lifecycle.rs:32–44、target.rs:433,456；operations 见 migration 0099:8–38，Claude/ACP 见 config.rs:28、spawn.rs:305 / process.rs:33–37（路径前缀见 §1）。A2 的可实施结论不消除 B2 反例。需限定：N-B3 是未来合同缺陷，当前没有这些 epoch 字段；N-B2 首尾采样不单独证明从未写入，须结合准入/写入约束。未发现应整项驳回的证据。第三轮 A3/B3 指 design-review-a3/b3，第四轮 A4/B4 指 design-review-a4/b4；对照 forge_git、worker、Services、gate 采样与 reopen 核实；B3-1 结构反例成立，L2 正式复审由 Planner 安排。

| id | 处置 / 位置 |
|---|---|
| N-B1、N-B2 | 第三轮 B3-1 收口至 §2.3/§4.3：账本唯一权威，自有 ref 仅 pin/intent，P 取已结算 tip（catch-up U），分支 stale 不阻塞准入；Planner 准入/同路结算，保留 agent replay；§2.2/§6 S3：证据不变量；worktree 原地 checkpoint/采样，view 钉 lower；无 GateSpec 新字段，不破坏 ignored 构建 |
| N-B3、N-B4 | §3.1–3.2：持久 WorkspaceRef 仅 workspace_id/cwd；源码证明另存 epoch/Revision；唯一 resolve，generation 仅运行句柄/票据/环境；§3.4/§6 S2b/§8：provider 拥有 ViewSupport 与合同；明确可用非 provider 入口及无 Supported Planner 的范围，每项阻断有解除片 |
| NB-1、NB-2 | §4/§5：复用 operations、删除子账本；睡眠/资源静默分层，唤醒活动后重新关闭 |
| NB-3、NB-4 | §3：闸门覆盖封装原语；ExternalDir 独立且 gate unbound，view 拒绝根外路径 |
| NB-5、NB-6 | §6：S2a 收口/B pin；S2b namespace、view API/生成器，S5 产品完善 |
| NB-7、NB-8 | §2.3/§4.3：最终 candidate pin 与自有 delivery ref 同一 update-ref 事务，分支投影 best-effort，HEAD/index→DB 恢复；§2.2：normalization 记录 ignore/exclude 来源摘要，私有 info/exclude 与 snapshot 一致 |
| A:N1、A:N2 | §2.3/§4.3/§6：run pin 与 delivery CAS 分离，完整候选 intent/receipt；连续 run/修复重跑及崩溃合同；§1/§3.3：复用已有 track read/raw，替换内部 opener，补 list/status/diff 与 FE 迁移 |
| 第三轮 B3-1（B:B1 → N-B1） | §2.3/§4.3/§6 S3：删除双权威定义，单一已结算 tip、detached 执行投影与 kernel 修复；Planner 经 Services/checkout_admission，无插件名特判 |
| 第三轮 NB3-1、NB3-4 | §2.3/§4.3：唯一 C 构造、catch-up 崩溃行；S3 明列 hooks/签名行为变化与恢复验收 |
| 第三轮 NB3-2、NB3-3、NB3-5、NB3-6、NB3-7、NB3-8、NB3-9、A3:N1/N2 | §6 各切片“实施时须定”；采纳 NB3-8 类型约束、NB3-9 clippy/逐处 expect 与存量转换；不扩大本轮设计 |
| 第四轮 B4-1、B4:N6 | §2.3/§4.3/§6 S3：账本唯一权威；HEAD/index 永不漂移，no-deref 写 HEAD；分支偏离/被 checkout 只 stale，自有 ref 无法对账才 DeliveryRecoveryRequired；与 WorkspaceRecoveryRequired 分名/分域，kernel 自动或 Planner 确认修复 |
| 第四轮 B4:N1 | §2.3/§4.1/§6 S3：补 upstream 全链清扫，receipt 与 detach 同片切换，验收 detach 后 catch-up/fetch |
| 第四轮 B4:N2–N5、N7、N8、A4:N1/N2 | §6 S3 各行实施时须定，开工前单独评审交付协议实施设计；S1–S2a 定稿 |
