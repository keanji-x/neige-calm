# 文档即计划

状态：当前架构。Report 中的 `task` block 是任务声明真源，`tasks` 表是执行投影。

## 产品主张

报告不是“计划的说明文字”；报告中的结构化任务就是计划。用户与 Agent 应在同一个文档里看到：

- 为什么要做；
- 要做什么；
- 依赖和验收是什么；
- 是否已准备执行；
- 当前执行状态与结果；
- 为什么被阻断或作废。

对外概念应收敛成三句话：

1. 任务卡写清目标和完成条件，ready 后进入队列。
2. 人删除的任务留下“不做”记录，Agent 不能用同一 key 偷偷重提；人可以撤回。
3. 每个 wave 决定 Agent 声明的任务自动执行，还是等待人工放行。

Hash、冻结集、projection、closure 和 CAS 都是实现细节，不应直接暴露给用户。

## 声明与执行分离

Task block 保存声明：

- 稳定 `key`；
- 目标、验收、依赖；
- worker kind、cwd/gate 等执行要求；
- refs/spawn 等上下文关系；
- `ready`；
- 声明者、墓碑和人工放行归因。

`tasks` row 保存运行态和冻结后的执行证据，例如 status、worker 绑定、gate attempt 和 context freeze。

同一个字段不能同时表示“文档现在希望什么”和“已经启动的 worker 实际承诺了什么”。任务 claim 后，后续文档编辑可能使当前执行失效，但不能原地改写已经交付给 worker 的输入。

## 身份

任务身份是 wave 内稳定的 `key`，不是 block id：

- block id 支持编辑、移动和 fork；
- key 连接依赖、运行记录、墓碑和幂等 operation；
- 同一文档中 key 必须唯一；
- 删除后用同一 key 重建受墓碑约束。

Key 不能在原 block 上修改。确实要改变身份时，删除/墓碑旧任务并创建新 key。

## 写入边界

所有能改变 report task block 的入口必须经过同一组 guard：

- 人与 Agent 的创建/修改/删除权限；
- `declared_by`、`tombstoned_by` 等归因由系统写入且不可伪造；
- 用户控制的 task 不能被 Agent 覆盖或删除；
- 整文档 replace 不能绕过 block 级 task 规则；
- revision/merge 冲突不能静默丢任务。

任务删除使用 block 级入口。普通 markdown/CRDT 整体写不能顺带删除或改写结构化任务。

## 同事务投影

一次 report 修改在同一事务内完成：

```text
更新 CRDT / report blocks
→ 校验 task 声明
→ 计算 block diagnostics
→ 投影 tasks
→ 更新反向引用与声明影子
→ 写 report/task events
→ 必要的 lifecycle 与 Wave VCS
→ commit
```

不能先提交文档、再异步补 tasks；否则 scheduler 会短暂执行旧计划。

投影函数必须满足：

- 纯声明错误产生诊断，不创建可执行 row；
- 未受影响的 in-flight/terminal row 不被 rebuild 改写；
- pending row 可以随声明更新或被守卫式删除；
- 同 key 重放幂等；
- incremental projection 与末态全量 rebuild 逐字段相同；
- 相同输入产生稳定顺序和稳定诊断。

## 诊断

声明解析和 DB-aware policy 产生结构化诊断。诊断读时派生，不成为第二份持久真源。

每条诊断至少回答：

- 哪个任务有问题；
- 原因是什么；
- 关联哪些 block/wave；
- 用户下一步可以做什么。

典型诊断包括重复 key、未知依赖、依赖环、缺 gate、超过额度、墓碑阻断、引用失效和 context stale。

诊断必须使用人能理解的句子。内部枚举值可以作为稳定 code，但 UI 不能只显示 code、圆点或红叉。

## Claim 与冻结

Scheduler claim ready task 时冻结实际执行上下文。冻结集合覆盖会改变 worker 语义的声明和引用闭包，而不包含只影响排队方向的控制位。

冻结后，系统持续比较当前文档与已交付上下文：

- 没有 material 变化：执行继续；
- material 变化且任务未启动：更新或撤回 pending 投影；
- material 变化且任务在飞：终止/作废本次执行并给诊断；
- 引用无法解析、闭包过深或状态矛盾：fail closed。

claim 前定位失败一律不下判决；它只产生诊断，由明确的状态写口决定是否终结。

Hash 只是变化检测载体，不是权威。真正的判定必须保留可解释的字段与引用关系。

`ready` 从 true 变 false、人工放行撤回等方向性控制不应靠内容 hash 猜测；投影保存必要前值并使用明确规则。

## 引用闭包

任务可以引用同 wave 或受允许范围内的 report block。执行上下文由根 task block 加 refs 闭包组成。

解析必须：

- 使用稳定 block identity；
- 检测 cycle、missing、cross-area 和深度上限；
- 有固定节点预算，超限 fail closed；
- 保存每个参与文档的 revision/evidence；
- 在被引用 block 改动时找到受影响 task。

反向索引只用于加速触发；周期全量 sweep 是正确性后备。漏掉一次 trigger 不能让 stale task 永久继续执行。

## 人与 Agent 的不对称

### 归因

人和 Agent 创建的 task 都由系统盖章。客户端不能提交任意 actor 字段来获得权限。

### 墓碑

人删除 Agent 声明的 task 时保留墓碑。墓碑阻止同一 key 被 Agent 重新声明；人可以显式清除。墓碑记录的是产品决定，不是隐藏已完成的运行历史。

### 放行

`automation_policy`：

- `auto-declare`：合法且 ready 的 Agent task 可进入调度；
- `declare-and-wait`：Agent 可以提议，但需要人工放行。

删除一个任务不会暗中把整个 wave 自动切到 wait。若用户希望以后都等待，应由确认操作显式修改 policy。

`released_by_user` 通过 UI 动作维护，不要求用户编辑内部字段。

## Budget 与 wave tree

两种限制解决不同问题：

- `spec_task_ceiling`：限制一个 wave 中 Agent 声明占用的任务数量；
- tree task budget：限制父/子 wave 整棵树的总容量和并发份额。

当前树深和总预算都有硬上限。创建 child、修改预算和重投影必须在事务内验证整树，不能让两个 sibling 各自认为还有同一份额度。

树预算使用稳定持久顺序分配；公平性不是安全保证。超过深度、出现 cycle 或无法求 root 时 fail closed。

Human task 与 Agent task 的配额语义不同；限制 Agent 自扩张不能顺带阻止用户明确创建工作。

## Template 与 fork

Workflow template 是普通 template wave 的 report。创建 wave 时复制报告，不建立持续引用：

- 新 wave 后续独立演化；
- task block 的归因和人工放行状态按 fork 规则归一化；
- `neige://` 引用改写到 fork 后实体；
- workspace 不从模板继承；
- 模板变化不追写正在运行的 wave。

模板被选中是一项用户产品决定；Spec 可以建议，但不能静默替用户换模板。

## Scheduler 边界

Projection 只决定“当前有哪些合法声明”。Scheduler 决定“哪些 pending task 现在可以运行”。Operation/gate 决定“如何可靠执行”。

三层不得互相替代：

- Projection 不 spawn worker；
- Scheduler 不修改 task block；
- Worker outcome 不直接改 report 声明；
- Gate 不成为计划编辑器。

执行状态机与恢复规则见 [644-plan-then-schedule.md](644-plan-then-schedule.md)，parked 外部工作见 [653-parked-operations.md](653-parked-operations.md)。

## 删除、取消与历史

- 删除 pending task：移除可执行 projection，并按作者规则留下或清除墓碑。
- 删除 in-flight task：不能只删 row；必须走 cancel/withdraw 协议，保留执行历史与诊断。
- Terminal task row 不因文档重建消失。
- Fork/模板复制声明，不复制原 wave 的运行身份。
- Wave/area 删除仍遵守 operation、workspace 和审计边界。

## 失败与 sweep

正确性不能依赖一次 report edit hook。需要周期和 boot sweep 处理：

- refs/index trigger 丢失；
- 投影暂时失败；
- task 已 claim 后文档变化；
- child tree 关系变化；
- runtime/operation 已终态但 task 未收敛。

瞬时读错不应立即制造不可逆误杀；连续失败升级为诊断或 fail-closed 终止。Sweep 自身停摆必须有健康信号。

## 主要风险

- 一次大规模文档重写可能使大量引用同时失效，引发 fail-closed 终止。
- Claim 提交到 spawn 之间仍有极窄窗口；任何改变这一窗口的工作属于 operation driver 改造。
- 误判 material change 会不可逆终结一次执行，必须持续观测误报率。
- 同毫秒创建的 sibling 若只靠随机 id 决定余数份额，结果稳定但不具有人类可预测的公平性。
- 删除父 wave 与并发创建 child 的竞态必须由数据库事务 guard 兜底，route 前检只改善错误体验。
- 父 wave cancel 不自动等于整个子树 cancel；产品必须提供清晰的树级操作或逐项恢复路径。
- 配额只约束同时占用，不天然限制 Agent 一生累计创建多少任务。

## 不变量测试

- 任意编辑序列后，incremental projection 与全量 rebuild 字节一致。
- 人/Agent 权限、归因、墓碑和放行不能被其它写入口绕过。
- Duplicate key、cycle、missing ref、cross-area 和闭包超限都 fail closed 且给可行动诊断。
- Claim 后修改根块或引用闭包会阻止旧上下文继续成为有效结果。
- 只改非 material 字段不会误杀执行。
- Rebuild 不改写 in-flight/terminal 状态和 worker identity。
- Fork 不携带人工放行、旧 workspace 或旧 operation identity。
- 并发修改 sibling/tree budget 不会超卖容量。
- Lost trigger、进程重启和 sweep 得出相同 projection/withdrawal 结果。
- 用户看到的 task 状态、诊断和操作均来自真实 report + projection 链路。
