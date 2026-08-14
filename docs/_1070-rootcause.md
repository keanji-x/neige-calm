# #1070 第 1 步：定性与确定性复现

基线：`origin/main` / `42767c5b9115cea8d956c119281cb7a5fa3d1de5`。

## 结论

1. **`SQLITE_LOCKED (6) / database is deadlocked` 是旧测试 helper 制造的测试基础设施问题，不是当前生产事务链路的互等风险。** 更准确地说，问题不在 `Boot` 使用了更小的 pool，而在旧 helper 用 deferred `pool.begin()` 绕过了生产“所有写事务均 `BEGIN IMMEDIATE`”的不变量。当前基线已由 `cc90b269`（PR #1080）把正常 `rebuild()` 改为 `begin_immediate_tx`，见 `crates/calm-server/tests/cases/task_projection_acceptance.rs:155-162`。
2. **拿到确定性红测。** 新增的默认忽略 repro 在旧 deferred 事务形态上显式会合锁，第一次运行就在 `tasks_rebuild_tx(...).await.unwrap()` 得到 `SqliteError { code: 6, message: "database is deadlocked" }`，见 `crates/calm-server/tests/cases/task_projection_acceptance.rs:165-216`。
3. **`row survived` 不能由已证实的 deadlock 路径解释，二者不能合并归因。** 删除 SQL 的错误没有被吞；PATCH 写事务会回滚并返回非 200，而测试在检查行之前先断言了 HTTP 200。`row survived` 的精确根因仍为 **UNKNOWN**，但没有发现“删除错误被吞”的独立缺陷。

## 证据 1：Boot 与生产 pool 配置

以下是读代码所得。

| 项目 | 测试 `Boot` | 生产 `SqlxRepo::open` | 判定 |
|---|---|---|---|
| 入口 | `SqlxRepo::open("sqlite::memory:")`，`crates/calm-server/tests/cases/mcp_wave_report.rs:128-133` | 非 `mock` 使用 `SqlxRepo::open(&cfg.db_url)`，`crates/calm-server/src/main.rs:39-46` | 两者调用同一个 builder；URL/存储介质不同 |
| `SqliteConnectOptions` | 同一实现 | 同一实现 | 显式 `from_str(url).create_if_missing(true).foreign_keys(true).log_statements(Debug)`，`crates/calm-truth/src/db/sqlite/mod.rs:209-215` |
| `max_connections` | 10 | 10 | workspace 未显式调用 `.max_connections(...)`，两者都从 `SqlitePoolOptions::new()` 开始，`crates/calm-truth/src/db/sqlite/mod.rs:217-228`；sqlx 0.8.6 默认值是 10，`/home/kenji/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlx-core-0.8.6/src/pool/options.rs:145-152` |
| `busy_timeout` | 5000 ms | 5000 ms | 未通过 `SqliteConnectOptions::busy_timeout` 设置；在每条新连接的 `after_connect` 中显式执行 `PRAGMA busy_timeout = 5000`，`crates/calm-truth/src/db/sqlite/mod.rs:217-227` |
| journal mode（请求） | `PRAGMA journal_mode = WAL` | `PRAGMA journal_mode = WAL` | builder 相同，`crates/calm-truth/src/db/sqlite/mod.rs:221-226` |
| journal mode（实际） | `memory`；WAL 对内存库无效 | 普通文件库为 WAL | 这是实际语义差异。仓库对当前 sqlx/sqlite 组合的机制说明见 `crates/calm-truth/src/db/sqlite/deadlock_semantics_tests.rs:21-51` |

内存库还有一条脱离 pool 的 keepalive connection，但它不执行查询且不占 pool capacity；用途只是防止 shared cache 随最后一条 pooled connection 消失，见 `crates/calm-truth/src/db/sqlite/mod.rs:288-309` 以及该字段说明 `:151-170`。

所以答案不是“Boot pool 比生产小”：**builder、连接上限、busy timeout 和请求的 journal pragma 一致；差异来自 `sqlite::memory:` 的 shared-cache/table-lock 语义和 WAL 无效。** `busy_timeout` 只管 `SQLITE_BUSY` 文件锁，对本次 `SQLITE_LOCKED_*` 无效，`crates/calm-truth/src/db/sqlite/deadlock_semantics_tests.rs:47-51`。

## 证据 2：`rebuild()` 与生产链路没有二次 acquire

### 这条调用链

- 当前测试 helper 在 `crates/calm-server/tests/cases/task_projection_acceptance.rs:155-162` 只 acquire 一次，然后把 `&mut Transaction` 传给 `tasks_rebuild_tx`。
- `tasks_rebuild_tx` 及其下一层对 `cards` 的读取都执行在 `&mut **tx` 上，`crates/calm-server/src/wave_report.rs:121-138`；投影也继续传同一个 `tx`，`:160-166`。
- `project_tasks_tx` 的树读取、判决和写入继续使用同一个事务，`crates/calm-truth/src/db/sqlite/task_projection.rs:1023-1040`；现有行读取用 `&mut **tx`，`:1065-1081`。
- 生产 PATCH 在一个 `write_with_actor_events_typed` closure 中依次 `wave_update_tx`、`tasks_rebuild_tx`，没有 repo/pool 调用，`crates/calm-server/src/routes/waves.rs:1333-1346`。
- 该 wrapper 的 SQLite 实现先 `begin_immediate_tx(&self.pool)`，再把唯一的 `&mut tx` 交给 closure，`crates/calm-truth/src/db/sqlite/events.rs:338-347`；closure 错误回滚，`:348-353`；成功才 commit，`:416-424`。

### 全生产面同构扫描

读代码与 AST 扫描所得：

- `crates/calm-server/src` + `crates/calm-truth/src` 中 49 个 typed write closure，closure body 内命中 `.repo.` / `.sqlite_pool(` / `.pool(` / `.acquire(` / `.begin(` 的数量为 **0**。
- 另外三条直接 trait write closure 逐条检查：`crates/calm-server/src/operation/worker_cleanup.rs:73-80` 和 `crates/calm-server/src/operation/terminal_adapter.rs:520-548` 都只调用 `_tx` helper；`crates/calm-truth/src/decision_gate.rs:279-292` 只把同一个 `tx` 传入 gate/closure，且生产代码没有 `commit_decision` 调用点。
- 24 个手写 `begin_immediate_tx` block 的余下 body 中，同样没有上述二次 acquire 形态。
- 仓库另有 fail-closed 源码门：生产 deferred transaction allowlist 为空，`crates/calm-server/tests/cases/deferred_write_tx_invariant.rs:68-93`；扫描 production source 并拒绝命中，`:101-195`。实跑该门为绿。

扫描命令的核心形式：

```text
navi sg --pattern 'write_*_typed(... move |$TX| { $$$BODY })' --lang rust ...
jq select(BODY contains repo/pool/acquire/begin)
=> closures: 49, bodies_with_repo_or_pool_acquire: []
```

**生产同构站点列表：空。** 在本次要求的两 crate 生产面中，没有发现“持事务期间，同一逻辑路径又从同池取连接”。

## 证据 3：为什么旧 helper 会得到 code 6

仓库已固定当前依赖组合的上游语义：内存 SQLite 是 shared cache，显式事务的 table locks 持有到事务结束；sqlx 的 unlock-notify 注册若闭合 waits-for cycle，就返回 plain `SQLITE_LOCKED (6)` 和 `database is deadlocked`，见 `crates/calm-truth/src/db/sqlite/deadlock_semantics_tests.rs:21-45`。

旧 helper 在 `cc90b269^` 中使用 `pool.begin()`；当前基线的 #1080 已把它替换成 `begin_immediate_tx`。新增 repro 在当前源码中保留旧形态但默认忽略：

1. deferred tx 先读取 `cards`，持有 `R(cards)`，`crates/calm-server/tests/cases/task_projection_acceptance.rs:183-189`；
2. 另一连接 `BEGIN IMMEDIATE`，取得 writer slot，然后更新 `cards` 并等待 `R(cards)`，`:191-207`；
3. deferred tx 调 `tasks_rebuild_tx`，删除预置的 pending damage 时请求 writer slot，闭环，`:209-213`。

这不是“同一个请求自己从 pool 再 acquire 一条连接”；它是**旧 deferred 测试事务与另一 writer 的 shared-cache 反向锁序**。原三次 CI 中负责交错的具体 peer 没有日志，仍为 **UNKNOWN**；但使 rebuild 成为可死锁一方的旧 helper 形态、错误码和当前依赖语义已经确定性复现。

生产写事务使用 `BEGIN IMMEDIATE`：第二个 writer 在 BEGIN 时持有零张 table lock 就等待，writer-vs-writer 无法闭环；这一不变量和理由在 `crates/calm-server/tests/cases/deferred_write_tx_invariant.rs:1-18`。`begin_immediate_tx` 只在 BEGIN 边界对 code 5/6 做有界全事务重试，`crates/calm-truth/src/db/sqlite/infra.rs:10-30,40-52`，不会在仍持锁的事务中重试失败 statement。

## 证据 4：`row survived` 的错误传播

删除 pending 行的 SQL 在 `task_delete_pending_tx` 中以 `.await?` 返回错误，`crates/calm-truth/src/db/sqlite/task_projection.rs:1013-1020`；调用点继续 `.await?`，`:1165-1166`。因此锁错误会从 `project_tasks_tx`、`tasks_rebuild_tx` 一路返回，而不是被解释为“0 rows affected”。

`explicit_wait_policy...` 的实际顺序是 PATCH 后再断言行不存在，`crates/calm-server/tests/cases/task_projection_acceptance.rs:1507-1516`。PATCH helper 在返回后立即断言 status 为 200，`:232-249`。生产 route 内投影错误通过 `?` 离开 closure，`crates/calm-server/src/routes/waves.rs:1333-1343`；底层 wrapper 显式 rollback 并返回错误，`crates/calm-truth/src/db/sqlite/events.rs:338-353`。所以：

- 若 DELETE 因锁争用失败，整笔 PATCH 回滚；
- handler 不会返回 200；
- 测试会先在 `patch_policy` 的 status 断言失败，不会走到 `"collateral row survived"`。

同一测试里的 `user_delete` 虽写作 `let _ = delete_block(...)`，但 future 结果仍 `.await.unwrap()`，错误不会被吞，`crates/calm-server/tests/cases/task_projection_acceptance.rs:218-230`；而且它删除的是 `vetoed` block，不是后续由 PATCH 应删除的 `collateral` row。

因此没有“删除路径吞掉锁错误”的独立缺陷可另记。`row survived` 说明当时 PATCH 返回了 200、最终读又看见该行；在现有日志和代码证据下，其精确原因是 **UNKNOWN**，不能并入已复现的 deferred deadlock。

## 实跑记录

### 确定性红测（第一次运行）

命令（未带任何 feature）：

```text
cargo test -p calm-server --test mcp_integration_suite \
  task_projection_acceptance::deferred_rebuild_deadlocks_when_immediate_writer_waits_on_its_read_lock \
  -- --ignored --exact --nocapture
```

摘要：

```text
Compiling calm-server v0.1.0 (.../crates/calm-server)
Finished `test` profile ... in 1m 05s
running 1 test
thread '...deferred_rebuild_deadlocks...' panicked at
crates/calm-server/tests/cases/task_projection_acceptance.rs:213:10:
called `Result::unwrap()` on an `Err` value:
Db(Database(SqliteError { code: 6, message: "database is deadlocked" }))
test ...deferred_rebuild_deadlocks... FAILED
test result: FAILED. 0 passed; 1 failed; ... finished in 0.43s
```

### 当前正常路径与门禁

- `cargo fmt --all -- --check`：PASS。
- `kernel_process_suite::deferred_write_tx_invariant::production_deferred_transactions_are_read_only_allowlisted`：1 PASS（日志含 `Compiling calm-server`）。
- `explicit_wait_policy_removes_unreleased_pending_rows_with_readable_reason`：1 PASS。
- `rebuild_matches_incremental_withdrawal_outcomes_and_exactly_once_events`：1 PASS。
- `cargo test -p calm-server --test mcp_integration_suite -- --test-threads=8`：163 PASS / 1 ignored（即本 repro）/ 0 failed。

未运行全 workspace；本片只新增默认忽略的确定性 repro 与本报告，没有改生产实现或现有断言。

---

# #1070 第 2 步：`row survived` 定性

基线：本分支上一片 `e7c5f2e7`，生产基线仍为 `origin/main` / `42767c5b9115cea8d956c119281cb7a5fa3d1de5`。

## 结论

1. **三次 `row survived` 是同一测试、同一 key。** 都是 `task_projection_acceptance::explicit_wait_policy_removes_unreleased_pending_rows_with_readable_reason`，都是 `collateral row survived`。该测试先删除 `vetoed`、确认只剩 `collateral`，再 PATCH `automation_policy=declare-and-wait`，见 `crates/calm-server/tests/cases/task_projection_acceptance.rs:1508-1516`；消息由只查 key、不查 status 的 helper 产生，见 `:141-145`。
2. **根因是测试 helper 意外启动的 live dispatcher 与两个顺序请求之间的合法 scheduler claim 竞速，不是 DELETE 漏执行，也不是第 1 步的 SQLite deadlock。** 删除 `vetoed` 同步提交后广播 `PlanUpdated`；dispatcher 异步 poke scheduler，可在下一条 policy PATCH 取得写事务前把 `collateral` 从 `pending` claim 为 `dispatched`。policy 投影按设计保留 `dispatched/running/verifying`，于是断言只看见“row survived”。事件、claim 与保留分支分别见 `crates/calm-server/src/wave_report.rs:837-876`、`crates/calm-server/src/dispatcher/mod.rs:788-810,1013-1018`、`crates/calm-server/src/scheduler/mod.rs:973-1016,1092-1103,1233-1242`、`crates/calm-truth/src/db/sqlite/task_projection.rs:1095-1105`。
3. **拿到确定性红测。** 新增默认忽略的 repro 用已有 post-claim hook 会合：它先按原 helper 生命周期丢掉 DELETE 的临时 `AppState`，等 claim commit，实查 PATCH 前后 status 均为 `dispatched`，最后复用原断言稳定得到 `collateral row survived`，见 `crates/calm-server/tests/cases/task_projection_acceptance.rs:1656-1720`。
4. **“该删的 pending 行没删”不生产可达；“两个用户请求之间 pending 已被合法 claim，随后 policy PATCH 保留 in-flight 行”生产可达且是既定语义，不是生产缺陷。** 生产 dispatcher 同样订阅 `PlanUpdated`，而 lifecycle `Planning` 允许新 claim，见 `crates/calm-server/src/dispatcher/mod.rs:744-810,1013-1018` 和 `crates/calm-server/src/scheduler/mod.rs:145-156`。PATCH 一旦拿到 `BEGIN IMMEDIATE`，policy 更新、重建和 guarded DELETE 位于同一事务，事务内不可能再被 claim 插入，见 `crates/calm-server/src/routes/waves.rs:1324-1346`、`crates/calm-truth/src/db/sqlite/task_projection.rs:1165-1170`。因此生产可达的是“PATCH 前已在飞”，不是“PATCH 看到 pending 却没删”。

历史 CI 没打印 status，所以三次历史现场在断言时的**直接观测 status 是 UNKNOWN**；其日志只证明 key 仍存在。上述根因定性来自同构路径的确定性会合与完整代码条件，而不是把历史未记录字段冒充成已观测值。确定性 repro 中的 status 则是实查所得 `dispatched`，见 `crates/calm-server/tests/cases/task_projection_acceptance.rs:1693-1716`。

## 三次 CI 对齐

以下是 CI JUnit artifact 实查所得；三份 artifact 都保存了完整 panic 文本。

| run / artifact | 分支与时间 | 测试 | key / 消息 | 历史现场 status |
|---|---|---|---|---|
| `31452754443` / `9086977392` | `main`，2026-08-11 | `explicit_wait_policy_removes_unreleased_pending_rows_with_readable_reason` | `collateral row survived`，历史源码 `task_projection_acceptance.rs:141` | **UNKNOWN**（断言只调用 `keys`） |
| `31597828275` attempt 1 / `9141986847` | PR #1063，2026-08-12 | 同上 | 同上，历史源码 `:141` | **UNKNOWN** |
| `31689276190` / `9176848836` | PR #1082，2026-08-13 | 同上 | 同上，历史源码 `:141` | **UNKNOWN** |

当前同一断言仍只做 `SELECT key ...` 后检查 key 是否存在，`crates/calm-server/tests/cases/task_projection_acceptance.rs:141-145,251-256`，所以不能从历史 panic 反推出 status。作为交叉核对，`31597828275` 的下一次重跑 artifact `9143138972` 是另一条测试 `rebuild_matches_incremental_withdrawal_outcomes_and_exactly_once_events` 的 `database is deadlocked`；它不是第 3 次 `row survived`，也没有混入上表。

## 删除路径的完整条件

以下均为读代码所得；括号内说明本测试现场。

1. **必须实际进入 projection rebuild。** PATCH 中 `automation_policy.is_some()` 令 `projection_policy_changed=true`，route 在同一个 typed write closure 中先 `wave_update_tx`，再 `tasks_rebuild_tx`，`crates/calm-server/src/routes/waves.rs:1324-1346`。（满足：helper 明确 PATCH `declare-and-wait`，测试 `crates/calm-server/tests/cases/task_projection_acceptance.rs:232-249,1515`。）
2. **wave-report 必须存在，且声明从事务内所读 CRDT 快照投影出来。** 无 report 会直接返回空 outcome；有 report 时从 `body_crdt` 取 blocks，再生成 declarations，`crates/calm-server/src/wave_report.rs:128-166`。（满足：`Boot` 创建 report card，`crates/calm-server/tests/cases/mcp_wave_report.rs:175-188`。）
3. **`collateral` 声明仍在报告中。** 测试只删除 `vetoed` block，`collateral` 未被删除，`crates/calm-server/tests/cases/task_projection_acceptance.rs:1510-1515`。（满足。）声明不在报告里时 `schedulable_by_key` 缺项也会令存量 pending 行进入删除考虑，但不会产生本测试要求的 `declare-and-wait` 声明诊断，谓词见 `crates/calm-truth/src/db/sqlite/task_projection.rs:1072-1100`。
4. **事务内读到的新 policy 必须是 `declare-and-wait`。** `wave_projection_state` 从 `waves.automation_policy` 读取，`effective_wait` 只在精确相等时为真，`crates/calm-truth/src/db/sqlite/task_projection.rs:396-445,542-576`。（满足；policy write 与 rebuild 同事务，见条件 1，不依赖 cache invalidation。）
5. **声明必须因 wait policy 判为不可调度。** 对 `declared_by == "spec"`、`released_by_user == false`、`tombstone == false` 的声明添加 `declare_and_wait` 诊断；最终 `schedulable = ready && !tombstone && diagnostics.is_empty()`，`crates/calm-truth/src/db/sqlite/task_projection.rs:697-726`。（满足：测试 `task()` 固定 `declared_by=spec, ready=true` 且未设置 release/tombstone，`crates/calm-server/tests/cases/task_projection_acceptance.rs:37-44`。）
6. **目标行必须进入 `existing` 集合。** 当前 SELECT 是 wave 下全部 tasks，`crates/calm-truth/src/db/sqlite/task_projection.rs:1077-1081`。#1055 / PR #1063 前的精确旧 SQL 是 `... WHERE wave_id=?1 AND origin='block'`（`git show 91e2b259^:crates/calm-truth/src/db/sqlite/task_projection.rs:1137-1142`）；`collateral` 本来就是投影生成的 block 行，所以前三次中的 pre/post-#1055 两种 schema 都会选中它。去掉 origin 条件只扩大存量扫描，不会造成这条行漏删。
7. **该 key 必须不可调度或发生 withdrawal。** 条件为 `!schedulable_by_key[key] || withdrawal`，`crates/calm-truth/src/db/sqlite/task_projection.rs:1095-1100`。（满足：条件 5 令 schedulable=false。）
8. **真正执行并命中 DELETE 的最后条件是 status 仍为 `pending`。** `dispatched/running/verifying` 直接进入保留/诊断分支；其余状态才调用 `task_delete_pending_tx`，而 SQL 自身又有 `AND status='pending'`，`crates/calm-truth/src/db/sqlite/task_projection.rs:1013-1020,1101-1166`。**这是 CI 并行/时序下可能尚未成立、且确定性 repro 证明会被破坏的唯一条件：DELETE 事件触发的 scheduler 已把 `collateral` claim 为 `dispatched`。** repro 在 PATCH 前后各实查一次，均为 `dispatched`，`crates/calm-server/tests/cases/task_projection_acceptance.rs:1693-1716`。
9. **不可调度声明不能在后半段被重新 upsert。** 写入循环对 `!verdict.schedulable` 直接 continue，`crates/calm-truth/src/db/sqlite/task_projection.rs:1173-1177`。（满足。）
10. **事务必须成功提交。** DELETE 错误仍以 `?` 返回；typed write closure 出错 rollback，成功才 commit 并广播，`crates/calm-truth/src/db/sqlite/task_projection.rs:1013-1020,1165-1166`、`crates/calm-truth/src/db/sqlite/events.rs:338-353,416-425`。helper 在读行前断言 HTTP 200，`crates/calm-server/tests/cases/task_projection_acceptance.rs:232-249`。（三次均越过 status 断言；不是错误吞掉。）

### wave lifecycle 在哪里起作用

Projection 删除谓词本身不读 lifecycle；`wave_projection_state` 读取的字段只有 policy、ceiling、gate、cove 和 task 聚合，`crates/calm-truth/src/db/sqlite/task_projection.rs:396-445`。lifecycle 是**竞速对手 scheduler 能否 claim** 的前置：`Boot` 把 wave 设为 `Planning`，`crates/calm-server/tests/cases/mcp_wave_report.rs:155-164`；scheduler 允许 `Planning/Dispatching/Working/Reviewing`，`crates/calm-server/src/scheduler/mod.rs:145-156`，并在成功 claim 时把 `Planning -> Dispatching -> Working` 与 `pending -> dispatched` 同事务提交，`crates/calm-server/src/scheduler/mod.rs:1284-1347`。

## 时序依赖与隐式“立即可见”假设

### 实际异步动作

- 测试 `route_state()` 每次用同一 repo 构造一个新 `AppState` 和新 `EventBus`，`crates/calm-server/tests/cases/task_projection_acceptance.rs:101-122`；`AppState::from_parts` 又无条件 spawn live dispatcher，`crates/calm-server/src/state.rs:648-671`。
- `user_delete()` 正是用这种临时状态调用 handler，`crates/calm-server/tests/cases/task_projection_acceptance.rs:219-239`。`Dispatcher` 保存 JoinHandle 但没有 shutdown/drop，源码注释也说明 handle 只是留待未来 abort，`crates/calm-server/src/dispatcher/mod.rs:368-400`；receiver task 自持 `Inner`，而 `Inner` 持 scheduler，`crates/calm-server/src/dispatcher/mod.rs:788-842,918-947`。因此临时 `AppState` 析构并不取消已 spawn 的订阅任务。
- 删除 handler 在同一事务更新 report、重做 projection，并在 `vetoed` pending 行被删后生成 `PlanUpdated`，`crates/calm-server/src/wave_report.rs:816-876`；write wrapper 先 commit、再同步 broadcast，`crates/calm-truth/src/db/sqlite/events.rs:416-425`。所以 `user_delete().await` 保证删除与事件已经提交，却**不保证异步 dispatcher 已消费完事件**。
- dispatcher 收到 `PlanUpdated` 后 per-event `tokio::spawn`，再由 `Scheduler::poke` 二次 `tokio::spawn`，`crates/calm-server/src/dispatcher/mod.rs:788-810,1013-1018`、`crates/calm-server/src/scheduler/mod.rs:745-751`。scheduler 读取 pending ready set并 claim，`crates/calm-server/src/scheduler/mod.rs:973-1016,1037-1089`。

### 不存在的异步依赖

- policy patch 生效、projection rebuild、DELETE 和 commit 都在 PATCH 请求 await 内完成；没有“等 dispatcher 才应用 policy”，`crates/calm-server/src/routes/waves.rs:1324-1378`。
- policy 由同一事务直接从 `waves` 表读取，不经过 cache；报告声明也从同一事务中的 CRDT/card row读取，`crates/calm-truth/src/db/sqlite/task_projection.rs:396-445`、`crates/calm-server/src/wave_report.rs:128-166`。
- cache invalidation 与 PATCH 后删除无关；事件只在 commit 后广播，`crates/calm-truth/src/db/sqlite/events.rs:416-425`。

因此隐式假设位于测试的三行顺序：`user_delete().await` → “只剩 collateral” → `patch_policy().await` → “collateral 必须消失”，`crates/calm-server/tests/cases/task_projection_acceptance.rs:1513-1516`。中间的行列表检查只证明当次读取时 key 存在，不冻结 status；它把“前一请求已 commit”错误等同于“前一请求派生的异步 scheduler 工作也已静止”。CI 资源紧张会改变 dispatcher 与下一 PATCH 谁先取得 writer 的顺序：PATCH 先赢则 pending 被删、测试 PASS；claim 先赢则变 `dispatched`、测试报 `row survived`。

## 确定性复现与实跑

新增 repro 没有 sleep/重试：它使用生产 scheduler 已有的 `PostClaimDriveTestHook`，该 hook 位于 claim commit 后、operation drive 前，定义与消费点见 `crates/calm-server/src/scheduler/mod.rs:582-589,1073-1081`。repro 的步骤和证据：

1. 建立与原测试相同的 `vetoed + collateral`，安装 hook，`crates/calm-server/tests/cases/task_projection_acceptance.rs:1662-1675`；
2. 先抽出 `RouteState` 再 drop 临时 `AppState`，精确模拟原 helper 的生命周期，然后删除 `vetoed`，`:1677-1692`；
3. 等 `claimed` 会合，实查 `collateral.status == dispatched`，`:1693-1702`；
4. 用另一新 `AppState` PATCH wait policy，再查仍为 `dispatched`，`:1704-1716`；
5. 调原 `assert_diagnosed_on_both_reads`，稳定 panic `collateral row survived`，`:1717-1719`。

实跑命令（无任何 feature）：

```text
cargo test -p calm-server --test mcp_integration_suite \
  task_projection_acceptance::dispatcher_claim_between_delete_and_policy_patch_reproduces_row_survived \
  -- --ignored --exact --nocapture
```

结果：第一次与生命周期收紧后的第二次均在约 0.32 秒确定性失败，panic 均为：

```text
thread '...dispatcher_claim_between_delete_and_policy_patch_reproduces_row_survived' panicked at
crates/calm-server/tests/cases/task_projection_acceptance.rs:142:5:
collateral row survived
```

正常原测试单跑仍为 1 PASS；这符合竞速定性，不是用重试得到的结论。第 2 步只新增默认忽略的红测，未改生产实现、未改 helper、未改既有断言。
