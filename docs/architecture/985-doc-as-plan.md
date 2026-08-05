# 文档即计划：`task` 块为声明真源，`tasks` 表降为投影（#985）

> **本文状态**：设计定稿。经 8 轮评审（r1–r8g，双通道：codex + 独立 subagent）收敛后
> **全文重写**——正文只陈述最终裁决，推翻史与评审处置记录移入附录 A / B。
>
> **代码基线**：`a6de2260`（切片 3b 合入后的 main）。切片 1/2/3a/3b 已合入；
> 本文的施工目标是**切片 3b′ 及其后**。
> 本文**刻意不带 `file:line` 锚点**（八轮修订已证明行号会随实现漂移、且漂移
> 不可见）；引用一律用**符号名**（函数 / 列 / 常量 / 类型），施工时按名检索。
>
> **伴生文档**：`644-plan-then-schedule.md`（`tasks` / scheduler / gate 的现状事实源）、
> `955-kernel-app-boundary.md`（能力边界判据、单写者原则）、
> `653-parked-operations.md`（parked 原语的真实能力边界）、
> `terminology-glossary.md`。

---

## 阅读指引

本文回答四个问题：

| 章 | 问题 |
|---|---|
| §1–§2 | **声明该住在哪** —— 边界判据 + 声明/状态分栏（承重墙） |
| §3 | **`task` 块长什么样** —— kind 归属、payload、身份、写口收口 |
| §4–§5 | **投影与失效怎么办** —— 同事务投影、rebuild、引用闭包、三级阶梯、判决强制点 |
| §6–§9 | **人机不对称、模板、预算、迁移** |
| §10–§12 | **怎么验、怎么切、还剩什么风险** |
| 附录 A | 裁决记录（每条含「什么能推翻它」）|
| 附录 B | 评审处置历史（含驳回反证与方法论）|
| **附录 C** | **载体与常数总表** —— 施工时逐行核对，新增机制必须登记 |
| **附录 D** | **不变量与切片验收清单** —— 少一行就是少一件事 |

**一条贯穿全文的格式纪律**（八轮评审最贵的教训）：**每引入一条机制，必须填满
「载体 / 谁写 / rebuild 怎么重放 / migration 怎么 backfill」四格。** 四格有一格
空着，就是一条迟早会炸的空洞规则。附录 B 记录了这条纪律是怎么用四个 BLOCKER
换来的。

---

## 1. 主张与边界

### 1.1 主张

> **文档是「声明」的唯一真源。** 人和 spec 在 wave-report 里写下 `task` 块；
> 内核在报告写事务里把它投影成 `tasks` 行并据此调度。`calm.plan.upsert`
> 从 spec 工具面退场 —— **agent 只剩一个写口：文档**。
>
> **workflow 拆成两半**：可编辑的那半（plan 骨架 / spec 指令 / 顾问式 gate 说明）
> 变成 **report 模板**，人看得见改得动；不可搬的那半（git/forge 工具与执行语义、
> plugin 自有卡类型）留在 plugin，走既有的 ③ 通道。

报告因此从「产出物」变成**人与 spec 共建的任务画板**：人写意图，spec 补齐可执行性，
内核撮合执行，worker 产出证据并反链回来。

### 1.2 为什么

**(a) 今天有两个写口，而人看不见的那个反而是权威的。** 任务声明分散在两处：
spec 通过 `calm.plan.upsert`（Spec-only）写 `tasks` 表，人只能在文档里写散文。
人想调整流程，得改 plugin manifest 再发版。

**(b) workflow descriptor 里五分之三本来就只是 prompt 文本。**
`WorkflowDescriptor` 的五个字段中，`gates` 明确标注 "Advisory, prompt-only gate
guidance — NOT an executable contract … NEVER executed as a shell command"，
只被渲染进 spec 系统提示；真正可执行的 gate 是 spec 按目标仓库工具链写进
`gate_json` 的那些。`plan_template` 与 `spec_instructions` 同理 ——
它们是**被锁进发版流程的文档**，不是机制。

**(c) 与单写者原则同构。** #973 确立了「报告只有一个逻辑作者 + 人可覆写」。
把任务声明收敛进报告，等于让这条原则**同时**管住产出与计划。

**(d) 收益不在存储统一，在写口统一。** 存储不可能统一（§2），但「人和 agent
只需要在一个地方写」可以做到，而塌缩掉的正好是人看不见的那个。

### 1.3 边界判据：这个东西最终会不会沉淀进记录

`955-kernel-app-boundary.md` §1.1 的三条判据回答的是「**能力放哪个平面**」；
本节回答的是「**事实放哪个载体**」。**两轴正交，本节不替代那三条。**
对本设计而言，三条决定归属：

| 判据 | 本设计的应用 |
|---|---|
| **写路径唯一** | 声明只经 `apply_report_op` 一条路（§3.7）；调度状态只经内核（§2） |
| **配额只能有一个负责人** | 预算（per-wave / 未结存量 / 树级）全部归内核，agent 无从绕过（§8） |
| **会沉淀进记录的属于声明** | 「是谁提出要做这件事」「验收标准是什么」会沉淀 ⇒ 住文档；「尝试了几次」「租约何时到期」不沉淀 ⇒ 住事件日志 |

### 1.4 非目标

- **不替换 codex-as-spec**（沿用 #760 的非目标）。
- **不把状态写进文档**（§2）。
- **不做 agent 抢单 / 常驻 agent 池**（§3.6）。
- **不引入第三个文档写者**：`task` 块由 spec + 人写，内核只在读路径贴状态。
- 不在本设计内解决 worker 级 human-in-loop（#830）与 workflow 组合语义（#761）。

---

## 2. 承重墙：声明与状态必须分开

| | 内容 | 唯一真源 | 写者 |
|---|---|---|---|
| **声明** | 任务是什么、依赖、验收标准、gate 该查什么 | **文档（CRDT）** | spec + 人 |
| **状态** | `pending` / `dispatched` / `running` / `verifying` / `done` / `failed` / `canceled`、尝试次数、租约、gate 结果、幂等键 | **事件日志** | 只有内核 |

**状态绝不能进文档**，四条理由，第 3 条是决定性的：

1. 内核会成为报告的第二个写者 —— #973 刚拆掉的东西；
2. 每次状态跳变要 load/save 整份 automerge BLOB 再发事件，文档退化成状态机日志；
3. **CRDT 的合并语义与状态机语义不兼容。** automerge 的设计目标是「永不冲突、
   总能合并」；状态机要的是「带前置条件的串行化事务」。并发编辑合并后可能出现
   「已完成的任务复活」「同一任务被认领两次」—— CRDT 不会报错，它会高兴地合并。
   这不是实现难度，是语义冲突；
4. operations saga 需要**提交时刻就 durable 的幂等键**，那东西不能住在一份会合并
   的文档里。

**精确表述**：文档是声明的唯一真源；状态的唯一真源是事件日志；
**`tasks` 表是两者的可重建投影**。这与项目既有取向一致 —— `cards.payload` 是
CRDT 的投影，已被 DROP 的 `proposals` 表也是事件的投影。

### 2.1 一处必须说准的既有事实

内核对 `task.dispatched` / `task.gate_result` 的写入约束（`role_gate.rs`）
**不是**严格 kernel-only —— 它们的实际语义是「非 AI、非 Plugin」，**`User` 也被
放行**。本设计新增的 `TaskContextFrozen` / `TaskContextAdvanced` 采用**严格
`Kernel | KernelDispatcher`**（`User` 也拒），是**例外而非同构**。

既有条款本设计不动（另案）；但**不把这份宽松传播到新面上** —— 伪造冻结集等于
植入假上下文，伪造判决等于伪造裁决结论。

---

## 3. `task` 块

### 3.1 kind 归属：内核拥有，plugin 不得定义

`task` 是**内核拥有**的块 kind，plugin 不得定义同名或等效 kind。
**就绪标记必须是 `task` kind 自己的字段**，不能是「任意块加 `ready: true`」——
否则任何 plugin 块都能变成调度输入。

**这是安全边界，不是风格问题**：否则 plugin 定义一个「看起来像 task」的块即可
间接驱动调度器，绕过全部 spawn 门禁。

### 3.2 payload schema

严格校验，未知字段一律拒绝。全集 **17 个字段**（`kinds.rs` 的 `TASK_FIELDS`）：

```jsonc
{
  "key": "impl-parser",        // 必填。^[a-z0-9][a-z0-9._-]{0,63}$。任务身份（§3.3）
  "kind": "codex",             // 必填。codex | claude | terminal
  "goal": "…",                 // 必填非空
  "acceptance": "…",           // 可选非空
  "gate": { "cwd": "…", "timeout_secs": 1800, "steps": [{"name":"…","cmd":"…"}] },
  "no_gate_reason": "…",       // gate 缺席时必填（wave 的 require_task_gates 打开时强制）
  "depends_on": ["setup"],     // 可选，同 wave 内的 key
  "priority": 0,               // 可选整数
  "cwd": "/abs/path",          // 可选绝对路径
  "context": { … },            // 可选，进 prompt
  "refs": ["neige://wave/w1#b_1f3a"],  // 可选，必须解析出块片段（§5.1）
  "ready": true,               // 必填 boolean。就绪判据（§6.3）
  "declared_by": "spec",       // 必填。spec | user。由写口强制（§3.7 规则 1）
  "released_by_user": false,   // 可选 boolean。人可写、spec 不可写（§6.6）
  "spawn": "in-wave",          // 可选。in-wave（默认）| sub-wave（§3.5，切片 6）
  "tombstone": { "reason": null },   // 墓碑形状，封闭（§6.1）
  "tombstoned_by": "user"      // 墓碑必填。spec | user
}
```

**尺寸上限**：单块 canonical bytes ≤ `MAX_CANONICAL_BYTES`（256 KiB）；
单个字符串字段 ≤ `MAX_STRING_CHARS`（2048）；`context` 的嵌套深度另有上限。

**墓碑是封闭形状（双向）**：`tombstone` 非 null 时，除 `key` / `tombstone` /
`declared_by` / `tombstoned_by` 外**其余字段必须全部缺席**；
**反向：`tombstone` 缺席时 `tombstoned_by` 必须缺席**。这条后来承担了一个
额外职责：它使墓碑覆盖时纳入集投影从非空变空 ⇒ `content_hash` 必变（§5.2 的收窄投影）。

**发布 schema 必须与 `validate_payload` 双向同步。** 项目里没有 JSON Schema
执行器，无法自动化防回归，因此在 task schema 旁写明这条纪律。

### 3.3 任务身份是 payload 的 `key`，不是 block id

**裁决：身份 = `key`；block id 只是引用锚。**

理由：block id 由启发式对齐铸造（`align.rs`），整文档重写下**不稳定**；
fence 不携带 id，所以写方无法指定。把安全语义挂在对齐启发式上是错的。

**`key` 同时是幂等键**：`UNIQUE(wave_id, key)` ⇒ `tasks.id = "{wave_id}:{key}"`
**就是 operation 的幂等键** —— 这才是「已派发过的 key 不复活」的真正理由，
不是策略选择。

推论：

- **改 `key` = 删旧建新。** 既有块上就地改 `key` 一律拒绝（§3.7 规则 6）。
- **删 in-flight 任务不走补偿路径**（#653 的 parked/compensating 机制不存在），
  改为记录撤回意图 + 可见待办（§6.5）。
- 复制粘贴产生重复 `key` → 投影产出 `duplicate key` 诊断、该块不可调度。

### 3.4 人的写口：块级 REST

人必须能不经 agent 直接编辑 `task` 块。四个端点 + 两种并发控制：

| 端点 | 并发前置 | 理由 |
|---|---|---|
| create | `if_doc_rev` | 块还不存在，块级 rev 守不住 |
| update | `if_block_rev` | 精确到块 |
| move | `if_doc_rev` | 改的是 order，块级 rev 不动 |
| delete | `if_block_rev` | 精确到块 |

**同一裁决同时适用于 MCP 侧 `calm.report.blocks.*`** —— 两条写口同一套并发前置。

**一处刻意的选择**：非 User 作者**可以移动**用户拥有的块。与仓库既有语义一致
（顺序不是内容，`move` 不升 rev），且位置式引用已被 §5.1 禁止。

### 3.5 执行粒度：默认不开子 wave

**裁决：默认 `spawn: "in-wave"`（沿用今天 scheduler → worker 卡路径，零改动）。
`spawn: "sub-wave"` 是显式选项，落在切片 6。**

成本已逐项算出（读自代码）：

| 项 | 一个子 wave |
|---|---|
| `waves` / `cards` / `overlays` 行 | 1 / **2**（spec 卡 + report 卡）/ 1 |
| 创建时事件 | **4**；一个最简 wave 全程 **9–11** 个 |
| CRDT 文档 | **1 份新的**（`cards.body_crdt`） |
| `wave_vcs` | **一条独立 commit 链** + 一行 `wave_vcs_refs` |
| 活的 spec agent | **1 个常驻 in-process Tokio 任务** + 共享 codex daemon 上的 1 条 thread |

对比**在本 wave 内跑一个 task**：1 个 `tasks` 行 + 1 张 worker 卡 + 若干事件 ——
没有第二个 spec harness、没有第二份 CRDT 文档、没有第二条 vcs 链。

**一处必须说准的更正**：子 wave **不会**多开 OS 进程 —— 自 #410 起全内核只有
**一个** `codex app-server`。边际成本是**一条独立的 LLM 会话上下文 + 一个常驻
Tokio 任务 + 一整套 CRDT/vcs/session/token 行**。

**可证伪**：若 in-wave 执行导致父报告被 worker 产出淹没到无法阅读，
或 per-wave `task_budget` 成为无法通过配置解决的吞吐瓶颈，则本裁决被推翻。

### 3.6 认领 = 内核撮合，不是 agent 抢单

执行体是内核按需实例化的 spec/worker，**不存在常驻 agent 池**，因此没有抢单的
位置。能力声明由模板承载，撮合、预算、幂等、审计全部归内核 —— 这是 §1.3
判据 1 与判据 2 的直接后果。

角色分工（最容易被误读的一条，写在这里以正视听）：

| 角色 | 职责 |
|---|---|
| **人** | 写意图（粗略 task 块）、墓碑否决、`released_by_user` 放行、选模板 |
| **spec** | 把意图**补齐成可执行的声明** —— 补 `acceptance`、写 `gate`、拆依赖，最后打 `ready: true` |
| **内核 scheduler** | 撮合：算 ready 集 → claim → 冻结引用闭包 → 派 worker 卡 |
| **worker** | 实现，产出证据并反链回来 |

**spec 不领取、不派发。** 领取的是 `Scheduler::claim_task`，actor =
`ActorId::KernelDispatcher`。

### 3.7 `guard_task_declarations`：所有写路径的唯一收口

**这是本设计的安全地基。** 起因是一条既有事实：`calm.report.write_markdown`
**不受 stomp guard 约束**（只有 `Replace` 走它），因此「块级工具的入口校验」
作为安全边界**整体失效** —— spec 今天就能任意改删任何非 prose 块。

**机制是两个函数，不是一个**：

- **`normalize_report_op`** —— 应用**前**的 op 改写器。承担规则 4（人删除活任务
  → 原位墓碑），因为那是**变更**不是判定，返回 `Result<(), _>` 的校验器做不到。
- **`guard_task_declarations`** —— 前后态校验器，在 `apply_report_op` 内对
  **每一个** op 变体运行。

**八条规则**：

| # | 规则 |
|---|---|
| 1 | 新出现的 `task` 块，其 `declared_by`（墓碑块则是 `tombstoned_by`）**必须**等于本次编辑的 `EditAuthor`；**其它任何 author（`Kernel` / `Plugin`）新建 `task` 块一律拒绝（fail closed）**——两者今天没有生产发射点，但 guard 不能在其中之一回归时行为未定义 |
| 2 | 既有块上 `declared_by` **永久冻结**，任何作者不得改变 |
| 2b | 已存在的墓碑块 `tombstoned_by` **不可改**；且墓碑块**不得原位改回非墓碑**（撤回 = 删除该墓碑块）。否则 spec 一次原位改写就能把人的否决变成自己的任务，**绕过规则 3** |
| 3 | **`author != User`** 时，不得删除、也不得修改任何满足 `declared_by == "user"` **或** `tombstoned_by == "user"` 的 `task` 块（**只能移动**）。**第二个析取项是必需的**：人否决 spec 声明的任务后，墓碑的 `declared_by` 仍是 `"spec"`，只靠第一个析取项 spec 可以直接删掉墓碑再重提，死循环原地复活。**主语是 `!= User` 而不是 `== Spec`**，与规则 1 同一形状同一理由 |
| 4 | User 删除一个活的 task 块 ⇒ 改写为**原位墓碑**（`tombstoned_by: "user"`）|
| 4′ | **主条款**：`author == User` 时，`before` 里任何非墓碑 `task` 块若在 `after` 中消失，则 `after` **必须存在同 `key` 且 `tombstoned_by=="user"` 的墓碑**，否则拒绝，**错误文案指向块级 DELETE 端点**（规则 4 只覆盖块级 DELETE，整文档路径必须 fail-closed）。**补丁**：满足它的墓碑必须是**本次编辑新立的**。live → tombstone 时 `tombstoned_by` 必须等于本次编辑的 author：没有它，spec 能伪造人的否决（终局、不可撤回，且规则 3 之后对 spec 永久锁死），人也能自废 §6.1 的防线。**且不得被一个早已存在的同 key 用户墓碑满足**——否则 fail-closed 兜底在它真正要生效的那天不成立 |
| 5 | `released_by_user` **只有 `EditAuthor::User` 能写入或改变** |
| 6 | 既有块上 `key` 不可就地改写（§3.3）|

**收口的完整性**（逐条查过绕过面）：`apply_report_op` 只在
`persist_report_with_shadow` 里被调；`card_update_tx` 对 wave-report 的 payload
写一律 400；`card_update_with_crdt_tx` 只接受 wave-report 且唯一生产调用者是
`wave_report.rs`；`card_create_with_id_tx` 强制 wave-report 必须是 kernel-minted
的 canonical initial payload；REST 块级写口经 `require_rest_user_actor`。
**fork 路径见 §7.2**（它不经 `apply_report_op`，自带三条补偿责任）。

### 3.8 状态回显：读时合成，不进文档

`task` 块渲染时，读路径把 `tasks` 行的状态（status / gate_result /
worker_card_id）贴在块上；`gate_result` 在服务端收窄为 `{passed, failing_step?}`，
不向浏览器或 MCP spec agent 暴露日志路径与原始输出。这是**读时**行为，不产生文档写、
不产生第三个写者。

**一条弱约束**：状态回显**不得引入任何新的读时数据源抽象**，就是投影表的直读。
这样 #976（活数据块）落地时无论选什么形状都不会与它冲突。

**#1016 合并旁注**：状态回显所需的 `tasks` 列与调度诊断核心状态合并进
`wave_projection_state` 的同一条 SQL，通过相关子查询一次返回。这样既不恢复会持锁
跨表 park 的多语句 deferred 读事务，也不承受拆成两次独立读所带来的状态比诊断新
一拍的可见性偏移；状态回显与该条 SQL 中的容量、in-flight 信息来自同一快照。

---

## 4. 投影：契约与重建

### 4.1 同事务投影

`persist_report_with_shadow` 今天已在同一事务内更新 CRDT + JSON payload 投影 +
发 `CardUpdated`/`WaveReportEdited`。「顺带 upsert `tasks` 行」是这条路径的
**同构扩展**。

**不做异步 reconcile**：那会引入一整类「文档改了但 plan 没跟上时调度器读到旧值」
的 bug，同事务直接消灭。

### 4.2 投影函数的六条规则

`project_tasks_tx(tx, wave_id, blocks) -> TaskProjectionOutcome`。

| # | 规则 |
|---|---|
| 1 | **声明消失 ⇒ 守卫式删除**（`DELETE … AND status='pending'`）。四种触发：块被删除 / 被墓碑覆盖 / `ready` 从 true 撤回 / 该块新产生了诊断 |
| 2 | **非 `pending` 行的声明内容不再更新**，只产出诊断；例外是收养与 `0070_` backfill 对 `decl_*` 影子位的**初始化**，它不改变 worker 可见规格。**已派发过的 `key` 不复活** —— `tasks.id = "{wave}:{key}"` 就是 operation 幂等键（§3.3）|
| 3 | **可调度谓词是唯一的**：`schedulable = ready ∧ ¬tombstone ∧ diagnostics.is_empty() ∧ 准入`（§8 的 ceiling）|
| 4 | **诊断零存储、零缓存、零事件** —— 读端在读事务内派生的标注 |
| 5 | **`kernel_events` 必须被调用者消费**（§5.4）|
| 6 | **原子性**：投影与报告写在同一事务；投影失败整体回滚，报告写也不落。**诊断不是失败** —— 诊断走渲染，不走回滚 |

**规则 1 的列级契约**（施工基本面）：

- **声明列**（可被覆盖）：`kind` / `goal` / `context_json` / `acceptance_criteria` /
  `cwd` / `depends_on_json` / `priority` / `gate_json` / `declared_by` / `origin`；
- **绝不触碰**：`status_detail` / `worker_card_id` / `gate_result_json` /
  `gate_attempt` / `gate_pid*` / `running_deadline_ms` / `finished_at_ms`；
- `status` 只在建行时写一次。
- **守卫式删除返回 0 行不是错误** —— 意味着该行在读投影与删除之间被 scheduler
  认领了（`task_claim_pending_tx` 同样 `WHERE status='pending'`）。按 §6.5 处理：
  不改状态、产出「正在执行，无法立即撤回」诊断。

**规则 3‴：`unknown_deps` 的入参只含在飞行。**
第二个入参 = 该 wave 内 `status IN ('dispatched','running','verifying')` 的行派生
出的 key 列表，**`pending` 行的 key 一律不在其中** —— 与规则 3 的 `occupied` 是
同一条判据的同一次应用（**`pending` 行永远是输出，在飞行永远是输入**）。
不收窄则同一事务内会分叉 ⇒ rebuild ≢ 增量。
**必须专列进切片 3b 的迁移验收**：一条 `depends_on` 指向 `origin='legacy'` 的
pending 行会**突然产生 `unknown_deps` 诊断** —— 这是存量库上线当天唯一会出现
「突然多出来一批诊断」的地方。

**等价性属性测试**（把「投影不得比 `calm.plan.upsert` 松」从散文变成机制）：
> `resolve_plan_batch(existing, B).is_err() ⟺ project_task_declarations(blocks_of(B))
> 的诊断集合非空`（规则 2 那一类除外）。

**规则 1 为什么是删除而不是 `pending → canceled`**：`canceled` 是非 pending
⇒ 规则 2 会**永久吸收**该 key，人删一次任务就再也无法重提，而 §6.1 明确承诺
可以。删除同时让 §10.1 的 rebuild ≡ 增量第一次真的成立。

**规则 4 为什么零存储**：诊断写进文档 = 内核成为文档的第二个写者，破 §2 与 §1.4。
读时派生同时保证了「删行的原因永远算得出来」—— 这是本设计在可发现性上最正确的
一个决定。

**规则 3 的准入必须幂等**（这一处已经被写错过两次，见附录 B）：

> `occupied` **只数 `status IN ('dispatched','running','verifying')`**，
> **`pending` 行一概不数** —— 因为 `pending` 行是本次求值的**产物**，
> 把产物数进输入会让投影函数不幂等 ⇒ rebuild ≢ 增量 ⇒ 击穿 §10.1。
> 准入顺序：块在文档中的顺序，同序时按 `key` 升序，取前 `capacity` 个。

### 4.3 事件

一次报告写恰好 **3 个**事件（声明无变化时 2 个）：
`CardUpdated` → `WaveReportEdited` → `PlanUpdated{changed_keys}`，同一事务、
同一 wave scope。

**`changed_keys` 的契约**：

> **插入的 key ∪ 声明列被更新的 key ∪ 被删除的 key**，排序去重。
> 「被删除」**含规则 1 的全部四种情形**；**仅产生诊断而未写任何行的 key 不进**。
> **`changed_keys` 为空则不发** `PlanUpdated`。

**必须包含删除**，否则一次纯撤回编辑一个事件都不发 ⇒ **丢一次 dispatcher poke**。

**Tier-A 必做项**：`Event::PlanUpdated` 的 doc comment 今天把它定义为
`calm.plan.upsert/cancel` 的 key 且写着「Spec-only」，**两句都要改**。

### 4.4 归因：`declared_by` 住文档

`declared_by` 住块 payload，`tasks.declared_by` 降为它的**投影副本**。

理由是 §1.3 判据：「是谁提出要做这件事」会收敛进记录 ⇒ 它是声明。
把它放在投影列里会导致 rebuild 重建不出、§8 预算失守。

正确性**不由纯函数保证**（纯函数看不到写者是谁），由 §3.7 规则 1 强制、
规则 2 冻结。

---

## 5. 失效检测：引用闭包、冻结、判决

这一章是本设计最硬的部分。它回答：**一个已被认领的任务，其上下文变了，
系统凭什么保证不让 agent 按旧标准跑完。**

### 5.1 闭包：stale 的作用域不是单块

`task` 块的 `goal` / `acceptance` 本身就是 prompt。但上下文应当**引用**而非复述。
若 `key=impl` 的块写着「按 `b_1f3a` 的方案实现」，**有人改了 `b_1f3a` 时该 task
块自己的 rev 不变** —— 基于单块 rev 的失效检查会完全漏掉。

**两条硬规定**：

- **引用必须是 id 式的**：`neige://wave/<id>#b_xxxx`。位置式表述（「按上面第 3 节」）
  **禁止** —— 位置漂移让声明静默变义而 rev 检测不到。机制上：`refs[]` 每一项都过
  `parse_destination` 且**必须解析出 `dst_block_id`**，否则该块不可调度。
- **`goal` / `acceptance` 正文里的 `neige://…#b_xxxx` 也进闭包**，用同一个
  `scan_links`。**机制上**：`report_backlinks` 的 `filter(kind == KIND_PROSE)`
  必须放宽为「prose + 内核声明为可扫描的 kind」，且**扫描的是该 kind 声明的
  文本字段**（对 `task` 是 `goal` / `acceptance`），**不是 fence 的 canonical
  JSON —— 否则 JSON 转义会让 markdown 链接语法解析失败**。
  不带块片段的整 wave 链接**不进闭包** —— 它没有可比对的 rev，
  进了只会产生永远无法收敛的 stale。

**失效检测有三个正交维度，补救手段不能互换**：
**① 内容**（一个冻结的内容能不能被改变而不被发现）—— 枚举写路径可以承载结论，
因为写路径**有限、静态可穷举、且都汇于 `apply_report_op``；
**② 观测**（谁保证有人去看）—— 枚举运行时投递**没有封闭性**，只能靠 fail-closed 重扫；
**③ 执行**（看过之后凭什么执行判决）—— 靠 `prepare_tx` 的必经漏斗。
维度 ① 在**纳入字段集上**已穷举关闭，表末的 8 个排除字段是**刻意开的口**（§5.2）。
**未来复审触发器：automerge 若将来开启 sync，维度 ① 的 CRDT merge 那一行必须重审**
（今天全仓唯一的 `.merge(` 在单测里）。

**`move_block` 不构成漏报**：`move` 既不动 rev 也不动内容，因此不改变任何被引用块
的 `content_hash`；而位置式引用已被上面第一条禁止并由机制强制。

**闭包的根是 task 块自身**（深度 0，计入 `MAX_REF_NODES`）。否则编辑一个
in-flight 任务自己的 `goal` / `acceptance` / `gate` **不触发任何失效判定** ——
而那恰恰是本设计要解的核心需求。

**闭包不得跨 cove**：只在同 cove（+ system cove）内解析，与反链同一条边界。
越界节点按「解析不到」处理。理由：第 2 级裁决会把被引用块的**内容**递给任务所在
wave 的 spec，项目里每一处跨 wave 的读时派生都是刻意 cove 内的。

**传递闭包 + 双预算 + 耗尽即 fail-closed**：`MAX_REF_DEPTH = 3`、
`MAX_REF_NODES = 64`。任一预算耗尽 ⇒ 该任务标记 `closure_truncated`，此后它闭包内
**任何** wave 的**任何**编辑一律按 `material` 处理，并产出诊断。
**预算耗尽不阻止派发**，只把该任务降级到最保守的失效判定。
**可证伪**：若 `closure_truncated` 的比例高到让第 2 级裁决成为主要成本项，
说明预算太小、或「引用而非复述」这条指导本身有问题。

> **`closure_truncated` 的 fail-closed 必须同时落在事件路径与 sweep 上。**
> 只落事件路径会让**正确性载体比延迟优化更宽松**（严格程度倒挂），
> 且违反「事件正常 / 事件被丢弃两个变体必须给出同一结论」。
> 为什么它是漏报而非宽松选择：`closure_truncated = true` 的定义就是
> 「闭包里有块**没被冻结**」，那些块的编辑对逐元组比对**结构性不可见**。

### 5.2 冻结集：四元组存储，三元组判定

**冻结集 = 闭包的 `(wave_id, block_id, rev, content_hash)` 四元组集合，
但相等式只比 `(wave_id, block_id, content_hash)` 三项。**

#### 为什么必须有 `content_hash`

只冻 `(block_id, rev)` 有两个**构造性的、日常发生的**反例，各自足以单独击穿
「不允许漏报」：

- **块 id 会被回收**：`reassign_ids` 只用存活块 id 预置 `used`，被删块的 id
  立即释放，而全新切片 `rev = 1`。于是：冻结 `(b_1f3a, 1)` → 一次整文档重写删掉它
  → 一个**毫不相干**的新块铸出 `b_1f3a`, `rev = 1` → 机械检测比对相等，报告
  「没变」。**而 `rev = 1` 恰恰是最常见的冻结值。**
- **rev 自增是饱和的**：`saturating_add(1)` 在 `u32::MAX` 处内容变而 rev 不变。

`content_hash = sha256(canonical_flat_text)`，而 canonical flat text **已经**被
对齐器每次写都算了 —— **这份哈希的计算是白拿的**，唯一存储成本是每引用 32 字节。

#### 为什么 `rev` 不得进入相等式

`rev` 只用于**诊断文案**。**不得以 rev 相等做任何短路。**

`align.rs` 的不变量说的是「一个**存活且匹配上**的块，规范文本相同则 rev 不变」，
它**不蕴含**逆命题「`(block_id, rev)` 相同 ⇒ 同一个块」。若把 rev 纳入相等式或
用作短路，两个后果：

- 纳入相等式 ⇒ **编辑被引用块 → 撤销回原文**（rev 3→4→5，内容逐字节相同）被判
  `material` ⇒ 不可逆终结。第 2 级裁决**救不了它** —— 递给 spec 的 diff 是空的。
- 用作短路 ⇒ 上面两个 id 回收 / 饱和反例原样复活。

而短路**买不到任何东西**：要读到 current rev 就必须先跑 `load_block`
（`wave_get` + `cards_by_wave` + 逐块反序列化），哈希相对那次 DB 读是噪声。

#### 根块的哈希只算 9 个字段

> **根块（深度 0）的 `content_hash` 只对 `kind` / `goal` / `acceptance` /
> `gate` / `no_gate_reason` / `depends_on` / `refs` / `cwd` / `context`
> 九个字段的 canonical 投影计算。**
> **排除 8 个**：`key`、`priority`、`declared_by`、`spawn`、`tombstone` /
> `tombstoned_by`、`ready` / `released_by_user`（后两者改由 §5.3 的撤回规则承载）。
> **9 + 8 = 17 = `TASK_FIELDS` 全集** —— 由集合相等元测试强制。
> **深度 ≥ 1 的被引用块不做任何收窄**：它们可以是任意 kind，内核无从判断哪个字段
> 「进 prompt」，整块哈希是那里唯一站得住的判据。

**canonical 投影的定义**：取块 payload 的 object，保留纳入集的键（**缺席与 `null`
视为等价、一律省略**），复用 `fence::canonical_json` 的规范化（排序键、定格缩进），
**不含 kind 头**。

**为什么收窄**（端到端因果链，不等实测）：整块哈希 ⇒ 改 `priority` 命中 ⇒
切片 4 之前**全判 material** ⇒ 写 `context_stale_at_ms` ⇒
`refuse_if_context_stale` 拒绝该任务上一切尚未越过 `prepare_tx` 的 operation
（**gate 也在内**）⇒ 行落 `failed` ⇒ §4.2 规则 2「已派发过的 key 不复活」⇒
**必须换 `key` 重开**。即：**「我把一个正在跑的任务的优先级从 0 改成 1，
它就永久失败了，而且不能用原来的名字重开」** —— 每一环局部论证都对，
端到端是使用者不可能预测的。

**排除的 8 个字段为什么不削弱安全性质**：`priority` 只影响 ready 集内取序；
`declared_by` 由 §3.7 规则 2 在**写口**上永久冻住（理由是「写口不可变」，
不是「投影会处理」）；`key` 与墓碑各有专门路径；`ready` / `released_by_user`
由撤回规则覆盖。**`context` 保留在哈希内** —— 它进 prompt，改它就是改规格。
**根块的 `block.kind == "task"` 由写口守卫承载，不由哈希承载**（收窄投影不含
kind 头，这条依赖必须写出来）。

**`spawn` 的排除是版本化的，不是永久的**：它今天无执行消费者，
**切片 6 让它获得执行语义之前必须重新裁决**。集合相等元测试保证「忘了分类」会红，
但**不会**提醒「分类需要改变」—— 这条只能靠 §12 的记录。

**可证伪**：若出现「改了 8 个排除字段之一、其撤回方向也未被撤回规则覆盖、
而 worker 确实应当停下来」的真实场景 ⇒ 退回整块哈希。

#### 冻结根的载体：claim 前按 `key` 定位

**不新增 `tasks.block_id` 列。** block id 由启发式对齐铸造、整文档重写下不稳定
（§3.3）；持久化一个会漂移的锚，等于把安全机制挂在对齐启发式上。

1. claim 前，用 `(wave_id, key)` 在该 wave 当前报告的块快照里定位那个
   `kind == "task"` 且 `payload.key == key` 的**存活块**，取其 `block_id` 作根。
2. **claim 前定位失败一律不下判决。** 瞬时与确定性失败同等按 race-lost 处理：
   不写事件、不写 `context_stale_at_ms`、行留在 `pending`，下一轮重来，**完全可逆**。
   两类只在可观测量上有区别：按 `ResolveError` 变体分桶的计数器 + 一条 `warn!`，
   不产生任何持久状态。确定性成因由投影既有的守卫式删除承载；该路径对应的事件是
   既有 `PlanUpdated{changed_keys}`，而不是 claim 路径凭空制造一条没有变更集的判决事件。

   **初稿为什么写反了**：把 in-flight 行的 fail-closed 纪律机械套到了仍为 `pending`
   的行上，但这里缺少判决成立所需的三个载体：(a) 没有冻结集就没有
   `changed_refs`，而 §5.4 明定它是 `TaskContextAdvanced` 的唯一存在理由；
   (b) §5.5 全量 sweep 的枚举源是 in-flight 行，够不到 `pending`；(c) 投影 upsert
   不清 `context_stale_at_ms`，且行永不回 `pending`，所以人把引用修对也无法复活。

   **四格**：载体 = 无，这正是本条的价值；谁写 = 无人写持久状态，确定性成因由投影
   守卫式删除写 `tasks` 当前投影并发既有 `PlanUpdated{changed_keys}`；rebuild = pending
   行是文档的纯函数，天然一致；migration = 不需要新列，但需要一次性、幂等地执行
   `UPDATE tasks SET context_stale_at_ms = NULL WHERE status = 'pending' AND context_stale_at_ms IS NOT NULL`，只碰 pending、
   不产生事件——该状态在已发布版本不可达，仅防御曾含 claim 侧误写的未合并开发分支。
3. `origin='legacy'` 的行跳过定位，冻结集为空集。
4. 闭包**展开**在 claim 事务之外的普通读里完成，**但根块与各 wave 的 `doc_rev`
   必须在 claim 事务内复核**（§5.2 栅栏）。

`task_claim_pending_tx` **不得**带 `context_stale_at_ms` 谓词：腿 1 后没有写者能让
pending 行带上该列（`mark_context_material_tx` 自带 in-flight 守卫，且行永不回
pending），该谓词因而是会诱导后来者补写者的空洞不变量。material 行本已被
`status = 'pending'` 排除；判决强制点只有 §5.6 的四个 `prepare_tx`，没有第五条。

> **这条分类要求 `ResolveError` 先拆变体。** 现有枚举只有
> `Missing / CrossCove / InvalidReference / Storage`，而 **`Missing` 一个变体同时
> 承载两类** —— wave 不存在、报告卡不存在、`blocks` 字段缺失、块不存在；
> 更糟的是单块反序列化失败被 `.ok()?` **静默吞掉**，最终伪装成「块不存在」
> ⇒ 损坏被静默伪装为块缺失，导致失去独立的损坏分桶。
>
> **拆成**：retryable = `StorageUnavailable`；
> deterministic = `RootAbsent` / `RootTombstoned` / `DuplicateLiveKey` /
> **`ReferencedWaveAbsent` / `ReferencedBlockAbsent` / `ReportAbsent`** /
> `MalformedStoredReport` / `CrossCove` / `InvalidReference`。损坏的持久报告不会
> 自行恢复；按 retryable 处理只会把同一个判决推迟 3 轮，期间该行仍占用
> in-flight 名额。§5.5 的「瞬时失败」专指**存储/IO 不可用**，反序列化失败
> 不属于它。反向代价是：一次瞬时不可解析（例如撕裂写入，或 schema 回滚后旧
> 二进制读取新 payload）会立即且不可逆地把受影响的 in-flight 行判为 material。
> **非根引用的缺失必须有自己的变体**，
> 否则实现者会把所有 missing 塞回 `RootAbsent`，只是换个名字。
> **分类按变体匹配，禁止按错误字符串推断。** 这项分类在 claim 前**只影响计数器
> 分桶**，所有变体仍一律 race-lost；per-row 连续失败升级只属于 3b′-ii 的
> **in-flight sweep**，不得回灌到 pending claim 路径。

#### claim 栅栏：关掉「解析 → claim」的 TOCTOU

「窗口内的竞态只会让系统更保守」这条论证**对「最终会发现」成立，对「禁止启动」
不成立** —— 而判决语义恰恰是后者。反例：

> scheduler 解析闭包 → 被引用块被改，`WaveReportEdited` **已处理完** →
> 此时 `task_ref_index` 里**还没有**这个任务（索引行在 claim 的 UPDATE 成功之后
> 才插），事件路径**结构性不可见** → scheduler 用**旧闭包**完成 claim、建索引、
> **立刻**驱动 worker → 最早等下一轮 sweep（默认 300 秒）。

> **裁决：claim 事务内、状态翻转之前，复核**
> **(a) 根块的 `(block_id, content_hash)`，以及**
> **(b) 闭包中出现的每一个不同 `wave_id` 的报告 `doc_rev`。**
> **任一不一致 ⇒ 回滚 claim，race-lost 重来。**

**为什么按文档而不是按节点**：`doc_rev` 的作用域是**单个 wave-report 文档**，
所以同 wave 的子节点被顺带覆盖；而闭包**按设计允许跨 wave**，跨 wave 子节点必须
各自有一个 `doc_rev`。N = 闭包涉及的不同 wave 数，通常 1–3，远小于
`MAX_REF_NODES`。进事务的只有 N 条定向 `SELECT` + N+1 项比较。
**可行性事实**：`doc_rev` **已镜像进 `cards.payload` JSON**（在同一个写事务里
与 payload 一起提交），所以能在 claim 同事务里用**普通 `SELECT`** 读到，
**不需要加载 automerge 文档**；且**镜像值与 CRDT 真值无不同步窗口**。
另两个已复核的非窗口：**同一 wave 在闭包出现多次 ⇒ 按 wave 去重即可**；
**报告卡删除重建导致 `doc_rev` 归零 ⇒ 归零只会小于冻结值、仍是 mismatch**（保守方向）。

**三件事必须写死**：

1. **采集时序** —— **每个 wave 的 `doc_rev` 必须在读该 wave 的第一个块之前采集**。
   反过来做（先读块、后补读 doc_rev）会让两者之间落的一次写被记成**新** `doc_rev`，
   栅栏比对通过而闭包是旧的 —— TOCTOU 原样复现，只是窗口更窄。
   反向误差（先采后读）只会造成保守的 race-lost，是安全侧。
2. **缺失即不一致** —— 窗口内 wave 或报告卡被删 ⇒ 栅栏 `SELECT` 返回 `None`
   ⇒ **视为不一致、race-lost**。不能写成 `unwrap_or(frozen)`。
3. **使用边界** —— **`doc_revs` 只用于 claim 提交前的栅栏比较，永不进入
   `refs_match` / sweep / detect 的相等式；不一致只产生 race-lost，
   绝不形成 post-claim 的 `material`。** 方向是灾难侧：`doc_rev` 随该 wave 的
   **任意**报告写自增，若并入 sweep 判据，**该 wave 一次编辑 ⇒ 全部引用它的
   在飞任务判 material**，当场推翻 9/8 哈希裁决。

**载体（四格）**：

| 载体 | 谁写 | rebuild 怎么重放 | migration |
|---|---|---|---|
| `doc_revs` **只进 `TaskContextFrozen` 事件 payload**，不落任何 `tasks` 列 | claim 事务（`task_claim_pending_tx` 增一个参数，仅用于事件构造）| 事件即真源，无需重放到列 | **无**（不落列）|

> **为什么不落列**：栅栏的判据来自本轮内存中的 map，且被硬裁为永不进 sweep/detect。
> 一列纯写不读的持久状态会连带一条「`NULL` 不可解释成不一致」的规则，
> 而那条规则**结构性不可达**（没人读，NULL 不可能被解释成任何东西）——
> 一条典型的空洞不变量。事件已是冻结集的真源（§5.4），取证能力不丢。
>
> **栅栏因此是纯粹的事务内一次性比较：判据只来自本轮内存中的 map，
> 没有任何持久状态参与判定。**

### 5.3 撤回规则：`ready` / `released_by_user` 不走哈希

**病因**：投影的守卫式删除只赢 `pending` 行，对 `dispatched/running/verifying`
行**不删不改，只追加撤回诊断**。于是若不处理这两位，会出现一个刺眼的不一致 ——
**人把在跑任务的 `ready` 改成 `false` → 什么都不发生；人删同一个块 → material、
任务停。同一个撤回意图，两条路径行为相反。**

**但修法不是把它们塞进哈希。** `auto-declare`（默认策略）下 `released_by_user`
对可调度谓词**毫无贡献**，任务通常带缺省 `false` 被 claim；此后人把它置 `true`
（**这是人唯一被允许写的那个位**）⇒ 哈希变 ⇒ 永久 `failed`。
**这与排除 `priority` 的理由完全同构。**

> **撤回规则**：`project_tasks_tx`（**写路径**）在处理**非 `pending`** 行时，若
> `row.decl_ready == 1 ∧ decl.ready == false`，或
> `row.decl_released_by_user == 1 ∧ decl.released_by_user == false`
> **∧ 该 wave 当前的 `effective_policy == declare-and-wait`**，
> 则与「块被删除 / 被墓碑覆盖」走同一条路径：
> 同事务写 `context_stale_at_ms` + 发 `TaskContextAdvanced{material}`。

**四格**：

`context_resolve_failures{variant="malformed_stored_report"}` 必须进入生产 health
snapshot；该桶任一非零增量立即告警（验收：注入一份损坏持久报告，首次解析后桶值
从 0 变 1 且触发阈值），因为该确定性判决不可用原 key 逆转。

| 载体 | 谁写 | rebuild 怎么重放 | migration |
|---|---|---|---|
| `tasks.decl_ready` / `tasks.decl_released_by_user`（`INTEGER NOT NULL DEFAULT 0`）| 与既有声明列同批写入；**非 `pending` 行不改声明内容**，但收养初始化 `decl_ready=1` | 撤回退化为**当前状态的纯函数**；收养与 `0070_` backfill 初始化影子位不改变 worker 可见规格 | `decl_ready` backfill 为 1（见 §9）；`decl_released_by_user` 保持 0 并列为显式例外 |

**三条边界**：

- **落点只在写路径。** `evaluate_schedulability_tx` 的注释逐字写着它 "used by
  writes, rebuilds **and reads**" —— 撤回规则若落在那里，**一次 GET 就会写
  `context_stale_at_ms` 并发事件**。它只产诊断，永不写判决。
- **策略条件用当前值，不用冻结时值。** 代价：若 wave 的策略在任务在飞期间从
  `declare-and-wait` 翻成 `auto-declare`，先前的撤回将不再被兑现（语义漂移）。
  接受它 —— 替代方案是持久化冻结时策略，而 §5.2 已为哈希拒绝过同一笔成本；
  且策略翻转是人的显式动作，翻转本身就表达了「这个 wave 不再需要逐条放行」。
  **rebuild ≡ 增量仍然成立，理由不是「不依赖策略」，而是两条路径从同一事务状态
  读到同一个当下的策略值。** `ready` 那一支完全不依赖策略，两种策略下都判。
- **读点与判点之间必须有回传通道。** `decl_*` 的定向 SELECT 在
  `evaluate_schedulability_tx` 的 `FrozenDeclarationRow` 里，`effective_policy`
  也是它的内部量，而判决只在 `project_tasks_tx` 写。
  **裁决：`BlockVerdict` 增 `withdrawal: Option<WithdrawalEdge>`；判定在
  evaluate 里算，写只在 project 里做。**

**§4.2 规则 1 的第四种情形（「块新产生了诊断」）明确不纳入撤回**：诊断可以由
**第三方块**的变化引发（未知依赖、越 cove 的 `refs`、gate 规则违反），
把它当成撤回会造成远距离误杀 —— 一个人编辑 wave B 的块，杀掉 wave A 里一条与他
无关的在跑任务。代价：pending 行上「新产生诊断」删行、in-flight 行上只追加诊断，
这一处不对称**保留**且是有意的。

### 5.4 三级阶梯与事件

| 级 | 机制 |
|---|---|
| **第 1 级** | 机械检测：逐元组重解析 + 比对 `content_hash`。**不允许漏报**（精确形式见 §5.7）|
| **第 2 级** | LLM 裁决（切片 4）：把「`b_1f3a` 从 X 变成了 Y」**递给任务所在 wave 的 spec**（不是被编辑 wave 的 —— 这也是闭包不得跨 cove 的原因），返回值只有 `{ verdict: "material" \| "immaterial", rationale }`（封闭二值）。**缺席 / 解析失败 / 超时一律按 `material`** |
| **第 3 级** | 人只在**实质变更**处出现（诊断 + UI）。**判 material 后不自动重做** |

**第 2 级的路由通道**：dispatcher 的 `WaveReportEdited` 分支必须从「单 wave
observe」改成**按 `task_ref_index` 扇出 observe** —— 一次编辑可能命中多个 wave 的
任务，每个都要推给**它自己**的 harness。这是反向索引存在的两个理由之一。

**两个扇出上限**（与闭包预算耗尽同一条 fail-closed 纪律，因而不引入漏报）：
第 1 级 `MAX_RERESOLVE_FANOUT = 64`（按 `dst_wave_id` 的重解析扇出，
超出**不做重解析、直接 `material`**）；
第 2 级 `MAX_ADJUDICATION_FANOUT = 16`（超出剩余任务直接 `material`）。

**分工总纲**：**agent 永远不是「发现」变化的那一环，只是「判断」重不重要的那一环。**
发现由机械检测与 sweep 承担（确定性、可证伪）；agent 只在第 2 级做价值判断。

**两个 NEW 事件**（均严格 `Kernel | KernelDispatcher`，`User` 也拒）：

```rust
TaskContextFrozen {
    wave_id, task_key, idempotency_key,
    refs: [{ wave_id, block_id, rev, content_hash, is_root }],
    doc_revs: { wave_id: u64 },     // §5.2 栅栏基线，只在事件里
    truncated: bool,                 // §5.1 预算耗尽
}
TaskContextAdvanced {
    wave_id, task_key,
    changed_refs: [{ wave_id, block_id, from_rev, to_rev, from_hash, to_hash }],
    verdict, rationale,
}
```

**为什么是独立事件而不是给 `TaskDispatched` 加字段**：那是同等的 Tier-A 代价，
却把一个可选的安全机制焊进一个所有路径都在发的核心事件；独立事件还能在
`truncated: true` 时单独告警。

**`truncated` 必须在事件里**：它是状态，而 §2 要求状态的真源是事件日志。
落在列上而不落事件 ⇒ 任何一次 `tasks` 的清除或 rebuild 都会把它洗成 `0` ⇒
**一个必须无条件 fail-closed 的截断闭包会静默变成「完整闭包」**。

**`changed_refs` 必须带变更明细**：这个事件的**唯一存在理由**是「否则无法排查
为什么 agent 拿着过期上下文产出了东西」。只说「过期了」而说不出哪个引用、
从什么变成什么，等于抹掉它的存在理由。

#### 事件的单赢家原语与消费契约

`mark_context_material_tx(&mut tx, task_id, …)`：条件 UPDATE
（`AND context_stale_at_ms IS NULL`）+ **`rows_affected == 1` 才追加事件** +
已 stale 视为**幂等成功**（不返回业务 `Conflict`）。
**投影 / 事件检测 / 全量 sweep 三条路径全部调用它。**
**归因写死 `ActorId::Kernel`** —— 撤回判决是内核的裁决结果，不能因为它恰好发生
在人的报告写事务里就被归到那个人头上。

> **消费契约**：**任何调用 `mark_context_material_tx` 并提交的路径，
> 必须把返回的 `kernel_events` 并入同一次 eventized write；不得丢弃。**

**载体**：`TaskProjectionOutcome` 增
`kernel_events: Vec<(ActorId, EventScope, Event)>`；`tasks_rebuild_tx` 透传；
**`routes/waves.rs` 的 wave PATCH 必须从 `write_with_events_typed` 迁到
`write_with_actor_events_typed`** —— 它今天是**单 actor 批（= 请求者 `User`）**
且会调 `tasks_rebuild_tx`，不迁移则 `PATCH /waves/{id}` 被闸拒 **403**。
**显式禁止在任何 `_tx` 原语内直接 `event_append_in_tx`**（绕过 role_gate 与
commit-then-emit 广播）。

**一个必须澄清的事实**：role_gate 是**逐 `(actor, scope, event)` 元组**校验的，
**混合 actor 批次本来就受支持** —— 人的报告写路径已经在同批混发
`ActorId::Kernel` 事件。所以问题从来不是「批次 actor 是 User 就一定被拒」，
而是**载体缺失**。

**丢弃 `kernel_events` 不会报错**，会静默产生「**判决落库、事件缺失**」——
而事件才是真源、`tasks` 列只是投影，缺事件即 rebuild 重建不出。
（重复 rebuild **不会**重复发事件：条件 UPDATE 是单赢家，第二次
`rows_affected == 0` 即不追加。）

### 5.5 正确性载体：fail-closed 全量 sweep

**事件路径整条是延迟优化。承载「不允许漏报」的是这一条**：

> **在 boot（先于 operation 恢复与 `Scheduler::sweep_boot`）、`RecvError::Lagged`
> 补偿臂、以及周期 reconcile tick 上，重扫全部 in-flight 任务的冻结集，
> 逐元组重解析；任何一个元组只要不能被**确定性地**验证为「与冻结值相同」
> （根块比**收窄投影哈希**、深度 ≥ 1 比整块哈希，**均不比 `rev`**），
> 该任务一律判 `material`。**

**枚举源是 `tasks` 的 in-flight 行且 `context_stale_at_ms IS NULL`，不是索引** ——
前者对「索引被提前清掉」不敏感，而 wave 删除正好是那种情况。
`MAX_SWEEP_NODES = 4096` 硬顶，用满即把本轮剩余判 `material`。

**必须有一个正向健康信号**：其余可观测量（检测次数 / 命中数 / 触顶次数 /
`closure_truncated` 比例）全是**计数器**，sweep 整体不跑时它们**一起静止**，
看起来和「一切正常」一样。因此上报
**`context_sweep_last_success_age_seconds`（正向 gauge：距上一次成功 sweep 多久）**
与 `context_sweep_consecutive_failures`，**在成功与失败两条路径上都导出**。

**顺序规则**：**凡是会起活的路径，上下文 sweep 都必须排在它前面** ——
boot 的 operation 恢复 / `sweep_boot` 如此，`Lagged` 分支的 `sweep_all` 如此，
**周期 reconcile tick 的 `sweep_all` 同样如此**。补一条源码序断言把它钉住。

#### 瞬时失败不下判决（全局规则）

> **只有确定性的不可验证**（块确实不在、越 cove、引用不合法、冻结集缺失、
> `closure_truncated`）**判 `material`**。**瞬时失败**（存储/IO 不可用）
> ⇒ 本轮 sweep 对该行**不下判决**：保持 in-flight、不置 `context_stale_at_ms`、
> **该行的持久失败计数 +1**；**连续 3 轮仍为瞬时失败 ⇒ 升级为 `material` 并告警**。

**四格**：

| 载体 | 谁写 | rebuild 怎么重放 | migration |
|---|---|---|---|
| `tasks.context_verify_failures INTEGER NOT NULL DEFAULT 0` | sweep 的定向 SQL：成功验证或落确定性判决时清零，retryable 时 +1 | 不需要重放（它是运行期计数，不是声明）| backfill `0` |

**为什么不能复用 `ContextMetrics::consecutive_failures`**：那是一个**全局**
`AtomicU64`，只在**整次 sweep 返回 `Err`** 时 +1，而任何一次成功 sweep 就清零 ——
而新规则恰恰要求「某行瞬时失败时 sweep **不再**整体失败、继续跑完」⇒
`sweep_inner` 返回 `Ok` ⇒ 全局计数器每轮被清零 ⇒ **连续 3 轮结构上永不达成**。
该全局指标的健康信号语义**保持不变，不得被复用成两件事**。

**`refs_match` 必须改三态返回**（相同 / 确定不匹配 / 可重试失败）——
今天它只返回 `bool`，三态表达不了。**三个吞错点都要改**：`wave_get`、
`load_block`、以及 **`cove_get_system().await.ok().flatten()`** ——
第三处会把存储错读成 `None`，进而产生一个**看起来确定性的** cross-cove mismatch。

**这不削弱 fail-closed**：不下判决 ≠ 放行。该行仍是 in-flight，
`refuse_if_context_stale` 的强制点不受影响，下一轮继续重扫；
改变的只是「把无法观测」错记成「已观测到变更」这一件事。

#### boot 门

`context_sweep_boot_done` 守 `resume_dispatched`，语义是「material 判决必须先持久」。

> **任何一次成功的全量上下文 sweep 即原子开门**（三个调用点一视同仁），
> **且仅当门从关翻到开时**（`compare_exchange(false, true)` 返回 `Ok`）
> **补跑一次 `scheduler.sweep_all()`**。

不这样做，boot sweep 一次失败（30 秒超时或一次瞬时 DB 错误）⇒ 该进程**余生**
所有 `dispatched` 行永不被重驱动，永久占住 `task_budget`，整个 wave 静默停摆，
且只有 `debug!` 级日志。
反过来若不加「仅在翻转时」，每个 tick 都会跑**两遍** `sweep_all`。

**一处必须区分的事实**：`boot_sweep_done` 是**另一个**门 —— 它守 `sweep_all`，
注释逐字写着防 "re-drive dispatched rows against unrecovered operation rows"，
本裁决**一个字都没动它**。两条到 `resume_dispatched` 的路径都安全，但理由不同：
`sweep_all` 那条被 `boot_sweep_done` 门住（整体 no-op）；`sweep_boot` 那条不受它
保护，但 `scheduler_sweep_on_boot` 排在 `recover_operations_on_boot` **之后**。
**boot 路径上「翻转即补跑」必然是 no-op**（此时 `boot_sweep_done` 仍为 false），
无害，但验收不得把它写成断言 —— 它真正的用武之地是「boot 失败、后续 tick 成功」。

### 5.6 判决的强制点

**非空的 `tasks.context_stale_at_ms` 禁止该任务上的任何 operation 启动，
从不打断已启动的。**

**强制点在四个 task 绑定 adapter 的 `prepare_tx`**（三个 `*-worker` +
`task-verify`）。理由：`prepare_tx` 是所有起活路径的**必经漏斗** —— `submit` 与
开机恢复都通向它，在事务内、在任何副作用之前。

**「不得再产生新的 `TaskDispatched`」在代码里恒真且无用**（唯一发射点在 claim，
行永不回 `pending`）。而选 `resume_dispatched` 作落点也是错的 —— operation 开机
恢复、`submit` 先插行后 drive 造成的 `Pending` 窗口、gate 首启三条都从它旁边走
过去。

**落地形态**：`refuse_if_context_stale(tx, task_id)` 约十行 —— 读
`context_stale_at_ms`，非空即 `CalmError::Conflict`；**task 绑定缺失 / 查不到行
一律 fail-closed**。**下游零新分支**：`Conflict` 是 `client_failure_parts` 认的
永久客户端失败 ⇒ worker 侧落既有 `fail_spawn`、gate 侧落既有 pre-bump 失败臂。

**判据是 payload 的 task 绑定，不是 op kind 的封闭性** —— `codex-create` 这类
kind 是任务派发与用户建卡**共用**的。

**防复发**：注册表元测试**先**对「真实注册的 adapter kind 集合」与权威常量
`TASK_BOUND_ADAPTER_KINDS` 做**集合相等断言**，**再**逐个断言拒绝。
新增 adapter 不分类 ⇒ 集合断言红；分类为 task-bound 却漏接检查 ⇒ 逐项断言红。
**挡不住**的是「被错误地分类进 `NON_TASK_BOUND_ADAPTER_KINDS`」——
在该常量上加判据注释（"payload 不含 task 绑定"）。

### 5.7 断言的最终形式

前几轮把「不允许漏报」当成全称断言在用，而 §5.2 刻意开了一个口、§5.2 的栅栏
暴露了「最终会发现 ≠ 禁止启动」的区分。**准确表述必须把两者写进断言本身**：

**本节是全文的优先条款：凡其余各处出现未限定的「不允许漏报」「任何内容变更都会
被检出」，一律以本节为准。**

> **(A) 最终检出（sweep 承载）**：对每个 in-flight 任务的冻结闭包，任何在 claim
> **成功之后**发生的内容变化，都会在**下一次完成的 sweep**结束时被判定为不匹配，
> **除以下四个例外**：
> **(a)** 根块的 8 个排除字段 —— **刻意不检测**，其中 `ready` /
> `released_by_user` 的**撤回方向**另由 §5.3 覆盖；
> **(b)** SHA-256 碰撞（密码学残余）；
> **(c)** 连续 3 轮瞬时存储失败之内的窗口 —— 期间不下判决，行仍 in-flight；
> **(d)** **冻结集为空（`claim_context_json = '[]'`）的行** —— 不写出它，
> (A) 在这批行上就是**空洞成立**的全称命题（本项目明令要审的那一类）。 —— `refs_match` 对空
> `refs` 零次迭代即通过，**按构造永不 material**。
>
> **(B) 启动前检出（更强的性质，范围有限）**：
> **(B1)** 解析完成到 claim 提交之间的变化，由 claim 事务内的**根块 + 各 wave
> `doc_rev` 栅栏**关闭 —— 不一致即 race-lost。
> **(B2)** 判决**已经持久化**之后，四个 task 绑定适配器 `prepare_tx` 里的
> `refuse_if_context_stale` 禁止该任务上任何 operation 首次启动。
> **(B) 不覆盖**「claim 提交之后、判决落库之前」那一段 —— 栅栏在提交那一刻解除，
> 而事件路径 lossy、sweep 最多 300 秒一轮，于是 worker 可能先于判决启动。
> 这是**允许的检测延迟**，与 §5.6「禁止在**已判定为** material 的上下文上启动」
> 不冲突，但 (B) 因此**不能**被读成「所有 material 变化都先于首启被判定」。
>
> **(C) 误报侧不属于本断言**：本条只描述 **in-flight 冻结集**在 sweep / detect
> 路径中的确定性验证失败；越 cove、超预算、块确定不存在等一律直接判 `material`，
> 是 fail-closed 的保守方向，只增加误报、不削弱 (A)(B)。**claim 前的定位失败不在
> 本条范围内，一律不下判决、按 race-lost 处理，见 §5.2。**
>
> **(A) 与 (B) 互不推出，要分别验收。**

**关于例外 (d) 的范围**（必须如实说）：判据是**「冻结集为空」而不是
`origin='legacy'`**。后者划错了范围，漏掉两类：**(i)** 收编路径会把在飞的 legacy
行 `origin` 翻成 `'block'` 而 `claim_context_json` 仍是 `'[]'`；
**(ii)** 更大的一块 —— **切片 3b′ 通电之前，生产 claim 对所有行都走空闭包**，
即**每一条在飞行**都属于这一类。**新 legacy 行真正停增要到切片 5**
（`calm.plan.upsert` 退场）；在那之前 (d) 是一个**持续增长**的集合。

### 5.8 检测的挂点

第 1 级检测挂在 `WaveReportEdited` 分支里、`event_warrants_spec_push` **之前**，
**对所有 author 无条件运行** —— 那个谓词只放行 `User | Plugin`，
而 spec 自己的编辑是最常见的变更源。并新增 `WaveDeleted` / `CoveDeleted` 触发
（wave/cove 删除一个报告编辑事件都不发）。

**反向索引 `task_ref_index(task_id, dst_wave_id, block_id)`** 是事件路径的加速结构，
**不是正确性载体**。**读端一律与 `tasks` 内联接并过滤 in-flight** ——
清单负责代价，联接负责正确性；漏一个清理点是**代价 bug 而非正确性 bug**。终结跃迁由 trigger 清扫，wave/cove 删除与 replay 有显式 DELETE，
sweep 末尾兜底。**代价要如实记**：用 FK + trigger 换掉了**编译期可见性** ——
第十条生产者路径可以无声出现而不会有任何 Rust 调用点编译失败。

---

## 6. 人 / AI 的不对称

### 6.1 撤销权不对称：墓碑

**没有这条机制会死循环**：人删掉一个 spec 声明的任务 → spec 下一轮又写回来 →
人再删 → 无限。

> **墓碑**：人删除一个活的 task 块 ⇒ `normalize_report_op` 把它改写为**原位墓碑**
> （保留 `key`，写 `tombstone: { reason }` 与 `tombstoned_by: "user"`）。
> **未清除的用户墓碑挡住同 `key` 的重声明** —— 投影对同 key 的活块产出
> 「该 key 有一个未清除的墓碑」诊断，不落行。

**审计契约要说准**：pending 行被删后的持久记录是 `WaveReportEdited` +
`PlanUpdated` + 墓碑块（受 `events_prune` 白名单保护），
**不能说成「`tasks` 保留了撤销历史」** —— 那一行确实被删了。

**清除路径两条都通**：人删自己的墓碑、spec 删自己的墓碑。
**spec 删人的墓碑被拒**（§3.7 规则 3 覆盖 `tombstoned_by == "user"`）。

**权属载体是独立的 `tombstoned_by`，`declared_by` 原样承接。** 用 `declared_by`
当载体时，「人删 spec 声明的任务」这条**默认路径必然 400**（规则 4 的改写
vs 规则 2 的冻结）。

**换 `key` 绕过否决怎么办（B2 裁决：改为一次点击，不再自动派生）**：
人删除一条 spec 声明的任务时，**UI 当场问一句**「要不要此后 spec 的任务都等你放行？
[要 / 只删这条]」；选「要」即写显式 `automation_policy = 'declare-and-wait'`
（§6.6）。**机制一个字都没改** —— 显式设置本来就是 `effective_policy` 的第一分支。
与 `key` 无关、零相似度判断，且复用本设计已经在付钱的两样东西。
**这条防线因此是 opt-in 的，代价要如实说**（B2 的定价，§12.2）：
人不点「要」的时候，「spec 换 key 重提」的循环**仍然存在** —— 本设计只保证
**同 `key`** 被墓碑挡住（§4.2 规则 2 + §3.7 规则 3），不保证换 key 也挡住。

**为什么接受它**：那条循环的真实频率**从未被观测到**（其证伪装置本身是一条尚未
上线的可观测量），而自动派生的代价是**每次否决都要付**的 ——「一处否决、全 wave
收紧」会让该 wave 内 `declared_by='spec'` 且未放行的 pending 行被守卫式删除，
即**删 A 导致 B、C 消失**，且触发的人与读到诊断的人常常不是同一时刻的同一个人。
**用一个未观测到的风险换一个必然发生的困惑，在使用者一侧是亏的。**

**可证伪 / 回退条件**：若真实使用中观测到「spec 换 key 重提绕过人的否决」的循环，
**退回自动派生**（B1）—— 那只需在 `effective_policy` 里恢复第二分支，
且必须同时配上 wave 级常驻横幅、触发时的确认对话框、以及两个清除**按钮**。

### 6.2 预算不对称

`declared_by == "user"` 的声明**不计入** §8 的 spec 预算 —— 人的声明有天然上限。
这也是 §5.2 把 `declared_by` 排除出哈希、但由**写口**永久冻住它的原因：
它是预算豁免的依据，伪造即绕过预算。

### 6.3 就绪判据：机器门，不是人工批准

> **门是「AI 必须先能把它变成可验收的，才能排进去」，不是「人批准 AI 的草稿」。**

标记是文档里的 `ready` 字段，投影在同一事务里生效、**无滞后**。
spec 写 `ready: true` 而校验不过 ⇒ 写时当场拒（`-32602`）。
**写时门只含块局部谓词** —— `dup_keys` / `unknown_deps` / `find_cycle` 等**批级**
谓词**明确不在写时拒**，否则一次并发的人的编辑会让 spec 的合法写变成非确定性 400。
批级规则只产诊断。

**可证伪**：若观测到 `ready: true` 但仍产出垃圾的比例高，说明 `acceptance` 的
校验太弱（今天只校验非空），需要更强的可执行性判据。

### 6.4 归因不对称

见 §4.4。`declared_by` 住文档、由写口强制、永久冻结。

### 6.5 in-flight 任务的删除

**不存在「任务终结的那一刻发生一次状态变化」这回事。** 墓碑此后只作为记录保留，
并挡住同 `key` 重声明。

删除 / 墓碑覆盖一个 in-flight 任务的块 ⇒ 该任务的根冻结元组解析不到 ⇒
判 `material` ⇒ `context_stale_at_ms` 被写入 ⇒ 该 task 上任何尚未进入
`prepare_tx` 的 operation 一律被拒，**gate 也在内**。

因此「worker 会跑完，其结果照常过 gate 并汇报」在删块这条路径上**恰恰最常不成立**：
只有当 gate op 在删块之前就已越过 `prepare_tx` 的那个窄窗口里它才为真。

**两条必须如实说的代价**：

1. **判 material 之后不会再有新的 gate 执行** —— 于是一个 worker 可能已经产出了
   完全可用的东西，却以 `failed`（`gate-infra` + `context-stale`）收场，
   **人要重做必须换一个 `key`**。
2. **不新增 gate 原因枚举值** —— 沿用既有 `"gate-infra"`，过期信息只在 `log_tail`
   里。这是**刻意省下的一次 wire 面变更**，代价是 **UI 上两类失败长得一样**
   （基础设施故障 vs 你的编辑作废了这次执行），**只能靠文案区分**。
   因此 §11 切片 3c 的两类 context-stale 文案**必须分得开**，
   且其中一类必须说明「**worker 的产出仍在**，只是没有被验证过」。

**用户可见的三件事**（撤回一个 in-flight 任务时）：撤回意图已记录、
任务仍在执行且不会被打断、以及**该任务终结后不会再重开同名任务**。

### 6.6 自动化程度作为 per-wave 策略

**默认 `auto-declare`。** 人逐条点「批准」不产生新信息 —— 人在点之前并没有比
`ready` 门更多的判据；它只把延迟加进每一条任务。护栏由机制提供：
§6.3 的写时机器门、§8 的预算、§3.5 的 in-wave 默认。

- **`auto-declare`（默认）** —— AI 自行声明即排队。
- **`declare-and-wait`（只能显式开启：人 PATCH 该列，或删任务时在确认框里选「要」）** ——
  `declared_by: "spec"` 的块**即使 `ready: true` 也不落 pending 行**，
  直到该块的 **`released_by_user == true`**。

**放行位必须是人可写、spec 不可写的独立载体**（`released_by_user`，
§3.7 规则 5）。用 `ready` 做不到 —— spec 写的块上它本来就是 `true`，
「人改成 true」是空操作；用 `declared_by` 也做不到 —— 规则 2 冻住它。

**`auto-declare` 下 `released_by_user` 没有语义**（投影忽略它），
所以默认路径零成本；这也是 §5.3 的撤回规则必须带策略条件的原因。

#### `effective_policy`：生效策略的形式化定义

```
effective_policy(wave) =
    waves.automation_policy      若该列非 NULL（人显式设过）
    'auto-declare'               否则
```

**载体（四格）**：

| 载体 | 谁写 | rebuild 怎么重放 | migration |
|---|---|---|---|
| `waves.automation_policy TEXT NULL`（NULL = 内核默认，**三态**）| **只有 `EditAuthor::User` 经 `WavePatch` 的定向单列写面**；spec 写入一律 403 | `effective_policy` 只是该列的读取（B2 裁决后不再依赖文档墓碑），rebuild 天然一致 | 新列，`NULL` 即默认，无需 backfill |

**`TEXT NULL` 的三态仍然是必需的**（B2 之后没有派生分支，但三态区分「人设过 auto」/「人设过 wait」/「从未设过」，后者是内核默认可随版本调整的口子），不是风格：它与 `spec_task_ceiling INTEGER NULL`
及 `WavePatch` 的 double-option（`Some(None)` = 清回默认）**本来就是同一形状**。
人有**两个语义不同的动作**，互相独立（B2 裁决后不再耦合）：

1. **删掉那块墓碑**（= §6.1 的「撤回否决」）⇒ 该 `key` 可以重新被声明；
2. **PATCH `automation_policy = 'auto-declare'`**（或清回 `NULL`）⇒ 恢复自动化，
   **墓碑保留** —— 于是人可以「保留否决记录，但让别的任务照跑」。

**写口是 user-only，且这是第二个强制点（NEW，缺一不可）**：

> `automation_policy` 与 `spec_task_ceiling` 的写入必须加一条**镜像
> `validate_transition` 形状的 user-only 检查** —— 非 `ActorId::User` ⇒
> `CalmError::Forbidden`，**写之前拒、不落行不发事件**。
> 理由：`X-Calm-Actor` 是**自述的**（`actor.rs` 逐字写着 "not a security
> boundary"）；**逐块守住 `released_by_user`、再留一个 wave 级总开关不守，
> 等于没守**。
>
> **「人可写、spec 不可写」有两个强制点，缺一不可**：块内三个位由
> `guard_task_declarations` 守，wave 级两列由 `update_wave` 的 user-only 检查守。
> **两个收口对应两条写路径，没有第三条。**

**连带的写面**（Tier-A）：`WavePatch` 两个 **double-option** 字段
（`Some(None)` = 清回默认）、`wave_update_tx` 的**定向单列 UPDATE**、
**两列刻意不上 `Wave` 结构体**、**`patch_has_other_changes` 的判空列表必须加这
两个字段**（否则「只改策略」的 patch 被当空 patch 短路，一个事件都不发）、
REST 400 校验、OpenAPI / zod / web 生成物。

**这一档的诊断必须成对**：「本 wave 已设为『spec 的任务等我放行』，spec 声明的任务需逐条放行；恢复自动化见下」
（**与墓碑脱钩**：B2 后可以有墓碑而 wave 仍是 `auto-declare`，也可以没有任何墓碑
而 wave 是 `declare-and-wait`，文案不得把成因说成墓碑）——**没有这条诊断就是静默降级**。

**不做「有界时间窗」**：时间窗一到循环即恢复，**吸收态没了**；
且窗口需要一个时间戳 ⇒ 不是当前文档的函数 ⇒ rebuild 重建不出
（与 §8 驳回累计配额同一条理由）。

---

## 7. 模板

### 7.1 拆分线

| workflow descriptor 字段 | 去向 |
|---|---|
| `plan_template` | → report 模板里的 `task` 块 |
| `gates` | → 模板里的 prose / `task.gate`（今天明注 NEVER executed）|
| `spec_instructions` | → 模板报告正文（prose 块）|
| `card_kinds` | **留在 plugin**（kind 命名空间注册，可执行）|
| `input_schema` | **留在 plugin**（`workflow_input` 校验）|
| forge-action 执行语义（argv / idem_key / probe）| **留在 plugin**（③ 通道）|

### 7.2 模板 = 任意 wave 的报告，wave 创建时 fork

不新建「模板 wave」实体、不给 cove 加文档载体。wave 创建时可选
`fork_report_from: "<wave_id>"`：在创建事务内，用源 wave 的 report 卡的块快照
（改写过引用之后）播种新报告，**块 id 逐个保留、`rev` 从源承接**。源 snapshot
一旦确定，引用改写、目标 doc 构造与写入全程**零 remint**；无持久 block snapshot
的 legacy 源在首次读取时仍会按既有读路径派生 id，这批派生 id 原样成为目标持久 id，
源 wave 不写回。

「这是一个模板」是一个 **kernel overlay 标记**（`entity_kind: "view"` /
`kind: "template"`），沿用 layout overlay 的先例。**零新表、零新权限面、
零新概念、零新 entity kind。**

**fork 的三条强制责任**（它不经 `apply_report_op`，§3.7 的守卫一个都不会跑）：

1. 对写入的**每一个** fence payload 跑 `validate_payload`，失败 ⇒ wave 创建 400；
2. 跑 `guard_task_declarations` 的**规则 1 之外**的部分（规则 2/3/4′ 在空前态上
   平凡成立；规则 1 正是被豁免的那条）；
3. 豁免**只**豁免规则 1，且**只**在 fork 路径上 —— 不新增任何「跳过 guard」的
   可复用开关。

**fork 强制改写两个字段**：

- **live task 的 `ready` 降为 `false`** —— 没有任何东西是在「这次」被决定要做的；
  这也让「wave 创建事务里意外派发」在结构上不可能。
- **墓碑 task 不写 `ready`** —— 墓碑 schema 禁止该字段。
- **`declared_by` 改写为 `"spec"`** —— 模板里的任务不是**这个人**为**这个 wave**
  提的；标成 `spec` 把它们纳入 §8 的预算（若标 `user`，模板就成了绕过预算的后门）。

### 7.3 fork 的 `neige://` 引用重写

fork 复制文本 ⇒ 文本里的 `neige://wave/<源 wave>#b_xxxx` 会**指回模板原文**。
必须在 fork 的同一事务里把 wave 段重写为新 wave，**`#b_x` 那一半原样不动**
（依赖块 id 被保留，见 §7.2）。

`scan_links` 不足以做原位改写：`ScannedLink.label_start/label_end` 是剥离 Markdown
后的 plain 文本标签 offset，不是源 Markdown destination 的 offset。fork 必须使用
source-aware helper 改写 inline / reference-style / autolink 的 destination；只有当
`pulldown-cmark` 的 destination 直接借用源 Markdown 字节（即原始 destination 与解码值
逐字节相同），且 link label 不含 inline HTML 时才允许改写。字符实体、反斜杠转义或
label 内 inline HTML 会让 `POST /api/waves` fail-closed 返回 400；错误逐条列出块 id、
字段与无法安全定位的 destination 源文本，并提示先改成普通形式再重试，绝不静默漏改。
code span 与 fenced code 仍按 parser 语义忽略。覆盖集合只包括 prose 的
`markdown`、task 的 `goal` 与可选 `acceptance`，以及 task `refs[]` 中的裸 URI；
不扫描 canonical fence JSON、`context`、gate 或 tombstone reason。只改目标 wave 等于
源 wave 的引用，外部引用逐字节不变。这里**错了是静默的**：指回模板原文的链接仍是
合法链接，只有内部/外部两侧都钉死的硬测试才能检出。

### 7.4 复制，不是引用

模板改动**不传播**，但每个 wave 能自证当时按什么流程跑。与「产出与证据」一致。
传播需求用「模板已更新，这些 wave 落后了」的提示解决，不用共享可变状态解决。

**权限与生命周期**：fork 是读源 + 写新，源 wave 无副作用；源 wave 被删除后已 fork
出的 wave 不受影响。源 wave 必须与新 wave **同 cove 或在 system cove 下**。

### 7.5 人选模板，spec 可提议

选模板是**一次性、wave 尺度**的选择，与「每条任务放行一次」不是同一量纲；
wave 创建本来就是人的动作。

### 7.6 与 #891 的关系：`workflow_input` 保留

**不消解。** 模板复制的是**文档**，`workflow_input` 传的是**本次运行的参数**，
两者正交。

---

## 8. 预算

三层，互不替代：

| 层 | 载体 | 约束什么 |
|---|---|---|
| **并发** | `waves.task_budget`（既有，默认 1）| 同时在飞的任务数 |
| **未结存量** | `waves.spec_task_ceiling`（默认 32）| 该 wave 内 `declared_by='spec'` 且 `origin='block'` 的**未结**行数 |
| **树级 + 深度** | `waves.parent_wave_id` + `waves.tree_task_budget`（默认 32）+ `MAX_WAVE_TREE_DEPTH = 3`（切片 6）| 只约束 `declared_by='spec'` 的子树递归展开 |

**为什么并发容量挡不住存量**：`compute_ready` 算的是
`capacity = budget - running_cost` 然后 `take` —— 一个失控的 spec 可以声明无界的
pending 行，以并发 1 串行地永远排下去。

**`spec_task_ceiling` 的谓词形状**（这一处已经被写错过两次）：
见 §4.2 规则 3 —— `occupied` **只数 `dispatched/running/verifying`**，
`pending` 一概不数；准入按块序 + `key` 升序在剩余容量内。

**上界的精确形式**：`非终结行数 = occupied + |准入| ≤ max(ceiling, occupied)`。
若人把 ceiling 调低到当时的在飞行数以下，上界**暂时退化为调低那一刻的在飞行数**，
并随这些行终结**单调收敛**回新 ceiling —— 期间 `capacity = 0`，不准入任何新行。

**所有上界常数都是猜的**：`spec_task_ceiling = 32`、`tree_task_budget = 32`、
`MAX_WAVE_TREE_DEPTH = 3`、`MAX_REF_DEPTH = 3`、`MAX_REF_NODES = 64`、
`MAX_RERESOLVE_FANOUT = 64`、`MAX_SWEEP_NODES = 4096`、瞬时失败升级阈值 3。
**校准装置是切片 4 的可观测量**（§5.5 的健康信号 + `closure_truncated` 比例 +
声明速率），上线后按数据调，不要把它们当成论证出来的值。

**它约束的是任一时刻的未结存量，不是生命周期总量。** 它挡住「一次性声明 500 条
排到天荒地老」，挡不住「每完成一条就再声明一条」。
**刻意不引入累计配额** —— 单调计数器不是当前文档的函数、rebuild 重建不出，
会成为 §2 承重墙上的第三个真源。证伪装置是「每 wave 的 spec 声明速率」这条可观测量。

---

## 9. 迁移

### 9.1 声明侧一次性物化，状态侧原样保留

现有 wave 的 `tasks` 行由 `calm.plan.upsert` 写入，**没有对应的 `task` 块**。
plan 表降为投影后，一次 `tasks_rebuild_tx` 会把它们全部抹掉。

**裁决：双向迁移。**

1. **`tasks.declared_by TEXT NOT NULL DEFAULT 'spec'`** —— 存量行全部标 `spec`
   （它们确实全部来自 `calm.plan.upsert`）。
2. **`tasks.origin TEXT NOT NULL DEFAULT 'legacy'`**（`legacy | block`）。
   **投影只管理 `origin='block'` 的行**：删除阶段只删 `origin='block'`、
   当前文档不再声明、且仍是 `pending` 的行。
3. **同 key 收编**（自动且幂等，因此物化工具只是便利工具、不是迁移必需品）：
   - **pending 行** ⇒ 原地收编：`origin` 翻 `'block'`、**声明列全量覆盖**、
     状态列一字不动，同一条 UPDATE 同一事务；
   - **非 pending 行** ⇒ 同样翻 `origin`，声明内容不动；但收养只发生在
     `schedulable`（当前 `ready=true`）时，必须初始化影子位 `decl_ready=1`。
     `decl_released_by_user` 保持 0，因为旧值不可重建；不一致则出 stale 诊断。
   - **任何情况下都不 INSERT**，所以 `UNIQUE(wave_id, key)` 永远不会被撞到。
4. **物化工具（可选、人触发）**：admin CLI 把某个 wave 的 `tasks` 行写成 `task` 块。
   **它与 fork 一样需要规则 1 的豁免**（写入的 `declared_by` 来自存量行的归因，
   而不是本次编辑的 author）。**豁免点必须是封闭集**：全仓只有 **fork 与物化工具
   两处**，配一条**枚举测试**断言不存在第三处——否则 §3.7「所有写路径唯一收口」
   是一条空洞的普遍否定。
   **对每一行写 `ready: true`** —— 语义是「这条声明现在由文档承载，与它此前的状态
   一致」，不是「现在决定要做它」。（**与 fork 的 `ready:false` 方向相反，这是对的**：
   fork 面对的是模板，物化面对的是**已经在排队的活行**；若写 `false`，
   §4.2 规则 1 会当场删掉那条活的 pending 行。）

### 9.2 新列的 backfill（每一列都必须有裁决）

**教训**：新列 + 同片上机制 + 无 backfill = 跨升级必然事故。已经发生过一次
（`claim_context_json`），不能再发生第二次。

**切片 3a（`0067_`，已合入）**：

```sql
UPDATE tasks SET claim_context_json = '[]'
 WHERE claim_context_json IS NULL
   AND status IN ('pending','dispatched','running','verifying');
```

不做则升级瞬间所有在飞任务是「缺失」而非「空」⇒ 首次 boot sweep 全判 material
⇒ 叠加强制点即为一次必然的 Stuck-ops 事故。
`context_stale_at_ms` **不需要** backfill —— `NULL` 的语义正是「从未被判 material」。

**切片 3b′-ii（`0070_`）—— 三列，同一个 migration 里连 backfill 一起写**：

| 列 | 类型 | backfill | 理由 |
|---|---|---|---|
| `decl_ready` | `INTEGER NOT NULL DEFAULT 0` | `UPDATE tasks SET decl_ready=1 WHERE status IN ('dispatched','running','verifying') AND origin='block'` | 保守可重建：能落成 `pending` 并被 claim 就说明当时 `ready==true`。**不 backfill 则升级当天所有在飞任务永久豁免撤回规则** |
| `decl_released_by_user` | `INTEGER NOT NULL DEFAULT 0` | **保持 0，列为显式例外** | 该位**不可从 `tasks` 行重建**（`auto-declare` 下当前值全是缺省 `false`）。**代价如实写**：升级时已在飞的任务不享有该位的撤回检测；新 claim 的行不受影响 |
| `context_verify_failures` | `INTEGER NOT NULL DEFAULT 0` | `0` | 语义正是「尚未连续失败」 |

**`doc_revs` 不落列**（只进 `TaskContextFrozen` 事件，§5.2），因此无第四列、
无第四条 backfill。

**三列刻意不进 `TASK_COLUMNS` / 公共 `Task`**，各配定向 reader：

| 列 | reader |
|---|---|
| `decl_ready` / `decl_released_by_user` | `evaluate_schedulability_tx` 的 `FrozenDeclarationRow` 定向 SELECT，经 `BlockVerdict.withdrawal` 回传给写路径（§5.3）|
| `context_verify_failures` | 只由 sweep 的定向 SQL 读写 |

**在 migration 旁注明「这三列刻意不进 `TASK_COLUMNS`」**，避免下一个人「顺手补齐」
——`TASK_COLUMNS` 服务的是通用 `Task` 查询，塞进去会白白扩大 model / 序列化 /
OpenAPI / TS / 所有 `query_as::<Task>` 的连带面。

### 9.3 纪律

- **已发布的 migration 一律不可编辑** —— sqlx 对整个文件做 checksum，
  改一个字（含注释）会让启动直接 `VersionMismatch`。backfill 必须写进**新**文件。
- **加列必须同步所有显式 SELECT** —— `sqlx::query_as` 在**运行期**失败而不是编译期。
  这条纪律对 `declared_by` / `origin` 成立（投影确实读 `Task`）。
- `replay.rs` 的 fixture 重置表清单必须包含 `task_ref_index`（该表与 `tasks` 一样
  **没有 FK**，删 `waves` / `tasks` 不会级联到它）。

### 9.4 不做的事

不在服务器启动时批量重写报告；不删任何 legacy 行；不改已发布的 migration。

---

## 10. 验收

### 10.1 核心断言（可证伪）

> **从文档重放能重建出同一份 plan。**

**精确形式**（初稿的形式化有致命洞，见附录 B.2 r8 系列）：

> **rebuild ≡ 同一份文档上的增量投影**，且只对**当前** plan 成立。
> 具体地：对任意 wave，「逐步增量投影」与「末态一次 `tasks_rebuild_tx`」
> 必须产出**逐字节相同**的 `tasks` 表（含 `decl_ready` / `decl_released_by_user` /
> `context_verify_failures` 三列与 stale 终态），并产生**同一组**
> `kernel_events`（各恰好一次）。

**为什么初稿的「重置成 ready 声明集合」是错的**：那会孤儿化活 worker 与幂等键 ——
非 pending 行不能被文档重置，它们的状态真源是事件日志（§2）。
（初稿援引的先例 `proposals_rebuild_tx` 已随 #978 删除。）

### 10.2 不变量骨架（E2E 用）

> **完整清单见附录 D.1 + D.4**（去重后 25 条）。此处只摘出最承重的几条；
> 施工与验收**以附录 D 为准**。

**顺序不变量**：

1. `CardUpdated` → `WaveReportEdited` → `PlanUpdated`，同一事务、同一 wave scope。
2. `PlanUpdated{key}` **严格早于**该 key 的第一个 `TaskDispatched`。
3. 每个 `TaskDispatched` **同批**必有一条 `TaskContextFrozen`（同 wave、
   同 `idempotency_key`）。
   **「空」不等于「缺失」**：`origin='legacy'` 的行仍然发射，只是 `refs: []`。
   一个空冻结集的语义是「这个任务没有可失效的上下文」，与「我们没记下它的上下文」
   （= fail-closed 判 material）是两件事。

**冻结与检测**：

- **3a. 空冻结集行可区分**：`claim_context_json = '[]'` ⇒ 不判 material；
  `NULL` ⇒ fail-closed 判 material。
  （**判据是「冻结集为空」不是 `origin='legacy'`** —— 收编后 origin 已翻成
  `'block'` 的行、以及 3b′ 通电前经 legacy claim 路径的**全部**行同属此类。）
- **3b. task 块自身在冻结集内**：任何 `origin='block'` 行的
  `TaskContextFrozen.refs` 必包含它自己的块，**且带显式 root 标记**。
  编辑一个 in-flight task 自己的 `goal` → 必有 `TaskContextAdvanced`。
  **限定**：这里的「编辑」指**纳入字段集内**的编辑（`goal` 恰在集内）；
  **不可据此推广**。
- **3c. 栅栏关闭解析→claim 的 TOCTOU**：在根块解析之后、claim 事务提交之前注入
  一次报告写（**必须同时覆盖同 wave 与跨 wave 两个被引用块**），则该 claim
  **必须 race-lost**：行仍在 `pending`、**无** `TaskContextFrozen`、
  **无** `task_ref_index` 行、**无任何 worker / gate 首次启动**。
- **3d. 根块哈希只在纳入字段集上敏感**：改 `goal` / `kind` / `gate` ⇒ 判；
  改 `priority` / `declared_by` / `spawn` ⇒ **不判**；
  `released_by_user: false → true` ⇒ **不判**。以下属于 **3b′-ii 撤回规则**、
  在 3b′-i 本片不成立：`ready: true → false` ⇒ **判**（两种策略）；
  `released_by_user: true → false` ⇒ **仅 `declare-and-wait` 下判**。
- **4. 改动落在冻结集内 ⇒ 在此后第一次完成的 sweep 结束时**必有一条
  `TaskContextAdvanced`，**且它属于 task 所在的 wave**。
  E2E 的断言形态：**不等待事件到达，而是显式跑一次 sweep 再断言** ——
  事件路径只是让它更快，不是让它成立。
- **4d. 瞬时失败不产生持久判决**：注入存储错误使 sweep 无法验证某行 ⇒ 该行
  **不得**被写 `context_stale_at_ms`、仍 in-flight、
  **`tasks.context_verify_failures` +1**；连续 3 轮后才升级；中间任一轮成功即清零。
- **5. 判决落库后禁止起活**：`context_stale_at_ms` 非空 ⇒ 该任务上任何 operation
  的 `prepare_tx` 拒绝；**从不打断已启动的**。
- **5b. boot 顺序**：上下文 sweep 排在 operation 恢复**之前**（源码序断言）。

**投影与重建**：

- **11. rebuild ≡ 增量差分**（§10.1）。
- **12. 终结 / 不存在的 task 不得拥有 `task_ref_index` 行**（含「一轮 sweep 之后」）。

### 10.3 切片 3b′ 的专项验收

- **根接线元测试** —— **必须经由生产 claim 路径**产生 `claim_context_json`，
  **禁止用 SQL 预置该列**；断言非空且包含根块自身。
  表驱动用例：唯一存活块 / 同 `key` 两块 / 同 `key` 普通块 + 墓碑 / 根刚被删 /
  `origin='legacy'` / 解析后 claim 前发生编辑（栅栏生效）/ 根块收窄投影 vs
  子块整块哈希 / claim 回滚不留索引行也不留冻结事件。
  **变异验证**（注释掉接线证明变红）保留，但承重的是「禁止 SQL 预置」这条
  结构性约束。
- **`doc_revs` 不进 sweep 判据**：claim 后编辑同 wave 的**无关块**（`doc_rev` 必变）
  ⇒ sweep **不**判 material。
- **㉑ 回归**：编辑被引用块 → 撤销回原文 ⇒ **不判**；
  删块后新块同 id 同 rev 不同内容 ⇒ **判**。
- **`kernel_events` 四条**：增量与 rebuild 得到同一 stale 终态且**各自恰好一次**；
  已 stale 后重复 rebuild **零**事件；回滚不留行变更也不留事件；
  **两条消费点都覆盖 mixed actor**。
- **事件兼容回归**：3a 形状的历史事件仍可被 `events_since` 读出
  （新字段 `#[serde(default)]`、`task_id` 保留兼容读）——
  `events_since` 对反序列化不上的行是**静默跳过**，改 payload 不会崩，
  但会让历史事件从事件流里**消失**。
- **升级路径专测**：构造「加列之前就在飞」的行 → 跑 migration → 断言
  `decl_ready=1` / `decl_released_by_user=0` / `context_verify_failures=0`
  → **且首次 sweep 不得判它 material**。

### 10.4 门

fmt / clippy `--workspace --all-targets --features calm-server/codex-e2e -- -D warnings` /
Rust test **`cargo nextest run --workspace --locked --features calm-server/codex-e2e`** /
OpenAPI 重生成无 diff / **`web` build + vitest** / **`fe` lint + build + test**。

> **这份清单必须与 CI 的 job 列表对齐**（`.github/workflows/ci.yml`：
> `lint` / `rust (test)` / `web-unit (build + test)` / `fe-unit (lint + build + test)` /
> `openapi drift` / `a11y` / `chromium e2e` / `stack e2e` / `frozen security vectors`）。
> **本片四次因为清单与 CI 实际命令不一致而报出假绿**：第一次漏 `calm-truth --lib`（防复发单测落在那个
> crate，实测 `299 passed; 1 failed`），一次漏 `fe` 的 `test:wire`
> （`fe/core/api/generated/wire.ts` 与 `web/src/api/generated-events.ts` 必须**逐字节相同**，
> 而 `gen:api` 只写 web 那一份，fe 那份靠手工同步）；第三次漏了 clippy 与 workspace test
> 的 `--features calm-server/codex-e2e`；第四次按 target 枚举，漏掉了 `domain_api_suite`，
> 即使 `--lib` 与四个点名的集成套全绿也不等于 workspace 全绿。**按 target 枚举必然漏，Rust 门只能执行上面的 CI 原样命令。**
> `codex_forge_e2e.rs` 整个文件被该 feature 门住，
> 不带 feature 时会编译成空；因此本地不带 feature 跑绿不代表 CI 绿。
> **四次都是「实际 CI 门没有被完整列出和执行」，必须逐字对照 workflow 命令，不能用近似命令或子集替代。**
>
> **两个生成物的行尾空格是真实的门**：`npm run gen:api` 的 ts-rs 输出带行尾空格，
> 编辑器/格式化器一旦剥掉，`openapi drift` 与 `test:wire` 双双变红。
> 生成物一律提交**生成器的原样输出**，不要手改。

---

## 11. 切片计划

**已合入**：

| 片 | PR | 合入后系统行为 |
|---|---|---|
| 1 — `task` 块 kind + 校验面 + 谓词下沉 | #988 | **零变化**（不写任何 `tasks` 行）|
| 2 — 写口收口 + 人的块级写口 + 链接扫描 | #990 | **仍然惰性**（不投影）|
| 3a — 冻结 + 反向索引 + 一级检测 + fail-closed sweep + 判决强制点 | #991 | 调度决策逐位不变；事件面已通电 |
| 3b — 投影 + rebuild + 唯一可调度谓词 + 存量护栏 | #994 | **声明从此有后果** |

### 切片 3b′ — 冻结通电 + 3a 加固（~1100 行，**部署硬门**）

**这一片之前，`task` 块的写口不得对人开放。** 3b 已经能投影出 `origin='block'`
的行并驱动调度，而冻结闭包的输入端整条没有接线 ⇒ 第 5 章对它唯一要保护的那类行
**恒为空**：`TaskContextFrozen` 照发、sweep 照跑、metrics 照导出，全部返回
「没问题」。这比切片 1/2 的「惰性」更坏，是**假装通电**。
**合入 main 与对人可用可以分开；两者必须进同一次发布。**

| # | 交付项 |
|---|---|
| 1 | **冻结根接线 + claim 栅栏**：按 `(wave_id, key)` 定位根块 → `resolve_closure` → 传进 `task_claim_pending_tx`。**栅栏的正面表述：claim 事务内、状态翻转之前，复核 (a) 根块 `(block_id, content_hash)` 与 (b) 闭包涉及的每个不同 `wave_id` 的报告 `doc_rev`；任一不一致 ⇒ 回滚 claim、race-lost 重来。** `doc_rev` 已镜像进 `cards.payload`，同事务普通 `SELECT` 可读，**不需要加载 automerge 文档**。**`doc_revs` 只进 `TaskContextFrozen` 事件，不落任何 `tasks` 列，严禁写进 `claim_context_json`**（该列是纯数组、解析失败即判 material，`0067` 已在生产 backfill `'[]'`）。**每个 wave 的 `doc_rev` 先于该 wave 首个块读取**；wave / 报告卡缺失即视为不一致；**栅栏判据只来自本轮内存中的 map**（§5.2「三件事必须写死」）。定位失败不分瞬时 / 确定性，一律不下判决、按变体计数并 `warn!` 后 race-lost；`task_claim_pending_tx` 不得带 `context_stale_at_ms` 谓词。**拆 `ResolveError` 变体**，`.ok()?` 改显式冒泡，**禁止按错误字符串推断** |
| 1a | **ready capacity 按成功 claim 记账**：不得在候选集上预先 `.take(capacity)`；逐个尝试 ready 行，仅成功 claim 才消耗一个名额，直到成功数达到 capacity，保证普通 race-lost 与定位失败都不队头阻塞 |
| 2 | **相等式去 `rev`**：只比 `(wave_id, block_id, content_hash)`，**恒比哈希、不得以 rev 短路** |
| 3 | **根块哈希收窄到 9 字段**；深度 ≥ 1 不收窄。**先把 `TASK_FIELDS` 从函数内局部常量提升为模块级 `pub`**（集合相等元测试位于 `calm-server`，而常量位于 `calm-types`，跨 crate 的 `pub(crate)` 不可见），再配集合相等元测试（否则测试只能复制一份「期望全集」，是一条自证自明的空洞断言）。写死 canonical 投影函数 + 冻结集加**显式 root 标记** |
| 4 | **3b′-ii（已做）：撤回规则**：`tasks` 增 `decl_ready` / `decl_released_by_user`；**只在 `project_tasks_tx`** 执行（`evaluate_schedulability_tx` **读路径也在用**，只产诊断）；`released_by_user` 带策略条件；`BlockVerdict` 增 `withdrawal` 作回传通道 |
| 5 | **3b′-ii（已做）：单赢家原语 + 事件管线**：提取 `mark_context_material_tx`，三条路径共用，归因 `ActorId::Kernel`；`TaskProjectionOutcome` 增 `kernel_events`；`tasks_rebuild_tx` 透传；**wave PATCH 从 `write_with_events_typed` 迁到 `write_with_actor_events_typed`**（不迁则 403）；禁止 `_tx` 内 `event_append_in_tx` |
| 6 | **`closure_truncated` 进 sweep**：与事件路径同形；改掉钉住反向行为的那条测试断言 |
| 7 | **3b′-ii：in-flight 瞬时失败不下判决**：`refs_match` 改三态；`tasks` 增 `context_verify_failures`；**三个吞错点都改**（`wave_get` / `load_block` / `cove_get_system().ok().flatten()`）；连续 3 轮升级。claim 前 pending 路径不升级，始终遵守交付项 1。**同片补腿 2**：把 `task_projection.rs` 的 refs 检查由目标 wave/card 存在下沉到目标**块**存在，并对 `goal` / `acceptance` 的 `scan_links` 结果施加同一判定，只做深度 ≤ 1；这是 `evaluate_schedulability_tx` 既有跨表读的同构扩展，不新增 §3.8 的读时数据源抽象。可推迟到此项，因为它只缩小残余集合并给 3c 诊断供料，而 3c 与 3b′ 本就必须同一次发布，不产生对人可见窗口。 |
| 8 | **tick 顺序 + boot 门**：周期 tick 的上下文 sweep 排在 `sweep_all` **之前**并补源码序断言；**任何一次成功 sweep 即原子开门，仅在翻转时补跑 `sweep_all`** |
| 9a | **3b′-i（已做）：冻结事件补齐 + 向后兼容**：`TaskContextFrozen` 补 `truncated` / `doc_revs` / root 标记。字段 `#[serde(default)]` + `task_id` 兼容读 + 历史事件可读回归。Tier-A 全流程 |
| 9b | **3b′-ii（已做）：裁决事件补齐 + 向后兼容**：`TaskContextAdvanced` 补 `changed_refs` / `wave_id` / `task_key` / `rationale`。字段 `#[serde(default)]` + `task_id` 兼容读 + 历史事件可读回归。Tier-A 全流程 |
| 10 | **migration `0070_`**：§9.2 的三列，连类型、backfill SQL、reader 分工、「三列刻意不进 `TASK_COLUMNS`」的旁注一起写在同一个文件里。`0069_` 是防御性清理；目标状态在已发布版本不可达，仅给曾含 claim 侧误写的开发分支兜底 |
| 11 | **3b′-ii（已做）：诊断对齐**：生产里已有第二份 in-flight 比较集，其 `withdrawal_diagnostic` 逐字承诺「any gate operation that has not started will be rejected」——收窄之后改 `priority` 会看到这句话而 gate 不会被拒，**诊断在说谎**。显式对齐两个字段集，并断言诊断承诺与 `context_stale_at_ms` 一致 |

**验收**：§10.3 + **附录 D.2 的 3b′ 四条补充项**（㉒ 元测试的常量引用、
㉓ 两条路同结论、㉕ 开门后当轮完成 `resume_dispatched`、㉖ SQL / migration 回归）。

### 切片 3c — 前端：状态回显 + 诊断渲染（~350 行）

**代码依赖 3b；发布依赖 3b′**，且**必须与 3b′ 进同一次对人可见的发布**（§12.2 A 硬约束）——
3b 是「行会被守卫式删除」的第一片，而诊断渲染在 3c，中间的窗口里系统第一次做决定
却完全沉默。

交付：`task` 块的状态回显（§3.8）；`taskDiagnostics` 渲染；
**`released_by_user` 的人用放行开关**（人可写、spec 不可写，§3.7 规则 5）——
**缺它则 `declare-and-wait` 这一档在 UI 上无法放行**，而人在确认框里选「要」会把该 wave 切到这一档，**出口必须存在**。
**每一类诊断都要有「人话 + 下一步动作」（§12.2 C 已裁决，全部采纳）**，
`Diagnostic` 结构定死为
`{ code, path, message_args, related_block_ids, related_wave_id?, action?, message }` ——
重复 `key` / 环 / 被 ceiling 挤出 / 引用失效四类的原因都**不在当前块上**，
没有相关块 id 就无法跳转。

**必须覆盖的诊断类别与各自要说清的东西**：

| 诊断 | 必须说清 |
|---|---|
| 重复 `key` | 哪两个块撞了（带跳转）、改哪个 |
| 依赖成环 | 环上的**完整 key 序列**（带跳转）|
| 未知依赖 | key 不存在 / 只是一条 legacy 行（**存量库上线当天会冒出一批，必须预置文案**，§4.2 规则 3‴）|
| 缺 gate / `no_gate_reason` | 该 wave 打开了 `require_task_gates`，二选一 |
| 超 `spec_task_ceiling` | 当前 ceiling、当前占用、容量大于零时**你被谁挤出、为什么（块序 + `key` 升序）**；容量为零时不得伪造关联块，只显示「提高上限」的 wave 入口 |
| 越 cove / 引用解析失败 | 哪一条 `refs`、指向哪、怎么重新链接 |
| `closure_truncated` | 「引用链过深/过宽，此任务将按最保守方式判失效；请把上下文收敛进更少的块」|
| 墓碑挡住重声明 | 哪条墓碑、谁立的、**两个清除动作各自的后果**（删墓碑 = key 可重提；PATCH 策略 = 恢复自动化但保留否决记录）|
| 本 wave 处于 `declare-and-wait` | 本 wave 需逐条放行、放行开关在哪、怎么恢复自动化（**不要把成因说成墓碑**）|
| context-stale (a) 引用变了 / (b) 声明自己变了 | **两形态必须分得开**；(b) 必须说明「**worker 的产出仍在**（卡与日志都在），只是没有被验证过」（§6.5）|

**对外概念契约（§12.2 D 已裁决）** —— 3c 的**每一处渲染都必须能归到下面三句之一，
归不进去的就是实现细节泄漏**：

> **① 任务卡：写清目标和怎么算完成，打勾就排队。**
> **② 我删掉的任务会留一条「不做」的记录，AI 不能翻案；我可以撤回。**
> **③ 这个 wave 里，AI 提的任务是自动跑，还是等我点头。**

推论（本片必须做到的三件事）：`released_by_user` 渲染成**按钮**而不是字段；
`declared_by` / `tombstoned_by` **不要求人填**（唯一合法值系统写入时已经知道，
由 `normalize_report_op` 顺手盖章）；`origin` / `content_hash` / 冻结集 /
`closure_truncated` / `if_doc_rev` vs `if_block_rev` / `effective_policy`
**只以句子出现，从不以名词出现**。

六项之外，`path` 是 §6.5 撤回判据的载体，不得按兼容展示字段删除。`message` 是既有
MCP 客户端的兼容文案，只能由服务端根据 `code + message_args` 派生，不得成为第二真源；`action` 只作为
前端选择人话动作与跳转的提示，`relatedWaveId` 用于引用目标 wave 的跳转；二者都不直出
字段名或枚举值。

**B2 连带的 UI 交付（§12.2 B 已裁决）**：人删除一条 `declared_by:"spec"` 的
task 块时，**当场弹确认框**「要不要此后 spec 的任务都等你放行？[要 / 只删这条]」；
选「要」即 PATCH `automation_policy = 'declare-and-wait'`。
另需**两个清除按钮**（删墓碑 / 恢复自动化）。

### 切片 4 — 第 2 级 LLM 裁决 + 冻结点推进（~700 行）

**§5.2 收窄之后紧迫性下降，可后排。** 它的主要价值是降误报，
而最大的误报来源（改 `priority`、撤销回原文）已被 3b′ 直接消灭。

判非实质后冻结点推进到新的 `content_hash`，每次推进带一条 `TaskContextAdvanced`
并同事务更新 `task_ref_index`；判 `material` **不推进**冻结点。

### 切片 5 — 模板 fork + `calm.plan.upsert` 退场（~1000 行）

`fork_report_from` + §7.3 的引用重写；`calm.plan.upsert` 从工具面移除
（形状复刻 `calm.task.dispatch` 的隐藏 shim：`visible_to_roles: &[]`、零写入、
返回迁移指引）。**`calm.plan.list` 保留**（读口）；**`calm.plan.cancel` 保留**
（in-flight 拒绝语义仍是唯一的取消入口）。

调用者已穷举：**只有 MCP spec agent** —— 无 REST 端点、无 web 调用者、
无 plugin 写口、无 admin CLI。

**manifest 过渡约束**：`plan_template` / `gates` / `spec_instructions` 三个字段
**保留可解析但标注为过渡**——它们是 **Tier-A 字段，不可删**。照清单施工时
不得顺手删掉它们。

**fork 的引用重写必须有硬测试，且要写清理由**：内部引用指向新 wave id、
**外部引用逐字节未变** —— **错了是静默的**（指回模板原文的链接看起来仍然合法），
所以必须专测。

**这一片之后新 legacy 行才真正停增**（§5.7 例外 (d) 的边界）。

### 切片 6 — 树级预算 + 深度上限 + `spawn: "sub-wave"`（~700 行）

§8 第三层 + 子 wave 创建 + 父块的 `neige://wave/<child>` 反链。
**前置检查**：把 `spawn` 移入 §5.2 的哈希纳入集，或证明它不影响已 claim 的执行
（§5.2 的版本化排除）。

### 切片 7 — 迁移物化工具 + cove 读时聚合（~700 行）

§9.1 第 4 项 + cove 级的读时聚合视图（不新建存储）。

### 依赖图

```
切片 1（惰性）→ 2（惰性）→ 3a（调度惰性、事件面通电）→ 3b（声明有后果）
   → 3b′（冻结通电，**部署硬门**）
        ├── 3c（前端，**必须**与 3b′ 同发布）
        ├── 4（LLM 裁决，可后排）
        ├── 5（模板 + upsert 退场）
        ├── 6（树预算 + sub-wave）
        └── 7（物化工具 + cove 聚合）
```

---

## 12. 风险与未解

### 12.1 机制侧

| # | 风险 | 定价 / 证伪装置 |
|---|---|---|
| 1 | **`content_hash` 是抗碰撞检测，不是不可能性证明** | 断言的精确形式见 §5.7，**不能写成全称命题**，也不能作为更强推理的前提 |
| 2 | **根块的 8 个排除字段构成一类「按设计不检测」的声明变更** | §5.2 已定价 + 可证伪条件；§5.7 例外 (a) |
| 3 | **根解析 → claim 的 TOCTOU 靠栅栏关闭，栅栏漏做即漏检** | §5.2 三件事写死 + §10.2 不变量 3c。**可观测量**：栅栏触发的 race-lost 频率；若高到影响吞吐，说明报告写与 claim 的争用比预期严重，需重新考虑解析点 |
| 4 | **claim 提交 → 判决落库之间 worker 可能先启动** | §5.7 (B) 如实写出。要更强保证只有两条路，**都不做**：(i) `prepare_tx` 重核闭包 —— 把最多 64 个节点的跨 wave 读放进**每一次** operation 启动的事务，数量级更贵且抖动会放大到全局；(ii) 文档版本租约 —— 与「文档永不拒绝合并」直接冲突 |
| 5 | **`spawn` 的哈希排除是版本化的** | 切片 6 前必须重裁（§5.2）。集合相等元测试保证「忘了分类」会红，但**不会**提醒「分类需要改变」 |
| 6 | **`spec_task_ceiling` 只约束未结存量** | 「每完成一条就再声明一条」不被它挡住。刻意不引入累计配额（§8）；证伪装置是声明速率可观测量 |
| 7 | **`decl_released_by_user` 的存量行豁免** | 升级时已在飞的任务不享有该位的撤回检测（§9.2）。有界、只影响一次升级窗口 |
| 8 | **反向索引换掉了编译期可见性** | 用 FK + trigger 而非显式 DELETE 原语 ⇒ 第十条生产者路径可以无声出现（§5.8）。sweep 末尾兜底使正确性不依赖它 |
| 9 | **`prepare_tx` 提交之后、spawn 之前的窗口** | **与 #4 不是同一个洞**（#4 是「claim 提交 → 判决落库」）。op 越过 `TxCommitted` 之后，恢复漏斗不再经过强制点 ⇒ 一条已被判 material 但恰好越过 `prepare_tx` 的 op，其 spawn 仍会发生。**不修的理由**：修法要求让 `prepare_tx` 与 spawn 落进同一次不可分割的推进，那是 operation 驱动层的改造，成本远超收益；窗口极窄且只影响单次 spawn，后续 gate 仍被拒 |
| 10 | **一次误报被变成不可逆终结** | sweep 判错 ⇒ 行落 `failed` ⇒ 人必须换 `key` 重开。**若切片 4 上线后误报率仍高，这条应重新评估**（例如改成可由人一键重开的形态）。这是误报定价的唯一出口 |
| 11 | **一次触发的重解析开销无常数上界** | 上界 = 冻结了该 wave 的 in-flight 任务数 × `MAX_REF_NODES` ≤ (Σ per-wave `task_budget`) × 64。**由 `MAX_RERESOLVE_FANOUT` 封顶**（超出即 fail-closed 判 material，不引入漏报）；封顶之外的代价靠可观测量监控 |
| 12 | **sweep 停摆** | 两条真实路径：(i) DB 出错「warn + 跳过本轮」会**静默降级**保证；(ii) reconcile tick 是 `tokio::spawn` 里的裸 `loop`，**一次 panic 静默终结该进程余生的所有 sweep**。健康信号（§5.5）只让它**可见**，不让它**不发生** —— 告警阈值 `3 ×` reconcile 周期 |
| 13 | **升级日 legacy 在飞行不计入 `occupied`** | 人看到的「本 wave 未结任务」口径会**暂时大于 ceiling**，随这些行终结收敛。且 `unknown_deps` 按规则 3‴ **必须把 legacy 在飞 key 视为已知**（否则存量依赖全变未知依赖），但**不得在 `occupied` 查询里无条件混入 legacy 行**（那会让准入不幂等）—— 两条方向相反，容易被实现成同一个 JOIN |
| 14 | **引用锚仍是 block id ⇒ 误中止潮** | 一次大幅整文档重写可能让 `refs[]` 批量失效 ⇒ 闭包解析不到 ⇒ fail-closed 判 material ⇒ **一批在飞任务同时终结**。缓解是「引用已失效，请重新链接」诊断 + marker 通道；**未量化**，上线后需观测 |
| 15 | **换 `key` 绕过人的否决，防线降为 opt-in** | B2 裁决（§12.2）把「墓碑 ⇒ 自动派生 `declare-and-wait`」改成删任务时的一次点击 ⇒ **人不点时该循环仍然存在**。本设计只保证**同 `key`** 被挡住（§4.2 规则 2 + §3.7 规则 3）。**回退条件**：观测到真实的换 key 循环 ⇒ 恢复 `effective_policy` 的第二分支 + wave 级横幅 + 确认框 + 两个清除按钮（§6.1）|
| 16 | **`task_budget` / `require_task_gates` 的既有写口不对称** | `update_wave` 对它们接受任何自述 actor，spec 仍可调高自己的并发度。那两列限的是**并发**，而本设计的护栏是**存量**与**能不能自动跑**，后两者已守住。属 #644 的面，应单开 issue |
| 17 | **投影暂时看不见深度 ≥ 2 的失效引用、被引用块自身不合法的 `refs`，以及整张报告卡缺席（`ReportAbsent`）** | pending 行会可逆地无限次重试 claim、反复定位失败；`ReportAbsent` 没有对应的投影守卫式删除路径。每次只付一次闭包重解析，不 spawn worker、不改状态，任一侧 wave 编辑即自愈，且成功 claim 才消耗 capacity，所以不拖住同 wave 其它任务。定价与证伪装置：`context_resolve_failures{variant}`；腿 2 在 3b′-ii 下沉块粒度检查以缩小 refs 侧集合 |
| 18 | **context-stale 的 (a)/(b) 形态尚未由 material 真因直接驱动** | 当前读投影以已有声明变更诊断区分 (b)，其余 stale 落入引用变化 (a)；未体现在声明影子位差异中的 material 成因可能误归为 (a)。代价是下一步动作可能指错到“重新链接”，但仍 fail-closed、不放行任务。切片 4 推进冻结点时须把 material 成因随判定结果持久化，并让读投影直接消费该真因。 |

### 12.2 产品侧裁决（2026-08-03 已拍板，四条全部按倾向落）

评审给了一个量化：本设计里面向「人会看到什么」的内容占比极低，七轮机制评审几乎
没有覆盖它。总判断是：**机制没问题，缺的是「一个人按直觉操作时能不能预测下一步」
这个维度。** 以下四条**都不改机制**，只改默认值、触发方式或渲染。
**四条已于 2026-08-03 全部按倾向裁定**：A1（升为硬约束）、B2（改为一次点击）、
C（全部采纳）、D（采纳为对外概念契约）。各条的现象、选项与定价原样保留在下面 ——
它们是「什么能推翻这条裁决」的依据。

---

#### A. 3c 与 3b′ 是否必须同一次对人发布

**现象**：3b 是「行会被守卫式删除」的第一片 —— 人写的 task 块可能因为诊断非空、
`ready` 撤回、ceiling 超限而**行被删掉**；而解释这件事的诊断渲染在 3c。
中间这个窗口里，**系统第一次做决定却完全沉默**：块还在文档里、`ready` 还是 `true`、
UI 上没有任何标记，任务就是不跑。

**这不是可见性延迟，是「无法归因的静默」**。它与 §4.2 规则 4 之所以坚持
「诊断零存储、读时派生」的理由同源 —— 那条裁决的全部意义就是「删行的原因永远
算得出来」，而算得出来却渲染不出来，等于白算。

| 选项 | 代价 |
|---|---|
| **A1 升为硬约束**（3c 与 3b′ 进同一次对人可见发布）| 3b′ 的发布被 3c（~350 行前端）拖住；但 3b′ 本来就是部署硬门，两者串起来只多一次前端迭代 |
| A2 保持「建议」 | 窗口期内任何一次删行都需要人来问、你来查库解释 |

**裁决：A1。** 理由不是洁癖：3b′ 之前 `task` 块写口本来就不对人开放（§11），
所以「对人可见」这条线本来就要跨一次，把 3c 并进那一次是**零额外窗口**的做法。

---

#### B. 墓碑 ⇒ `declare-and-wait` 的触发方式

**现象**：人删掉一条 spec 声明的任务（= 立墓碑，§6.1）⇒ 该 wave 的
`effective_policy` **自动**派生为 `declare-and-wait` ⇒ 该 wave 内所有
`declared_by='spec'` 且未放行的 **pending 行被守卫式删除**。

即：**删 A 导致 B、C 消失**。而且触发的人和读到诊断的人**常常不是同一时刻的同一个人**
—— 你删完就走了，三小时后回来发现整个 wave 停住了。

**机制本身是漂亮的**（§6.1）：零新列、与 `key` 无关、零相似度判断、
「人的不作为是吸收态」正是终止性质的要求。**争议只在默认触发方式。**

| 选项 | 代价 |
|---|---|
| **B1 自动派生**（原设计）| 每次否决都要付「全 wave 收紧 + 删掉别的 pending 行」的摩擦；诊断能解释，但要人回来看 |
| **B2 改为一次点击** —— 人删任务时 UI 当场问「要不要此后 spec 的任务都等你放行？[要 / 只删这条]」，选「要」就写显式 `automation_policy = 'declare-and-wait'` | **机制一个字都不用改**（显式设置本来就是第一分支；**B2 落地后第二分支已删**，§6.6）。代价：人不点时，「spec 换 key 重提绕过否决」的循环仍然存在 |

**裁决：B2。** 定价理由：那条循环的真实频率**从未被观测到**（它的证伪装置本身是一条
尚未上线的可观测量），而「全 wave 收紧 + 删掉别人的 pending 行」的摩擦是**每次
否决都要付**。**用一个未观测到的风险，去换一个必然发生的困惑，在使用者一侧是亏的。**

**若日后退回 B1**（见 §6.1 的回退条件），则至少要求三件事：wave 级的**常驻横幅**（不能只是块级诊断）、
触发当时的**确认对话框**、以及两条清除办法必须是**按钮**而不是文案
（§6.6 已定义两条：删墓碑 / 显式 PATCH 策略）。

---

#### C. 诊断的「人话 + 下一步动作」

**现状**：§11 切片 3c 只点名了三类文案（人工否决 / context-stale 两形态）。
而实际会打到人脸上的诊断至少还有 **8 类**：重复 `key`、依赖成环、未知依赖、
缺 gate / `no_gate_reason`、引用越 cove、引用解析失败、`closure_truncated`、
超 `spec_task_ceiling`、墓碑挡住同 key 重声明。

**最刺眼的一个例子**：ceiling 的准入顺序是「块在文档中的顺序、同序按 `key` 升序」
取前 `capacity` 个（§4.2 规则 3）。于是 ——

> **我在文档中间插入了一个新任务，文档下方一个本来在队列里的任务被挤出去了。**

没有任何直觉能预测这件事，而目前没有任何文案要解释它。

**同时 `Diagnostic` 的结构必须定死**：`{ code, path, message_args, related_block_ids,
related_wave_id?, action?, message }`。`message` 是由前两项派生的兼容字段，不是第二真源。因为重复 `key` / 环 / 被 ceiling 挤出 / 引用失效
**四类的原因都不在当前块上** —— 没有 `related_block_ids`，UI 只能渲染一句话，
人得自己在文档里找。

| 诊断 | 必须说清 |
|---|---|
| 重复 `key` | 哪两个块撞了（带跳转）、改哪个 |
| 依赖成环 | 环上的完整 key 序列（带跳转）|
| 未知依赖 | 依赖的 key 不存在 / 只是一条 legacy 行（**存量库上线当天会冒出一批，必须预置文案**，§4.2 规则 3‴）|
| 超 ceiling | 当前 ceiling、当前占用、**你被谁挤出、为什么（块序）**、以及「提高上限」的入口 |
| 越 cove / 引用失效 | 哪一条 `refs`、指向哪、怎么重新链接 |
| `closure_truncated` | 「引用链过深，此任务将按最保守方式判失效」|
| context-stale (a)(b) | 两形态必须分得开；(b) 必须说明「**worker 的产出仍在**，只是没有被验证过」（§6.5）|

**裁决：全部采纳** —— 这一条没有取舍，只有工作量。落点见 §11 切片 3c。

---

#### D. 对外概念是否收敛为三句话

**现状**：会以某种形式进入使用者视野的概念数了 **30 个以上**（`key` / `ready` /
`declared_by` / `tombstoned_by` / `released_by_user` / 墓碑 / `spawn` / `refs` /
`depends_on` / `gate` / `priority` / `cwd` / `context` / `automation_policy` /
`effective_policy` / `spec_task_ceiling` / `task_budget` / `origin` / 冻结集 /
`content_hash` / stale / material / `closure_truncated` / `if_doc_rev` vs
`if_block_rev` / 模板 overlay / cove / …）。

**分类**：

- **必要且必须让人理解**（6 个）：任务本身（key/goal/验收/gate/依赖）、`ready`、
  墓碑、`declare-and-wait`、诊断、执行史；
- **必要但不该由人书写**（3 个）：`declared_by` / `tombstoned_by`
  （**填错就 400、填对等于没填** —— 唯一合法值系统写入时已经知道，
  建议由 `normalize_report_op` 顺手盖章）、`released_by_user`（应当是**按钮**不是字段）；
- **实现细节泄漏**（其余全部）：`origin`、`content_hash`、冻结集、
  `closure_truncated`、`if_doc_rev`/`if_block_rev` 的区分、`effective_policy`
  这个名字 —— 这些应当**只以句子出现，从不以名词出现**。

**建议的对外概念契约**：

> **① 任务卡：写清目标和怎么算完成，打勾就排队。**
> **② 我删掉的任务会留一条「不做」的记录，AI 不能翻案；我可以撤回。**
> **③ 这个 wave 里，AI 提的任务是自动跑，还是等我点头。**

**用法**：把这三句写进设计作为对外概念契约，并要求 **3c 的每一处渲染都能归到
其中之一** —— **归不进去的，就是实现细节泄漏**。

**裁决：采纳。** 它不增加工作量，只是给 3c 一把筛子。落点见 §11 切片 3c。

---

**一条产品叙事必须修正**：`guard_non_prose_stomp` 让整文档写路径碰 `task` 块一律
400 —— 即「文档即计划」里**计划那一半在 markdown 编辑器里改不动**。
**裁决本身是对的**（放松它等于让一次无关的整文档重写静默删掉正在执行的任务），
但主张里「人看得见**改得动**」这句话不再成立。正确表述：
**报告是文档 + 结构化任务卡的混合体，任务卡只能在卡片 UI 上改。**

**一条设计从未正面回答、但与 §2 同时成立的推论**：
**文档是计划的真源，但只对还没开始跑的任务是真源。** 跨过 claim 之后，
「声明式更新」的语义翻转成「你的编辑让这次执行作废」，而分界线
（`pending` vs `dispatched`）人看不见，且 `task_budget` 默认 1 + `auto-declare`
让这个窗口只有几秒。这不需要改机制，但产品叙事需要承认它，
UI 应给 task 块两种截然不同的形态（草案/可改 vs 已交付/只读）。

---

## 附录 A：裁决记录

每条含「什么能推翻它」。

| # | 问题 | 裁决 | 什么能推翻它 |
|---|---|---|---|
| **Q1** | 模板放在哪个 wave | **fork 任意既有 wave 的报告；「是模板」只是 kernel overlay 标记** | 出现「模板需要独立于任何 wave 的生命周期」的真实需求（如跨 cove 共享）|
| **Q2** | `calm.plan.upsert` 废弃还是保留 shim | **从工具面移除**（隐藏 shim，零写入，返回迁移指引）；`plan.list` / `plan.cancel` 保留 | 发现某条恢复/重放路径依赖 upsert 的写能力 |
| **Q3** | 块 id 作为 task 身份 | **否，身份是 `key`**（§3.3）| `key` 的重复率高到让人烦 ⇒ 块级写口自动 uniquify |
| **Q4** | 模板引用 vs 复制 | **复制**（§7.4）| 「模板一改、所有在跑的 wave 立刻跟进」成为真实且高频的需求 |
| **Q5** | 谁选模板 | **人选**，spec 可提议 | spec 提议的接受率高到人只是在盲点头 |
| **Q6** | 就绪判据落在哪 | **机器门 + 文档里的 `ready` 字段**，默认 `auto-declare`（§6.3 / §6.6）| `ready: true` 但仍产出垃圾的比例高 ⇒ `acceptance` 校验太弱 |
| **A1** | 一个 task 一个子 wave | **否，默认 in-wave**（§3.5）| 父报告被淹没到不可读，或 per-wave budget 成为无法配置解决的瓶颈 |
| **A2** | 冻结元组的形状 | **四元组存储、三元组判定、根块 9 字段**（§5.2）| 出现「改了 8 个排除字段之一、撤回方向也未覆盖、而 worker 应当停下」的场景 |
| **A3** | 冻结根的载体 | **claim 前按 `key` 定位，不持久化 block id**（§5.2）| —— |
| **A4** | `doc_revs` 的载体 | **只进事件，不落列**（§5.2）| 出现一个真实的、必须从 `tasks` 行直读 `doc_revs` 的诊断需求 |
| **A5** | `ready` / `released_by_user` 怎么管 | **移出哈希，改用方向敏感的投影层撤回规则**（§5.3）| —— |
| **A6** | 撤回规则的前值载体 | **`decl_ready` / `decl_released_by_user` 两列**（§5.3 / §9.2）| —— |
| **A7** | 「新产生诊断」算不算撤回 | **不算**（远距离误杀，§5.3）| —— |
| **A8** | 瞬时失败怎么处置 | **不下判决 + per-row 计数 + 连续 3 轮升级**（§5.5）| —— |
| **A9** | 判决的强制点 | **四个 task 绑定 adapter 的 `prepare_tx`**（§5.6）| 出现「任务行可以回到 `pending`」的机制 ⇒ 载体语义要重新定义 |
| **A10** | 两个 NEW 事件的 role_gate | **严格 `Kernel \| KernelDispatcher`，例外条款不是同构**（§2.1）| —— |
| **A11** | `spec_task_ceiling` 的谓词 | **`occupied` 只数在飞行，`pending` 是产物不数**（§4.2 规则 3）| ceiling 改成跨 wave / 跨树的量 ⇒ 准入顺序需重新定义 |
| **A12** | 人的否决能否被换 key 绕过 | **本设计只挡同 `key`；换 key 的循环由 B2 的一次点击（人显式切 `declare-and-wait`）挡，不再自动派生**（§6.1，2026-08-03 改） | **观测到真实的换 key 循环** ⇒ 恢复 `effective_policy` 的第二分支（自动派生），并配 wave 级横幅 + 确认框 + 两个清除按钮 |
| **A13** | 三列进不进 `TASK_COLUMNS` | **不进**，各配定向 reader（§9.2）| —— |
| **P1** | 3c 与 3b′ 是否同发布 | **必须同一次对人可见发布**（硬约束）| 3c 的工期长到拖垮 3b′ 的部署窗口 |
| **P2** | 墓碑 ⇒ `declare-and-wait` 的触发 | **一次点击（确认框），不自动派生**；机制未改，只改触发 | 同 A12 |
| **P3** | 诊断的人话与下一步 | **全部采纳**：10 类诊断各自的必说内容 + `Diagnostic` 带 `related_block_ids` | —— |
| **P4** | 对外概念 | **收敛为三句话，作为 3c 的筛子**：归不进三句的渲染即实现细节泄漏 | 出现一个真实必要、却归不进三句的概念 |

---

## 附录 B：评审处置历史

八轮双通道（codex + 独立 subagent），每一条发现**先对着源码复核，再决定采纳或驳回**；
驳回的一律附反证。

### B.1 轮次概览

| 轮 | 发现规模 | 性质 |
|---|---|---|
| r1 | 31 条（接受 27 / 驳回 4）| 四处**改变设计方向**：`declared_by` 迁进块 payload、冻结集加 `content_hash`、`auto-declare` 默认翻转、切片计划整体重切 |
| r2 | 21 条（全部接受，驳回 3 条补救手段）| 闭包根是 task 块自身、闭包不得跨 cove、闭包展开移出 claim 事务 |
| r3 | 19 条（事实全部成立）| 墓碑权属载体独立、`released_by_user` 放行位、`spec_task_ceiling` |
| r4 | —— | **fail-closed 全量 sweep** 成为正确性载体，事件路径降级为延迟优化 |
| r5 | —— | 判决的持久载体 `context_stale_at_ms`；`claim_context_json` 的 backfill（⑱）|
| r6 | —— | 强制点从 `resume_dispatched` 改为四个 `prepare_tx`；boot 顺序前移 |
| r7 | —— | ceiling 谓词的幂等形状（⑳）|
| **r8 系列** | 见 B.2 | **首次包含已合入实现**的评审 |

### B.2 r8 系列：七轮，逐层下沉

r8 系列的形状很清晰，值得记下来作为方法论：

> **r8「裁决错」→ r8b「改得不够远」→ r8c「机制无载体」→ r8d「载体无写入时刻/
> 归因/迁移」→ r8e「载体的列选型/backfill/事件出口」→ r8f「边界收紧」→
> r8g「裁决没下沉到施工清单」**

| 轮 | 关键发现 |
|---|---|
| **r8** | 三通道（外部心智 / 内部机制 / 文档一致性）。**3b 合入后 §5 是「假装通电」** —— 冻结输入端整条没接线；`closure_truncated` 只落事件路径；tick 顺序反了；boot sweep 一次失败永久关门；两个事件 payload 被裁 |
| **r8b** | **2 BLOCKER**：㉑ 的「rev 相同 ⇒ 跳过哈希」短路把 r1 已判定的两个漏报入口原样放回来（同一节的上一段刚证过它假）；㉔ 的「解析 → claim」是 TOCTOU。另：㉒ 漏了 `kind`（8+8=16 ≠ 17）、错误排除 `ready` / `released_by_user` |
| **r8c** | **3 BLOCKER**：栅栏只覆盖单个文档（跨 wave 子节点漏在外面）；`released_by_user` 纳入哈希在默认策略下是新误杀；「瞬时 vs 确定性」在现有 `ResolveError` 上不可判定。另：同一病灶在 sweep 侧已上线（root cause 级）|
| **r8d** | **载体补全轮**：撤回规则的边沿没有前值载体（rebuild 判不出「曾经是 true」，且照字面实现会**杀光该 wave 所有 `auto-declare` 在飞任务**）；落点未指定（`evaluate_schedulability_tx` **读路径也在用**，写在那里会让一次 GET 产生持久判决）；栅栏的 `doc_rev` 缺载体/时序；「连续 N 轮」用错了计数器（全局、每轮成功即清零 ⇒ 永不达成）；断言 (A) 漏了第四个例外 |
| **r8e** | **载体选型轮**：`doc_revs` 塞进 `claim_context_json` 会造成跨升级必然 Stuck-ops（**与 r5 ⑱ 是同一个洞，被重新打开了一遍**）；判决事件出 `project_tasks_tx` 没有载体且 wave PATCH 那条边运行时必然 403；例外 (d) 的判据划错范围；三列 migration backfill 完全没写 |
| **r8f** | **首次零 BLOCKER**。三个机制边界收紧：栅栏生命周期状态机、三列不进 `TASK_COLUMNS`、`kernel_events` 消费契约 |
| **r8g** | **收口轮**：全文级扫描后诊断 —— **机制章节之间已无残留矛盾，矛盾全部出现在 §12 切片清单与前面章节之间**。最刺眼的一条：切片 3b′ 交付项逐字要求把 `doc_revs` 写进 `claim_context_json`，而那正是 r8e 判为 BLOCKER 的做法 ⇒ **照清单施工必然复现已定价的事故**。三条裁决：撤回 `claim_doc_revs_json` 列、`kernel_events` 量词、`BlockVerdict` 回传通道 |

### B.3 驳回记录（附反证）

| 主张 | 反证 |
|---|---|
| 「墓碑可被删除」是洞 | `user_owned` 判定覆盖 `tombstoned_by == "user"`，spec 删人的墓碑被拒；而「人删自己的墓碑」「spec 删自己的墓碑」是 §6.1 明确规定的撤销路径 |
| 「每次 sweep 成功即开门」会破坏 boot 门 | **把两个门当成了一个**。`boot_sweep_done` 守 `sweep_all`（注释逐字写着防 "re-drive dispatched rows against unrecovered operation rows"），本裁决一个字没动它；`context_sweep_boot_done` **只**守 `resume_dispatched`。且 `resume_dispatched` 是从 `sweep_reconcile` ← `sweep_all` 调下来的，`sweep_all` 在 `boot_sweep_done` 之前**整体 no-op** |
| (B) 的拆分与 §5.6 冲突 | §5.6 的条件本来就是「**判 material 之后**不得再开始」，「claim 提交 → 判决落库」那段延迟**不构成矛盾** |
| 把 `spec_task_ceiling` 改成累计配额 | 单调计数器不是当前文档的函数 ⇒ rebuild 重建不出 ⇒ 成为 §2 承重墙上的第三个真源 |
| 「冻结规范字节本身」 | 256 KiB × 64 = 16 MiB/claim 进事件 payload |
| 「引入持久的块化身 id」 | 动身份层，换的检测强度与 32 字节哈希相同 |
| 「切片 3a 先不给 legacy 行发空 `TaskContextFrozen`」 | 那会让不变量 3 到 3b 才第一次通电，交付一条**从未被真实流量执行过**的硬不变量 |

### B.4 实现期发现的、设计评审七轮没抓到的

| 发现 | 教训 |
|---|---|
| `unknown_deps` 的签名不能收 `&[Task]` —— `Task` 在 `calm-truth`，`calm-types` 依赖不了它（crate DAG 反向）| 设计里的函数签名要对着 crate 依赖方向核 |
| 强制点被接到了 `*-create`（用户手动建卡）而不是 `*-worker` 适配器，**四个强制点里三个根本不存在**，而测试是绿的 —— 因为 fixture **自己在 `prepare_tx` 里调那个 helper** | **测试必须驱动生产接线**。修法不止于接对：删 fixture、改走真实 adapter、加注册表集合相等元测试 |
| 冻结相关测试全部用 raw SQL 直接种 `claim_context_json`，于是「生产 claim → 冻结」**从来没被任何测试走过一次** | 这是上一条的**残留形态**：fixture 问题修了，但数据预置的旁路还在。3b′ 的验收因此明确**禁止 SQL 预置该列** |
| `bbaa62b5` 把 `ReportDocOp::MoveBlock` 的前置从 `Option<u32> if_rev` 改成 `u64 if_doc_rev` —— 这是**内核 op 层**变更、影响所有块 kind，设计只把它描述成 MCP 工具契约迁移 | 契约迁移要区分「工具面」与「内核 op 层」 |
| `e4696f7a` 的 `refuse_if_context_stale` 对**查不到 task 行**也 fail-closed，而三个 worker adapter 此前根本不查 task 行 ⇒ wave 中途被删的 in-flight worker op 现在会在 `prepare_tx` 终结失败 | 新增 fail-closed 分支要枚举「此前不走这条路的调用者」 |
| 未合并开发提交 `1915601f` 曾让 claim 前确定性定位失败把 `pending` 行写成 stale；该状态在已发布版本中从未可达，但若合入就会叠加 claim stale 谓词与 ready `.take(capacity)`，形成僵尸行 | 分支内同样要核对载体、枚举源与复活路径；capacity 按成功 claim 记账。实现期测试必须经生产 claim 路径构造失败，禁止 SQL 预置 |
| `MalformedStoredReport` 在实现中被归为确定性，但 §5.2 仍把它列为 retryable；损坏的持久报告不会自愈，等待 3 轮只会延迟同一判决并持续占用 in-flight 名额 | 实现期裁决必须按 §6 同步回写设计正文，并横扫章节、交付清单与附录，不能只改单个实现分支 |

### B.5 方法论：四格纪律的来历

r8d–r8f 三轮的 BLOCKER 有同一个形状 —— **「我写了『用 X 承载』，但没写『X 怎么被
创建 / 传递 / 迁移』」**：

- 撤回规则说了「比较 `true → false`」，没说前值存在哪 ⇒ rebuild 判不出；
- 栅栏说了「比较 `doc_rev`」，没说它存在哪、什么时候采 ⇒ TOCTOU 原样复现；
- 「连续 3 轮」说了要计数，没说 per-row 存在哪 ⇒ 用了全局计数器，结构上永不达成；
- 判决说了「归因 Kernel」，没说事件怎么出函数 ⇒ 要么被闸拒、要么绕过广播。

因此本文对每条机制强制「**载体 / 谁写 / rebuild 怎么重放 / migration 怎么
backfill**」四格。这四个问题只要有一个空着，就是下一轮的 BLOCKER。

---

## Related

- **#973** —— 单写者原则（本设计是它在计划面的延伸）
- **#979 / #986** —— 整文档 / REST 写路径的 `if_rev`（已独立合入，**不阻塞本设计**）
- **#760** —— workflow 即插件平台（本设计拆解其 descriptor）
- **#891** —— `workflow_input` 保留且与模板正交（§7.6）
- **#830** —— workers run headless（worker 级 human-in-loop 不在本设计内）
- **#761** —— workflow 组合（模板化后被削弱，但依赖/顺序语义仍需设计）
- **#976** —— 活数据块（本设计不作共用机制承诺，只约束不引入新读时抽象）
- **#955** —— 内核 ↔ app 能力边界（§1.3 判据依据）
- **#330** —— 「产出与证据，不是协作文档平台」（§7.4 取舍动机）
- **#644** —— `tasks` / scheduler / gate 现状；`task_budget` 写口不对称属它的面
- **#653** —— parked 原语的真实能力边界（§3.3 的「不走补偿路径」依据）
- **#410** —— 共享 codex daemon（§3.5 成本计算的依据）

---

## 附录 C：载体与常数总表

**这张表是压缩的下界。** 正文的四格纪律要求每条机制填满
「载体 / 谁写 / rebuild 怎么重放 / migration 怎么 backfill」；本表把全部载体
集中一处，便于施工时逐行核对。**新增机制必须在此登记一行。**

### C.1 `tasks` 表的新列

| 列 | 类型 | 谁写 | 谁读 | rebuild | backfill | 片 |
|---|---|---|---|---|---|---|
| `declared_by` | `TEXT NOT NULL DEFAULT 'spec'` | 投影（块 payload 的副本）| 投影、预算 | 从文档重建 | 存量全标 `spec` | 3b |
| `origin` | `TEXT NOT NULL DEFAULT 'legacy'` | 投影 / 收编 | 投影只管 `'block'` | 从文档重建 | 存量全 `legacy` | 3b |
| `claim_context_json` | `TEXT NULL`（**纯 JSON 数组**）| claim 事务 | `detect_wave_edit` / `sweep_inner`（**解析失败即判 material**）| `TaskContextFrozen` 的投影 | **`'[]'`**（`NULL` = 缺失 ≠ 空）| 3a |
| `context_stale_at_ms` | `INTEGER NULL` | `mark_context_material_tx`（**单赢家，且只写 in-flight 行；claim 路径与任何 `pending` 行一律不写**）| `refuse_if_context_stale` | `TaskContextAdvanced` 的投影 | `0069_` 防御性幂等清理不可达的 pending stale 状态（`NULL` = 从未判 material）| 3a / 3b′-i 修复 |
| `context_closure_truncated` | `INTEGER NOT NULL DEFAULT 0` | claim 事务 | `detect_wave_edit` **与 `sweep_inner`（两处必须同形）** | `TaskContextFrozen.truncated` 的投影 | `0` | 3a |
| `decl_ready` | `INTEGER NOT NULL DEFAULT 0` | pending 投影；收养路径初始化为 1 | `FrozenDeclarationRow` 定向 SELECT → `BlockVerdict.withdrawal` | 当前状态的纯函数 | **`1` for in-flight `origin='block'`** | 3b′-ii |
| `decl_released_by_user` | `INTEGER NOT NULL DEFAULT 0` | pending 投影；存量 legacy 收养不初始化 | 同上 | 同上 | **保持 `0`，显式例外；升级时已在飞 legacy 行永久缺少该位的撤回前值，新 claim 不受影响** | 3b′-ii |
| `context_verify_failures` | `INTEGER NOT NULL DEFAULT 0` | sweep 定向 SQL | sweep | 运行期计数，不需重放 | `0` | 3b′-ii |

**三列（`decl_*` / `context_verify_failures`）刻意不进 `TASK_COLUMNS` / 公共 `Task`**
——`TASK_COLUMNS` 服务通用 `Task` 查询，塞进去会扩大 model / 序列化 / OpenAPI /
TS / 所有 `query_as::<Task>` 的连带面。**在 migration 旁注明这是刻意的。**

### C.2 `waves` 表的新列

| 列 | 类型 | 谁写 | rebuild | 片 |
|---|---|---|---|---|
| `spec_task_ceiling` | `INTEGER NULL`（默认常数 32）| **只有 `EditAuthor::User` 经 `WavePatch`**；spec 403 | 列本身即真源 | 3b |
| `automation_policy` | `TEXT NULL`（**三态**：NULL = 内核默认）| 同上 | **列本身即真源**：`effective_policy` = 该列非 NULL 时取列值，否则 `auto-declare`（**B2 后不再依赖文档墓碑**）| 3b |
| `parent_wave_id` / `tree_task_budget` | `TEXT NULL` / `INTEGER NULL`（默认 32）| 内核（wave 创建）| —— | 6 |

### C.3 不落列的载体

| 载体 | 住哪 | 为什么不落列 |
|---|---|---|
| `doc_revs`（栅栏基线）| **只进 `TaskContextFrozen` 事件 payload** | 栅栏判据来自本轮内存中的 map，且硬裁永不进 sweep/detect ⇒ 落列即一列纯写不读的持久状态，连带一条结构性不可达的「NULL 不可解释成不一致」规则 |
| `withdrawal` | **`BlockVerdict` 的字段**（进程内回传）| 判点与读点分离，需要通道而非持久状态 |
| `kernel_events` | **`TaskProjectionOutcome` 的字段** | 事件必须由外层 eventized write 统一过闸 |

### C.4 常数总表（**全部是猜的，校准装置见 §8**）

| 常数 | 值 | 用途 | 耗尽行为 |
|---|---|---|---|
| `MAX_REF_DEPTH` | 3 | 闭包传递展开深度 | 标 `closure_truncated` ⇒ 此后一律 `material` |
| `MAX_REF_NODES` | 64 | 闭包节点数 | 同上 |
| `MAX_RERESOLVE_FANOUT` | 64 | 第 1 级：一次编辑触发的重解析扇出 | 超出即直接 `material` |
| `MAX_ADJUDICATION_FANOUT` | 16 | 第 2 级：裁决扇出 | 剩余任务直接 `material` |
| `MAX_STRING_CHARS` | 2048 | 单个字符串字段上限 | 校验拒绝 |
| `MAX_CANONICAL_BYTES` | 256 KiB | 单块 canonical bytes 上限 | 校验拒绝 |
| `MAX_SWEEP_NODES` | 4096 | 单轮 sweep 的重解析预算 | 本轮剩余未验证的一律 `material` |
| `DEFAULT_WAVE_TASK_BUDGET` | 1（既有）| per-wave 并发 | —— |
| `DEFAULT_SPEC_TASK_CEILING` | 32 | 单 wave 未结存量 | 超限不落行 + 诊断 |
| `tree_task_budget` | 32 | 树内未结存量（切片 6）| 子 wave 创建被拒 |
| `MAX_WAVE_TREE_DEPTH` | 3 | 递归深度（切片 6）| 同上 |
| 瞬时失败升级阈值 | 3 轮 | per-row `context_verify_failures` | 升级为 `material` + 告警 |
| `NEIGE_SCHEDULER_RECONCILE_SECS` | 300（既有）| sweep 节奏 | **不新增环境旋钮** |

### C.5 健康信号

| 信号 | 类型 | 为什么必须有 |
|---|---|---|
| `context_sweep_last_success_age_seconds` | **正向 gauge** | 其余全是计数器 —— sweep 整体不跑时它们**一起静止**，看起来和「一切正常」一样 |
| `context_sweep_consecutive_failures` | 计数器 | 与上者**在成功与失败两条路径上都导出** |
| 检测次数 / 送裁决次数 / `material` 次数 / `closure_truncated` 比例 | 计数器 | 「编辑稀疏」是个假设，让它可被数据推翻 |
| 栅栏 race-lost 频率 | 计数器 | 若高到影响吞吐，说明报告写与 claim 争用超预期 |
| `context_resolve_failures{variant}` | 按 `ResolveError` 变体分桶、进入 health 快照与周期 tracing 的计数器 | claim 前 race-lost 不落持久诊断；定位失败不能只剩一条 `warn!` |
| `context_resolve_failures{variant="malformed_stored_report"}` | 上述计数器的点名桶；任一增量立即告警 | 撕裂写 / schema 回滚会触发不可逆 material 判决，必须能立即证伪 |
| `claim_fence_race_lost` | claim 栅栏失败计数器，进入同一 health 快照与周期 tracing | 衡量报告写与 claim 的争用及存储错误导致的 fail-closed |
| 每 wave 的 spec 声明速率 | 计数器 | §8 未结存量上限的证伪装置 |

---

## 附录 D：不变量与切片验收清单

**这张清单是压缩的下界。** 少一行就是少一件事。

### D.1 事件与顺序不变量

| # | 不变量 |
|---|---|
| 1 | `CardUpdated` → `WaveReportEdited` → `PlanUpdated`，同一事务、同一 wave scope |
| 2 | `PlanUpdated{key}` **严格早于**该 key 的第一个 `TaskDispatched` |
| 3 | 每个 `TaskDispatched` **同批**必有一条 `TaskContextFrozen`（同 wave、同 `idempotency_key`）|
| 3a | **空冻结集行可区分**：`'[]'` ⇒ 不判 material；`NULL` ⇒ fail-closed 判 material。判据是「冻结集为空」**不是** `origin='legacy'` |
| 3b | **task 块自身在冻结集内**且带**显式 root 标记**；改 in-flight 的 `goal` ⇒ 必有 `TaskContextAdvanced`（**限定：纳入字段集内的编辑**）|
| 3c | **栅栏关闭解析→claim 的 TOCTOU**：注入报告写（**同 wave 与跨 wave 各一**）⇒ claim 必须 race-lost、行仍 `pending`、无 `TaskContextFrozen`、无 `task_ref_index` 行、**无任何 worker / gate 首次启动** |
| 3d | **根块哈希只在纳入集敏感**：`goal`/`kind`/`gate` ⇒ 判；`priority`/`declared_by`/`spawn` ⇒ 不判；`released_by_user: false→true` ⇒ 不判。**3b′-ii 撤回规则**另覆盖：`ready: true→false` ⇒ 判（两种策略）；`released_by_user: true→false` ⇒ **仅 `declare-and-wait` 下判**；后二者在 3b′-i 本片不成立 |
| 3e | **claim 前定位失败一律不产生持久状态**：经生产 claim 路径注入根块删除 / 墓碑 / 同 key 多活 / 根块 refs 语法不合法 / 引用目标块被删 / 深度 ≥ 2 引用被删六类成因 ⇒ 无 `TaskContextAdvanced`、无 `TaskContextFrozen`、`context_stale_at_ms` 仍为 `NULL`；前四类另断言 pending 行已被投影删除，后两类另断言行仍 pending 且下一次编辑后可正常 claim。禁止 SQL 预置失败状态 |
| 3f | **不队头阻塞**：预算 = 1 且一条 ready pending 行 claim 不成功时，同 wave 另一条 ready 任务在同一次调度 pass 内正常派发；capacity 只按成功 claim 消耗 |
| 4 | 改动落在冻结集内 ⇒ **在此后第一次完成的 sweep 结束时**必有 `TaskContextAdvanced`，**且属于 task 所在的 wave**。E2E **显式跑一次 sweep 再断言**，不等事件到达 |
| 4d | **瞬时失败不产生持久判决**：不写 `context_stale_at_ms`、仍 in-flight、`context_verify_failures` +1；连续 3 轮才升级；中间任一轮成功即清零 |
| 5 | **判决落库后禁止起活**：`context_stale_at_ms` 非空 ⇒ 该任务上任何 operation 的 `prepare_tx` 拒绝；**从不打断已启动的** |
| 5b | **boot 顺序**：上下文 sweep 排在 operation 恢复**之前**（源码序断言）|
| 6 | 墓碑不进 `known_task_keys` / `gate_rule_violations` / `declaration_graph`，但**进 `dup_keys`**；墓碑 + 同 key 重声明 ⇒ 产出墓碑否决诊断而非 `duplicate key` |
| 7b | 仍被声明的**在飞行**永远算进 `occupied`（§4.2 规则 3）|
| 8 | §3.7 八条规则 × **四条写路径**（`Replace` / `WriteMarkdown` / `UpsertBlock` / `DeleteBlock`）的**全部否定测试**；其中「人删一个 `declared_by:"spec"` 的 task 块 ⇒ 200 且产出规范形态的墓碑」必须专列 |
| 8(f) | **第二条写路径的守卫**：非 `ActorId::User` 的 `PATCH` 写 `automation_policy` / `spec_task_ceiling` ⇒ **403，且两列不变、不发任何事件**（§6.6 的 user-only 闸 —— 「两个强制点，没有第三条」的另一个强制点，规则有验收也必须有）|
| 9 | **`key` 复活端到端**：删任务 → 墓碑 → 删墓碑 → 重提落新行（只对从未派发过的行成立，§4.2 规则 2）|
| 11 | **rebuild ≡ 增量差分**（§10.1），含三列与 stale 终态，并产生**同一组** `kernel_events`（各恰好一次）|
| 12 | **终结 / 不存在的 task 不得拥有 `task_ref_index` 行**（含「一轮 sweep 之后」）|
| 13 | spec 编辑被引用块必被检出 / wave 删除 / cove 删除三条触发路径 |
| 13a | 上述三条各要**事件正常投递**与**事件被丢弃**两个变体，且**必须给出同一结论**（详见 D.4 的 13）|

### D.2 各切片的验收清单

**切片 3b′**（见 §10.3，此处补齐四条无落点的）：

- **㉒ 元测试**：纳入集 ∪ 排除集 == `TASK_FIELDS`，且**引用同一个常量而非复制品**
  （`TASK_FIELDS` 必须先提升为模块级 `pub`，供跨 crate 的元测试引用）；
- **㉓ 回归**：`closure_truncated = true` 的任务，**事件路径与 sweep 两条路给出同一结论**；
- **㉕ 回归**：boot sweep 失败后，**一次周期 tick 成功即开门并当轮完成
  `resume_dispatched`**（注意：boot 路径上的补跑必然 no-op，不得把那一格写成断言，
  但**必须断言这一格**）。
- **㉖ SQL / migration 回归**：`task_claim_pending_tx` 的 SQL 不含
  `context_stale_at_ms` 谓词；从 `0068` fixture 状态运行真实 `0069_`，断言只清 pending、
  不产事件，且真实 migrator 再跑一次零变更。该清理防御的状态在已发布版本中不可达。

**切片 3c**：三类文案（本 wave 处于 `declare-and-wait` / context-stale (a) / context-stale (b)）+
其余每一类诊断的「人话 + 下一步动作」+ `Diagnostic` 带 `related_block_ids` +
**`released_by_user` 放行开关可用**（`declare-and-wait` 的唯一 UI 出口）。

**切片 4**：裁决工具 fail-closed（缺席 / 畸形 / 超时 ⇒ `material`）；
冻结点推进后同一处变更不再反复触发；判 `material` **不推进**冻结点。

**切片 5**：fork 的引用重写**硬测试**（内部引用指向新 wave id、
**外部引用逐字节未变** —— 错了是静默的）；fork 强制 `ready:false` +
`declared_by:"spec"`；fork 自跑 `validate_payload` 与 guard；
**规则 1 豁免点的枚举测试**（本片全仓恰好一处：`Fork`；切片 7 物化工具落地后
再扩成 `{Fork, Materialize}` 两处）；
`calm.plan.upsert` 隐藏 shim 返回迁移指引且零写入。

**切片 6**：树预算与深度上限的拒绝路径；**`spawn` 移入哈希纳入集的前置重裁**。

**切片 7**：物化工具对每行写 `ready: true`（**不是 `false`** ——
写 `false` 会当场删掉活的 pending 行）。

### D.3 每一片的自洽性自查（重切的验收标准）

| 片 | 合入后系统处于什么状态 | 有没有半截机制 |
|---|---|---|
| 1 | 词汇与校验就位，**零行为变化** | 无（`project_task_declarations` 零生产调用者是**刻意**的）|
| 2 | 归因与写口收口就位，**仍不投影** | 无 |
| 3a | 冻结/检测/判决的**载体与强制点**就位，调度决策逐位不变 | **有意的半截**：输入端未接线，由 3b′ 补 —— 这一点必须在 3a 的 PR 描述里如实写明 |
| 3b | **声明开始有后果**，存量上限与策略列到位 | **有**：§5 对 `origin='block'` 行恒空 ⇒ 3b′ 是部署硬门 |
| 3b′ | 冻结通电，§5 第一次被真实流量执行 | 无 |
| 3c | 人能看见状态与诊断、能放行 | 无 |
| 4 | 误报下降，冻结点可推进 | 无 |
| 5 | 模板可 fork，agent 只剩一个写口 | 无（新 legacy 行从此停增）|
| 6 | 子 wave 可用且被树预算约束 | 无 |
| 7 | 存量可见，cove 可聚合 | 无 |

### D.4 补充不变量（B13 恢复项）

| # | 不变量 |
|---|---|
| 4b | **sweep 是 fail-closed 的**，三条必测构造：(i) 丢事件（绕总线直接改 DB）(ii) 崩溃重启 (iii) 冻结集缺失（`NULL`）。前置断言：**判定必须持久，重启后 `context_stale_at_ms` 仍非空** |
| 4c | **once-per-condition**：已判 material 且冻结点未推进的任务，其后任意多轮 sweep **不再产生新 `TaskContextAdvanced`，也不再送第 2 级裁决**。**成本论证**：电平触发在一个按构造持续存在的条件上 ⇒ 每轮重发一条**不可裁剪**的事件并**重复调用 LLM**。（material 侧靠 `context_stale_at_ms` 不重发，immaterial 侧靠冻结点推进不重发，**两者成对**）|
| 5 的三构造 | (a) worker 未开始 → 重启 → 不得 spawn；(b) **gate 未开始**（`gate_attempt = 0` + material）→ 重启 → **不得有任何 gate shell 命令被执行**；(c) **已开始的不受影响**（验证没有过度收紧）|
| 5b seam | boot 顺序**两半缺一不可**：(a) 源码序断言 + (b) **seam 测试**（真实跑一次 boot，断言上下文 sweep 的副作用先于 operation 恢复可见）|
| 6b | **两个清除动作互相独立**（B2 后不再有派生耦合）：删墓碑 ⇒ 该 `key` 可重新声明；PATCH `automation_policy='auto-declare'` ⇒ 恢复自动化**且墓碑保留**。（换 key 不再被机制挡 —— 那是 §12.1 风险 15 记录的**已知缺口**，不是本条要验收的不变量）|
| 7 | **稳态并发上界**：任一时刻该 wave 的 in-flight 数 ≤ per-wave `task_budget`。**不得写成「由 dispatcher 全局信号量封顶」** —— `DEFAULT_PERMITS = 8` 是 **global concurrent-spawn cap，不是生命周期持有量**，那条论断已被明确驳回。另两半：**一次报告写产生的 `TaskDispatched` 恒为 0**（投影只落 `pending` 行，派发是 scheduler 的独立动作）；树内 `declared_by='spec'` 的非终结行 ≤ `tree_task_budget`（切片 6）|
| 10 | **`refs[]` 的 cove 边界**：越界引用 ⇒ 该块不可调度 + 诊断，且**该引用不得出现在任何 `TaskContextFrozen.refs` 里** |
| 11 生成器 | rebuild ≡ 增量的属性测试，其生成器**必须能生成「制造诊断的编辑」**：重复 key / 环 / 跨 cove / 撤回放行位 |
| 13 | 删除必须能被检出：(a) wave 删除 (b) cove 删除 —— **各要「事件正常投递」与「事件被丢弃」两个变体，且必须给出同一结论**；(c) `EditAuthor::Spec` 编辑被引用块必须被第 1 级检出 |

### D.5 §10.1 核心断言的五条形式化

对任意 wave，rebuild 与增量必须在以下五点上一致：

1. **删除阶段**只删同时满足三条的行：`origin='block'` ∧ 当前文档不再声明 ∧
   `status='pending'`。**「可调度」谓词的完整形式**：
   非墓碑 ∧ `ready` ∧ 诊断集合为空 ∧（`declare-and-wait` 时 `released_by_user`
   已置）∧（`declared_by='spec'` 时通过 ceiling 准入）。
2. **非 `pending` 行的声明内容一字不动**；收养与 `0070_` backfill 对 `decl_*`
   影子位的初始化不受此限，因为它不改变 worker 可见规格。
3. **所有存活行的状态列逐字节不变**。
4. **`origin='legacy'` 行逐字节不变**。
5. **`declared_by` 从块 payload 重建**，不依赖行内残留值。

### D.6 其余待补录的残余风险（按需取舍）

13.1（in-flight 无法撤回，**应单开 issue**）、13.3（`guard_non_prose_stomp` 的两处
连带）、13.5（「编辑稀疏」是未验证假设）、13.10（`declared_by` 的语义边界）、
13.11（`tasks` 积累执行史行、**刻意无清理**、读端必须能区分）、13.13、13.16、
13.19（三个权属位只能靠 §3.7 收口守住）、13.22 残余两条（**不阻止 spec 反复提议**；
**「人一直不放行」与「人没看见」在机制上不可区分**）、
**13.22b（升级日 legacy 在飞行不计入 `occupied`** ⇒ 人的「本 wave 未结任务」口径
会暂时**大于 ceiling**；且 `unknown_deps` 按规则 3‴ **必须把 legacy 在飞 key 视为
已知**，**不得在 `occupied` 查询里无条件混入 legacy 行**）、
13.2（block id 稳定性 ⇒ **误中止潮**，缓解是「引用已失效，请重新链接」诊断 +
marker 通道，**未量化**）、13.14（闭包开销：一次触发的重解析上界 =
冻结了该 wave 的 in-flight 任务数 × `MAX_REF_NODES` ≤ (Σ per-wave `task_budget`)
× 64 —— **由 `MAX_RERESOLVE_FANOUT` 封顶**）。
