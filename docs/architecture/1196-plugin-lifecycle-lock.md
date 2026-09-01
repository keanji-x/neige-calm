# #1196 + #1169 — plugin 生命周期锁（设计 r4）

**基线**：`origin/main` @ `ba27404f`。行号以该提交为准。r1→r4 的演化见 §8 评审账本。

## §0 目标

**同一个 plugin id 的生命周期操作全部串行化，并让「决定」与「发射」无法被拆开。**

| | issue | 层 | 症状 |
|---|---|---|---|
| A | #1169 | 操作层 | install / enable / disable / uninstall / reload / spawn 两两可交错，四条已确认竞争 |
| B | #1196 | 状态层 | 即使单个操作内部，「改表」与「发 `plugin.state` 事件」也被锁的释放劈开 |

只做 B 是半吊子：guard 内部对了，外面的操作照样交错。只做 A 也不够：锁若只包住操作的一部分，发射仍在锁外。**A 与 B 共用同一把锁**——不是两把锁配合，是一把锁的两种用法。

**范围说明（r3 收缩）**：`Event::PluginState` 的**跨 crate 构造权**移出本设计，另立 **#1210**（§3、§8）。它与本设计正交，且两轮评审证明「在 server 适配层加运行时围栏」这条路覆盖不到真正的写面。

## §1 事实基线

### 1.1 状态发射的现状

`state_emit`（`mod.rs:335`）是 #1171 第五轮加的 per-id 异步锁，两条入口：`emit_state(&self, id, status)`（`:1963`）自己取锁再发射；`emit_state_under(&self, guard, status)`（`:1974`）由调用方持锁、id 从 guard 读。

`emit_state` 就是缺陷本身：它让「表锁内决定 → 放开表锁 → 之后发射」成为可写出的形状。7 个调用点：

| 行 | 位置 | 形状 |
|---|---|---|
| `1132` | `spawn_admitted` → `Spawning` | app 插件；**缺陷类在 connector 之前就存在** |
| `1263` | `spawn_admitted` → `Running` | 表锁 `1234-1261` 取放在前，发射在锁外 |
| `1305` | `spawn_mcp_http` → `Spawning` | |
| `1380` | `spawn_mcp_http` → `Unavailable`（registry 中途消失） | |
| `1412` | `spawn_mcp_http` → `Running` | 表锁 `1386-1410`，发射在锁外 |
| `1521` | `publish_unavailable` → `Unavailable` | `mark_unavailable` 改表 → 返回 → 才发射 |
| `2015` | `emit_crashed` | 被 `1065` / `1166` / `1169` / `2092` / `2133` 五处调用 |

只有 `stop()`（`:1714-1722`）与 `reaffirm_running()`（`:1623-1635`）这一对是对的。

**可达的坏交错**（两个通道各自复核属实）：

```
spawn_mcp_http                       并发 stop()
──────────────                       ──────────
拿表锁 → 插入 Running → drop
                                ──►  拿到条目，stopping=true
                                     拿 state_emit 锁 → live.remove
                                     提交 Disabled，释放
拿 state_emit 锁  ◄──
提交 Running  ←── 最后落库的是这个
```

活表为空，事件日志与总线的最后一句是 `running`，且**没有后续事件来和解**。

### 1.2 操作层的现状

`ProcessTable`（`:186`）只有 `live` 与 `spawning`；`spawning` 由 `AdmissionGuard`（`:210`）RAII 管理，**只覆盖 spawn-vs-spawn**。

**四条竞争的共同根因是「同 id 的复合生命周期操作没有串行化」**，不是「`stop` 只查 `live`」——后者只直接解释其中两条：

| # | 竞争 | 机械原因 |
|---|---|---|
| 1 | uninstall vs 在途 spawn | `stop` 只查 `live`（`:1658-1661`）→ 对 spawning 返回 `NotFound` → 路由当良性（`routes/plugins.rs:533`）→ 删 token/kv/overlay（`:540-542`）+ `plugin_delete`（`:543`）+ `registry().remove`（`:544`）；在途 spawn 随后插进 `live` |
| 2 | 并发 install 同 id | 路由 `SELECT`（`:350`）与写入（`:379`）之间 TOCTOU，且底层是 `ON CONFLICT DO UPDATE`（`calm-truth/src/db/sqlite/out_of_domain.rs:374`）。**与 `stop` 无关** |
| 3 | 并发 disable→enable | 两个复合操作的「DB 位」与「运行时步骤」可交错（`:408` / `:443`）。**与 `stop` 无关** |
| 4 | reload vs 在途 spawn | 同 1 的 `NotFound`（`:620`），加上旧 spawn 带着旧 manifest 副本继续跑完 |

竞争 1 有一处不对称：**connector 有缓解，app 没有**。`set_exposes_tools` 在 id 不存在时 no-op 并让 spawn 放弃（`:1368-1383`）；app 的 `spawn_admitted`（`:1085-1263`）在 `spawn` 开头 `registry.get`（`:973`）之后再无第二次 registry 查询。

**竞争 4 的终局**：spawn 路径对 registry 的唯一写是字段级 `set_exposes_tools`（`registry.rs:224`）；整份替换只发生在路由的 `registry().insert`（`routes/plugins.rs:664`）。真实终局是 **DB 与 registry 是新 manifest，运行时是旧 connector（旧 URL / 旧 allow-list），`exposes_tools` 可能是旧物化结果**——不是「整份 manifest 回退」。验收 8 照这个写。

### 1.3 一个既存缺陷（本设计会路过）

`supervise_inner` 的 respawn 先 `live.remove`（`:2129`）再 `spawn`，而崩溃计数的继承靠 `spawn_admitted` 的 `table.live.get(id)`（`:1236-1239`）——此时必为 `None`，`crashes_in_window` 每次归零。于是 `:2082` 的 `+= 1` 恒为 1，`:2088` 的 `exceeded` 恒为 false（该分支 `:2093-2106` 只是不再 respawn，**不写 `enabled=false`**——不要去找一个不存在的 DB 写），**`CRASH_WINDOW_LIMIT`（5 次 / 5 分钟）的「停止 respawn」从不触发**，退避恒取 `BACKOFF_SCHEDULE_MS[0] = 1 s`。`:1229-1233` 那段「保留崩溃窗口计数器」的注释与 `:2129` 的行为直接矛盾。

**而 `tests/cases/plugin_host_smoke.rs:233 crash_loop_disables_after_threshold` 今天绿着，且自称验证的正是这条阈值。** 它的判据是「看到 `Crashed` → 睡 3 s → 仍是 `Crashed` 就算过」（`:263-269`）。在退避恒为 1 s、崩溃桩立刻再崩的情况下，插件绝大多数时间都停在 `Crashed`，所以 3 s 后「仍是 `Crashed`」是**崩溃循环本身**就能满足的——这是一条假门禁，也是这个缺陷能活到今天的原因。

缺陷本身是**读代码得出的推断，尚未跑过**（验收 13 的第一步就是把它跑成红）。

## §2 形状

### 2.1 一把锁：`LifecycleCell`

`state_emit` 那张 per-id 表扩权为生命周期锁表：

```rust
/// Per-plugin-id 生命周期锁。一个 id 上的任何生命周期操作 —— install /
/// enable / disable / uninstall / reload / spawn / stop / restart / 崩溃重生
/// —— 以及它产生的每一次 `plugin.state` 发射，都在同一个 guard 生存期内。
lifecycle: std::sync::Mutex<HashMap<String, Arc<LifecycleCell>>>,
```

guard 类型 `LifecycleGuard { id, cell, _held: OwnedMutexGuard<()> }`。

**为什么一把不是两把**：两把锁意味着存在「持生命周期锁但不持发射锁」的中间态，即把今天的缺陷原样搬进锁内。

**为什么 per-id 不是全局**：真实代价在运行期——一次 connector bring-up 最长 30.5 s 会挡住**所有**插件的 enable/disable。（不是「全局锁会拖慢 boot」，boot 的 autospawn 本来就是串行 for 循环 `:820-879`。）

### 2.2 入口取锁，内层带证

**规则**：公开入口取锁，内层函数收 `&LifecycleGuard`，id 一律从 guard 读、不作为第二参数传。

```rust
pub async fn spawn(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
    let g = self.try_lock_lifecycle(id)?;      // 外部语义，见 §2.5
    self.spawn_under(&g).await
}
async fn spawn_under(self: &Arc<Self>, g: &LifecycleGuard) -> Result<(), HostError> { … }
```

**必须成对拆分的点**（评审逐个点名，漏一个就自重入）：

- `spawn` / `stop` / `restart`（= 一个 guard 下 `stop_under` + `spawn_under`；今天 `:1730-1734` 的 stop 与 spawn 之间是一条真实窗口）
- `rotate_plugin_token`（`:721`）→ registry 查 + kind 守卫 + token 删 + restart 收进一个 guard
- `publish_unavailable`（`:1512`）**有两类调用者**：`spawn_under` 内部失败出口（已持锁）与 boot timeout 后的外部 reconciliation（未持锁）→ 拆 `publish_unavailable_under` + 取锁包装
- `reaffirm_running`（`:1623`）同为 reconciliation 入口
- `emit_crashed` → `emit_crashed_under`
- `autospawn_enabled(_within)`（`:769` / `:782`）逐个插件取放，不是整轮持一把
- `ensure_plugin_token`（`:708`）写 `plugin_tokens`，生产唯一调用方是 `:1130` → 改收 `&LifecycleGuard`

**删掉 `emit_state(&self, id, status)`。** 7 个调用点全部改为 `emit_state_under(g, …)`。这样「先决定后发射」在 `plugin_host` 内部不可表达。

### 2.3 复合操作搬进 host —— 封闭范围如实说

五个复合操作做成 `PluginHost` 上的方法，路由退化成 HTTP 适配层（解析 + 错误映射）：

| 操作 | guard 覆盖 | 关掉哪条竞争 |
|---|---|---|
| `install` | 重名检查 → `materialize_install_tree` → `plugin_install` → registry 写 | 2 |
| `enable` | `plugin_update_enabled(true)` → `spawn_under` | 3 |
| `disable` | `stop_under` → `plugin_update_enabled(false)`（顺序见 §2.6） | 3 |
| `uninstall` | `stop_under` → token/kv/overlay 清理 → `plugin_delete` → registry 移除 | 1 |
| `reload` | `stop_under` → 读盘 → 校验 → registry 写 + `plugin_update_manifest` → **`if plug.enabled`** 才 `spawn_under` | 4 |

**搬迁不扩宽写面**：六个 repo 方法全在 `RepoOutOfDomain`（`calm-truth/src/db/mod.rs:971-996`），而 `RouteRepo: RepoEventWrite + RepoOutOfDomain + …`（`:1114`），host 的 `repo` 已是 `Arc<dyn RouteRepo>`（`mod.rs:277`）。PR #41 那条「raw sync-domain writes 不可达」的窄化决定完全不受影响。

**取锁点**：`install` 的 guard 只能在 `Manifest::parse` 之后取（id 来自 manifest，`routes/plugins.rs:326`）。取锁前的动作全是只读（`:314` / `:315` / `:322`），落盘从 `materialize_install_tree` 开始——所以「Busy 之前无副作用」成立。

S0b 之后 min_kernel 校验已随 `install` 一起搬进 host，住在 `plugin_host/lifecycle.rs:61-72`（原 `routes/plugins.rs:332-345` 的行号引用作废）。**S1 的取锁点必须落在这段只读校验之后、重名检查之前**，即 `lifecycle.rs:72` 之后、`:76`（`plugin_get_by_id` 重名探针）之前。写在 `install()` 第一行是错的：那样「某 id 正忙 + manifest 内核过老」会返回 `LifecycleBusy` 409，而不是今天的 `PluginKernelTooOld` 422——一个被锁掩盖掉的错误码回归。min_kernel 段一行都不写，放在 guard 之前不破坏「Busy 之前无副作用」。

**`stop_under` 的 `BadState("already stopping")`（`:1662-1664`）**：持锁后不可能发生，降级为 `debug_assert!` + 走 `NotFound` 分支。

#### 封闭到什么程度（r2 的「类型封闭」主张撤回）

r2 说「新增入口忘了取锁在类型上写不出来」。**这是假的**，两个通道各自给出同一组反证：

- `PluginHost.registry` 是 `pub` 字段（`mod.rs:271`），`registry()` 公开返回可变能力（`:675`），`PluginRegistry::insert/remove/set_exposes_tools` 全 `pub`（`registry.rs:196/236/224`）——**恰好是竞争 1 与竞争 4 的写面**。`CallbackCtx` 也持有这个句柄（`callbacks.rs:65`），插件回调线程同样拿得到。
- 路由层持有 `Arc<dyn RouteRepo>`（`state.rs:95`），与 host 的是同一个 trait 对象；将来任何 handler 都能直接 `s.repo.plugin_update_enabled` + `plugin.spawn`。

**本设计做的收窄**（把「一张会过时的表」缩到可论证的范围）：

1. `PluginRegistry` 的三个写方法改收 `&LifecycleGuard`；`registry` 字段私有化，`registry()` 只返回**只读视图**（`get` / `list` / `install_path`）。registry 与 `live` 表因此都进了同一把锁。
2. **剩下的残留如实登记**：路由层的 `s.repo` 仍能绕过 host 直接改 plugin 行。**这一条靠纪律，不靠类型**，因此验收 1 保留一张入口登记表——但登记表只覆盖 DB 半边，不再假装覆盖全部。

#### 建表期 vs 运行期：两种写，不是一个逃生舱

收窄有一个必须提前定死的实际障碍：**本仓有 24 处集成测试在 host 存在之前写 registry**（标准形状见 `tests/cases/plugin_host_smoke.rs:73` → `:91`：`PluginRegistry::empty()` → `insert` → `PluginHost::new_full(Arc::new(registry), …)`）。此刻 guard 不可能存在，而且 **`#[cfg(test)]` 逃生舱对它们不可见**——`tests/plugin_suite.rs` 用 `#[path]` 把 `tests/cases/*` 编成集成测试 crate，链接的是 lib 的非 test 产物。

实现者撞上 24 个编译错误时，最省事的出路是加一个 `pub fn insert_unlocked` / `seed`。**那会当场作废这次收窄的全部实质内容**——`registry` 句柄早已通过 `Arc` 分发给 `spawn_neige_router`（`mod.rs:1194`）与 `CallbackCtx`（`callbacks.rs:65`），字段私有化本身一点用没有，唯一起作用的就是签名。而这个逃生舱会以「测试辅助」的名义通过评审。

**因此把两种写在类型上分开**：

- **建表期**：消费型构造 `PluginRegistry::from_manifests(…)` / builder，`build()` 之后不可再写。覆盖那 24 处以及 lib 内 `#[cfg(test)]` 的 9 处。
- **运行期**：唯一入口是 host 上收 `&LifecycleGuard` 的 `registry_insert` / `registry_remove` / `set_exposes_tools`。供 host 自身（`:1368`）、搬迁后的三个复合操作，以及 host 之后写入的测试（`forge_workflow_e2e.rs:350/380/405`、`connector_host.rs:1988`——`try_lock_lifecycle` 是 `pub`，见 R7）使用。

`load_from_dir`（`registry.rs:92-155`）**不走 `insert`**，直写 `inner`，不受影响。

**还有第三种形状，两个桶都接不住**：`src/mcp_server/transport.rs:1330/1337` 在 `registry_after_materialization` 返回一个**已建好**的 registry 之后再 `insert` + `set_exposes_tools`，且全程**没有 host**。建表期（消费型构造已 `build()`）与运行期（要 guard、要 host）都不适用。它在 lib 内的 `#[cfg(test)]` 里，所以 **S0 把三个写方法降到 `pub(crate)` 时它照常编译，撞墙发生在 S1**（签名要 guard）。修法是把那个 helper 改成一次收全部插件（五行）——不明写的话，实现者撞上的第一反应就是 `insert_unlocked`。

这 24 + 9 处改造属于 §7 的 **S0**，必须进量级预算。

### 2.4 为什么「`stop` 看不见 `spawning`」会自己消失

不是给 `stop` 加一条「也查 `spawning`」的分支。而是：**在途 spawn 必然持有该 id 的 guard**，所以 `stop_under` 拿到 guard 时，那次 spawn 只可能已落地或已回退。

**前提要写全**：

1. 每条 same-id spawn 路径从 admission 之前直到「成功插表」或「`AdmissionGuard` 回退」全程持 guard；
2. 每条 stop / uninstall / reload 路径先持同一 guard；
3. **没有任何不持 guard 的 same-id 表写。**

前提 3 的逐点核对（`ProcessTable.live` 的全部写点，测试代码零处直写）：

| 行 | 位置 | 持锁 |
|---|---|---|
| `1165` | auth-mismatch `live.remove` | spawn guard 内 |
| `1242` / `1394` | 两条 insert | spawn guard 内 |
| `1561` | `mark_unavailable` insert | 两个调用者：`publish_unavailable_under`（持锁）+ boot fallback（§5 R5 改 `try_lock`） |
| `1718` | `stop` remove | stop guard 内 |
| `2071` / `2105` / `2129` | 监督器三处 | §2.6 三段各自持锁 |

**唯一不被 guard 覆盖却能继续动的续体是监督器任务**（`:1217-1223` 由 `tokio::spawn` 创建，早于 live 插入 `:1242`），由 §2.6 承接。

**取消的语义**：`autospawn_one_connector` 的 `timeout_at`（`:908-912`）丢弃整个 future，guard 与 `AdmissionGuard` 一起 drop，不产生第三态。但「不存在第三种状态」的范围限定在**锁与 admission 保留**，不是世界：丢弃点若落在 `set_exposes_tools`（`:1368`）之后、live 插入（`:1394`）之前，registry 里会留下一份已物化的 tools 而无 live 条目。既存，登记在案。

`ProcessTable.spawning` **保留**：它服务的是**跨 id** 的 workflow-id 唯一性（`workflow_holder_ids`，`:246`），per-id 锁按定义做不到。职责正交。

### 2.5 两种获取语义

r2 规定「唯一获取函数是非阻塞」。**这一刀切是错的**，两个通道各自给出同一个反例：**监督器不是请求，没有重试者，放弃即丢信息。**

具体：监督器任务创建于 `:1217`，早于 live 插入（`:1242`）与 `emit_state(Running)`（`:1263`），而 spawn 的 guard 要持到 `spawn_under` 返回。子进程若在握手之后、spawn 返回之前就死（崩溃桩、启动即失败的真插件、被 OOM kill 的），监督器第一段**必然**撞上 spawn 自己的锁。一次 `try` 就放弃 → 崩溃从不记账，`live` 里留下 `status: Running` + 已死子进程 + 已结束的监督器句柄，**永远没人来救**。第三段同理：退避醒来撞上任意并发操作就放弃 → 永久停在 `Crashed`。

**因此分两种语义：**

| | 函数 | 语义 | 用于 |
|---|---|---|---|
| 外部 | `pub fn try_lock_lifecycle(id) -> Result<LifecycleGuard, HostError>` | 非阻塞，`LifecycleBusy` → HTTP **409**，**不做任何事** | 五个复合操作、`spawn`/`stop`/`restart`/`rotate_plugin_token` 等有调用方可以收错误并重试的入口 |
| 内部 | `async fn await_lifecycle(id) -> LifecycleGuard` | 等待直到取得 | 监督器三段、boot reconciliation（`publish_unavailable` / `reaffirm_running` 的取锁包装） |

`await_lifecycle` **私有**，且只允许在「没有调用方可以答复」的后台路径使用；每次取得后**完整重判**（epoch 已使重判安全，§2.6）。它没有预算常量，因为后台任务等待没有 UI 成本。

**r1 的 `LIFECYCLE_ACQUIRE_BUDGET ≈ 32.5 s` 撤销**：量纲错（那是整个 boot 连接器阶段跨所有 connector 的墙钟，不是单次 spawn），而且**那个上界根本不存在**——临界区里还有 bring-up 之后的 `emit_state`（事件库写无 timeout，`:1993`）、reload 的文件 I/O、多次 DB 写。没有常量能诚实地声称「大于任何合法的在途操作」。

**`Busy` 的终局必须显式**，不能像今天的 `:831-833` / `:916` 那样只 `warn!` 就 `continue`（那会留下「`enabled=true`、无 live 条目、事件停在上一条」＝ `connector_unavailable` 存在理由所反对的「像是从没被启用过」）：

- `autospawn` 拿到 `Busy`：走 `await_lifecycle` 重试一次（boot 期唯一的对手是监督器，等待有界），仍失败则落 `Unavailable` 终局态。
- `autospawn_one_connector` 的 timeout 臂（`:918-959`）今天是 `publish_unavailable(..) ? .. : reaffirm_running(..)` 的二值分支，`Busy` 是**第三种结果**，必须显式定义——否则实现者会把它塞进 `false` 分支，让已经起来的 connector 走进 `reaffirm_running`，而后者也 `Busy`，事件日志永久停在 `spawning`。这两个入口用 `await_lifecycle`，从源头消掉第三种结果。

**代价说清楚**：UI 双击 / 快速连点会拿到 409 而不是排队，需要 UI 重试。`LifecycleBusy` **必须有一个可判别的错误码**——`install` 的重名冲突今天也是 409（`CalmError::PluginConflict`，`routes/plugins.rs:351`），两者语义完全不同（一个永久失败、一个可重试），测试与 UI 都必须分得开。

**自重入（R1）在非阻塞下不再是永久卡死，但也不再会超时暴露**——它变成静默的 409。验收 12 因此必须补一条「无竞争时任何入口不得返回 `Busy`」。

### 2.6 监督器：三段式 + `run_epoch`

**分段**：**[持锁] 判定 + 崩溃记账 + 状态改写 + 发 `Crashed`** → **[无锁] 退避 sleep** → **[重新持锁] 重新判定 → `spawn_under`**。退避 sleep（`:2123`，最长 30 s）必须在锁外。三段的取锁全部走 `await_lifecycle`（§2.5）。

**判据用每实例 `run_epoch`，不用「删除计数 generation」**（r2 的写法有三个洞）：

- 挂在 `LifecycleCell` 上会 fail-open：将来谁在 uninstall 里顺手 `lifecycle.remove(id)`，计数归零，睡着的监督器醒来比对 `0 == 0` 就把已卸载的插件拉起来。
- 「live 条目被移除时自增」会自我作废：第三段自己先 `live.remove`（`:2129`）。
- 不覆盖替换：新 spawn 用 `HashMap::insert` 直接替换旧条目（`:1242`/`:1394`），未必经过 `remove`。

改为：**每次成功运行实例分配 `run_epoch: u64`**（host 级 `AtomicU64`），存进 `RunningPlugin`，**监督器在创建时捕获**（epoch 在插表前分配好并传进去）。

**三处判定**：

- **第一段（拿到锁后、记账之前）**：`live` 仍在 ∧ epoch 相同 ∧ 状态仍 `Running` ∧ `!stopping`。缺了它，旧监督器可能在 stop + 新 spawn 之后才第一次拿到锁，把**新条目**标成 `Crashed`。
- **sleep 前记录**：`run_epoch` + 本次 crash attempt 序号。
  - **attempt 需要自己的字段**，不能复用 `crashes_in_window`：后者会被窗口过期重置（`:2078-2081`），当判据不可靠。
- **第三段（重新拿锁后，且在自己的 `remove` 之前）**：条目仍在 ∧ epoch 相同 ∧ 状态仍是自己写下的 `Crashed` ∧ `!stopping` ∧ attempt 未变 ∧ registry 仍有该 id ∧ DB 行仍在且 `enabled=true`；**不得复用第一段读到的 manifest**（reload 可能已整份换掉），必须走完整的 `spawn_under`。
  - **DB 读失败必须 fail-closed**：保留 `Crashed`、释放锁、稍后重试读取（有界重试），每次重试重新核对 epoch / attempt / registry / DB。**不得跳过 enabled 判据。**
    - r3 写的是「跳过 DB 检查、依赖 epoch」，**不安全**：`run_epoch` 只证明运行实例没被替换，证明不了 DB 仍是 `enabled=true`。
    - **理由要用活着的那个。** r3 举的反例是「disable 在 DB 写成功之后、`stop_under` 之前被取消」——**这个窗口被本节的顺序翻转消掉了**（现在写库在 stop 之后）。fail-closed 现在唯一活着的理由是 §2.3 **显式登记的残留**：路由层持有 `Arc<dyn RouteRepo>`，可以绕过 host 直接写 plugin 行，造出「DB 说 disabled、而 live 与 epoch 未变」的状态。
    - 这条替换不是措辞：验收 15 的变异见证必须由这个残留驱动，否则那条验收的见证会退化：变异体在读失败窗口内本来就会立刻 respawn，只靠 15a 也能红一半，但那只证明了「读失败时别急着重生」，没证明「DB 位仍是权威」——15b 才是后者的见证（见验收 15）。
    - 代价是「repo 长期不可读 ⇒ 插件停在 `Crashed`」。这是正确的一侧：可用性换正确性，且状态是可观测的。
  - **不变量（S1 之后成立）**：同一个 guard 下，`stop_under` **成功移除**、或以 `NotFound` **证明 live 本就不存在**（已停止 / 从未启动的插件，今天路由也把 `NotFound` 当良性）之后，才写 `enabled=false`；**其他任何 stop 错误都不得写库**。（不能写成「每次 `enabled=false` 都伴随一次实际的 `live.remove`」——那对从未启动的插件不成立。）支撑：`plugin_update_enabled` 的两个生产调用点（`routes/plugins.rs:417` / `:452`）都被搬进 host 的 `enable`/`disable`，而 `disable` 必调 `stop_under`，后者对任何存在的 live 条目（含 `Crashed`、`Unavailable`、退避中——退避中 `supervisor` 仍在条目里，`:1666` 会 take 并 abort）都会 remove 并让 epoch 失配。这条不变量是 epoch 判据之所以比 DB 位更权威的理由，**在复合操作搬迁之前不成立**。
  - **`disable` 的内部顺序**：guard 内先 `stop_under`、后 `plugin_update_enabled(false)`。反过来（今天路由的顺序）在 DB 写成功而 stop 失败时留下「`enabled=false` 且插件仍在跑」，且下次 boot 的 autospawn 因 `enabled=false` 不会来和解；而选定的顺序在 DB 写失败时留下「已停但 `enabled=true`」，下次 boot 重新拉起——与 `enable` 既有的哲学（`routes/plugins.rs:418-420` 注释）同向，是可接受的一侧。
- `exceeded` 分支（`:2104-2107` 无条件 `live.get_mut` 写 `supervisor = None`）同样要核对 epoch。

**崩溃窗口的搬运要有机械形状**（r2 只说「epoch 让计数有了正确载体」，那是错的——`run_epoch` 只判身份，不搬运计数）：第三段在 `remove` **之前**把 `(crashes_in_window, window_started)` 取出，显式传给 `spawn_under` 作为「继承窗口」参数，由它写进新条目——而不是靠 `spawn_admitted` 去 `live.get(id)` 读一个必为 `None` 的东西。这条同时修掉 §1.3。

### 2.7 状态发射的封闭范围

**本设计只做 `plugin_host` 内部的封闭**：删掉 `emit_state(id, status)`，`emit_state_under(&guard, …)` 成为模块内唯一发射器。这是 #1196 的正文要求，也是本设计能完整论证的范围。

**跨 crate 的构造权移出本设计**（r2 的 D8 撤销）。两轮评审把「在 server 适配层加运行时围栏」这条路走到了尽头：

- `ServerRepoEventWriteExt` 的四个方法（`calm-server/src/db/mod.rs:383-441`）之外，还有三个 typed 自由函数（`:1109` / `:1133` / `:1158`，约 40 处调用），它们直接委托 `calm_truth::db::write_with_event_typed`（`calm-truth/src/db/mod.rs:1159`）——**结构上不可能被 server 侧 trait 围栏拦到**；
- `calm-server/src/db/mod.rs` 顶部 `pub use` 重导出了 `calm_truth::db::RepoEventWrite`，只写 `use crate::db::RepoEventWrite;` 就绕过适配层（`replay.rs:49/:272` 是现成例子）；
- **而且 `plugin_host/mod.rs` 自己只 import 了 `RouteRepo`（`:43`），没有 `ServerRepoEventWriteExt`**——所以 `:1993` 那个今天生产上**唯一**的 `PluginState` 写点，解析到的本来就是 calm-truth 的原方法。围栏对它零覆盖。

即：三轮下来，每一轮都发现这个「窄价值子组件」还有更深一层。按仓库既有纪律，这时候该重跑降范围决定，而不是加第四层围栏。

**另立 #1210，并把结论带过去**：真正能封闭的是**类型围栏**——把变体改成 `PluginState(PluginStatePayload)`、payload 字段全私有 + `#[derive(Deserialize)]`，serde 生成的代码在定义模块内，反序列化照常工作，而 crate 外没有任何字面构造式。代价是动 wire 类型（`event.rs:1112/:1264/:1430` 三处 match、zod goldens、`tests/cases/event_serde_goldens.rs`）。

**不要写「类型上不可能绕过」，也不要写「类型围栏不可行」**——r1 写过后者，是错的。

## §3 显式不做

- **`Event::PluginState` 的跨 crate 构造权**（§2.7）→ 新 issue，推荐类型围栏。
- **`plugin_install` 改成真冲突插入**：有了 §2.3 的 guard，竞争 2 已由「检查与插入在同一临界区」关闭；DB 层唯一约束只是纵深防御。
  （r1 给的理由「`callbacks.rs:1015` 是生产调用方」**是错的**：那处在 `#[cfg(test)]` 内（`callbacks.rs:873` 起），生产唯一调用方是 install 路由。正确的理由只剩范围控制。）
- **跨 id 的生命周期串行化**（§2.1）。
- **`ProcessTable.spawning` 的删除**（§2.4）。
- **#1194 / #1188 / #1168 / #1167**：正交。

## §4 验收

每条给出「变异什么会让它红」。**凡是本设计成立后无法通过公开 API 构造的坏顺序，一律写成变异驱动。** 并且：**所有并发用例都按 reject 语义写**——败者拿到 `Busy` 就结束了，不会自动在线性化点之后继续执行；要它执行就显式重试。

1. **入口取锁（每入口一个独立 fixture）**：持有该 id 的 guard，对每个公开入口断言返回 `LifecycleBusy`（可判别的 409）**且无任何副作用**（DB 行 / registry / live 表 / token 行逐项比对前后快照）；释放后重试一次，断言**成功的终局**（201/200/204 + 实际效果）。
   - 必须是独立 fixture：一次持锁把所有入口调一遍，`uninstall` 成功之后 `spawn`/`enable`/`reload` 只能是 `NotFound`。
   - 入口清单含 `spawn` / `stop` / `restart` / `rotate_plugin_token` / 五个复合操作；`reaffirm_running` 返回 `bool`（`:1623`），进清单前先定 signature（`Busy` 与 `false` 必须可分）。`autospawn_enabled(_within)` 是 boot 批量入口，**明确排除**在「公开生命周期入口」定义之外，单测其逐 id `Busy` 处置（§2.5）。
   - 变异：任一入口不取锁 → 该 fixture 红。
2. **`emit_state(id, …)` 不存在**：编译期即证——**这条只证明这一件事**。
   - 「`emit_state_under` 是 `plugin_host` 内唯一发射器」**不是被测出来的**：直接构造 `Event::PluginState` 并写库仍会绿。跨 crate 的构造权已随 §2.7 移出本设计，#1210 的类型围栏才是它的真解。这里如实登记为**经评审确认的性质，不是门禁**——不要在代码注释里把它写成「不可能绕过」。
3. **#1196 的坏交错（变异驱动）**：在真实 spawn「已插 live、尚未发 `Running`」处设确定性 barrier → 启动 `stop` → 断言它拿到 `Busy` 且**未改动任何状态** → 放行 spawn → **显式重试 `stop`** → 事件后缀 `Running → Disabled`、终局无 live。
   - **变异：去掉 spawn 外层的 guard → stop 先提交 `Disabled`、旧 spawn 后提交 `Running` → 必须红。**
   - barrier 手法可复用现有的「持一个 `write_in_tx` 事务」（`connector_host.rs:2380-2400`），但要注意它卡住的是**全库所有写**（含同 fixture 内任何 autocommit 写，见验收 16 的机制），同 fixture 内不要并驱第二个插件——写进注释时说全，否则下一个人会把它照抄到验收 16 里去。
4. **（移出）** —— 事件围栏随 §2.7 一并移到新 issue。
5. **竞争 1**：uninstall 撞在途 spawn → uninstall 拿 `Busy`，**DB 行、registry、token 全部仍在**（fail-closed），spawn 正常完成；显式重试 uninstall → 终局断言 host `status == None`、无 admission 保留、registry / DB / token / kv / overlay 均不存在。**app 与 connector 各一条**（app 没有 `set_exposes_tools` 那层缓解）。
6. **竞争 2**：并发 install 同 id → 一个 201，另一个 **`LifecycleBusy` 409**（**不是** `PluginConflict` 409——reject 语义下败者根本没跑到重名检查），断言错误码可判别；重试败者 → 这次才是 `PluginConflict`；胜者的 manifest / install_path 未被覆盖。
7. **竞争 3**：enable / disable 重叠 → 一个执行、一个 `Busy` 且无副作用；重试败者 → 断言两种终局各自的 `enabled` 位与运行时一致。**两个方向各一条 fixture。**
8. **竞争 4**：reload 撞在途 spawn → reload 拿 `Busy` 且 manifest 未更新；重试 → 断言**新 endpoint 实际收到 initialize / tools 请求**、新 allow-list 生效、live client 属于新 epoch、旧 tools 不可见。

   > **5 / 6 / 7 / 8 必须点名 barrier，并断言「至少观测到一次 `LifecycleBusy`」。**
   > 仅仅并发启动**不保证败者撞上锁**：胜者的临界区若先跑完，败者拿到的是 `PluginConflict`（6）或直接成功（7），而上面那些文本**在这种退化下依然全绿**——测试一次也没观测到锁，却看不出它空转了。本仓已经栽过两次同形的假门禁（`two_emitters_…`、`crash_loop_…`）。
   > 可用的接缝：`connector_host.rs:2380-2400` 的「持一个 `write_in_tx` 事务」把胜者钉在锁内（install 的临界区以 `plugin_install` 的 DB 写收尾）；`StubServer::start_gated`（`connector_host.rs:1976`）把 connector 的 `spawn_under` 钉住（5 / 7 / 8）。
   > 每条再按验收 12(a) 的写法列出败者允许的返回码集合。
9. **退避不持锁 + 不复活**：崩溃后退避期间 `disable` 能在退避时长内完成；**等完整退避之后**断言 DB `enabled=false`、无 live 条目、无新进程。变异：sleep 挪进锁内 → 前半红；去掉第三段判定 → 后半红。
10. **监督器 sleep 后重新判定**：退避期间 uninstall → 醒来不 respawn。
11. **`LifecycleBusy` fail-closed**：持锁不放，`uninstall` → 409 且 DB 行仍在。
12. **锁的活性与自重入**：
    - (a) 全部生命周期入口两两并发（含自反）→ **超时即红**，并**逐对列出允许的结果集合**（不能「全部快速返回 `Busy`」就算过）+ 核对终局状态；
    - (b) **无竞争时任何入口不得返回 `Busy`** —— 这条抓的是自重入（非阻塞语义下它不会超时，只会静默 409）。
13. **崩溃窗口计数（§1.3）**：先写失败复现——连续崩溃达到 `CRASH_WINDOW_LIMIT` 应停止 respawn；今天恒不触发。
    - **接缝定死为构造参数**（`PluginHost::new_full` 旁的 `with_backoff_schedule` builder），**不是 `#[cfg(test)]` 覆写**：`BACKOFF_SCHEDULE_MS` / `CRASH_WINDOW` / `CRASH_WINDOW_LIMIT` 是 `mod.rs:64-68` 的 `const`，而驱动这条的 `plugin_host_smoke.rs` 是集成测试，lib 的 `#[cfg(test)]` 对它不可见（与 §2.3 建表期问题同根）。
    - 选错的后果是具体的：实现者写完发现 `#[cfg(test)]` 不可用，此时最省力的出路是把测试退回「等 15 s 看状态」——**原地重造 §1.3 那条假门禁**，整个诊断白做。
    - 裸跑最少 ~15 s；`tokio::time::pause` 有先例（`tests/no_double_spawn.rs:2000`）但要同时驱动**真子进程** `child.wait()`，自动推进时钟与真实进程生命周期混用会飘。
    - **必须是构造后的 builder**（`PluginHost::new_full(...).with_backoff_schedule(..)`），**不是给 `new_full` 加参数**：`PluginHost::new_full(` 在 `crates/calm-server` 内有 **106 处**调用（实测），加参数会让 S0 变成 106 处 churn、没人读得完的 diff。builder 让这 106 处一处不动。
14. **监督器争锁**（§2.5 的两个洞，各一条确定性用例）：
    - (a) 第一段撞上 spawn 自己的 guard（子进程在 spawn 返回前就死）→ 崩溃仍被记账，终局不是「假 `Running` + 已死进程」；
    - (b) 第三段撞上一个最终无副作用的持锁操作（例如会失败的重名 install）→ 插件仍能被 respawn，不永久停在 `Crashed`。
    - **接缝**：14(a) 用 `write_in_tx` 事务 barrier 把 spawn 钉在 guard 内的 `emit_state_under(Running)`（新设计下 `:1263` 那次发射在 guard 内），配 `plugin_host_smoke.rs` 的 `CRASH_BIN`（握手后立刻退出）——子进程已死、监督器第一段撞锁，确定性成立。14(b) 由测试直接 `host.try_lock_lifecycle(id)` 持锁跨过退避窗口再释放（R7 正是为此把它设为 `pub`）。
    - 变异：把 `await_lifecycle` 换成 `try_lock` → 两条都红。

15. **监督器第三段的 DB 读失败（故障注入）**。**拆成两条**——r5 把「恢复后仍 `enabled=true` 才 respawn」与「用绕行把 DB 改成 disabled」写进了同一条 fixture，两者不可兼得，不拆实现者会二选一，且多半选掉见证强的那条：
    - **15a（活性）**：DB 保持 `enabled=true`，注入一次读失败 → 失败期**不** respawn、条目仍 `Crashed`；恢复读取 → 这次 respawn。
    - **15b（正确性 + 变异见证）**：用 §2.3 登记的 `s.repo` 绕行直接 `plugin_update_enabled(id,false)`（纯 DB 写，`live` 与 `run_epoch` 都不动），再叠一次读失败 → 失败期不 respawn，恢复后**永不** respawn。变异「把 fail-closed 改回跳过 DB 检查」在此红：变异体在读失败那一刻就 `live.remove` + `spawn_under`，重生了一个 DB 上已 disabled 的插件。
    - **接缝**：`PluginHost` 收的是庞大的 `Arc<dyn RouteRepo>`，仓内**没有**一次性读失败注入器——只有 `SqlxRepo` 一个实现（`MockRepo` 因 #4 被刻意删除），按 `RouteRepo` 逐方法委托是 600–900 行样板；`pool().close()` 是**永久**失败，喂不出 15a 的后半。
      → **把监督器第三段用到的 DB 读写收窄成一个独立窄 port**：`#[async_trait] pub trait LifecycleDb: Send + Sync { async fn enabled_row(&self, id:&str) -> Result<Option<bool>>; async fn set_enabled(&self, id:&str, v:bool) -> Result<()>; }`。生产实现委托 `repo`；默认注入，测试替换。
    - **注入路径必须写死**：`new_full`（`mod.rs:648-670`）是全量 `Self { … }` 构造、字段私有、仓内**没有任何 `with_*` builder`。所以 port 走与验收 13 同一个构造后 builder：`PluginHost::new_full(..).with_lifecycle_db(..)`。trait 必须 `pub`（集成测试是外部 crate）且 `#[async_trait]`（与 `RouteRepo` 系一致，否则 `dyn` 不可用）。
    - port 上还要有「失败已被消费」通知 + 恢复后重试的暂停闸，否则 fail-closed 的有界重试会在断言之前就成功，这条会飘。
    - **这些都要计进 S1 量级**（port + 生产实现 + builder + 两条测试重写）。

16. **`disable` 的内部顺序**：**用验收 15 的同一个 port，不要用 DB 屏障。**
    - 判据：测试的 `LifecycleDb` 假实现在 `set_enabled` **被调用的那一刻**同步读 `host.status(id)`——新顺序（stop 先）必为 `None`，旧顺序（写库先）仍是 `Some(Running)`。变异「把体内两句换回旧顺序」直接红。零 DB 锁、零时序漂移。
    - **不要用 `write_in_tx` 做窗口屏障：那条在本仓是结构性假绿。** `tests/` 用 `sqlite::memory:`（shared-cache），而 `PRAGMA journal_mode = WAL` **对内存库是 no-op**——没有 WAL，读者没有快照隔离（`calm-truth/src/db/sqlite/deadlock_semantics_tests.rs:49-51` 已就此立过案）。`plugin_update_enabled` 是一条 autocommit `UPDATE`（`out_of_domain.rs:405-412`，走 `&self.pool`），**旧顺序下它会直接 park 在同一个屏障上、永不提交**，于是窗口内读到的仍是 `true` —— 新旧两种顺序同样绿。
    - r4 初稿那版（持锁 → disable → 断言 409 且 `enabled` 仍 true）同样惰性：`disable` 在**入口**就 `Busy` 返回，函数体一行没跑，换回旧顺序照样绿——那只是重复了验收 1。
    - `StubServer::start_gated` 也钉不住这一侧：`stop()`（`mod.rs:1655-1720`）对 connector 完全不发 HTTP。

**现有测试的处置（不能留着当装饰）**：

| 测试 | 处置 |
|---|---|
| `connector_host.rs:2365 two_emitters_for_one_connector_never_interleave` | **重写**。本设计下 `stop()` 在取锁处就拿到 `Busy`（或被阻塞），根本走不到发射临界区——**即便把发射锁整个删掉，`peak_concurrent_state_emits()` 也永远是 1，测试照绿**。改为对生命周期锁的变异见证 |
| `plugin_host_smoke.rs:233 crash_loop_disables_after_threshold` | **重写**。§1.3 判定为假门禁，与验收 13 直接冲突；不改会出现「验收 13 红着、老测试绿着」，实现者会去迁就老测试 |
| `state_emit_peak` / `peak_concurrent_state_emits`（`:345` / `:1949`） | 说明改测什么，或连同测试一起**退役**。留一个恒真探针比没有更糟 |
| `connector_host.rs:1969 uninstall_during_an_in_flight_spawn_…` | 它直接调 `registry().remove()` 绕开路由；`:1987` 那条「there is no per-plugin lifecycle lock (risk R12)」的注释会变成谎言。验收 5 升级为走真实路由的版本 |
| `connector_host.rs:1522 / :2166`（boot 预算丢弃 spawn future） | 保留并跑。注意它们**不再是死锁探测器**：try 语义下 guard 泄漏表现为快速 `Busy`，不是挂起 |
| `plugin_host_smoke.rs:191 crash_stub_respawns_after_first_crash` | 必跑：§2.5 的监督器洞会让它红或飘红 |
| `connector_host.rs:2322` / `:1377`（boot 上界） | 新增的取锁在 `:854` 的 `timeout_at` 围栏内部，boot 上界公式仍成立——不自明，写进注释 |

## §5 风险

- **R1 自重入**：tokio `Mutex` 不可重入。`*_under` 误调取锁包装 → 外部路径静默 409、内部路径（`await_lifecycle`）永久卡死。缓解：取锁包装是可枚举的小集合（§2.2 已枚举）；验收 12(a)(b) 覆盖。**仍是最可能出事的一条。**
- **R2 锁序**：`lifecycle`（异步）→ `processes`（同步）→ registry（叶子）。无环证据：`subscriptions`（`mod.rs:154`）的两个使用者是 `stop` 的 drain（`:1682`）与 `callbacks.rs:748` 的 push，而 `CallbackCtx`（`callbacks.rs:69-84`）**不持有 `PluginHost` 句柄**；registry 的全部方法（`registry.rs:169-232`）同步、无 await、不回调。
  - 一条**没有类型阻止的**反向边：传给 `write_*` 的事务闭包（`calm-truth/src/db/mod.rs:122-126`，走 `BEGIN IMMEDIATE`）若取生命周期锁。今天不存在，明写为约束。
  - r1 说「反向不可写，因为 `MutexGuard` 非 `Send`」**表述过强**：它只约束特定 `Send` future 跨 await。真正的保证是锁序 + helper 形状。
- **R3 临界区成本**：主要是 connector 的 30.5 s bring-up。（r1 担心的「跨一次子进程拆卸」虚高：`stop` 里的 `process.stop(STOP_GRACE)`（`:1698`）对被监督的 app 插件必然返回 `AlreadyDead`——`Child` 早在 `:1214` 被 `take_child()` 移交监督器，子进程实际靠 `kill_on_drop` 在监督器被 abort 时死掉。）
- **R4 持有侧无上界**：`install` 临界区含文件系统操作与多次 repo 写；一个卡死的 repo 写会让该 id 的所有外部操作拿到 409、所有内部路径排队。可接受：同样的写今天也会卡死请求本身。
- **R5 boot fallback**：`mark_unavailable` 的无锁例外**取消**（r1 的 D9 撤销）。理由不是 r1 写的「它不发事件所以安全」——它写的是 `live` 表，而 live 表就是 `GET /api/plugins/{id}` 的 `state`/`last_error`（`routes/plugins.rs:1209-1223`）与 `running_plugin_ids`（`:1786`）的来源，**它可以成为最后一次运行时表写**（例如 `stop_under` 刚 remove 并发 `disabled`，`mark_unavailable` 把 `Unavailable` 插回去 → 插件复活且无对应事件）。
  - 处置：boot fence 臂（`:877`）用**同步** `try_lock`（`OwnedMutexGuard::try_lock_owned` 无需 await，因此 `mark_unavailable` 保持同步、`MAX_CONNECTOR_AUTOSPAWN_WALL` 公式不受影响）；拿不到就只记日志。**放弃的是可观测性契约（`:1526-1536`），不是正确性**，且窗口极窄（被丢弃的 spawn future 刚 drop 掉 guard）。
- **R6 boot 期不是结构性安全**：`autospawn` 与 REST enable 在生产上撞不上（`AppState::new` 内联 await autospawn（`state.rs:1201`），HTTP listener 之后才 bind（`main.rs:196`）），但**不能用时序当证明**——MCP server 与 dispatcher 先于 autospawn 建立（`state.rs:1081/:1132/:1182`），且 `autospawn_enabled_within` 是 `pub`。真正可达的对手是运行期 REST 与监督器争锁（§2.5）。
- **R7 `try_lock_lifecycle` 是 `pub`**：验收 1、3 需要测试持锁。外部拿不到任何 `*_under`（私有），最坏是自我阻塞。
- **R8 切片间的中间态**（r2 那句「不产生新的坏状态」**是错的**）：若 `Busy → 409` 的错误映射留到 S2，S1 单独合入后 `disable` 会：`plugin_update_enabled(false)` 已落库（`routes/plugins.rs:452`）→ `stop` 返回 `Busy` → `:459` 的 catch-all → **500，且 `enabled=false` 与「插件仍在跑」永久分叉**——这是今天不可能出现的坏终局。`enable` 同形（`spawn_error_to_calm` 的 catch-all `:1096` → 500 而 `enabled=true` 已写）。
  - **处置**：`LifecycleBusy → 409` 的映射以及 `disable`/`uninstall`/`reload` 三处 `stop` 的 `Busy` 分支**放进 S1**（S1 本来就要动 `HostError`）。

## §6 决定

- **D1** 一把锁而非两把；per-id 而非全局（§2.1）。
- **D2** 入口取锁 / 内层带证 / id 从 guard 读；成对拆分点已枚举（§2.2）。
- **D3** 删除 `emit_state(id, status)`（§2.2）。
- **D4** 五个复合操作搬进 `PluginHost`（不扩宽写面）；registry 三个写方法收 `&LifecycleGuard`、字段私有化、`registry()` 只读；**「类型封闭」主张撤回**，路由层 `s.repo` 的残留如实登记 + 验收 1 保留登记表（§2.3）。
- **D5** **两种获取语义**：外部非阻塞 `try` → 可判别的 409；内部 `await_lifecycle` 用于监督器与 boot reconciliation。无等待预算常量。`Busy` 的终局显式定义（§2.5）。
- **D6** `ProcessTable.spawning` 保留（§2.4）。
- **D7** 监督器三段式 + 每实例 `run_epoch`（创建时捕获、三处判定、attempt 独立字段、DB 读失败 fail-closed）+ **崩溃窗口显式搬运**，一并修 §1.3（§2.6）。
- **D8** **跨 crate 构造权移出本设计**，另立 **#1210**，推荐类型围栏（§2.7）。
- **D9** `mark_unavailable` 的无锁例外取消，boot fallback 用同步 `try_lock`（§5 R5）。
- **D10** `plugin_install` 的 UPSERT 不动（§3，理由已更正）。

## §7 切片

r3 的 S1/S2 划分**不成立**（两路各自指出）：S1 让 registry 写方法收 guard，而 install/uninstall/reload 路由到 S2 才搬迁；临时只在 registry 写点取锁会产生新的半提交终局，跨路由全程持锁又会让 `stop`/`spawn` 包装自重入。而 `disable` 在调 host 之前就写了 DB（`routes/plugins.rs:452`），把 500 换成 409 **不能撤销那次已提交的写**——终局仍是「`enabled=false` 且插件仍在跑，且下次 boot 的 autospawn 因 `enabled=false` 不会来和解」，是今天不可能出现的持久坏终局。

改为按**「有无行为变更」**切，而不是按模块切：

| 切片 | 内容 | 行为变更 | 量级 |
|---|---|---|---|
| **S0** | 纯机械准备，**零行为变更**：① `PluginRegistry` 建表期消费型构造（24 处集成测试 + 9 处 lib 内 `#[cfg(test)]`，§2.3）；② 退避表 / 崩溃窗口改**构造后 builder**（`…new_full(…).with_backoff_schedule(..)`；`new_full` 的 **106 处**调用一处不动，验收 13）；③ 五个复合操作原样搬进 `PluginHost`（**先不加锁**），路由改薄 | 无 | ~500–600 行，绝大多数是调用点改写 |
| **S1** | 锁本身（含验收 15 的 `LifecycleDb` port + 生产实现 + builder 注入 + 两条现有测试重写）：`LifecycleCell` / 两种获取语义 / `*_under` 拆分 / 删 `emit_state` / registry 运行期写方法收 guard / 监督器三段式 + `run_epoch` + 崩溃窗口搬运 + §1.3 修复 / `HostError::LifecycleBusy` + 409 映射 / `disable` 顺序 | 有 | ~800–1000 行 |

S0 行为不变，因此可独立合入、独立评审，且**不产生任何中间态坏终局**——这正是 r3 划分做不到的。S1 合入后 #1196 与 #1169 一并关闭（两者不再分属两个切片：`disable` 的顺序、registry 的收窄、复合操作的锁，都是同一把锁的组成部分，拆开就会重现上面那条半提交终局）。

S0 的价值不只是让 S1 可评审：那 24 处集成测试的改造若混在 S1 里，评审注意力会被调用点淹没，而 S1 的每一行都是并发语义。

### S0 的「零行为变更」要钉死在哪里

搬迁本身是安全的：五个 handler 用到的 repo 方法（`plugin_install` / `plugin_update_enabled` / `plugin_update_manifest` / `plugin_delete` / `plugin_token_delete` / `plugin_kv_clear` / `overlays_clear_by_plugin`，`calm-truth/src/db/sqlite/out_of_domain.rs:373` 起）**全部非事件化、不带 actor、单语句无跨调用事务**，所以「搬进 host 会不会改事务边界 / `PluginState` 的 actor 与 scope」这两个问题是空的。

真正会漏的只有**错误面**，而它恰恰是 S0 唯一的卖点所在：

- `install`：`CalmError::PluginInstall`（`routes/plugins.rs:1127-1187`）、`PluginConflict`（`:350`）、`PluginKernelTooOld`（`:344`）；repo 错误经 `From<TruthError> for CalmError`（`error.rs:240-251`）保留 `Db` / `NotFound` 分档。
- `enable` / `reload`：`spawn_error_to_calm`（`:1077-1098`）的五臂映射（422 / 409 / 503 / 400 / 500）。
- `disable` / `uninstall`：`HostError::NotFound` 视为良性，其余包成 `Internal("stop failed: …")`。

`HostError`（`plugin_host/error.rs:89-173`）里**没有任何变体承载前两类**，而 host 内现存的先例是把 repo 错误压成 `BadState`（`mod.rs:711-715`）。照抄它，install 的 409 会变 500、400 会变 500——OpenAPI 上写着的响应码当场失真，还贴着「零行为变更」的标签独立合入。

**因此钉死**：

1. 五个 host 方法返回 `Result<_, CalmError>`，`spawn_error_to_calm` 一并搬进 host；路由只剩 `build_detail` + 状态码。
2. `build_detail` 的读时点原样保留（今天 enable / disable / reload 都在运行时步骤**之后**重读一次行）。
3. `Manifest::parse` / `resolve_install_source` 留在路由（§2.3 的取锁点依赖它）。
4. **S0 必须同时把 `PluginRegistry::insert` / `remove` / `set_exposes_tools` 降到 `pub(crate)`**，否则编译器不强制那 24 处迁移，S0 会以「builder 加好了、老 `insert` 还 `pub`、调用点一处没动」的形态合入，24 处洪水原样冲进 S1——正是这次切片要避免的事。
5. **前置 `plugin_get_by_id` 的 404 必须保留**（`:416` / `:451` / `:528` / `:616`，不只是钉子 2 说的 `build_detail` 那次**后**读）。丢了它，`reload` 从 404 变成读盘失败的 400/500。

> **S0b 跟进更正**：原文这里还写着「`uninstall` 会从 404 变 204（`plugin_delete` 不返回 `NotFound`）」——**事实相反**，`plugin_delete` 在 `rows_affected()==0` 时就返回 `NotFound`（`out_of_domain.rs:469-471`）。`enable`/`disable` 同样被 `plugin_update_enabled` 兜住（`:413`）。所以四个探针里只有 `reload` 的是单独可观测的；另外三个是与 repo 层重复的防御。要守的是端点契约，见下面「钉子 5 / 6 / 7 的门禁」。
6. **`reload` 的条件重生**：`:666` 是 `if plug.enabled` 才 spawn，且 `plug` 是 **stop 之前**读到的那一行（`plug.install_path` / `plug.enabled` 都来自它）。§2.3 的表已同步。无条件 spawn 会把一个已禁用插件拉起来——一个贴着「零行为变更」标签的行为变更。
7. **`uninstall` 的三次 `let _ =` 吞错**（`:540-542`：token / kv / overlay）是**刻意**的（注释在 `:536-539`）。搬进 host 后最自然的写法是 `?`，那会让「overlay 清理失败」从静默变 500。明写为契约。
8. **评审证据**：每个 handler 的错误码集合前后逐条对照表。没有这张表，「零行为变更」是一句自称。

#### 钉子 5 / 6 / 7 的门禁（S0b 跟进补齐）

S0b 首版把 5 / 6 / 7 写进了代码与注释，但**没有任何测试会因为删掉它们而变红**。跟进提交在 `crates/calm-server/tests/cases/plugin_routes.rs` 补了五条：

| 测试 | 钉住 |
|---|---|
| `enable_unknown_id_returns_404` | 钉子 5（`lifecycle.rs:133` 探针） |
| `disable_unknown_id_returns_404` | 钉子 5（`:155` 探针） |
| `uninstall_unknown_id_returns_404_not_204` | 钉子 5（`:178` 探针；丢了就是 204） |
| `reload_unknown_id_returns_404_not_manifest_read_error` | 钉子 5（`:215` 探针；丢了就是读盘 400） |
| `reload_disabled_plugin_does_not_spawn` | 钉子 7（`:264` 的 `if plug.enabled`），断言 `PluginHost::status()` 仍为 `None`，不只断 HTTP 码 |

五条均已做变异验证，并顺带证伪了钉子 5 自己的论据：

- **`plugin_delete` 其实会返回 `NotFound`**（`out_of_domain.rs:469-471`，自 #899 拆分起就在）。钉子 5 写的「丢了探针 `uninstall` 会从 404 变 204」是错的——单独删掉 `uninstall` 的探针，门禁全绿。
- 同理，单独删掉 `enable` / `disable` 的探针也全绿：`plugin_update_enabled` 的 `rows_affected()==0` 与其后的 `plugin_get_by_id().ok_or_else()` 是两层兜底，且 `disable` 结尾还有一次重读，产出的 `CalmError::NotFound(format!("plugin {id}"))` 与探针**逐字节相同**。
- 唯一单独可变异的是 **`reload` 的探针**：删掉后 404 立刻变成读盘 400（实测 `left: 400 / right: 404`）。

**因此钉子 5 的正确表述是**：这三个探针今天是与 repo 层重复的**防御**，不是唯一防线；要守的是**端点契约**（未知 id ⇒ 404），门禁钉的正是契约。三条的决定性变异需要同时打掉 repo 兜底（`let _ = plugin_update_enabled(...)` / `let _ = plugin_delete(...)`），此时 `enable` 变 500、`disable` 变 200、`uninstall` 变 204，三条分别变红。S1 若把 repo 调用换形（例如收进 guard 后改用别的写法），契约就只剩探针撑着——这正是补门禁的理由。

钉子 6（`uninstall` 的三次 `let _ =` 吞错）仍只是契约注释，无门禁：把它们改成 `?` 只在 repo 清理失败时可观测，而 sqlite/mock 两个后端都不会失败——要门禁得先有可注错的 repo 接缝，留给 S1。

## §8 评审账本

### r1 → r2

**事实更正**：竞争共同根因、竞争 4 终局、`callbacks.rs:1015` 非生产调用方、「类型围栏必假」、「`MutexGuard` 非 `Send` ⇒ 反向边不可写」过强、全局锁否决理由、`process.stop` 成本。
**结构改动**：复合操作搬进 host；有界等待 → 非阻塞 `try`；`generation` → 每实例 `run_epoch`；围栏 1 入口 → 4 入口；`mark_unavailable` 无锁例外取消。
**新增发现**：§1.3 崩溃计数归零；`two_emitters_…` 将退化为假门禁；`state_emit_peak` 去留。

### r2 → r3（两通道再次各自判 REVISE，阻断项重合）

- **D5 一刀切非阻塞是错的**（两路同时给出）：监督器没有重试者，第一段**必然**撞上 spawn 自己的锁，一次 `try` 就放弃 → 崩溃不记账 + 永久假 `Running`；第三段同理 → 永久 `Crashed`。→ 拆两种语义，新增验收 14。
- **D4「类型封闭」撤回**：`pub registry` 字段 + `registry()` + `PluginRegistry` 三个 `pub` 写方法是完整的无 guard 写路径，且恰是竞争 1/4 的写面；路由层 `s.repo` 同理。→ 收窄 registry + 残留登记 + 保留登记表。
- **D8 移出本设计**：typed 自由函数（约 40 处调用）结构上拦不到；`plugin_host` 自己（`:43` 只 import `RouteRepo`）就在四入口之外——**今天唯一的 `PluginState` 写点本就不被围栏覆盖**。三轮各挖出更深一层 ⇒ 重跑降范围决定。
- **D7 计数搬运没有机械形状**：`run_epoch` 只判身份；补显式搬运 + attempt 独立字段 + DB 读失败策略。
- **R8 错**：S1 中间态会产生今天不存在的坏终局（`enabled=false` + 插件仍在跑 + 500）→ 错误映射移进 S1。
- **验收按 reject 语义重写**（3/5/6/7/8：败者拿 `Busy` 不会自动续跑，要显式重试）；`LifecycleBusy` 需可判别错误码（与 `PluginConflict` 都是 409）；验收 1 改每入口独立 fixture；验收 12 补「无竞争不得 `Busy`」；验收 13 点名时间接缝。
- **新增假门禁**：`plugin_host_smoke.rs:233 crash_loop_disables_after_threshold` 今天绿着且自称验证 §1.3 的阈值，实为崩溃循环本身即可满足。

### r3 → r4（两通道第三轮，均判「主干可实现、阻断项收窄为机械问题」）

- **切片按模块切不成立** → 改为按「有无行为变更」切：S0 纯机械零行为变更，S1 锁本身。`disable` 的 DB-写-在前是唯一脏的那个复合操作，顺序在 S1 内翻转（§2.6）。
- **DB 读失败 fail-open 不安全** → 改 fail-closed。反例：disable 写库成功后 future 被取消，live 条目与 epoch 都没变，此时一次读失败会让监督器 respawn 一个已禁用的插件（§2.6）。
- **registry 收窄撞上 24 处建表期调用方**，且 `#[cfg(test)]` 逃生舱对集成测试不可见 → 建表期/运行期两种写在类型上分开，否则实现者会加一个 `insert_unlocked` 当场作废收窄（§2.3）。
- **验收 13 的接缝定死为构造参数**：`#[cfg(test)]` 覆写对 `tests/` 不可见，选错会原地重造 §1.3 那条假门禁。
- **验收 5/6/7/8 必须点名 barrier 并断言观测到 `LifecycleBusy`**：否则胜者先跑完时败者根本不撞锁，测试全绿却一次没测到锁。
- **验收 2 如实降级**：只证明函数被删；「唯一发射器」是评审确认的性质，不是门禁。
- 新增验收 15（DB 读失败故障注入）、16（`disable` 顺序）；验收 14 点名接缝。
- 措辞更正：`exceeded` 分支只停 respawn、不写 `enabled=false`；`load_from_dir` 不走 `insert`。

### r4 → r5（第四轮窄范围确认，两路各自 REVISE，均只剩验收与机械项）

- **fail-closed 的理由被自己的顺序翻转打死**（两路同时指出）：r4 举的「disable 写库后取消」窗口在新顺序下不存在，验收 15 的变异见证因此跑不出红。改用 §2.3 登记的 `s.repo` 绕行作为理由**与**驱动路径。
- **验收 15 没有接缝**：仓内只有 `SqlxRepo` 一个实现（`MockRepo` 因 #4 被刻意删除），`RouteRepo` 约 100 个方法，装饰器 600–900 行；且 `pool().close()` 是永久失败，喂不出「恢复后才 respawn」那一半。→ 把第三段的 DB 读收窄成独立窄 port（另一可选形状：照 `test_seams.rs:34 crash_point()` 的 fixtures 门控计数器）。
- **验收 16 变异惰性**：`disable` 在入口就 `Busy` 返回，函数体一行没跑，换回旧顺序照样绿 → 改窗口内观测。
- **S0 的「零行为变更」只在错误面会漏**，且 `HostError` 无变体承载 `PluginInstall` / `PluginConflict` / `PluginKernelTooOld` → 五个 host 方法返回 `CalmError`，`spawn_error_to_calm` 一并搬入；补 `build_detail` 读时点、`pub(crate)` 收窄、错误码对照表三条硬要求。
- **量级更正**：`PluginHost::new_full(` 有 **106** 处调用（实测），不是 ~15 → 接缝必须是构造后 builder。
- **§2.3 二分法漏了第三种形状**：`mcp_server/transport.rs:1330/1337` 在已建好的 registry 上、无 host 的情况下 `insert`。

### r5 → r6（第五轮，两路各自 REVISE，全部为验收与机械项；主干两轮无新反对）

- **验收 16 的 `write_in_tx` 屏障是结构性假绿**：`tests/` 用 `sqlite::memory:`，`journal_mode = WAL` 对内存库是 no-op、读者无快照隔离（`deadlock_semantics_tests.rs:49-51` 已立案）；`plugin_update_enabled` 是 autocommit `UPDATE`，旧顺序下它会 park 在同一屏障上永不提交 ⇒ 新旧两种顺序同样绿。→ 改用 `LifecycleDb` port 的 `set_enabled` 回调内读 `host.status`。
- **窄 port 替换不进去**：`new_full` 是全量构造、字段私有、仓内无任何 `with_*` builder → port 必须 `pub` + `#[async_trait]` + 走与验收 13 同一个构造后 builder。
- **验收 15 自相矛盾**（标题要 `enabled=true`、正文要绕行改成 disabled）→ 拆 15a（活性）/ 15b（正确性 + 变异见证）。
- **S0 的钉子漏了三个行为面**：前置 404、`reload` 的 `if plug.enabled` 与前读时点、`uninstall` 的三次刻意吞错。
- 量级更正：S1 从 ~600–800 提到 ~800–1000（port + builder + 测试重写）。
- 措辞：§2.6 过强的「没有任何可构造场景能让它红」改掉；§2.3 第三种形状撞墙点从 S0 更正为 S1；验收 3 的屏障注释改为「卡住全库所有写」。

（r6：主干已两轮无反对，剩余为实现期纪律）
