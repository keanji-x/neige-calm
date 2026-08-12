# #985 切片 6 PR-B —— 实现评审 r9（放行检查 · CHANNEL_NAME = subagent）

范围 `c71e4132..d2adea3d`。跑测命令：`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH CARGO_BUILD_JOBS=6 cargo test -p calm-truth --lib wave_tree`（基线 **36 passed / 0 failed，6.16s**；穷举网格单跑 **2.60s**）。本 worktree 无 `web/node_modules`，web 侧结论均为**结构性复核，未实际执行**。

## BLOCKER

无。

## MAJOR

### M1（r8 新引入）`bounds_tied` 被重载后，`spec_task_ceiling` 会谎报「树份额已用尽」

- 结论：`task_projection.rs:1026` 把 `bounds_tied` 定义为 `admission_frozen && ceiling_capacity == 0`。但 `bounds_tied` 的文案含义是「本波份额与本地上限**双双打满**」（`tasks.rs:332-338`、`tasks.rs:275-281`、`web/src/pages/report-blocks/task.tsx:67-70`）。**兄弟成员超额导致的冻结**里目标波份额根本没用，文案即为假陈述。
- 触发条件：N=2、B=6（份额 3/3），成员 0 有 5 行 legacy live → 冻结；目标 = 成员 1，占用 0，`spec_task_ceiling=0`。
- 证据：我就地加探针 `probe_sibling_freeze_zero_ceiling_prose` 实跑，输出为
  `share=TreeShare{ share: 3, admission_frozen: true, minimum_budget_to_unfreeze: Some(9) }`，
  而 `spec_task_ceiling` 消息是 `spec task ceiling of 0 and this wave's tree share are both reached`——share=3、occupied=0，未 reached。
- 另注：`tasks.rs:325` 的 frozen 分支先于 `bounds_tied` 判定，所以这个重载在 `tree_budget_exhausted` 上**完全是死参数**；它唯一的实际效果就是上面这句错话。
- 为何可随 PR 合入：它命名的两个动作（抬 ceiling + 抬 B 到 minimum）依然正确且**有效**（B→9、ceiling→1 后 capacity=1），穷举网格断言的是效果不是措辞；这是文案准确性缺口，不是可达的错误裁决。
- 最小修法：`ceiling_diagnostic` 增一个 `frozen: bool`，冻结时走「树已冻结，且本地上限无余位」的独立文案，别复用 `bounds_tied`；`tree_diagnostic` 的 `bounds_tied` 在冻结时传 `false`（反正是死值）。

### M2（r8 改了一边）Rust 与 web 的 frozen 文案不再对齐

- 结论：r8 给 web 的 frozen 分支加上了「to at least ${minimum}」（`web/src/pages/report-blocks/task.tsx:92-94`），Rust 的 frozen 分支**没有**渲染 `minimum_budget`（`crates/calm-types/src/report_blocks/tasks.rs:325-331`）。
- 触发条件：任何 `admission_frozen=true` 且有合法 B 的树。上面 M1 的探针实测输出即为证：Rust 消息为 `raise tree_task_budget enough for every member's existing work to fit`，没有数字 9；`minimum_tree_task_budget=9` 只在 `messageArgs` 里。
- 后果：MCP / API 读到的 `message` 比 UI 少一个关键数字，等于把 r8 想解决的「可执行性」在 Rust 侧漏掉了一档。
- 「改一边不改另一边会红吗」：**不会**。Rust 断言只看 Rust 渲染（`wave_tree_budget_tests.rs:901-911`），web 断言只看 web copy（`report-blocks.test.tsx:906-924`），两侧无任何交叉校验；本条就是这个结构漏洞的现成实例。
- 最小修法：Rust frozen 文案补 `（at least {minimum_budget}）`；跨语言一致性另开 issue。

### M3（既存，非 r8 回归）穷举网格触及不到 `ceiling_occupied > 0`，那里「ceiling+1」无效

- 结论：网格里 block 行始终是 `pending`，而 `ceiling_occupied` 只数 `dispatched|running|verifying`（`task_projection.rs:628-633`、`:470-476`），所以 504 个格子里 `ceiling_occupied ≡ 0`、`ceiling_capacity ≡ ceiling`。`ceiling < ceiling_occupied`（运维把 ceiling 调到在飞量以下，`routes/waves.rs:1275-1280` 无下调守卫）这一族完全没被扫到。
- 触发条件 / 实测：探针 `probe_ceiling_below_inflight_occupancy_action_is_ineffective` —— 单波，5 行 `origin='block'` running，`ceiling=1`、`B=5` → tied 双诊断，动作 `raise_spec_task_ceiling` + `raise_tree_task_budget(min=6)`；照做（ceiling→2、B→6）后 **before=0 → after=0**，与网格判红标准一致。
- 为何可随 PR 合入：`ceiling` 侧动作方向正确、迭代可收敛（再报一次 ceiling=2/occupied=5），且 `bounds_tied` 之外的 ceiling 文案本来就打印 `occupied`；缺的是 tied 文案里的目标数字，不是错值。
- 最小修法：ceiling 诊断补 `minimum_spec_task_ceiling = ceiling_occupied + 1`，并给网格加一维「把部分 block 行置为 running」。

## MINOR

- N1 `web/src/pages/report-blocks/task.tsx:68-69` 新增的 `spec_task_ceiling + capacity_raise_unavailable` 分支**无任何 web 用例**（`report-blocks.test.tsx` 只补了 `tree_budget_exhausted` 的 unavailable 例）。Rust 侧已覆盖（`wave_tree_budget_tests.rs:895-911`）。未实际执行 web。
- N2 `Diagnostic::coded` 的动作断言（`tasks.rs:179-195`）是**生产期 assert**：若将来出现 `ceiling_diagnostic(None, false)`（tied 为 None 而 raise 不可用），会插不进 `capacity_raise_unavailable`、进而 panic。当前不可达，但 `task_projection.rs:1000-1013` 把 arg 写入放在 `if let Some(share) = tied` 里是脆的，建议把 arg 提到 `if !raise_available` 顶层。
- N3 网格文档注释说「varies remaining local capacity from zero upward」，与 M3 的事实（只变 ceiling、不变 occupied）不符，建议改口径。

## 修订轮 8 的四处，哪几处修出了新洞

1. **联合目标搜索 `> target_occupancy`**（`task_projection.rs:938-946`）：**无新洞**。变异复核：把 `share.share.max(tree_occupied)` 改回 `share.share`，`the_diagnosed_capacity_action_increases_admission` 报 **210 bounded capacity actions were ineffective**、`a_frozen_wave_with_nonzero_ceiling_occupancy_names_both_bounds` 同时红（36 → 34 passed / 2 failed）。数值与实现方声明一致。单调性我另行手证：`deterministic_share` 对 B 单调不减，`minimum_for_target` 与 `minimum_budget_to_unfreeze` 取 `max` 后两个条件同时成立，逻辑成立。
2. **冻结态双归因**（`task_projection.rs:1025-1033`）：**修出 M1**。变异复核：把条件写死为 `false`，红 **3** 个测试、网格报 **35 ineffective**，与实现方声明的「精确 35 例」一致；但 `bounds_tied` 的语义重载带来了 M1 的假陈述。
3. **无合法目标 B 时不登记动作 / 不渲染空数字**（`task_projection.rs:983-986,1019-1023`；`tasks.rs:317-324`；`task.tsx:85-90`）：**无新洞**，但留下 M2（Rust frozen 文案未同步）与 N1。fail-closed 过头我专门查过：`minimum_for_target` 的搜索域是 `budget+1..=64`，`share(B)` 对 B 单调不减，冻结时还要 `.zip(minimum_budget_to_unfreeze)`，两者都单调，**没有把有解误判成无解**的构造。反向变异：把搜索域改成空区间（永远 None），网格立刻在 `N=1,B=0,target=0,legacy=None,C=0: no capacity action` 处红，共红 5 个测试——过严也是被夹住的。
4. **穷举验收**（`wave_tree_budget_tests.rs:626-804`）：边界 `N=1..=3 × B=0..=6 × target × ceiling=0..=5 × legacy∈{无, 目标波 3 行}` = **504**，`assert_eq!(checked, 504)` 防缩水；期望值**不与生产同源**（读诊断给出的 minimum 去改配置，再用 `SELECT count(*) … origin='block'` 数真实准入），耗时 **2.60s**，CI 无压力。已知被排除的族有两个：兄弟超额（作者已注明，另有 `legacy_member_overage_freezes_new_blocks_across_the_tree`）与 `ceiling_occupied > 0`（**未注明，即 M3**）。

## 可以合入了吗

**YES。**

三条 MAJOR 都不满足放行判据里的两种 NO 理由：M1/M2 是文案准确性与跨端一致性缺口（命名的动作本身正确且实测有效，无错值裁决）；M3 是既存覆盖族缺口而非 r8 回归，且动作方向正确、可迭代收敛。核心不变量 `Σ_v live_spec(v) ≤ B`、fail-closed 归属、双归因效果这三条我都用变异证明夹住了（210 / 35 / 5 例红）。建议 M1 + M2 + M3 合并为一条「容量诊断文案与目标数字」跟进 issue，不阻塞本片。

## git status --short

```
?? docs/_985-s6b-impl-review-r9-subagent.md
（仅本评审文档；两个临时探针与三处变异均已 `git checkout -- .` 复原）
```
