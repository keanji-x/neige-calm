# #985 切片 6 PR-A 实现评审 r2（codex）

评审对象：`9d30006a..98e3acba`。结论先行：**NO**。修复轮 1 的 B1 在修旧竞态时把慢、
不可回滚的 teardown 搬进 SQLite 唯一 writer 事务，形成新的全库写阻塞；B2 的新接缝仍由
25ms 猜测调度顺序；#21c 则用新性质替换了原性质的一半。所有命令均保持
`NEIGE_CODEX_BIN` 未设置、`CARGO_BUILD_JOBS=6`，PATH 含指定 `.local-bin`。

## BLOCKER

### B1. leaf fence 正确了，但外部 teardown 占住全库 writer 且无法随事务回滚

- **结论**：route 在 `BEGIN IMMEDIATE` 取得唯一 writer 后，串行执行 turn interrupt、terminal
  renderer/socket/process teardown 与 harness shutdown；一个 hung terminal 可持锁 5s，多个 terminal
  线性叠加。后续任一 DB 步骤失败时事务回滚，已 kill/remove 的外部状态却不会恢复。
- **攻击**：在 leaf guard 后模拟 1.5s teardown，同时提交完全无关的 cove 写；该写在 250ms
  硬超时内拿不到 writer。真实 terminal 路径明确允许单个 5s，且 route 对 terminals 串行 await。
- **证据**：writer 事务与 leaf guard在 `crates/calm-server/src/routes/waves.rs:1400`、`:1406`；
  三类外部调用在 `:1407`、`:1415`、`:1422`；可能失败并回滚的 DB 尾段仍在 `:1431`、`:1446`。
  terminal 的 5s 上界及 timeout 在 `crates/calm-server/src/terminal_sweeper.rs:220`、`:237`。
  此外 cards/terminals 快照仍在事务外（`routes/waves.rs:1374`、`:1385`），快照后新资源会让尾段失败。
- **实际验证**：临时在 `wave_require_leaf_tx` 后 sleep 1.5s，并把
  `wave_delete_shuts_down_active_spec_harness` 加为并发 writer 探针：**PASS 1.772s**，其中无关
  `cove_create` 按预期超过 250ms；恢复态该测试 **PASS 0.265s**，
  `acceptance_20_descendant_refusal_preserves_live_wave_runtime_and_terminal` **PASS 0.469s**。
- **最小修法**：短事务内把 wave 原子标成 durable `deleting`（child adapter/资源创建口均拒绝），
  commit 后在锁外 teardown；第二个短事务复核 fence 并删除。teardown/尾段失败保留可恢复的
  deleting 状态供重试，不能把“DB 看似仍 live、运行体已死”作为回滚结果。

## MAJOR

### M1. B2 的 Parked observer 仍靠 25ms 猜测 `submit()` 已返回

- **结论**：生产顺序目前正确，但 tripwire 会随调度顺序假绿。observer 被 spawn 后 sleep 25ms
  就宣称 scheduler 已进 `wait()`；这不是 happens-before，在高负载 CI 不成立。
- **攻击**：把 `mark_sub_wave_running` 前移到 bootstrap `wait` 前并令成功臂 `Ok(())`：保留 25ms
  时测试红；再只删 25ms sleep，同一个错误实现转绿，精确复现实现方第一版的遮蔽形状。
- **证据**：概率窗和无依据注释在 `crates/calm-server/tests/scheduler.rs:968`、`:972`；测试收到通知
  便读状态在 `:6208`、`:6211`；生产承重顺序在 `crates/calm-server/src/scheduler/mod.rs:1346`、`:1359`。
- **实际验证**：`acceptance_19_child_bootstrap_is_before_running_and_exactly_once_after_redrive`：
  提前 flip + 25ms 为 **FAIL 0.193s**（Running != Dispatched）；提前 flip + 0ms 为
  **PASS 0.324s**；恢复态 **PASS 0.398s**。
- **最小修法**：在 fixture/runtime 增加确定性的 `wait_entered` barrier，由 `OperationRuntime::wait`
  入口通知测试；observer 只能在该 barrier 后通知/等待 release，删除所有调度 sleep。

### M2. #21c 现在只测“真实 adapter 不造错边”，没有再测原来的 loud tripwire

- **结论**：新测试解决了“raw SQL 绕过 adapter”，但把设计中的合取性质缩成了一半：它只造正常
  同-cove child 并删除无关 cove；没有手工造跨-cove 边，也不再断言删除被 NO ACTION 响亮拒绝。
- **攻击**：把 0071 self-FK 改成 `ON DELETE CASCADE`。当前 #21c 的全表 mismatch 仍为 0，
  无关 cove 仍可删，因此测试绿；原 #21c 的跨-cove 删除必会由红变绿。
- **证据**：原验收明确要求两个合取项（`docs/_985-s6-design.md:725`）；现测试只提交真实 adapter、
  断言零错边并删 unrelated cove（`crates/calm-server/src/operation/child_wave_adapter.rs:677`、`:684`、`:693`）。
- **实际验证**：CASCADE 单点变异下
  `acceptance_21c_real_adapter_never_writes_a_cross_cove_edge` **PASS 0.120s**；恢复态 **PASS 0.126s**。
  （#21 结构测试会另行抓住 CASCADE，但这不等于 #21c 仍覆盖其原行为性质。）
- **最小修法**：保留当前 adapter 测试，同时恢复独立反向测试：raw SQL 造跨-cove edge，断言
  `cove_delete_tx(parent_cove)` 失败且两 cove 数据均未变。

## MINOR

### m1. M3 新测试的“无 TaskCompleted event”断言是恒真的附属断言

- **结论**：M3 的 `changed == 0` 与状态未变断言有判别力；但 event-count 断言没有。
  fixture-only helper只执行 guarded UPDATE，从来不走 event append，所以无论 guard 对错都不会发事件。
- **攻击**：直接调用 helper 后查询 event 表不能证明生产 reconciliation 的 event 行为。
- **证据**：helper只转调 SQL flip（`crates/calm-server/src/scheduler/mod.rs:353`、`:383`）；恒真断言在
  `crates/calm-server/tests/scheduler.rs:6151`，而真正 event append 不在该 helper 内。
- **实际验证**：恢复态 `acceptance_18_success_flip_rechecks_done_after_its_snapshot` **PASS 0.235s**；
  同次运行确认它的承重断言是 `changed == 0`/Running（`tests/scheduler.rs:6146`、`:6147`）。
- **最小修法**：删掉该 event 断言，或另用完整 `reconcile_child_wave` 接缝验证 race-lost 不发事件。

## 修复轮 1 的七处，哪几处修出了新洞

- **B1：新 BLOCKER**，见上；拒删原子性买到了，但把慢 IO 放进唯一 writer。
- **B2：新 MAJOR 测试洞**，见上；durable Parked 正确，25ms 接缝不确定。
- **B3：未打穿**。我在 create/Failed fixture 中先删 durable child 再 sweep；测试因期望
  `child-wave-create-failed` 而红，实际父任务已由 reconciliation 收为 `child-wave-deleted`，没有半截状态。
  自查与失败 stamp 同属一 writer tx（`crates/calm-server/src/scheduler/mod.rs:1400`、`:1412`、`:1420`）。
- **M1（共享 CTE）：未打穿**。两个 SQL 都 concat 同一宏（
  `crates/calm-server/src/operation/child_wave_adapter.rs:35`、`:49`、`:54`）；运行用例有 500ms
  硬 timeout（`:726`），不依赖 nextest warn-only。恢复态静态+cycle 两测均 PASS，cycle 总耗时 0.135s。
- **M2：未打穿**。三个反例分别改变 status/spawn/child id，且检查 `rows_affected==0`
  （`crates/calm-server/tests/scheduler.rs:5855`、`:5895`、`:5900`）；恢复态 PASS 0.103s。
- **M3：主性质未打穿**；同 tx 先观察 Done、再 delete/reopen、再调生产 SQL helper
  （`tests/scheduler.rs:6114`、`:6122`、`:6128`、`:6141`）。仅附属 event 断言恒真，见 m1。
- **M4：未打穿**。父 fixture 先写四个非默认值，child 又直接按确定 oracle 断言 Draft/三个 NULL
  （`crates/calm-server/src/operation/child_wave_adapter.rs:411`、`:578`、`:590`）；恢复态 PASS 0.115s。
- **#21c：新 MAJOR 测试洞**，见上；驱动真实 adapter 后没有保留原反向性质。

`.config/nextest.toml:25`、`:34` 的 slow-timeout 确为 warn-only。我检索了本片新增的 timeout/
elapsed 断言：无限 CTE 已由测试内 500ms 硬失败；B2 两个 1s barrier 也会硬失败。未发现另一条
仅靠 nextest slow 标记才“变红”的承重断言。

## 我自己设计的变异里，哪几条打穿了

1. **提前 Running + 删除 25ms 窗**：同一错误生产实现从红变绿，打穿 B2。
2. **0071 改 CASCADE**：当前 #21c 仍绿，打穿被删除的原反向性质。
3. **leaf guard 后慢 1.5s + 无关 writer 探针**：无关写超过 250ms，证实 B1 全库阻塞。

三组变异后均已执行 `git checkout -- .`；恢复态定向门 **10/10 passed（1.354s）**。

## SLOW 测试的基线对比数字

同一命令单线程单跑：`98e3acba` **25.549s**，`9d30006a` **25.397s**，差
**+0.152s / +0.6%**；编译时间不计入。结论：**不是 0071 引入的退化**，>120s 是整 workspace
并发/冷态资源竞争下的放大，本分支单跑未复现。

测试会遍历所有版本（`crates/calm-server/tests/cases/migration_replay_harness.rs:48`、`:56`），每轮先
应用 `1..N`（`crates/calm-server/tests/support/migration_replay.rs:73`、`:83`），再应用剩余迁移
（`:96`），故既有增长形状是 **O(M²)**。本片只把迁移数从 70 增到 71；理论增量 O(M)，实测在噪声内。

## 可以合入了吗

**NO。** 最小阻塞集：B1（durable deleting 两阶段，外部 IO 移出 writer tx）、M1（B2 改成确定性
`wait_entered` barrier）、M2（保留真实 adapter 测试并恢复跨-cove loud-failure 反向测试）。
m1 不单独阻塞。

最终 `git status --short`：

```text
?? docs/_985-s6-impl-review-r2-codex.md
```
