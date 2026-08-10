# #985 切片 6 PR-A 实现评审（codex）

评审范围：`9d30006a..61f52b82`。结论先行：**NO**。生产主链当前大体接对，
但有 1 个真实的拒删副作用缺陷，另有 4 个设计指定的承重行为可被改坏而验收仍绿。
以下所有变异均为本通道自行设计、实际执行，并已复原。

## BLOCKER

### B1. descendant 拒删落在 DB 太晚；返回 409 前已经杀掉父 wave 的运行体

- **结论**：DB writer 的确拒删 descendant，但 route 在调用它之前已经执行不可回滚的
  turn interrupt、terminal reap、harness remove/shutdown。用户删除一个有 child 的父 wave 会得到
  Conflict，父/子 DB 行都还在，父 harness/terminal 却已被停掉。
- **攻击**：父 wave 有 child，同时父卡有活 turn、terminal 和 harness；DELETE 先执行
  `interrupt_shared_card_active_turn`、`reap_terminal_artifacts_with_renderer`、`harness.shutdown`，
  最后 `wave_delete_tx` 才因 child 拒绝。并发新建 child 时，任何不持锁的 route precheck 仍有同一窗口。
- **证据**：`crates/calm-server/src/routes/waves.rs:1377`、`:1386`、`:1391`、`:1421`；
  真正 descendant guard 在 `crates/calm-truth/src/db/sqlite/wave.rs:222`。
- **实际验证**：我在 route DB guard 前加入持久 title 改写，运行
  `cards_deletable::acceptance_20_wave_delete_route_refuses_descendant_and_names_child`：**1 passed**；
  测试只看 409、child id 和父行存在（`crates/calm-server/tests/cases/cards_deletable.rs:568`、`:572`），
  不看拒绝前副作用。
- **最小修法**：在同一个 `BEGIN IMMEDIATE` 保留 descendant 判定/写锁，确认可删后才做外部 teardown，
  并用该事务完成删除；增加活 harness + terminal fixture，断言拒删时进程、registry、socket、DB 全不变。

### B2. #19 没有证明 bootstrap 严格早于 running flip

- **结论**：实现当前顺序正确，但指定的崩溃安全门是假的；把 flip 前移后测试仍绿。
- **攻击**：`submit` 后先把父任务写 `running`，进程在 bootstrap `wait`/成功前崩溃；恢复 sweep 的
  sub-wave running 臂直接 no-op，Draft child 和 running 父永久挂住。
- **证据**：当前正确顺序在 `crates/calm-server/src/scheduler/mod.rs:1312`、`:1325`、`:1327`；
  running sweep no-op 在同文件 `:1743`。测试只预造成功 op、事后把状态改回 dispatched，
  没观察 bootstrap 未完成时的父状态：`crates/calm-server/tests/scheduler.rs:6032`、`:6076`。
- **实际验证**：我把 `mark_sub_wave_running` 移到 bootstrap `wait` 前，成功臂改成 `Ok(())`；
  `acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive`：**1 passed**。
- **最小修法**：给 bootstrap adapter 一个可阻塞 test hook；阻塞时断言父仍 dispatched，放行后才 running；
  再在阻塞点丢弃 runtime、重建并验证一条 op/一次 mint。该顺序变异必须红。

## MAJOR

### M1. #18 的 SQL guard 门与生产源码同源，且“真实链路”走错分支

- **结论**：生产成功 flip 当前确有 child-Done + quiescence SQL guard；但测试不能证明它。
- **攻击**：删掉实际 `EXISTS(child.lifecycle='done')`，只把原字串留在 SQL 注释中。
  静态测试仍数到两次；运行时测试在调用 reconcile **之前**就删 child，只会走 deleted 失败臂，
  从未尝试成功 flip。
- **证据**：生产 guard 在 `crates/calm-server/src/scheduler/mod.rs:657`；源码字符串断言在
  `crates/calm-server/src/scheduler/tests.rs:19`；先删后调在
  `crates/calm-server/tests/scheduler.rs:5998`、`:6003`。
- **实际验证**：我把成功 guard 换成 `AND 1=1`，原字串只放 SQL comment；
  `acceptance_18_child_success_flip_rechecks_child_state_in_its_sql_guard` 与
  `acceptance_18_deleted_child_cannot_pass_the_done_sql_guard`：**2 passed**。
- **最小修法**：抽出 guarded flip；在同一测试事务中先读 Done、再删/改 child、再调用 flip，断言 0 行且
  无 `TaskCompleted`。删除源码字符串计数 oracle。

### M2. 递归有两段 CTE，静态门禁只守第一段；CI 超时还是 warn-only

- **结论**：cycle/missing-root 的诊断 CTE 也只能靠 depth 截断终止，当前静态门没有覆盖它。
- **攻击**：只删 `WAVE_BOUNDED_PATH_SQL` 的 `WHERE up.depth <= ?2`；第一段返回零行后第二段在 2-cycle
  无限递归。CI 不会把 slow-timeout 当失败。
- **证据**：两段独立 SQL 在 `crates/calm-server/src/operation/child_wave_adapter.rs:35`、`:46`，
  两个截断分别在 `:41`、`:52`；静态测试只检查第一常量（同文件 `:474`）；
  `.config/nextest.toml:34` 明写 warn-only。
- **实际验证**：上述变异后 `upward_cte_keeps_its_only_cycle_termination_guard`：**1 passed**；
  `acceptance_7_two_cycle_fails_fast_with_cycle_reason` 在 8 秒外层 timeout 下 **SIGTERM / exit 124**。
- **最小修法**：两查询共享同一 bounded CTE 片段，静态断言两处；运行时用 `tokio::time::timeout(<1s)`
  包住 future，使去截断在 CI 确定性红而不是无限挂起。

### M3. §3.5 继承矩阵的 fresh-child 负面字段整块无断言

- **结论**：实现现在靠 `wave_create_tx` 正确得到 Draft/未 archive/未 pin/无 terminal stamp，
  但 #5/#6/#19 全部允许这些字段被改坏。
- **攻击**：child 创建后强写 `lifecycle='planning', archived_at=1, pinned_at=1, terminal_at=1`；
  bootstrap fixture 的 Draft→Planning UPDATE 命中 0 行但照样 mint，所有承重测试仍绿。
- **证据**：设计要求矩阵每行有断言：`docs/_985-s6-design.md:449`；创建位置在
  `crates/calm-server/src/operation/child_wave_adapter.rs:170`。现有继承断言只读
  cwd/workflow/input/purpose：同文件 `:553`；#19 反而只断言 child **不是** Draft：
  `crates/calm-server/tests/scheduler.rs:6070`。
- **实际验证**：上述四字段变异后 #5、#6、#19：**3 passed**。
- **最小修法**：让父 fixture 带 archive/pin/terminal 非默认值，直接在 child adapter 提交后、bootstrap 前
  断言 child 是 Draft，且 archive/pin/terminal 均 NULL；每字段独立变异必须红。

## MINOR

### m1. 权威文档的 30/33 计数和附录 C 均漂移

- **结论**：§7 表实际是 **33** 个编号行，没有漏验收行；错误的是两处“30”。此外附录 C.1 未登记
  本片新增的 `tasks.spawn` / `tasks.child_wave_id`。
- **证据**：表从 `docs/_985-s6-design.md:693` 到 `:726` 可数 33 行，但同文 `:890`、
  `docs/architecture/985-doc-as-plan.md:1983` 写 30；附录 C.1 在权威文档 `:1859` 开表、`:1868`
  已结束旧列，而 migration 在 `crates/calm-truth/migrations/0071_sub_wave_tree.sql:10`、`:12` 新增两列。
- **实际验证**：用 awk 对 §7 的编号行计数得到 `count=33`；实现方的 33 行裁决是对的。
- **最小修法**：两处 30 改 33；附录 C.1 增 `spawn`、`child_wave_id` 两行并写明后者不进 TASK_COLUMNS。

## 实现方自述的缺口，哪些属实、哪些被低估

- **#21c 属实且被低估为“只有这一条 tripwire 弱”**：指定测试完全用 raw SQL 造跨-cove 边并删 cove
  （`crates/calm-truth/src/db/sqlite/sub_wave_tree_tests.rs:108`、`:114`、`:128`），不驱动 adapter；
  真实 adapter 同域断言在另一个 #6 测试（`crates/calm-server/src/operation/child_wave_adapter.rs:563`、`:593`）。
  因此 adapter 写错 cove 时 #21c 必绿；此外本评审还打穿 #19/#18/矩阵/route，漏报范围更大。
- **#11 属实**：正确臂序的 sub-wave arm 在 `scheduler/mod.rs:1743` 已吞掉该行，内层 helper 的
  spawn 排除（`:293`）不可达。我删除该排除，
  `acceptance_11_sub_wave_parent_survives_two_timeout_sweeps_without_deadline`：**1 passed**。
- **#5/#6/#8 的“未执行”属实，但含义不同**：#5 的手写完整 seed oracle在
  `child_wave_adapter.rs:525`，#6 的 direct-parent oracle 在 `:587`，有判别力；#8 只有 missing id
  fixture（`:648`），确实没有“超深、无环、断根”毒链 fixture。
- **#12/#13/#13e/#14 只是实现方没有跑完子变异，不是现有测试必绿**：我分别做 deadline-only、
  删除 `TaskCompleted`、bootstrap/Stuck 理由、Failed 理由变异；对应 #12/#13/#13e/#14 分别
  **红、红、红、红**。证据断言在 `scheduler.rs:5786`、`:5811`、`:6178`、`:5905`。

## 我自己设计的变异里，哪几条打穿了

1. 第二段 CTE 去截断 → 静态门绿，真实用例挂死。
2. descendant DB guard 前加入持久副作用 → route 拒删验收绿。
3. child fresh 字段四连破坏 → #5/#6/#19 三条绿。
4. running flip 提到 bootstrap wait 前 → #19 绿。
5. 成功 SQL guard 删除、原文留注释 → #18 静态 + 运行时两条绿。

全部变异执行后均已 `git checkout -- .` 复原；报告是唯一新增文件。

## 其余重点核对与实际门

- UPSERT 的 INSERT/SET/变更析取及 `?16` 双时间绑定同步：
  `crates/calm-truth/src/db/sqlite/task_projection.rs:1000`、`:1018`、`:1035`、`:1056`。
- 5 个完整 Task reader 共用 `TASK_COLUMNS`，其中含 spawn：
  `crates/calm-truth/src/db/sqlite/task.rs:19`、`:37`、`:135`；
  `crates/calm-truth/src/db/sqlite/read.rs:403`、`:415`、`:428`。`calm-truth` 全包 **352/352 passed**。
- 父闭合是 eventized DB 写并发 `TaskCompleted`/`TaskFailed`：`scheduler/mod.rs:600`、`:672`、`:727`；
  DTO 只有 child link、没有 spawn：`task_projection.rs:138`、`:152`。
- 两个 child task 并发由 IMMEDIATE 写收口；第二层 task id 含 wave id，且 operations 唯一键是
  `(kind,idempotency_key)`，所以 child-wave/worker 共用 task.id 不碰撞：
  `crates/calm-truth/migrations/0029_operations.sql:36`、`scheduler/mod.rs:1246`。
- 恢复后最终实际门：切片 acceptance/迁移筛选 **18/18 passed**；`calm-truth` **352/352 passed**。
  所有命令均 `env -u NEIGE_CODEX_BIN`、`CARGO_BUILD_JOBS=6`，nextest 使用指定 PATH。

## 可以合入了吗

**NO。** 最小阻塞集：B1 拒删前副作用；B2 bootstrap/running 顺序门；M1 child-Done SQL guard 行为门；
M2 第二 CTE 硬超时门；M3 继承矩阵负面字段门。m1 可随修复一并改，不单独阻塞。

最终 `git status --short`（写报告前树为空；写报告后唯一允许新增文件）：

```text
?? docs/_985-s6-impl-review-codex.md
```
