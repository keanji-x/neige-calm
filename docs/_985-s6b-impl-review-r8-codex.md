# #985 切片 6 PR-B 实现评审 r8（codex）

评审范围：`c71e4132..e9a1052a`。结论：2 个 MAJOR，0 个 BLOCKER，0 个独立 MINOR。

## BLOCKER / MINOR

均无。

## MAJOR

### MAJOR-1：冻结态无条件归因 tree，遗漏同时绑定的本地 ceiling

- **结论**：冻结把总 capacity 强制为 0 后，代码无条件只发 tree 诊断；若目标 wave 的本地
  capacity 也为 0，执行系统给出的全部动作仍不能增加准入。该选择分支见
  `crates/calm-truth/src/db/sqlite/task_projection.rs:927-934,1008-1020`。
- **触发条件 / 错值**：可达升级态 `N=2,B=4,target index=1,share=2,target ceiling=0`，root 有
  5 条不可裁 legacy。诊断只给 `raise_tree_task_budget(minimum=9)`；照做后冻结解除，但同一报告准入
  **0 → 0**，正确动作集合还必须包含 `raise_spec_task_ceiling`。本地/tree capacity 的合成在
  `crates/calm-truth/src/db/sqlite/task_projection.rs:628-650`。
- **证据**：注释甚至明确把 freeze 规定为“不管本地数值容量都归 tree”，随后 `tree_bound` 抢先吞掉
  tie/local 分支；`crates/calm-truth/src/db/sqlite/task_projection.rs:922-934`。现有唯一 freeze 表行却用
  `ceiling=4,share=2`，目标本地仍有余量；`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:780-789`。
- **我实际跑过的验证**：临时给 `the_diagnosed_capacity_action_increases_admission` 增加上述第 9 行，
  **FAIL 0/1**：`following {"raise_tree_task_budget": Some(9)} did not increase admission: 0 -> 0`；
  复原态 **PASS 1/1**。效果断言位于 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:835-853`；已复原。
- **最小修法**：freeze 时若 `ceiling_capacity==0` 同时发 local 动作；tree minimum 应直接搜索首个
  “全员 fixed_live 装回且目标 `share-tree_occupied>0`”的合法 B，而非先决定单一归因；计算输入见
  `crates/calm-truth/src/db/sqlite/task_projection.rs:633-650,937-947`。把上述行常驻验收。
- **不能登记后合入**：这不是容量策略代价，而是新验收承诺“执行全部命名动作后同一报告准入严格增加”
  的直接反例；该承诺写在 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:627-633`。

### MAJOR-2：不存在合法目标 B 时仍登记 raise 动作，并把缺失 minimum 渲染成 0/空串

- **结论**：搜索可以返回 `None`，但动作仍由诊断 code 无条件登记；Rust 又把缺失 minimum 当 0，
  web 当空串。证据为 `crates/calm-truth/src/db/sqlite/task_projection.rs:937-980`、
  `crates/calm-types/src/report_blocks/tasks.rs:287-324`、`web/src/pages/report-blocks/task.tsx:83-89`。
- **触发条件 / 错值 A**：孤根 `N=1,B=64,C=65`，65 条声明；share=64 且 B 已达合法上限 64，
  系统仍给 `raise_tree_task_budget`，无 minimum，Rust 文案成为“to at least **0**”。B 上限见
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:32-35`，写 API 拒绝大于 64 见
  `crates/calm-server/src/routes/waves.rs:1282-1290`。
- **触发条件 / 错值 B**：升级后两成员、`B=64`、index 0 有 33 条 legacy，则最大 share 仍仅 32，
  `minimum_budget_to_unfreeze=None`，但仍给 `Some("raise_tree_task_budget")`；无任何合法 B 能解除冻结。
  全员搜索与 `None` 语义见 `crates/calm-truth/src/db/sqlite/wave_tree.rs:258-280`。
- **我实际跑过的验证**：性质表临时加 `N=1,B=64,C=65`，**FAIL 0/1**：`tree action must carry a remainder-safe minimum budget`；临时测试 `review_unreachable_legacy_freeze_does_not_offer_a_budget_action`
  **FAIL 0/1**：`Some("raise_tree_task_budget") != None`。helper 见
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:835-845`；两次均已复原。
- **最小修法**：仅当联合可行性搜索得到合法 B 时登记 raise 动作和 minimum；否则 action 为 `None`，文案只保留等待或删 wave。禁止 `unwrap_or_default()`；
  `crates/calm-types/src/report_blocks/tasks.rs:287-305`。常驻 B=64 普通/冻结两行。
- **不能登记后合入**：这是对合法设置与明确支持的 upgrade legacy 状态给出不可执行动作，不是已登记的
  legacy 上界退化；代码自己声明 `None` 表示 64 内无解，见
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:175-182`。

## 表驱动性质、边界与删测复核

- 所谓性质测试是 **8 个手挑 Case**，不是 `(C,S,N,index)` 枚举/生成；表与单循环见
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:636-645,709-792`。它覆盖两种余数位置和平局，
  但没有 freeze+local-full、`B=64`、无解 freeze；MAJOR-1/2 证明这些遗漏能让实现错而原测试仍绿。
- 对有合法下一 B 的纯 share 问题，按每个 index 从 `B+1..=64` 搜索并直接调用确定性公式是正确的；`N>B`、`B=0`、`N=1` 不产生额外算术洞。见
  `crates/calm-truth/src/db/sqlite/wave_tree.rs:128-151`、
  `crates/calm-truth/src/db/sqlite/task_projection.rs:937-940`；真正缺口是搜索无解和联合约束。
- 删除“非树零递归”没有丢现存语义：对应 shortcut 已删除；孤根预算由显式/NULL 用例承重，见
  `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:975-1050`。删除“孤根固定两查询”也未丢真实回退门：
  whole-tree 入口累计并强制恰为 2，见 `crates/calm-server/src/wave_report.rs:169-227`。

## 旧条目更新抽查（至少 3 条）

- M9/R1-B1c/R3-B1a 更新属实；见 `docs/_985-s6b-mutation-map.md:253-260`。临时恢复精确孤根无限-share 早退后，`an_explicit_budget_applies_to_a_singleton_root`、`a_null_ceiling_and_tiny_budget_still_bind_a_singleton_root`、
  `singleton_default_budget_counts_legacy_occupancy_before_admission` **0/3，全 RED**，错值为多放 2、
  `5 != 1`、`33 != 32`；断言见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:977-1049,1143-1167`。已复原。
- R5-m1 退役并由 R7-B1a/B1d 替换属实；当前入口一致性明确要求双 code，见
  `crates/calm-server/src/operation/child_wave_adapter.rs:1195-1242`；复原态三包门中该测试 PASS。

## 修订轮 7 的五处，哪几处修出了新洞

1. 平局双点名本身正确，双诊断分支见 `crates/calm-truth/src/db/sqlite/task_projection.rs:1012-1018`。
2. 8 行表是手挑构造，漏出 MAJOR-1/2；`crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:709-790`。
3. “成员最小 B”在有解时正确，但在 B=64 无解仍发动作，修出 MAJOR-2；
   `crates/calm-truth/src/db/sqlite/task_projection.rs:937-980`。
4. freeze minimum 与 tree-only 归因共同修出 MAJOR-1，并在无解 freeze 扩大 MAJOR-2；
   `crates/calm-truth/src/db/sqlite/wave_tree.rs:258-280`、`crates/calm-truth/src/db/sqlite/task_projection.rs:927-947`。
5. 两条删测与四条旧证据更新未发现新洞；替代承重点见
   `crates/calm-server/src/wave_report.rs:169-227`、`docs/_985-s6b-mutation-map.md:253-260`。

## 实跑门与 web

- 题定环境、`NEIGE_CODEX_BIN` 未设置：三包 **2874 passed / 0 failed / 50 skipped**；核心性质见 `crates/calm-truth/src/db/sqlite/wave_tree_budget_tests.rs:627-856`。
- web 因无 `web/node_modules`，**未实际执行**；仅结构复核，见 `web/src/pages/report-blocks/task.tsx:67-90`、
  `web/src/pages/report-blocks/report-blocks.test.tsx:827-835`。

## 可以合入了吗

**NO。** MAJOR-1 的可达后果是用户执行系统给出的合法 B=9 动作后准入仍 **0→0**；MAJOR-2 的可达
后果是系统建议超过产品硬上限的 B，并在 Rust 显示错值“至少 0”。两者都直接违背本轮新增的动作有效性
验收，而非既有 legacy 退化例外；动作生成点见 `crates/calm-truth/src/db/sqlite/task_projection.rs:927-980`。

## `git status --short`

```text
?? docs/_985-s6b-impl-review-r8-codex.md
```
