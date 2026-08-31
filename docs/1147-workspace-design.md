# Wave 工作区

状态：实现中。S0 已合入 #1158；S1 已合入 #1163；S2 已合入 #1182；S4 已合入 #1193；S5 已合入 #1201；S3 实现完成（更换与冻结 + FE attached 入口）。

## 前提

两条并列前提，作用范围不同，别混用。

**一、新 FE 尚未上生产。** S2 起不再为旧客户端新增字段别名、缺键默认或其他过渡层；破坏性的 API/DTO 变更可以一步到位。S1 已存在的 wire `cwd` 别名保留。

**二、老数据不迁移、不兼容**（用户 2026-08-31 定调）。范围是**所有现存库，生产库 `:4040` 也不例外**——这版按大版本更新处理，上线时起一个全新的库。所以不存在「dev 可以丢、生产要留」的分级，也不做任何存量行的回填、修复或兼容守卫。

因此本文档不再把「存量行会变成什么样」写成后续切片的约束。S1 的 0077 回填（存量 wave 一律 `attached`、`frozen_at = created_at`）已经合入，**保留为历史记录**：已发布迁移一律不碰，但它的推理不再约束后面的切片。

> ⚠️ 这条**只**豁免「老数据」。**同一次运行内的正确性一条都不打折**：并发、崩溃后重跑、幂等、失败回滚——这些与数据新旧无关，仍然是硬要求。

## 产品契约

- Neige 在 `$HOME/neige-workspaces` 下管理默认工作区。
- Cove 只是命名空间；每个 wave 拥有独立仓库。
- 创建 wave 时默认分配工作区，不要求用户先选目录。
- 工作区在产生工作前可更换；开始工作后永久冻结。
- 用户仓库可以附加，但 Neige 永不初始化、移动或删除它。
- 子 wave 的工作区按父 wave 的 `kind` 分情况：父 managed 则独立分配，父 attached 则继承同一路径。两种都在创建时冻结。

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

### 更换（S3）

实现在 `crates/calm-server/src/workspace_repoint.rs`（判据）与
`routes/waves.rs::repoint_wave_workspace`（三步执行），入口是
`PATCH /api/waves/{id}`，请求体 `{"workspace": {"kind": "managed"}}`。

**请求体给的是 kind，不是路径 —— 刻意的。** managed 路径是
`<root>/<cove_id>/<wave_id>`，由服务端派生。接受调用方给的路径会造出 S5 回收守卫 2
（深度必须恰好两层）拒收的行，也就是**结构性泄漏**——而那条守卫的注释里写的理由正是
「S3 是会往 `workspace_path` 写任意路径的那一片」。所以 S3 不写任意路径：它重新派生。
`WaveWorkspacePatch` 做成单字段结构体而不是裸 enum，是为了以后 `managed → attached`
能在同一处加 `path` 而不改已发布的形状。

**因此 managed → managed 的可观察语义是「重置」而不是「搬家」**：旧目录整个 rename 进
`.trash`，在派生路径上物化一个全新的空仓库，spec harness 以新 cwd 重开线程。
派生路径与旧路径通常相同（`CALM_WORKSPACE_ROOT` / `$HOME` 没变时一定相同）。
这一条在写实现时才浮现，与「更换工作区」的字面读法有出入，**留给下一轮评审裁决**，
见文末「S3 的一处存疑」。

**PATCH 的工作区字段与其它字段互斥（400）。** 重指是「两个事务夹着一次文件系统移动」，
不是列写入；混在一起会让部分失败（标题改了、工作区没改）在 wire 上和成功无法区分。
同理，改工作区是 user-only（与 #985 给 `automation_policy` 的口径一致，而这个动作更具破坏性）。

**三步执行，缺一不可。** SQLite 事务对文件系统零隔离，「事务内查一次」关不掉检查与
`rename` 之间的窗口：spec harness 从第一条消息起就是 `workspace-write` 且此刻**刻意
没有冻结**，dispatcher 还会主动推 observation 开启新 turn。

1. **真栅栏，与判据同一个 `BEGIN IMMEDIATE`。** 把该 wave 全部
   `state IN ('starting','running','idle','turn_pending')` 的 `worker_sessions` 行标成
   `superseded`——这正是 `dispatcher::harness_runtime_id_for_spec_card` 读的那条状态
   （经 `session_projection_active_for_card`），提交之后 push 无处可落。
   **interrupt 不算栅栏**：它是异步的，而且对「下一个 turn」什么也没说。
   紧接着做内存那一半（`HarnessRegistry::remove` + `shutdown()`），因为
   `maybe_issue_turn` 不读任何持久状态，否则一条提交前就入队的 observation 照样会变成 turn。
   口径是「该 wave 的全部活跃 runtime」而不是「spec harness」：worker runtime 已被判据蕴含
   （取租约会加 worktree，被 `worktree list` 那条挡下），但 terminal runtime **不**被蕴含
   （见 N17），这正是宽口径值钱的地方——它不依赖任何一条「今天恰好如此」的推理。
2. **移动前重跑判据。** 栅栏与移动之间任何写入都让整次 PATCH 变成 409，且**什么都没动**：
   盘上没动、列没改。唯一残留是 spec harness 被拆了，所以这条路径会在**旧路径上**把它重开再返回
   409——和 `POST /api/cards/{id}/reset` 每天做的是同一个操作，harness item 按 card 持久化，
   用户的历史不受影响。
3. **移动本身走 S5 的唯一受控入口** `workspace_recycle::recycle_wave_workspace`，
   于是 `kind == Managed`、canonical 前缀、`<root>/<cove>/<wave>` 深度、所有权标记四条守卫
   与「rename 不 `rm -rf`」「EXDEV 硬失败」「落点复验」全部免费继承。
   `PathMissing`（从未物化）放行；其余任何拒绝都是硬错误并回滚到旧路径上重开 harness——
   守卫在此刻拒绝意味着行与盘的说法不一致，这时写新 `workspace_path` 会造出没人能再命名的孤儿目录。

**重开线程必须 `force_new_thread: true`。** 这是唯一会重新读 `cwd` 的机制
（`spec_harness_start_adapter.rs`：resume 分支复用 `runtime.thread_id` 且根本不再发 cwd）。
`reset_harness_items: false`：harness item 按 **card** 持久化，重开线程丢的是 agent 的
thread 内上下文，不是用户看得见的历史。幂等键 `None`，与所有非 launchpad 的
`spec-harness-start` 一致；带路径摘要的键只有 launchpad 与 child bootstrap 需要，因为只有它们会被同键重驱。

### 冻结（S3）

`wave_workspace_freeze_tx` 是唯一的关栓函数，`wave_workspace_write_tx` 里加了
`AND workspace_frozen_at IS NULL` 作为门栓本身（S1 特意留空的那一处，理由见该文件）。
四个冻结点都落在**真正的底层写入口**，不靠调用点枚举：

| # | 冻结点 | 位置 | 为什么这里就不可重锚 |
|---|---|---|---|
| 1 | 首次 workspace lease | `operation/workspace_lease/mod.rs::acquire_workspace_lease_at_path_tx` | 租约行存绝对路径，而 worktree 与仓库靠 `<wt>/.git` 与 `<repo>/.git/worktrees/<n>/gitdir` 两个绝对指针互指，rename 之后双向悬空且无人重锚 |
| 2 | terminal 持久化 | **未做，推到 S6** | 见下 N17 |
| 3 | wave 离开 Draft | `calm-truth/db/sqlite/wave.rs::wave_update_tx` | 判据是 `w.lifecycle != Draft` 而不是「本次 patch 发生了转换」：只在转换上触发会漏掉所有转换发生在本片之前的行 |
| 4 | child wave 创建 | S4 已做（`ManagedFrozenUnder` / `InheritAttachedFrozen`） | 机器在 spec 运行中建的，harness 立刻 bootstrap，没有可安全重指的窗口 |

**N17：冻结点 2 没做，两条实测理由。** 先写了、跑门禁才发现，如实记下：

1. **今天还不吃紧。** terminal 的 `cwd` 来自请求体或 `default_cwd()`
   （`operation/terminal_adapter.rs`），**从来不读 `waves.workspace_path`**。
   让 terminal 落进 wave 工作区的是 S6；在那之前，重指工作区不可能让任何 terminal 失效。
2. **在 terminal 的事务里写 `waves` 会死锁。** 实测：把冻结放进 `terminal_create_tx` 之后，
   `claude_card_endpoint::post_claude_restart_recreates_missing_terminal_row_and_resumes_session`
   永久挂在 `sqlx_sqlite::statement::unlock_notify::wait`（gdb 抓到）。内存库跑在 shared-cache 模式，
   锁是**表级**的，这条流程里另有连接占着 `waves`。同一个事务里对 `waves` 做 `SELECT` 正常返回，
   `UPDATE` 永不返回。把冻结留在那里等于用一个洞换一次挂死。

    N17 有测试钉住（`wave_workspace_repoint::a_terminal_card_does_not_freeze_the_workspace_yet_n17`），
    并且**同时断言了理由 1 的前提**（terminal 的 cwd ≠ wave 工作区路径）——所以 S6 一旦让
    terminal 落进工作区，这条测试就会红，必须显式替换而不是顺手改绿。

**system cove 在冻结函数内部被排除，不是在调用点。** launchpad 自带 terminal 卡、也会取租约，
按上表它每次 boot 都会被冻上，而 `today_launchpad_ensure_tx` 下一次 `ensure` 就会撞上门栓
→ 500 → Today 面板永久死亡。排除写成一条 SQL 子句而不是三处 `if`，理由和门栓本身一样：
复制到三个模块的例外，会在第四个地方被忘记。

### 子 wave 的工作区（D7，S4 修正）

**这条原先写错了，S4 改正。** 原文是「子 wave 必须拥有独立工作区，不能继承父 wave 的路径」，无条件。它的**依据**只有一个：删除子 wave 会 `rm -rf` 掉父 wave 的仓库。而这个事故**只对 managed 父成立**——回收只碰 `kind = managed`，attached 路径永不创建、移动或删除。结论比前提宽，代价是把功能写坏了：attached wave 的子 wave 会拿到一个空的 managed 仓库，看不见父 wave 的代码，而「把代码 wave 的活拆给子 wave」正是子 wave 的典型用法。

改成按父的 `kind` 分情况：

| 父的 kind | 子的工作区 | 为什么安全 |
|---|---|---|
| `managed` | 派生 `<root>/<cove_id>/<child_id>`，`managed`，创建时冻结 | 两行共用一个 managed 目录会让回收删掉父仓库，所以必须分开 |
| `attached` | 继承父的路径，`attached`，创建时冻结 | 回收只处理 managed；attached 目录服务端永不动。**多个 wave 指向同一个 attached 仓库是合法状态**，与库的新旧无关：同一个 checkout 被几个 wave 打开是常态用法 |

由此，「没有两行 wave 共享同一路径」这条不变量**只约束 managed 路径**。不收窄的话它会把上面那个既有状态判成违规。

两种情况都在创建时冻结：子 wave 是 spec 运行中机器创建的，harness 立刻在这个路径上 bootstrap，没有可以安全重指的窗口。

## 生命周期

- 删除 wave：只回收 managed 目录，并先验证路径位于 workspace root。
- 删除 cove：回收其所有 managed wave 目录和空的 cove 目录。
- attached 路径永不移动、初始化或删除。
- 归档不回收；长期磁盘回收需要独立策略。
- 子 wave 创建时冻结工作区：父 managed → 分配独立 managed 仓库；父 attached → 继承父的 attached 路径（见「子 wave 的工作区」）。
- fork/template 复制报告，不继承源 wave 的工作区。

回收必须通过单一受控入口；任何直接递归删除都不得接受未经类型和根目录校验的路径。

### 回收（S5）

前四片关于「标签不能撒谎」「managed 必须在托管根下」「canonicalize 而非词法前缀」「所有权标记」的全部工作，都是为了让这一片敢删。实现在 `crates/calm-server/src/workspace_recycle.rs`，是全树唯一移除 wave 工作目录的地方。

**四条守卫，全部满足才动，缺一不动。**

1. `workspace.kind == Managed`。权限来自类型化的列，绝不从路径猜。
2. `fs::canonicalize(path)` 在 `fs::canonicalize(workspace_root)` 之下，**并且深度恰好是 `<root>/<cove_id>/<wave_id>`**。
   - 不是词法 `starts_with`——S2 实测符号链接能让词法检查通过而真实字节在根外。根本身与 `.trash` 之下同样排除。
   - 深度这半条是 S5 红队 R1/R2 补的：只查「在根之下」时，`<root>/<cove_id>/` 这层只要带一个有效标记，**整个 cove 目录连同兄弟 wave 的仓库**一起进 trash（实测兄弟仓库消失）；任意更深的子目录同理。今天这两种形状只被守卫 3「恰好」挡住——没有任何东西往那些深度写标记——那是巧合不是守卫，而 **S3 正是会往 `workspace_path` 写任意路径的那一片**。`remove_empty_cove_dir` 本来就断言自己的深度；移动整棵树的回收路径不该是两者中更弱的那个。
3. `<path>/.git/neige-workspace` 存在，且内容等于**该 wave 的 id**。「是不是 git 仓库」不是替代品。
4. 所属 cove 不是 system cove（launchpad 由内核维护）。**这条今天整条都不可达，是纯纵深，不是「一半可达」**：`Some(System)` 被两条路由的 403 挡在前面；`None`（读不到 cove 行）也不可能——`waves.cove_id` 是 `NOT NULL REFERENCES coves(id) ON DELETE CASCADE`（`0001_init.sql`）且连接开着 `PRAGMA foreign_keys = ON`，「有 wave 行没有 cove 行」不是可表达的状态。保留它的理由是纵深：路由的 403 是边界策略，这条是不可逆移动前的最后一道，将来任何绕过路由的内部调用方免费获得。它的单违规 fixture 只在单元套件里，构造的是数据库不会产生的状态。

守卫 4 在**行**这一层有对应的一半：`DELETE /api/waves/{id}` 对 system cove 的 wave 返回 403（`DELETE /api/coves/{id}` 本来就有）。此前这条路不对称——删掉 system cove 的 wave 行、返回 204，而目录（正确地）留着。**那个组合才是真正的泄漏**：回收要靠 wave 行来命名目录，行一没目录就永远不可达，于是每一轮「删 launchpad + `ensure` 重铸」都多攒一个孤儿仓库。同一条不变量，两层都要有。

**403 的口径是整个 system cove，不是只有 launchpad——刻意的**（2026-09-01 裁决）。system cove 里还有 `ensure_workflow_templates` seed 的 3 个 workflow template wave，它们从此经 API 也不可删。接受：它们同样是内核 seed、boot 会重建，删除从来不是有意义的用户操作；而另一条路（给 `purpose = launchpad` 开特例）会让「system cove 是内核所有」这条不变量带上例外，**例外正是这条设计线上反复出事的形状**。一条宽而无聊的规则比三行 seed 数据的可删性值钱。

**任何一条判不出来（读不到、解析失败、`canonicalize` 失败、cove 行读不到）都算不满足。** 没有「老行没有标记就放行」这类兜底：按§前提二，那种行不存在，兜底只会是个洞。

**拒绝回收目录 ≠ 拒绝删除行。** 守卫不成立时返回「拒绝」并留下目录，DB 行照删。反过来会让丢了标记的 wave（缺口 N5）永久不可删，那比漏一个目录更糟，还会把用户逼去手动 `rm -rf`。拒绝一律打 `error` 日志，泄漏是可见的。

**移动，不删除。** 回收动作是 `rename` 到 `<workspace-root>/.trash/<wave_id>-<ts>`，不是 `rm -rf`。这样「某条守卫将来被削弱」的后果从「用户仓库没了」降级成「trash 里多个目录」。跨设备 `rename`（`EXDEV`）直接报错：copy + delete 是穿了马甲的递归删除，会把这条设计的全部价值退回去。

**目标也要校验，不能假设**（S5 红队 R6/R11，一条断言封两个洞）。四条守卫只证明了工作区从哪来，对它去哪一个字都没说：

- `.trash` 是**符号链接**时，`create_dir_all` 跟着走，工作区落到托管根之外，而 `gc_trash` 会 canonicalize、从此再也找不到它——永久泄漏，而且是静默的，回收本身还报成功；
- `wave_id` 直接插进条目名，**完全没有校验**：含 `../` 的 id 能把 rename 引到根外任意位置（实测落在 `<root>/../escaped-…`）。今天封闭只是因为 wave id 恰好是 uuid-simple——又一处「靠巧合封闭」。

做法：`create_dir_all` 之后 canonicalize trash root，要求它是托管根的直接子项；再要求每个候选条目是那个 canonical trash root 的直接子项。两条都是硬错误，不是拒绝——目标不对时没有「留在原地」这个安全选项可选，只能中止。

**但这两条只封住 R6 的静态形状，封不住时间窗**（红队 R22，200 次里第 2 次复现）。canonicalize 与 `rename` 之间把 `.trash` 换成符号链接，内核会在 rename 时重新解析候选路径：工作区落到托管根之外，而返回值仍报 `Trashed { to: <root>/.trash/… }`。

威胁模型不高——能在托管根里造这个符号链接的人，本来就能直接把目录删掉——所以不上 `openat(O_NOFOLLOW)` + `renameat`。**但返回值撒谎必须消掉**：「静默永久泄漏且报成功」严格劣于失败，因为下游（GC、看日志的人）没有任何办法察觉。因此 rename **之后**再 canonicalize 一次落点，要求其 parent 等于 rename **之前**解析出来的那个 trash root；不符则尽力把目录移回原处，并且无论移没移回都返回硬错误。

这是检测不是防御，防御是 `renameat`，缺口登记为 N16。

**顺序。** 回收发生在 teardown 之后、删行事务之前。teardown 已经停掉所有 harness 与 terminal，没人在写；行还在，所以 rename 失败就整个 DELETE 失败、目录与行都完好、可重试。反过来（先删行）会把 rename 失败变成「wave 没了、仓库还在」——不可重试，只能人工。

**这条顺序换来的另一半，如实记下。** rename 成功之后删行事务失败时，现状是**行还在、目录已进 trash**：wave 在 UI 上还在，它的仓库不在原处了。重试 DELETE 会走通（目录已不在原路径 ⇒ 判为 `PathMissing` ⇒ 放行删行），worker 取 lease 时 S2 的 ensure-materialize 会在原路径重建一个空仓库；旧内容 7 天内可从 `.trash` 手工取回，但**没有任何重新指向的机制**。`recycle_cove_workspaces` 中途失败同理，会留下「一部分 wave 的目录进了 trash、cove 行还在」的部分状态。

接受这个取舍，理由是两害相权：这一半的后果是「数据在 trash 里、可取回」，而反序那一半的后果是「行没了、目录永远不可达」（回收要靠 wave 行命名目录）。没有为它加测试固定行为，因为要固定的不是一个正确行为而是一个已知的不完美窗口；真解是把 rename 纳入与删行同一个可恢复的操作记录，那属于 S3 的重指向机制，不是本片。

**cove 目录用非递归 `remove_dir`。** 只在真正为空时成功，于是「是不是每个子目录都回收了」成为内核无法搞错的前提，而不是一句断言。有任何一个 wave 被拒绝，cove 目录就留着——这是正确且可见的结果。

**`.trash` 的 GC：按时间，7 天，每次回收时顺带清扫。**

- 选时间不选数量：保留窗口要扛的是「有人发现删错了」，而人是按日历发现的。数量上限（留最近 20 个）会在脚本连删 20 个 wave 时把今早的误删挤掉，又会在安静的实例上把一年前的目录永久钉住。
- 条目的年龄按**名字里的时间戳**判定，不看 `mtime`：`rename` 保留 mtime，而工作区的 mtime 通常远早于它被回收的时刻，按 mtime 扫会把刚回收的工作区立刻删掉——对最值得留的仓库等于没有保留窗口。名字因此固定为 `<wave_id>-<ts>`，同毫秒冲突靠 `ts + 1` 而不是加后缀，以免破坏解析。
- 判不出年龄、不是真目录（含符号链接）的条目一律**保留**。这里的 fail-closed 同样是「不删」。
- GC 是独立的一步，且是本设计里唯一调用 `remove_dir_all` 的地方；它自己校验条目是 canonical `<root>/.trash` 的直接子项。挂在回收动作上而不是新起后台任务：trash 只在回收时增长，所以随回收清扫就足以把它限制在一个保留窗口的删除量内。

**不做**：

- 归档（`archived_at`）不回收，长期磁盘回收仍是独立策略。
- **GC 只在回收动作上触发**，所以一个之后再不删任何 wave / cove 的实例，最后那批 trash 条目会无限期留着。这是磁盘上的一条明确边界，不是 bug：上界是「最后一次删除时窗口内的那批工作区」，有界且可预期。要拿掉它得引入定时任务或 boot 清扫，本片不做。
- 缺口 N4/N5/N7/N9/N10 保持钉住。

## Cove 对话与执行器

没有 attached folder 的 cove 仍可创建对话；它使用默认 managed 工作区。已有 folder claim 的 cove 可以继续采用 attached 语义。

Managed 仓库保证 Codex 能获得 Git 工作区。Claude 当前不读取 wave workspace，迁移到同等 worktree 租约是独立工作，不属于本设计。

Managed 仓库默认没有 remote；需要操作真实代码仓库时，用户应在创建时 attached，或在冻结前从 managed 切换到 attached。

## 交付顺序

1. S0：失败原因进入 task 状态与前端。
2. S1：typed workspace、唯一存储、wire 类型（0077 的一次性回填已合入，按前提二保留为历史记录，不再作为后续切片的约束）。
3. S2：托管根、物化、所有创建入口默认 managed。
4. S4：子 wave 工作区按父 kind 分情况并冻结。
5. S5：安全回收和根目录断言（四条守卫 + trash + GC，见「回收（S5）」）。
6. S3：工作区更换、冻结、harness 重锚定、FE attached 入口。
7. S6：terminal 默认落在 wave 工作区。

S4 必须不晚于 S5；S3 必须复用 S5 的路径安全边界。

## 已知缺口

实测可达、被刻意推迟的缺口。N4–N11 每条都有一个断言缺口本身的测试（`nX_` 命名或带 `KNOWN GAP (#1147 nX)` 文案），所以修好它的那一片会看到测试变红，必须显式替换而不是顺手改绿。

S5 新增的三条按同一标准登记，但状态不同，别混：**N14 有测试**（`wave_workspace_recycle::a_managed_workspace_without_our_marker_is_left_on_disk` 同时断言目录还在 **和** 行已删）；**N12、N13、N15 没有测试（N16 **有**测试：`a_trash_swapped_between_canonicalize_and_rename_is_not_reported_as_success`，用确定性注入而不是多线程 hammer）**——N12 今天没有任何路由触达，N13 藏在守卫 3 后面，N15 要跨线程时序才能构造，三条都只能靠这张表记住。写测试之前它们只是记录，不是防线。

| # | 缺口 | 后果 | 归属 |
|---|---|---|---|
| N4 | 挪动 `CALM_WORKSPACE_ROOT` / `$HOME` 后，既有 managed wave 的存库路径不再落在配置根下 | 该 wave 取 lease 永久硬失败，**无迁移路径**。launchpad 会自愈（路径每次 ensure 重新派生），普通 wave 不会 | 回收/搬迁片 |
| N5 | 我们自己的工作区丢了 `.git/neige-workspace` 标记（备份部分还原、误清理） | 永久拒绝，且没有任何管理入口能重新认领 | 回收片 / 终端片 |
| N7 | 符号链接是「拒绝，但已经写过了」 | 根外留下一个带我们标记的完整仓库，无人回收。先拒后写不可避免：真实位置要 `create_dir_all` 之后才知道 | 回收片 |
| N9 | 物化互斥是**进程内**的 | 两个 calm-server 实例（升级重叠、admin CLI、健康重启）仍会撞出锁残留。清理逻辑让它可自愈而非永久砖化，但撞击本身还在；真解是文件锁 | 回收片或独立 issue |
| N10 | 环境隔离只到物化边界 | 紧邻的 `git_repo_root_for_wave_cwd` 与 worktree 相关 spawn 仍是裸 `git` 调用，同一组变量对它们仍然有效 | 独立 issue |
| ~~N11~~ | S2 的 adapter 会**持久化** `kind = managed` 且路径等于父目录的 child 行 | 见下 | **代码已由 S4 修复；存量行按前提二不迁移** |
| N12 | `Repo::wave_delete`（`calm-truth/src/db/sqlite/session_repo_impl.rs`）删 wave 行**不经过回收** | 该行的 managed 目录从此不可达（回收要靠 wave 行来命名它），永久孤儿。今天没有任何路由走它，所以后果是泄漏而非丢失 | 独立一片：Repo 层拿不到 workspace root，要封闭得把 root 下沉到 Repo 或给这个方法加一个**必传**的回收回调；「回收必须通过单一受控入口」这句在写入侧因此还不是全封闭的 |
| N13 | 符号链接**指向根内**时，回收 rename 的是 canonical 目标，存库路径上留下一个悬空符号链接 | 泄漏，不是丢失。守卫 3 几乎必然先拦下（目标的标记名字不对），这条是它万一没拦下时的残留形态。N7 的形态不变 | 回收/搬迁片 |
| N14 | 守卫拒绝时**目录留在盘上**（行照删） | 刻意取舍，不是 bug：见下 | — |
| N15 | 回收**不取**物化用的那把 per-path 进程内互斥锁 | 一次回收与同一路径上的一次物化并发时，物化可能在刚被 rename 走的路径上重建目录，或回收撞上物化的中间态。后果是泄漏（trash 里一个目录 + 原路径一个新空仓库），不是数据丢失——rename 是原子的，两边都不会删东西 | 与 N9 同源（真解是跨进程文件锁），归同一片 |
| N16 | canonicalize trash root 与 `rename` 之间的 TOCTOU：`.trash` 被换成符号链接 | 工作区被 rename 到托管根之外。**已由 rename 后的复验兜住**——检测得到、报硬错误、尽力移回，所以后果是「一次失败的 DELETE」而不是静默泄漏；未做的是防御（`openat(O_NOFOLLOW)` + `renameat`） | 独立 issue；威胁模型不高（能造这个符号链接的人本来就能直接删目录） |
| N17 | 冻结点 2（terminal 持久化）没做 | 建了 terminal 卡的 wave 仍可更换工作区。今天无害（terminal 的 cwd 不来自工作区），S6 让 terminal 落进工作区之后就是真洞 | S6；**有测试**（`a_terminal_card_does_not_freeze_the_workspace_yet_n17`，同时钉住「无害」的前提）。实现时注意：在 terminal 事务里 `UPDATE waves` 会因 shared-cache 表级锁死锁 |

**N14：拒绝回收目录 ≠ 拒绝删行，这是刻意的。**

守卫不成立时 S5 留下目录、打 `error` 日志，但 DB 行照删。反过来（守卫不成立就让 DELETE 失败）会让丢了标记的 wave（N5）永久不可删，把用户逼去手工 `rm -rf` ——那比漏一个目录更糟，而且方向从「泄漏」翻成「用户自己动手删」。所以这条泄漏是**买来的**，不要有人把它当 bug「修」成拒绝删行。真要收，收在 N5 上（给一个重新认领标记的管理入口），不是收在这里。

**N11：代码在 S4 修好；存量行不迁移。**

这类行只可能由「跑着 S2 代码的实例去派子 wave」产生。按前提二，带着这种行的库不迁移——上线起新库。因此本片不带任何 boot 修复或回填：为老数据在每次启动时扫一遍 `waves` 表并可能改写行，是净增的风险面。

代码侧 S4 从两处消灭它，缺一处都不够：

- **adapter**：父 managed 的子走 `ManagedFrozenUnder`，路径由自己的 id 派生，物化时所有权标记写的是**子 wave 的 id**。同时删掉了 `InheritFrozen` 这个计划变体本身——它复制父的 kind **和** 路径，只要它还在，「两行 wave 共用一个 **managed** 目录」就仍是可构造状态；取而代之的 `InheritAttachedFrozen` 只能产出 `kind = Attached`，够不着回收路径。
- **operation result**：scheduler 的 child bootstrap 从 **operation 的持久 result** 取 cwd，从不重读 wave 行；只改 adapter 不改 result，子 wave 的 harness 仍然锚在父目录上。

bootstrap 的幂等键带上了路径摘要，理由与 S2 给 launchpad 加摘要的完全一样：载荷含 cwd，运行时对「同键不同载荷哈希」是永久拒绝，而 operation 行从不删除——将来 S3 的工作区 PATCH 重指过的子 wave 否则会在下一次 re-drive 上永久失败。这条属于「同一次运行内的正确性」，不受前提二豁免。

「没有两行 wave 共享同一 **managed** 路径」由 `today_launchpad.rs::no_two_waves_share_a_managed_workspace_path` 全表钉住，由真实创建入口驱动（launchpad ensure、`POST /api/waves` 的 managed 与 attached、child adapter 的两种父），并配一条单违规 fixture。

顺带记一条实测：假如那种行真的存在，它其实**早已是砖**而不只是「将来会被误删」——worker 取 lease 时用该 wave 自己的 id 去物化它的 managed 工作区，落在父目录上时所有权标记不匹配，直接报 `is the managed workspace of wave <parent>, not <child>`，codex worker 根本起不来。

## S3 的一处存疑（未自行改设计，交评审裁决）

**`managed → managed 重新分配` 在派生路径不变时，观感是「重置工作区」而不是「更换工作区」。**

设计 §更换与冻结 允许 `managed → managed` 与 `managed → attached`，交付顺序把 S3 定成
「工作区更换、冻结、harness 重锚定、FE attached 入口」，而 #1147 的评审留痕里 D6 那行写的是
「删旧目录 + 建新目录，全在 `<workspace-root>` 前缀内」。实现时才发现这三句合起来有一个空洞：

* managed 路径由 `<root>/<cove_id>/<wave_id>` 派生，wave 的 cove 不可变、id 不可变
  ⇒ 重新派生**必然得到同一个路径**（除非 `CALM_WORKSPACE_ROOT` / `$HOME` 变了，而那是被钉住的 N4）。
* 让调用方给路径可以让它真的「换」，但 S5 回收守卫 2 要求深度恰好 `<root>/<cove>/<wave>`，
  任何别的路径都会变成**回收拒绝 ⇒ 结构性泄漏**；而那条守卫存在的理由，注释里写的就是防 S3。

所以本片选了「服务端派生」，代价是：判据（盘上必须是空的）成立时，重置一个空仓库在盘上
几乎是恒等变换。真正被这一片买下来的是**机制**——栅栏、重检、复用 S5 的移动边界、
harness 以新 cwd 重开——而 `managed → attached`（把 wave 指到用户真仓库）才是这套机制唯一
有实质位移的消费者，它今天被 §不做 挡在外面。

两条可能的收敛方向，本片不擅自选：

1. **接受**：S3 的产品价值就是 FE 的 attached 创建入口 + 冻结门栓，PATCH 是为
   `managed → attached` 铺的机制，下一片接上。
2. **扩到 `managed → attached`**：那才需要 `cove_folders` 认领规则、目标校验
   （绝对路径 / 目录存在 / 是 git 仓库，见 §托管工作区，且该校验今天**并未实现**——
   `AttachedFromCwd` 原样收下 `cwd`）与 kind 变更，属于新的一片。

## 验收重点

- 新 managed wave 能直接创建第一个 worktree。
- 物化失败时 create 返回非 2xx，且不留下可见 wave。
- 删除 attached wave 不触碰用户仓库。
- 父 managed 时子 wave 路径与父不同，删除子 wave 后父仓库仍可用；父 attached 时子 wave 与父同路径且用户目录零改动。
- 全局 Git 签名、模板和 hook 配置不会影响物化。
- 工作区有普通文件、ignored worker 产出、其他分支提交、stash、活 worktree 或 terminal 时，更换均被拒绝。
- 活跃 turn 与更换并发时不能在 trash 中继续产生产出。
- 任意回收或 rename 传入 workspace root 外路径时失败。
- GET wave 与 Today/launchpad 路径均只从 `workspace_path` 读取并派生 wire `cwd`。
