# 调研：Track 工作区的 COW 与可选 worktree（草案）

状态：调研稿，未定案，不对应任何已合并实现。基于 `22b530cce` 的代码。

## 1. 现状事实

| # | 事实 | 位置 |
|---|---|---|
| F1 | 给了 `cwd` 的 track 一律走 `AttachedWithTrackWorktree`，没有别的分支 | `crates/calm-server/src/routes/tracks.rs:983-994` |
| F2 | attached `cwd` 必须是 git 仓库，否则创建直接 400 | `workspace_lease/mod.rs:594-617` |
| F3 | worktree 由 `git worktree add -b neige/track-<id>` 生成，对象库共享，只复制工作文件 | `workspace_lease/track_worktree.rs:64-117` |
| F4 | `TrackWorkspace.worktree: Option`，`agent_cwd()` 在 `None` 时回落到 `path`（用户本人的 checkout） | `calm-types/src/model.rs:210` |
| F5 | Codex Planner 线程以 `workspace-write` 启动，cwd 终身不变 | `planner_harness_start_adapter.rs:1178` |
| F6 | 只读任务（#1917）已能共享 checkout、不记 base、不产生交付 | `workspace_lease/worker.rs` 头注释 |
| F7 | 本机：ext4（无 reflink）；内核 6.1，支持 overlay，允许非特权 userns | `df -T`、`/proc/filesystems` |
| F8 | 本仓库每个 worktree 约 63–73 MB，当前 37 个共 2.7 GB；`.git` 不复制 | `du -sh` |
| F9 | Claude 沙箱每个已注册 worktree 约多 2 条 deny 路径，约 590 个时 E2BIG（#1815） | `1830-track-worktree.md` F21 |

结论先行：对这个仓库，全量复制的**磁盘**代价并不大（对象共享）；真正的痛点是
(a) 只支持 git、(b) 每个 track 都要一份、数量线性增长并撞 #1815 上限、
(c) 大仓库/带构建产物时复制慢。可选 worktree 比 COW 收益更直接。

## 2. 问题一：有没有比 git worktree 更好的 COW 方式

| 方案 | 原理 | 非 git 目录 | 创建开销 | 依赖 | 主要风险 |
|---|---|---|---|---|---|
| A. git worktree（现状） | 共享对象库，检出全部文件 | 否 | O(文件数) | 仅 git | 线性增长、仅 git |
| B. worktree + sparse-checkout | 只检出需要的目录 | 否 | O(子集) | git ≥2.25 | 需要知道范围，工具读到缺失文件会困惑 |
| C. reflink 克隆（`cp --reflink`/`FICLONE`） | 文件系统块级 COW | 是 | 近 O(元数据) | btrfs / XFS / APFS | **本机 ext4 不支持**；仍需 git 元数据 |
| D. 文件系统快照（btrfs subvolume / ZFS clone） | 整目录瞬时快照 | 是 | O(1) | 目录须在对应 FS 上 | 部署绑定存储，难以通用 |
| E. OverlayFS（内核或 fuse-overlayfs） | 只读 lower + 每 track 的 upper | 是 | O(1) | Linux；需挂载命名空间或 fuse | lower 必须不可变；挂载重启即失、要重建；agent/终端都得进入同一挂载命名空间；macOS 不可用 |

推荐分层，而不是押单一协议：

1. **抽象一层 `WorkspaceProvider`**（kernel 内的封闭集合，按能力探测选择）：
   `materialize(base) -> path`、`changes() -> 交付物`、`discard()`。现有 git worktree 是第一个实现，
   行为不变。交付契约仍然是 **git commit/SHA**——候选、gate、publish 都依赖它，不要换。
2. **不可变基线快照按 commit SHA 去重**：`<repo>/.claude/snapshots/<sha>`，同一基线的多个 track 共用，
   作为将来 overlay 的 lower，天然满足“lower 不可变”。注意它和 worktree 一样只含被跟踪文件（见第 4 节）。
3. **COW 实现按宿主能力渐进**：有 reflink 的 FS 用 C（从快照 reflink 出可写副本 + `git worktree add --no-checkout`
   接上元数据）；Linux 且接受挂载命名空间时再评估 E；都没有则回落 A。注意这是**按能力选择实现**，
   不是对同一件事的静默兼容兜底：选择结果记录在 track 上，可观测。
4. **非 git 目录**：交付仍需 git。可在 kernel 侧为其建影子仓库（类似 managed track 的 `git init`），
   用户目录本身不写；变更以 patch/commit 形式交给用户确认后再落盘。这是独立的大改，建议单列。

不建议现在就做 E：挂载生命周期、重启恢复、所有子进程（codex、claude、终端、gate）进入同一命名空间，
这些是新的隔离边界（L2），收益在本机 ext4 + 70 MB 仓库上很小。

## 3. 问题二：worktree 可选（只读场景）

### 目标

只读 track（调研、评审、问答）不创建 worktree；但**绝不允许写到用户 checkout / 主分支**。

### 关键约束

- F4 的回落是陷阱：如果简单地让 attached track 的 `worktree = None`，`agent_cwd()` 会回到用户主
  checkout，而 Planner 以 `workspace-write` 运行（F5）——这正是要避免的“破坏主分支”。
  所以不能靠 `Option` 表达“只读”，要有显式类型。
- 用户 checkout 可能脏、可能在动，只读 agent 读到的内容不可复现。

### 设计

1. **显式的工作区模式**（创建时选择，冻结）：
   `TrackWorkspaceMode = Isolated（现状） | ReadOnly { base_sha } | Lazy { base_sha }`。
2. **ReadOnly / Lazy 的 cwd = 用户自己的 checkout**，由沙箱强制只读（修订：初稿用共享快照，
   但快照和 worktree 一样丢失本地环境，见第 4 节）。这样 `.env`、`node_modules`、`external/`
   等本地环境原样可用，相对路径全部有效；代价是读到的内容随用户 checkout 变化、不可复现，
   需要钉住版本的评审仍走 `head` 校验（#1933）。
3. **写入栅栏**（纵深防御，任何一道都不依赖 agent 自觉）：
   - Planner：Codex 用 `read-only` 沙箱，Claude 去掉 `Edit/Write cwd/**` 许可、Bash 沙箱对 checkout 只给读；
   - 任务：ReadOnly track 拒绝没有 `access: read_only` 的 codex/claude 任务（新的 refusal 词，如 `track-read-only`）；
   - forge：`git.commit`、`publish` 对无 worktree 的 track 直接拒绝；
   - 终端卡片：同样以只读沙箱启动，或在 ReadOnly track 上不提供可写终端。
4. **Lazy 升级**：第一个写任务到来时，kernel 调用现有幂等的 `ensure_track_worktree`（含第 4 节的环境带入），
   之后写任务在新 worktree 运行。Planner 线程 cwd 终身不变（F5），所以 Planner 留在用户 checkout 里只读，
   写全部交给 worker；这也与“Planner 不实现、只规划”的分工一致。
5. **默认值**：保持 `Isolated` 为默认（不改变现有 track 行为）；模板（如调研模板）或创建表单显式选择
   `ReadOnly`/`Lazy`。

### 需要改动的面（估计）

`TrackWorkspace` 类型与迁移（新列，新迁移，不改旧迁移）、创建路由的计划分支、`worker.rs` 准入、
Planner 两种 provider 的沙箱参数、forge 动作准入、快照的建立/回收（随 track 删除引用计数）、前端创建表单。
涉及隔离与持久化边界，按 AGENTS.md 属于 **L2**，需先出正式设计文档再分片实现。

## 4. 问题三：worktree 丢失本地环境

### 现象

`git worktree add` 只检出**被跟踪的文件**。用户 checkout 里被 ignore 的本地环境不会出现在 worktree 里。
本仓库主 checkout 实测（`git status --ignored`）：`.env`、`external/`、`fe/node_modules/`、
`mobile/node_modules/`、`.codex-local/` 等都不在 track worktree 中。另外 worktree 位于
`<repo>/.claude/worktrees/track-<id>`，比仓库根深三层，指向仓库外的 `../xxx` 相对路径也会解析错位。

COW 方案对此帮助不同：基于 git 的 A/B 同样丢失；C/D/E 若以**用户 checkout 整个目录**（含 ignored 文件）
为基线，则环境天然带上——这是 COW 相对 worktree 的真实优势，但本机 ext4 只能用 E（overlay）。

### 设计：仓库声明的环境带入清单

1. **仓库自己声明**要带入 worktree 的本地环境（提交在仓库里的配置文件，例如 `.neige/worktree.toml`），
   kernel 不硬编码 `.env`、`node_modules` 之类的名字。每项一个仓库内相对路径和一种方式：
   - `copy`：复制一份，track 改它不影响主 checkout（适合 `.env`、本地配置等小文件）；
   - `link`：符号链接到主 checkout 的同一路径，共享不复制（适合 `external/`、大依赖目录）。
     写穿到主 checkout 是这种方式的固有代价，所以链接目标同时加入沙箱的禁写路径，只读共享；
   - `setup`：在 worktree 里运行一条声明好的命令重建（例如 `npm ci`），彼此完全隔离，最慢。
2. **在 `ensure_track_worktree` 里执行**，在任何 agent 启动之前；源路径不存在时明确报错或记录，不静默跳过。
3. **防止被提交**：每项必须被仓库 `.gitignore` 忽略（`git check-ignore` 校验），否则拒绝带入，
   避免 kernel 提交 worker 改动时把 `.env` 带进 commit。
4. **仓库外的 `../` 路径**：清单无法覆盖。可选办法是把 worktree 放在与仓库同级、同深度的位置
   （如 `<parent>/<repo>@track-<id>`），代价是在用户目录旁新增目录、改变 #1830 D3 的路径约定和沙箱登记。
   只有确实存在这类依赖时才值得做。

## 5. 建议的推进顺序

1. 先做 `ReadOnly`/`Lazy` 模式：只读场景直接在用户 checkout 里只读运行，本地环境问题在这类 track 上消失。
2. 再做环境带入清单，解决写场景 worktree 缺环境的问题（与 1 互不依赖，可并行设计）。
3. 引入 `WorkspaceProvider` 抽象，现有 git worktree 作为唯一实现落地（纯重构）；
   视部署存储决定是否加以“用户 checkout 全目录”为基线的 reflink/overlay 实现；非 git 目录支持单独立项。
