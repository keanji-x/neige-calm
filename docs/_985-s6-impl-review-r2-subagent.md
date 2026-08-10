# #985 切片 6 PR-A 实现评审 r2（channel = subagent）

对象 `9d30006a` → `98e3acba`。工作树 `/tmp/wt6b`，`NEIGE_CODEX_BIN` 未设置，
`CARGO_BUILD_JOBS=6`，PATH 含 `/mnt/data2/kenji/neige-calm/.local-bin`。
变异实验结束后已 `git checkout -- .` 复原，复原态 26 passed（§5）。
分级：**BLOCKER ×2，MAJOR ×1，MINOR ×4。**

## 1. 修复轮 1 的七处，哪几处修出了新洞

**B1 修出新洞（BLOCKER-1）；M3 修出新洞（BLOCKER-2）；#21c 修出新洞（MAJOR-1）。
B2 / B3 / M1 / M2 / M4 没有，我的定向攻击没打穿。**

### BLOCKER-1 — B1 把外部 teardown 搬进 `BEGIN IMMEDIATE`：持写锁等待非-sqlite 资源

**结论**：leaf 守卫前移是对的，但把 **进程 kill / UDS RPC / 第二条池连接上的读**
一并塞进同一个写事务，违反本仓库自己写下的 #930/#1016 单写者纪律，现有扫描器结构上看不见。
**攻击 / 证据**：

- `crates/calm-server/src/routes/waves.rs:1406` 起，同一写事务内依次做：
  `:1408` `interrupt_shared_card_active_turn`（→ `routes/cards.rs:78`
  `repo.session_projection_active_for_card()`，实现是
  `calm-truth/src/db/sqlite/session_projection.rs:416` 的 `..._from_pool`
  —— **持写锁期间再从池里取一条连接**；随后 `shared_codex_appserver.rs:1247`
  `resolve_active_thread_for_card` 又取一次，再走 `:1218` `client.turn_interrupt` 的 UDS RPC）；
  `:1416` `reap_terminal_artifacts_with_renderer`（`terminal_sweeper.rs:97`
  `GRACEFUL_KILL_TIMEOUT = 5s`，**每个 terminal 最多 5 秒**）；
  `:1424` `harness.shutdown().await?`（`harness/run_loop.rs:189` 起 daemon interrupt RPC ×2）。
- 数字对撞：`calm-truth/src/db/sqlite/mod.rs:216` `PRAGMA busy_timeout = 5000`。
  **一个挂死 terminal（5s）就吃满全库写者的 busy 预算**，并发 `BEGIN IMMEDIATE`
  不是变慢而是直接 SQLITE_BUSY 报错；N 个 terminal 线性叠加。
- 与 `tests/cases/deferred_write_tx_invariant.rs:1-30` 明写的禁忌同族：“ANY explicit
  transaction that already holds table locks and then parks on another table”。B1 后
  写事务第一条语句 `wave_require_leaf_tx`（`db/sqlite/wave.rs:229` 的 `SELECT id FROM waves`）
  先拿 R(waves)，然后 **parks 在 sqlite 看不见的资源上**（进程等待 / socket / 另一条池连接）；
  unlock_notify 只看得见一侧，这类环无人能打断。
- 守卫抓不到：`deferred_write_tx_invariant` 只词法扫描 `.begin(` / `::begin(` /
  `begin_with(`，对「写事务闭包里做外部 IO / 二次取连接」无感。
  `deferred_read_tx_deadlock_repro.rs:26-29` 列举的「生产 DELETE 写序列」已过时。

**我实际跑过的验证**（Mut-C）：在 `routes/waves.rs:1425` 后插入
`tokio::time::sleep(Duration::from_secs(6))`（模拟**一个**挂死 terminal，仍在
`GRACEFUL_KILL_TIMEOUT` 预算内），跑
`-E 'test(wave_delete) or test(acceptance_20) or test(acceptance_13*) or test(acceptance_18)'`
→ `Summary [6.252s] 25 tests run: 25 passed, 2266 skipped`。
**25/25 全绿**，含 `wave_delete_shuts_down_active_spec_harness`、
`acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal`、
`terminal_lifecycle::wave_delete_reaps_every_terminal_under_wave`，以及**专打 wave-delete
写者的** `deferred_read_tx_deadlock_repro::read_only_deferred_wave_detail_closes_a_deadlock_cycle_with_the_wave_delete_writer`。
时长从 0.2s 涨到 6.1–6.2s，**没有任何断言把这 6 秒变红**（`.config/nextest.toml` 的
`slow-timeout` 在 default/ci 都是 warn-only）→ **写者锁持有时长在本片是无约束、无观测的量。**

**最小修法**（二选一，推荐 1）：(1) teardown 移回事务外，事务外先做一次**只读** leaf 预检
（拒绝时不 teardown）；check-to-delete 窗口仍由 `wave_delete_tx`（`wave.rs:222`）
**事务内那次**复核关闭 —— B1 想买的两件事都拿到，且不持锁做外部 IO。
(2) 若坚持持锁：整段 teardown 包 `tokio::time::timeout`，上限**显著小于** `busy_timeout=5000`
（如 1s），超时回滚返 503，并加一条断言持锁时长上界的测试（可复用
`deferred_read_tx_deadlock_repro` 的双 oneshot 编排）。两条路都要更新上述两处失真文档。

### BLOCKER-2 — M3 删掉的源码 oracle 是 guard 站点 #2 的**唯一**覆盖，替换品只覆盖站点 #1

**结论**：`98e3acba` 删除了 `scheduler/tests.rs` 中断言
`"WHERE child.id=?5 AND child.lifecycle='done')"` 出现 **2 次**的
`acceptance_18_child_success_flip_rechecks_child_state_in_its_sql_guard`
（原注释即 “success and pending-incomplete flips must **each** recheck child Done”）。
新增的 `acceptance_18_success_flip_rechecks_done_after_its_snapshot` 只驱动抽出的
`guarded_child_success_flip_tx`（站点 #1）。**站点 #2（`child-wave-incomplete` 臂，
`scheduler/mod.rs:738`）的 child-Done 复核现在没有任何测试覆盖。**
**我实际跑过的验证**（Mut-A）：把 `scheduler/mod.rs:738` 的
`"EXISTS(SELECT 1 FROM waves child WHERE child.id=?5 AND child.lifecycle='done') AND ..."`
改成 `"1=1 AND ..."`，跑 `acceptance_13` / `acceptance_13b_and_13c` / `acceptance_13d` /
`acceptance_13e` / `acceptance_18_success_flip_rechecks_done_after_its_snapshot`
→ **全部 PASS**（即上面那次 25/25 绿的运行）。

典型的「修复在刚修完的位置开新洞」：r1 指出源码计数是弱 oracle，修复方换成行为测试，
但只换到两个站点中的一个。**最小修法**：把该臂 guard 同样抽成
`guarded_child_incomplete_flip_tx(..)`，在新 `acceptance_18` 加第二维度
（事务内先观察 Done → delete/reopen child → 断言 `rows_affected == 0`）。不要把源码计数加回来。

### MAJOR-1 — #21c 换了被测性质：设计要求的「删 cove 失败」半条无人覆盖

设计 `docs/_985-s6-design.md:725` 的 #21c 是**两条**：(a) 全表断言每条
`parent_wave_id IS NOT NULL` 的行同 cove；(b) **手工造跨 cove 边 ⇒ 删 cove 失败**。
修复删掉了 `calm-truth/src/db/sqlite/sub_wave_tree_tests.rs` 里的
`acceptance_21c_cross_cove_edge_is_a_loud_delete_tripwire`（(b) 的唯一实现），
新增的 `child_wave_adapter.rs:653` 只实现 (a)，末尾 `:696` 反而是一条**正例**
`cove_delete_tx(&mut tx, &second_cove).await.unwrap()`。

**验证（Mut-F）**：`grep -rn cove_delete_tx crates` → 全仓仅剩两处测试内调用
（`sub_wave_tree_tests.rs:97`、`child_wave_adapter.rs:696`），**都是 `.unwrap()` 正例**。
没有任何测试断言跨 cove 边会让 cove 删除失败 ——「loud」所指的性质整条消失。
**最小修法**：把删掉的 tripwire 原样加回；它与 adapter 测试不是替代关系
（一条钉 FK 是 NO ACTION 而非 CASCADE/SET NULL，一条钉 adapter 不写这种边）。

### 未打穿的四处

- **B2（25ms 调度窗）**：25ms 只是等 driver 落库 observer 的余量，不是同步点 —— 变异体
  （`mark_sub_wave_running` 提到 `wait` 前）在程序序上**严格早于** observer 被 spawn，
  `entered` 触发时必然已 `running`，与 sleep 时长无关。实测 12 路 `yes` 满载下重复跑
  `acceptance_19` + `acceptance_7_two_cycle` **20 轮，FAILED_RUNS = 0 / 20**。
- **B3**：`child_wave_adapter.rs:240` 的 `UPDATE tasks SET child_wave_id=COALESCE(...)`
  与 child INSERT 在**同一个 `prepare_tx`** 提交，无「child 已存在但未 stamp」的可见中间态。
- **M1**：`concat!` 编译期展开，两常量物理含同一片文本，遍历断言删宏体两条同红；
  被测对象**就是**那两个生产常量，与 r1 批评的 `include_str!("mod.rs")` 计数不同形状。
- **M2 / M4 无恒真断言**：M2 三条是**单变量负例表**（三行 fixture 各只偏离一个字段，
  断言 `changed == 0` 且状态未被改写）；M4 靠 `child_wave_adapter.rs:415` 给**父** wave
  写 `archived_at=101,pinned_at=102,lifecycle='done',terminal_at=103` 非默认值，
  子行断言 `None`/`draft` 不会被新 wave 默认值遮蔽 —— 正确的防恒真做法。

## 2. 我自己设计的变异里，哪几条打穿了

| # | 变异 | 目标 | 结果 |
|---|---|---|---|
| Mut-A | `scheduler/mod.rs:738` guard → `1=1 AND ...` | acceptance_13/13b/13c/13d/13e/18 | **打穿**：6 条全绿 |
| Mut-C | `routes/waves.rs:1425` 后插 6s sleep（持写事务） | 全部 wave-delete / acceptance_20 / deadlock-repro | **打穿**：25/25 全绿 |
| Mut-D | 12 路满载下重复 `acceptance_19` + `acceptance_7` ×20 | 25ms 窗 / 500ms / 1s timeout | 未打穿：0/20 失败 |
| Mut-E | 审 `child_wave_id` stamp 与 child INSERT 的事务边界 | B3 自查顺序 | 未打穿：同事务原子 |
| Mut-F | `grep` 全仓 `cove_delete_tx` 断言极性 | #21c (b) 覆盖存在性 | **打穿**：仅剩两条正例 |

关于 `slow-timeout` warn-only：本片**没有**「靠 nextest 超时表现失败」的断言
（#7 已由 M1 换成显式 `tokio::time::timeout(500ms)`），这点修得干净；
但反过来，warn-only 正是 Mut-C 能全身而退的原因。

## 3. SLOW 测试的基线对比数字

`calm-server::migration_suite migration_replay_harness::synthetic_fixture_replays_from_every_version`，
单跑独占，同一工作树 `git checkout` 切换：

| 提交 | 迁移条数 | 实测 | 备注 |
|---|---|---|---|
| `98e3acba` | 71 | **76.263s** | 首跑（冷）|
| `9d30006a` | 70 | **24.830s** | 基线 |
| `98e3acba` | 71 | **25.978s** | 复跑 |
| `98e3acba` | 71 | **25.963s** | 复跑 |
| `98e3acba` | 71 | **28.307s** | 复跑 |

**76.263s 不可复现，是首跑/冷启动离群点，不是本片退化。**同条件对比
**24.83s → 25.96 / 25.98 / 28.31s，即 +1.1 ~ +3.5s（+5% ~ +14%）**，与「多一条迁移」
量级相符。**本片没有引入 3× 退化。增长形状：O(V²)（V = 迁移条数），本来就慢，会自己撞线。**
`tests/support/migration_replay.rs:73/96`：每个版本 v 先 `stage_db_at`（跑 v+1 条）
再 `replay_to_head`（跑 V−v 条），每条一个独立事务 + WAL fsync（`:57` 真实文件 + WAL），
总 apply ≈ Σ_v (v+1)+(V−v) ≈ V(V+1) ≈ V²。V:70→71 多约 141 次 apply，实测 +1.1~3.5s
→ 每次 ~8–25ms，与 sqlite3 CLI 单条实测一致（`0071_sub_wave_tree.sql` 未进最慢 8 条，整链
每条 ~20–30ms）—— **没有单条迁移病态慢**，成本纯是次数的二次增长。外推 V=100
约 `26 × (100/71)² ≈ 51s`，V≈150 越过 ci 的 120s 线。**建议单开 issue**（不阻塞本 PR）：
每版本 stage 从最近缓存快照增量前进，V² → V。

## 4. MINOR

- **MINOR-1**：`tests/scheduler.rs` 的 `sweep.await.unwrap()` /
  `recovered_sweep.await.unwrap()` 无外层 timeout，配合 warn-only slow-timeout，
  「变异导致挂死」表现为 CI job 级超时而非测试红。建议包 `tokio::time::timeout(30s)`。
- **MINOR-2**：`child_wave_adapter.rs:415` 把**父** wave 改成 `lifecycle='done',terminal_at=103`，
  而 `seed_parent` 被 #5/#5b/#6/#7/#8/#21c **全部**用例共享 —— 它们现在都在「父 wave 已终态」
  前提下建子 wave 并通过，顺带把「终态父不该再开子 wave」从测试语义里抹掉。
  建议加 `lifecycle` 参数，只在 M4 用 `done`。
- **MINOR-3**：`routes/waves.rs:1424` 的事务内 `?` 早退 —— harness 已出 registry、terminal
  已 SIGTERM，但事务回滚、wave 仍在库（撕裂态）。B1 之前也存在，**不是本轮新洞**；
  但 `acceptance_20_descendant_refusal_*` 只覆盖「拒绝前不 teardown」。可留后续切片。
- **MINOR-4**：`terminal_sweeper.rs:220-224` 与 `deferred_read_tx_deadlock_repro.rs:26-29`
  两处文档被 B1 改坏，随 BLOCKER-1 一并更新。

## 5. 可以合入了吗

**NO。**最小阻塞集两条：

1. **BLOCKER-1**：外部 teardown 移出 `BEGIN IMMEDIATE`（推荐 §1 修法 1），
   或加显著小于 `busy_timeout=5000` 的硬超时 + 一条持锁时长上界测试；顺带修两处失真文档。
2. **BLOCKER-2**：给 `scheduler/mod.rs:738` 的 `child-wave-incomplete` 臂补真正的 guard
   行为测试（抽函数 + 事务内 delete/reopen + `rows_affected == 0`），使 Mut-A 变红。

MAJOR-1 强烈建议同轮补回（把删掉的 tripwire 原样加回，成本近似为零）；
不加则设计 §7 有一行是「声称已验收但实际无覆盖」。MINOR 1–4 不阻塞。

**复原态验证**（`git checkout -- .` 之后，定向集合
13d/18/wave_delete/20/21c/19/7_two_cycle/12）：`Summary [0.448s] 26 tests run: 26 passed, 2265 skipped`。

## git status --short

```
?? docs/_985-s6-impl-review-r2-subagent.md
```
