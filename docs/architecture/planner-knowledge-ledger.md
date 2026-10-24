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

为父表 `report_card_id` 建索引服务 Report 删除；子表 PK 服务按会话/Report 加载与级联。sections JSON 保留顺序，不能排序或去重掩盖损坏；严格解码仅拒绝损坏：整数溢出、非法 JSON/类型、重复成员、缺失必填快照字段等，不默认为空、不静默重建权限。sections 与 blocks 是独立的历史证书，不做跨证书 revision 相等校验：section A 可合法保留 `[b@1,c@1]`，局部重读 b 后 blocks[b]=2；这应使 section 写冲突，而不是存储错误。外部删除后旧成员仍可留在 section 证书中，不能因当前文档缺块而拒绝加载。无需 caller_card_id 冗余列：session 的归属由 worker_sessions 决定。不存在的父行表示未读；读到空 Report 也创建 whole=true 的父行。

`LastRead` 类型移到 calm-types 的 Report 领域模块，供 truth 存储与 server 规则共用；calm-truth 提供事务内 load/save/delete API，不依赖 calm-server。渲染、section 认证和 `project_commit` 继续由 server Report 领域拥有；MCP、REST、harness 不各造一套规则。浏览器的 [read_receipts](../../crates/calm-truth/migrations/0161_read_receipts.sql) 是人类时间水位，不可复用为模型块证据。

仅保存 revision 不能生成“已见版本 → 当前版本”的 diff，故需要子表 content；不能拿上一次 wake HEAD、相邻事件 before 或作者相同代替已见版本。内容随父行生命周期清理，无独立永久历史。空间成本按所有保留会话/Report 的 ledger 子行计算：`Σ(UTF-8(content)+UTF-8(kind)+UTF-8(block_id)+行/索引开销)`，另加父行、sections_json 和冻结 plan 的呈现字节；不复制全量提交链。外部删除的旧块直到全文读或生命周期清理仍占空间，长寿会话持续局部读与外部增删会积累超过当前 Report 大小的历史 ids，不能用当前块数作上界。验收用磁盘 DB 的 churn 场景记录子行数、content 总字节、sections_json 字节、冻结 plan 字节及 SQLite page_count/page_size（区分逻辑存量与已分配文件空间），并对全文读/删除会话前后复测；不因此添加 TTL、静默逐出或容量白名单。

**读取事务**：`neige_report_read` 经身份/角色/Report 解析后，用现有 repo 事务入口取得写事务；在事务内再次确认 session 仍有权访问此 Report，取 card+CRDT 单快照，复用 `report_doc_snapshot` 和选择/渲染函数，生成实际 text 与 rendered ids，merge LastRead 并保存相应块快照，冻结返回的 anchor 状态后提交。投影或选择失败、未知块、记账失败均回滚且不返回成功 text。index-only 不改变账本。复用 [track_report_read.rs](../../crates/calm-server/src/track_report_read.rs) 的 CRDT 优先、NULL/旧布局投影规则，不能另解 payload 推测块 id。

SQL 不能与网络送达原子提交：这里“渲染同事务”指响应主体已成功生成且账本与该快照一起提交，不声称模型已经接收字节。提交后连接断开沿用当前“成功生成的响应算读”语义，重试幂等 merge；不能在响应发送后异步记账。task diagnostics、hydration/来源警告属于衍生回执，不能改变账本；主 text 及其存储验证要在提交前完成，衍生查询失败可能造成已提交但回执失败，须延续现有 warning-failure 测试。

**提交事务**：MCP 仅在外层解析无 revision 的用户意图；将认证 session 和原始意图带入 `commit_report_op` 的单一 Report 写入口。在 `persist` 的同一个事务内完成 recorder admission、读取同一账本、绑定 block/section/doc anchors、加载 CRDT、全部操作校验、写 Report/task 投影/事件、`project_commit` 和账本保存。不能沿用“handler 先取 anchor、sink 又 capture 一次”的分裂快照。返回同事务冻结的 anchor；事务后只允许衍生 enrichment，不再 publish。

使用 [db/mod.rs](../../crates/calm-truth/src/db/mod.rs) 的 `write_with_actor_events_typed` / `write_in_tx_typed` 契约，以及 [infra.rs](../../crates/calm-truth/src/db/sqlite/infra.rs) 的 `BEGIN IMMEDIATE` 写者串行化；读记账也走 writer 连接，不能升级一个 read-pool 快照。读取与提交不会发生“晚 publish 覆盖新读”，无需内存 generation。失败事务对 Report、tasks、events 和账本都零改动；S3 冲突诊断在回滚前从同一初始快照构造，经 typed error 带出。

保留全部安全规则：whole 只由实际覆盖全部非 task 块的 text read 或保持有效的自身提交延续；未覆盖全部的局部读跨 doc_rev 清 whole；summary/创建/移动/创建 section 仍要求当前 doc anchor；已有块只比较该块；section 比较有序成员，不能由零散块知识推导 placement；task 按 id 读，Assistant 任务权限、objective/compact 等现有 guard 均不变。own move 不等于见过内容；自身提交只授予确实写出的内容及有来源证据的结构。

**保留旧块证据的明确调整**：当前 `project_commit` 会丢掉并未被本次写入、已被外部改动/删除的旧 block rev。仅把它原样落库，会使后续 wake 丢失 known_rev 和外部删除。提议只对 `blocks` 保留这些“确实见过、但可能已过时”的旧 `(id, rev, content)`，本次自身写入覆盖它们、自身删除移除它们；全文重读按当前 record 规则替换集合。partial read 仅更新实际 rendered 项。`sections` 的保守重建和 whole/doc_rev 投影仍复用原算法，绝不由 stale 块认证新 section。旧 rev 只会在真实 comparator 相等时通过，不会授予当前内容；ABA 例外另立 issue。此调整必须作为 S1 的行为变化单独评审并补测试，不能以“纯搬库”略过。若评审否决，必须重定设计，不能偷偷退回遗漏删除的增量算法。

**生命周期**：新增库为空，不从历史、作者或 provider thread 重建；升级提示现存会话首次写需 `neige_report_read`，之后重启直接复用。迁移不能扫描一遍当前 Report 当作已读。没有 TTL，idle、kernel restart、同一 session resume 不清理。provider thread id 不等于 worker_session_id。

supersede 时在通用 session 状态转换所在事务删除该 session 全部账本，不等新会话首次读。覆盖 [session_mirror.rs](../../crates/calm-truth/src/db/sqlite/session_mirror.rs) 的 `session_supersede_active_tx` 和 `session_mark_superseded_tx` 及其 projection wrapper；扫所有写 state=superseded 的路径，不能只挂 Planner UI。失败启动会经 [compensation.rs](../../crates/calm-server/src/mcp_server/wiring/compensation.rs) / `session_restore_from_superseded_tx` 恢复旧 session：已删除知识不恢复，旧会话须重读（安全且有意失去便利）；事务整体回滚则账本也回滚。该取舍须在提示和测试明确。

session/card/track/area 删除的直接 SQL 均由 FK cascade 清理，包括 [session_mirror.rs](../../crates/calm-truth/src/db/sqlite/session_mirror.rs) 的 `session_delete_tx`、[card.rs](../../crates/calm-truth/src/db/sqlite/card.rs)、[track.rs](../../crates/calm-truth/src/db/sqlite/track.rs)、[area.rs](../../crates/calm-truth/src/db/sqlite/area.rs)。Report reset 是内容编辑，不是删除会话账本，必须留下 stale 证据并冲突。不同 session、不同 Report 不共享权限；fork/新 Report 不复制源账本。无后台 TTL 扫描器、恢复旧内存或宽限期。

## S2：wake 只比较当前与已见 revision

在 server Report 领域提供一个只读 `knowledge_delta(LastRead, current_snapshot)`，S3 复用其事实比较。wake 从认证 harness worker_session_id 获取账本和当前 CRDT 的一致读快照；不依赖 last_seen_head、作者、排队事件的 before 或提交数，也不能被 `previous == current` / 没有 previous HEAD 提前跳过。wake、diff、track cat/index、hydration、冲突诊断都不更新账本；只有上述 text read 和自身提交更新。

对 current ids ∪ ledger.blocks ids 比较：`known_rev != current_rev` 就生成 `{id, known_rev, current_rev}`，缺失一侧用 JSON null（新增/未读为 known=null，删除为 current=null）。按当前文档顺序列现存块、删除项按 id 排序，不能截掉尾部丢失 ids。不按作者过滤：他人改后自己又改成最终版本且已见，不再提示；自己已成功写入的 revision 从不作为变化回送。未知块不是“他人写的”事实，措辞用“未见/已变”。task 当前 rev 可以作为引用列出，但 diff 不偷偷全文渲染未显式读过的 task，也不赋权。

小变更复用并扩展 [report_edit_diff.rs](../../crates/calm-types/src/report_edit_diff.rs) 的纯展示层、data fence 与转义约束：按稳定 id 配对账本 content 和当前 content，不再用 heading/位置猜配对；新增以空 before、删除以空 after。明确阈值提案：每块完整 diff ≤24 行且 ≤2048 bytes、所有完整 diff 总和 ≤8192 bytes 时显示 diff；某块超界则该块只给 id/old→new rev/读取引用，总预算不足则剩余块全部只给引用。计入 UTF-8 与围栏开销，不截出半个 diff；引用列表不受 diff 正文预算截断。这复用现有预算常量而把当前 excerpt 截断语义改成 D3。未知 task 只引用，其他未知新增可显示有界 additions。

块 revision 不涵盖顺序和 summary，不能假装没有变化就安全覆盖：另显示 `known_doc_rev/current_doc_rev` 以及 section 有序成员是否不匹配；doc_rev 不同但 changed=[] 时写明“文档/结构锚点已变化，未发现块 revision 差异”，不冒称具体移动或 summary 内容已知。该状态不把所有块列成 changed。whole write 仍需 whole 证据；summary/创建/move/从未认证的缺失声明 section 只需当前 doc anchor，局部 text read 即可刷新；已有 section 需其独立证书，重读该 section。`whole=yes` 表示在记录的 doc_rev 完整见过，不能省略该 doc_rev 让模型误以为当前可 whole write。

### canonical 的 claim、呈现和 proof

真实生产入口有两支，不能只改 issuance：[run_loop.rs](../../crates/calm-server/src/harness/run_loop.rs) 对 canonical 调用 [canonical.rs](../../crates/calm-server/src/harness/run_loop/canonical.rs) 的 tick 后 continue；后者在 writer 事务内 `pending_plan_for_prefix_tx` → [conversation_inputs/source.rs](../../crates/calm-server/src/conversation_inputs/source.rs) 的 `claim_initial_tx` → truth claim。历史队列才经 [queue.rs](../../crates/calm-server/src/harness/queue.rs) → [issuance.rs](../../crates/calm-server/src/harness/run_loop/issuance.rs)。[dispatcher/canonical_inputs.rs](../../crates/calm-server/src/dispatcher/canonical_inputs.rs) 的 `authorizes_tx` 要求 System 输入仍等于真实 producer 绑定的原 observation；[claim_validation.rs](../../crates/calm-truth/src/db/sqlite/conversation/claim_validation.rs) 同样比较 pending 与 plan 的原 payload。新 delta 不能替换或伪造这个 observation，也不能改 observation_id、input refs、队列顺序或原事件来源授权。

选择新增内部 `SubmissionRenderPlan::V2`，明确区分**来源事实**和**冻结呈现**，不扩展事件成为知识权威。V2 保留原 `SubmissionRenderInput`（包括完整原 observation）；每个 Input 段另有必填的 typed rendering：原输入呈现，或仅对 System 输入引用本 plan 内一个被冻结 System context 段（Human 不允许替代呈现）。后者是通用的系统呈现关系，只能引用系统 context、不能改变 human 归属；位置、引用存在性与不可重复呈现由 plan validator 检查。Report 领域的受信 freeze 构造入口决定哪些 ReportEdited 输入使用该关系（调用方不得自行提交任意替代正文），并为一次 wake 生成一个 Report delta context；所有这些输入可引用同一个 context，引用本身不再输出文字，hydrate 在该 context 声明的位置只输出一次，不再输出它们原 observation 的 before→after 文本。其他 system 输入及 human 输入仍按原契约呈现；没有 ReportEdited 的实际 Planner wake 也附加一次 Report context。领域规则在 server Report 拥有层，通用 truth/plan validator 只检查 typed 结构和持久事实，不特判 Planner/provider/template 身份。

新 PendingPrefix claim 在现有 `conversation_inputs::claim_initial_tx` 的 server 组合层完成冻结：保留并检查原 source admission，在同一 writer 事务内从 carrier 的认证 worker_session_id 与所属 Report 解析身份，读取账本和 Report card+CRDT，调用纯 `knowledge_delta` 并渲染最终 System context，构造 V2，随后 truth 校验原 pending prefix、来源事实并写 submission。调用方准备的模型/预算可以仍在事务外，Report 呈现只能在最终 claim 事务产生；早期 selection/read admission 不得缓存它。该入口覆盖所有新 canonical claim 调用方，而非只在 tick 补一个特例。Report、账本与 claim 因同一事务得到一致顺序；失败不留半个 claim，也不推进账本。该事务内渲染增加的成本须在实现验收测量 writer 占用时间与冻结字节量（含大量删除积累），复用有界 diff 正文预算；不能移到 hydrate 换取更短锁，也不能截掉变更引用。

现存 [render.rs](../../crates/calm-types/src/conversation/render.rs) 只有 V1 的 Input/Context，不能仅用旁挂 context 解决，因为那仍会输出旧 observation 文本；因此需要上述显式替代呈现关系。[claim_storage.rs](../../crates/calm-truth/src/db/sqlite/conversation/claim_storage.rs) 将整个 V2（原来源、呈现关系、最终文字）序列化后一次计算并保存 digest；[proof.rs](../../crates/calm-types/src/conversation/proof.rs) 的 plan version/ref、校验和私有 journal 恢复必须识别 V2 并绑定同一 digest。journal proof 自身仍为现有 V2，独立于新增 plan V2；proof 仍无正文且不表示授权或送达。无需新 prompt 归档表、第二 outcome ledger 或自造 proof；扩展现有内部版本及其全部解码/擦除/恢复调用方。

[hydrate.rs](../../crates/calm-server/src/harness/input_dispatch/hydrate.rs) 只消费 plan 中冻结的 Report context 与呈现关系，**不查询当前 Report/知识账本、不重算 delta**。它仍可按既有契约读取 human revision、身份和擦除事实；“不动态读库”针对 Report 呈现，不取消原 admission/隐私校验。[input_dispatch/mod.rs](../../crates/calm-server/src/harness/input_dispatch/mod.rs) 在 crossing 事务和 await 后 revalidate 都会 hydrate；两次必须给相同未擦除文字。Report context 与原 observation 同属 private history：擦除必须覆盖新增 context，不得经原 observation 回退复活正文；擦除后拒绝 dispatch/retry，原 digest/proof 保持不变。

### 新 claim 与冻结 retry 的分界

[canonical/retry.rs](../../crates/calm-server/src/harness/run_loop/canonical/retry.rs) 的 ExplicitSuccessor 继承 [conversation/retry.rs](../../crates/calm-truth/src/db/sqlite/conversation/retry.rs) / `claim_storage::insert_retry_submission_tx` 的 root plan、原 digest 和 frozen invocation。V2 retry 不调用 Report freeze，即使 Report/账本/配置变化也只消费原冻结呈现；每次 crossing 仍重验原 source 与当前 carrier/resource 授权。claim 后未 crossing、确认未发送但仍引用原 submission 的重试也不得原地重算。只有已有 `NotDispatched` 协议证据允许 [release.rs](../../crates/calm-truth/src/db/sqlite/conversation/release.rs) 的 `release_submission_tx` 真正释放有效 claim。普通 initial 释放后恢复原 PendingInput，此后 PendingPrefix 是**新 claim**，允许用新一致快照冻结新文字及新 digest；但 ExplicitSuccessor 的 release 恢复的是同一个 retry intent，后续 claim 仍继承原 root plan/digest，不能借释放刷新冻结输入。二者按现有 release 的 retry 分支区分，不按“释放过”泛化。

`CrossingUnknown` 是调用前持久标记，`Unknown` 是不确定结果，都不是“未发送”；`Accepted` 才有正向接受证据。这些状态、keep/retry receipt、exact thread settlement 与原执行归属沿用 [checkpoint.rs](../../crates/calm-types/src/conversation/checkpoint.rs)，不能由知识账本推断。重启保留冻结 plan 和状态；不确定结果不得自动重发。显式 Human retry 仍须现有 retry availability、原来源与 exact thread settlement 通过，继承 root plan；新收到的独立 wake 才计算新 delta。历史队列对尚未发行的 Report 段也用同一个领域算法和一致快照，去掉旧事件 diff；已发行历史文字不重写。

### D4 一次切换与旧计划升级

V1 是可审计的历史格式，不是新发行的兼容回退。[storage_contract.rs](../../crates/calm-server/src/conversation_inputs/storage_contract.rs) 要求已有 canonical 工作保留原 carrier/workspace，[resolution.rs](../../crates/calm-truth/src/db/sqlite/conversation/resolution.rs) 的 keep 只针对有真实 attempt 的 held 输入；因此不能以换会话、重建 carrier 或伪造 Human keep 消除旧 claim。以下 fail-closed 排空/保留策略来自这些现存契约与 D4，不假设有一个尚不存在的“无损升级重发”入口。升级前暂停新 claim/dispatch，按存储中的实际 plan version、有效 claim、attempt delivery 和 retry relation 做只读预检，不能把“有 claim”归类为“已发送”。切换条件与处理如下；不改旧 plan、digest、proof 或 receipt，不从历史内容回填账本：

| 切换时状态 | 处理 |
| --- | --- |
| 旧 observation/历史队列仍未 claim，或已有确证 NotDispatched 且按原入口完成 release、无待执行旧 retry | 保留原 producer/source 绑定；切换后首次新 claim 生成 V2，旧事件 before 不作基线。 |
| V1 已 claim 但没有 crossing attempt，或仍有效的 NotDispatched claim | **不是已发送历史**。阻止切换激活；在旧版本以既有安全流程完成/确认释放并重新预检。不能直接把 claim 改回 pending、伪造一次 NotDispatched 或套用只接受 held attempt 的 Human keep。若现场不能安全排空，保持暂停并交 Planner/运维处理，不能放宽条件。 |
| V1 CrossingUnknown/Unknown | 原样保留 held 与原 proof，禁止自动重发；只允许原协议的精确回执/settlement 或已授权 Human keep。未解决时可保留为不可执行 held；新版本显式 retry 返回 typed `render_plan_upgrade_required`，提示先解决原执行，再提交新的独立输入。keep 不证明未发送，也不取消原执行；继续遵守 exact thread settlement。 |
| V1 Accepted、已结束/已 Human keep 的历史 | 保留历史与原证据，允许原执行结算；不补算或追加旧 turn 的 Report delta，不把 Accepted 当作完成。若另有待执行的 V1 ExplicitSuccessor，则仍阻止激活，须在旧版本按原协议解决后重新预检。 |
| V2 已 claim/显式 retry/重启恢复 | 按上一节冻结语义处理，包含无 attempt 与未知结果分支；Report 变化只影响下一次新 claim。 |

预检不是只靠人工约定：新版本所有 fresh crossing（含无 attempt 恢复）、retry intent 创建/消费入口必须拒绝 V1，并返回同一升级原因；预检与激活之间禁止并发旧 writer，避免刚通过又产生 V1。读取旧 proof 作关联核验/接受迟到回执不调用旧文本 hydrate，不属于旧 diff 发行回退。已存在的 resolution key 重放仍返回原 receipt，不伪造新动作；V1 retry 若在切换前已经形成但尚可执行，由预检拦住，不能改 root 到 V2。升级提示明确：“Report 知识账本从空开始，首次写前须 text read；旧冻结输入不支持新版重发，不能判断未知发送为未发送；请先处理原执行，再发新输入。”若现场要求保留 V1 可重发能力或自动迁移未知执行，就与 D4/冻结 proof 冲突，需 Planner 新决策，不能在实现中增兼容分支。

[run_loop.rs](../../crates/calm-server/src/harness/run_loop.rs) 的 `since_last_turn_diff_block` 继续给非 Report 的 track-vcs 差异，`last_seen_head` 只服务这些文件。删 report.md 分支、ReportPatch Include/Omit、report_own/report_other/mixed/unknown 和基于 50 commits 的 Report 归因。若非 Report 仍需 latest-author 标签，应从展示要求判断是否保留其独立逻辑，绝不可让它决定 Report delta 完整性；本设计建议去掉 wake 的作者标签与整段 `path_authors_since`，保留显式 track history/log 的作者数据。调度层“自己写入不唤醒自己”的现有策略不是知识判断，可保留；每次新 wake claim 都按账本算；已有冻结 submission 的 dispatch/retry 不重算。

现有 track-vcs timeout 降级不可吞掉 Report 事实并标成已知。已取得认证与一致快照、但 delta 展示失败时，冻结明确 `knowledge_unavailable` 提示及读取入口；身份、存储/解码、事务或 claim 失败仍整体拒绝，不能以此吞掉错误。此提示既不返回空 changed 假称无变化，也不更新账本；无需回退作者或全篇旧 patch。新 claim 无需从旧 HEAD 回放；冻结 retry 也不读 HEAD 或当前 Report。差异是辅助信息，不放宽任何写锚点。

## S3：可枚举 anchor 与最小重读

读/写成功回执增加结构化 `anchor: {doc_rev, whole, known_blocks}` 及一行 `anchor: doc_rev N · whole=yes|no · K blocks known`；未读用 `anchor: null` 和 `reason: never_read`，不假造 rev 0。index-only 说明未增加知识。whole 的有效性必须结合当前 doc_rev；回执数据来自同一事务结果，不在回执阶段重新查询一个新版本。读取全文/局部、提交、whole write 的 summary line 都带 anchor。外部编辑后账本不会自动变新。

拒绝原因使用 typed enum（snake_case）而非分析英文文本：`never_read`、`unread_block`、`unread_section`、`whole_required`、`changed_since`、`document_changed`、`section_membership_changed`、`batch_dependency_conflict`、`superseded_session`。前三类及 whole_required 保持现有 -32602；真实 revision/结构冲突保持 -32409；superseded_session 仍用身份 admission 的拒绝码，不能为知识诊断绕过认证。已删 session/token 的失败保持现有认证拒绝，不泄露 Report；不把存储故障误报 never_read。`knowledge_unavailable` 是展示失败状态，不是锚点失效原因。

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

`changed[]` 只描述事务开始 D0 上外部知识失效的相关差异；doc 级失败才附整个 Report delta。所有 -32409 带数组，纯顺序/summary/doc 变化与批内冲突可以为 []，不伪造块 rev。section 冲突另带 `sections: [{title, known_members, current_members}]`（有序 id/rev 对）。`rev` 旧单值字段和 message 数字解析一次删除；`doc_rev` 保留但不是重试 token，输入仍不接受 revisions。

### D0 外部失效与有序 batch 依赖

既有 [ops.rs](../../crates/calm-server/src/track_report/ops.rs) 按序操作同一个内存 ReportDoc，MCP 的 duplicate 检查只分别覆盖 block id 和 section 名，不能假定跨 section/block 没重叠。确定反例：完整读 D0，op[0] replace section A 用 marker 保留 b.id 并改 b 内容，op[1] delete b。op[1] 的 anchor 仍绑定 D0 的 b.rev，而执行到 D1 时 b.rev 已变；D0 知识完全有效，重读原 batch 永远无效。不能默认为自身前项授予后项新 rev，否则会改变现有有序执行/锚定语义。

保留既有执行器为唯一行为权威，不新增 MCP 模拟器，不预跑另一套操作或把 D0 诊断变成改变成功集合的前置拒绝。在 Report 领域 comparator/执行器把错误改为 typed predicate（block rev、section members、doc anchor、目标存在等），保留失败 op_index 和该时刻实际观察值；外层持有不可变 D0 与绑定账本。仅在真实执行失败后、事务回滚前，复用同一 comparator 比较 D0：

- 失败前提在 D0 已不成立：按 `changed_since` / `document_changed` / `section_membership_changed` 或未读原因报告外部失效；汇总本 batch 对 D0 的相关失败前提，不能混入 D1…Dn 的值。建议仅修复这些证据，不保证错误内容或相互依赖的原 batch 可直接成功。
- 前提在 D0 成立、到失败步骤才不成立：`reason: batch_dependency_conflict`，保持该原失败的错误码（revision/结构 Conflict 为 -32409，目标消失等 BadRequest 仍 -32602），`changed: []`、`reread: null`。带 `failed_op_index`、`target`、`action: revise_batch`，建议合并重叠修改到同一 section op，或拆成独立提交，第一笔成功后按其回执/必要读取再构造第二笔。不给模型传 revision 的通道。可用执行 trace 提供确证的先行 op_index；不猜测没有记录的 cause。
- 若还有其他 D0 失败，作为 `external_failures` typed 数组另列各自 changed/reread；不把失败步骤的批内原因改称 changed_since，也不声称外部重读可消除批内依赖。身份/存储/内容校验失败保持原分类，非 anchor 错误不塞进上述枚举。

同样覆盖先前 block op 删除标题/移动成员导致后续 section 证书失配等同类路径。同名 section replace→delete 在现有 MCP duplicate 检查即被拒绝，不冒充可达的执行器反例，也不放宽该检查。执行到哪里失败、事务零落盘、task guard 和既有有效 batch 的成功行为保持不变；诊断 trace 不是持久历史或新授权来源。

### 最小重读及操作修正

`whole_required` 与 `document_changed` 是不同前提：whole write 先要求 whole=true，再校验当前 doc_rev；summary/新块/move/创建从未认证的缺失声明 section 只有当前 doc anchor，**没有 whole 要求**。证据是 [track_report_blocks.rs](../../crates/calm-server/src/mcp_server/tools/track_report_blocks.rs) 的 write_markdown/commit、[track_report.rs](../../crates/calm-server/src/track_report.rs) 的 `check_doc_anchor` 和 [sections.rs](../../crates/calm-server/src/track_report/sections.rs) 的创建分支；[report_read_ledger.rs](../../crates/calm-server/src/report_read_ledger.rs) 的任意成功局部 text read 都更新 doc_rev。不因 doc 级比较范围扩大内容读取权限。

| 拒绝的操作/前提 | `reread` 最小范围与边界 |
| --- | --- |
| 已有 id 的 upsert/delete，外部 rev 失效或未读 | `blocks` 为相关仍存在的 ids，batch 汇总同类失败。已删除 id 保留 current=null，不建议读取不存在的 id；`scope:index` 只发现目标，`action: revise_target`，须删除失效操作或明确选择替代目标，不能承诺原 delete/upsert 自动成功。 |
| 已有 section replace/delete，证书失效或未读 | `sections` 为受影响仍唯一存在的标题。只读块不能更新 section 有序证书。标题消失/歧义先 `scope:index` 与 `action: revise_target`，不建议读取不存在/歧义标题。曾认证但已消失的 section 按现有 Seen 规则冲突；若明确要重建声明 section，需实际全文读清旧证书后按新意图提交，不能靠 doc anchor 局部刷新绕过该规则。 |
| whole write 的 `whole_required`，或 whole=true 但 `document_changed` | `scope:full`，实际渲染全部非 task；不能只读 changed blocks 后承诺可 whole write。 |
| summary-only、创建新块、move、从未认证的缺失声明 section 的 `never_read`/`document_changed` | 优先复用本次其他失败已要求的成功 text read（block 或 section 即可）；否则选 D0 中一个仍存在的非 task 块做 `scope:blocks` text read，优先已知块、其次文档顺序首块。无需读全部 changed；不认证无关 section、不授予未渲染内容。move 的未读目标不额外要求内容证据，保留现有允许仅读另一块后 move 的行为。 |
| `batch_dependency_conflict` | `reread:null`，`action:revise_batch`；重读不是修复。 |

最小并集先合并相关 sections/blocks；如果已选读取会刷新 doc anchor，就不为 doc 级前提再加一块。只有确需 whole 或清除消失 section 证书才升级 full；full 覆盖非 task 读取，task ids 必须显式保留在额外 blocks 中。若 doc-only 失败没有非 task 块、但同批要求读仍存在的 task，读这个 task 也可刷新 doc anchor；不另加 full。没有任何其他必读项、且非 task 集合为空（空 Report 或只有 task）时，建议 `scope:full`，返回空的非 task text 也按现有 all_seen 规则记录 doc_rev/whole，不读取或认证 task。index-only 永不刷新 doc anchor。

不承诺局部读取一定 whole=false：只有实际覆盖所有非 task 块时才按既有 all_seen 规则授 whole；大型 Report 仅读一个块时 whole 仍 false，其余块仍未知。section 内双成员部分重读可合法形成 sections[b@1,c@1] 与 blocks[b@2]，应正常加载并继续拒绝 section 写。不同读取之间若再发生外部编辑，可能仍需重读；建议不是自动合并或自动重试。未知/未读 -32602 也给对应最小范围但不假称版本冲突；已删目标/歧义/批内依赖必须修正操作。未认证、superseded 不返回 changed 或泄露范围。

## 调用方审计与删除清单

| 入口/调用方 | 实现切片要求 |
| --- | --- |
| MCP report_read：全文、markers、sections、blocks、index、resolve | 一个领域 read-and-record 入口；保留实际 rendered 集合、whole/section/task 规则。hydrated index 或 source 文档不是 Report 知识。 |
| MCP report_commit / report_write → sink → persist → ops/sections | 两个嘴共用事务内绑定、检查、保存；不在 anchors.rs 提前读库制造 TOCTOU。批操作失败零落盘，receipt enrichment 失败不撤销已落盘知识。 |
| REST Report 全文、block CRUD/move、reset；初始创建/fork、task/worker verdict | [write.rs](../../crates/calm-server/src/track_report/write.rs) 的目的专属入口和 structural_init_report_tx；只修改 Report，不推进任何模型账本。worker task report/verdict 不等于 Planner 已看见。 |
| track cat/部分 Report 路径、links、REST read、series、workspace_reports/history、track status | [track_file.rs](../../crates/calm-server/src/mcp_server/tools/track_file.rs)、[report_links.rs](../../crates/calm-server/src/mcp_server/tools/report_links.rs)、[workspace_reports.rs](../../crates/calm-server/src/workspace_reports.rs) 共用快照读取但全部不锚定；CLI/MCP 不新增隐式例外。 |
| dispatcher refs、ReportEdited、queue fold/replay、issuance、track-vcs wake | canonical 新 claim 冻结 V2，hydrate/retry 只消费冻结值；历史队列发行用同一领域 delta。旧事件仍作真实 source/审计事实，不能作已知基线或展示 fallback。覆盖 plan/proof/擦除/升级预检，不绕过原 producer 授权。 |
| AppContext / sink / 所有 fixture 构造 | 删除 ReadLedger Arc/Mutex、Clock/TTL、generation、capture/publish、惰性 caller-card 清理。test 通过真实 DB/session 建立状态，不自动补齐 required fields。`rg read_ledger/LastRead` 的所有调用方都迁移，含 vocabulary、concurrent_sessions、sections 和 anchor_knowledge 测试。 |
| 指南、描述、示例/生成物 | 删除两处 doc_rev ≥ 已读说法；更新 read/commit/write prompt 和冲突契约。无兼容双输出。若实现改变 schema/OpenAPI，则跑真实生成器带齐产物；本文不生成。 |

## 生产入口验收与分片

S1 先交持久化、同事务 read/commit、清理和 anchor；S2 再接新 wake 并删旧机制；S3 最后换 typed conflict。每片 L2 两路独立 Codex 评审，明确检查抽象层、重复逻辑、硬编码、外部契约事实与永久维护成本。首轮两路完整评审见 track 的 `runs/e0c67d85560547c4827e407f4df5d4cc:design-review-a-v1.json`（event 716358）与 `runs/e0c67d85560547c4827e407f4df5d4cc:design-review-b-v1.json`（event 716312）。本次补齐 canonical 冻结与升级、batch 分类和 doc-anchor 最小读取三项阻塞，以及独立证书/空间测量两项建议；仍由 Planner 安排 L2 两路重新完整评审，不把本文修订当作评审通过。

实现时先在真实 MCP/REST/harness 入口加最小 red 复现，再修实现，不能在 fixture 手写一份 delta 算法。计划至少覆盖：

1. 磁盘 DB text read →销毁并重建 repo/AppContext（同一身份 session）→无重读直接 commit；超过 2h 也成功。index-only/从未读/新 session 拒绝。升级空账本须读，旧内存不回填。
2. partial read 推进 doc_rev 但不授 whole；跨版本局部合并、空 Report、按 id 读 task、sections 重名/不存在/来源分裂合并/移动、marker 保 id、own move 未读内容不授知识；[mcp_report_anchor_knowledge.rs](../../crates/calm-server/tests/cases/mcp_report_anchor_knowledge.rs) 全部断言保持。原“新建内存 ledger 为空”断言迁成“新 session 为空”，另测重启持久，不能保留与新契约相反的旧断言。
3. REST/user/Assistant 外部编辑后重启仍冲突；自身无关提交不吞掉外部改动或删除的 old rev，wake 能给 old→new/null；自身删除不再回送。summary-only/foreign move 的 changed=[] 仍提示 doc/section 状态且写 guard 拒绝。
4. 双读、读与 commit、同 session 两提交交错（显式 barrier）：只允许事务顺序的完整结果。batch 后项失败、task guard 拒绝、ledger 保存失败使 Report/task/event/ledger 全回滚；提交后 warning 查询故障仍可用已提交 anchor 继续写。
5. supersede 两条入口、失败新启动补偿、session 删除及 card/track/area 直接删除均无孤儿；事务回滚保留，恢复 superseded session 后明确需重读；其他 session/Report 不受影响，fork 不继承知识。
6. 60+ 次 Planner/Assistant/user/kernel 交错提交，真实 harness 发 wake，精确断言 ids/revs 与实际未见差异一致、自身已写 rev 零回送、没有 mixed/unknown/50 上限；无 HEAD/相同 HEAD、重启、旧队列 replay、折叠事件均使用同算法。两次独立新 wake claim 不会推进知识；读完后下一次新 wake 的对应差异消失（已有冻结 retry 不变）。小/大/混合 diff 阈值、UTF-8/围栏、未知 task 只引用、全部变更 ids 不漏列。
7. MCP -32409 数据逐项断言 changed 的 known/current/null、doc_rev、reason 与最小 reread；外部多 op 失效按 D0 汇总，纯结构变化数组为空。对无内容/目标/批内错误的合法 batch，只重读建议后重试成功，其余块仍未读；不把这个保证套在 revise_target/revise_batch 上。无 revision 输入通道，未认证/superseded 不泄露 changed。
8. 真实 dispatcher → canonical tick → claim → input_dispatch → provider 测试入口（沿用现有 backend 测试适配器，不新造协议模拟器）：原 ReportEdited observation/source/ref 保持相等，新 plan 只有一个知识 delta、无旧事件 diff。claim 后分别改变 Report 和账本，首次 crossing、await 后 rehydrate、进程重启的未 crossing 恢复均仍逐字等于冻结文字；journal proof 的 V2 plan ref/digest 精确一致。篡改原 observation/source 仍拒绝，篡改冻结呈现/关系使 digest/validator 拒绝，擦除任一依赖拒绝且无原 observation 回退。独立 human/system wake 均冻结一次 context，非 Report 文字不受抑制。
9. 用实际 input_dispatch 的 NotDispatched/release 入口断言普通 initial 被释放后新 PendingPrefix claim 可产生新 delta/digest，而 ExplicitSuccessor 释放后恢复原 retry intent、后续仍用原 root plan/digest；不能用 fixture 直接改状态证明此分界。Unknown/CrossingUnknown 重启不重发；实际 Human retry 路由 → retry intent → canonical/retry → provider 继承同一 V2 root plan、字节与 frozen invocation（新 submission/attempt 身份可变，plan digest 不变），Report/账本/配置随后变化无影响；授权失效、未结算 exact thread、擦除仍阻止 crossing。
10. 磁盘升级 fixture 经真实 activation/admission：旧未 claim 输入保留真实 producer 绑定并生成 V2；V1 已 claim 无 attempt/有效 NotDispatched/待执行 ExplicitSuccessor 阻止激活，不能被说成已发历史；已按真实入口 release 后可激活。V1 unknown/accepted/keep 分别保留精确事实和迟到 settlement，未知不重发；V1 显式 retry 与 fresh crossing 返回 render_plan_upgrade_required，原 receipt 重放不变。暂停边界阻止并发旧 writer，升级提示覆盖空账本与冻结计划不兼容；无迁移修改旧 digest/proof。
11. MCP 最小读取反例：大型 Report 只读一块后 summary/创建/move 未读块/创建从未见过的缺失声明 section，经外部 doc_rev 变化后只读建议的一块即可提交，whole=false、无关块未读、目标未读内容不因 move 变已知；whole write 仍拒绝直到 full。空文档、只有 task、doc+task 混合、已删除块与消失/歧义 section 分别断言上述范围/action；双成员 section 局部重读产生不同 rev 证书，持久加载成功而 section 写仍冲突。
12. 完整读 D0 后，经真实 MCP 提交 marker 保 id 的 replace section A → delete b：断言 batch_dependency_conflict、changed=[]、reread=null、failed_op_index=1、revise_batch，所有持久状态回滚；重读后原 batch 仍同类失败，合并或拆分操作才成功。另测 block 删除标题/移动成员后 section 操作的结构依赖、同名 section 重复仍在 MCP 解析阶段拒绝，以及外部 stale 与批内依赖同时存在的分类；已有合法有序 batch 仍成功。使用现有执行器与真实入口，不在测试 fixture 复制执行策略。S1 的磁盘 churn 测量按前述成本模型记录逻辑字节与删除积累。

实现 focused 命令使用现存注册目标：`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test mcp_integration_suite mcp_report_anchor_knowledge --test-threads 8`；vocabulary/concurrent_sessions/sections 同 target 按测试名过滤。harness/dispatcher 与 migration/cascade 分别选 calm-server lib 和 calm-truth 的实际新增测试名；注册后用列表确认非零，禁止虚构过滤器或空通过。关键安全断言按 AGENTS.md 单因素 mutation，预报全红集合、跑整个相关 filter、恢复并复绿；不在本设计任务中修改生产代码做 mutation。

每片 worker 只跑 focused 和格式修复；kernel gate 跑 quick Rust、文本、contract 及一次非空 focused replay；CI 跑 broad suites，不在共享主机跑真实 Codex E2E。不增加测试名永久白名单/额外 CI 扫描；普通新增用例只改所属测试和必要注册，每工作流沿用既有 gates。当前设计仅运行任务声明的 `scripts/local-ratchet-gates.sh` 文本 gate；未运行 Rust 或行为测试，不宣称 mcp_report_anchor_knowledge 已绿。

## 独立 ABA issue 建议

建议标题：`fix(report): prevent stale anchors from matching reused block identities`。证据是 [report_read_ledger.rs](../../crates/calm-server/src/report_read_ledger.rs) 第 9–10 行，以及 [ops.rs](../../crates/calm-server/src/track_report/ops.rs) 的 reset 说明：foreign delete 后重用同 id、同 rev，旧 `(id, rev)` 可误匹配；REST if_rev 同样存在。持久账本只是延长可遇到的窗口，不修复它。

独立 issue 验收：构造 read A →外部删 A（含 tombstone 删除/reset）→新内容复用 A.id/A.rev →旧 MCP anchor 和 REST if_rev 均拒绝；在 ReportDoc 身份生成/版本拥有层选择不可重用 incarnation 或单调版本，审计 fork/reset/marker/历史恢复，明确 migration 与 API 影响。不能在本片悄加 doc_rev 全局校验来“修复”而破坏局部并发写。本文不新开 issue、不宣称 ABA 已解决；Planner 在独立任务先查重再建 issue。
