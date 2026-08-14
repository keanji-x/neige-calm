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
