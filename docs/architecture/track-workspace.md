# Track Workspace 框架

状态：第四轮 L2 评审后修订（第五轮文档），待分片实现。承接 [track-cow-view.md](track-cow-view.md) 的实验；设计冲突以本文为准。英文类型为合同草案，不表示已有 API。方向保持 Workspace 框架 + git worktree / Linux 同路径 overlay 视图两种实现。

## Outcome 与初版边界

kernel 统一拥有选择、授权、交付、恢复和回收政策；workspace 实现只做文件系统与进程机制。Linux attached track 最终推荐视图：同 track 进程共享私有写入，能读取来源 checkout 的 dirty/ignored 文件，仓库内写入不落到用户 checkout。现存 track 不自动切换 backend；不引入 Lazy 升级、用户 checkout 备份或自研 Codex exec 协议。

初版 opt-in，默认 worktree；仅开放通过合同的 Linux Git 入口，创建与关闭/删除/恢复、证据和文件 API 同片。未完成入口拒绝，不回落来源目录；推荐默认、非 Git 交付和非 Linux 新实现后置。仓库外写入仍由 provider 沙箱约束，网络/端口不隔离，外部副作用仍走 #2617 批准。

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

已有 `crates/sandbox/src/lib.rs` 是 typed bwrap policy（最小宿主挂载、进程 namespace、环境配置），desktop 在用；不是可接管的常驻 overlay holder。`external/codex` 不在 checkout，历史行号不能证明部署版本 experimental 协议或宿主项目读取行为。

## 2. S-A：一条源码证据路径

### 2.1 Revision、Tree 与运行视图

分开三个对象：活动视图是随 lower 变化的读环境；私有写入记录是 agent 相对已安装源码的修改；`Revision` 是持有内容的不可变交付证据。只给 upper 计数不构成源码证据。

`Revision { workspace_id, install_epoch, content_digest, tree_oid, changes, overlap, normalization }` 由框架 snapshot 产生，绑定稳定内容句柄和受保护 Git tree。changes 是相对当前 delivery baseline 的源码差异，含 tracked（即使被 ignore）、新增非 ignored、删除、mode/symlink；不含内部 Git 元数据和新增 ignored 产物。不支持类型显式拒绝。已 commit 且 status clean 仍比较完整 tree，不从 status 猜变化。非 Git 用显式 DirectoryRevision，初版不进入 candidate/gate Git 合同。

创建基线 B 永久不变；当前安装基线 D 初始为 B，catch-up 准备后为 U 的 tree，agent replay 的结果属于 W。视图将 kernel 安装层 D 与 agent 写入层 W 分开记录（独立安装清单/代次，不能把整个 upper 当 W）；snapshot 合成 D ⊕ W。未触碰的活动 lower 脏源码不自动进入交付。copy-up 可能含用户改动时标 overlap；归属无法证明也保守标记，确认绑定 revision，未经确认拒绝交付。snapshot 期间来源变化则重试或返回 Busy，不能发布撕裂内容。

worktree 同样返回 Revision/GitFacts，只在实现内解析真实 checkout。kernel 的 lease、gate、只读 head pin、publish、catch-up、archive 只消费这组证据，不按 same_path/live_source 分支。Capabilities 布尔只做展示；动作以类型化 Result/Unsupported 和准备好的 plan 为准。

### 2.2 被测树 = 交付树

`prepare_verification(revision, candidate)` 返回证据句柄：开始/结束观察到的 tracked tree 都等于 candidate tree，期间无 install_epoch 变化和源码写入；新增非 ignored 文件同样使证据失效。gate 结果绑定 `(revision, tree_oid, candidate_oid, environment_digest)`，缺证据不能 bound。此为不变量，不规定统一物化方式，不新增 GateSpec 字段。

worktree 保留原地验证，以 §2.3 统一构造的 C 作 checkpoint，复用 HEAD/status/provenance 采样和排他准入，保留仓库内 ignored 构建产物、热缓存及现有 cwd/用户步骤语义；不要求只读挂载或独立输出目录。view 则从 candidate tree 钉住验证 lower、使用私有 Git 元数据，ignored 写入进验证 upper，不遮蔽交付源码。`a` 在活动 lower 改新而 agent 只改 `b` 时，验证和交付仍读 B 中的 `a`；活动 lower 更新不影响验证。源码写入型步骤不能获 bound 结果，回工作视图修改后重跑；不把这个约束扩大为拒绝既有 ignored 输出。只读 head pin 复用同一证据合同：worktree 原地核验 head，view 钉住 head tree，不强制为所有 reader 新建验证环境。

Git 内容规范化属于 snapshot：worktree 保留现有 Git clean/smudge、eol、fileMode 语义，在 launcher 内执行可能运行仓库代码的转换，记录规则和规范化 tree；验证使用同一规则。normalization 同时记录 ignored 分类的规则来源/摘要（仓库 ignore、有效 Git exclude 配置、kernel `.neige` 排除项）；view 私有 info/exclude 安装同一规则，snapshot 与 agent Git 观察一致，tracked 文件不因 ignore 消失。宿主可信写入器只接收已规范化 blob/tree，禁 hooks/filter/fsmonitor，不能用“禁 filter”代替规范化。初版 view 对 custom clean/smudge、LFS、working-tree-encoding、eol/autocrlf 等转换仓库明确 Unsupported（创建及 snapshot 重检 attributes/config）；不能把指针或工作树原字节静默当交付 blob。放开必须新增正反向物化等价合同。

### 2.3 attempt、交付与 Git facts

交付 tip 的唯一权威是资源账本；Neige 自有 `refs/neige/…` delivery ref 仅作账本 tip 的 GC pin 与 Git 侧 intent，不承担权威语义。`refs/heads/neige/track-<id>` 是 best-effort publish 投影，工作 HEAD 是执行投影，二者都不能决定父链。worktree 正常形态为 detached HEAD（view 私有 HEAD 同义）；kernel 可安装已知 run 的 C，但不推进已结算 tip。detached 或 attached 的工作 HEAD 与 index 的任何变化都只是源码输入，只由 Revision 判定，永不算漂移；kernel 写 HEAD 一律 `update-ref --no-deref HEAD`，不移动附着分支。须清扫 forge_git 的 branch/exit 11、catch-up 第 0 步及 verify_worker_checkout/verify_worktree_base 的 attached-branch 检查，改核对资源身份/源码证据，spawn 校验失败仍为 spawn-failed；upstream 的 head_upstream/prepare_fetch/upstream_view/last_known_upstream 及其 scheduler refresh_catch_up_upstream、track_worktree ensure/choose_lease_start、worker catch-up、staleness 调用方一并清扫，S3 与 detach 同片切到账本 upstream receipt。

lease 记录 identity、开始 Revision 与 `LeaseBase` 起点 P：普通 attempt 只取已结算 tip，catch-up 取已结算的 U，不从工作 HEAD 或上个 candidate 推断。dirty 判定是 Revision 相对已结算 tip 的差异，并拒绝未完成 Git 操作/冲突和未确认 overlap，不看工作 HEAD/status 或活动 lower 的 dirty。准备持排他准入，确认 Revision 为空后由 kernel 安装工作 HEAD/index 为 P；私有 commit 的相同 tree 不算 dirty，其历史不进入父链。Planner 私有改动先由 git_commit 结算或撤销。

C 只有一种构造方式：launcher 内 snapshot → 规范化 tree → 可信 plumbing 提交（父为冻结 P）→ pin → 安装工作 HEAD/index；gated checkpoint、ungated 和 Planner git_commit 共用，不走 git commit/hooks/签名，agent 私有历史只是源码输入。同 attempt 各 run 固定 P，未交付 run 不改变 GitFacts.delivery_tip。Planner git_commit 经 gitforge Services 的类型化方法进入 kernel（参照 builtin_plugins/dev/publish.rs 的 Services），先走 checkout_admission，非 Free 返回 Busy，Free 时登记排他 owner 并持有至结算；forge 适配器不得按插件/动作名截获。Planner 提交推进已结算 tip，publish 仍区分 Planner commit 与 done candidate。

完整候选 OID/父/tree/message、预期 refs 与投影 receipt 先入现有 operation intent；最终 candidate pin + 自有 delivery ref P→C 用一次 update-ref 事务，随后 best-effort 更新分支投影、install_delivered(C)，最后以现有 settlement/candidate 事务推进账本 tip。结算全程持写 fence，幂等键为 op id + revision + P，acceptance 独立。漂移仅两类：分支投影与 receipt 不符，或被任何 worktree checkout（以 `git worktree list --porcelain` 为准），则 pin 偏离 OID、跳过投影写入、标 stale，publish 拒绝并说明，不停止准入或阻断结算；自有 ref 与账本/持久 intent 无法对账才进入 `DeliveryRecoveryRequired`，仅阻塞写准入与结算，不影响 resolve 或 Planner 启动，区别于生命周期 `WorkspaceRecoveryRequired`。修复是 S3 kernel op：无损时自动执行，需确认时向 Planner 提供工具/API；保全未知对象与源码，按账本/原 intent 恢复 pin 并重驱原 settlement，不重造 C，已 done attempt 可继续结算，不收编偏离 tip。

`GitFacts` 返回 repo identity、受保护基线、已结算 delivery tip、固定 upstream、候选与 ancestry/refs receipt；publish check_tip 核验权威链及投影一致性，不靠 worktree Some/用户当前分支推断。catch-up 保留 agent 回放：T 为最新 done candidate，fetch 得 U，H 只取已结算 tip，冻结 M/T/H/U 后由排他 kernel CAS 自有 ref H→U、best-effort 更新分支投影并安装 U，账本结算 U 后发布 P=U 的 lease；D 逻辑重定到 U，清源码 W、保留 ignored，再由 agent 回放 M..T 到 W/解冲突，C 父为 U。kernel 不自动 replay、不 reset 用户 checkout；live sync 只刷新 lower。

归档也消费 Revision，archive commit 父为已结算 tip，必要时保留上次 archive 保护链；不推进已结算 tip。保留期清理释放自有 archive/baseline refs，不删除被 candidate、未结算操作或打开 PR 引用的分支。PR 状态未知则保留分支并显示延期；branch 删除是独立的引用释放条件，不能随私有目录 destroy 隐式发生。

## 3. S-B：类型化启动与文件能力

### 3.1 所有权与持久化身份

新增 `workspace` crate，不加 calm- 前缀。它拥有路径解析、Backend、句柄/代次、launch 与内容；`calm-truth` 拥有事务/冻结/CAS；kernel workspace service 拥有授权、选择、生命周期、Git 交付政策并复用 operations；provider 拥有外部协议与 ViewSupport 资格声明。实现不读 tasks/approval，不判断模板/插件，不决定保留期或准入。复用 checkout_admission，不另造并发任务政策。

持久身份只存 `WorkspaceRef { workspace_id, cwd: WorkspaceCwd(RelativePath) }`，track 归属由领域记录关联，不把运行代次写进启动身份。需证明源码身份的 lease、gate target、冻结 forge/checkpoint/交付 intent 另存 install_epoch/Revision；重驱核对它们，不能重绑成当前源码。覆盖 lease、terminal、operation input/tx_output、FrozenForge、child output、session/handle、task.cwd、gate.cwd 和恢复/重启记录。writer 唯一索引与 supersede 以 workspace_id 为键；跨 track 不共享 workspace 的 view 不因 same_path 冲突，legacy 同一真实 checkout 仍映射同一锁域。

新增 migration，不改旧 migration/事件字节/hash。旧 cwd 只由显式 LegacyWorktree decoder 核对原身份和根后转换；未能核实拒绝恢复。历史事件不回填；已冻结旧 operation 的 payload 不原地改写，新增 sidecar binding 由 op id CAS 保存并供重驱消费。新 operation 必须用新类型；不能通用“缺身份就读 cwd”fallback。显式绝对 cwd 在 API 边界验证后转换为根内相对路径；旧 worktree 根外显式 cwd 使用独立类型化 `ExternalDir` 启动目标，无 workspace 账本/生命周期/源码证据；根外 gate.cwd 明确 unbound。保留既有行为，不伪装为 workspace。view 初版拒绝根外 cwd，不能借 ExternalDir 绕开来源保护。

### 3.2 LaunchHandle 与运行期闸门

唯一解析入口 `resolve(principal, purpose, WorkspaceRef) → Ready(LaunchHandle) / WorkspaceRecoveryRequired / Unsupported(reason)` 先核对资源、授权和恢复状态，再生成当前代次句柄；恢复只解析身份，源码证据仍须单独匹配。generation 只进入 LaunchHandle、单次 ticket 和 environment id（资源运行状态保留计数），不进入持久启动身份或 Revision。track spawn 只接受不可伪造的 LaunchHandle，宿主 cwd 构造器在 crate 内私有。gate shell/采样/checkpoint、forge action/probe、Claude session/restart/rewind/readiness、ACP、普通/任务 terminal、child/system/today 都经它。展示路径不是启动权限；扫描发现回归，不证明隔离或维护文件名单。

每次 spawn（包括重复调用旧 handle）在同一 lifecycle fence 下原子校验 generation、install_epoch、closing/admission 状态与 permit，登记 launch intent，再将可消费的单次 launch ticket 交给执行器。supervisor 在 fork 前核销 ticket；关闭/恢复必须等未决 ticket 被确认或撤销，不能检查后释放 fence 让迟到的 fork 穿过关闭。失败/未知启动不能当空闲。被封装的宿主 spawn/read/write 原语若命中已登记的 view 来源根却没有相应内容/管理能力，运行期拒绝；裸 std::fs/Command 只能靠评审/扫描发现，此闸门不声称覆盖任意宿主代码或单独证明隔离。使用目录身份/受控 fd 防 symlink 和路径换名竞态，不仅比较字符串前缀。

PTY 由 calm-proc-supervisor 实际 fork：扩展 calm-session 协议的版本握手与 launch envelope，传受控 namespace fd/身份、uid 映射、workspace generation 和 ticket；supervisor 先进入视图再检查逻辑 cwd、fork，用真实 uid/gid。kernel 内的类型限制由序列化能力票据延续到这个进程，不能发送任意 PID/ns 路径。旧 supervisor 不懂 envelope 时，拒绝 view；升级先 drain，不能为了升级杀掉未知旧 PTY。worktree 旧协议只在显式协商的 legacy 模式保留，不能服务 view。

### 3.3 三类宿主文件能力

| 能力 | 授权范围和消费者 |
|---|---|
| WorkspaceContent | 通过 workspace handle 的 read/list/status/diff/write/verification；工作区文件、Report 相对链接、HTML/image/raw 预览、provider 项目配置和附件均在此类。保留附件 managed-only 准入，既有 `.neige/attachments` 也由受控 write/open 访问，不新增宿主直写通道 |
| KernelArtifact | kernel 自有日志、operation/terminal 产物、runtime identity 与资源账本 storage；不接受工作区路径，不将 artifact 路径当用户 checkout。共享 app-server/插件宿主自身启动用此服务能力，不能据此执行 track 仓库脚本 |
| SourceGitAdmin | 只允许框架提供的 repo identity 与自有 ref CAS、对象 pin/写入等固定 plumbing；不接受任意 argv，不执行仓库代码。hooks/filter/credential-sensitive 动作走 launcher 与既有凭据拆分。不能让 forge probe 冒充可信 Git |

复用 `/api/tracks/{track_id}/workspace/readfile{,-raw}`，内部 opener 改用 WorkspaceContent；补齐 track list/status/diff 并迁移 FE 绝对路径消费者，不新建平行 read/raw API。输入相对路径，保留鉴权、越界、symlink、MIME 和 revision 冲突规则。旧绝对路径通用路由对 view 来源根拒绝；不能让 FE 留在旧 agentCwd 规则上。checkpoint refs_digest、lease facts、publish/staleness 都用 GitFacts，不自行开 cwd。

### 3.4 namespace 与 provider

overlay upper/work/runtime 只在 backend 内定位；lower 活动来源只读，`.git` 元数据私有。linked gitdir/common dir/alternates 的路径只取 Git 报告；不能私有化时拒绝，不能写穿真实 common dir。launcher 映射回真实 uid/gid，环境显式 allowlist，继承 provider 的仓库外沙箱。复用 sandbox crate 的 typed 挂载/环境与子树约束；常驻 holder 的 open/recover/join 属 workspace 新机制，因为现有 bwrap policy 是一次性进程沙箱，不另抄一套通用沙箱政策。

Codex 使用共享 app-server + 每个活跃 view 的官方 exec-server；provider 编码 environment/add、thread/start、turn/start 的环境参数。接口来自实验，仅部署版本官方 schema/响应及无模型协议合同能授权启用；当前不猜 RPC 字段、socket 布局或注销方法。environment id 由 kernel 拥有并绑定 generation；注册/readiness 成功才启动，恢复必重注册，断连拒绝，不 fallback 本地工具。

provider 所在层声明类型化 `ViewSupport = Supported / Unsupported(reason)` 并提供合同测试；kernel 只消费声明，不按 provider 身份特判。Supported 必须证明项目读取、配置、Git/ghost/undo/搜索及按 cwd 的状态均隔离或显式禁用，exec 路由本身不足以证明。S2b 负责本地 provider 合同：Claude 的共享 CLAUDE_CONFIG_DIR 不能证明 project 状态隔离，须按官方合同建立独立作用域；未证明前 Unsupported。任意配置 ACP 默认 Unsupported，只有具体适配层通过合同才能声明支持。Codex 由 S4 解除。共享推理/鉴权不变，Codex TUI `!` 不支持。

## 4. S-C：资源账本与两段式持久化

### 4.1 selection 与 materialization

资源账本由 kernel 拥有，生存期独立于 tracks；track/area 删除不能 cascade 它。保存 workspace_id、versioned descriptor、repo/storage identity、selection version、materialization receipt、B 的保护 ref、当前 D/tip/upstream、运行 generation/install_epoch、runtime identity、close 截止时间及 tombstone。账本只保存资源状态；机制操作注册为现有 operations kind，复用其 idempotency_key、payload_hash、tx_output、阶段/probe/补偿，保存不可变 input、expected CAS version 与 FS/ref receipt，不建子账本，也不依赖已删除 track 回查。

selection 是 backend/source/storage，由现有唯一工作区写者与四列一起写，未冻结时 create/repoint/today launchpad 都可更新；与现有 frozen_at 闩一起冻结，不提前到创建事务。system 特例仍只在 kernel，不启用 view。materialization 是 Pending → Ready(receipt) 的显式枚举；baseline/upstream 在 ensure 或 view create 的实际成功点以 NULL→值 CAS 安装，不从创建请求猜值。LegacyWorktree 为显式变体，保留 attached None、managed、child 和旧 cwd/refusal；不凭默认值补虚假 baseline，unborn 是独立类型。

worktree ensure 保留 fetch/choose_lease_start 时点，部分失败后探测已建 branch/实际 base；重试只能记录实际 receipt。S3 切换前 branch config 是 worktree upstream 唯一权威；S3 与 detach 同片验证一致后切换到账本 receipt，后续消费者统一读 GitFacts，不双读 fallback。冻结约束 selection，不阻止首次 materialization 或后续授权 install；恢复不能重选 backend/source。

### 4.2 机制合同

机制返回强类型 receipt；kernel 只传 immutable plan/能力，不拼 upper/mount/gitdir 或调用 backend helper：

| 接口族 | 输入与输出合同 |
|---|---|
| create / open / recover | selection + op id / ledger descriptor + runtime identity；返回句柄、实际 materialization receipt 或 WorkspaceRecoveryRequired。相同请求幂等，不同资源拒绝覆盖 |
| snapshot / prepare_verification | handle + exclusive/read permit → Revision/SnapshotHandle；固定 candidate tree → VerificationHandle/环境证据；内容受保护到所有消费者释放 |
| stage_install / install / inspect_install | 受保护 Revision、expected generation/epoch、意图 → staging/原子安装 receipt；支持 U 安装、archive restore，不靠 create 猜 upper |
| install_delivered | C + expected Revision/epoch → 投影安装 receipt；遵循 §2.3 恢复协议，HEAD/index 设 C 且保留源码；D 逻辑重定到 C，剩余 W = 当前内容 − C，不将正常并发写视为投影漂移，不在挂载期间物理修改 lower/upper |
| git_facts / pin / ref_cas | repo capability + owned refs + expected oid → GitFacts/receipt；Git 配置/网络政策仍由 kernel 决定 |
| launcher / environment | handle + 准入 permit → LaunchHandle / typed provider 环境；每次执行仍核销 fence |
| content / sync / reclaim | read/list/status/diff/授权 write；IdlePermit 刷新；DestroyPlan 回收/恢复/最终删除 owned storage，不连带删 branch |

错误区分 Unsupported、Busy、Unavailable、StaleGeneration、InvalidPath、WorkspaceRecoveryRequired。Backend 有意闭合（Worktree/LinuxOverlay）；新增 variant 改 enum/decoder/持久化版本、registry、声明与共享合同，kernel 不增实现分支。APFS 另立任务。

### 4.3 崩溃边界与恢复

外部写入前持久化 intent，结果可探测并绑定 op id；DB 不跨慢 FS/网络。rename 限同一 owned 文件系统，文件/父目录按耐久合同 fsync。

| 操作/写入边界 | 崩溃后的唯一恢复动作与线性化点 |
|---|---|
| selection DB → materialize intent DB | 未提交无动作；提交后按同一 selection/op id ensure，不能重新选 backend |
| 选 B → Git 保护 ref CAS → Ready DB | 先持久化所选 oid/预期 ref；保护 ref 存在才可 Ready。写 ref 后崩溃按 input/实际 branch 校验并补 receipt；保护前对象丢失显式失败，不能换 B；view 未 Ready 不开放 |
| 创建 FS/staging → rename → receipt DB | owned marker/storage identity 绑定 op id，逐项探测；完成后补 receipt，半成品只清理本 op 所有资源，不覆盖用户目录；Ready CAS 是可使用的提交点 |
| install intent DB → staging+pin → 切换 active manifest → epoch/D DB CAS | fence 禁启动；manifest 包含 op id/epoch/tree，原子切换是磁盘提交点。崩溃后 inspect：旧 manifest 则重做/丢 staging，新 manifest 则补 DB，再开放准入；不能出现旧 D 配新源码 |
| run object → run ref pin → run receipt | OID/元数据先入 intent；pin 已为目标补 receipt，否则按 expected CAS；不推进 delivery tip，同 attempt 多 run 不串成父链 |
| candidate pin + 自有 delivery ref CAS → 投影/install_delivered → settlement/candidate DB | 多 ref 全旧重试、全目标补齐；自有 ref 按 intent 重驱，无法对账才为 DeliveryRecoveryRequired；分支 stale 跳过写入且不阻断结算。按 intent 补 HEAD/index 后补账本 tip 与 settlement/candidate，不从工作 HEAD 猜父、不生成第二 C；结算完成才开写准入，resolve/Planner 不受交付对账阻塞 |
| catch-up intent → 自有 ref H→U → 投影/安装 U → 账本 tip/lease DB | 冻结 H/U/M/T、旧工作 HEAD 与目标 epoch，不重新 fetch/选 T；ref 为 H 重试、为 U 补安装，其他按 §2.3 修复。view inspect manifest，worktree inspect tree/目标并以 read-tree 幂等重做；安装 receipt 后才结算 U/发布 lease，全程 fence，慢 FS 不放 prepare DB 事务内 |
| archive intent → tree/commit+保护 ref → archive receipt DB | 每个 close op 独立 pin；补 DB 前禁止 destroy。重复 close 不释放旧 close 的 pin，失败保留私有层 |
| tombstone/reclaim intent DB → .trash rename/unmount/remove → receipt DB | 先保留完整 descriptor/refs/截止时间再删领域行；失败重驱。area 事务回滚按原 recycle report 补偿，不恢复到已占用路径；保留 restoration gate |
| 到期 intent DB → refs CAS 释放/FS 删除 → final DB | 每项检查引用与静默证明；缺失已属于预期删除可补 receipt，未知 identity/外部引用则延期。旧 GC 不能越过账本释放资源 |

generation 在 holder 重建/恢复时单调增加，旧 handle/environment 失效；install_epoch 单独区分源码安装。PID 不作身份：需 nonce、进程开始时间、namespace 和 supervisor 事实；未知存活进入 WorkspaceRecoveryRequired，不建空目录冒充恢复。B/D、candidate、archive、未完成 op 均有保护 ref；最后引用释放才可交给 Git GC。

## 5. S-D：一个生命周期状态机

### 5.1 静默、启动与刷新

分层复用现有事实：hibernation 保持 Planner session 静默语义，不因 PTY/Readers 新增睡眠阻断。workspace 静默供 sync/install/close/reclaim 使用，组合现有 session 静默 ∧ occupancy ∧ 进程证据：occupancy 必须 Free（Readers 也阻塞），无 starting/running/turn_pending session、未决 launch ticket、gate/forge/内容快照读取、远端工具调用；proc-supervisor 确认 PTY 和整棵后代退出。terminal shell 等输入、dev server、setsid 后代、未知 terminal/daemon、stuck owner 残留均阻塞。idle session 仅在无执行/工具且调用准入关闭后才可通过。

remote environment 从开放工具准入到 drain 完成一直持运行许可；若协议不能证明工具/后台后代已 drain，就持续 Busy，不由 DB idle 猜空闲。排他准备窗口由当前 owner 持有，不能以 except_attempt 排除其他工作。claim 前刷新，或 owner 在该窗口执行；并发 reader 不可忽略。

锁序沿用 card start fence（含 per_card_lock）→ operation drive → track lifecycle fence → resume replay；短 BEGIN IMMEDIATE 在所需 fence 后获取，不在事务内等待 card/drive/进程 drain。多 card 按稳定 id 顺序获取，后台维护不能持 track 锁再回取 card。检查、准入关闭、登记与安装受同一 track fence 串行化；耗时动作依 durable intent 重驱，不靠长 DB 锁。

空闲点无条件 sync，worktree no-op；view 保留 W，刷新 live lower，不依赖 inotify 指纹。常驻 Claude shell、terminal、Codex viewer PTY 可以无限期推迟，明确显示阻塞进程；不承诺两轮间必刷新，不强杀。来源 HEAD 变化在下一次可静默点提示并暂停新启动，用户确认继续只更新 observed HEAD，不改 B；交互式 PTY 活跃期间不承诺暂停下一条命令。重建先归档再新 track。

### 5.2 状态与现有机制的替换点

状态为 Active → Quiescing(reason) → ClosedRetained / DeletePending → Reclaimed → Expired；另有 WorkspaceRecoveryRequired。关闭先停止新准入，未静默显示 snapshot_pending；成功 snapshot 才进入可回收状态。显式唤醒 ClosedRetained 是授权的状态转换：取消待执行旧回收 intent、恢复/验证资源、增加代次再开放准入；不是绕过 closing 的 spawn。领域 track 仍关闭时，显式活动结束后再次 Quiescing（沿用现有 session 睡眠触发），重新 snapshot 并冻结本次截止时间；PTY 仅推迟资源静默，不阻止 Planner 睡眠。

复用 workspace_recycle 的 managed .trash、owner marker、恢复闸门和 area saga 补偿；managed destroy 的第一阶段就是 recycle，不另建一个 trash service。worktree_clean 的 ignored 清理由同一状态机调度；S1 保持旧 10 分钟行为，S3 切换到新归档/保留政策。track/area delete、Admin close、hibernation 后的资源回收、planner recovery 和旧 worktree sweep 全部接入这个入口；新政策启用时旧 timer/GC 对账本资源停止独立决策，不能并行双删。

私有层保留 **7 天**，关闭源码快照保留 **90 天**，为 typed settings；每次 close 冻结截止时间，改设置不追溯删除。私有层到期只在归档成功、无进程/挂载、无活动引用后回收。Reclaimed 后显式唤醒从 archive 安装新代次，ignored 环境重建；archive 已到期或缺失则拒绝并说明不可恢复。archive 到期只释放无引用的自有 pin，不强删 PR/candidate 分支。快照失败永不触发破坏性回收。

删除先保存独立 tombstone 和逐资源作业再删 track/area；恢复 area 时同时恢复其 resource/archive 引用和恢复闸门。单项失败不吞掉兄弟项。用户来源路径永不在 DestroyPlan 中。

初版拒绝 view 父 track 创建子 track（selection 前拒绝，重驱同样拒绝），不产生 attached None 子行。以后若支持共享父视图或派生视图，需独立定义 lease/证据关系；不能沿用当前复制 path 的实现。

## 6. S-E：可独立合并的切片

依赖 **S1 → S2a → S3 → S2b → S4 → S5**：证据/生命周期先于 view，只有合同齐备入口可用。以下为未来验收，非本次测试。

公共实现 gate G：kernel gate 各跑一次 text ratchet、contract、`scripts/local-rust-gates.sh --quick --base origin/main`，加一次聚焦 nextest replay 并拒绝空选择；worker 跑聚焦测试/formatter 修复，CI 才跑广套件。命令模板 `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p <package> <filter> --test-threads 8`。新增合同过滤器在实现时注册，不把未存在的名字写成已验证；只有实际需要才加 codex-e2e feature。真实模型 E2E 不在共享生产主机跑。

| 切片/结果 | 模块与不变量 | 聚焦验证、gate、等级/依赖 |
|---|---|---|
| S1：现有 worktree/managed 机制封装，只有真实生产调用者 | 新 workspace crate/service；track_worktree、materialize、clean、workspace_recycle 与 area/recovery 调用链迁入机制入口。cwd、选择时点、refusal、branch config、wire/事件、7 天 trash 与 10 分钟 clean 均不变；无新归档/retention/schema 闲置机制 | 现有 track_worktree、回收/area 补偿和恢复 tests；对生产 ensure/delete/recovery 路径作前后等价验证。G；L2（回收所有权），无依赖 |
| S2a：只有 worktree 的类型化收口 + WorkspaceRef 迁移 | §1 全入口、truth 新 migration/sidecar binding、lease 索引、session/control/supervisor、FE track 文件端口。按持久身份/句柄、supervisor、FS/FE 三组收口，建立三类能力、票据/握手和资源账本；不预加 namespace fd。此片开始为实际 materialization 写 B pin（显式新增 Git ref 副作用），不前移 ensure 时点；无 view 可选 | 旧行与事件字节、create→ensure 来源变化、已有 branch 的失败重试、repoint/today、持久化重驱、PTY pipe/attach/restart/ACP/forge/gate/FS 生产入口。G；L2，依赖 S1 |
| S3：worktree 的统一 Revision/Tree、交付与生命周期 | S3 开工前须产出并评审其交付协议实施设计，汇总下列待定点；本文档对 S1–S2a 为定稿。snapshot/verification、git_candidate、lease/checkpoint/publish、账本 install/archive、hibernation/recycle/area/reaper。复用原地 gate 证据与 agent catch-up。显式行为变更：§2.3 账本唯一权威/best-effort 分支投影、自动/Planner 确认的 kernel 修复 op、Planner 准入、统一 plumbing C（不再运行提交 hooks/签名、不保留 agent 私有历史）、存量 worktree/upstream receipt 同片转换、关闭快照/7/90；不改 GateSpec/ignored 构建语义，同片替换旧销毁策略 | 多 attempt 父链、agent 已 commit、只读 head、check_tip、dirty 拒绝、filter/eol 规范化、detach 后 catch-up/fetch/staleness、原地 gate/热缓存、连续 run 与修复重跑、每个 FS/ref/HEAD/DB 崩溃点、PR 阻止删分支、关闭/唤醒/area 回滚。G + 崩溃 replay；L2，依赖 S2a |
| S2b：Linux 同路径视图，创建与关闭/删除/恢复同片 | holder/overlay、D/W、sandbox、supervisor namespace envelope；同片承担显式 view 选择的 API/wire/OpenAPI 字段及真实生成器产物。已过机制合同的 terminal、gate、受控 forge/FS 可用；Planner/conversation/worker 另须 provider ViewSupport（§3.4）。无 Supported Planner 时只开放显式 API/terminal 路径，两 track 生产链隔离用无模型测试，不宣称 Planner 端到端可用；子 track 拒绝 | 两 track 隔离、lower 原地/原子替换、copy-up overlap、whiteout/opaque、后台后代/uid、holder 崩溃恢复、旧 ticket 竞态、外部 gitdir 拒绝、gate lower 钉 tree、create 后立即 close/delete、7/90 到期。G + 允许 userns 的真实 Linux 机制合同；不能 skip 当通过。L2，依赖 S3 |
| S4：Codex exec-server 环境接入 | provider、shared app-server、Planner/conversation/worker 的 start/turn/resume/recovery；配置和所有宿主项目读取受 §3.4 合同约束，未证明不开放 | 部署版本官方 schema、无模型协议 stub/二进制合同、断连/重注册/drain/旧环境拒绝；生产参数编码而非复制 fixture。G + 协议 gate；L2，依赖 S2b |
| S5：完整产品预览/设置与推荐默认 | 产品设置及相关 types/OpenAPI、wire/zod、FE domain/ports/events、创建/恢复/保留期 UI。view 选择字段/生成器属 S2b；S2a 已封住绝对路径，S2b 已提供 view 预览；此片完善体验，能力齐备才推荐 Linux view，设置不换冻结 backend | 真生成器、schema/事件兼容、Report/image/raw/HTML、路径越界、白洞 404、设置和恢复浏览器测试；G + FE lint/build/test、真实浏览器预览与相关 browser E2E，遵循 development-preview。L2，依赖 S4 |

实施时须定（仅归属与约束，不在本文展开）：

- S2a / NB3-6、NB3-8、NB3-9：resolve 增 principal/purpose；按 backend 标明 QuiescenceEvidence 覆盖范围，定 supervisor 核销/撤销与 fork 线性化；删除/私有化 `agent_cwd()`，`DisplayPath` 不实现 `AsRef<Path>`，定 view 四列；扫描用 clippy disallowed-methods（Command::current_dir、std::fs）与逐处 `#[expect(reason)]`，不建文件名单。
- S3 / NB3-2、A3:N2、A4:N2：worktree 采样+排他准入不排除瞬时写入；定 environment_digest、VerificationHandle 写入约束/失败关闭证据，验收 tracked 改后还原、新非 ignored 增后删、后台写与 ignored 热构建。
- S3 / NB3-5：ask/reopen 与 patch closed=false 接入恢复/reopenable，Reclaimed 唤醒、archive 到期拒绝。
- S3 / NB3-9、B4:N5、A4:N1：定 Free 排他转换 op 与可重启协议切换点；drain 旧写入/交付，首次采纳当前分支 tip、B=LegacyUnknown，与 upstream receipt/detach 一次切换；之前不接新写准入，旧 hash payload drain 或版本重驱且不改字节，禁止重试再次采纳。
- S3 / B4:N2：定 git_commit 的 Planner-only 合同、worker 经 done 交付的明确拒绝与旧 branch 参数删除。
- S3 / B4:N3：定以 ReadWrite workspace_lease/commit op owner 登记排他占用，Readers 也 Busy。
- S3 / B4:N4：定未交付 run 释放后 HEAD/index 回装已结算 tip、保留文件的行为变更，含只读 head pin 与类型化放弃入口。
- S3 / B4:N6：定 checkpoint 移出 shell 步骤 0 后 gate 日志编号的显式行为变更。
- S3 / B4:N7：定安装窗口外 D = tree(已结算 tip)，覆盖交付、Planner 提交、catch-up 与 archive 恢复。
- S3 / B4:N8：定普通 lease 准备的 HEAD/index 安装幂等重驱与移出 DB 事务。
- S3 / NB3-4、A3:N1：验收 ref 已 U/安装未完、安装已完/DB 或 lease 未发、连续 catch-up 的生产恢复入口（§4.3）。
- S2b / NB3-2、NB3-3：定验证 ignored 依赖/缓存投影和 upper 丢弃规则；D/W 用逻辑 manifest 重定，清 W 保留 ignored（§2.3/§4.2）。
- S2b / NB3-7：定创建时拒绝 Unsupported Planner 或无 Planner track 的明确 API 语义、overlap 确认执行者/API；无 Supported provider 时交付链无生产 provider 调用者是有意范围。

关键 invariant 的 mutation 只在独占可恢复 worktree 完成：移除 generation/ticket 校验、让 gate 读 live lower、archive 失败仍回收等，单次只改一个生产因素，先预测完整红集再跑整个相关 target/filter，安全恢复并重绿。本轮只执行 doc-exists 与 ratchet gate。

## 7. 架构五点与维护成本

- 抽象边界：kernel 决策、truth 事务、workspace 机制、provider 外部编码；共享 Revision/Tree 和能力阻止 adapter 猜实现。验证以证据不变量为准，worktree 复用原地采样。
- 重复逻辑：只保留 checkout admission、分层组合的静默事实与一台资源生命周期状态机；复用 recycle/hibernation/sandbox，旧清理入口不旁挂第二套政策。
- 硬编码假设：backend 有意闭合，ref 命名是 Neige 自有协议，7/90 为 typed settings；不按模板、插件或路径字符串决定执行隔离。
- 外部事实：Git 路径由 Git 返回；进程身份由自有 nonce 与监督事实证明；Codex 只认部署版本协议。实验记录不足以解除发布阻断。
- 永久成本：新启动/文件调用只用既有能力，不加名单；backend 扩展改 §4.2 owner/合同。增改测试不改扫描名单/阈值；G text/contract/quick 各一次，focused worker + gate 各一次（有意 replay），CI 广套件按现有 workflow。平台/协议合同仅对应实现/升级运行；Facts 不转成永久检查。

## 8. Known gaps 与发布阻断

| 阻断 / 初版范围 | 解除片与准入条件 |
|---|---|
| 活 lower 的 rename/delete/切分支/git gc、复杂构建及 tracked 一致性 | S2b 真实 Linux 合同通过才开放对应宿主；不能 skip 算通过 |
| 本地 provider 按 cwd 状态/项目读取 | S2b provider 层 ViewSupport 合同；Claude 依官方状态作用域，ACP 默认拒绝；未通过仍 Unsupported，不猜 HOME 私有布局 |
| Codex 宿主项目文档/config/git info/ghost/undo/搜索、trust 状态 | S4 部署版本协议合同及独立状态/禁用功能证明，未过拒绝 Codex view |
| 转换仓库、非 Git 交付、子 track、根外 cwd | S2b 在创建/准备和重驱边界拒绝（转换在 snapshot 重检）；解除另立扩展片，非 Linux 继续 worktree |
| gate 写源码、ignored 环境/依赖缓存 | S3 原地证据兼容，S2b 钉 lower；源码写入使 bound 失效，ignored 写入合法，环境摘要不保证可重复构建；不强制新输出目录 |
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
