# 任务声明默认化 + `calm.task.replace`（#1785，#1727 片 5 新定义）— 设计 v1

> **状态（2026-09-23）**：设计稿 v1，未经设计评审。基线 `origin/main` = `2ef67292d`（工作树 `design/1785-task-replace`）。
> 所有 file:line 在该基线上实测；路径省略 `crates/calm-server/src/` 前缀，其余写全；`event.rs` 指 `crates/calm-types/src/event.rs`。
> 4140 数据来自只读 `~/.local/share/neige-next/data/calm.db`（`events.max(id)=34967`，最后事件 2026-09-23 13:58:52Z），
> 复现命令见 §9。
>
> **owner 规则**：(1) 只解决在 #1772 / #1782 上**观察到的**症状，假想情形写成一行 KNOWN GAP；(2) 兼容只看 4140 库里真实存在的东西。

## 0. 结论先行

- **片 1（最小、止痛最多）**：`calm.plan.cancel` 接受 `dispatched|running`（复用已有的「liveness 超时 → 清理标记 → sweep 杀 worker」通路），
  外加一个 scheduler sweep 臂：codex worker 的线程已 `idle`、任务仍 `running` 超过宽限期 ⇒ 内核以 `worker-turn-ended` 失败该任务（唤醒 Planner）。
  没有新事件种类、没有迁移。它直接消除 #1772 r4 的 **118 分钟静默卡死**和 Planner 为收拾它花掉的 **35 次工具调用**（§2.3）。
- **片 2**：`calm.task.replace`——停旧（片 1 的 CAS）→ 内核派生后继声明（确定性 key `<root>.<n>`，继承门禁/验收/依赖）
  → 在**新租约准备时**用 `git merge-tree` 把旧候选的 delta 搬到当前上游，冲突 ⇒ `spawn-failed: refused: carry-conflict` 唤醒。
  一张新表（回执），零新事件种类；租约表已有 `base_source='attempt'` 列形状（0 行在用）。
- **片 3（有条件）**：替换时连带重派评审（评审者复用上一轮集合 + 候选事实注入）。只在片 2 上真栈复测后，若「每轮重声明评审」仍是主要负担才做。
- 另有两句**提示词**修正（不需要机制）：评审任务不需要 `task.verdict`；不要写 `reviewing→working`。见 §5。
- 与 issue 原文相左的发现见 §8。

## 1. 事实表（`origin/main` = `2ef67292d`）

### 1.1 任务声明、身份、门禁

| # | 事实 | 位置 |
|---|---|---|
| F1 | 任务 = 报告里的 `task` 块；`TaskDeclaration{key,kind,goal,acceptance,gate,no_gate_reason,depends_on,context,…,tombstone}`，**没有** `base` 字段 | `crates/calm-types/src/report_blocks/tasks.rs:121-145` |
| F2 | key 形状 `^[a-z0-9][a-z0-9._-]{0,63}$`（`.` 合法） | `calm-types/.../tasks.rs:444-453` |
| F3 | Planner 提示词：用 `calm.report.read` 取 `docRev`，再 `blocks.upsert`/`report.commit` 写 task 块 | `crates/calm-server/prompts/planner.md:68`、`:78` |
| F4 | 首代执行 id = `"{track}:{key}"`（所以任务 `idempotency_key` 就是 `track:key`） | `crates/calm-truth/src/db/sqlite/task_projection.rs:1621-1624` |
| F5 | 代次表 `task_attempt_allocations`：`origin_json.kind` 只允许 `initial`/`recovery`；recovery 前驱必须是当前 `failed` 执行；表 append-only | `crates/calm-truth/migrations/0097_task_attempt_allocations.sql:13-45` |
| F6 | `require_task_gates` 时非 terminal 任务必须有 `gate` 或 `no_gate_reason`；agent 任务的 `gate.cwd` 被拒 | `calm-types/.../tasks.rs:551-585` |
| F7 | 门禁没有继承概念：每个新 key 必须重写整份 `gate` | F1 + F6（无继承字段） |

### 1.2 评审、裁决、lifecycle

| # | 事实 | 位置 |
|---|---|---|
| F8 | `calm.task.verdict`：`accepted` ⇒ `TaskCompleted`，`rejected` ⇒ `TaskFailed`（Planner 作者），可捎带 `lifecycle` | `mcp_server/tools/track_state.rs:215-241`；`decision_sink.rs:258-330` |
| F9 | **`depends_on` 等 `Task.done`，不等 verdict；纯顺序依赖不需要 verdict** | `prompts/planner.md:73` |
| F10 | git 候选的「接受决定」没有表（S4 片 6 未做）；`task_candidate_decisions` 只服务 isolated file-delivery | `file_delivery/candidate_qualification.rs`（唯一写者，`grep -rln task_candidate_decisions`） |
| F11 | `calm.review.round`：Planner-only、按 subject 单调 n，只落事件；**内核无任何读者据它放行**（唤醒谓词 `=> false`） | `mcp_server/tools/review.rs:22,51-110`；`crates/calm-truth/src/role_gate.rs:357`；`dispatcher/mod.rs:142-148` |
| F12 | 合并围栏 F4「最新 review.round converged 才 merge」只写在模板里（prompt-only） | `crates/calm-server/templates/builtin/issue-development.md:77-95,122-123` |
| F13 | lifecycle：内核自动推 `draft→planning→dispatching→working`、认领时 `reviewing→working`、报告/门禁后 `working→reviewing`；提示词明令不要为开任务而写 `working` | `prompts/planner.md:20` |

### 1.3 停止、失败、取消、恢复

| # | 事实 | 位置 |
|---|---|---|
| F14 | `calm.task.fail` 只对 Worker 可见且 `require_role(Worker)` | `mcp_server/tools/emit.rs:347-372` |
| F15 | `calm.plan.cancel` 对 `dispatched|running|verifying` 返回 `-32409 … out of scope (#644)`；只取消 `pending`（SQL `WHERE status='pending'`） | `mcp_server/tools/plan.rs:486-501`；`calm-truth/src/db/sqlite/task.rs:107-118` |
| F16 | #644 状态机本来就画了 `running → canceled` 边，只是没有入口 | `docs/architecture/644-plan-then-schedule.md:17-25` |
| F17 | liveness 超时：`task_fail_from_worker_tx`（CAS `status IN ('dispatched','running')`）+ 同事务写 `worker_sessions.handle_state_json.timeout_cleanup` 标记；sweep 按标记 `fail_running_worker_card`（中断 codex turn → 收割 PTY → 等 pid 退出 → session `failed`），失败则下轮重试 | `scheduler/mod.rs:77-106,1972-2060,2097-2140`；`operation/driver.rs:176-207`；`calm-truth/.../task.rs:564-580` |
| F18 | sweep 的 running 臂只看 `running_deadline_ms` | `scheduler/mod.rs:1897-1923` |
| F19 | worker 报完成的 CAS 也是 `status IN ('dispatched','running')`；交付行在同一报告事务里插入 ⇒ **状态一旦离开 running，旧 worker 再也交付不出候选** | `calm-truth/.../task.rs:308-330`；`decision_sink.rs:208` |
| F20 | codex worker 线程状态由 feeder 持久化到 `worker_sessions.{last_thread_status,last_activity_ms,last_turn_completed_ms}`（观测用，**无人消费它来判卡死**） | `liveness_feeder.rs:1-3,45-72` |
| F21 | 唤醒谓词：`TaskFailed`/`TaskCompleted` 非 Planner 作者即推；worker 的 `hook.*.stop` 推；gated 任务的 `task.failed` 只有当 `status_detail` 类属 `worker-reported|spawn-failed|worker-timeout` 才推（其余当「门禁后自报」吞掉） | `dispatcher/mod.rs:109-150,186-232`（列表在 `:228`） |
| F22 | 恢复（`calm.plan.recover`）= 同 key 新代次、**同契约**；普通 worker 永远 `predecessor_not_quiescent` | `task_recovery/admission.rs:420-460`；`task_recovery/refusal.rs:21,40` |
| F23 | `calm.task.repair` 只收 isolated 空工作区 producer；repair/review key 为随机 `repair-<uuid>`；内核把派生 task 块写进报告并用回执校验（伪造引用被拒） | `mcp_server/tools/task_repair.rs:11-24`；`file_delivery/repair.rs:117-129,160-190,228-262`；写入口 `track_report/write.rs:233` |
| F24 | Planner 可以对 codex worker 卡 `terminal.resolve/input`（只要是 Worker 卡、会话匹配、任务 running） | `terminal_interaction/target.rs:115-125,219`（#1784 提议拒绝） |

### 1.4 租约、上游、候选（#1771/#1777/#1778）

| # | 事实 | 位置 |
|---|---|---|
| F25 | 租约 base 在 worker `prepare_tx` 内决议并冻结：上游或 HEAD；分叉 ⇒ `refused: attached-repo-diverged`，经 `spawn-failed` 落到任务 | `operation/codex_adapter/mod.rs:795-797`；`operation/workspace_lease/base.rs:238-287`；`workspace_lease/upstream.rs:323,378-402`；`scheduler/mod.rs:1753` |
| F26 | `BaseSource` 已有 `Commit`/`Attempt{base_attempt_id}` 两臂（「slice 5」占位），表 CHECK 已接受 `base_source='attempt' AND base_attempt_id NOT NULL` | `workspace_lease/base.rs:42-62`；`crates/calm-truth/migrations/0111_workspace_lease_base.sql`、`0115_workspace_lease_upstream_base.sql` |
| F27 | `git worktree add -b <branch> <path> <commit-ish>` 钉到租约 base；三种注册态都以 `verify_worktree_base` 收尾 | `workspace_lease/mod.rs:1163-1238` |
| F28 | 候选 = 一次成功内核交付：`task_candidates` 行不可改（trigger），ref `refs/neige/candidates/<track>/<card>/<delivery_id>` 在 common dir | `crates/calm-truth/migrations/0113_task_git_deliveries.sql:68`；`git_candidate/refs.rs:16-18`；`git_candidate/candidate.rs:134` |
| F29 | 门禁核对钉住的候选（`target.kind=candidate`）；失配时唤醒文本和 `calm.plan.list.md` 都叫 Planner「declare a follow-up with `base:{attempt}`」——**该字段不存在，是死路** | `operation/task_verify_adapter/target.rs:631`；`prompts/tools/calm.plan.list.md:1` |
| F30 | `plan.list.candidate.upstream{sha,behind}` 已由内核计算（只读、建议性） | `prompts/tools/calm.plan.list.md:1` |

### 1.5 源码扫描门禁（决策必须服从）

| 门禁 | 规则 | 本设计哪里碰到 |
|---|---|---|
| `crates/calm-server/tests/cases/deferred_write_tx_invariant.rs` | 生产写事务一律 `begin_immediate_tx` | 片 1 cancel、片 2 回执事务 |
| `crates/calm-server/tests/cases/fork_guard_exemption_invariant.rs` | 报告写边界的结构性入口参数/返回类型被钉死 | 片 2 派生块必须走现有 `track_report::write` 入口（先例 `planner_repair`），不开新门 |
| `crates/calm-server/tests/cases/harness_turn_start_invariant.rs` | Planner harness 发 turn 只能经 `IssueTurnHandle` | 本设计不发 Planner turn，唤醒全走既有事件 |
| `crates/calm-server/tests/cases/boot_invariants.rs` | boot 恢复不变量 | 片 1 清理标记沿用既有 boot/sweep 语义 |
| `scripts/gate-sync-event-version-lockstep.sh` | `SYNC_EVENT_VERSION` 与迁移戳一致 | 不加事件种类 ⇒ 不 bump |
| `scripts/gate-prose-ratchet.sh` | Rust 里不许新增 ≥120 字节长字面量 / CJK 串 | 拒绝文案放 `prompts/tools/*.md`，Rust 里只放短 code |
| `scripts/gate-1316-terminology-ratchet.sh` | 术语棘轮 | 新词只用 replace/carry/superseded |
| goldens `mcp_tool_registry.json`、`runtime_status_matrix.json`、`issue_development_planner_prompt.txt` | 工具描述/状态矩阵/提示词逐字节 | 片 1/2 都要重生成 |

## 2. 4140 证据

### 2.1 全库规模

| 量 | 值 |
|---|---|
| tasks / 有任务的 track | 76 / 10；全部终态（done 57，failed 19）；**在途 0** |
| 有任务的非终态 track | 0（tracks：done 17、draft 4、planning 2） |
| key 含 `.` / 最长 key | **0** / 41 字符 |
| `task_attempt_allocations` | initial 75、recovery 1 |
| `task_candidate_repairs`（`calm.task.repair`） | **0 行** |
| `workspace_leases` base_source | NULL 38（legacy）、head 12、upstream 18、`attempt` **0** |
| `task_candidates` | 26 |
| `review.round` 事件 | 19（3 条 track） |

### 2.2 三条重活 track 的 key 形态（同一规律，三种拼法）

| track | key 序列（摘） |
|---|---|
| `32acbdf9`（#1726） | `implement-change` → `fix-review-findings-r1..r5` + `review-pr-a/b`、`review-r3-a/b`…`review-r6-a/b` |
| `58bc82ae`（#1774） | `design-*`、`design2-*` … `design5-*`、`implement-1774` |
| `affb2b97`（#1772） | `design-*`、`design-*-r2`、`implement-ownership-squash-audit`、`repair-ownership-squash-audit-r1..r5`、`review-impl-*-r1..r3`、`final-review-*` |

`key GLOB '*-r[0-9]*'`：26/76。规律固定为（根、关系、轮次），拼法每条 track 自创。

### 2.3 `affb2b97`（#1772）逐项核实

Planner 卡 `5b4cbbe5…`：27 个 turn、94 次 MCP 调用。

| issue 声称 | 实测 | 结论 |
|---|---|---|
| 18 个手写 key | 18 行 tasks、18 次 task 块 upsert（分布在 12 次 `report.commit`），**0 次删除 task 块**，task payload 合计 39,298 B | 成立 |
| 纯记账 44 次 | `report.commit` 20、`task.verdict` 16、`review.round` 6、`ratify.request` 2（1 次失败：`track is not in working or a ratify request is already pending`） | 成立（精确 44） |
| 每轮重新声明门禁 | 6 个 gated 任务的 `gate` JSON **完全相同（1 个 distinct 值）** | 成立 |
| 手抄 base/候选/发现 | 8 个评审块都带 `base_sha/candidate_ref/candidate_sha`；r3/r4/r5 带 `materialize_command: git diff <A> <B> \| git apply` | 成立 |
| verdict 逐个 | 16 次里 **12 次是对评审任务的 `accepted`**，3 次对 producer 的 `rejected`（均捎带 `lifecycle:"working"`），1 次最终 `accepted` | 12 次内核并不需要（F9） |
| 切 lifecycle | 31 次迁移中 **20 次是内核自动**；Planner 11 次：`reviewing→working` 5、`working⇄blocked` 4、`working→reviewing` 1、`reviewing→done` 1 | 5 次 `reviewing→working` 冗余（F13） |
| 基线漂移（r2） | r2 17:52:54 失败：worker 按 Planner 在 context 里写的 `base_sha` 拒绝新租约 HEAD `9aecedeb`（#1781 刚合入）；Planner 17:54:14 `ratify`，owner 裁决「在新 main 上 apply」 | 成立；根因是**基线由 Planner 手写在 context**，内核租约本身按 #1777 正确 |
| r4 卡死 | 派发 18:39:03；Planner 18:40–18:41 用 `terminal.input` 3 次改范围；`worker_sessions.last_thread_status='idle'`、`last_activity=18:41:24`、`last_turn_completed_ms=NULL`；**18:41:24 → 20:40:12 无任何事件唤醒 Planner（118 min）**；19:30:38 用户消息唤醒后，Planner 调用 #43–#77（35 次：resolve/observe/input ×18/open ×3「Stop stale repair r4」/`task.fail` 被拒/`plan.cancel` 被拒） | 成立；**内核早在 18:41:24 就持有「turn 已结束」的事实，没人读** |
| 候选迁移可行性 | r3：`8ff1e135..c8678bfd` 搬到 `9aecedeb` 无冲突；r4/r5 搬 `9aecedeb..771a59c2` 到同一 base（平凡） | 观察到 3 次迁移、**0 次冲突** |

全库 failed 且 `worker-timeout` 的 3 个 kernel-lease worker：r4 `idle` 118 min；`32acbdf9` review-r6-a/b `active` 但 112 min 无活动（挂死，另一种形态，见 G4）。
codex worker 会话的 `userMessage` 条数：46 个任务为 2（38 done、8 failed），唯一为 4 的就是 r4（Planner 手打的输入）——**4140 上从未有 codex worker 在「turn 结束后再被续一轮」之后完成任务**，所以「idle + running = 卡死」在现有数据上无反例。

## 3. 片 1：在途可停 + 空转必醒

### 3.1 `calm.plan.cancel` 接受在途执行

- `dispatched|running`：同一 `begin_immediate_tx` 内 CAS `status IN ('dispatched','running') → 'canceled'`，`status_detail='planner-canceled'`，
  并写 F17 的清理标记（`reason` 由 `running_liveness_timeout` 泛化为参数）；提交后 poke `sweep_timeout_worker_cleanups`。事件沿用现有 `plan.updated`（Planner 作者，不自唤醒）。
- `verifying`：仍拒绝，但文案换成「门禁运行中；等 `task.gate_result`」，删掉 `out of scope (#644)`（G1）。
- 旧 worker 迟到的 `calm.task.complete`/`fail` 被 F19 的 CAS 拒绝 ⇒ 不产生交付、不产生候选。

### 3.2 codex 空转检测（#1782 缺陷 2）

sweep 的 `TaskStatus::Running` 臂（F18）加一条：任务 kind=codex、有 worker 会话、`last_thread_status='idle'`、
`now - last_activity_ms ≥ worker_idle_grace`（typed 配置，默认 300 s，与 `task_run_timeout_ms` 同处）、且该会话在本执行内至少开始过一个 turn（U1）
⇒ 走 `fail_task_liveness_timeout`，`status_detail='worker-turn-ended'`。事件是现有 `task.failed`（`KernelDispatcher` 作者 ⇒ 推送）。

**必须**把 `worker-turn-ended` 加进 `dispatcher/mod.rs:228` 的 pre-gate 列表：r4 是 gated 任务，漏加则这次失败会被当成「门禁后自报」吞掉，唤醒丢失——这正是本片要消灭的症状。

Claude worker 不在本臂内：它的 `hook.claude.stop` 已经唤醒 Planner（F21），Planner 之后用 3.1 收场（G3）。

### 3.3 提示词（随片 1）

- `prompts/tools/calm.plan.cancel.md` + `planner.md` 取消一句：在途可取消、会收割 worker、`verifying` 不行。
- `planner.md:74` 后加一句：评审/审计任务不需要 `calm.task.verdict`；只对要接受/拒绝的 producer 下 verdict（F9 已写，强调给 verdict 那条）。
- `planner.md:20` 已禁止为开任务写 `working`；加一例「拒绝候选后不要写 `lifecycle:"working"`」。

### 3.4 4140 反事实

r4 会在 ≈18:46（idle 18:41:24 + 300 s）以 `worker-turn-ended` 失败并唤醒 Planner，而非 20:40:12；#43–#77 这 35 次调用和 3 张「Stop stale repair」终端卡都不会发生。

## 4. 片 2：`calm.task.replace`

### 4.1 形状

```
calm.task.replace {
  key, expected_attempt_id, idempotency_key, reason,        // 必填
  goal?, acceptance?,                                       // 缺省继承
  context?,                                                 // 浅合并进继承的 context（findings 等 Planner 判断）
  carry?: "candidate" | "none"                              // 缺省 "candidate"（有来源时）
}
→ { replayed, receipt_id,
    predecessor: {key, attempt_id, prior_status, stop: "canceled_now" | "already_terminal"},
    successor:   {key, attempt_id},                         // attempt_id = "{track}:{key}"（F4），提交时即确定
    carry:       {source_attempt_id, source_candidate_id, from_sha, to_sha} | {none: <reason>} }
```

不继承、不可覆盖：`gate`（原样继承；要换门禁就声明新任务——4140 上 6/6 相同）、`kind`、`depends_on`、`priority`、`spawn`。
Planner-only；只收 attached、kind ∈ {codex, claude}、`delivery_policy='kernel'` 的任务。isolated 用 `calm.task.repair`（F23，4140 0 行，不合并，G11）。

### 4.2 为什么是新 key 而不是同 key 新代次

同 key 新代次要 (a) 重建 append-only 的 `task_attempt_allocations`（CHECK 只允许 `initial|recovery`、trigger 要求前驱 `failed`，F5）；
(b) 让同一个 key 在不同代次背不同契约，与 `calm.plan.recover`「同契约」语义冲突（F22）——一个值两个意思。
新 key = 一个 key 一份契约；执行 id 仍由 F4 推出，`track:key` 的所有既有消费者（verdict、delivery、runs 视图）零改动。

### 4.3 确定性 key

后继 key = `<root>.<n>`：`root` = 该谱系第一个 key，`n` 从 2 递增（谱系深度）。例：`implement-x` → `implement-x.2` → `implement-x.3`。
4140：0/76 个 key 含 `.`，最长 41 ⇒ 不会与现有 key 撞，`root.99` 仍 ≤ 64。派生出的 key 已存在（Planner 手写了同名）⇒ 拒绝 `derived_key_taken`，不自动跳号。
幂等：`(track, idempotency_key)` 唯一 + `predecessor_attempt_id` 唯一（一个执行至多一个后继），同键同指纹重放回执、不同指纹冲突——与 recovery 的两条唯一索引同构（`0097:33-37`）。

### 4.4 事务内（一次 `begin_immediate_tx`）

1. 重放检查先于一切状态读。
2. `task_attempt_current_tx(track,key) == expected_attempt_id`，否则 `stale_attempt`。
3. 前驱在途 ⇒ 片 1 的 CAS，`status_detail='superseded: <succ key>'` + 清理标记。
4. 解析 carry 来源：前驱自己的已结算候选（F28）；没有则沿用前驱自己的 carry 来源（r4 这类「在修复途中被转向」）；都没有 ⇒ `none`。
5. 经 `track_report::write` 现有入口（`planner_repair` 先例，`track_report/write.rs:233`）**原地**改写前驱的 task 块：同 block id、新 key、继承字段 + 覆盖字段、
   `context.neige_execution.replace = {receipt_id}`；Planner 伪造/编辑该引用 ⇒ 按 `repair.rs:228-262` 的「块 ↔ 回执契约」校验拒绝。
6. 插回执 `task_replacements`（新迁移 0116；列：`receipt_id, track_id, predecessor_attempt_id UNIQUE, predecessor_key, successor_key, request_idempotency_key, request_fingerprint, reason, source_attempt_id NULL, source_candidate_id NULL, from_sha NULL, to_sha NULL, predecessor_payload_json, created_at_ms`；`UNIQUE(track_id, request_idempotency_key)`；不可改 trigger）。
7. 事件：`track.report_edited`（author planner）+ `plan.updated`——均为现有种类、均不自唤醒。

### 4.5 carry：在新租约的 prepare 里做，不在 worker 里做

`resolve_lease_base`（F25）对带 replace 回执且有 carry 来源的任务分支：

1. 照旧决议上游 `U`（F25；分叉仍 `attached-repo-diverged`）。
2. `git merge-tree --write-tree --merge-base=<from> <U> <to>`：退出 0 ⇒ 树 `T`；退出 1 ⇒ 冲突文件列表；其它 ⇒ 基础设施错。
3. `git commit-tree T -p U`（作者/提交时间取回执 `created_at_ms`，信息 `neige carry <receipt_id>`）⇒ `C'`。**总是**新建 `C'`（即使 `U == from`），保证不变量 `C'^1 == U`。
4. 租约行 `base_sha=C'`、`base_source='attempt'`、`base_attempt_id=source_attempt_id`（F26，表形状已在，无迁移）；随后 F27 原样 `worktree add … C'`。

两条 `Command` 各自判 `ExitStatus`（owner 的 S4 裁决：不走 shell 管道）；整段放在 S4 G26 同款 4 s 上限内（U3）。
纯对象库操作：不碰任何工作树、不建 ref，崩溃重试时租约行已冻结 `C'` 即幂等。
评审看「完整改动」的范围 = `C'^1..candidate`；看「本轮修复」= `C'..candidate`（在 `plan.list.candidate` 上加 `carry{receipt_id, upstream_sha, carry_sha}` 只读投影，从租约行 + `C'^1` 得出）。

冲突 ⇒ prepare 返回 `refused: carry-conflict: <paths>`，经现有 `spawn-failed` 通路（`scheduler/mod.rs:1753`）落成 `task.failed` ⇒ 推送（F21，`spawn-failed` 已在 pre-gate 列表）。
Planner 可以再 replace 一次 `carry:"none"` 并把冲突说明写进 context，或自己声明解冲突任务（Q2）。

同片清扫死路提示：`task_verify_adapter/target.rs:631` 与 `prompts/tools/calm.plan.list.md` 的 `base:{attempt}` 改为 `calm.task.replace`（F29）。

### 4.6 oracle trace

| seq | phase | actor | trigger | external effect | observable event | invariant | status |
|---|---|---|---|---|---|---|---|
| 1 | request | Planner | `calm.task.replace(...)` | — | — | Planner-only（`require_role`，同 `task_repair.rs:18`） | NEW |
| 2 | admit | kernel tx | 同上 | — | — | 重放先于状态读；`expected_attempt_id` = 当前执行 | NEW（形状同 `0097:33-37`） |
| 3 | stop | kernel tx | 前驱 `dispatched/running` | — | `plan.updated` `event.rs:581` | CAS 后旧 worker 的 complete/fail 0 行（`task.rs:308-330,564-580`）⇒ 无交付 | 片 1 |
| 4 | stop | kernel tx | 同上 | 清理标记 | — | 标记与 CAS 同事务（`scheduler/mod.rs:77-106`） | 复用 |
| 5 | pin | kernel tx | 前驱有候选 | 无（ref 已在） | — | 候选行不可改（`0113:68`）；来源 = `candidate_for_attempt_tx`（`candidate.rs:134`） | 复用 |
| 6 | declare | kernel tx | — | 报告块原地改写 | `track.report_edited` `event.rs:409`（author planner，不自唤醒 `dispatcher/mod.rs:134`）+ `plan.updated` | 块 ↔ 回执契约一致 | NEW（先例 `write.rs:233`） |
| 7 | receipt | kernel tx | — | `task_replacements` 行 | — | 每前驱 ≤1 后继；每请求键 ≤1 回执 | NEW 表 |
| 8 | return | kernel | 提交成功 | — | 工具结果 | `successor.attempt_id = "{track}:{key}"`（`task_projection.rs:1624`） | NEW |
| 9 | reap | kernel sweep | 清理标记 | 中断 turn、收割 PTY、释放租约 | `workspace.released`、`terminal.deleted` | 失败下轮重试（`driver.rs:176-207`） | 复用 |
| 10 | claim | scheduler | 后继 ready | — | `task.dispatched` `event.rs:592`（`scheduler/mod.rs:1281`） | 前驱已终态，不占预算 | 复用 |
| 11 | carry | kernel prepare tx | 回执有来源 | 对象库写 `T`、`C'`（无 ref） | — | `C'^1 == U`；≤4 s；`base_source='attempt'` | NEW（`base.rs:238`） |
| 12a | carry 冲突 | kernel | merge-tree 退出 1 | 无工作树 | `task.failed` `event.rs:533`，`spawn-failed: refused: carry-conflict: …` | kernel 作者 ⇒ 推送；pre-gate 列表含 `spawn-failed` | NEW detail |
| 12b | lease | kernel | 无冲突 | `worktree add … C'` | `workspace.leased` `event.rs:635`、`worktree.provisioned` | `verify_worktree_base`（`mod.rs:1163-1238`） | 复用 |
| 13a | 完成 | worker→kernel | `calm.task.complete` | 交付 commit + ref | `task.git_delivery_settled` `event.rs:561`；gated 再 `task.gate_result` `event.rs:772` | 恰好一次推送 | 复用 |
| 13b | 空转 | kernel sweep | idle ≥ 宽限 | 收割 | `task.failed` `worker-turn-ended` | 在 pre-gate 列表 | 片 1 |
| 13c | 超时 | kernel sweep | deadline | 收割 | `task.failed` `worker-timeout` | — | 复用（`scheduler/mod.rs:1972`） |
| 14 | replay | Planner | 同请求重试 | — | — | 同回执、`replayed:true`、无事件 | NEW |

**唤醒保证**（可枚举测试）：每个后继执行恰好以 {13a 非 `deferred_to_gate` 的结算、13a 门禁结果、12a/13b/13c 的 kernel `task.failed`、worker 自报 `task.failed`} 之一结束并推送 Planner；
唯一不推送的出口是 Planner 自己的 cancel/replace。这是对既有事件的覆盖性断言，不是新机制。

### 4.7 producer × state 矩阵（`calm.task.replace` 结果）

| 前驱当前执行状态 | stop | carry 来源 | 结果 |
|---|---|---|---|
| `pending` | CAS→`canceled`（`superseded`） | 前驱自身 carry 来源，否则 none | 后继 |
| `dispatched` / `running` | CAS→`canceled` + 清理标记 | 同上 | 后继 |
| `verifying` | — | — | 拒绝 `predecessor_verifying`（G1） |
| `done`，delivery `committed` / `no_change` | 无 | 该候选 | 后继 |
| `done`，delivery `pending` | — | — | 拒绝 `candidate_pending`（等 `task.git_delivery_settled`） |
| `done`，delivery `failed`/`abandoned`，或 `failed`/`canceled` | 无 | 前驱自身 carry 来源，否则 none | 后继（`carry.none` 带原因） |
| 租约 legacy（`candidate.binding="unbound"`） | 按上各行 | none | 后继（`carry.none="legacy_lease"`） |
| `expected_attempt_id` ≠ 当前 | — | — | 拒绝 `stale_attempt` |
| 已有后继（其它请求键） | — | — | 拒绝 `already_replaced{successor_key}` |
| 同请求键同指纹 / 不同指纹 | — | — | 重放回执 / 拒绝 `idempotency_conflict` |
| 有非终态依赖者 | — | — | 拒绝 `pending_dependents{keys}`（G6） |
| isolated / terminal / child-track / 非 kernel delivery | — | — | 拒绝 `unsupported_route` |
| 派生 key 已存在或 > 64 | — | — | 拒绝 `derived_key_taken` / `derived_key_too_long` |
| track 终态 | — | — | 拒绝 `track_terminal` |

carry 结果（租约 prepare）：clean ⇒ `base_source='attempt'`；merge-tree 1 ⇒ `spawn-failed: refused: carry-conflict`；来源对象缺失 ⇒ `spawn-failed: refused: carry-source-missing`；git 错误/超时 ⇒ `spawn-failed: carry-infra`。

新 `status_detail` 值：`planner-canceled`、`superseded`（canceled 行）、`worker-turn-ended`（failed 行）；`runtime_status_matrix.json` golden 同步。

## 5. issue「默认化」表逐行取舍

| issue 行 | 取舍 | 一行理由 |
|---|---|---|
| 内核生成确定性 key | **做**（片 2，仅 replace 后继） | 18 个 key 中 5 个是修复改名；根 key 仍是 Planner 的命名判断 |
| 修复任务继承门禁 | **做**（片 2） | 6/6 相同 |
| 内核注入 base/候选/lineage | **做**（片 2 回执 + `plan.list.candidate.carry`）；findings 仍由 Planner 写 | 哪条发现算阻塞是判断，不是记账 |
| 内核迁移 delta、冲突 fail closed | **做**（片 2，merge-tree） | r2 失败 + owner 介入的直接根因 |
| 按轨道评审策略自动派发评审 | **砍策略**；「沿用上一轮评审集合」放片 3（有条件） | 58bc82ae 的浪费来自评审预算判断（5 轮设计评审），策略对象替不了这个判断 |
| `review.round` 自动汇总 | **砍** | git 评审者结果是自由文本，无结构化 verdict 可汇总（需 S5 findings 模型）；6 次/track；它唯一的用途 F4 围栏本就是 prompt-only（F12） |
| 批准的 verdict 自动接受 | **砍机制，改提示词** | 内核从不要求（F9）；12/16 是可省的评审接受 |
| lifecycle 从任务状态推导 | **砍** | 已经推导（20/31 自动）；剩下的 `blocked`/`done` 是判断，5 次 `reviewing→working` 是提示词问题 |
| 任务不写在报告正文 | **收窄**：replace 原地改写块，一条谱系一个块 | 不新增块种类/摘要层（Q4） |

## 6. 四个待定问题的建议答案

**Q1 评审策略粒度（轨道 / 任务 / 按体量）** → **按任务，由 Planner 在首轮声明，replace 时沿用或显式缩减**；不做轨道级策略对象，不做内核按体量分级。
证据：58bc82ae 107 min / 10 worker / 0 行代码的浪费来自 Planner 首版设计过重 + 单通道否决（#1727 comment 2026-09-22），affb2b97 的收敛来自 owner 缩范围——都是判断；体量分级写进模板/评审合同（#1727 第二轮建议 4），内核只保证「重派评审不需要手抄事实」（片 3）。

**Q2 迁移冲突：fail 等人，还是派解冲突 worker** → **fail closed 并唤醒 Planner**（`spawn-failed: refused: carry-conflict: <paths>`）。
证据：4140 上 3 次迁移 0 次冲突（§2.3）；r2 里 Planner 自己的策略就是「冲突才停、不得自行解冲突」（call #34/#41/#78 的 context `apply_policy`）。Planner 可自行声明解冲突任务；内核不自动派（无观察到的需求）。

**Q3 确定性 key 格式与 idempotency 兼容** → `<root>.<n>`；任务 `idempotency_key` 仍是 `"{track}:{key}"`（F4），请求幂等用独立的 `(track, idempotency_key)` 唯一 + 每前驱唯一后继。
兼容证据：4140 76 个 key 里 0 个含 `.`、最长 41；无在途任务；不动 `task_attempt_allocations`（F5）。

**Q4 报告里的任务摘要与折叠** → 不加摘要层。replace 在**同一个块**上改写 key/契约，报告里一条谱系一个块；历史在回执表（含 `predecessor_payload_json`）、`task.context_frozen` 事件和 `runs/` 视图。
证据：affb2b97 18 个块 39 KB 从未删除；Planner 原话「二十多个历史 task block 对恢复当前重点帮助不大」。前提 U2（改写终态执行的声明对 plan.list/runs/恢复无副作用）；U2 不成立则退回「追加块」，记 G9。

## 7. 切片（按顺序，各自可合）

| 片 | 内容 | 估算 | 验收（必须绿） | 必须红（变异/反转） |
|---|---|---|---|---|
| **1** 在途可停 + 空转必醒 | `plan.cancel` 收 `dispatched|running`（新 CAS + 泛化清理标记 + poke sweep）；sweep 空转臂 + typed 宽限配置；`worker-turn-ended` 进 `dispatcher/mod.rs:228`；3.3 三句提示词；goldens | ~450 | running codex 任务 cancel ⇒ `canceled/planner-canceled`、session `failed`、租约 `released`、迟到 `task.complete` 被拒且无交付行；idle ≥ 宽限 ⇒ `failed/worker-turn-ended` 且 Planner 收到推送（gated 与非 gated 各一）；`active`/宽限内/Claude/`verifying` 不动；`verifying` cancel 拒绝文案无 `#644` | `tests/cases/mcp_plan.rs::cancel_in_flight_task_refused_with_409_text` 反转为 `cancel_running_task_cancels_and_reaps_worker`；变异①删空转臂 ⇒ 仅空转测试红；②从 `:228` 列表去掉 `worker-turn-ended` ⇒ 仅 gated 推送测试红；③CAS 退回 `status='pending'` ⇒ 仅 cancel-running 测试红 |
| **2** `calm.task.replace` + carry | 迁移 0116 `task_replacements`；工具 + 描述 + 注册 golden；派生/原地改写/回执校验；`resolve_lease_base` carry 分支（merge-tree/commit-tree、`attempt` 列）；`plan.list.candidate.carry`；`target.rs:631` 与 `calm.plan.list.md` 死路改名；`planner.md` 修复轮一段（replace 代替拒绝 verdict + 新块） | ~1000（超了就拆 A=表+carry 经测试 seam，B=工具+提示词） | §4.7 每行一个测试；上游前进时后继租约 `C'^1==U` 且树 = 3-way 结果；`U==from` 时仍新建 `C'`；冲突 ⇒ `spawn-failed: refused: carry-conflict` 且推送；重放不发事件；伪造 `neige_execution.replace` 被拒；门禁 JSON 与前驱逐字节相等；§4.6 唤醒保证枚举测试 | 变异①carry 分支改用 `U` 作 base（丢 delta）⇒ carry 树测试红；②去掉 `predecessor_attempt_id` 唯一 ⇒ `already_replaced` 测试红；③回执校验短路 ⇒ 伪造引用测试红 |
| **3**（有条件）评审沿用 | replace 同时为前驱的终态直接依赖者（上一轮评审）派生 `<root>.<n>` 后继，依赖新 producer；认领时把依赖的候选 `{ref, commit_sha, carry range}` 注入评审 worker 上下文；参数 `reviewers: []` 可缩减 | ~600 | 一轮修复 = Planner 一次调用；评审上下文里的 sha 等于依赖的已结算候选 | 变异：注入取前驱而非新 producer 的候选 ⇒ 红 |

门槛：片 3 只在片 2 部署到 4140 并跑完一条真实开发轨后，按访谈（每轮修复 Planner 的调用数与手抄事实）决定做或砍。
前置/伴随：#1784（拒绝对 codex worker 卡 `terminal.input`）最好先于或随片 1 合入——它堵住触发 #1782 的入口，片 1 兜住其余空转。

每片流程照 `AGENTS.md`：subagent 实现 → 双通道代码评审 → `scripts/local-rust-gates.sh --quick` + 目标 nextest + goldens。

### 7.1 affb2b97 反事实（估算，基于 §2.3 计数）

| 量 | 实际 | 片 1+2（+提示词） | +片 3 |
|---|---|---|---|
| 手写 key | 18 | 13（修复 5 个变成派生，r2 不再发生） | 7 |
| 纯记账调用 | 44 | ≈26：commit 14、verdict 1、round 6、ratify 1、replace 4 | ≈23 |
| 手抄 sha / apply 散文 | 13 个块 | 8 个评审块 | 0 |
| r4 静默卡死 | 118 min + 35 次调用 | ≤ 宽限 300 s | — |
| 基线失败 + owner 裁决 | 1 + 1 | 0 | — |

调用数下降有限；主要收益是**没有手抄事实、没有两类失败（r2 基线、r4 卡死）**。

## 8. 与 issue 原文相左的发现

1. **「对评审逐个 `task.verdict`」不是内核要求**：`planner.md:73` 明写纯顺序依赖不需要 verdict；16 次里 12 次可省。解法是提示词，不是「自动接受」机制。
2. **「切 lifecycle」已由内核推导**：31 次里 20 次自动；Planner 的 5 次 `reviewing→working` 本就被提示词禁止。
3. **stop proof 不是启动后继的安全前提**：F19 的 CAS 让被取消执行永远交付不出候选；r4 与 r5 在 4140 上并行跑了 51 min 无害。停旧只为资源/预算，可异步（沿用 F17 sweep）。
4. **「返回新 turn id」做不到也不需要**：后继的 worker turn 在工具返回时尚不存在；能同步返回的是前驱/后继执行 id、carry 来源 sha；上游 sha 在认领时才决议，走 `plan.list`。
5. **r4 卡死的事实内核早就有**：`last_thread_status='idle'` 在 18:41:24 已落库，缺的只是一个读者——这比 steer 原语便宜得多，放片 1。
6. **r2 基线失败的根因是 Planner 把 base 手写进 context**（worker 按 `blocking_policy` 拒绝了内核按 #1777 正确发放的新租约），不是内核发错租约；carry 让 base 由内核拥有即消除。
7. **原片 5 的 `base:{attempt}` 已有死路提示在生产上**（F29），片 2 顺手修掉。
8. 访谈里「`calm.task.repair` 不支持 attached」属实，但 repair 在 4140 上 0 次使用；不扩展 repair、不合并，replace 另立（G11）。

## 9. 复现命令（4140，只读）

```bash
DB=~/.local/share/neige-next/data/calm.db; T=affb2b97103d45c78bf50b81b7b058bf; P=5b4cbbe5d6b64b95adc400c70e60220c
sqlite3 -readonly $DB "select count(*), count(distinct track_id), sum(key glob '*-r[0-9]*'), sum(key like '%.%'), max(length(key)) from tasks"
sqlite3 -readonly $DB "select status, count(*) from tasks group by 1"
sqlite3 -readonly $DB "select base_source, count(*) from workspace_leases group by 1"
sqlite3 -readonly $DB "select count(*) from task_candidate_repairs"
sqlite3 -readonly $DB "select json_extract(params,'$.item.tool'), json_extract(params,'$.item.status'), count(*) from harness_items
  where card_id='$P' and item_type='mcpToolCall' and method='item/completed' group by 1,2 order by 3 desc"
sqlite3 -readonly $DB "select case when json_extract(payload,'$.agent_message') like '[auto]%' then 'auto' else 'planner' end,
  json_extract(payload,'$.from')||'->'||json_extract(payload,'$.to'), count(*) from events
  where kind='track.lifecycle_changed' and scope_track='$T' group by 1,2"
sqlite3 -readonly $DB "select last_thread_status, datetime(last_activity_ms/1000,'unixepoch','localtime'), last_turn_completed_ms
  from worker_sessions where card_id='1b83e377ae054f9289ed0ea19158ab6e'"
sqlite3 -readonly $DB "select t.key, t.status_detail, ws.last_thread_status, (t.finished_at_ms-ws.last_activity_ms)/60000
  from tasks t join worker_sessions ws on ws.card_id=t.worker_card_id where t.status_detail='worker-timeout'"
sqlite3 -readonly $DB "select ws.provider, u.n, t.status, count(*) from (select worker_session_id s, count(*) n from worker_flow_items
  where kind='userMessage' group by 1) u join worker_sessions ws on ws.id=u.s join tasks t on t.worker_card_id=ws.card_id group by 1,2,3"
```

task 块、门禁去重、context 键与 verdict 分类由对上面 `harness_items.params.item.arguments`（`calm.report.commit` 的 `ops[].kind=='task'`、`calm.task.verdict`）的 JSON 解析得出（门禁：`json.dumps(payload.gate, sort_keys=True)` 去重得 1 个值 × 6）。

## 10. 风险与 KNOWN GAPS

- **G1** `verifying`（门禁在跑）既不能 cancel 也不能 replace；等 `task.gate_result`。
- **G2** 取消后旧 worker 最多再活一个 sweep 周期，而预算槽已释放 ⇒ 瞬时多一个进程。
- **G3** 空转检测只覆盖 codex worker；Claude worker 靠既有 stop hook 唤醒，任务仍 `running`，由 Planner cancel/replace。
- **G4** 「`active` 但无活动」的挂死（32acbdf9 r6-a/b，112 min）不检测，仍由 2 h deadline 兜底。
- **G5** 被取消 worker 迟到的 `hook.codex.stop` 仍会唤醒 Planner 一次（`dispatcher/mod.rs:144-148` 不查任务行）。
- **G6** 前驱有非终态依赖者时 replace 拒绝，不自动改指向（片 3 只处理终态评审者）。
- **G7** carry 是 3-way 语义（merge-tree），比 `git apply` 宽容；二进制/改名冲突一律按冲突拒绝。
- **G8** `C'` 在 `worktree add` 前无 ref 引用，依赖 gc 默认 2 周宽限；实际窗口是毫秒级。
- **G9** 原地改写后前驱契约只存在回执快照与 `task.context_frozen`；U2 不成立则退回追加块，报告继续增长。
- **G10** `review.round` 仍 Planner 手写，F4 合并围栏仍 prompt-only。
- **G11** `calm.task.repair`（isolated）与 `calm.task.replace`（attached）并存。
- **G12** 宽限 300 s：人工在 idle 超过宽限后才往 codex worker 卡打字会被收割（#1784 已禁止 Planner 这么做）。
- **G13** `ratify` pending 与 lifecycle 不同步（第二轮访谈 P1）不在本设计内。

### 实现前 spike

- **U1** feeder 在「线程创建后、首个 turn 前」是否也写 `idle`（r4 显示中断时写 `idle` 且 `last_turn_completed_ms` 仍 NULL）；据此定「本执行已开始过 turn」的判据（片 1 前置）。
- **U2** 改写终态执行的声明块对 `plan.list`、`runs/`、恢复视图无副作用（片 2 前置，决定 Q4 走原地还是追加）。
- **U3** 本仓 `merge-tree --write-tree` + `commit-tree` 在 prepare 事务内的耗时 < 4 s（片 2 前置）。
- **U4** `merge-tree --write-tree` 需 git ≥ 2.38：本机 2.39.5、CI ≥ 2.43（S4 U1 已核）。
