# 设计：wave 工作区 —— default 语义 + 托管根 + 一次性冻结

> 归属：#1147（worker 工作区隔离）。相关：#1149（pending 不可见 / 失败不可读）、#1131（建 wave 只填名字，遗留 `cwd=$HOME`）、#1098（cove 对话）、#275/#1109（`cove_folders`）。
> 状态：**r3.3 — 实现中**（S0 已合入 #1158；S1 评审回灌了两条：system wave 的冻结例外、元测试必须列名驱动）。

## 1. 问题

今天一个 `waves.cwd: TEXT NOT NULL` 字符串同时承担三件互不相同的事：

| # | 语义 | 今天靠什么 | 现状 |
|---|---|---|---|
| A | spec/对话 agent 的 cwd | `waves.cwd`，建后不可改（`wave_update_tx` 的 UPDATE 语句根本没有 cwd 列，`db/sqlite/wave.rs:177-192`） | #1131 之后新 FE 不传 → `default_cwd()` = `$HOME` |
| B | 每个 worker 的隔离工作区 | codex：`git rev-parse --show-toplevel(wave.cwd)` → `<repo>/.claude/worktrees/<wave>/<card>` + 分支 `neige/<wave>/<card>`；claude：**纯相对路径**，**完全不读 wave.cwd**（`claude_adapter/mod.rs:774-781` → `workspace_lease/mod.rs:1357-1363`） | claude 那条是 pre-3c 遗留，无隔离、无分支、cwd 靠 spawn 继承 |
| C | 哪个 cove 拥有哪个目录 | `cove_folders(path UNIQUE, cove_id)` + 建 wave 时认领/409 | #1131 的省略-cwd 分支完全绕过它 |

后果（#1147 实测）：新建 wave 的 cwd 是 `$HOME`，不是 git 仓库 → 任何 `kind: codex` 任务在
`git_repo_root_for_wave_cwd`（`mod.rs:1386`）以 `BadRequest` 死掉，`tasks.status_detail` 只剩
`spawn-failed`，真实文本停在 `operations.phase_detail_json`。

线上事实（2026-08-30 读库）：`:4040` 的 `cove_folders` 7 条全是用户真实项目目录，9 个 wave 的 cwd 全落在其上；
`:4140`（新 FE）3 个 wave 的 cwd 是 `/home/kenji`、`/tmp`、`/` —— 即 #1131 留下的坑。

## 2. 产品意图（用户定调，不可推翻）

1. neige 在 home 下自建一个工作文件夹放数据；wave 工作区默认落在那里。
2. cove 那层文件夹**只是命名空间**，不是 git 仓库；每个 wave 自己是一个仓库。
3. 工作区是 **default 语义**：建 wave 不问用户，缺省就分配一个；**在还没发生任何工作之前可以改**；
   一旦开始工作，固化为实际值，**不可变**。

## 3. 决策

### D1 — workspace 是带类型的字段，不是一个路径字符串

wave 上新增（`calm-types::Wave` + 读 DTO + `WavePatch`）：

```rust
pub enum WaveWorkspaceKind {
    /// 服务端在托管根下创建、独占、可回收。
    Managed,
    /// 用户指向的既有仓库。永不删除、永不 git init。
    Attached,
}

pub struct WaveWorkspace {
    pub kind: WaveWorkspaceKind,
    /// 绝对路径。
    pub path: String,
    /// 一次性、单调。Some ⇒ path 与 kind 均不可再改。
    pub frozen_at: Option<i64>,
}
```

理由：托管目录和用户仓库在**下游行为**上是两种东西 —— 前者 wave 删除时可以回收，后者一根汗毛不能碰。
只留 `cwd: String` 的话拆除路径无法区分二者，那是 `rm -rf` 级别的事故面。
`waves.cwd` 保留为 `workspace.path` 的**投影**（旧客户端、terminal、`task_verify_adapter` 都还在读它），不新增第二真源。

**r3.2 补一条不变量（原文只说了意图，没说成约束）**：列还在 ⇒ 每个写者都必须同时写两处，否则漂移。
所以 —— **`waves.cwd` 只能由写 `workspace.path` 的那一个函数写，不允许任何其它写点直接碰它。**
§5 测试 4 的 `UPDATE waves` allowlist **显式承担这条**，不只服务于 `freeze_workspace_tx`；
allowlist 里每一项要标明它是「冻结写点」还是「path 写点」，两类都不许有第三个成员。

> r1 有 `materialized_at: Option<i64>`，**已砍**：D5 选定「建 wave 即物化」后它恒为 `Some(created_at)`，
> 是一个永远不为 `None` 的 `Option`（`feedback_required_over_option` 的反模式）。

### D2 — 托管根

新增典型配置字段（与 `data_dir` 同一模式，clap + env）：

```
--workspace-root / CALM_WORKSPACE_ROOT，默认 $HOME/neige-workspaces
```

**不复用 `CALM_DATA_DIR`**（`~/.local/share/neige-calm`）：那里被定义为 runtime state（socket / db / scratch），
会被 reset 清；工作区是用户要用编辑器打开、要备份的产出。本机 `~/neige`、`~/neige-calm`、`~/neige-calm-wt`
已被占用，故默认名取 `neige-workspaces`。

布局（cove 层纯命名空间，**目录名一律用 id，不用 slug**）：

```
<workspace-root>/<cove_id>/<wave_id>/                                     ← wave 仓库根
<workspace-root>/<cove_id>/<wave_id>/.claude/worktrees/<wave>/<card>      ← worker 租约（现有代码原样成立）
```

> r1 用「标题 slugify + id 前 8 位」，**已砍**：D2 同时规定「重命名不移动目录」，所以标题一改 slug 就撒谎；
> 可读性收益只在 `ls` 的一瞬间，代价是 unicode/长度/冲突的 slugify 实现加一份文档债。可读性交给 FE（它有 title）。

托管路径**不写 `cove_folders`**：它由 cove 派生、天然独占；`cove_folders` 继续只管「用户真实目录归哪个 cove」这条 attached 路径。

### D3 — 物化（materialize）

在**建 wave 时**执行（见 D5），幂等：

1. `mkdir -p <path>`；目录必须不存在或为空，非空且非本 wave 所有 → **硬失败**，绝不复用。
2. ```
   git -c init.templateDir= -c init.defaultBranch=main init <path>
   ```
   ⚠️ `-c` 必须在子命令**之前**：`git init -c ...` 在 git 2.39.5 上报 `未知开关 'c'`（r1 语法错误，已实测）。
3. **建一个空的初始提交**：
   ```
   git -C <path> -c commit.gpgsign=false -c user.name=neige -c user.email=neige@localhost \
       commit --allow-empty -m "neige workspace init"
   ```
   ⚠️ 两条实测结论：
   - **没有初始提交，`git worktree add` 直接失败**（`致命错误：不是一个有效的对象名：'HEAD'`，exit 255）。
     少了这步，托管 wave 的第一个 codex worker 仍然起不来 —— 等于白做。
   - **全局 `commit.gpgsign=true` 会让空提交硬失败**（`无法写提交对象`）；而缺 `user.name/email` **不会**失败
     （git 自动派生 `kenji@pivot.local` 并打警告）—— 但会污染产出仓库的作者信息，所以仍要显式给。
     `core.hooksPath` 同理必须压掉。
4. 排除 `.claude/worktrees/` —— **写 `.git/info/exclude`，不要写 `.gitignore`**，直接复用既有
   `ensure_workspace_worktree_root_excluded`（`workspace_lease/mod.rs:1250`，其 `git_exclude_path`(:1302)
   走的就是 `git rev-parse --git-path info/exclude`）。
   ⚠️ r2 写成「写 `.gitignore`」是把机制说错了，而且会**直接废掉 D4 的判据 (2)**：工作区里出现一个未提交的
   `.gitignore` ⇒ `status --porcelain` 恒有 `?? .gitignore` ⇒ 新 wave 从第 0 秒起就判「盘上不是空的」，
   「可改」在 UI 上永不可达（实测）。把它提交进 init commit 也不行 —— HEAD 就不再是空提交，基线计数要跟着变。
   用 `.git/info/exclude` 两个问题一起没有：HEAD 保持真空提交，工作树保持干净。

**物化的挂载点必须覆盖全部 5 个建 wave 入口**（r2 只点了一个，评审补齐）：
`create_wave_structure` 的三个调用方 —— `routes/waves.rs:1017`（`POST /api/waves`）、`:525`
（`seed_workflow_template_wave`）、`:1092-1130`（D10 的 cove chat）—— 外加**完全不经过它**的两条：
`routes/today.rs:89`（裸 `INSERT INTO waves(... cwd ...)`，launchpad wave）与 `child_wave_adapter.rs:191`
（走 `wave_create_tx`）。漏掉 `today.rs` 那条，Today 面板上的 codex 任务会继续以 `spawn-failed` 死掉，
而 S2 的独立价值声明是「新 wave 的 codex 任务能跑」—— 不成立。

**物化失败必须让 `POST /api/waves` 返回非 2xx**，不得走 `waves.rs:1529` 那种 `tracing::warn!` + `Ok(())` —— 否则
wave 照样 201、看起来正常，第一个 codex worker 再次以 `spawn-failed` 死掉，就是 #1147 的原样重演换个位置。

attached 不做任何物化，只校验：绝对路径 + 存在 + `git rev-parse --show-toplevel` 成功 + `cove_folders` 认领/409（沿用今天规则）。

### D4 — 可改的判据：一条冻结闩 + 一条「盘上是空的」

> **r3 重写。** r2 的「wave 上存在未完成 operation 就 409」被双路从两个不同角度证伪，已删除：
> - subagent：`ChildWaveOperationPayload` 的字段是 `parent_wave_id` 不是 `wave_id`，`target_from_payload`
>   （`repo_sqlite.rs:846-871`）推不出归属而落成 `("unknown", NULL)`；`prepare_tx` 后 `TxOutput` 又把 target
>   覆写成**子** wave（`child_wave_adapter.rs:351`）。这条闸门**查不到**它要挡的那行。
> - codex：就算查得到也**漏拦** —— operation 早已 `succeeded`，而它交出去的 cwd 还活在 codex thread 与
>   harness handle 里（`spec_harness_start_adapter.rs:646/721/936`）。operation 的生命周期 ≠ cwd 消费者的生命周期。
>
> 更根本的教训：「wave 上的未完成 operation」这个集合本身就是跨 kind 的**逐条枚举**（harness-start 过 pending
> 后 target 变 `card`，task-verify 是 `task`，带 `runtime_id` 的变 `runtime`）—— 正是 r1 被否掉的那个形状换层皮。

r3 的判据分两条。

> ### ⚠️ r3.2 —— 「同一个 `BEGIN IMMEDIATE` 内判定」是不够的（第三方评审，阻断级）
>
> **SQLite 事务对文件系统零隔离。** 它能真正栅住的只有并发的 **DB 写者**（lease 获取、terminal 创建——
> 这些会在同一 tx 内写 `frozen_at`）。而：
> - spec harness agent 被 (1) **刻意排除**在冻结之外（必须如此，否则「可改」当场作废）；
> - D5 选了 (a) ⇒ 它从建 wave 那一刻起就是 `workspace-write`（`spec_harness_start_adapter.rs:723-724`）；
> - dispatcher 会**主动**推送 observation 并由此开启新 turn（线上日志：
>   `dispatcher push: delivering observation to spec harness … kind="task.failed"`）。
>
> 机械推论：
> ```
> 判据(2) 判定为空 → agent 的 turn 写入文件 → rename 到 .trash
>                                          → 进程 cwd 随 inode 走，继续写 .trash
>                                          → S5 的 GC 回收，产出无声蒸发
> ```
> 动作 0「先 interrupt 活跃 turn」**不是充分条件**：interrupt 是异步的，且没有任何东西阻止一次
> **新** turn 开始 —— 一条 `task.completed` / `task.failed` 推送就够。
> 这条竞态丢的恰恰是 rename **之后**才写下的字节，「rename 不 rm」那个 fail-safe 兜不住。
>
> **因此判据的执行形状是（三步，缺一不可）**：
> 1. **真栅栏**：在写新 path 的**同一个 tx 内**把 harness runtime 置为 superseded / parked，
>    让 `dispatcher push` 无处可推。不是「interrupt 然后祈祷」。
> 2. **rename 之前重跑一次判据 (2)**，失败即 409，且不留任何中间状态。
> 3. rename 自身断言 `kind == Managed && path.starts_with(<workspace-root>)` —— 见下 B。

两条判据（在同一个 `BEGIN IMMEDIATE` 内判定，但需叠加上面的栅栏与重检）：

**(1) 冻结闩 `frozen_at`** —— 一个幂等、单调的写入函数 `freeze_workspace_tx(tx, wave_id)`，只在
**出现「不可重锚的持久 cwd 消费者」**时写入：

| 调用点 | 位置 | 为什么 |
|---|---|---|
| 首次取得 workspace lease | `acquire_workspace_lease_at_path_tx`（`workspace_lease/mod.rs:179`） | worker 落盘 |
| **任何 terminal 行创建** —— 调用点**下沉到 `terminal_create_tx` 内部**（`calm-truth/src/db/sqlite/card.rs:613`，全仓唯一的 `INSERT INTO terminals`），不要在四个 `card_with_*_create_tx` 上逐个挂 | `card_composite.rs:34/229/391/508` | **卡片级 cwd 是一份独立持久真源**，重启后 `ws/terminal.rs:158` 直接拿 `term.cwd` 重新 spawn PTY（`calm-proc-supervisor/src/lib.rs:1766`）。活 PTY 不可重锚。⚠️ 挂在四个 composite 上会漏 `claude_restart_adapter.rs:182` —— 它在 terminals 行缺失时**直接调 `terminal_create_tx`** 新建带 cwd 的行，不经任何 composite。下沉到 `terminal_create_tx` 才是真正消灭「逐条枚举」这个形状，未来任何新入口自动覆盖。另需单独处理 `out_of_domain.rs:115` 的 `terminal_create`（走自有事务） |
| lifecycle 离开 `Draft` | `wave_update_tx` 的 lifecycle 分支（`db/sqlite/wave.rs:148`） | scheduler 只调度非 Draft（`scheduler/mod.rs:146-157`） |
| child wave 创建 | `child_wave_adapter`（见 D7） | 机器创建，用户没有理由改它 |
| `today_launchpad_ensure_tx` 收编 legacy wave | `routes/today.rs:77-83` | 那条 `UPDATE waves SET ... cwd=?2` **绕过 PATCH**，必须自己冻 |

**spec harness 线程刻意不在此列** —— 它是**唯一可重锚**的消费者（见下），否则建 wave 当场就冻死，「可改」这个产品意图直接作废。

**(2) 盘上是空的** —— 旧目录必须「除了 init 提交之外什么都没有」。
**r3.1 定型（三条命令都经实测，逐条都有各自要挡的东西）**：

```
git -C <old> status --porcelain --ignored   → 空          # 含被 exclude 的 worker 产出
git -C <old> rev-list --count --all         → == 1        # 含 slice 分支提交与 stash
git -C <old> worktree list                  → 恰好 1 行    # 活租约
```

三条各自的证伪场景（r3 评审实测，缺任何一条都会误判为「空」而把目录搬走）：

| 缺的那条 | 逃逸场景 | 观测 |
|---|---|---|
| `--ignored` | worker 产出全在 `<wave>/.claude/worktrees/<wave>/<card>`，而该前缀在 `.git/info/exclude` 里 | 裸 `--porcelain` **空**；`--ignored` 输出 `!! .claude/` |
| `--all`（用了 `HEAD`） | worker 提交到 slice branch → worktree 被 sweep 掉（**租约的正常终局**）；或 `git stash` | `status` 空、`HEAD` 计数 **1**，`--all` 计数 **2** |
| `worktree list` | 活租约存在时只能靠 `!! .claude/` 间接命中 | 把「有活 worker」变成显式拒绝理由 |

⚠️ **两路评审在 `--ignored` 的行为上给出过相反结论，已亲手实测判定**：
空的 `.claude/worktrees/` 目录 **不会** 让 `--ignored` 非空（干净 / 空目录 / 删文件后留空目录三种情形输出均为 `[]`），
只有目录**含文件**时才输出 `!! .claude/`。因此「加了 `--ignored` 会永远非空、导致永远不可改」的担心不成立。
`--ignored` 会把整个 `.claude/` 折叠成一行、无法按前缀过滤 —— 这不是问题：worker 产出**本来就该**挡住改动。

这条判据的价值仍然是「不问谁可能写过，只问盘上有没有东西」——spec harness 的 `workspace-write` 产出
（`spec_harness_start_adapter.rs:723-724`）与 forge action 在 `wave.cwd` 里的提交（`transport.rs:942`）
都不需要各自登记，天然体现在这三条上。

这条是本设计里唯一不需要枚举的守卫：**它不问「谁可能写过」，只问「盘上有没有东西」**。
它一次性关掉了 r2 漏掉的整类问题 —— spec harness 是 `workspace-write`（`spec_harness_start_adapter.rs:723-724`），
选 (a) 之后 agent 从第一条消息起就能写文件；MCP forge action 的 Spec 分支直接在 `wave.cwd` 里跑 git
（`transport.rs:942`）。这些路径**不需要**各自登记，它们的产出天然体现在这两条 git 判据上。

> **为什么不用 codex 提的 `wave_workspace_bindings` 表**：那要求「所有捕获 cwd 的路径」在同一 tx 内登记 binding
> —— 又是一次全枚举，且 spec harness 在建 wave 那一刻就是活 binding，等于立刻锁死。(2) 用**结果**代替**枚举**，
> 语义更强也更便宜。

**PATCH 通过后的动作**（同一事务 + 事务后副作用）：

0. **先 interrupt 旧 codex thread 的活跃 turn**。Linux 下 `rename` 不会因活进程失败，进程 cwd 跟着 inode 走 ⇒
   老写者会**静默继续往 `.trash` 里写**，等 S5 的 GC 一收，产出无声蒸发。
1. 旧目录 **rename 到 `<workspace-root>/.trash/<wave_id>-<ts>`**，**不 `rm -rf`**。
   这一步让「冻结判定漏了某条路径」从**静默删数据**降级成**留个垃圾目录**。
   ⚠️ fail-safe 的承诺只到「保住文件字节」，**不保 git 历史可访问**：实测 rename 之后
   `<wt>/.git` 与 `<repo>/.git/worktrees/<n>/gitdir` 两个**绝对路径**指针双向悬空，trash 里的东西已经不是一个可用仓库
   （`worktree remove` 报 exit 128，`prune` 行为还不确定）。判据 (2) 的第三条（`worktree list` 恰好 1 行）
   正是为了让这种情况根本不发生。
   `EXDEV`（跨设备）**直接报错拒绝改动，绝不 fallback 到 copy+delete**。`.trash` 由 S5 的 GC 回收。
2. 物化新目录（D3）。
3. **spec harness 以新 cwd 重开线程**（`spec-harness-start` + `force_new_thread: true`）。
   这条路今天真实可用：`/api/cards/{id}/spec/reset`（`routes/cards.rs:121/1303`）就在用，
   `defer_runtime_start` 分支（`spec_harness_start_adapter.rs:453/669/721`）有 live-daemon 测试覆盖
   （`tests/spec_harness_adapters.rs:573/593/972`）。旧 runtime 在同一 tx 内被 `supersede`
   （`session_mirror.rs:285-315`），不会被 boot recovery 复活（`session_projection.rs:512-517`）。
   ⚠️ 但 **app-server 侧没有 thread close API**（`codex_appserver.rs:613` 只有 start/resume/read/list + turn interrupt），
   旧 codex thread 与 `thread_cache` 映射会**泄漏**（`shared_codex_appserver.rs:3165` 只在整体 rebuild 时 clear）。
   危害有限（无人再引用），但这正是第 0 步必须先 interrupt 的原因。
   ⚠️ **r2 说「`handle_state_json` 是第四份 cwd 快照、重开时要刷新」是错的**：`HarnessSnapshot`
   （`harness/snapshot.rs:24-47`）根本没有 cwd 字段，`worker_sessions` 行也不存 cwd —— 那是给实现者的假任务。
   真正的第四份在 **operation payload/result**（`spec_harness_start_adapter.rs:606` 写入、`:646/:722` 消费）。
   要盯的是所有从**旧 operation result** 取 cwd 的重放路径（尤其 `scheduler/mod.rs:1499-1517` 的 bootstrap），
   它们必须改成重读 `waves.path`，否则改完工作区后一次重放就指回 trash。
   另注：`WavePatch` 今天**没有** cwd 字段（`calm-truth/src/model.rs:159`），S3 要新增。

> 被砍掉的 r1 谓词（harness 首条消息 / 非自动卡片）不再需要：前者被 (2) 覆盖，后者被 (1) 的卡片创建行覆盖。

### D5 — spec harness 启动时机：**选 (a) 建 wave 即物化**

r1 推荐的 (c)「路径先定、物化延后」被两路独立证伪：今天不存在「有 cwd 但不落盘」的档位 ——
`force_new_thread=false` ⇒ `defer_runtime_start=false`（`spec_harness_start_adapter.rs:449-453`），
且 `thread_start_mint` 照样发带 cwd 的 `thread/start`（`shared_codex_appserver.rs:1161-1172`），
随后还起 harness run loop（`:920-953`）。(b)「harness 推迟到首条消息」的改动面是四个建 wave 调用点
加上前端「打开就有对话」的观感，属于「窄价值 / 深层次」，本设计不做。

**(a) 的成本被 r1 高估**：一个空 git 仓库几十 KB，`init + commit --allow-empty` < 20ms；有了 D8 的回收，
随手建的 wave 就是可回收垃圾而非永久债。**(a) 还顺带收窄了 D4 的窗口** —— 改工作区变成「删旧空目录 + 建新目录」
的原子操作，不需要跟 operation 做时序推理（再叠加「有未完成 operation 就 409」）。

执行顺序：物化在 `create_wave_structure` 的 tx **之外**、`start_spec_harness` **之前**；失败回滚 wave 行或返回 5xx。

### D6 — 允许的工作区变更：只做 managed → managed

| 转换 | r2 是否支持 | 说明 |
|---|---|---|
| managed → managed（重新分配） | ✅ S3 | 删旧目录 + 建新目录，全在 `<workspace-root>` 前缀内，零「删到用户目录」风险；可加一条**无条件前缀断言** |
| managed → attached | ✅ **r3.2 改判：做**（S3，FE 是唯一真实成本） |
| 创建时指定 attached | ✅ 后端已有；**FE 必须补**；**创建时即写 `frozen_at`**（见 B） | 走既有 `cwd` + `attach_folder` 路径，后端今天就工作且有测试 `tests/cases/wave_cwd_terminal_at.rs`。⚠️ 但**新 FE 没有这个入口**（`grep cwd fe/src` 零命中；`attach_folder` 只存在于旧 `web/`）。#1131 之后新 wave 都从 `fe/` 建 ⇒ 若不补，每个新 wave 都是 managed 且禁止转 attached，「写代码的 wave 建时 attach 到真仓库」这条取舍**在 UI 上不可达**，等于把 attached 判死刑。**S3 的 FE 项必须包含「建 wave 时选已有目录」** |
| attached → * | ❌ 不做 | 源侧是用户真仓库，任何「搬走旧目录」的动作都不该发生在它身上；换目标等于新建 wave |

> **r3.2 —— 撤回 r3 拒绝 `managed → attached` 的理由。** 原文写的是「需要认领+409+删旧目录三套规则
> 各自组合，价值低而 `rm -rf` 面大」，三处都不成立（第三方评审指出，我核实同意）：
> - `rm -rf` 面与 `managed → managed` **完全相同** —— 两者删的都是旧的 managed 目录，attached 侧只做校验、不碰；
> - 认领 + 409 的规则在**创建路径上已经存在**（`cove_folder_resolve` + `attach_folder`），不是新写；
> - 需求可预期：研究型 wave 做着做着要开始写代码 —— 正是 #1147 那个 wave 的反向情形。
>
> **真实阻力是 FE 工作量**（D6 自己已承认 attached 入口在新 FE 完全不可达）。把它写成风险论证，
> 将来会被当成既定结论引用。所以：改判为做，成本诚实记在 S3 的 FE 项上。

### D7 — 子 wave 与 fork（阻断级）

- **子 wave 必须独立分配自己的 managed 工作区，并在创建时立即冻结。**
  今天 `child_wave_adapter.rs:176-192` 把父 wave 的 cwd 原样写进子 wave。若不改：
  建 managed 父 wave P → 派 `spawn: sub-wave` → 子 wave C 拿到同一个 path → 用户删 C →
  S8 的回收对 C 执行 `remove_dir_all` → **P 的整个仓库（含全部产出）被删**，P 还在库里但下一个 worker
  以 `not a git repository` 死掉（`waves.rs:2188` 只挡「父有子时不许删父」，不挡「删子」）。
  「子记为 attached 指向父目录」不自洽（父删除时会删掉子还在用的目录），**只有独立分配是自洽的**。
  ⚠️ 同一片里必须把 `child_wave_adapter.rs:351` 写进 operation result 的那个 `cwd` 一并换成**子 wave 自己的
  path** —— scheduler bootstrap 不重读 wave 行，而是从**旧 operation result** 取 cwd
  （`scheduler/mod.rs:1499-1510`）。只改 adapter 不改 result，bootstrap 仍会用父路径起 harness。
- **fork / `as_template` 今天并不继承 cwd**（r1 写错了对象）：`waves.rs:1274` 的 fork 只复制 report 快照，
  cwd 仍来自请求体/`default_cwd()`。fork 出来的 wave 按普通新建走 default 分配即可。

### D8 — 回收（`rm -rf` 面）

- wave 删除：只回收 `kind == Managed` 的目录；**attached 永不碰**。
- cove 删除（`routes/coves.rs:373-392` 今天只做 lease release + worktree sweep）：还需回收其下所有 managed 目录
  与 `<workspace-root>/<cove_id>/` 这层，否则删完 cove 留一堆**没有任何 DB 行指向**的孤儿仓库，GC 无从下手。
- 归档（`archived_at`）**不回收**，但需要一条长期 GC 故事；「磁盘增长」在归档语义下比删除更常见。
- 所有回收路径前置一条无条件断言：**待删路径必须以 `<workspace-root>` 为前缀**。

### D9 — 存量迁移

- **例外：system cove 的 Today/launchpad wave 是内核所有，`frozen_at` 恒为 `NULL`**（r3.3，S1 评审）。
  理由：`today_launchpad_ensure_tx`（`routes/today.rs:98/107`）的收编分支会重指向这条 wave 的 path。
  若它是冻结的，D1「`frozen_at` 一次性、单调，`Some` 之后 path/kind 不可改」当场为假 —— 这是 S1 评审
  实测到的**当下可达**违约，不是 S3 的坑。让内核所有的 wave 不冻结，单调性就从不被违反，
  也不需要为它开 allowlist 豁免口子。
  代价是存在一条 `attached + frozen_at IS NULL` 的行 —— 因此配一条断言：
  **该状态只允许出现在 system cove 的 wave 上**；且 **S3 的 `PATCH` 必须拒绝 system cove 的 wave**
  （它的 path 由内核管，不是用户可改的东西）。
- **新建 attached wave（用户 cove）在创建时即写 `frozen_at`**（r3.2，第三方评审）。存量在下面已经这么做了，新建的原文没说。
  若它是 `None`，PATCH 只要有一个分支漏判 `kind`，判据 (2) 就会跑到**用户真仓库**上 —— 而「工作树干净、
  只有 1 个提交」的真仓库是存在的（刚 `git init` 的项目），那就真被搬走了。attached 从不需要改指向，冻它零代价。
- 所有现存 wave → `kind = Attached`，`path = waves.cwd`，`frozen_at = created_at`（它们物理上确实在那跑过，一律视为已冻结）。
- `:4140` 那三个 `$HOME` / `/tmp` / `/` 的 wave 同样按 attached 冻结，不做救治 —— 它们本来就坏，新建的走新路。
- 只加列，**不得编辑已发布迁移文件**（sqlx 对整文件做 checksum，改了会 `VersionMismatch` 起不来）。
- ⚠️ `routes/today.rs:71-95` 有**三处显式列名的 `SELECT` / `INSERT waves` + 手写 `Wave` 构造**，绕过
  `wave_create_tx`。新增列必须同步这三处，否则是**运行时** sqlx 错误（cards 列那个老坑的 wave 版）。

### D10 — cove 对话入口（阻断级）

`ensure_cove_chat_wave_inner`（`waves.rs:1092-1109`）在 cove 没有 `cove_folders` claim 时直接 409
（`POST /api/coves/{id}/conversations` 无条件先调它，`cove_conversations.rs:241-245`；唯一例外是该 cove 已有 chat wave）。
cove 变成「纯命名空间、不选目录」后，**新 cove 的对话入口按定义永远失败**，#1098 整条路径挂掉。
r2：该路径改用 managed 默认分配（cove 有 claim 时仍优先用 claim，保持 attached 语义）。

### D11 — 与 #1147 ②③ 的关系

- ②「非 git wave 策略」：本设计的答案是**托管目录一律 git init**（在应用自己拥有的目录里 init，不是在用户目录
  里制造隐藏副作用 —— #1147 反对的是后者）。于是 `git_repo_root_for_wave_cwd` 对 managed 恒成功；
  attached 在**声明期**校验并 409，失败前移，不再等到 dispatch 才死。
- ③「claude 迁到对等租约」：**与本设计零耦合**。claude 完全不读 `wave.cwd`，托管根落地既不会让它更近也不会更远。
  r1 写的「托管后才可能」是不成立的依赖论证。→ 独立 issue，不在本设计切片内。
  ⚠️ 因此必须显式说清：**S2 落地后 claude 任务仍落在 supervisor 继承到的 cwd 下**，不要以为托管后就归位了。

## 4. 切片

| 切片 | 内容 | 独立价值 |
|---|---|---|
| **S0** | #1147 ① 失败可读：`last_error` 上浮到 `tasks.status_detail` + 进 `BlockVerdict` | 独立，且是 #1149 的依赖；**先做**，不要被本设计吸收 |
| S1 | typed `WaveWorkspace`：模型 + 迁移 + 读 DTO + OpenAPI/TS；存量按 D9 回填；`waves.cwd` 降为投影；同步 `today.rs` 三处 SQL | 下游能区分 managed/attached |
| S2 | 托管根配置 + 路径派生 + 物化（空提交、gitconfig 隔离、gitignore、幂等、失败即非 2xx）；新建 wave 默认 managed；D10 的 cove 对话入口 | 新 wave 的 codex 任务能跑 |
| S3 | `PATCH` 改工作区（仅 managed→managed）+ `freeze_workspace_tx` 四个调用点 + 未完成 operation 409 + harness 再锚定；FE 目录展示与切换 | 用户可改 |
| S4 | 子 wave 独立分配 + 创建即冻结（D7） | 阻断级：否则删子 wave 会删父仓库 |
| S5 | 回收：wave 删除 / cove 删除 / 前缀断言（D8） | 不泄漏磁盘、不误删 |
| S6 | terminal card 在 managed wave 里落到工作区而不是 `$HOME`（`terminal_adapter.rs:185-191` 今天回退到 `default_cwd()`） | 否则「wave 是一个仓库」在终端里不成立 |
| — | claude 迁 worktree（#1147 ③） | **独立 issue**，与本设计零耦合 |

**合入顺序：S0 → S1 → S2 → S4 → S5 → S3 → S6。** 评审判定的捆绑约束：

- **S4 必须先于或同于 S5**。反过来（S2+S5 落地、S4 未落地）就是 D7 描述的原样事故：子 wave 与父共享 managed
  path，删子触发对父仓库的 `remove_dir_all`。这是唯一「单独合入即破损」的组合。
- **S3 依赖 S5 的前缀断言**（S3 要动旧目录）。要么把前缀断言提到 S2，要么 S3 排在 S5 之后。
- **S2 必须自带 `today.rs` 的 launchpad 建 wave 路径**，否则其独立价值声明不成立（见 D3）。
- S1、S6 单独合入安全。

## 5. 必须证伪的测试（单违规 fixture）

1. **空仓库 worktree**：跑 materialize → 直接调 `provision_workspace_worktree` 断言 `Ok` 且目录存在。
   单违规：关掉「空提交」那步，断言变红且错误含 `not a valid object name`；并断言 mutation 真的生效（`git rev-parse HEAD` 失败）。
2. **attached 永不删**：tmpdir 造真实 git 仓库 R + 哨兵文件 → attached wave 指向 R → 走完整 `delete_wave` →
   断言 `R/.git` 与哨兵仍在。单违规：把 kind 判定改成恒 `Managed`，断言变红。
3. **子 wave 不共享 managed 根**：父 P → child adapter 建 C → 断言 `C.path != P.path`；删 C 后 P 的
   `git rev-parse --show-toplevel` 仍成功。
4. **冻结写入点的注册表元测试**。⚠️ **判据必须是列名驱动，不是表名驱动**（r3.3，S1 评审实测）：
   以 `UPDATE waves` 这类表名字面量做入口的扫描器，被 `format!("UPDATE {WAVES_TABLE} …")`、小写
   `update waves`、以及多行 `r#"UPDATE\n  waves\n SET …"#` 三种**能编译的真写点**全部绕过 —— 而本仓
   大量 SQL 正是多行 raw string，rustfmt 换一次行门禁就瞎。
   正确形状：扫描前 **whitespace 折叠 + lowercase**，直接找列名（`cwd` / `workspace_*`）的词边界命中，
   命中所在的 `sqlx::query*(` 不是单写者就红；表名 marker 只用于维护 allowlist 的集合相等。
   **任何「豁免类」的准入判据必须是语义的，不能是文件名** —— S1 评审实测：后缀判据下，新建一个
   `xxx_migration_tests.rs` 生产模块 + allowlist 里自我声明一行，即可让一个真写点全绿通过。⚠️ 本仓**没有运行时可枚举的写入点注册表**（写 `waves` 的是散落的裸 sqlx），
   唯一诚实的实现是**扫源码**：对 `INSERT INTO waves` / `UPDATE waves` / `wave_create_tx(` /
   `INSERT INTO workspace_leases` / `INSERT INTO terminals` 的命中集合与一份显式 allowlist 做**集合相等**。
   形状必须写死在设计里，否则实现者会退化成「逐条断言四个已知点」= 空转
   （`feedback_test_must_drive_production_wiring`）。枚举集合**必须包含 `terminals` 与三条建卡路由**。
   单违规：加一个不在 allowlist 的假写入点，断言元测试变红。
5. **materialize 失败让 create 失败**：⚠️ **不要用「只读目录」注入** —— CI 里以 root 跑时 `chmod 0555` 对 root
   无效，测试会假绿。改成把 `<root>/<cove_id>` 预先造成一个**普通文件**（`mkdir` 必 `ENOTDIR`），
   断言 `POST /api/waves` 非 2xx 且响应体含真实错误文本。
6. **可改判据的三条命令各自可证伪**（每条都要有自己的单违规 fixture，因为每条挡的是不同的逃逸）：
   (a) 在工作区写一个**普通**文件 → PATCH 断言 409。单违规：去掉 `status` 判据 → 变红。
   (b) 在 `.claude/worktrees/<w>/<c>/` 里写文件（**被 exclude**）→ PATCH 断言 409。
       单违规：把 `--ignored` 去掉 → 变红（这条是 r3 评审实测抓到的误删场景）。
   (c) **worker 在租约 worktree 里提交、然后 sweep 掉 worktree**（租约的正常终局）→ PATCH 断言 409。
       单违规：把 `rev-list --count --all` 换回 `HEAD` → 变红（`HEAD`=1 而 `--all`=2）。
   (d) 建一张 terminal 卡 → PATCH 断言 409（`frozen_at` 已写）。
   (e) **turn 进行中改工作区 → 断言 409**（r3.2，堵 A 的 TOCTOU）。单违规：去掉「rename 前重跑判据 (2)」
       这一步，断言变红。这条测的不是「判据算得对」，而是「判据与 rename 之间没有窗口」——
       是本设计里唯一一条**时序**判据，不能用静态状态断言代替。
       再补一条走 `claude_restart_adapter.rs:182` 的：断言它也冻结（证明 `freeze_workspace_tx`
       确实下沉到了 `terminal_create_tx`，而不是挂在四个 composite 上）。
   —— r2 的「未完成 operation → 409」测试**已删除**：那条规则本身被证伪（child-wave op 的 target 是
   `unknown`/`NULL`，`payload_json` 里也没有 `wave_id`，断言必然红）。
7. **gitconfig 隔离**：`GIT_CONFIG_GLOBAL` 指向含 `commit.gpgsign=true` 的临时文件 → 跑 materialize → 断言成功。
   （本机全局 config 不含 gpgsign，不注入就测不出来。）
8. **列同步**：新增列后 `GET /api/waves/{id}` 与 `today_launchpad_ensure_tx` 两条路径都仍 200。
9. **回收前缀断言**：任何回收调用传入非 `<workspace-root>` 前缀的路径 → panic/Err，带测试。

## 6. 其余风险

- **路径长度**：`root + <cove_id 32> + <wave_id 32> + .claude/worktrees/<32>/<32>` ≈ 150 字符。
  ⚠️ 注意 **wave id 在这条路径里出现了两次** —— 在一份需要专门论证 `SUN_LEN` 的设计里白扔 32 字符。
  这是沿用现有 `workspace_lease_path_for` 的形状，**不是刻意设计**；不改也行，但别当成有意为之。已确认 socket
  都在 `CALM_DATA_DIR` 下、不在工作区内（`state.rs`、`spec_appserver.rs`），故 `SUN_LEN(~108)` 无风险 ——
  但这个结论要用一条断言钉住，否则将来谁把 socket 挪进工作区就静默炸。
- **托管仓库无 remote**：forge 的 PR 流程在 managed 上退化为只出 diff。这是**有意**取舍（研究/写作型 wave 用
  managed，写代码的 wave 建时 attach 到真仓库），但要在 UI 上说清楚，不能让用户以为能开 PR。
- **`waves.cwd` 的真实读者清单**（r1 列错了两个）：`mcp_server/transport.rs:958`、`tools/emit.rs:217`、
  `task_verify_adapter.rs:670-681`（gate cwd 回退）、`child_wave_adapter.rs:176`。
  **不是**读者：`plan.rs`（只搬运 `task.cwd`）、`terminal_adapter.rs`（回退到 `default_cwd()`，见 S6）。

## 7. 评审留痕

r1 → r2 的双路评审（codex 只读通道 + subagent），两路独立同意的事实纠正：

| 结论 | 两路是否一致 |
|---|---|
| 子 wave 继承父 cwd ⇒ 删子会删父仓库 | 一致同意 |
| `today.rs` 的 `UPDATE waves SET cwd` 绕过 PATCH 闸门；三处显式列名 SQL 必须同步 | 一致同意 |
| 无 claim 的 cove 上开对话必然 409 | 一致同意（codex 补：已有 chat wave 的 cove 是例外） |
| harness 无法「创建但不 spawn」，D5(c) 不成立 | 一致同意 |
| claude 完全不读 `wave.cwd`，与本设计零耦合 | 一致同意 |
| 空仓库 `git worktree add` 失败 / `gpgsign` 让空提交失败 | subagent 实测复现 |
| payload hash 本身不破，破的是「同一 hash 对应稳定工作区语义」；且 child bootstrap 从旧 operation result 取 cwd 是**第二个**窗口 | codex 独有，已并入 D4/D7 |

r2 → r3 的第二轮双路评审，**r2 新加的「未完成 operation 就 409」被两路从不同角度独立证伪**：

| 结论 | 来源 |
|---|---|
| 该闸门**查不到**目标行（child-wave op 落成 `("unknown", NULL)`，`TxOutput` 又覆写成子 wave） | subagent |
| 该闸门**漏拦**（operation 已 `succeeded`，cwd 仍活在 codex thread / harness handle 里） | codex |
| 「wave 上的未完成 operation」本身就是跨 kind 逐条枚举 —— r1 被否掉的形状换层皮 | subagent |
| 卡片级 cwd 是独立持久真源（`terminals.cwd`），重启后直接 spawn PTY；三条建卡路由不继承 wave、不校验绝对路径 | subagent |
| spec harness 是 `workspace-write`，选 (a) 后「已有产出但未冻结」是常态而非边角 | subagent |
| 物化挂载点漏了 `today.rs:89` 与另外两个 `create_wave_structure` 调用方 | subagent |
| attached 在新 FE 完全不可达（`grep cwd fe/src` 零命中） | subagent |
| 建完 wave 的稳态是**零未完成 operation**（`spec-harness-start` 在 201 返回前已终态），所以闸门不是「过严」而是「无效」 | 两路一致 |
| lost-lease 会把非终态行 latch 到重启（`driver.rs:441-646` 只 log 不推进），若曾保留该闸门会变永久 409 | subagent |
| `harness/mod.rs:114/127` 的 `handle_state_json` 是**第四份** cwd 快照 | 两路一致 |
| `task-verify` 把 cwd 冻进 `tx_output` 且跨内核重启存活；forge action Spec 分支持久化 `wave.cwd` 进 payload | codex（均被 r3 的「盘上是空的」判据覆盖） |

r3 的核心改动：删掉 operation 闸门；冻结写入点补「卡片创建」；新增**「盘上是空的」git 判据**代替枚举；
删目录改为 rename 到 `.trash`；物化挂载点补齐 5 个入口；S3 补 FE 的 attached 入口；S4 补 operation result 的 cwd。

r3 → r3.1 的第三轮（只查 D4 的新判据）。判据的**形状**两路都认可，但**命令形式全错**，三条各自独立地
把判据变成「永假」或「误判为空」——全部是改文字级修正，方向未动：

| 结论 | 处置 |
|---|---|
| D3 写 `.gitignore` 的说法把机制说错了（既有实现写的是 `.git/info/exclude`），且会让 `?? .gitignore` 令判据 (2) **永假** | D3 步骤 4 改写 |
| `status --porcelain` 看不见被 exclude 的 worker 产出（全部 lease 都在 `.claude/worktrees/`）⇒ 必须 `--ignored` | D4 (2) 第一条 |
| `rev-list --count HEAD` 看不见 slice 分支提交与 stash（租约正常终局下 `HEAD`=1 而 `--all`=2）⇒ 必须 `--all` | D4 (2) 第二条 |
| 冻结闩仍漏 `claude_restart_adapter.rs:182`；下沉到 `terminal_create_tx` 才能消灭枚举形状 | D4 (1) 改写 |
| rename 后 worktree 的两个绝对路径指针双向悬空 ⇒ fail-safe 只保文件不保 git 历史；且活进程会继续写 trash ⇒ 重开前必须先 interrupt | D4 动作 0/1 |
| **`handle_state_json` 没有 cwd 字段**，r2/r3 写的「重开时刷新它」是假任务 | D4 动作 3 改写 |

**两路结论冲突一处，由我亲手实测判定**：`--ignored` 在 `.claude/worktrees/` **为空目录**时是否非空 ——
subagent 说否、codex 说是。实测（干净 / 空目录 / 含文件 / 删文件后留空目录）证明 **subagent 对**：
只有目录含文件时才输出 `!! .claude/`。故「`--ignored` 会导致永远不可改」不成立，该形式可用。

### r3.1 → r3.2（第三方通道评审，issue 上的独立评论）

对方总评「结构、切片、合入顺序的捆绑约束与单违规 fixture 这一整套足够严谨，可以进实现」，
并指出一条阻断级 + 三条应改：

| 结论 | 处置 |
|---|---|
| **A（阻断）** 判据 (2) 与 rename 之间是 TOCTOU：SQLite 事务对文件系统零隔离，只栅得住 DB 写者；而 harness agent 刻意不冻结、从建 wave 起就 `workspace-write`，dispatcher 还会主动推 observation 开新 turn。动作 0 的「先 interrupt」是异步的，且挡不住**新** turn 开始 | D4 加「真栅栏（同 tx 内 supersede/park harness runtime）+ rename 前重跑判据 (2) + 前缀断言」三步；§5 加「turn 进行中改工作区 → 409」的可证伪测试 |
| **B** 前缀断言只覆盖回收路径，没覆盖 rename；且新建 attached wave 没说要写 `frozen_at`，漏判 kind 就会搬走用户真仓库 | D4 第 3 步 + D9 各补一条 |
| **C** 拒绝 `managed → attached` 的三条理由全不成立，真实阻力是 FE 工作量 | **改判为做**，理由诚实写明。用不成立的风险论证挡真需求，将来会被当既定结论引用 |
| **D** `waves.cwd` 作为投影缺单写者约束，正文只说了意图没说成约束 | D1 明写不变量，并让 §5 测试 4 的 allowlist 显式承担 |
| 小：wave id 在租约路径里出现两次 | §6 注明是沿用既有形状、非刻意设计 |

对方已核实**不成立**、无需再查的两条（记下来免得下一轮重复劳动）：

- 「wave 换 cove 会让 `<cove_id>/<wave_id>` 路径撒谎」—— `db/sqlite/wave.rs` 里没有任何
  `UPDATE waves SET cove_id`，wave 建后不能换 cove，路径稳定。
- 「D4 动作 3 重开 thread 会丢对话历史」—— harness item 按 **card** 持久化
  （`db/sqlite/read.rs:671-674` 的 `WHERE card_id = ?1`），与 thread 无关；
  `GET /api/cards/{id}/harness/items`（`routes/cards.rs:194`）读的就是这张表。
  动作 3 的产品代价比它读起来小得多。
