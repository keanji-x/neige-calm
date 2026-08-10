# #985 切片 6 PR-B 实现评审 r6（收敛检查）— channel: subagent

范围 `c71e4132..7ebbfd48`。环境 `/tmp/wtb3`，`RUSTC_WRAPPER=` + `CARGO_BUILD_JOBS=6`，`NEIGE_CODEX_BIN` 未设置。
**web 未实际执行**（本 worktree 无 `web/node_modules`），`task.tsx` / `report-blocks.test.tsx` 仅结构性复核。
我实际跑过的门：`cargo test -p calm-truth --lib` **354 passed/0 failed**；
`-p calm-truth --test bounded_wave_tree_sql` **17 passed/0 failed**；
`-p calm-server --lib -- child_wave_adapter wave_report` **77 passed/0 failed**；
`scheduler` 测试二进制全量并行 1 次 **109 passed/1 failed**（见 §acceptance_19，非缺陷）；
`scheduler acceptance_19*` 定向 5 次 + 全量内 1 次 **6/6 passed**。两个探针测试跑完已 `git checkout -- .` 复原。

---

## BLOCKER — B1 单例短路仍然不计 legacy 占用（修复点上的同形状新洞）

- **结论**：修订轮 5 把 legacy 计入 tree 占用只做在 `Share` 分支上；`wave_tree_term` 的**单例短路**
  （`crates/calm-truth/src/db/sqlite/wave_tree.rs:222-242`）只把 `>=` 改成了 `>`，占用一项没动。命中
  `NotInTree` 时 `tree_capacity = i64::MAX`（`crates/calm-truth/src/db/sqlite/task_projection.rs:636-638`），
  准入只受 `ceiling - block_inflight` 约束，legacy 活行完全不可见 —— 正是 r5 codex 判 BLOCKER 的那个形状：
  **新 block 行叠加在遗留在飞行之上**。
- **触发条件**：无父无子单例 wave（`wave_tree.rs:197-215` shortcut 命中）+ 存在 `origin='legacy'` 非终结 spec 行 +
  `budget > ceiling`。安全条件应为 `budget - legacy_live >= ceiling`，代码只判了 `budget > ceiling`。
  可达配置：ceiling 显式调小 + budget 用默认 32；或 ceiling=4 / budget=6。
- **证据（探针，已复原）**：单例 root，`set_ceiling=4`、`set_tree_budget=6`，4 条 legacy running 行，再投影 4 条新 key ⇒
  `term=NotInTree new_block_rows=4 live_spec=8 budget=6`：Σ live_spec=8 **超出** B=6，超出量是本次写入**主动新增**的。
  对照组 `legacy_live_spec_consumes_tree_share_until_it_terminates`（`wave_tree_budget_tests.rs:880`）绿，
  是因为它设了 `ceiling=8 > budget=2`，恰好走 `Share` 分支绕开本路径。
- **不修的可达后果**（判 NO 的依据）：(1) 中心不变式「Σ_v live_spec(v) ≤ B」在受支持配置下被**主动写入**破坏，
  不是 §8 批准的「暂时超出、单调收敛」——每次 PATCH 都能重新把 block 行顶回 ceiling；(2) 进入该状态后，该 root 的
  任何整树操作（child-wave 创建 `child_wave_adapter.rs:272`、budget PATCH `routes/waves.rs:1340`）都会在
  `require_tree_budget_postcondition`（`crates/calm-server/src/wave_report.rs:245-249`）上 409，用户被锁死，
  而 409 让他等的在飞行里有一半是系统刚替他加上去的。
- **最小修法**：`wave_tree_shortcut` 那条非递归语句再取一列 `non_block_live_spec`（谓词同 `task_projection.rs:443-448`），
  `wave_tree.rs:227` 改为 `budget - non_block_live >= ceiling`（同时把 `>` 退回 `>=`，见 M1）。仍是 0 次递归查询。

## MAJOR — M1 `>=` → `>` 让**默认孤 wave**的诊断指向一个按不动的旋钮

- **结论 / 触发条件**：`wave_tree.rs:227` 改 `>` 后，`ceiling == budget` 的单例不再是 `NotInTree` 而是
  `Share{members:1}`；`task_projection.rs:924-927` 的归因过滤器用 `tree_capacity <= ceiling_capacity`（含等号），
  **平局判给 tree**。命中面是 ceiling/budget 均取默认（32/32）的**任意孤 wave**，排到第 33 条任务时。
- **证据（探针，已复原）**：全新默认 wave + 33 条声明 ⇒
  `term=Share(TreeShare{budget:32,members:1,share:32}) code=["tree_budget_exhausted"] args={tree_waves:1,share:32,ceiling:32}`。
  文案链路 `crates/calm-types/src/report_blocks/tasks.rs:261-288` + `web/src/pages/report-blocks/task.tsx:81-83`
  （**未执行 vitest**，只读代码）渲染为「This wave is part of a group of **1** linked waves…」。
- **可达的错误后果**（不只是措辞）：动作是 `raise_tree_task_budget`（`tasks.rs:71`）。ceiling=32 同样绑定，
  把 budget 抬到 64 后 `capacity = min(ceiling_cap, tree_cap)` 仍是 32 —— **照着诊断做完全无效**，
  而正确旋钮 `spec_task_ceiling` 一字未提；r5 之前（`>=`）给的正是 `spec_task_ceiling`。
- **最小修法**：`task_projection.rs:926` 改为
  `share.admission_frozen || tree_capacity < ceiling_capacity || (share.members > 1 && tree_capacity == ceiling_capacity)`。
  这同时消除 `singleton_rebuild_entrypoints_agree_when_budget_equals_ceiling`（`child_wave_adapter.rs:1197`）
  当初要修的分叉**根因** —— 分叉真正来源是 `tasks_rebuild_tree_tx` 对单成员树也硬造 `Share`（`wave_report.rs:206-212`）。

## MINOR

### m1 SQL 门收窄后的误红面：常量 / 未限定列 / 匿名参数 / 引号标识符

`bounded_wave_tree_sql.rs:315-331`（`direct_parameterized_depth_bound`）+ `:333-357`。探针结果：
accepted — `down.depth <= ?2` / `… AND w.id IS NOT NULL` / `(down.depth <= ?2)` / `?2 >= down.depth` /
`down.depth < ?2` / `down.depth<=?2` / `DOWN.DEPTH <= ?2` / ON 子句里的界；
REJECTED — `down.depth <= 3`（合法有界的常量）、`depth <= ?2`（未限定列）、`down.depth <= ?`（sqlx 合法匿名参数）、
`down."depth" <= ?2`。生产 4 条 CTE 全部仍通过（`every_recursive_parent_wave_cte_…` 绿）。常量被拒是简报说明过的
自觉取舍，另外三种是后来者很可能写出的正当形状。最小修法：不放宽语法，把 `:536-539` 的失败文案补一句
「右侧必须是绑定参数 `?N`，列名必须用递归别名限定」。

### m2 `workspace_member_roots` 在成员目录不存在时 fail-open

`bounded_wave_tree_sql.rs:471-498` 解析后直接 `workspace.join(member)`，而 `production_sources_below`（`:16-19`）
对不存在的路径**静默 return**。今天 `Cargo.toml:3-18` 全是显式路径故无害，但一旦有人写 glob（`"crates/*"`，cargo 合法）
或改名，那些 crate 会**无声退出扫描范围**——扫描面缩小而门保持绿。修订轮 5 想让清单解析 fail-closed，这一半没做到。
最小修法：`assert!(root.join("Cargo.toml").is_file(), …)`。

### m3 冻结的解冻路径（复核结论，非缺陷）

`admission_frozen`（`wave_tree.rs:301-306`）不会永久冻死：占用谓词终结集（`wave_tree.rs:115`）与
`TaskStatus::is_terminal`（`crates/calm-truth/src/model.rs:374-379`）逐字一致；legacy **pending** 行也会被调度
（`compute_ready` 只按 status 过滤、不看 origin，`crates/calm-server/src/scheduler/mod.rs:198`）从而自然排空；
第二条路是抬 budget（PATCH 走 `tasks_rebuild_tree_tx`，后置条件用**新** shares，`wave_report.rs:196-220`）。
仅「单成员遗留占用 > 64/N」时两路都不通 —— 可接受，建议 §8 补一句。

## 修订轮 5 的五处，哪几处修出了新洞

| # | 修改 | 判定 |
| --- | --- | --- |
| 1 | 占用含 legacy | **修出新洞（B1）**：只补了 `Share` 分支，单例短路整条绕过。计入范围本身正确（非终结 / `declared_by='spec'` / 同 wave，与 `is_terminal` 逐字对齐），无跨树、无终结行误计 |
| 2 | 两条后置条件各自可红 | 未修出新洞。两侧成员集合同事务同 CTE 派生（`wave_report.rs:181` vs `wave_tree.rs:373-388`），`is_none_or` 不会误伤；没有找到被误拒的合法生产序列 |
| 3 | SQL 门改「合法形状」 | 误红面按预期扩大，生产 4 条 CTE 全绿；仅剩文案问题（m1） |
| 4 | `>=` → `>` | **修出新洞（M1）**：修好了整树/普通两入口的分叉，却把最常见的「根本没有树」路径的诊断和动作改错 |
| 5 | TOML 结构化解析 | 解析本身 fail-closed（`unwrap_or_else(panic)` + `expect`），但下游路径缺失 fail-open（m2） |

## `acceptance_19` 的时序敏感面判断

**仍有时序敏感面，且属 PR-A 遗留，本片没碰**（`git diff --name-only c71e4132..7ebbfd48 -- crates/calm-server/tests/scheduler.rs` 为空；
`git log -S 'bootstrap adapter must reach its blocking hook'` 只指向 `c71e4132`）。

PR-A 的 durable Parked + observer barrier 消除的是**顺序竞争**，不是**绝对墙钟**。残留三处绝对期限，均在
`crates/calm-server/tests/scheduler.rs`：`:6357`/`:6437` 的 `timeout(Duration::from_secs(1), block.entered.notified())`
（等 `tokio::spawn` 的 `sweep_all` 走完 prepare_tx + spawn 到达阻塞钩子）、`:6297` 附近 observer 的
`Parked{deadline_ms: now+10_000}`（超时被 `enforce_parked_deadline`，`crates/calm-server/src/operation/driver.rs:296`
判死）、`:6371` 的 30s join。**唤醒本身不会丢**：`wait_entered`/`entered` 都是 `notify_one()`
（`driver.rs:274`、`scheduler.rs:975`），permit 留存。唯一翻转面是那两处 1s：全量并行门（48 核跑 3400+ 用例
外加 cargo 编译争抢）下 spawn 任务 1s 内到不了钩子完全可能 —— 与「全量偶发一次、定向 1/1、其后三次全绿」吻合。

**复现尝试未翻**：`--test-threads=1` 单跑 5 次（伴随 16 个 CPU 自旋进程）+ 全量并行 1 次，6/6 过。
结论：属实但低频，**属 PR-A，建议另开 issue**（两处 1s 提到 5–10s，或 barrier 改无期限 + 外层单一总超时），不阻塞本片。
附注：我直接跑测试二进制时 `gate_step_env_is_minimal_and_exit_path_scrubbed` 失败，是 `scheduler.rs:7140`
的「test must run under cargo」哨兵，属调用方式，不是缺陷。

## 可以合入了吗

**NO** —— 因为 B1，不是因为覆盖度。B1 与 r5 已裁决为 BLOCKER 的那条是**同一条不变式、同一种破坏方式**
（准入不计已有占用 ⇒ 新行叠加 ⇒ 总量主动超出 B），只是活在单例短路这条支路上；探针给出的可达证据是
「live_spec=8 > budget=6，其中 4 条是本次写入新增」。B1 + M1 的修法都在同一处（短路条件 + 归因过滤器），
合计约 15 行，无需新设计；建议合并为一轮修复后直接进 r7。其余三条 MINOR 不阻塞。

## git status --short

```
?? docs/_985-s6b-impl-review-r6-subagent.md
```
（探针与临时改动均已 `git checkout -- .` 复原；本文件为唯一新增。）
