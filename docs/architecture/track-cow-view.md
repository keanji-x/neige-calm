# Track 工作区：Workspace 框架与写时复制视图实现（第三版草案）

状态：历史实验与草案，正式设计及分片以 [track-workspace.md](track-workspace.md) 为准。本文不对应已实现功能。取代 [lazy-track-worktree.md](lazy-track-worktree.md) 的第一、二版。
背景调研见 [research-track-workspace-cow-optional.md](research-track-workspace-cow-optional.md)。

## 框架与实现分层（2026-10-16 修订）

overlay 视图只是一种实现。kernel 只依赖一个 Workspace 接口：

- 实现提供：`create(来源目录, track, 基线)`、`sync()`（仅在 track 空闲时调用）、`launcher()`（启动任何进程的唯一入口：
  工作目录 + 如何把命令放进工作区）、`codex_environment()`（本地目录或 exec-server 地址）、`changes()`（相对基线的
  未被 ignore 的新增/修改/删除，并标出改动时来源已有未提交改动的文件）、`read(path)`、`destroy()`、`capabilities()`。
- kernel 统一完成：交付（用 `changes()` 以基线为父生成 commit 到 `neige/track-<id>`）、关闭快照（同样的 commit 到
  `refs/neige/archive/track-<id>`）、保留期清理、`sync()` 的调度、按宿主能力选择实现并冻结在 track 上。
- 实现一 git worktree：现有行为收进接口，`sync` 不自动做（仍用 catch-up），`launcher` 只给 cwd，`changes` 用 `git status`。
- 实现二 写时复制视图：本文其余部分。`launcher` 进入 track 的命名空间后再嵌套一层用户命名空间把身份映射回真实 uid
  （外层 root 只用于挂载）；否则 Claude 自带沙箱起不来（见第四组实验）。
- 将来的实现（如 macOS APFS 克隆）只需实现同一组操作。

刷新策略（取代“每轮重挂”）：每个 track 一个视图，所有进程共享；只在“来源目录自上次挂载后确实变了”且“track 空闲”时
`sync()`。Planner 轮次之间通常是空闲点，但不是必须；有常驻进程时推迟，可手动触发。不同 track 不共享视图（需各自的私有层）。

## 问题出在哪

前两版都在回答“agent 在**哪个目录**里跑”：用户 checkout（保护不住）、worktree（丢环境、要按需创建）、
参考副本（看不到未提交改动）。每个答案都要在“看得全”和“碰不到”之间取舍，于是长出 Lazy 状态机、栅栏、
快照这些补丁。

结构性的换法：**不给 agent 换目录，而是给 agent 换文件系统视图。**
agent 在与用户 checkout **完全相同的路径**上工作，看到的就是用户的真实状态；它的写入落在私有层，
物理上到不了用户的文件。

## 机制

每个 track 一个 Linux mount namespace，在用户 checkout 的路径上挂一个 overlay：

- lower = 用户的 checkout（活的，用户继续正常编辑）；
- upper = kernel 自有目录 `<state>/tracks/<id>/upper`（在仓库外）；
- kernel 为这个 track 启动的**所有**进程——Planner、assistant、worker、终端、gate、forge shell——都进入这个 namespace。

隔离的唯一卡点是“kernel 启动进程”这一处，不再依赖各 provider、各权限模式的沙箱是否完备。

## 本机实测（ext4，内核 6.1，普通用户）

| 验证 | 结果 |
|---|---|
| `unshare -Urm` + overlay，无需 root | 可行 |
| overlay 挂在与 lower 相同的路径上 | 可行，路径不变 |
| 视图内写文件 | 写入只出现在 upper，用户文件不变 |
| 视图内读取被 ignore 的 `.env` 和用户未提交的改动 | 可见 |
| 用户在视图外原地修改一个 agent 没碰过的文件 | 视图内立即看到新内容（原子替换则看不到，见“实验结果”） |
| 视图内 `git add -A && git commit` | 用户仓库 HEAD 不变，新对象和引用只在 upper 的 `.git` |
| bwrap 0.8.0 的 `--overlay` | 不支持（需更新版本），直接用 unshare + mount 即可 |

## 它一次性回答了前面所有问题

| 问题 | 怎么解决 |
|---|---|
| 复制全量（问题一） | 不复制；upper 只存 agent 改过的文件和构建产物 |
| 只支持 git（问题一） | overlay 对任何目录都成立 |
| worktree 可选（问题二） | 不再需要 worktree，也不需要 Lazy 状态机；只读 track 就是从不写 upper 的 track |
| 丢本地环境、`../` 路径（问题三） | 路径相同，`.env`、`node_modules`、`../` 全部原样可用；不需要带入清单 |
| 不破坏主分支 | 由挂载保证，不靠沙箱；终端、gate、内核代跑的 forge 都在视图内 |
| 部署 main / 只运行不修改 | 直接在视图里跑，产物进 upper |
| 并发 | 每个 track 一个 upper，彼此看不到对方的写入；都能看到用户的实时状态 |
| 快照/备份 | 不需要对你的 checkout 做；丢弃 track = 删除 upper（关闭时的源码快照见“生命周期”） |

语义可以概括为：**按文件写时分叉**——agent 没写过的文件始终跟随用户的实时状态，写过的文件从那一刻起是它自己的副本。

## 交付

- 视图内的 `.git` 也在 overlay 下：agent 的 git 操作（含 commit、切分支）只改它的私有副本，碰不到你的引用。
  （修订：初稿想给视图配一个共享对象库的 gitdir，但那要求视图能写真实 `.git`，agent 就能改到你的 `main`，放弃。）
- 交付不依赖 agent 自己的 commit。track 创建时 kernel 记下基线 B = 你当时的 HEAD；交付时 kernel 在视图外，
  用临时 index 以 B 为父、把 upper 中出现的、未被忽略的路径（含删除标记）写成 commit，放到 `neige/track-<id>`。
  你的工作区、index、分支都不动。之后 gate、publish、PR 流程不变。
- 你未提交的改动不会进 commit，除非 agent 改了同一个文件（它复制上去时连带你的改动），这种文件交付前标出。
- 你在 track 运行期间切分支或提交，会让视图底下大面积变化；kernel 检测到你的 HEAD 变化时提示，由你决定继续或重建视图。

## 风险与待验证

- R1 内核文档称 overlay 挂载期间修改 lower 属于未定义行为（不会崩溃或死锁）。实测普通编辑正常，
  需要专门压测：重命名、删除目录、用户切分支、`git gc`、编辑器原子替换写入。
- R2 共享 Codex app-server：见下文“Codex 接入”，已有官方路径，不需要每个 track 起一个 app-server。
- R3 视图外的读者（前端文件预览、kernel 自身的 gate 编排）要通过 namespace 读，或由 kernel 自己合并 upper 与 lower。
- R4 挂载随进程消失；upper 在磁盘上持久，服务重启后重建 namespace 即可，状态不丢。
- R5 macOS 没有 overlayfs：后端可用 APFS `clonefile`（写时复制，但路径不同）或退回 worktree。
  按宿主能力选择后端，并把结果记在 track 上。
- R6 网络不隔离：多个 track 同时起服务会抢端口，需要端口段分配（Conductor 的做法）或后续加 net namespace。
- R7 文件以外的副作用（部署到服务器、改数据库）不在视图范围内，仍走逐次用户批准（#2617）。

## Codex 接入：不需要每个 track 一个 app-server

整个服务只有一个共享的 `codex app-server`（`crates/calm-server/src/shared_codex_appserver.rs:1-2`），
一个进程只能处在一个 namespace。但 Codex 已有“执行环境”抽象，正好用来解决这个问题（源码：`external/codex`，`5a440c0`）：

- `codex exec-server`：一个只负责“起进程 + 读写文件”的小服务（`codex-rs/exec-server/README.md`）。
- app-server 的实验接口 `environment/add {environmentId, execServerUrl}` 注册一个环境；
  `thread/start.environments` / `turn/start.environments` 为线程或单轮选择环境与 cwd
  （`codex-rs/app-server/README.md:133、196、660`，`app-server-protocol/src/protocol/v2/thread.rs:146-152`）。
- 改文件（apply_patch）走所选环境的文件系统（`core/src/tools/runtimes/apply_patch.rs:4-5、227`），
  执行命令也走所选环境（`core/src/tools/handlers/unified_exec/exec_command.rs`）。

做法：app-server 保持一个；每个 track 在自己的 namespace 里起一个 exec-server，注册为该 track 的环境，
线程启动时选它。模型推理、会话、鉴权都还在共享 app-server 里，只有“动文件和跑命令”进入视图。

| 方案 | 每 track 额外内存（本机实测） | 评价 |
|---|---|---|
| 每 track 一个 app-server | 约 290 MB RSS | 太重 |
| 共享 app-server + 官方 exec-server | 空闲约 258 MB 私有内存（`codex exec-server`，0.159.2，PSS） | 可行，但只在 track 有活跃轮次时起、空闲即停 |
| 共享 app-server + neige 自己实现的精简 exec-server（同时充当 namespace 持有进程） | 预计几 MB | 最省，但要跟随一个实验协议，有维护成本 |

风险：`environments`、`environment/add`、`exec-server` 都标为 experimental，Codex 升级可能改动；
需按 AGENTS.md“外部契约事实”只依赖其文档化字段，并在升级时有契约测试。

## 资源消耗

| 项 | 视图 | 现在的 worktree |
|---|---|---|
| 创建 | 一次挂载，毫秒级，不复制文件 | 检出约 4600 个文件，约 64 MB |
| 常驻进程 | namespace 持有进程（几 MB）+ 活跃时的 exec-server | 无 |
| 源码改动 | 只存改过的文件 | 全量一份 |
| 构建产物 | 写入 upper；可复用你已有的 `target/`（本机 132 GB）做增量编译 | 每个 track 从零构建（本机一个 track 的 `target` 约 8 GB） |
| 内核资源 | 每个视图一个 mount，可忽略 | 每个 worktree 一条 git 注册与 Claude 沙箱路径（#1815） |

构建产物是最大的变量，需要在验证阶段实测：
- 复用你的 `target/` 时，cargo 只重编改动的 crate，新产物进 upper，预计远小于从零构建；
  但增量编译缓存会被整文件复制到 upper。
- 你在视图外同时构建，会改动下层的 `target/`，视图可能读到写了一半的产物。保守做法是给视图单独的
  构建目录（不复用缓存，等同现在），或只读复用、写到 upper（需压测）。

## 生命周期：关闭时快照，定时清理

现状（#2356 S2，`operation/workspace_lease/track_worktree_clean.rs`）：track 关闭后每 10 分钟扫一次，
对关闭且空闲的 track worktree 执行 `git clean -fdX` 删被忽略的构建产物；worktree 目录、源码改动和分支要等
track 或 area 删除才清。`.claude/worktrees/` 下还有不是 track 建的 worktree（如 `997-c1-today`、`fe-fix-1016`），
kernel 不管。

新增三步，worktree 与视图通用：

1. **关闭即快照源码改动**：用临时 index，以工作区 HEAD 为父，把所有未被忽略的改动（含未提交、未跟踪、删除）
   写成一个 commit，挂在 `refs/neige/archive/track-<id>`。不碰工作区，只存源码，体积很小。
   视图下就是把 upper 中未被忽略的文件写成 commit。
2. **保留期后删除工作区**：关闭满 N 天（可配置，例如 7 天）且无人占用，删除 worktree 目录与注册（视图下删除 upper）。
   分支和快照引用保留。重新打开 track 时，从分支重建工作区并恢复快照。
3. **快照的保留期**：快照引用在 track 删除时或关闭满 M 天（例如 90 天）后清除。

不管：不是 kernel 建的 worktree（只在界面列出供你处理）；被忽略的文件不进快照（构建产物可重建；
`.env` 之类由你自己保管）。

## 实验结果（2026-10-16，本机，Codex 0.163.0 装在隔离目录，未切换 neige 正在用的版本）

| 实验 | 结果 |
|---|---|
| 在视图 namespace 里起 `codex exec-server`，用其协议 `fs/readFile` 读 `.env` 和用户未提交的文件 | 读到 |
| `fs/writeFile` 写新文件 | 只进 upper，用户目录没有该文件 |
| `process/start` 跑 `mkdir out && echo >> f.rs && git commit` | 在视图内成功；用户 HEAD、分支、工作区不变；upper 出现 `out/`、`f.rs`、`.git` 改动 |
| kernel 在视图外收割：以基线为父，把 upper 中未被忽略的文件写成 commit 到 `neige/track-exp` | 成功；`out/bin`（被忽略）跳过；`f.rs` 被标出“复制时带着用户未提交改动” |
| exec-server 0.163.0 空闲内存 | 约 326 MB PSS（比 0.159.2 的约 266 MB 更大） |
| 视图挂载后，用户**原地修改**文件 / 新建文件 | 视图内可见 |
| 视图挂载后，用户**原子替换**一个视图已读过的文件（编辑器“写临时文件再改名”、`git checkout`） | **视图内看不到，仍是旧内容** |
| 视图已查过“不存在”的文件，用户随后创建；视图已访问过的目录被用户改名 | **看不到** |
| 重新挂载（同一 upper） | 全部最新 |

结论与设计修订：

- “视图实时跟随你”只对原地修改成立；编辑器和 `git checkout` 多为原子替换，视图读过的文件会停在旧内容。
  所以**不能承诺实时**，改为“**每轮（每次 Planner 轮次、每个任务尝试）开始时刷新视图**”：在 namespace 内卸载旧挂载、
  用同一个 upper 重新挂载。内核文档允许在未挂载时修改 lower（前提是不启用 index、metacopy、redirect_dir、xino，
  非特权挂载默认不启用），因此“轮与轮之间刷新”是受支持的用法；轮内的视图近似快照。
- 跨轮常驻的进程（例如终端里的 dev server）仍停在旧挂载，需要重启才能看到新内容。
- exec-server 按请求解析绝对路径，刷新后新请求自动落到新挂载；需验证就地重挂时旧挂载上的打开文件与 upper 复用的告警。
- exec-server 官方实现内存偏大，“neige 自己的精简 exec-server”优先级上升，但它要跟随实验协议。

## Codex 0.159.2 → 0.163.0 与本设计相关的变化

- exec-server 与执行环境在密集开发：可取消的文件读取、流式文件写入（`fs/writeBlock`、writable file streams）、
  会话恢复、环境请求可在线程与轮次边界设置（#49286、#49407、#49696、#49778、#49805、#50177、#51221、#51422）。
- `capability roots` 限定在本轮捕获的环境内（#49280），工具暴露随环境就绪变化（#50741、#50962）。
- 0.162.0 新增“受管 Git worktree”工具（#50148）：Codex 自己也在做 worktree 管理，需避免与 neige 的工作区管理冲突。
- Linux 沙箱：修复多个禁读文件时的启动问题，拒绝可被沙箱写入的 bwrap 可执行文件（#50059、#51211）；文档说明 bwrap + seccomp（#52610）。
- 恢复线程时恢复其权限与网络审批（#53150 等）；不再捆绑打过补丁的 zsh（#52160）。
- 默认模型目录换为 GPT-6.1 Sol（#49318）；0.163.0 默认启用 Daybreak（#53701）。

## 第二组实验（2026-10-16，neige 已切到 Codex 0.163.0）

| 实验 | 结果 |
|---|---|
| 升级：`codex update` 装入 0.163.0 并切换 `current`，终止旧 app-server 后由 neige 冷重启 | 23 秒内新 app-server（0.163.0）起来，29 个线程恢复 25 个；失败的 4 个报 “no rollout found”，升级前日志里已出现过，非升级引起 |
| 就地刷新：namespace 内 `umount -l` 后用同一 upper 重新挂载，exec-server 进程不重启 | 刷新后新请求看到你原子替换的文件；agent 写的文件仍在 upper；你的目录没有 agent 文件 |
| `environment/add` 注册视图里的 exec-server，`thread/start.environments` 绑定它 | app-server 接受，线程响应里带上该环境，app-server 已连上视图内的 exec-server |
| 在该线程上用 `thread/shellCommand` 跑命令 | 报 “shell is unavailable in this session”（`core/src/tools/handlers/shell.rs:76-79`，该轮没有主环境）；尚未查清是用户 shell 路径不走线程环境，还是环境当时仍在连接 |

未做：需要模型轮次才能验证的 apply_patch 走环境（按 AGENTS.md 不在生产主机上跑真实 Codex 轮次）；视图内构建本仓库与产物量。

## 第三组实验（2026-10-16，本机，Codex 0.163.0）

| 实验 | 结果 |
|---|---|
| 真实模型轮次：独立 app-server（共享模式）+ 视图内 exec-server，`thread/start` 与 `turn/start` 都带 `environments`，让 Codex 用 apply_patch 新建文件、修改带你未提交改动的文件、跑 `cat .env; mkdir out; git status` | 轮次完成；`agent_note.txt`、`f.txt`、`out/bin` 全部只出现在 upper；你的目录、`f.txt`、git 状态不变；Codex 能读到 `.env` |
| 真实模型轮次里 7 种 shell 调用（`workspace-write`）：6 秒长任务、`cd` 子目录、写 `$TMPDIR`、等待输入的 `read`、`git`、`python3`、在仓库内建文件 | 全部退出码 0；仓库内新文件只在 upper；写 `/tmp` 落在真实 `/tmp`（视图只覆盖仓库路径） |
| 视图挂在你真实的 neige-calm checkout 上，改 `calm-types` 后 `cargo check -p calm-types` | 8.5 秒，复用你已有的 100 GB `target/debug`；upper 只增 8.4 MB |
| 同一改动下 `cargo check -p calm-server`（依赖链重编） | 53 秒，upper 共 79 MB |
| 对照：同一源码、空构建目录（相当于新 worktree）`cargo check -p calm-types` | 12.9 秒、72 MB；calm-server 级别的冷构建按现有 track 数据约 8 GB |
| 你的 `crates/calm-types/src/lib.rs` | 未被修改 |

`thread/shellCommand`（Codex TUI 用户 `!` 命令）报 “shell is unavailable”，按用户意见暂不支持；不影响结论：真实轮次的命令与 apply_patch 都走了环境。

## 结论

1. **可行性已验证**：同路径写时复制视图 + 共享 app-server + 每 track 的 exec-server，Codex 的读文件、改文件、跑命令都在视图内，
   用户 checkout 与分支不受影响；kernel 能在视图外把私有层收割成交付 commit。
2. **语义**：视图每轮刷新、轮内近似快照（原子替换在轮内看不到，就地重挂后可见，exec-server 不必重启）。
3. **资源**：创建零复制；构建复用用户已有缓存，一次真实改动的私有层是几十 MB 量级，而新 worktree 是 GB 级；
   主要成本变为每个活跃 track 一个 exec-server（官方实现约 326 MB），可按需启停或由 neige 自行实现精简版。
4. **剩余风险**：执行环境接口是 experimental；你在 track 运行期间自己构建时，视图可能读到写了一半的下层产物；
   macOS 无 overlay；端口不隔离；Claude、终端、gate 进入视图的统一入口尚未实现。

## 第四组实验（2026-10-16，Claude Code 2.1.280，开启 Claude 自带沙箱）

| 实验 | 结果 |
|---|---|
| 在视图命名空间里（root 映射）运行 `claude -p`，要求 Write、Edit 和 5 条 Bash | Write/Edit 成功且只在 upper；Claude 沙箱起不来：`apply-seccomp: write /proc/self/uid_map: Operation not permitted`，Bash 被拒 |
| 同上，但启动前 `unshare --map-user=1000 --map-group=1000 -U` 把身份映射回真实 uid | 全部成功：Write、Edit、`cat .env`、循环、`cd sub`、`git status`、`mkdir`；写入只在 upper，用户 `f.txt` 与 git 状态不变 |

推论：`launcher` 必须让 agent 以真实 uid 运行；Codex 实验中 exec-server 以 root 映射运行，其沙箱是否生效需按同样方式复测。

## 分片（L2，先做验证再定案）

0. 验证：R1 压测；exec-server 环境接入实测；视图内跑本仓库的构建与测试并量构建产物。
   可独立先做：关闭快照与保留期清理（对现有 worktree 立即生效）。
1. 视图原语：namespace 持有进程、overlay 挂载、进入视图的统一 spawn 入口、重启重建。
2. 所有 track 进程走统一入口（Planner、assistant、worker、终端、gate、forge）。
3. 交付：kernel 在视图外以基线为父提交 upper 路径、脏文件标注。
4. 视图外读取与前端预览。
5. 非 Linux 后端与回退。
