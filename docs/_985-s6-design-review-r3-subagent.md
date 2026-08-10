# #985 切片 6 设计增量 v3 · 对抗性评审 r3（subagent 通道 / 收敛检查）

> 对象：`docs/_985-s6-design.md`（v3，PR-A）。基线 `/tmp/wt6b` @ origin/main `9d30006a`。
> 只读；未跑 cargo。**跑了 sqlite3 复核 §2.1 的三条构造并新增两条**（下方标「实测」）。
> 结论：**NO** —— 但最小阻塞集只剩 **1 条**（BLOCKER-1）。1 BLOCKER / 6 MAJOR / 4 MINOR。
> v3 确实清空了 r2 两通道的最小阻塞集；本轮的 BLOCKER 是 **v3 新增的 quiescence 修法自己开出来的**。

---

## BLOCKER

### [BLOCKER-1] §5.2 的 quiescence「no-op + 等下一轮 sweep」在可达状态下**永远等不到**：子 wave `Done` + 残留 `pending` 任务

**结论**：设计 `:522` 写「不满足 ⇒ no-op + 诊断，等下一轮 sweep —— **子 wave 的任务终结后会再次触发**」。
这句话的前提对 `pending` 一格**为假**：`Done` 是终态 lifecycle，**新 claim 被 lifecycle gate 挡住**，
残留 pending 行**永远不会被派发、也永远不会自行终结**。

**攻击**：子 wave 声明 3 个 task（或 2 个 + `depends_on`），并发上限使第 3 条留在 `pending`；
spec 合法写 `Reviewing → Done`（`crates/calm-types/src/wave_lifecycle.rs:279-286`）。
此后每一轮 sweep：父任务被枚举 → quiescence 断言看到 `pending` → no-op → 下一轮同样 no-op。
父任务永久 `running` 且 `running_deadline_ms IS NULL`（§4 裁决 1 亲手删掉了唯一的 liveness 收割器），
PR-B 的树预算被永久占用，父 wave 因 §5.4 的 descendant 守卫**不可删**。
唯一出路是人工删掉子 wave —— 而设计把这条恢复路径只登记给了「父 wave 被 cancel」（`:586-590`），
Done+pending 这一格连诊断文案都没有。

**证据**：
- `crates/calm-server/src/scheduler/mod.rs:147-155` —— `lifecycle_allows_scheduling` 只含
  `Planning|Dispatching|Working|Reviewing`；`Done` 持有新 claim；
- 同文件 `:1262-1264` —— sweep 的 `Pending` 臂只把 wave 塞进 `pending_waves` 交给 `poke`，
  `:572-576` 再被 lifecycle gate 拒掉 ⇒ 该行永不推进；
- `crates/calm-truth/src/db/sqlite/task.rs:161-171` —— `pending → canceled` 只有**显式**调用，
  全仓没有任何「wave 进终态时清理残留 pending」的写点；
- 设计 `:520-523`（quiescence 集合含 `'pending'`）、`:451-452`（deadline NULL）、`:573-575`（拒删）。

**为什么 v3 验收抓不到**：§7 #13b（`:637`）只造「仍有 `verifying` 任务」并只断言**父任务 no-op**。
`verifying` 的 gate 确实会自行终结 ⇒ 那条交错最终收敛；**pending 那一格没有任何用例**，
而且 28 条里**没有一条断言「子 wave 静止后父任务必须闭合」**——整张表只买了「不许早闭合」，
没买「必须最终闭合」。这正是 D.4 「活性不变量」被写成单向安全性断言的老形状。

**最小修法**（二选一，都很小）：
- (a) quiescence 集合改为 `{dispatched, running, verifying}`，**`pending` 不计**。安全性不受损：
  §5.2 已禁止 reopen 被引用的 wave ⇒ `Done` 子 wave 的 pending 行无法复活，
  而 r2-A 那条并发 gate 攻击走的是 `verifying`，仍被挡住；
- (b) 保留 `pending`，但在 `→ Done` 边沿把该 wave 残留 pending 一律 `canceled`（复用 `task_cancel_tx`）。
- 并**新增一条活性验收**：「子 wave `Done` + 一条永不可调度的 `pending` ⇒ 父任务必须在 ≤N 轮 sweep 内闭合」，
  变异 = 把 `pending` 加回 quiescence 集合 ⇒ 必红。

---

## MAJOR

### [MAJOR-1] §7 #11 要求的「三站点各单点变异、每处都要有会红的用例」**不可满足** —— v3 修 r2-B MAJOR-3 时制造的新洞
`:1290` 的复核与 `:1319` 的回填函数**都嵌套在 `:1271` 那条匹配臂内部**
（`scheduler/mod.rs:1271-1296` 调用 `stamp_missing_running_liveness_deadline`，其体在 `:1318-1347`）。
一旦 §4 裁决 3 把 `spawn='sub-wave'` 臂**排在 terminal 臂与 kind 超时臂之前**（设计 `:460-462`），
sub-wave 行**根本到不了 `:1271`**，`:1290`/`:1319` 的守卫是死代码 ⇒ 单独删掉任一处，**全绿**。
设计 `:634` 却写「三个站点各删一处，每一处都要有一条会红的用例」。
**最小修法**：把变异矩阵写成二维 —— 基线变异是「把 sub-wave 臂挪到 terminal/liveness 臂之后」，
在该基线下再对三个站点各做单点变异；并明写三站点是纵深防御、臂序是主防御。

### [MAJOR-2] §7 #10 的 oracle「驱动真实 adapter + 断言零副作用」对任意前置失败都绿
四个 adapter 里 `refuse_if_context_stale` 之后紧跟的都是**会对同一 stale fixture 失败的其它前置**：
`codex_adapter/mod.rs:773` → `:779` `prepare_workspace_lease_target_tx`（需要 workspace lease）；
`terminal_adapter.rs:583` → `:588` 终端 env/card 构造；
`task_verify_adapter.rs:634` → `:637-651` idem key 必须是 `{task_id}#g{N}`、`:653-660` 任务必须 `Verifying`。
一个「造一条 stale task 就遍历名单驱动」的表驱动 fixture，删掉那一行后依然 `Err` + 零副作用 ⇒ **恒绿**，
而且实现方会顺理成章地把它降级成「每个 kind 都返回 Err 即可」——比 v2 的单 kind 更糟（假装买了 4 个）。
**最小修法**：改成**差分**元测试：每个 kind 用**同一份完整 fixture** 跑两遍，
`context_stale_at_ms` 为 NULL 的那遍**必须成功并产生可断言的副作用**，
置位的那遍必须以 `context-stale:` 文案失败。删掉那一行 ⇒ 两遍都成功 ⇒ 红。

### [MAJOR-3] §3.3 写死的稳定 idem key 让 harness bootstrap **永不可自动重试**，与 dead-root reaper 的前提冲突
`driver.rs:116-121`：`(kind, idempotency_key)` 命中已存在的行且 hash 相同 ⇒ **直接返回该 op id**，
不看它的 phase。所以 `child-wave:<child_id>:bootstrap` 一旦 `failed`，
「每次 drive 都跑的幂等步骤」每次都拿回同一条 failed op、`wait` 立即返回 Failed，**永远起不来**。
而 dead-root reaper 的注释明说恢复语义依赖「start/reset 用**FRESH op id** 重新提交」
（`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:176-186`）—— v3 的 exactly-once 修法正好拆掉这个前提。
**最小修法**：定型失败臂 —— bootstrap op 终态 `Failed/Stuck` ⇒ 父任务 fail-closed
`failed('child-wave-harness-failed')`（或子 wave → `Failed` 再走 §5.2 既有映射）；
#19 除两个崩溃注入点外，**加一条「bootstrap op 失败」用例**，断言父任务终结而不是无限重进。

### [MAJOR-4] child-wave op 在 child 已落库后 `Failed/Stuck` ⇒ 父任务终结、子 wave 活着、无人闭合、父 wave 不可删
`reconcile_spawn_result` 的 `Failed`/`Stuck` 两臂都走 `fail_spawn`（`scheduler/mod.rs:1035-1041`），
父任务落 `failed('spawn-failed')`。§5.2 的映射表只有「子 wave 状态 ⇒ 父任务」，
**没有「父任务已终结但 `child_wave_id` 指向非终态 child」这一行**；
§5.3 的 sweep 臂只枚举 `dispatched/running` ⇒ 这条边此后永不被检视。
后果与 BLOCKER-1 同形：子 wave 孤儿、树预算幽灵占用、父 wave 被 descendant 守卫锁死。
**最小修法**：要么在设计里定型「`child-wave` op 是 prepare-only、无 parked 相位，故 Failed/Stuck 蕴含 child 未创建」
并加一条结构断言；要么补一条补偿臂（fail_spawn 前检查 `child_wave_id`，有则改走 §5.2 而非 `spawn-failed`）。

### [MAJOR-5] §4 删掉 liveness 后，父任务**完全没有上界**；dead-root 收割器够不到 Working/Reviewing
`dead_root_candidates` 只有两条臂：**Draft**（最近一条 start-op failed）与 **Planning**（root session 丢失），
且注释明写「Dispatching/Blocked 故意不在范围内」（`session_repo_impl.rs:171-205`）。
子 wave 一旦进入 `Working`/`Reviewing` 后 spec 死掉，wave 永不终态 ⇒ 父任务永久 running。
设计承认「子 wave 动辄跑几小时」所以删 deadline，但**没有给出任何替代的可观测上界**。
**最小修法**：不必重新引入 deadline —— 至少登记一条「父任务 running 超 X 无 child 进展 ⇒ 结构化诊断/告警」，
或在 §12.1 明确登记为已知产品限制 + 人工恢复路径（同 §5.4 父 cancel 那段的写法）。**不写等于名义不变量为假。**

### [MAJOR-6] §7 #21c 的跨 cove 绊线是**空真**，且它规定的变异不可执行
第一半「全表断言每条 `parent_wave_id IS NOT NULL` 的行满足 `child.cove_id = parent.cove_id`」
在只有一个 cove 的 fixture 下恒真 —— 这正是本项目自己列的「vacuous invariant」形状。
指定的变异「`child-wave` adapter 不复制 `cove_id`」也**不可执行**：`wave_create_tx` 的 `cove_id` 是
必填入参且先做 cove 存在性校验（`crates/calm-truth/src/db/sqlite/wave.rs:19-23,49-53`），
「不复制」写不出来。
**最小修法**：fixture 必须有 **≥2 个 cove**，变异写成「adapter 用 `NewWave` 默认/另一个 cove 的 id 而不是父的」；
第二半（手工造跨 cove 边 ⇒ 删 cove 失败）我已实测成立，保留。

---

## MINOR

1. **#4 的「显式 `null`」在公共写口是 validation error，不是规范化输入**：
   `crates/calm-types/src/report_blocks/kinds.rs:252-256` 的判据是
   `if let Some(v) = map.get("spawn") && !matches!(v.as_str(), Some("in-wave"|"sub-wave"))` ——
   `Value::Null` 的 `as_str()` 是 `None` ⇒ **报错**。第三格只能由绕过校验的 fixture 制造，
   或者诱使实现方**放宽写口**去满足验收（静默扩大 schema）。
   建议：#4 第三格改成正面断言「`spawn: null` ⇒ 写口拒绝」，规范化仍只覆盖「缺席 / 显式 `in-wave`」。
2. **跨 cove 绊线的补强（回答 §12 第 3 问）**：今天**确实只有 `child-wave` adapter 能写出父子边**，
   且 wave **没有换 cove 的面**——`WavePatch` 无 `cove_id`（`crates/calm-truth/src/db/sqlite/wave.rs:105-148` 的
   apply 分支只有 title/sort/archive/pin/lifecycle/cwd 等），全仓也搜不到任何 `UPDATE waves SET cove_id`。
   建议绊线再加一条源码闭集断言（「不存在 cove-move 写点」），否则将来一个 `moveWave` API 会静默重开这个洞。
3. **§10 文档修订清单与 §2.1 裁决自相矛盾**：`:752` 附录 C.2 那行仍写 **`ON DELETE CASCADE`**，
   而 §2.1（`:225-251`）已裁决回 `NO ACTION`。v3 漏改，**照此清单施工会把撤回的 BLOCKER 原样种回去**。
4. **§10 / 标题的陈旧计数**：`:1` 标题仍写「v2」；`:755` 附录 D.2 写「五条 ⇒ 二十三条」，
   而 §7 现在是 28 条。计数错的清单会让「已验证的变异数」再次失真（r2 MINOR-4 同族）。

---

## v3 的修法里，哪几处制造了新洞

| v3 修法 | 新洞 |
|---|---|
| §5.2 新增 quiescence（修 r2-A BLOCKER）| **BLOCKER-1**：`pending` 一格永远不静止，且整表没有活性断言 |
| §4 三站点按 `(spawn, kind)` 判（修 r2-B MAJOR-3）| **MAJOR-1**：两处嵌在臂内 ⇒ 单点变异不可能红，#11 的要求不可满足 |
| §7 #10 升级成表驱动真实 adapter（修 r2-B MAJOR-5）| **MAJOR-2**：oracle 是「Err + 零副作用」，任何前置失败都满足 ⇒ 更贵的恒绿 |
| §3.3 稳定 idem key（修 r2-A exactly-once）| **MAJOR-3**：拆掉了 dead-root reaper 依赖的「fresh op id」重试前提 |
| §7 #21c 新增跨 cove 绊线（v3 自创）| **MAJOR-6**：单 cove fixture 下空真 + 变异不可执行 |
| §2.1 回到 `NO ACTION` | **修得干净**（我实测复核通过），但 §10 的清单漏改（MINOR-3）|

**§2.1 实证复核（我自己跑的 sqlite3，非引用）**：
① 同 cove `w_root←w_child←w_gc` + 单条 `DELETE FROM coves` ⇒ **成功，waves 剩 0 行**；
② 单行 `DELETE FROM waves WHERE id='w_root'`（即 `wave_delete_tx` 形状）⇒ **FK failed** ⇒ 印证 descendant 守卫必须下沉；
③ 跨 cove `w_cross(c2)→w_root(c1)`，删 c1 ⇒ **FK failed**（绊线成立）；
④ **新增**：`DELETE FROM coves WHERE id IN ('c1','c2')` 单语句同时删两个 cove ⇒ **成功** ——
说明跨 cove 边只在「引用方跨过语句边界存活」时致命，绊线文案应这么写；
⑤ **新增**：先删 c2 再删 c1（两条语句）⇒ 成功。
**`cove_delete_tx` 确实没有逐 wave 删除**：它逐 wave 清 `wave_vcs_*`/`tasks`/`worker_sessions`（无 FK 的表），
waves 本身只由末尾那条 `DELETE FROM coves`（`crates/calm-truth/src/db/sqlite/cove.rs:147-182`）经 `cove_id CASCADE` 消失；
route 层 `routes/coves.rs:317-388` 也只做 terminal teardown + overlay + lease 释放，**不调 `wave_delete_tx`**。
⇒ **§12 第 1 问：v3 的裁决成立。**

## §7 的 28 条里，哪几条在我设计的变异下仍然绿

| # | 为什么绿 |
|---|---|
| **#4** | 「显式 `null`」进不了生产写口（`kinds.rs:252-256` 直接报错）⇒ 该分支要么是死代码、要么靠绕校验的 fixture（MINOR-1）|
| **#10** | oracle 是「Err + 零副作用」；删掉那一行后各 adapter 的下一条前置照样失败 ⇒ 恒绿（MAJOR-2）|
| **#11** | `:1290` / `:1319` 嵌在 `:1271` 臂内，臂序修好后二者是死代码 ⇒ 单点变异必绿（MAJOR-1）|
| **#13b** | 只买「不许早闭合」；**没有任何一条买「子 wave 静止后必须闭合」** ⇒ 一个「永远 no-op」的实现全绿（BLOCKER-1）|
| **#19** | 崩溃注入点枚举了两个，但**没有「bootstrap op 落 `failed`」这一格**；稳定 key 下该状态不可自愈却无用例（MAJOR-3）|
| **#21c** | 单 cove fixture 下前半段空真；指定变异写不出来（MAJOR-6）|

其余 22 条我未证伪。本轮质量最高的是 #1、#3c、#5/#5b（拆开后）、#8、#18、#21/#21b ——
尤其 #21 的 `PRAGMA foreign_key_list` 结构断言 + #21b 行为断言这一对，是全表最干净的一处。

## 可以施工了吗

**NO**，但只差一条。

**最小阻塞集（1 条）**：
**B1** —— §5.2 的 quiescence 集合排除 `pending`（或在 `→ Done` 边沿清理残留 pending），
并新增一条**活性验收**：「子 wave `Done` + 一条永不可调度的 `pending` ⇒ 父任务必须在有限轮 sweep 内闭合」。

**随修订一并落、不需要再开一轮**：MAJOR-1/2/3/4/6 与 4 条 MINOR 都是 §4 / §5.2 / §7 / §10 的**局部改字**，
其中 MINOR-3（§10 清单里残留的 `ON DELETE CASCADE`）**必须改**，否则实现方照清单会把已撤回的 BLOCKER 种回去。
MAJOR-5 可以选择「登记为已知限制」而不是修。
