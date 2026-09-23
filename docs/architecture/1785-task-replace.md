# 任务声明默认化 + `calm.task.replace`（#1785，#1727 片 5 新定义）— 设计 v4

> **状态（2026-09-23）**：设计稿 v4，已折入第 1–3 轮双通道设计评审（第 3 轮：A 1 MAJOR + 7 MINOR；B 3 MAJOR + 1 MINOR），处置见 §11–§13。
> 基线 `origin/main` = `2ef67292d`（工作树 `design/1785-task-replace`）。所有 file:line 在该基线上实测；路径省略 `crates/calm-server/src/` 前缀，其余写全；`event.rs` 指 `crates/calm-types/src/event.rs`。
> 4140 数据来自只读 `~/.local/share/neige-next/data/calm.db`（`events.max(id)=34967`，最后事件 2026-09-23 13:58:52Z），复现命令见 §9。
>
> **owner 规则**：(1) 只解决在 #1772 / #1782 上**观察到的**症状，假想情形写成一行 KNOWN GAP；(2) 兼容只看 4140 库里真实存在的东西。

## 0. 结论先行

- **片 1（最小、止痛最多）**：`calm.plan.cancel` 接受 `running`（仅限内核超时通路已能收割的 codex/claude 执行），复用「liveness 超时 → 清理标记 → sweep 杀 worker」；
  外加 scheduler sweep 的空转臂：持久化的 `idle` 只当**候选信号**，再用权威的实时 `thread/read`（reaper 已在用）确认本执行线程的最后一个 turn 已结束 ≥ 300 s，
  才以 `worker-turn-ended` 失败该任务（唤醒 Planner）。无新事件种类、无迁移、无新环境变量。消除 #1772 r4 的 **118 分钟静默卡死**与唤醒后 **28 次收拾调用**（§2.3）。
- **片 2**：`calm.task.replace`——停旧（片 1 的 CAS）→ 内核**追加**后继声明块（确定性 key `<root>.<n>`，继承门禁/kind/依赖/优先级；goal、acceptance 必填、context 整体替换）
  → 新租约准备时用 git 2.39.5 可跑的两参数 `git merge-tree --write-tree <U> <cand>` 把整条谱系的改动搬到当前上游，冲突 ⇒ `spawn-failed: refused: carry-conflict` 唤醒。
  一张新回执表（迁移 0116），零新事件种类；租约表已接受 `base_source='attempt'`（0 行在用）。
- **片 3（有条件）**：替换时连带重派上一轮评审。片 2 上真栈复测后，若「每轮重声明评审」仍是主要负担才做。
- 两处**提示词**修正（不加机制）：评审任务不需要 `task.verdict`；拒绝候选后不必写 `reviewing→working`（内核认领时自动推）。见 §3.3。
- 与 issue 原文相左的发现见 §8。

## 1. 事实表（`origin/main` = `2ef67292d`）

### 1.1 任务声明、身份、门禁

| # | 事实 | 位置 |
|---|---|---|
| F1 | 任务 = 报告里的 `task` 块；`TaskDeclaration{key,kind,goal,acceptance,gate,no_gate_reason,depends_on,context,…,tombstone}`，**没有** `base` 字段 | `crates/calm-types/src/report_blocks/tasks.rs:121-145` |
| F2 | key 形状 `^[a-z0-9][a-z0-9._-]{0,63}$`（`.` 合法） | `calm-types/.../tasks.rs:444-453` |
| F3 | Planner 提示词：用 `calm.report.read` 取 `docRev`，再 `blocks.upsert`/`report.commit` 写 task 块 | `crates/calm-server/prompts/planner.md:68`、`:78` |
| F4 | 首代执行 id = `"{track}:{key}"`（任务 `idempotency_key` 即 `track:key`） | `crates/calm-truth/src/db/sqlite/task_projection.rs:1621-1624` |
| F5 | 代次表 `task_attempt_allocations`：`origin_json.kind` 只允许 `initial`/`recovery`；append-only trigger 与「recovery 前驱必须是当前 `failed` 执行」trigger | `crates/calm-truth/migrations/0097_task_attempt_allocations.sql:13-37,119-127` |
| F6 | `require_task_gates` 时非 terminal 任务必须有 `gate` 或 `no_gate_reason`；agent 任务的 `gate.cwd` 被拒 | `calm-types/.../tasks.rs:551-585` |
| F7 | 门禁没有继承概念：每个新 key 必须重写整份 `gate` | F1 + F6 |
| F8 | `context.neige_execution` 是**严格解码**的 isolated 选择器（`deny_unknown_fields`，必需 `version`/`workspace`），报告校验对每个 task 块都跑它 | `crates/calm-types/src/task_execution.rs:18-29`；`crates/calm-types/src/report_blocks/kinds.rs:260-270` |
| F9 | 依赖校验只拿「在途持久化 key」当已知集合：块里 `depends_on` 指向不在文档里的 key ⇒ `unknown_dependency` 诊断 | `task_projection.rs:1014`；`calm-types/.../tasks.rs:528-545` |

### 1.2 评审、裁决、lifecycle

| # | 事实 | 位置 |
|---|---|---|
| F10 | `calm.task.verdict`：`accepted` ⇒ `TaskCompleted`，`rejected` ⇒ `TaskFailed`（Planner 作者），可捎带 `lifecycle`；**只写事件，不改任务行**——被拒 producer 的行仍是 `done`（#1772 三个被拒 producer 皆然） | `mcp_server/tools/track_state.rs:215-241`；`decision_sink.rs:258-330` |
| F11 | **`depends_on` 等 `Task.done`，不等 verdict；纯顺序依赖不需要 verdict** | `prompts/planner.md:73` |
| F12 | git 候选的「接受决定」没有表（S4 片 6 未做）；`task_candidate_decisions` 只服务 isolated file-delivery | `file_delivery/candidate_qualification.rs`（唯一写者） |
| F13 | `calm.review.round`：Planner-only、按 subject 单调 n，只落事件；**内核无读者据它放行**（唤醒谓词 `=> false`） | `mcp_server/tools/review.rs:22,405-425`；`crates/calm-truth/src/role_gate.rs:357`；`dispatcher/mod.rs:142-148` |
| F14 | 合并围栏 F4 只写在模板里（prompt-only） | `crates/calm-server/templates/builtin/issue-development.md:77-95,122-123` |
| F15 | lifecycle：`:16` 把 `reviewing → working`「需要更多工作时」列为 Planner 边；`:20` 说内核在认领任务时自己推进 `working`，不要为开任务而写 | `prompts/planner.md:16,20` |

### 1.3 停止、失败、取消、恢复

| # | 事实 | 位置 |
|---|---|---|
| F16 | `calm.task.fail` 只对 Worker 可见且 `require_role(Worker)` | `mcp_server/tools/emit.rs:347-372` |
| F17 | `calm.plan.cancel` 对 `dispatched|running|verifying` 返回 `-32409 … out of scope (#644)`；只取消 `pending` | `mcp_server/tools/plan.rs:486-501`；`calm-truth/src/db/sqlite/task.rs:107-118` |
| F18 | #644 状态机画了 `running → canceled` 边，但无入口 | `docs/architecture/644-plan-then-schedule.md:17-25` |
| F19 | liveness 超时通路：只对 `task_has_running_liveness_deadline`（kind codex/claude、非子 track 派生路由（`spawn` 判据见 `scheduler/mod.rs:234`））的 `running` 行；`task_fail_from_worker_tx`（CAS `dispatched|running`，detail 硬编码 `"worker-timeout"`）+ 同事务写 `timeout_cleanup` 标记；sweep 只扫 provider codex/claude 的标记，`fail_running_worker_card` 中断 turn → 收割 PTY → 等 pid → session `failed`，失败下轮重试 | `scheduler/mod.rs:77-106,233-235,1897-1923,1972-2060(:2048),2097-2140(:2104)`；`operation/driver.rs:176-207`；`calm-truth/.../task.rs:564-580` |
| F20 | `dispatched` 期间 `worker_card_id` 可为 NULL、无 session 行；spawn 成功后 `mark_running` 0 行只打 debug | `scheduler/mod.rs:1712-1735` |
| F21 | 完成 CAS 也是 `dispatched|running`，交付行在同一报告事务插入 ⇒ **cancel 先赢则旧 worker 的报告被拒、不会有交付；报告先赢则任务已 `verifying/done`，cancel 被拒** | `calm-truth/.../task.rs:308-330,364`；`decision_sink.rs:160-210` |
| F22 | feeder 把线程状态写进 `worker_sessions.{last_thread_status,last_activity_ms,last_turn_completed_ms}`，自述为 best-effort 提示（lag 丢通知、连续两次写失败丢戳），「实时 `thread_read` 才是权威」 | `liveness_feeder.rs:1-3,45-72,123-170` |
| F23 | 权威活性读：`read_liveness_facts(thread_id)` = `thread/read(include_turns)` → `{status, last_turn_completed_at: None(无 turn)/Some(None)(未结束)/Some(Some(ts))(正常结束或中断)}`；连接或 `thread/read` 失败 ⇒ `None`；次要的 `thread/loaded/list` 失败被吞掉、记 `loaded=false` 仍返回 `Some(facts)`（线程事实本身仍权威） | `shared_codex_appserver.rs:4458-4475`；`crates/calm-provider/src/provider/codex.rs:30-50` |
| F24 | 唤醒：`TaskFailed`/`TaskCompleted` 非 Planner 作者即推；worker stop hook 仅当任务行仍 `dispatched|running` 才推（终态后被压）；gated 任务的 `task.failed` 只当 detail 类属 `worker-reported|spawn-failed|worker-timeout` 才推 | `dispatcher/mod.rs:109-150,186-232(:228),240-256,1058-1066` |
| F25 | 恢复 = 同 key 新代次、同契约；普通 worker 除「准备前失败」外均 `predecessor_not_quiescent` | `task_recovery/admission.rs:420-500`；`task_recovery/refusal.rs:21,40` |
| F26 | `calm.task.repair` 只收 isolated 空工作区 producer；key 为随机 `repair-<uuid>`/`review-<uuid>`；派生块写入报告并以回执校验 | `mcp_server/tools/task_repair.rs:11-24`；`file_delivery/repair.rs:117-129,160-190,228-262`；`track_report/write.rs:233` |
| F27 | Planner 可以对 codex worker 卡 `terminal.resolve/input` | `terminal_interaction/target.rs:115-125,219`（#1784 提议拒绝） |

### 1.4 租约、上游、候选（#1771/#1777/#1778）

| # | 事实 | 位置 |
|---|---|---|
| F28 | 租约 base 在 worker `prepare_tx` 内决议并冻结：上游或 HEAD；分叉 ⇒ `refused: attached-repo-diverged`，经 `spawn-failed` 落到任务 | `operation/codex_adapter/mod.rs:795-797`；`operation/workspace_lease/base.rs:238-287`；`operation/workspace_lease/upstream.rs:323,378-402`；`scheduler/mod.rs:1753` |
| F29 | `BaseSource` 有单元变体 `Commit`/`Attempt`；`LeaseBase.base_attempt_id` 字段「iff Attempt」；表 CHECK 接受 `base_source='attempt' AND base_attempt_id NOT NULL`；最新迁移 0115（0116 空闲） | `operation/workspace_lease/base.rs:42-62,126-131`；`crates/calm-truth/migrations/0111_workspace_lease_base.sql`、`0115_workspace_lease_upstream_base.sql` |
| F30 | `git worktree add -b <branch> <path> <commit-ish>` 钉到租约 base；三种注册态都以 `verify_worktree_base` 收尾 | `operation/workspace_lease/mod.rs:1163-1238` |
| F31 | 候选 = 一次成功内核交付：行不可改（trigger），`base_sha` = 租约 base，ref 在 common dir；`no_change` ⇔ `commit_sha == base_sha` | `crates/calm-truth/migrations/0113_task_git_deliveries.sql:68`；`git_candidate/candidate.rs:125,134`；`git_candidate/refs.rs:16-18`；`git_candidate/view.rs:145` |
| F32 | `base:{attempt}` 死路提示：`gate.cwd` 拒绝文案（`target.rs:631`）与 `calm.plan.list.md` 的 `target_mismatch`「continue with `base:{attempt}`」——该字段不存在 | `operation/task_verify_adapter/target.rs:628-634`；`prompts/tools/calm.plan.list.md:1` |
| F33 | worker 提示词 = goal + context + acceptance 原样渲染 | `operation/codex_adapter/mod.rs:798-803,1544` |

### 1.5 源码扫描门禁

| 门禁 | 规则 | 本设计哪里碰到 |
|---|---|---|
| `crates/calm-server/tests/cases/deferred_write_tx_invariant.rs` | 生产写事务一律 `begin_immediate_tx` | 片 1 cancel、片 2 回执事务 |
| `crates/calm-server/tests/cases/fork_guard_exemption_invariant.rs` | 报告写边界的结构性入口被钉死 | 片 2 追加块走现有 `track_report::write` 入口（先例 `planner_repair`） |
| `crates/calm-server/tests/cases/harness_turn_start_invariant.rs` | Planner turn 只能经 `IssueTurnHandle` | 本设计不发 Planner turn |
| `crates/calm-server/tests/cases/boot_invariants.rs` | boot 恢复不变量 | 片 1 清理标记沿用既有 boot/sweep 语义 |
| `scripts/gate-sync-event-version-lockstep.sh` | `SYNC_EVENT_VERSION` 与迁移戳一致 | 不加事件种类 ⇒ 不 bump |
| `scripts/gate-prose-ratchet.sh` | Rust 里不许新增长字面量 / CJK 串 | 拒绝文案放 `prompts/tools/*.md`，Rust 只放短 code |
| `scripts/gate-1316-terminology-ratchet.sh` | 术语棘轮（含 `docs/`） | 本文档已跑过（§9 末） |
| goldens `mcp_tool_registry.json`、`issue_development_planner_prompt.txt`、`worker_prompt_*.txt` | 工具描述/提示词逐字节 | 片 1/2 重生成（`runtime_status_matrix.json` 只管 worker-session 七态，不碰） |

## 2. 4140 证据

### 2.1 全库规模

| 量 | 值 |
|---|---|
| tasks / 有任务的 track | 76 / 10；全部终态（done 57，failed 19）；**在途 0**；kind：codex 55、claude 17、terminal 4 |
| 有任务的非终态 track | 0（tracks：done 17、draft 4、planning 2） |
| key 含 `.` / 最长 key | **0** / 41 字符 |
| `task_attempt_allocations` | initial 75、recovery 1 |
| `task_candidate_repairs`（`calm.task.repair`） | **0 行** |
| `workspace_leases` base_source | NULL 38（legacy）、head 12、upstream 18、`attempt` **0** |
| `task_candidates` / `review.round` 事件 | 26 / 19（3 条 track） |

### 2.2 三条重活 track 的 key 形态（同一规律，三种拼法）

| track | key 序列（摘） |
|---|---|
| `32acbdf9`（#1726） | `implement-change` → `fix-review-findings-r1..r5` + `review-pr-a/b`、`review-r3-a/b`…`review-r6-a/b` |
| `58bc82ae`（#1774） | `design-*`、`design2-*` … `design5-*`、`implement-1774` |
| `affb2b97`（#1772） | `design-*`、`design-*-r2`、`implement-ownership-squash-audit`、`repair-ownership-squash-audit-r1..r5`、`review-impl-*-r1..r3`、`final-review-*` |

`key GLOB '*-r[0-9]*'`：26/76。规律固定为（根、关系、轮次），拼法每条 track 自创。

### 2.3 `affb2b97`（#1772）逐项核实

Planner 卡 `5b4cbbe5…`：27 个 turn、94 次 MCP 调用（下文 `#n` 为按时间排序的 0 起调用序号）。

| issue 声称 | 实测 | 结论 |
|---|---|---|
| 18 个手写 key | 18 行 tasks、18 次 task 块 upsert（分布在 12 次 `report.commit`），**0 次删除 task 块**，task payload 合计 39,298 B | 成立 |
| 纯记账 44 次 | `report.commit` 20、`task.verdict` 16、`review.round` 6、`ratify.request` 2（1 次失败） | 成立（精确 44） |
| 每轮重新声明门禁 | 6 个 gated 任务的 `gate` JSON **完全相同（1 个 distinct 值）** | 成立 |
| 手抄 base/候选/发现 | 8 个评审块带 `base_sha/candidate_ref/candidate_sha`；r3/r4/r5 的 goal、acceptance、context 都写了 `git diff <A> <B> \| git apply`；r1/r2 context 写了 `base_sha` 与 `blocking_policy` | 成立 |
| verdict 逐个 | 16 次里 **12 次是对评审任务的 `accepted`**，3 次对 producer 的 `rejected`（均捎带 `lifecycle:"working"`），1 次最终 `accepted` | 12 次内核不需要（F11） |
| 切 lifecycle | 31 次迁移中 **20 次内核自动**（含 6 次认领时 `reviewing→working`）；Planner 11 次：`reviewing→working` 5、`working⇄blocked` 4、`working→reviewing` 1、`reviewing→done` 1 | Planner 的 5 次 `reviewing→working` 是 `:16` 允许的，但每次紧接着声明任务，认领本会自动推（F15） |
| 基线漂移（r2） | r2 17:52:54 失败：worker 按 Planner 写在 context 的 `base_sha` 拒绝新租约 HEAD `9aecedeb`（#1781 刚合入）；Planner 17:54:14 `ratify`，owner 裁决「在新 main 上 apply」 | 成立；根因是**基线由 Planner 写进契约**，内核租约按 #1777 正确 |
| r4 卡死 | 派发 18:39:03；#43–#49（18:39:51–18:42:27，7 次）是用 `terminal.input` 改范围本身；`last_thread_status='idle'`、`last_activity=18:41:24`、`last_turn_completed_ms=NULL`；**18:41:24 → 20:40:12 无事件唤醒 Planner（118 min）**；19:30:38 用户消息唤醒后 #50–#77（19:31:11–19:44:25，**28 次**：plan.list/resolve/observe/input/open ×3「Stop stale repair r4」/`task.fail` 被拒/`plan.cancel` 被拒）；#78 声明 r5 | 成立；**内核在 18:41:24 就有「turn 已结束」的提示，没人读** |
| 候选迁移可行性 | r3：`8ff1e135..c8678bfd` 搬到 `9aecedeb` 无冲突；r4/r5 搬 `9aecedeb..771a59c2` 到同一 base | 3 次迁移、**0 次冲突**；r3→r4→r5 正是**链式**形状 |

全库 4 个 `worker-timeout`，其中 codex 3 个：r4 `idle` 118 min；`32acbdf9` review-r6-a/b `active` 但 112 min 无活动（挂死，另一种形态，G4）；第 4 个是 `06ff541a` write-findings（claude，无 worker session，§9 的内连接看不到它）。
codex worker 会话的 `userMessage` 条数：46 个任务为 2（38 done、8 failed），唯一为 4 的是 r4——4140 上没有「turn 结束后被续一轮再完成」的样本。

## 3. 片 1：在途可停 + 空转必醒

### 3.1 `calm.plan.cancel` 接受 `running`

- **准入**：当前执行 `status='running'`、`task_has_running_liveness_deadline`（kind codex/claude、非子 track 派生路由，`scheduler/mod.rs:233-235`）、**非 isolated**（`isolated_codex::lookup::is_isolated_task_tx`；isolated 由自己的控制器停，`isolated_codex/observe.rs:69-93`，通用 sweep 也跳过它，`scheduler/mod.rs:1866-1881`）、`worker_card_id` 非空——正是通用超时通路能收割的集合。
  `dispatched` 拒绝（card 可能尚未绑定，清理标记写 0 行而 worker 随后起来成孤儿，F20）；`running` 但 card 为 NULL 拒绝（`mark_running(None)`，`scheduler/mod.rs:1679-1683`，G13）；terminal kind（4140 有 4 个）、isolated（4140 有 4 行）、`verifying` 拒绝。
  拒绝文案删去 `out of scope (#644)`，改为说明「等 `task.dispatched` 后的 running / 等 `task.gate_result`」。
- **写**：同一 `begin_immediate_tx` 内 CAS `status='running' AND worker_card_id=? → 'canceled'`，`status_detail='planner-canceled'`，写 F19 清理标记（`reason` 泛化为参数）；提交后 poke `sweep_timeout_worker_cleanups`。
  事件沿用 `plan.updated`（Planner 作者，不自唤醒）。不做 `working→reviewing` 自动推进（与 spawn-failed 通路 `scheduler/mod.rs:1738` 不同）：Planner 若要推 lifecycle，用 `plan.cancel` 已有的 `lifecycle` 参数。
- **竞态**：CAS 0 行 ⇒ `-32409`，文案带当前 `status`（`verifying`/`done`/`failed`/`canceled`）；不重试、不部分写。worker 报告与 cancel 两种先后都有测试（F21）。

### 3.2 codex 空转检测（#1782 缺陷 2）

持久化列是 best-effort（F22），只用来**挑候选**，终止证据来自权威实时读（F23）：

1. 候选：sweep 的 `Running` 臂（`scheduler/mod.rs:1897-1923`，已跳过 isolated，`:1866-1881`）里，kind=codex、`task_has_running_liveness_deadline`、`worker_card_id` 非空，且该卡 session 的 `last_thread_status='idle'`。
   每次执行都新建卡与线程（`operation/codex_adapter/mod.rs:784`；4140 上无任务卡有多于一个 session，§9），所以「该卡的线程」就是本执行的线程，不需要时间下界。
2. 权威确认：经 `Scheduler` 构造参数注入的 `CodexDaemonProbe`（今天 `Scheduler` 没有该句柄，`scheduler/mod.rs:466-510`）对该线程调 `read_liveness_facts`，要求**全部**成立：`status=Idle`；`last_turn_completed_at = Some(Some(ts))`（至少一个 turn 且已结束）；
   `now_ms - ts*1000 ≥ WORKER_IDLE_TURN_GRACE`（`ts` 是 Unix **秒**，`crates/calm-server/tests/fixtures/turn_completed_failed.json:25`；r4 为 `1790160084`）；`active_turn_id_for_thread` 为 `None`。
   每个候选的复核放进单独 spawn 并套总超时（该读是两次各 10 s 上限的 RPC），不阻塞串行 sweep；`read_liveness_facts` 返回 `None`（连接或 `thread/read` 失败）或总超时 ⇒ 本轮不动作，2 h deadline 兜底；`thread/loaded/list` 单独失败不影响判定（本臂不看 `loaded`）。
   turn 在 running 戳之前就结束（spawn 里开 turn，`operation/codex_adapter/mod.rs:1250`，早于 `mark_running`，`scheduler/mod.rs:1679`）同样被检测。
3. 失败：`fail_task_liveness_timeout` 增 `detail` 参数（今天硬编码 `"worker-timeout"`，`scheduler/mod.rs:2048`），CAS 追加 `worker_card_id=?`，detail `worker-turn-ended`。事件是现有 `task.failed`（`KernelDispatcher` 作者 ⇒ 推送）。

**为什么 300 s + 实时复核是安全的**：误杀只可能发生在「最后一个 turn 已结束 ≥ 300 s、线程当前 idle、无活动 turn」时仍有人会再开一轮。内核只为 worker 开一个 turn（无续轮机制，F33 渲染一次提示词），
唯一能续轮的是人往 worker 卡打字——#1784 对 Planner 关掉该入口；人 300 s 后才续轮的代价是一次 `worker-turn-ended` 唤醒，Planner 可 replace（G5）。4140 上 46/46 个非 r4 codex worker 会话都是单轮（§2.3）。

**宽限的归属**：`WORKER_IDLE_TURN_GRACE` 是 `scheduler` 模块常量，经 `Scheduler` 构造参数（typed）注入，测试传短值；**不加环境变量**（`task_run_timeout` 的 `NEIGE_TASK_RUN_TIMEOUT_SECS` 是既有遗留，`scheduler/mod.rs:56,483-484`，不效仿）。

**必须**把 `worker-turn-ended` 加进 `dispatcher/mod.rs:228` 的 pre-gate 列表：r4 是 gated 任务，漏加则唤醒被当「门禁后自报」吞掉。
Claude worker 不在本臂：它的 `hook.claude.stop` 已唤醒 Planner（F24），Planner 之后用 3.1 收场（G3）。

### 3.3 提示词（随片 1）

- `prompts/tools/calm.plan.cancel.md` + `planner.md` 取消条目：`running` 的 codex/claude 可取消并收割；`dispatched`/`verifying`/terminal 不行。
- `planner.md:74` 后一句：评审/审计任务不需要 `calm.task.verdict`；verdict 只给要接受/拒绝的 producer（呼应 `:73`）。
- `planner.md:16` 把 `reviewing → working when more work is needed` 改为「需要更多工作时直接声明任务，内核认领时推进；只有不声明任务而要回 working 时才写」；`:20` 不动。

### 3.4 4140 反事实

r4 的线程 18:41:24 结束 turn；宽限到 18:46:24，下一次 reconcile（周期 300 s，`scheduler/mod.rs:52`）在 18:46:24–18:51:24 之间以 `worker-turn-ended` 失败并唤醒 Planner，而非 20:40:12；#50–#77 这 28 次收拾调用与 3 张终端卡不会发生；#43–#49 的改范围本身由 #1784 拦下。
r4 与 r5 实际重叠 23 min（r5 19:49:27–20:12:44）。

## 4. 片 2：`calm.task.replace`

### 4.1 形状

```
calm.task.replace {
  key, expected_attempt_id, idempotency_key, reason,        // 必填
  goal, acceptance,                                         // 必填：新一轮的完整契约（不继承）
  context?,                                                 // 整体替换，缺省 {}（不继承）
  carry?: "candidate" | "none"                              // 缺省 "candidate"（有来源时）
}
→ { replayed, receipt_id,
    predecessor: {key, attempt_id, prior_status, stop: "canceled_now" | "already_terminal"},
    successor:   {key, attempt_id},                         // attempt_id = "{track}:{key}"（F4）
    carry:       {source_attempt_id, source_candidate_id, candidate_sha} | {none: <reason>} }
    // 全部字段落在回执里，重放原样返回
```

后继块 = 前驱块载荷的拷贝，再做：继承且不可覆盖 `gate` 与 `no_gate_reason`（二者原样同进同出；4140 上 6/6 门禁相同，#1772 另有 12 个块用 `no_gate_reason`）、`kind`、`depends_on`、`priority`、`spawn`；
`key` 换成派生 key；`declared_by=PLANNER_DECLARATION_AUTHOR`（`calm-types/.../tasks.rs:13`）、`ready:true`、去掉 `released_by_user`（照 `file_delivery/repair.rs:162-169`）；goal/acceptance/context 换成请求里的值。
**不继承** goal/acceptance/context：#1772 每轮修复本来就重写 goal，而旧契约里恰恰是要被消灭的指令（r1/r2 的 `base_sha` + `blocking_policy`，r3–r5 的 `git diff | git apply`，F33 会原样喂给 worker）。
内核在 carry 租约的 worker 提示词末尾追加一行固定说明（「工作树已含候选 `<sha>` 的改动，基于上游 `<U>`；不要自行 apply diff 或校验基线」），`worker_prompt_*` golden 同步；工具描述要求 goal/acceptance 不写 base 与 apply 指令。

**路由准入**（只看声明路由，与租约/交付状态无关）：Planner-only；track `workspace_kind='attached'`；kind ∈ {codex, claude}；`spawn` 为 Track 内默认路由（`TASK_IN_TRACK_ROUTE`）；非 isolated；track 生效策略**不是** `declare-and-wait`（后继会去掉 `released_by_user`，在该策略下不可调度、不会分配执行，F4 的 attempt id 与唤醒保证都不成立 ⇒ 在停前驱之前、同一事务内拒绝 `requires_user_release`；4140：23 条 track 的 `automation_policy` 全为 NULL、`decl_released_by_user=1` 的任务 0 个）。carry 是否可用由 §4.5 的来源决定，不是准入条件。isolated 用 `calm.task.repair`（F26，4140 0 行，不合并，G11）。

### 4.2 新 key 而非同 key 新代次

同 key 新代次要重建 append-only 的 `task_attempt_allocations`（CHECK 只允许 `initial|recovery`，F5），并让一个 key 在不同代次背不同契约，与 recover「同契约」（F25）冲突——一个值两个意思。
新 key = 一个 key 一份契约；执行 id 由 F4 推出，`track:key` 的既有消费者（verdict、delivery、runs 视图）零改动。

### 4.3 确定性 key

后继 key = `<root>.<n>`：`root` = 谱系第一个 key，`n` 从 2 递增。例：`implement-x` → `implement-x.2` → `implement-x.3`。
4140：0/76 个 key 含 `.`，最长 41 ⇒ 不撞现有 key，`root.99` ≤ 64。派生 key 已存在 ⇒ 拒绝 `derived_key_taken`，不跳号。
幂等：`(track, idempotency_key)` 唯一 + `predecessor_attempt_id` 唯一，同键同指纹重放、不同指纹冲突——与 recovery 的两条唯一索引同构（`0097:33-37`）。

### 4.4 事务内（一次 `begin_immediate_tx`，顺序即不变量）

1. 重放检查先于一切状态读：命中则**从回执**返回原响应（`prior_status`、`stop`、`carry` 都存在回执里，不重读任务行）。
2. `task_attempt_current_tx(track,key) == expected_attempt_id`，否则 `stale_attempt`。
3. 前驱 `running` ⇒ 片 1 的 CAS（detail `superseded: <succ key>`）+ 清理标记；`pending` ⇒ 现有 pending CAS；CAS 0 行 ⇒ 拒绝并带当前状态（同 3.1）。
4. 解析 carry 来源（§4.5）。
5. 经现有 `track_report::write` 入口（`planner_repair` 先例，`track_report/write.rs:233`）**追加**后继块（紧跟前驱块）；前驱块**原样保留**，所以评审块里 `depends_on` 旧 key 不产生 `unknown_dependency`（F9）。
6. 插回执 `task_replacements`（迁移 0116：`receipt_id, track_id, predecessor_attempt_id UNIQUE, predecessor_key, successor_key, request_idempotency_key, request_fingerprint, reason, prior_status, stop, source_attempt_id NULL, source_candidate_id NULL, carry_none_reason NULL, created_at_ms`（`carry` 响应由这些列 + 不可改的候选行重建，不另存 JSON）；`UNIQUE(track_id, request_idempotency_key)`、`UNIQUE(track_id, successor_key)`；不可改 trigger）。
7. 事件：`track.report_edited`（author planner）+ `plan.updated`——现有种类、不自唤醒。

1–7 在一个事务里（报告写入口本就在事务内，`track_report/write.rs:383-399,563-585`）：任一步失败整体回滚，无块、无回执、无事件——以直接的回滚原子性测试钉住。

**替换元数据不进报告块**：回执以 `(track, successor_key)` 为键，租约准备按 key 查回执；块里没有任何内核引用（`context.neige_execution` 是 isolated 严格选择器，F8，不碰）。
Planner 事后编辑后继块 = 普通声明编辑（在途执行照旧变 context-stale）；carry 归属于 key。编辑可能把后继改离支持路由（例如加上 isolated 选择器，调度器会先选 isolated 适配器，`scheduler/mod.rs:158`，绕过 carry）：**只在一处**拦——`build_worker_payload` 为有回执的任务选适配器前复核 §4.1 路由，不符 ⇒ `spawn-failed: refused: replace-route-changed`（推送），不逐个守编辑入口。前驱块保持终态，不会被再调度（终态不迁移，`644-plan-then-schedule.md:27`；测试钉住）。

### 4.5 carry：在新租约的 prepare 里做

**来源**：该执行的**已结算候选**（delivery `committed`/`no_change`），不看任务状态——门禁在交付结算后才准入（`scheduler/git_delivery.rs:69-78`），gate-red 的执行同样有候选；没有候选（pending、交付失败、准备前失败、legacy）才沿用前驱回执的来源（r4 这类「修复途中被转向」）；都没有 ⇒ `none`。来源只取**候选提交 `cand`**，不取 `from`。

`resolve_lease_base`（F28）对带回执且有来源的任务：

1. 照旧决议上游 `U`（分叉仍 `attached-repo-diverged`）。
2. `git merge-tree --write-tree --name-only --no-messages -z <U> <cand>`（两参数，git 2.39.5 可用；`--merge-base` 需 2.40）：退出 0 ⇒ 输出为 `OID\0`，即树 `T`；退出 1 且输出为 `OID\0path\0…` ⇒ 冲突路径（不解析本地化消息）；退出 1 但输出为空/不可解析（如 `<cand>` 缺失）、其它退出码、超时 ⇒ 基础设施错。
   合并基由 git 自动取 `merge-base(U, cand)`，即谱系的**上游部分**：链上每个 carry 基 `C'` 的父都是当时的上游，所以链式替换时累计改动全部保留（反例已核：以 `C'` 为基的 cherry-pick 语义会静默丢掉前一轮 carry 的改动）。
3. `git commit-tree T -p U -m "neige carry <receipt_id>"`，经 `neige_git_command`（`workspace_materialize.rs:55`），显式设置作者/提交者名与邮箱（固定内核身份）及两者日期（回执 `created_at_ms`）⇒ `C'`。同一回执、同一 `U` 重算得同一 `C'`；**总是**新建，保证 `C'^1 == U`。
4. 租约行 `base_sha=C'`、`base_source='attempt'`、`base_attempt_id=source_attempt_id`（F29，无迁移）；随后 F30 原样 `worktree add … C'`。

两条 `Command` 各自判 `ExitStatus`（不走 shell 管道）；两条命令都经 S4 G26 的有界执行器跑（`operation/task_verify_adapter/target.rs:317` 的 `run_sampling_command` 模式：`plugin_host/child_process.rs:114` `spawn_within` + `child_process/timed.rs:53` `finish_within`，进程组、4 s 总期限、`-c core.fsmonitor=false`）；`neige_git_command` 本身是无期限的 `std::process::Command`（`workspace_materialize.rs:55-61`），不能直接在 prepare 事务里用（U3）。纯对象库操作，不碰工作树、不建 ref；租约行冻结 `C'` 后崩溃重试幂等。

**读面**：`plan.list.candidate.carry{receipt_id, upstream_sha: C'^1, carry_sha: C'}`。整条改动 = `C'^1..candidate`；本轮修复 = `C'..candidate`。
carry 租约上的 `no_change`（`candidate == base_sha == C'`，F31）表示「carry 之外无新改动」，不是「无改动」——`calm.plan.list.md` 与 replace 描述写明。

冲突 ⇒ `refused: carry-conflict: <paths>` 经 `spawn-failed` 通路（`scheduler/mod.rs:1753`）落成 `task.failed` ⇒ 推送（`spawn-failed` 已在 pre-gate 列表）。该执行无候选，Planner 可对它再 replace `carry:"none"` 或自己声明解冲突任务（Q2）。

同片清扫死路提示（F32）：`target.rs:631` 的 `gate.cwd` 拒绝文案、`calm.plan.list.md` 的 `target_mismatch` 句、`operation/workspace_lease/base.rs:19,44-58` 注释、`crates/calm-types/src/observation.rs:345` 注释、`tests/cases/gate_binding.rs:718` 断言，统一改指 `calm.task.replace`。

### 4.6 oracle trace

| seq | phase | actor | trigger | external effect | observable event | invariant | status |
|---|---|---|---|---|---|---|---|
| 1 | request | Planner | `calm.task.replace(...)` | — | — | Planner-only（`require_role`，同 `task_repair.rs:18`） | NEW |
| 2 | admit | kernel tx | 同上 | — | — | 重放先于状态读；`expected_attempt_id` = 当前执行 | NEW（同构 `0097:33-37`） |
| 3 | stop | kernel tx | 前驱 `running` | — | `plan.updated` `event.rs:581` | 与报告写同一事务，任一步失败整体回滚；cancel 先赢则旧 worker 报告 0 行、无交付（`task.rs:308-330`）；报告先赢则拒绝 | 片 1 |
| 4 | stop | kernel tx | 同上 | 清理标记 | — | 与 CAS 同事务（`scheduler/mod.rs:77-106`） | 复用 |
| 5 | pin | kernel tx | 前驱有候选 | 无（ref 已在） | — | 候选行不可改（`0113:68`）；`candidate_for_attempt_tx`（`candidate.rs:134`） | 复用 |
| 6 | declare | kernel tx | — | 报告**追加**后继块 | `track.report_edited` `event.rs:409`（author planner，`dispatcher/mod.rs:134` 不自唤醒）+ `plan.updated` | 前驱块不变；无块内引用 | NEW（先例 `write.rs:233`） |
| 7 | receipt | kernel tx | — | `task_replacements` 行 | — | 每前驱 ≤1 后继；每请求键 ≤1 回执 | NEW 表 |
| 8 | return | kernel | 提交成功 | — | 工具结果 | `successor.attempt_id="{track}:{key}"`（`task_projection.rs:1624`） | NEW |
| 9 | reap | kernel sweep | 清理标记 | 中断 turn、收割 PTY、释放租约 | `workspace.released`、`terminal.deleted` | 失败下轮重试（`driver.rs:176-207`）；旧 stop hook 被压（`dispatcher/mod.rs:240-256`） | 复用 |
| 10 | claim | scheduler | 后继 ready | — | `task.dispatched` `event.rs:592`（`scheduler/mod.rs:1281`） | 前驱已终态，不占预算 | 复用 |
| 11 | carry | kernel prepare tx | 回执有来源 | 对象库写 `T`、`C'`（无 ref） | — | 两参数 merge-tree；确定性 `C'`、`C'^1==U`；≤4 s；`base_source='attempt'` | NEW（`operation/workspace_lease/base.rs:238`） |
| 12a | carry 冲突 | kernel | merge-tree 退出 1 | 无工作树 | `task.failed` `event.rs:533`，`spawn-failed: refused: carry-conflict: …` | kernel 作者 ⇒ 推送 | NEW detail |
| 12b | lease | kernel | 无冲突 | `worktree add … C'` | `workspace.leased` `event.rs:635`、`worktree.provisioned` | `verify_worktree_base`（`mod.rs:1163-1238`） | 复用 |
| 13a | 完成 | worker→kernel | `calm.task.complete` | 交付 commit + ref | `task.git_delivery_settled` `event.rs:561`；gated 再 `task.gate_result` `event.rs:772` | 恰好一次推送 | 复用 |
| 13b | 空转 | kernel sweep | 候选 idle + 实时复核 | 收割 | `task.failed` `worker-turn-ended` | 在 pre-gate 列表 | 片 1 |
| 13c | 超时 | kernel sweep | deadline | 收割 | `task.failed` `worker-timeout` | — | 复用（`scheduler/mod.rs:1972`） |
| 14 | replay | Planner | 同请求重试 | — | — | 同回执、`replayed:true`、无事件 | NEW |

**唤醒保证**（可枚举测试）：每个后继执行恰好以 {13a 非 `deferred_to_gate` 的结算、13a 门禁结果、12a/13b/13c 的 kernel `task.failed`、worker 自报 `task.failed`} 之一结束并推送；唯一不推送的出口是 Planner 自己的 cancel/replace。这是对既有事件的覆盖性断言，不是新机制。

### 4.7 producer × state 矩阵（`calm.task.replace` 结果）

| 前驱当前执行状态 | stop | carry 来源 | 结果 |
|---|---|---|---|
| 有已结算候选（delivery `committed`/`no_change`；任务 `done`、`failed`（如 gate-red）或 `canceled` 皆然） | 无（已终态） | **该候选** | 后继 |
| `pending`（无租约） | pending CAS→`canceled` | 前驱回执来源，否则 none | 后继 |
| `running`（codex/claude、非 isolated、card 非空） | CAS→`canceled` + 清理标记 | 前驱回执来源，否则 none | 后继 |
| 终态且无候选（交付失败/放弃、准备前失败含 `carry-conflict`、legacy 租约） | 无 | 前驱回执来源，否则 none | 后继（`carry.none` 带原因） |
| `dispatched`，或 `running` 但 `worker_card_id` 为 NULL | — | — | 拒绝 `predecessor_dispatching`（F20、G13） |
| track 策略 `declare-and-wait` | — | — | 拒绝 `requires_user_release`（先于任何写） |
| 后继被编辑离开支持路由（派发时） | — | — | 后继 `spawn-failed: refused: replace-route-changed` |
| `verifying` | — | — | 拒绝 `predecessor_verifying`（G1） |
| delivery `pending`（未结算） | — | — | 拒绝 `candidate_pending`（等 `task.git_delivery_settled`） |
| stop 的 CAS 0 行（并发推进） | — | — | 拒绝 `predecessor_changed{status}`，整事务回滚 |
| `expected_attempt_id` ≠ 当前 | — | — | 拒绝 `stale_attempt` |
| 已有后继（其它请求键） | — | — | 拒绝 `already_replaced{successor_key}` |
| 同请求键同指纹 / 不同指纹 | — | — | 从回执重放 / 拒绝 `idempotency_conflict` |
| 有非终态依赖者 | — | — | 拒绝 `pending_dependents{keys}`（G6） |
| 路由不符（§4.1） | — | — | 拒绝 `unsupported_route` |
| 派生 key 已存在或 > 64 | — | — | 拒绝 `derived_key_taken` / `derived_key_too_long` |
| track 终态 | — | — | 拒绝 `track_terminal` |

`carry:"none"` 时来源一律 none。carry 结果（租约 prepare）：clean ⇒ `base_source='attempt'`；rc=1 且输出可解析为 OID + ≥1 路径 ⇒ `spawn-failed: refused: carry-conflict`；rc=1 但输出为空/不可解析（如来源对象缺失）、其它 rc、超时 ⇒ `spawn-failed: carry-infra`。三者都是「准备前失败、无候选」，可再 replace（回执来源不变；冲突要换 `carry:"none"`）。
新 `status_detail` 值：`planner-canceled`、`superseded`（canceled 行）、`worker-turn-ended`（failed 行）。

## 5. issue「默认化」表逐行取舍

| issue 行 | 取舍 | 一行理由 |
|---|---|---|
| 内核生成确定性 key | **做**（片 2，仅 replace 后继） | 18 个 key 中 5 个是修复改名；根 key 仍是 Planner 的命名判断 |
| 修复任务继承门禁 | **做**（片 2） | 6/6 相同 |
| 内核注入 base/候选/lineage | **做**（回执 + `plan.list.candidate.carry` + worker 提示词一行）；goal/acceptance/findings 仍由 Planner 写 | 哪条发现算阻塞是判断 |
| 内核迁移 delta、冲突 fail closed | **做**（片 2，两参数 merge-tree） | r2 失败 + owner 介入的直接根因 |
| 按轨道评审策略自动派发评审 | **砍策略**；「沿用上一轮评审集合」放片 3（有条件） | 58bc82ae 的浪费来自评审预算判断，策略对象替不了 |
| `review.round` 自动汇总 | **砍** | git 评审者结果是自由文本（需 S5 findings 模型）；唯一用途 F4 围栏本是 prompt-only（F14） |
| 批准的 verdict 自动接受 | **砍机制，改提示词** | 内核从不要求（F11）；12/16 可省 |
| lifecycle 从任务状态推导 | **砍** | 已推导（20/31 自动）；剩下 `blocked`/`done` 是判断；`:16` 的措辞改一句 |
| 任务不写在报告正文 | **砍**：沿用追加块 | 原地改写会让 8 个评审块的 `depends_on` 变 `unknown_dependency`（F9）；报告照旧每轮 +1 块（G9） |

## 6. 四个待定问题的建议答案

**Q1 评审策略粒度** → **按任务，由 Planner 在首轮声明，replace 时沿用或显式缩减**；不做轨道级策略对象、不做内核按体量分级。
证据：58bc82ae 107 min / 10 worker / 0 行代码来自首版设计过重 + 单通道否决（#1727 comment 2026-09-22）；affb2b97 靠 owner 缩范围收敛——都是判断。体量分级写进模板/评审合同。

**Q2 迁移冲突** → **fail closed 并唤醒**（`spawn-failed: refused: carry-conflict: <paths>`）。
证据：4140 上 3 次迁移 0 次冲突；Planner 自己的 `apply_policy` 是「冲突才停、不得自行解冲突」（#34/#41/#78 的 context）。内核不自动派解冲突 worker。

**Q3 key 格式与 idempotency 兼容** → `<root>.<n>`；任务 `idempotency_key` 仍 `"{track}:{key}"`（F4）；请求幂等 = `(track, idempotency_key)` 唯一 + 每前驱唯一后继。
兼容证据：4140 76 个 key 里 0 个含 `.`、最长 41；无在途任务；不动 `task_attempt_allocations`。

**Q4 报告里的任务摘要与折叠** → **本设计不折叠**：后继块追加在前驱块之后，前驱块保留（F9 使原地改写不可行：affb2b97 的 8 个评审块 `depends_on` 旧 key）。
报告增长与今天相同（每轮 +1 块，但块内不再有手抄 sha/apply 散文，也不再有前驱门禁以外的新门禁）。折叠留给读面（前端按终态收起），不改声明模型（G9）。

## 7. 切片（按顺序，各自可合）

| 片 | 内容 | 估算 | 验收（必须绿） | 必须红（变异/反转） |
|---|---|---|---|---|
| **1** 在途可停 + 空转必醒 | `plan.cancel` 收 `running`（codex/claude、非子 track 派生路由（`spawn` 判据见 `scheduler/mod.rs:234`）、非 isolated、`worker_card_id` 非空）；新 CAS + 泛化清理标记 + poke；sweep 空转臂（候选 + 注入的 `CodexDaemonProbe` 复核，限时 spawn）+ 构造参数宽限；`fail_task_liveness_timeout` 加 detail；`worker-turn-ended` 进 `dispatcher/mod.rs:228`；3.3 提示词；goldens | ~450 | running codex/claude cancel ⇒ `canceled/planner-canceled`、session `failed`、租约 `released`；cancel 先赢 ⇒ 迟到 `task.complete` 被拒且无交付行；报告先赢（`verifying`）⇒ cancel 拒绝带状态；`dispatched`/terminal/isolated/`verifying`/card NULL 拒绝且文案无 `#644`；空转：**r4 真值夹具对**（`completed_at=1790160084` 秒 = 18:41:24）：`now`=18:46:23（299 s）⇒ 不动，`now`=18:46:25（301 s）⇒ `failed/worker-turn-ended` 且推送（gated 与非 gated 各一）；turn 在 running 戳**之前**已结束 ⇒ 同样检测；持久化 idle 但实时 `Active` / `last_turn_completed_at=None` / `Some(None)` / 活动 turn 非空 / `read_liveness_facts` 为 `None` 或超时 / 宽限内 ⇒ 不动；`thread/loaded/list` 失败（`loaded=false`）而线程事实满足 ⇒ 照常触发；Claude、isolated 不动 | `tests/cases/mcp_plan.rs::cancel_in_flight_task_refused_with_409_text` 反转为 `cancel_running_task_cancels_and_reaps_worker`；变异①删空转臂 ⇒ 仅空转正例红；②从 `:228` 去掉 `worker-turn-ended` ⇒ 仅 gated 推送红；③去掉实时复核只看持久化列 ⇒ 所有「持久化 idle 但实时不满足」的反例红（实时 `Active`、`None`、`Some(None)`、活动 turn 非空、读失败/超时）；若该变异改读 `last_turn_completed_ms`，r4 正例也红（r4 该列为 NULL，`liveness_feeder.rs:67`）；④CAS 放宽到 `dispatched` ⇒ dispatched 拒绝测试红；⑤去掉 `*1000`（`now_ms - ts` 恒约 1.79e12）⇒ 仅 r4 夹具对的 18:46:23「不动」半边红 |
| **2** `calm.task.replace` + carry | 迁移 0116；工具 + 描述 + 注册 golden；追加块 + 回执；`resolve_lease_base` carry 分支；worker 提示词一行 + golden；`plan.list.candidate.carry` 与 `no_change` 说明；死路提示清扫（§4.5 末）；`planner.md` 修复轮一段 | ~1000（超了拆 A=表+carry 经测试 seam，B=工具+提示词） | §4.7 每行一测；**链式两次替换**：`U0→C'1(+A)→cand1(+B)`，上游进到 `U1(+X)` 后第二次 replace 的租约树含 A、B、X（且 `U==U0` 时同样含 A、B）；gate-red 前驱 ⇒ carry 其候选；**被 `task.verdict rejected` 后仍为 `done` 行的前驱 ⇒ carry 其候选**（#1772 的主路径）；`declare-and-wait` track ⇒ `requires_user_release` 且前驱未被停；后继被编辑成 isolated 选择器 ⇒ 派发时 `replace-route-changed` 且推送；来源 `<cand>` 缺失 ⇒ `carry-infra`；冲突 ⇒ `carry-conflict` 且推送，再 replace 该失败执行被准入；同一回执两次 prepare 得同一 `C'`；前驱块字节不变、评审块无新诊断、canceled 前驱不再被调度；**响应丢失重放**：首次提交后丢弃响应、同请求重放返回相同 `prior_status/stop/carry/successor`；**回滚原子性**：stop CAS 0 行 ⇒ 无块、无回执、无事件；门禁与 `no_gate_reason` 与前驱逐字节相等、后继 `ready:true`/`declared_by` 为 Planner 声明作者；后继 context 不含前驱 context 的任何键；§4.6 唤醒保证枚举 | 变异①carry 基用 `U` 丢候选 ⇒ 所有带来源的 carry 测试红（单次、链式、gate-red、verdict 拒绝后的 `done`、冲突）；②改用以前驱租约 base 为合并基的 cherry-pick 语义 ⇒ **仅链式测试红**；③去掉 `predecessor_attempt_id` 唯一 ⇒ `already_replaced` 红；④回执不存 `prior_status` 而重放时重读任务行 ⇒ 仅响应丢失重放测试红；⑤来源改回「只看 done」⇒ 仅 gate-red 测试红；⑥去掉派发时路由复核 ⇒ 仅 `replace-route-changed` 测试红；⑦去掉策略检查 ⇒ 仅 `requires_user_release` 测试红 |
| **3**（有条件）评审沿用 | replace 同时为前驱的终态直接依赖者（上一轮评审）追加 `<root>.<n>` 后继、依赖新 producer；认领时把依赖候选 `{ref, sha, carry range}` 注入评审提示词；参数 `reviewers: []` 可缩减 | ~600 | 一轮修复 = Planner 一次调用；评审提示词里的 sha = 依赖的已结算候选 | 变异：注入前驱而非新 producer 的候选 ⇒ 红 |

门槛：片 3 只在片 2 部署到 4140 并跑完一条真实开发轨后，按访谈决定做或砍。
前置/伴随：#1784（拒绝对 codex worker 卡 `terminal.input`）先于或随片 1 合入。每片流程照 `AGENTS.md`。

### 7.1 affb2b97 反事实（估算，基于 §2.3 计数）

| 量 | 实际 | 片 1+2（+提示词） | +片 3 |
|---|---|---|---|
| 手写 key | 18 | 13（5 个修复变派生，r2 不再发生） | 7 |
| 纯记账调用 | 44 | ≈26：commit 14、verdict 1、round 6、ratify 1、replace 4 | ≈23 |
| 手抄 sha / apply 散文 | 13 个块 | 8 个评审块 | 0 |
| r4 静默卡死 | 118 min + 28 次收拾调用 | 5–10 min（宽限 + 一个 reconcile 周期） | — |
| 基线失败 + owner 裁决 | 1 + 1 | 0 | — |

调用数降幅有限；主要收益是**没有手抄事实、没有 r2/r4 两类失败**。报告块数不降（Q4）。

## 8. 与 issue 原文相左的发现

1. **「对评审逐个 `task.verdict`」不是内核要求**：`planner.md:73`；16 次里 12 次可省。解法是提示词。
2. **「切 lifecycle」已由内核推导**：31 次里 20 次自动；Planner 的 5 次 `reviewing→working` 合乎 `planner.md:16`，但每次都紧接声明任务，认领本会推进——改措辞，不加机制。
3. **stop proof 不是启动后继的安全前提**：cancel 的 CAS 先赢时旧 worker 的报告 0 行、不会交付候选（F21）。r4 与 r5 在 4140 上重叠 23 min 无害。停旧只为资源/预算，可异步（F19 sweep）。
4. **「返回新 turn id」做不到也不需要**：后继的 worker turn 在工具返回时尚不存在；上游 sha 在认领时才决议，走 `plan.list`。
5. **r4 卡死的提示内核早就有**：`last_thread_status='idle'` 在 18:41:24 已落库，缺一个读者 + 实时复核——比 steer 原语便宜，放片 1。
6. **r2 基线失败的根因是 Planner 把 base 写进契约**：所以 replace 不继承 goal/acceptance/context，carry 由内核拥有。
7. **原片 5 的 `base:{attempt}` 已有死路提示在生产上**（F32），片 2 顺手修掉。
8. `calm.task.repair` 不支持 attached 属实，但它在 4140 上 0 次使用；不扩展、不合并（G11）。
9. **「任务不写在报告里」做不到最简**：原地改写破坏评审块依赖（F9），本设计保留追加块。

## 9. 复现命令（4140，只读）

```bash
DB=~/.local/share/neige-next/data/calm.db; T=affb2b97103d45c78bf50b81b7b058bf
sqlite3 -readonly $DB "select count(*), count(distinct track_id), sum(key glob '*-r[0-9]*'), sum(key like '%.%'), max(length(key)) from tasks"
sqlite3 -readonly $DB "select status, count(*) from tasks group by 1; select kind, count(*) from tasks group by 1"
sqlite3 -readonly $DB "select base_source, count(*) from workspace_leases group by 1"
sqlite3 -readonly $DB "select count(*) from task_candidate_repairs"
sqlite3 -readonly $DB "select case when json_extract(payload,'$.agent_message') like '[auto]%' then 'auto' else 'planner' end,
  json_extract(payload,'$.from')||'->'||json_extract(payload,'$.to'), count(*) from events
  where kind='track.lifecycle_changed' and scope_track='$T' group by 1,2"
sqlite3 -readonly $DB "select last_thread_status, datetime(last_activity_ms/1000,'unixepoch','localtime'), last_turn_completed_ms
  from worker_sessions where card_id='1b83e377ae054f9289ed0ea19158ab6e'"
sqlite3 -readonly $DB "select t.key, ws.last_thread_status, (t.finished_at_ms-ws.last_activity_ms)/60000
  from tasks t join worker_sessions ws on ws.card_id=t.worker_card_id where t.status_detail='worker-timeout'"
sqlite3 -readonly $DB "select ws.provider, u.n, t.status, count(*) from (select worker_session_id s, count(*) n from worker_flow_items
  where kind='userMessage' group by 1) u join worker_sessions ws on ws.id=u.s join tasks t on t.worker_card_id=ws.card_id group by 1,2,3"
sqlite3 -readonly $DB "select count(*) from (select card_id from worker_sessions where card_id in (select worker_card_id from tasks)
  group by card_id having count(*)>1)"                                   # 复用的任务卡：0
sqlite3 -readonly $DB "select count(*) from tasks where id in (select idempotency_key from operations where kind='codex-isolated-worker')"  # isolated 执行：4
```

Planner 调用计数（94 次、#n 序号、44 次记账、12 次 task 块提交、门禁去重 1 值 × 6、context 键）取自 Planner 卡 `5b4cbbe5d6b64b95adc400c70e60220c` 的会话 item 表
（`.tables` 里以 `harness_` 开头的 items 表；`item_type='mcpToolCall' AND method='item/completed'`，解析 `params.item.tool/arguments`），方法同 #1727 取证。

carry 命令核验（本机 `git version 2.39.5`）：`git merge-tree --write-tree --merge-base=<X> A B` ⇒ `unknown option`，rc=129；
链式场景 `U0→C'(a=A1)→cand(b=B1)`、`U1(x=X1)`：`git merge-tree --write-tree U1 cand` ⇒ rc=0，树内 `a=A1 b=B1 x=X1`。

门禁：`bash scripts/gate-1316-terminology-ratchet.sh` 在本提交上通过（输出见提交说明所附报告）。

## 10. 风险与 KNOWN GAPS

- **G1** `verifying`（门禁在跑）与 `dispatched` 既不能 cancel 也不能 replace；分别等 `task.gate_result` / running。
- **G13** `running` 但 `worker_card_id` 为 NULL（`scheduler/mod.rs:1679-1683`）时 cancel 拒绝、空转臂跳过，只剩 2 h deadline。
- **G2** 取消后旧 worker 最多再活一个 sweep 周期，而预算槽已释放 ⇒ 瞬时多一个进程。
- **G3** 空转检测只覆盖 codex worker；Claude worker 靠 stop hook 唤醒，任务仍 `running`，由 Planner cancel/replace。
- **G4** 「`active` 但无活动」的挂死（32acbdf9 r6-a/b，112 min）不检测，2 h deadline 兜底。
- **G5** 人在 worker 卡 idle 超过 300 s 后才续轮会被收割（#1784 已禁 Planner 这么做）；这是片 1 后唯一能误杀健康任务的情形。
- **G6** 前驱有非终态依赖者时 replace 拒绝，不改指向（片 3 只处理终态评审者）。
- **G7** carry 是 3-way 语义，比 `git apply` 宽容；二进制/改名冲突一律按冲突拒绝。上游被改写（非快进）时 merge-base 可能落在更早处，冲突即拒绝。
- **G8** `C'` 在 `worktree add` 前无 ref 引用，依赖 gc 默认 2 周宽限；实际窗口毫秒级。
- **G9** 报告每轮仍 +1 块；折叠留给读面。
- **G10** `review.round` 仍 Planner 手写，F4 合并围栏仍 prompt-only。
- **G11** `calm.task.repair`（isolated）与 `calm.task.replace`（attached）并存。
- **G12** `ratify` pending 与 lifecycle 不同步（第二轮访谈 P1）不在本设计内。

### 实现前 spike

- **U1**（v3 关闭）：中断的 turn 带 `completed_at`（r4 rollout `turn_aborted`，`completed_at=1790160084`），`codex.rs:35-36` 注释亦称「deliberately aborted」算 `Some(Some)`；片 1 以 r4 真值夹具钉住。
- **U3** 本仓 `merge-tree --write-tree` + `commit-tree` 在 prepare 事务内 < 4 s（片 2 前置）。
- **U4** 两参数 `merge-tree --write-tree` 需 git ≥ 2.38：本机 2.39.5 已实跑（§9）、CI ≥ 2.43。

## 11. 第 1 轮处置（Round-1 resolutions）

| 发现 | 处置 | 证据 |
|---|---|---|
| A1 / B1 `--merge-base` 在 2.39.5 不可用 | 接受 | 本机实跑 rc=129 `unknown option`（§9）；改两参数形式（§4.5） |
| A2 / B3 `from` 未定义，链式丢改动 | 接受 | 链式实跑 `a=A1 b=B1 x=X1`；来源只取 `cand`，合并基由 git 取谱系上游部分；加链式验收 + 专杀变异② |
| A3 `dispatched` cancel 留孤儿 | 接受 | `scheduler/mod.rs:77-106`（标记按 card 写）、`:1712-1735`（0 行只 debug）；只收 `running` |
| B5 cancel 收了清理扫不到的 kind | 接受 | `scheduler/mod.rs:2104`（只扫 codex/claude）、`:233-235`；4140 terminal 4 个；准入 = `task_has_running_liveness_deadline` |
| B2 元数据进 `neige_execution` 被严格解码拒绝 | 接受 | `task_execution.rs:18-29`（`deny_unknown_fields`）、`kinds.rs:260-270`；改为回执按 `(track, successor_key)` 查，块内无引用 |
| B7 原地改写使评审 `depends_on` 失效 | 接受 | `task_projection.rs:1014`、`tasks.rs:528`；改追加块，Q4/片 2/§7.1/G9 同步 |
| A9 CAS 与写块顺序 | 接受 | 追加块后前驱块不变；顺序写成 §4.4 不变量 + 变异④ |
| B6 继承的 goal/acceptance/context 带旧基线与 apply 指令 | 接受 | r1–r5 块内容（§2.3）、`codex_adapter/mod.rs:1544`；goal/acceptance 必填、context 不继承、内核加一行说明 |
| B4 空转判据用 best-effort 列 | 接受 | `liveness_feeder.rs:123,163`；持久化列只挑候选，`read_liveness_facts`（`shared_codex_appserver.rs:4458-4475`）复核并限定本执行 |
| A4 carry 后 `no_change` 语义 | 接受 | `view.rs:145`；读面与描述写明 |
| B8 术语棘轮红 | 接受 | 原 §9 两处旧术语表名；已改写，门禁重跑通过 |
| A5 竞态拒绝 / 自动 lifecycle | 接受 | §3.1、§4.7 `predecessor_changed`；不做自动推进（对照 `scheduler/mod.rs:1738`） |
| A6 `planner.md:16` | 接受 | `:16` 明列该边；§3.3、§8.2 改 |
| A7 计数 | 接受 | #43–#49 是改范围本身（7 次）；唤醒后收拾 #50–#77 = 28 次；#78 声明 r5 仍需；重叠 23 min。A 所说「#51–#78」与本文 0 起序号差一位，内容一致 |
| A8 环境变量配置 / detail 硬编码 | 接受 | `scheduler/mod.rs:56,483-484`、`:2046`；宽限为构造参数，不加 env；加 detail 参数 |
| B9 F19 围栏表述过度 | 接受 | `decision_sink.rs:160-210`；改为两种先后，均入验收 |
| B10 G5 迟到 stop hook | 接受（删 G5） | `dispatcher/mod.rs:240-256,1058-1066` 已压终态行的 hook |
| B11 引用 | 接受 | F5 `0097:119-127`；F25 准备前失败例外 `admission.rs:488-500`；F26 `review-<uuid>` `repair.rs:161`；F29 `Attempt` 单元变体；0116 空闲 |
| B12 `target.rs:631` 用途 / `runtime_status_matrix` | 接受 | `target.rs:628-634` 是 `gate.cwd` 拒绝；golden 只管七个 session 态（`runtime_status_matrix_golden.rs:19-25`），已移出 |
| A10 切片表其余一致 | 无需改 | — |

无驳回项：每条都在代码 / 库 / git 实跑上复现。

## 12. 第 2 轮处置（Round-2 resolutions）

| 发现 | 处置 | 证据 |
|---|---|---|
| A1 / B1 `completed_at` 是秒 | 接受 | `crates/calm-server/tests/fixtures/turn_completed_failed.json:25`（「Unix SECONDS」）；r4 rollout `completed_at=1790160084` = 2026-09-23 18:41:24 CST（`date -d @1790160084`）；§3.2 统一换算 `ts*1000`，片 1 加 r4 真值夹具 |
| A3 / B2 删「≥ running 起点」 | 接受（删除，不做替代绑定） | 每次执行新卡新线程（`operation/codex_adapter/mod.rs:784`）；4140 上被复用的任务卡 0 个（§9 查询）；「running 戳之前 turn 已结束」列为**会被检测**的验收例 |
| A2 / B5 carry 来源 | 接受 | 门禁在交付结算后才准入（`scheduler/git_delivery.rs:69-78`），gate-red 行也有候选；来源 = 该执行的已结算候选（不看任务状态），无候选才回退回执来源；§4.5/§4.7 一致 |
| B3 cancel 含 isolated | 接受 | sweep 另行跳过 isolated（`scheduler/mod.rs:1866-1881`，`is_isolated_task_tx`）；cancel 与空转臂都排除 |
| A8 running 但 `worker_card_id` NULL | 接受（拒绝，不查找） | `mark_running(None)`（`scheduler/mod.rs:1679-1683`）；少一条查找路径即少一个机制；G13 |
| B4 replace 准入与租约策略混写 | 接受 | 准入改为只看路由（§4.1）；pending/无租约/carry 冲突后的失败执行都按「无候选 ⇒ 回执来源」统一处理 |
| A10 `legacy_lease` 行 | 接受（删行） | 4140 所有 legacy 租约都在终态 track 上、无在途任务（§2.1）；legacy 执行无候选，落入「无候选」行 |
| A9 / B6 `no_gate_reason`、`declared_by`、`ready` | 接受 | `calm-types/.../tasks.rs:551-564,807-827`；照 `file_delivery/repair.rs:162-169` 设 `declared_by=PLANNER_DECLARATION_AUTHOR`（`calm-types/.../tasks.rs:13`）、`ready:true`、去掉 `released_by_user` |
| B7 回执不能重现响应 | 接受 | 回执加 `prior_status`、`stop`（v4：`carry_json` 去掉，由列重建）；重放从回执返回；加「响应丢失后重放」测试 |
| B8 顺序变异无区分力 | 接受 | `track_report/write.rs:383-399,563-585` 同事务；改为直接的回滚原子性测试 |
| A4 commit-tree 身份与确定性 | 接受 | 显式作者/提交者名、邮箱、日期（回执时间），经 `neige_git_command`（`workspace_materialize.rs:55`）；重试得同一 `C'` |
| A5 merge-tree 消息本地化 | 接受 | 本机消息为中文；改 `--write-tree --name-only --no-messages -z`，先 OID 后路径 |
| A6 死路提示清扫不全 | 接受 | `operation/workspace_lease/base.rs:19,44-58`、`tests/cases/gate_binding.rs:718`、`crates/calm-types/src/observation.rs:345` 并入 |
| A7 Scheduler 无 probe；RPC 未限时；时间表述 | 接受 | `scheduler/mod.rs:466-510` 无 `CodexDaemonProbe`；构造参数注入；每候选一个带总超时的 spawn；reconcile 周期 300 s（`:52`）⇒ r4 在 18:46–18:51 醒 |
| B9 引用 | 接受 | F28–F30 改 `operation/workspace_lease/…`；F13 单调在 `review.rs:405-425`；F19 `"worker-timeout"` 在 `:2048` |
| A11 / A12 | 信息 | 无需改；A12 即 G5 |

无驳回项。

## 13. 第 3 轮处置（Round-3 resolutions）

| 发现 | 处置 | 证据 |
|---|---|---|
| B1 declare-and-wait 下后继不可调度 | 接受；选**原子拒绝**（少一种响应形态） | 4140：`automation_policy` 23/23 为 NULL、`decl_released_by_user=1` 0 行；`task_projection.rs:989,1106,1617`；§4.1 准入 + §4.7 行，先于任何写 |
| B2 编辑使后继离开支持路由 | 接受；**派发时一处复核** | `scheduler/mod.rs:158`（`build_worker_payload` 先选 isolated）；`replace-route-changed`，不守编辑入口 |
| A1 / B3 `*1000` 变异预测错 | 接受 | 301 s 时两种写法都 ≥ 300000；改 r4 夹具对 18:46:23/18:46:25，预测「不动」半边红 |
| A2 变异红集不全 | 接受 | 片 1 ③、片 2 ① 列出完整红集 |
| A3 rc=1 也用于来源缺失 | 接受；删 `carry-source-missing` | rc=1 且输出空/不可解析 ⇒ `carry-infra` |
| A4 NULL card 行 | 接受 | §4.7 与 `dispatched` 合为一行，复用 `predecessor_dispatching` |
| A5 4 s 上限没接线 | 接受 | `workspace_materialize.rs:55-61` 无期限；改用 `target.rs:317` 的 `spawn_within`/`finish_within` 模式 |
| A6 carry 双存 | 接受 | 保留列 + `carry_none_reason`，删 `carry_json` |
| A7 缺「verdict 拒绝后 done」验收；F10 措辞 | 接受 | `decision_sink.rs:258+` 只写事件；#1772 三个被拒 producer 行仍 `done` |
| A8 超时计数 | 接受 | 4 行 `worker-timeout`，codex 3（第 4 个 claude、无 session） |
| B4 probe 失败精度 | 接受 | `shared_codex_appserver.rs:4464-4472`：`thread/loaded/list` 失败被吞、仍 `Some(facts)`；F23 与片 1 验收区分 |

无驳回项。
