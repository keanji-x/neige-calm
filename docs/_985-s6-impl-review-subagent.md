# #985 切片 6 PR-A —— 实现评审（CHANNEL_NAME = subagent）

评审对象 `9d30006a..61f52b82`。工作树 `/tmp/wt6b`（detached `61f52b82`）。
所有测试在 `NEIGE_CODEX_BIN` 未设置、`PATH` 含 `.local-bin`、`CARGO_BUILD_JOBS=6` 下跑。
本轮所有代码改动均为一次性变异实验，交付前已 `git checkout -- .` 复原（末尾附证明）。

---

## BLOCKER

### B1 —— child-wave op 在 **tx 提交之后** 变 `Stuck`/`Failed`，会留下永久孤儿子 wave + 悬空 `child_wave_id`，并让父 wave 从此删不掉

**结论**：`drive_child_wave` 的两条 create 失败臂把 `failed_child_id` 硬写成 `None`
（`crates/calm-server/src/scheduler/mod.rs:1258-1276`），前提是「创建失败 ⇒ 子 wave 不存在」。
这个前提**在提交后失败的路径上不成立**：`prepare_tx` 已经提交了子 wave、spec/report card、
`tasks.child_wave_id`（`child_wave_adapter.rs:170-252`），之后任何一次 `drive_one` 报错都会
`mark_stuck(..., from_phase)`（`operation/driver.rs:333-340`；恢复路径同形 `:1057-1066`），
`from_phase` 完全可以是 `TxCommitted`/`SpawnStarted`。

**攻击（真实交错，非合成）**：child-wave op 的 `prepare_tx` 提交 → 子 wave 落库 →
`set_phase(SpawnStarted)` 或后续任一步 DB/租约报错 → op 变 `stuck` →
sweep 的 `resume_dispatched → drive_spawn → drive_child_wave` 命中 `(kind,idem)` 复用这条
`stuck` op → `OperationOutcome::Stuck` → `fail_child_wave_task(task, wave, "child-wave-create-stuck", …, None)`。

**证据 / 我实际跑过的验证**：我在 `crates/calm-server/tests/scheduler.rs` 临时插入探针
`zzz_review_probe_post_commit_stuck_orphans_child_wave`：先 `sweep_all()` 走**真实** child adapter +
bootstrap adapter 把子 wave 造出来（父任务 `running`），再把 `operations.phase` 置 `stuck`
（`kind='child-wave'`）、父任务回置 `dispatched`，第二次 `sweep_all()`。实测输出：

```
PROBE: parent=Some("child-wave-create-stuck")
       child_wave_lifecycle=Some("planning")
       dangling_child_wave_id=Some("e1dc36d27ecc4ad3aff798ee4748b7ab")
```

即：父任务 `failed`，子 wave **仍然活着**（`planning`，没被置 `failed`），`tasks.child_wave_id`
仍指着它。后果链：
1. `reconcile_all_child_wave_tasks` 只扫 `status IN ('dispatched','running')`
   （`scheduler/mod.rs:576-580`），永远不会再碰它 —— 无自愈；
2. `wave_delete_tx` 的 descendant 守卫（`calm-truth/src/db/sqlite/wave.rs:222-231`）从此
   **永久拒绝删除父 wave**，用户必须手工先删这个没人知道存在的孤儿；
3. bootstrap 从未跑过，子 wave 是个带 spec card 但永不启动的僵尸。

**为什么现有测试全绿**：`acceptance_13e` 的 create 两臂用
`op_repo.insert_operation(...)` 直接插一条**从未执行过**的 op 再改 `phase`
（`tests/scheduler.rs:6120-6152`），因此 `tx_output` 为空、子 wave 不存在。
这正是简报点名的「fixture 省掉生产真会产出的字段」形状 —— 生产必然产出的
`tx_output` / `tasks.child_wave_id` 被 fixture 抹掉，于是四臂表看着全绿。
实现方自述「#13e 只删了 create/Stuck 一个 mutant」严重低估了这条。

**最小修法**：`fail_child_wave_task` 不要靠调用方传 `failed_child_id`，改成在同一 tx 内
`SELECT child_wave_id FROM tasks WHERE id=?1`；非 NULL 就走既有的「置子 wave `Failed` + 发
`WaveLifecycleChanged`/`WaveUpdated`」分支（`scheduler/mod.rs:1341-1381` 已经写好，只是 create 臂不走）。
回归测试直接用上面的探针形状：**先跑真实 adapter 造出子 wave**，再注入 `stuck`。

---

## MAJOR

### M1 —— `task_mark_sub_wave_running_tx` 的三条守卫**零覆盖**（我设计的变异，全绿）

**结论**：`WHERE id=?2 AND status='dispatched' AND spawn='sub-wave' AND child_wave_id IS NOT NULL`
（`crates/calm-truth/src/db/sqlite/task.rs:150-151`）三个谓词一个都没有测试承重。

**我改了什么 → 结果**：把 WHERE 子句整体削成 `WHERE id=?2`（三条守卫全删），跑
`cargo nextest run --workspace -E 'test(acceptance_) or test(every_registered_task_adapter) or test(sub_wave) or test(child_wave)'`
→ **39 tests run: 39 passed**，一条不红。已复原。

**攻击**：`drive_child_wave` 在 bootstrap 成功与 running flip 之间没有事务保护
（`scheduler/mod.rs:1332-1340`）。同进程 sweep 的 `reconcile_all_child_wave_tasks` 此刻发现子 wave 被删，
把父任务写成 `failed`；紧接着 `mark_sub_wave_running` 落地 —— 没有 `status='dispatched'` 守卫的话，
一条已终态、已发过 `TaskFailed` 的行会被**复活成 `running`**，且没有任何事件解释它。
`acceptance_12`（`tests/scheduler.rs:5763`）自己 seed 的就是 `dispatched`+`child_wave_id`，两边都满足，
所以它对守卫恒真。生产代码是对的，问题是**没有任何断言钉住它**。

**最小修法**：给 `acceptance_12` 加三条负例（`status='failed'` / `spawn='in-wave'` /
`child_wave_id IS NULL` 各一条，断言 `rows_affected()==0` 且行未被改写）。这也顺带把
`mark_sub_wave_running` 丢弃 `rows_affected` 的语义（丢竞态 = 静默 no-op）写成契约。

### M2 —— 第二条递归 CTE 不在静态门禁覆盖内；删掉它的深度截断，门禁 + #6 + #8 全绿，只有 #7 靠**挂死**兜底

**结论**：`WAVE_ROOT_DEPTH_SQL` 的注释自称「`WHERE up.depth <= ?2` 是 ★ 唯一终止保证 ★」
（`child_wave_adapter.rs:32-34`），但真正在**环路径上被执行**的是 `WAVE_BOUNDED_PATH_SQL`
（`:46-55`，由 `root_and_depth` 的 `[]` 分支调用 `:107-111`），而静态门禁
`upward_cte_keeps_its_only_cycle_termination_guard`（`:474-478`）只检查前者。

**我改了什么 → 结果**：只把 `WAVE_BOUNDED_PATH_SQL:52` 改成 `WHERE up.depth <= ?2 OR 1=1`。
- `upward_cte_keeps_its_only_cycle_termination_guard` / `acceptance_8` / `acceptance_6` → **3 passed，全绿**；
- 单独跑 `acceptance_7_two_cycle_fails_fast_with_cycle_reason` → 无限循环，
  `has been running for over 60 seconds`，99.8s 后被 SIGTERM 杀掉才算 failed。
  测试里那句 `assert!(start.elapsed() < 1s)`（`:643`）**永远到不了**，因为查询根本不返回。

已复原。这是「fail-closed 被翻成挂死而门禁全绿」——CI 上表现为几分钟的 slow-timeout，不是干净的红。

**最小修法**：门禁改成对**两个** SQL 常量（或一个 `&[&str]` 常量表 + set-equality）都断言含
`WHERE up.depth <= ?2`；`acceptance_7` 外面套 `tokio::time::timeout`，超时即 `panic!`，把挂死变成硬红。

### M3 —— #21c 不只是「无归因」，它是一条**没有生产代码在测**的假门禁（实现方低估）

**结论**：`acceptance_21c_cross_cove_edge_is_a_loud_delete_tripwire`
（`crates/calm-truth/src/db/sqlite/sub_wave_tree_tests.rs:107-130`）两条断言都不承重：
- `:120-127` 断言 `mismatch == 1` —— 这条边正是测试自己在 `:114-119` 用 raw SQL 写进去的，
  **断言与被测事实同源**，恒真；
- `:128-129` 断言 `cove_delete_tx` 报错 —— 测的是 SQLite NO ACTION FK 的语义，
  本片没有一行生产代码参与。

实现方自述「不驱动写路径、只有 #6 会红」属实，但表述成「§7 所称 tripwire 本身没有归因能力」
偏轻：它连**任何**归因能力都没有，是简报列的「测试绕过唯一生产接线」原型。
生产行为确实有保护（`acceptance_6` 的 `cross_cove_edges == 0`，`child_wave_adapter.rs:593-600`，
驱动真实 adapter），所以**不是生产漏洞**，但 §7 第 21c 行按现状交付等于虚报一条验收。

**最小修法**：按实现方自己写的方向改 —— 造第二个 cove、驱动真实 `ChildWaveAdapter`、
再断言全表 `cross-cove` 边为 0 且 cove 删除不报错；否则把 21c 从 §7 划掉，写明由 #6 承担。

---

## MINOR

- **N1** `root_and_depth` 对「父 wave 根本不存在」返回 `sub-wave-depth-exceeded`
  （`child_wave_adapter.rs:104-119`），`acceptance_8`（`:648-653`）把这个误诊当契约钉死了。
  结果 `:166-168` 的 `parent wave {} is missing` 分支对这条路径是死代码。fail-closed，但理由码会误导排障。
- **N2** 0071 的 `CHECK (parent_wave_id IS NULL OR parent_wave_id <> id)`
  （`migrations/0071_sub_wave_tree.sql:5`）没有任何断言；`acceptance_21` 只查 `on_delete` 和索引偏筛
  （`sub_wave_tree_tests.rs:44-66`）。自环目前靠 CTE 兜成 `sub-wave-tree-cycle`，不是靠这条 CHECK。
- **N3** 权威文档 `docs/architecture/985-doc-as-plan.md:1983` 写「§7 的 30 个编号验收行」，
  实测 `docs/_985-s6-design.md` §7 有 **33** 行（`grep -c '^| [0-9]'` = 33，编号
  1,2,3a,3b,3c,4,5,5b,6,7,8,9,10,11,12,13,13b,13c,13d,13e,14,14b,15,16,17,18,19,20,21,21b,21c,22,23）。
  **33 是对的**，实现方裁决正确；本片改过的权威文档那句「30」是新写进去的漂移，应改成 33。
- **N4** `acceptance_12` 直接调 `task_mark_sub_wave_running_tx`（`tests/scheduler.rs:5779-5786`），
  绕过生产接线 `mark_sub_wave_running`。生产链路由 `acceptance_19` 间接覆盖，但 #12 这行本身不算端到端。
- **N5** `wave_update_tx` 的 reopen 守卫不看父任务状态（`wave.rs:135-143`）：父任务已经 `failed` 之后，
  这个子 wave 也永远不能被 reopen。把守卫收窄成 `AND status IN ('dispatched','running')` 后
  `acceptance_17` 仍绿，说明"对终态父任务也生效"这层语义没有断言。
- **N6** 变异映射里 13b/13c/13d 三行的编号与设计 §7 不对齐（映射的 "13c" 对应设计 13c 的变异但红在
  名为 `acceptance_13d_...` 的测试上）。我逐条核对过：设计 13b/13c/13d 三行的**指定变异都实际执行过**，
  只是测试命名错位，不是漏项。

---

## 实现方自述的缺口，哪些属实、哪些被低估

| 自述 | 复核结论 |
|---|---|
| #21c 跨 cove 绊线不驱动写路径，只有 #6 会红 | **属实但低估** → 见 M3：两条断言一条恒真、一条只测 SQLite，整条零生产覆盖 |
| #11 两个内层站点在正确臂序下是死代码 | **属实**。我独立复核臂序：`TaskStatus::Running if task.spawn == "sub-wave"` 在 `scheduler/mod.rs:1743` 排在 terminal 臂与 kind 超时臂之前，因此 `:294` 的 spawn 排除与 `stamp_missing_running_liveness_deadline` 里的同一判定确实不可达。不是生产缺陷，保留即可 |
| #5/#6/#8/#12/#13/#14 子变异未跑 | **属实**，且均为「正向断言已覆盖、只缺独立 mutant」，我不追加为阻塞项 |
| #13e 只删了 create/Stuck 一个 mutant | **严重低估** → 见 B1。真正的问题不是少跑一个 mutant，而是 create 两臂的 fixture 抹掉了生产必然产出的 `tx_output`/`child_wave_id`，把一条真实的孤儿-子-wave 缺陷藏在了「已知缺口」这句话背后 |
| 漏报 | M1（三条守卫零覆盖）、M2（第二条 CTE 无静态门禁）在 impl-notes 与变异映射里都没有出现 |

## 我自己设计的变异里，哪几条打穿了

1. **削掉 `task_mark_sub_wave_running_tx` 全部三条守卫** → 39/39 全绿（M1）。已复原。
2. **只解除 `WAVE_BOUNDED_PATH_SQL` 的深度截断** → 静态门禁 + #6 + #8 全绿，#7 挂死 99.8s 被 SIGTERM（M2）。已复原。
3. **不是变异而是补测：真实 adapter 建好子 wave 之后再注入 `stuck`** → 暴露 B1，探针打印出
   `child_wave_lifecycle=planning` + 悬空 `child_wave_id`。探针已删除。

另外复核而未打穿的面（结论：现状正确）：`(kind, idempotency_key)` 唯一索引使 child-wave op 与
worker op 共用 `task.id` 不会撞（`migrations/0042_operations_parked.sql:96-98`）；投影 UPSERT 三段
（19 列 / `DO UPDATE SET` / 变更检测析取）与 `?1..?16` 占位符逐个核对无误，`?16` 复用两次符合设计；
`Task` 用 `sqlx::FromRow` 按名映射且 `TASK_COLUMNS`、`task_insert_tx`（24 列 / 24 bind）、
`task_update_pending_tx`（`?9/?10/?11` 重排）三处同步；全仓仅两处 `INSERT INTO tasks`，均已带 `spawn`；
三条守卫确实下沉在 `calm-truth` 的 `wave_delete_tx`/`wave_update_tx`，且 REST 入口
（`tests/cases/cards_deletable.rs` 的 `acceptance_20_wave_delete_route_...`）真的打的是 `/api/waves/{id}` 拿 409；
`spawn` 未进 `BlockVerdict`，#22 用序列化 JSON 断言而非结构体字段（非编译期恒真）；
`PROJECTION_DRIFT_TASK_FIELDS` 不含 `spawn` 与设计附录 D.1 3d「不判」一致，非遗漏。

## 可以合入了吗

**NO。**

最小阻塞集：
1. **B1** —— `fail_child_wave_task` 改为在 tx 内自查 `tasks.child_wave_id`，非 NULL 即走既有的
   子 wave 置 `Failed` 分支；配一条**先跑真实 adapter 再注入 stuck** 的回归测试。

强烈建议同轮一起修（各 ≤10 行，且都属于「测试在但改坏不红」）：
2. **M1** 给 `acceptance_12` 补三条守卫负例；
3. **M2** 静态门禁覆盖两个 CTE 常量 + `acceptance_7` 加 `tokio::time::timeout`。

M3 与全部 MINOR 可以随本轮或下一片处理，不单独阻塞。

---

```
$ git status --short
（空）
```
