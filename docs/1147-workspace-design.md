# Wave 工作区

状态：实现中。S0 已合入 #1158；S1 已合入 #1163；S2 实现完成、已过三轮红队，见「已知缺口」。

## 前提

新 FE 尚未上生产。S2 起不再为旧客户端新增字段别名、缺键默认或其他过渡层；破坏性的 API/DTO 变更可以一步到位。S1 已存在的 wire `cwd` 别名保留。

服务端仍必须迁移和回放已经持久化的数据。FE 是否上线不改变这项责任。

## 产品契约

- Neige 在 `$HOME/neige-workspaces` 下管理默认工作区。
- Cove 只是命名空间；每个 wave 拥有独立仓库。
- 创建 wave 时默认分配工作区，不要求用户先选目录。
- 工作区在产生工作前可更换；开始工作后永久冻结。
- 用户仓库可以附加，但 Neige 永不初始化、移动或删除它。
- 子 wave 必须拥有独立工作区，不能继承父 wave 的路径。

## 数据模型

```rust
enum WaveWorkspaceKind {
    Managed,
    Attached,
}

struct WaveWorkspace {
    kind: WaveWorkspaceKind,
    path: String,
    frozen_at: Option<i64>,
}
```

`waves.workspace_path` 是路径的唯一存储。wire 上的 `cwd` 从它派生，不是第二真源。

`Managed` 表示服务端创建、独占且可回收的目录；`Attached` 表示用户已有仓库。这个类型决定删除权限，不能由路径猜测。

除 system cove 的 Today/launchpad wave 外，`frozen_at` 单调：一旦有值，`kind` 和 `path` 永不再变。用户 cove 中的 attached wave 创建时即冻结；launchpad 路径由内核维护，保持未冻结且不接受用户 PATCH。

## 托管工作区

配置：

```text
--workspace-root / CALM_WORKSPACE_ROOT
默认：$HOME/neige-workspaces
```

布局：

```text
<root>/<cove_id>/<wave_id>/
<root>/<cove_id>/<wave_id>/.claude/worktrees/<wave>/<card>
```

目录使用稳定 ID，不使用标题 slug。托管路径不写入 `cove_folders`；该表只维护 attached 路径的 cove 归属。

### 物化

创建 managed wave 时：

1. 按**所有权标记**判定目录归属，再决定是新建、修复还是拒绝（见下）。
2. 使用隔离的 Git 配置**与隔离的 Git 环境变量**初始化仓库，避免全局 template、hook 和签名设置影响结果。
3. 创建一个固定身份的空初始提交，使首次 `git worktree add` 可用。
4. 在 `.git/info/exclude` 中排除 `.claude/worktrees/`。
5. 物化后对 **canonical 路径**断言仍在 workspace root 之内。
6. 任一步失败都让 wave 创建返回错误，不能留下一个稍后才以 `spawn-failed` 暴露的坏 wave。

所有 wave 创建入口必须走同一物化契约，包括普通 REST、workflow/template、cove chat、Today/launchpad 和 child wave。这个集合不应长期靠调用点清单维持，应收敛到统一创建边界。

Attached 创建只做校验：绝对路径、目录存在、是 Git 仓库，并完成 `cove_folders` 的唯一归属检查。

以下六条都是 S2 实测得出的，**每条都有一个能让它变红的测试**；删掉任何一条都会让下一片重新踩一遍。

**所有权标记，而不是「是不是 git 仓库」。** 判据是我们自己写下的 `.git/neige-workspace`（内含 wave id），且必须写在 `git init` **之前**——git init 会保留 `.git/` 下的未知文件，所以任何中途崩溃留下的目录都带标记、可辨认。标记放在 `.git/` 内，对「盘上是空的」判据天然不可见，不会重蹈 `.gitignore` 的覆辙。

用「这是不是一个 git 仓库根」当判据是错的：实测把第三方仓库放在派生路径上，该判据直接放行——仓库被复用、`.git/info/exclude` 被追加、若它还没有提交连 `neige` 作者的初始提交都替它落了。而删除 wave 会 `rm -rf` 所有 `kind = managed` 目录，**误判即误删用户仓库**。

**目录非空时的两种结局。**（修正：不是「非空即失败」。）

- 非空且**没有**我们的标记 → 硬失败，绝不复用。
- 非空且**带有**我们的标记 → 是我们自己的半成品，允许清理并重建：清掉 `.git/` 下所有 `*.lock` 再 `git init`。

第二条不是让步，是必需品。互斥 + 标记 + **清理自有半成品**三者配套，缺第三条仍然砖化：实测标记存在 + `.git/config.lock` 残留（init 中途被 SIGKILL）会让之后**每一次**物化都报 `could not lock config file`，无限重复；落在 Today/launchpad 上就是面板永久死亡。清理的安全性来自两个前提同时成立——标记证明目录是我们的，`HEAD` 不可解析证明没有值得保留的仓库状态、也没有可能持有这些锁的活 worker。

**空初始提交不可省——但理由不是「否则 worker 起不开」。** 它的作用是给「盘上是空的」判据提供**基线**：该判据要求 `git rev-list --count --all == 1`，没有这个提交就没有 1 可比，工作区从第 0 秒起就无法判定是否动过。这条与 Git 版本无关。

⚠️ 此处早先写的实测结论「没有它 `git worktree add` 直接失败（`不是一个有效的对象名：'HEAD'`）」**只对 Git < 2.42.0 成立**，本文档曾据此论证过三轮，现更正。Git 2.42.0（commit `128e5496b`，*worktree add: extend DWIM to infer `--orphan`*）改为：在 HEAD 未出生的仓库里 `git worktree add` **成功**，自动推断 `--orphan`，向 stderr 打印 `No possible source branch, inferring '--orphan'`，把新工作树的 HEAD 指向一个未出生的分支，**不创建任何提交**；`git-worktree` 文档的措辞是「as if `--orphan` was passed」。已在 git 2.54.0 上实测复现：`worktree add` 退出码 0，`rev-list --count --all` 仍为 0，新工作树 HEAD 为 `refs/heads/<branch>`。

也就是说在新版 Git 上，缺少基线提交是**静默**的，比旧版的硬失败更糟：租约工作树建得出来、没有历史、彼此还是互不相关的孤儿分支。测试因此按版本分支断言，并且用**解析出的版本号**判断而非匹配错误文本——该文本在裸/非裸仓库间不同，且会被本地化。

**排除写 `.git/info/exclude`，不写 `.gitignore`。** 未提交的 `.gitignore` 会让 `status --porcelain` 恒有 `?? .gitignore`，于是「盘上是空的」判据从第 0 秒起永假，工作区在 UI 上永远不可更换。

**Git 环境隔离，不只是配置文件隔离。** `GIT_TEMPLATE_DIR` / `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` 的优先级**高于** `-c` 覆盖：实测设了 `GIT_TEMPLATE_DIR` 就能绕过 `-c init.templateDir=`，把 `hooks/` 拷进新仓库。初始提交因 `--no-verify` 幸存，所以它很安静——hook 只在之后 worker 在这个仓里跑的每条 git（`worktree add`、agent 的 commit）上才发作。另需清掉重定向仓库（`GIT_DIR`、`GIT_WORK_TREE`、`GIT_INDEX_FILE` 等）与污染提交身份（`GIT_AUTHOR_*` / `GIT_COMMITTER_*`）的变量。只做配置文件隔离不是隔离，只是把洞换了个位置。

**per-path 互斥，boot 时 canonicalize，物化后按 canonical 路径断言前缀。** Today/launchpad 的 ensure 并发是设计预期（自带竞态重试），而物化在事务之外跑多条 git 命令：实测并发会撞出 `.git/config` 锁冲突，进程中途死掉则留下半成品。前缀断言必须对 `canonicalize` 之后的真实路径做——`create_dir_all` 跟随符号链接，实测存库路径在根下、仓库真身在根外，而词法 `starts_with` 察觉不到，回收路径的前缀断言也就跟着失效。workspace root 在 boot 时 canonicalize 一次；`HOME` 未设时回退**绝对**路径（否则相对根会让每次建 wave 都失败——supervisor / systemd 起的进程没有 `HOME` 是常见配置）。

## 更换与冻结

允许：

- managed → managed
- managed → attached

拒绝：

- attached → 任意目标
- 已冻结 wave 的任何变更
- system cove wave 的用户 PATCH

不可重锚的持久 cwd 消费者出现前必须冻结工作区，包括首次 workspace lease、terminal 持久化、wave 离开 Draft、child wave 创建。冻结应位于真正的底层写入口，不能依赖上层调用点枚举。

更换未冻结的 managed 工作区时，旧仓库必须同时满足：

```text
git status --porcelain --ignored 为空
git rev-list --count --all == 1
git worktree list 只有主工作区
```

变更流程：

1. 在数据库事务内 park 或 supersede harness runtime，阻止新 turn 获得旧路径。
2. 在移动前重新检查仓库仍为空。
3. 断言旧路径属于 workspace root。
4. 将旧目录 rename 到 `<root>/.trash/`；跨文件系统移动失败时中止，不做 copy + delete。
5. 物化或校验新目录，更新唯一存储字段。
6. 用新路径启动新的 harness thread。

SQLite 事务不能隔离文件系统写入，因此“事务内检查一次”不足以关闭检查与 rename 之间的竞态。

### 重指向的两条持久性要求

这两条都来自 S2 实测，都是「换路径」这个动作本身的正确性前提，与更换的判据无关。

**幂等键必须包含路径摘要。** `spec-harness-start` 的载荷带 cwd，而操作运行时拒绝「同一幂等键、不同载荷哈希」。Today/launchpad 的键若只按 `<card>:<mode>` 构造，pre-S2 库里已有按旧路径算哈希的记录；升级重指向之后每次 ensure 都用同一个键提交新 cwd，从**第二次起永久 409**，而系统从不删除操作记录，因此不会自愈。任何 `CALM_WORKSPACE_ROOT` 变更同理。把路径摘要并进键即可：重指向会铸造新键，而同一工作区内的幂等性不受影响。

**重指向意图必须可持久推断，不能靠一次内存比较。** 「存储路径 ≠ 期望路径」只在移动路径的那一个事务里为真，而物化在事务提交之后执行：若物化失败，或进程在提交与记录操作之间被杀，意图就丢了。下一次 ensure 看到「已是期望值」判为稳态、不强制新建 thread，于是 spec harness 的 thread 永远停在旧 cwd 而所有 worker 用新 cwd。正确的问法是一个持久事实：**这个路径上有没有成功启动过 harness**——路径摘要已在幂等键里，操作表可以直接回答，且该答案只在启动真正成功后才被写下，因而跨越所有崩溃窗口。

## 生命周期

- 删除 wave：只回收 managed 目录，并先验证路径位于 workspace root。
- 删除 cove：回收其所有 managed wave 目录和空的 cove 目录。
- attached 路径永不移动、初始化或删除。
- 归档不回收；长期磁盘回收需要独立策略。
- 子 wave 创建时分配独立 managed 仓库并立即冻结。
- fork/template 复制报告，不继承源 wave 的工作区。

回收必须通过单一受控入口；任何直接递归删除都不得接受未经类型和根目录校验的路径。

## Cove 对话与执行器

没有 attached folder 的 cove 仍可创建对话；它使用默认 managed 工作区。已有 folder claim 的 cove 可以继续采用 attached 语义。

Managed 仓库保证 Codex 能获得 Git 工作区。Claude 当前不读取 wave workspace，迁移到同等 worktree 租约是独立工作，不属于本设计。

Managed 仓库默认没有 remote；需要操作真实代码仓库时，用户应在创建时 attached，或在冻结前从 managed 切换到 attached。

## 交付顺序

1. S0：失败原因进入 task 状态与前端。
2. S1：typed workspace、迁移、唯一存储、wire 类型。
3. S2：托管根、物化、所有创建入口默认 managed。
4. S4：子 wave 独立分配并冻结。
5. S5：安全回收和根目录断言。
6. S3：工作区更换、冻结、harness 重锚定、FE attached 入口。
7. S6：terminal 默认落在 wave 工作区。

S4 必须不晚于 S5；S3 必须复用 S5 的路径安全边界。

## 已知缺口

S2 实测可达、被刻意推迟的缺口。**每条都有一个断言缺口本身的测试**（`nX_` 命名或带 `KNOWN GAP (#1147 nX)` 文案），所以修好它的那一片会看到测试变红，必须显式替换而不是顺手改绿。

| # | 缺口 | 后果 | 归属 |
|---|---|---|---|
| N4 | 挪动 `CALM_WORKSPACE_ROOT` / `$HOME` 后，既有 managed wave 的存库路径不再落在配置根下 | 该 wave 取 lease 永久硬失败，**无迁移路径**。launchpad 会自愈（路径每次 ensure 重新派生），普通 wave 不会 | 回收/搬迁片 |
| N5 | 我们自己的工作区丢了 `.git/neige-workspace` 标记（备份部分还原、误清理） | 永久拒绝，且没有任何管理入口能重新认领 | 回收片 / 终端片 |
| N7 | 符号链接是「拒绝，但已经写过了」 | 根外留下一个带我们标记的完整仓库，无人回收。先拒后写不可避免：真实位置要 `create_dir_all` 之后才知道 | 回收片 |
| N9 | 物化互斥是**进程内**的 | 两个 calm-server 实例（升级重叠、admin CLI、健康重启）仍会撞出锁残留。清理逻辑让它可自愈而非永久砖化，但撞击本身还在；真解是文件锁 | 回收片或独立 issue |
| N10 | 环境隔离只到物化边界 | 紧邻的 `git_repo_root_for_wave_cwd` 与 worktree 相关 spawn 仍是裸 `git` 调用，同一组变量对它们仍然有效 | 独立 issue |
| N11 | S2 **持久化**了 `kind = managed` 且路径等于父目录的 child 行 | 见下 | **子 wave 片，必须带数据迁移** |

**N11 单列强调。** 交付顺序把「子 wave 独立分配」排在回收之前，依据是前者消除共享目录。但子 wave 片若**只改 adapter**，只修好此后新建的子 wave；S2 已经写进库的行仍然指向父仓库，于是回收片一落地，删除子 wave 依旧会 `rm -rf` 掉父 wave 的整个仓库——事故没有被代码修复消灭，而是以**数据**的形式存活了下来。**子 wave 片的验收必须包含存量 child 行的迁移**，不能只有 adapter 改动加一条新建路径的测试。

## 验收重点

- 新 managed wave 能直接创建第一个 worktree。
- 物化失败时 create 返回非 2xx，且不留下可见 wave。
- 删除 attached wave 不触碰用户仓库。
- 子 wave 路径与父 wave 不同；删除子 wave 后父仓库仍可用。
- 全局 Git 签名、模板和 hook 配置不会影响物化。
- 工作区有普通文件、ignored worker 产出、其他分支提交、stash、活 worktree 或 terminal 时，更换均被拒绝。
- 活跃 turn 与更换并发时不能在 trash 中继续产生产出。
- 任意回收或 rename 传入 workspace root 外路径时失败。
- GET wave 与 Today/launchpad 路径均只从 `workspace_path` 读取并派生 wire `cwd`。
