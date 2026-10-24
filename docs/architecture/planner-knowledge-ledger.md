# Planner 知识账本短设计

状态：设计提案，只交付本文；由 Planner 安排两路独立 Codex 设计评审，收敛后再分片实现、评审及合并。风险层级 L2：涉及会话授权与持久化边界。基线 `ea4fbde0f`。遵守根目录 AGENTS.md、CONTRIBUTING.md；docs/ 与 docs/architecture/ 没有更具体的 AGENTS.md。

## 目标、已定取舍与证据

让一个会话“实际看到的 Report 版本和块”有一个持久、对模型可见的权威。D2：键为 `(worker_session_id, report_card_id)` 的服务端账本，复用 `LastRead {doc_rev, whole, blocks, sections}`；D3：小变更给块 diff，大变更只给块引用；D4：一次切换，无内存/旧作者归因兼容回退。模型不持有 token，不传 revision（#1883）；session id 只来自认证身份。D10 的 track status 增量化另片处理。

已核对 track Planner payload 中完整用户 context；其中使用次数和 40% 比例属于用户提供的运行统计，本次没有日志数据可独立验证，不能作为已复现实验。源码事实如下（行号以基线为准）：

| 事实 | 源码证据及结论 |
| --- | --- |
| 账本在内存，重启遗忘，2h TTL，首次读惰性驱逐同 caller card 的旧会话 | [report_read_ledger.rs](../../crates/calm-server/src/report_read_ledger.rs)，`LastRead` / `ReadLedger` / `record` / `capture` / `publish`；context 的第 5–8、22、47–51 行判断成立。 |
| 局部读也更新 doc_rev；whole 和 sections 是独立证据 | 同文件 `record`；[track_report.rs](../../crates/calm-server/src/mcp_server/tools/track_report.rs) 的 `report_read`（125–210）。`detail:index` 不记账，全文不包含 task，task 必须按 id 读。 |
| 写入锚点与投影现在分两次从内存取值，事务后发布 | [track_report_blocks.rs](../../crates/calm-server/src/mcp_server/tools/track_report_blocks.rs) 的 `commit` / `write_markdown` → [decision_sink.rs](../../crates/calm-server/src/decision_sink.rs) 的 `commit_report_op`（311–346）→ [write.rs](../../crates/calm-server/src/track_report/write.rs) 的 `persist`（287–427）。投影在事务内算，但账本发布不在事务内。 |
| wake 作者推断最多回溯 50 commits | [track_vcs/read.rs](../../crates/calm-truth/src/track_vcs/read.rs) 的 `ATTRIBUTION_COMMIT_BOUND`、`since_last_turn_block`、`path_authors_since`。同时读 `report_own/report_other` 和 mixed/unknown 分支，context 源码事实成立，行号已有漂移。 |
| “doc_rev ≥ 就读过该变更”不成立 | [guides/report.md](../../crates/calm-server/prompts/guides/report.md) 第 7 行、[observation.rs](../../crates/calm-types/src/observation.rs) 的 `ReportEdited::to_turn_text`（335–340）。局部读反例成立。 |
| 冲突当前只从文本抽数字，不能提供最小范围 | [track_report_blocks.rs](../../crates/calm-server/src/mcp_server/tools/track_report_blocks.rs) 的 `rev_conflict_error`；[track_report.rs](../../crates/calm-server/src/track_report.rs) 的 `check_doc_anchor`；[sections.rs](../../crates/calm-server/src/track_report/sections.rs) 的 `locate_read` / `check_section_read`。 |

## S1：持久模型、归属与事务

新增 migration `0194_planner_knowledge_ledger.sql`（当前最新为 0193；若合并前编号被占，使用当时下一个编号），不改任何已发布 migration。建议表如下；本文不是可执行 migration：

| 表 | 必填列、键与约束 |
| --- | --- |
| `report_read_ledger` | `worker_session_id TEXT` FK → `worker_sessions(id) ON DELETE CASCADE`；`report_card_id TEXT` FK → `cards(id) ON DELETE CASCADE`；复合 PK；`doc_rev INTEGER CHECK >= 0`；`whole INTEGER CHECK IN (0,1)`；`sections_json TEXT CHECK json_valid`，严格解码为 title → 有序 `(id, rev)` 列表。无 TTL、过期时间或作者列。 |
| `report_read_ledger_blocks` | 同一复合父键并 `ON DELETE CASCADE`；`block_id TEXT`；`rev INTEGER CHECK BETWEEN 0 AND 4294967295`；`kind TEXT`；`content TEXT`；PK 为父键加 block_id。`rev` 投影为 LastRead.blocks；kind/content 是该 revision 实际见过的原始块快照，用于 diff，不独立授予权限。 |

为父表 `report_card_id` 建索引服务 Report 删除；子表 PK 服务按会话/Report 加载与级联。sections JSON 保留顺序，不能排序或去重掩盖损坏；整数转换溢出、JSON/块快照不一致均报存储错误，不默认为空、不静默重建权限。无需 caller_card_id 冗余列：session 的归属由 worker_sessions 决定。不存在的父行表示未读；读到空 Report 也创建 whole=true 的父行。

`LastRead` 类型移到 calm-types 的 Report 领域模块，供 truth 存储与 server 规则共用；calm-truth 提供事务内 load/save/delete API，不依赖 calm-server。渲染、section 认证和 `project_commit` 继续由 server Report 领域拥有；MCP、REST、harness 不各造一套规则。浏览器的 [read_receipts](../../crates/calm-truth/migrations/0161_read_receipts.sql) 是人类时间水位，不可复用为模型块证据。

仅保存 revision 不能生成“已见版本 → 当前版本”的 diff，故需要子表 content；不能拿上一次 wake HEAD、相邻事件 before 或作者相同代替已见版本。内容随父行生命周期清理，无独立永久历史。空间成本是每个仍保留会话、每个读过 Report 的最新已见块快照；不复制全量提交链。

**读取事务**：`neige_report_read` 经身份/角色/Report 解析后，用现有 repo 事务入口取得写事务；在事务内再次确认 session 仍有权访问此 Report，取 card+CRDT 单快照，复用 `report_doc_snapshot` 和选择/渲染函数，生成实际 text 与 rendered ids，merge LastRead 并保存相应块快照，冻结返回的 anchor 状态后提交。投影或选择失败、未知块、记账失败均回滚且不返回成功 text。index-only 不改变账本。复用 [track_report_read.rs](../../crates/calm-server/src/track_report_read.rs) 的 CRDT 优先、NULL/旧布局投影规则，不能另解 payload 推测块 id。

SQL 不能与网络送达原子提交：这里“渲染同事务”指响应主体已成功生成且账本与该快照一起提交，不声称模型已经接收字节。提交后连接断开沿用当前“成功生成的响应算读”语义，重试幂等 merge；不能在响应发送后异步记账。task diagnostics、hydration/来源警告属于衍生回执，不能改变账本；主 text 及其存储验证要在提交前完成，衍生查询失败可能造成已提交但回执失败，须延续现有 warning-failure 测试。

**提交事务**：MCP 仅在外层解析无 revision 的用户意图；将认证 session 和原始意图带入 `commit_report_op` 的单一 Report 写入口。在 `persist` 的同一个事务内完成 recorder admission、读取同一账本、绑定 block/section/doc anchors、加载 CRDT、全部操作校验、写 Report/task 投影/事件、`project_commit` 和账本保存。不能沿用“handler 先取 anchor、sink 又 capture 一次”的分裂快照。返回同事务冻结的 anchor；事务后只允许衍生 enrichment，不再 publish。

使用 [db/mod.rs](../../crates/calm-truth/src/db/mod.rs) 的 `write_with_actor_events_typed` / `write_in_tx_typed` 契约，以及 [infra.rs](../../crates/calm-truth/src/db/sqlite/infra.rs) 的 `BEGIN IMMEDIATE` 写者串行化；读记账也走 writer 连接，不能升级一个 read-pool 快照。读取与提交不会发生“晚 publish 覆盖新读”，无需内存 generation。失败事务对 Report、tasks、events 和账本都零改动；S3 冲突诊断在回滚前从同一初始快照构造，经 typed error 带出。

保留全部安全规则：whole 只由实际全文读或保持有效的自身提交延续；局部读跨 doc_rev 清 whole；summary/创建/移动/创建 section 仍要求当前 doc anchor；已有块只比较该块；section 比较有序成员，不能由零散块知识推导 placement；task 按 id 读，Assistant 任务权限、objective/compact 等现有 guard 均不变。own move 不等于见过内容；自身提交只授予确实写出的内容及有来源证据的结构。

**保留旧块证据的明确调整**：当前 `project_commit` 会丢掉并未被本次写入、已被外部改动/删除的旧 block rev。仅把它原样落库，会使后续 wake 丢失 known_rev 和外部删除。提议只对 `blocks` 保留这些“确实见过、但可能已过时”的旧 `(id, rev, content)`，本次自身写入覆盖它们、自身删除移除它们；全文重读按当前 record 规则替换集合。partial read 仅更新实际 rendered 项。`sections` 的保守重建和 whole/doc_rev 投影仍复用原算法，绝不由 stale 块认证新 section。旧 rev 只会在真实 comparator 相等时通过，不会授予当前内容；ABA 例外另立 issue。此调整必须作为 S1 的行为变化单独评审并补测试，不能以“纯搬库”略过。若评审否决，必须重定设计，不能偷偷退回遗漏删除的增量算法。

**生命周期**：新增库为空，不从历史、作者或 provider thread 重建；升级提示现存会话首次写需 `neige_report_read`，之后重启直接复用。迁移不能扫描一遍当前 Report 当作已读。没有 TTL，idle、kernel restart、同一 session resume 不清理。provider thread id 不等于 worker_session_id。

supersede 时在通用 session 状态转换所在事务删除该 session 全部账本，不等新会话首次读。覆盖 [session_mirror.rs](../../crates/calm-truth/src/db/sqlite/session_mirror.rs) 的 `session_supersede_active_tx` 和 `session_mark_superseded_tx` 及其 projection wrapper；扫所有写 state=superseded 的路径，不能只挂 Planner UI。失败启动会经 [compensation.rs](../../crates/calm-server/src/mcp_server/wiring/compensation.rs) / `session_restore_from_superseded_tx` 恢复旧 session：已删除知识不恢复，旧会话须重读（安全且有意失去便利）；事务整体回滚则账本也回滚。该取舍须在提示和测试明确。

session/card/track/area 删除的直接 SQL 均由 FK cascade 清理，包括 [session_mirror.rs](../../crates/calm-truth/src/db/sqlite/session_mirror.rs) 的 `session_delete_tx`、[card.rs](../../crates/calm-truth/src/db/sqlite/card.rs)、[track.rs](../../crates/calm-truth/src/db/sqlite/track.rs)、[area.rs](../../crates/calm-truth/src/db/sqlite/area.rs)。Report reset 是内容编辑，不是删除会话账本，必须留下 stale 证据并冲突。不同 session、不同 Report 不共享权限；fork/新 Report 不复制源账本。无后台 TTL 扫描器、恢复旧内存或宽限期。

## S2：wake 只比较当前与已见 revision

在 server Report 领域提供一个只读 `knowledge_delta(LastRead, current_snapshot)`，S3 复用其事实比较。wake 从认证 harness worker_session_id 获取账本和当前 CRDT 的一致读快照；不依赖 last_seen_head、作者、排队事件的 before 或提交数，也不能被 `previous == current` / 没有 previous HEAD 提前跳过。wake、diff、track cat/index、hydration、冲突诊断都不更新账本；只有上述 text read 和自身提交更新。

对 current ids ∪ ledger.blocks ids 比较：`known_rev != current_rev` 就生成 `{id, known_rev, current_rev}`，缺失一侧用 JSON null（新增/未读为 known=null，删除为 current=null）。按当前文档顺序列现存块、删除项按 id 排序，不能截掉尾部丢失 ids。不按作者过滤：他人改后自己又改成最终版本且已见，不再提示；自己已成功写入的 revision 从不作为变化回送。未知块不是“他人写的”事实，措辞用“未见/已变”。task 当前 rev 可以作为引用列出，但 diff 不偷偷全文渲染未显式读过的 task，也不赋权。

小变更复用并扩展 [report_edit_diff.rs](../../crates/calm-types/src/report_edit_diff.rs) 的纯展示层、data fence 与转义约束：按稳定 id 配对账本 content 和当前 content，不再用 heading/位置猜配对；新增以空 before、删除以空 after。明确阈值提案：每块完整 diff ≤24 行且 ≤2048 bytes、所有完整 diff 总和 ≤8192 bytes 时显示 diff；某块超界则该块只给 id/old→new rev/读取引用，总预算不足则剩余块全部只给引用。计入 UTF-8 与围栏开销，不截出半个 diff；引用列表不受 diff 正文预算截断。这复用现有预算常量而把当前 excerpt 截断语义改成 D3。未知 task 只引用，其他未知新增可显示有界 additions。

块 revision 不涵盖顺序和 summary，不能假装没有变化就安全覆盖：另显示 `known_doc_rev/current_doc_rev` 以及 section 有序成员是否不匹配；doc_rev 不同但 changed=[] 时写明“文档/结构锚点已变化，未发现块 revision 差异”，不冒称具体移动或 summary 内容已知。该状态不把所有块列成 changed。涉及全局操作仍需全文读；已有 section 只需重读该 section。`whole=yes` 表示在记录的 doc_rev 完整见过，不能省略该 doc_rev 让模型误以为当前可 whole write。

完整 wake 路径为 [dispatcher/mod.rs](../../crates/calm-server/src/dispatcher/mod.rs) 的 ReportEdited 事件 → [queue.rs](../../crates/calm-server/src/harness/queue.rs) 折叠/持久队列 → [issuance.rs](../../crates/calm-server/src/harness/run_loop/issuance.rs) → 新 Report delta 呈现。已有事件 before→after diff 即使折叠也不是该 session 知识，不能与新 delta 并排继续回送。事件继续作为调度事实/人类历史，issuance 对新发出的 ReportEdited 段只呈现一次新 delta；旧持久队列也按新规则呈现，不使用缺字段旧句兜底。持久历史中已经发出的文本不重写。

[run_loop.rs](../../crates/calm-server/src/harness/run_loop.rs) 的 `since_last_turn_diff_block` 继续给非 Report 的 track-vcs 差异，`last_seen_head` 只服务这些文件。删 report.md 分支、ReportPatch Include/Omit、report_own/report_other/mixed/unknown 和基于 50 commits 的 Report 归因。若非 Report 仍需 latest-author 标签，应从展示要求判断是否保留其独立逻辑，绝不可让它决定 Report delta 完整性；本设计建议去掉 wake 的作者标签与整段 `path_authors_since`，保留显式 track history/log 的作者数据。调度层“自己写入不唤醒自己”的现有策略不是知识判断，可保留；每次实际 wake 都重新按账本算。

现有 track-vcs timeout 降级不可吞掉 Report 事实并标成已知。Report delta 失败时给明确 `knowledge_unavailable` 提示及读取入口，既不返回空 changed 假称无变化，也不更新账本；无需回退作者或全篇旧 patch。重试无需从旧 HEAD 回放。差异是辅助信息，不放宽任何写锚点。

## S3：可枚举 anchor 与最小重读

读/写成功回执增加结构化 `anchor: {doc_rev, whole, known_blocks}` 及一行 `anchor: doc_rev N · whole=yes|no · K blocks known`；未读用 `anchor: null` 和 `reason: never_read`，不假造 rev 0。index-only 说明未增加知识。whole 的有效性必须结合当前 doc_rev；回执数据来自同一事务结果，不在回执阶段重新查询一个新版本。读取全文/局部、提交、whole write 的 summary line 都带 anchor。外部编辑后账本不会自动变新。

拒绝原因使用 typed enum（snake_case）而非分析英文文本：`never_read`、`unread_block`、`unread_section`、`whole_required`、`changed_since`、`document_changed`、`section_membership_changed`、`superseded_session`。前三类及 whole_required 保持现有 -32602；真实 revision/结构冲突保持 -32409；superseded_session 仍用身份 admission 的拒绝码，不能为知识诊断绕过认证。已删 session/token 的失败保持现有认证拒绝，不泄露 Report；不把存储故障误报 never_read。`knowledge_unavailable` 是展示失败状态，不是锚点失效原因。

Report -32409 的 typed data 示例：

```json
{
  "reason": "changed_since",
  "doc_rev": 18,
  "anchor": {"doc_rev": 16, "whole": false, "known_blocks": 7},
  "changed": [{"id": "b_a", "known_rev": 2, "current_rev": 3}],
  "reread": {"scope": "blocks", "blocks": ["b_a"]}
}
```

`changed[]` 是导致本次操作拒绝的最小相关差异（在事务开始的 D0 快照比较，不能混入本次已模拟的前几项操作）；doc 级操作才取整个 Report delta。所有冲突带数组，纯顺序/summary/doc 变化可以为 []，不伪造块 rev。section 顺序冲突另带 `sections: [{title, known_members, current_members}]`（成员都是有序 id/rev 对），不能把 rev 相等的块塞入 changed 冒称内容变化。`rev` 旧单值字段和 message 数字解析一次删除；`doc_rev` 保留但不是重试 token；输入仍不接受 revisions。汇总诊断复用 Report 领域的锚点比较器，以 D0 做失败前提收集，随后仍由既有有序操作执行器处理内容/结构依赖；不能在 MCP 再造一份批操作模拟器。

| 拒绝的操作 | `reread` 最小范围与边界 |
| --- | --- |
| 已有 id 的 upsert/delete | `blocks` 为受影响仍存在的 ids；batch 汇总相关 ids，不因第一个冲突遗漏后面的同类冲突。当前已删除 id 在 changed 中 current=null；用 `scope:index` 发现替代 id，不能推荐读取不存在的 id。 |
| 已有 section replace/delete | `sections` 为受影响、仍唯一存在的标题，包含原有成员被删除/新增/换序的差异。标题消失或歧义时 `scope:index` 先发现目标；歧义仍按现有无效输入规则拒绝。只读块不能修复 section membership 证据。 |
| whole write、summary、创建、move、创建缺失的声明 section | `scope:full`（保持当前文档锚点规则）；full read 不包含 task，若同批还触及 task，额外 `blocks` 列 task ids，必须显式读取。只读 changed blocks 不保证升级 whole/doc anchor。 |

同批混合 scope 取能满足所有失败前提的最小并集（full 覆盖非 task sections/blocks；task 显式保留）；index 是发现步骤，不声称执行后必然可写。未知/未读的 -32602 也提供相同格式的最小 read 建议，但不假称版本冲突。不读取或赋权无关块，不替模型自动合并或自动重试写入。

## 调用方审计与删除清单

| 入口/调用方 | 实现切片要求 |
| --- | --- |
| MCP report_read：全文、markers、sections、blocks、index、resolve | 一个领域 read-and-record 入口；保留实际 rendered 集合、whole/section/task 规则。hydrated index 或 source 文档不是 Report 知识。 |
| MCP report_commit / report_write → sink → persist → ops/sections | 两个嘴共用事务内绑定、检查、保存；不在 anchors.rs 提前读库制造 TOCTOU。批操作失败零落盘，receipt enrichment 失败不撤销已落盘知识。 |
| REST Report 全文、block CRUD/move、reset；初始创建/fork、task/worker verdict | [write.rs](../../crates/calm-server/src/track_report/write.rs) 的目的专属入口和 structural_init_report_tx；只修改 Report，不推进任何模型账本。worker task report/verdict 不等于 Planner 已看见。 |
| track cat/部分 Report 路径、links、REST read、series、workspace_reports/history、track status | [track_file.rs](../../crates/calm-server/src/mcp_server/tools/track_file.rs)、[report_links.rs](../../crates/calm-server/src/mcp_server/tools/report_links.rs)、[workspace_reports.rs](../../crates/calm-server/src/workspace_reports.rs) 共用快照读取但全部不锚定；CLI/MCP 不新增隐式例外。 |
| dispatcher refs、ReportEdited、queue fold/replay、issuance、track-vcs wake | 改为实际发行时一次比较账本；旧事件字段可继续存储为审计事实，不能继续作为已知版本或展示 fallback。canonical 与历史队列发行路径都要覆盖。 |
| AppContext / sink / 所有 fixture 构造 | 删除 ReadLedger Arc/Mutex、Clock/TTL、generation、capture/publish、惰性 caller-card 清理。test 通过真实 DB/session 建立状态，不自动补齐 required fields。`rg read_ledger/LastRead` 的所有调用方都迁移，含 vocabulary、concurrent_sessions、sections 和 anchor_knowledge 测试。 |
| 指南、描述、示例/生成物 | 删除两处 doc_rev ≥ 已读说法；更新 read/commit/write prompt 和冲突契约。无兼容双输出。若实现改变 schema/OpenAPI，则跑真实生成器带齐产物；本文不生成。 |

## 生产入口验收与分片

S1 先交持久化、同事务 read/commit、清理和 anchor；S2 再接新 wake 并删旧机制；S3 最后换 typed conflict。每片 L2 两路独立 Codex 评审，明确检查抽象层、重复逻辑、硬编码、外部契约事实与永久维护成本。设计双评审的未决点是块旧证据保留、内容基线成本和补偿恢复须重读，本文均给出提案而非假称已获新授权。

实现时先在真实 MCP/REST/harness 入口加最小 red 复现，再修实现，不能在 fixture 手写一份 delta 算法。计划至少覆盖：

1. 磁盘 DB text read →销毁并重建 repo/AppContext（同一身份 session）→无重读直接 commit；超过 2h 也成功。index-only/从未读/新 session 拒绝。升级空账本须读，旧内存不回填。
2. partial read 推进 doc_rev 但不授 whole；跨版本局部合并、空 Report、按 id 读 task、sections 重名/不存在/来源分裂合并/移动、marker 保 id、own move 未读内容不授知识；[mcp_report_anchor_knowledge.rs](../../crates/calm-server/tests/cases/mcp_report_anchor_knowledge.rs) 全部断言保持。原“新建内存 ledger 为空”断言迁成“新 session 为空”，另测重启持久，不能保留与新契约相反的旧断言。
3. REST/user/Assistant 外部编辑后重启仍冲突；自身无关提交不吞掉外部改动或删除的 old rev，wake 能给 old→new/null；自身删除不再回送。summary-only/foreign move 的 changed=[] 仍提示 doc/section 状态且写 guard 拒绝。
4. 双读、读与 commit、同 session 两提交交错（显式 barrier）：只允许事务顺序的完整结果。batch 后项失败、task guard 拒绝、ledger 保存失败使 Report/task/event/ledger 全回滚；提交后 warning 查询故障仍可用已提交 anchor 继续写。
5. supersede 两条入口、失败新启动补偿、session 删除及 card/track/area 直接删除均无孤儿；事务回滚保留，恢复 superseded session 后明确需重读；其他 session/Report 不受影响，fork 不继承知识。
6. 60+ 次 Planner/Assistant/user/kernel 交错提交，真实 harness 发 wake，精确断言 ids/revs 与实际未见差异一致、自身已写 rev 零回送、没有 mixed/unknown/50 上限；无 HEAD/相同 HEAD、重启、旧队列 replay、折叠事件均使用同算法。两次 wake 不会推进知识；读完后下一次 wake 消失。小/大/混合 diff 阈值、UTF-8/围栏、未知 task 只引用、全部变更 ids 不漏列。
7. -32409 数据逐项断言 changed 的 known/current/null、doc_rev、reason、最小 reread；多 op 冲突按 D0 汇总，纯结构变化数组为空；只重读建议范围后重试成功，其余块仍未读；无 revision 输入通道。未认证/superseded 不泄露 changed。

实现 focused 命令使用现存注册目标：`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test mcp_integration_suite mcp_report_anchor_knowledge --test-threads 8`；vocabulary/concurrent_sessions/sections 同 target 按测试名过滤。harness/dispatcher 与 migration/cascade 分别选 calm-server lib 和 calm-truth 的实际新增测试名；注册后用列表确认非零，禁止虚构过滤器或空通过。关键安全断言按 AGENTS.md 单因素 mutation，预报全红集合、跑整个相关 filter、恢复并复绿；不在本设计任务中修改生产代码做 mutation。

每片 worker 只跑 focused 和格式修复；kernel gate 跑 quick Rust、文本、contract 及一次非空 focused replay；CI 跑 broad suites，不在共享主机跑真实 Codex E2E。不增加测试名永久白名单/额外 CI 扫描；普通新增用例只改所属测试和必要注册，每工作流沿用既有 gates。当前设计仅运行任务声明的 `scripts/local-ratchet-gates.sh` 文本 gate；未运行 Rust 或行为测试，不宣称 mcp_report_anchor_knowledge 已绿。

## 独立 ABA issue 建议

建议标题：`fix(report): prevent stale anchors from matching reused block identities`。证据是 [report_read_ledger.rs](../../crates/calm-server/src/report_read_ledger.rs) 第 9–10 行，以及 [ops.rs](../../crates/calm-server/src/track_report/ops.rs) 的 reset 说明：foreign delete 后重用同 id、同 rev，旧 `(id, rev)` 可误匹配；REST if_rev 同样存在。持久账本只是延长可遇到的窗口，不修复它。

独立 issue 验收：构造 read A →外部删 A（含 tombstone 删除/reset）→新内容复用 A.id/A.rev →旧 MCP anchor 和 REST if_rev 均拒绝；在 ReportDoc 身份生成/版本拥有层选择不可重用 incarnation 或单调版本，审计 fork/reset/marker/历史恢复，明确 migration 与 API 影响。不能在本片悄加 doc_rev 全局校验来“修复”而破坏局部并发写。本文不新开 issue、不宣称 ABA 已解决；Planner 在独立任务先查重再建 issue。
