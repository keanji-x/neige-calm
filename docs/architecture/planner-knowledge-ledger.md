# Planner Report 知识账本

仅设计；基线 `a4b53515b28b953a9b9dc2425dc47046f1247fec`（#3186）。L2：授权/持久化边界。S1–S3 仅 Report，非 Report 保持。Planner 安排双 Codex L2 完整评审后开 PR/合并，将文档路径及精确 head 发父 Planner，明确通过后实现。ABA #3185 另案。

## 源码与范围

[账本](../../crates/calm-server/src/report_read_ledger.rs)拥有 LastRead/record/project_commit；MCP read 选择 rendered ids，track_report_read.rs 负责 CRDT 优先/NULL/旧布局。`track_report_blocks.rs`→decision_sink::commit_report_op→[persist](../../crates/calm-server/src/track_report/write.rs)→ops.rs/`sections.rs`，唯一执行 owner。

event718084 核实：[snapshot](../../crates/calm-truth/src/track_vcs/snapshot.rs)的 report.md 只有 body，cards/<report_id>/.payload.json 有稳定 id/rev/kind/payload；[cat_at](../../crates/calm-truth/src/track_vcs/read.rs)需 commit/path，无 (id,rev) 索引；[GC](../../crates/calm-truth/src/track_vcs/gc.rs)不保护旧块知识。非 Report latest=self 不排除外部改动，不扩项。

## S1：持久化与事务

migration `0194_planner_knowledge_ledger.sql`，已发布不改。

| 表 | 必填列与约束 |
| --- | --- |
| report_read_ledger | worker_session_id FK→worker_sessions(id)、report_card_id FK→cards(id)，均 ON DELETE CASCADE，复合 PK；doc_rev 非负整数、whole 为 0/1、sections_json 严格 title→有序(id,rev)列表；report_card_id 索引。 |
| report_read_ledger_blocks | 同一复合父 FK ON DELETE CASCADE；block_id 与父键组成 PK；rev 为 u32；history_ref JSON 二选一 `{commit:hash}` / `{unavailable:reason}`。无 kind/content 副本。 |

`LastRead {doc_rev,whole,blocks:id→rev,sections}` 保留。calm-types Report 值类型，truth 存取/严格解码，server 认证/投影/展示。缺行未读，损坏不吞。sections 独立：section=[b@1,c@1]、blocks[b]=2 可加载但 section 写冲突；外部删块可留旧证据。unavailable 不否定已读 rev。全文非 task 置 whole；跨 doc_rev 局部读清 whole，同 rev 保留；task 按 id。

**读**：report_read 经 db::write_in_tx_typed 的 BEGIN IMMEDIATE writer 事务，重验认证 session/card/Report 归属及当前性，加载 card+CRDT，选择/渲染主 text，冻结 rendered ids/anchor，合并保存后提交。禁止升级 read-pool 快照；任一失败不返回成功 text。index 不记账。提交后断线仍算成功生成的阅读，重试幂等，不声称网络原子；衍生 diagnostics/hydration/warning 失败不撤销知识。

**定位**：读事务内 VCS 暴露 tx 版 head/tree/blob 查询，复用 store::head_in_tx/tree_at_in_tx/cat_at。验证 commit payload 的 rendered 块 id/rev/kind/payload 等于渲染快照才记 commit；无 HEAD/对象、旧 payload 无 blocks、不匹配则 unavailable，不以新 HEAD 补绑旧 rev。局部只覆盖 rendered 定位，允许 b@1→C1、c@3→C7。全文沿 record 清换非 task 集合；未渲染 task 只保留仍相等的已知项。定位 commit 非块作者提交。

**写**：MCP 原始无 revision 意图/身份进 persist；load 身份/账本/CRDT、bind/check、Report/tasks/events/project/save 同事务。删 handler 预绑定/sink capture；REST/reset/fork/任务投影不记账。

自身 commit：[events](../../crates/calm-truth/src/db/sqlite/events.rs)::write_with_actor_events 现于闭包返回后才授权、append、VCS commit。提取该通用事务步骤为可复用原语，返回实际 track→CommitHash，旧 wrapper 共用，保持授权/事件顺序/提交后广播。Report persist 在同一事务的投影/事件/历史之后、SQL commit 之前保存知识；事件层不识别账本，不复制写入器/生成第二次历史/事后补 HEAD。VCS commit.rs::commit_events_with_author_in_tx 已返回 Option<CommitHash>，Report 编辑应取得 Some，否则失败。验证该 payload 最终 written 块后覆盖定位，未写保留旧定位，自身删移除。授权/历史/账本失败全回滚，enrichment 消费冻结 anchor。

`project_commit` 保留外部改动/删除的 stale blocks 及定位，own 写覆盖、own 删移除；whole/doc_rev 仅按原 anchored 条件推进。无关 own 写不能洗白外部 b@1→b@2。sections 仍保守认证内容与结构来源，不由 stale/零散内容推断；全文重读才清换集合。

**batch**：ops.rs 原循环复用 Written/written_block_ids/section_output_ids，逐步推进临时知识，后 op 执行时绑定有效证据。前 op 实写新 rev 后可 delete/upsert，section marker 保 id 同理。删 MCP 同目标一次限制/预绑定，保留语法/内容校验。不能凭累计 written ids 认证最终快照：own 删移除，move 不授未知，外部 stale 不因无关 op 变新。结构只由有效来源证书/实际 section 输出推进，未知成员不认证。doc/whole/task/objective/compact/声明 guard 不放宽。最终存仍存在的已知 rev，written 定位自身 commit；失败全回滚，

**清理/GC**：[session_mirror](../../crates/calm-truth/src/db/sqlite/session_mirror.rs)::session_supersede_active_tx、session_mark_superseded_tx 及包装 转换事务清账，扫全路径，回滚一起恢复。`compensation.rs`恢复旧 session 后不恢复已清知识，须重读。session/card/track/area 删除级联；reset 留 stale，fork 不复制。idle/重启/resume 不清，session/Report 隔离。升级空账本须读，不回填。

S1 将所有尚存账本 commit 引用并入 gc.rs::active_diff_endpoint_commits_tx，不能仅筛 active harness。缺引用/损坏沿既有跳过 prune 规则，展示明确缺历史。与 last_seen_head/issued_turn_head 共用 floor，不建保护通道，非 Report endpoints 保留。引用/GC 同 writer 串行，覆盖/清换/清理释放引用后可回收，sweeper 沿 commit/tree 保护。最老引用保留后缀；测积累/释放后的 commit/对象量，无 TTL/逐出。

## S2：观测产生处的增量

Report owner 一致读取目标 session 账本/当前快照：current ids ∪ known ids 的不同 rev 列 `{id,known_rev,current_rev}`，缺侧 null，现存文档序/删除 id 序，不截引用。相等（含 own 写）不列。doc_rev/有序 sections 另比，纯结构 changed=[]。wake、cat/history/status、冲突、hydration 不记账。

入口：[dispatcher](../../crates/calm-server/src/dispatcher/mod.rs)::resolve_event_observation/harness_observation_from_event → [canonical_inputs](../../crates/calm-server/src/dispatcher/canonical_inputs.rs)::accept_canonical_events。第二 writer tx 中 PlannerAdmission::authorize_tx/current_input_content_carrier_tx 重验 cards.session_id/worker；未接受的 Report 事件在 binding.authorize_tx 后、accept_pending_system_tx 前生成 session 建议，不在 qualify。保持 event fingerprint/track/card/session/producer/source；`acceptance.rs`::event_system_observation_id_tx 绑定该内容。DispatcherInputSource::authorizes_tx 仍比较已存观测，不用文本替换授权源。

ReportEdited 增可选 knowledge_text，新观测必填，to_turn_text 逐字使用；旧呈现不变，event/body/hash 保留，标 known/current。legacy live 也从 harness worker_session_id 调 owner 再入 observe/queue；fold 取最新完整 knowledge_text，不拼 before→after。产生后又读允许略多，下一新观测重比。D4 仅新观测，重复 acceptance 用原内容；历史/SubmissionRenderPlan/claim/retry/proof/会话存储不改。

VCS read::since_last_turn_block 移除 Report 输出，由 ledger delta 统一给 report.md 行/正文；已知不列，无/相同 HEAD 不短路。非 Report/last_seen_head/作者逻辑保持；Report 删除 own/other/mixed/unknown/50 提交归因。

小变更 history_ref 取旧块、验 id/rev，复用 [report_edit_diff](../../crates/calm-types/src/report_edit_diff.rs)展示/转义/data fence 做真 diff，每块 ≤24 行/2048 UTF-8 bytes，总正文 ≤8192 bytes（含围栏），超界整块引用。新增空 before/删除空 after。未读/缺历史明确标注，不伪造 diff；大改/未知 task 仅 id/标题/known→current/读取引用。作者只附 commit_record 直接可得的 commit 作者，缺失省略。

## S3：anchor 与冲突

读/写冻结返回 `anchor:{doc_rev,whole,known_blocks}` 及一行 `anchor: doc_rev N · whole=yes|no · K blocks known`；未读 null，index 明示不增加知识；whole 有效性结合当前 doc_rev，不二次查询新状态。删除 prompts/guides/report.md与 `observation.rs`两处 doc_rev≥已读建议（新观测取代旧分支；旧冻结历史不重写）。

检索无 R4 共享类型；rev_conflict_error 仅抽 message 数字。只沿 R4 reason_code/target/next_step 形状，Report typed 原因/MCP 映射，不造框架：

`{reason_code:"changed_since",target:{report_card_id,block_ids,sections},next_step:{action:"read",blocks:["b"],sections:[]},changed:[{id:"b",known_rev:1,current_rev:2}],doc_rev:8}`

仅 never_read（块/section）、changed_since（含文档/成员）、whole_required、superseded_session。未读/缺 whole 为 -32602，真实冲突 -32409，superseded 沿 admission 拒绝码；失效身份不泄露 changed/范围，DB 错误非未读。changed 为同事务相关证据，附块/section 重读列表；不解析英文/做诊断矩阵。

whole 写须 full；块读相关现存 ids，section 读唯一标题。doc-only（summary/创建/move/从未认证的缺失声明 section）允许局部 text 刷新，无 revision。空/仅 task 的 full 空 text 可刷新但不认证 task，task 按 id。删目标/消失或歧义 section 给 index/revise_target；曾认证后消失的 section 须 full 清证书再提交新意图。建议不授 whole/section/task 权限。

## 删除与验收

相对 #3186 删除：Arc/Mutex/Clock/TTL、generation、capture/publish、惰性 caller-card 清理、handler 预绑定、数字抽取、旧 Report 作者归因/双 diff。放弃旧大设计的 kind/content 副本、plan V2/替代呈现关系、claim 改造、retry 分界、proof 扩展、V1 排空预检、D0 诊断/第二执行器、batch_dependency_conflict/external_failures、逐场景最小重读表。非 Report 使用的作者扫描不可整段删。扫 read_ledger/LastRead 全调用方、fixtures/描述/生成物；schema 改动运行真实生成器。

S1 持久化/事务/清理/GC/anchor，S2 观测/diff，S3 信封。双 L2 查边界/重复/硬编码/外部事实/维护成本。先生产入口 red：

1. MCP read→磁盘重启/超2h→commit；index/新 session/升级未读拒绝。局部 commit 不串/不绑新 HEAD；缺历史仅引用；whole/空/markers/task/独立 sections。
2. MCP→persist 读写交错，自身定位匹配；后 op/guards/授权/VCS/ledger 失败全回滚 Report/tasks/events/VCS/ledger；warning 失败后 anchor 可用。
3. section 保 id 改 b→delete b 成功，多次自身写绑定最新 rev；move 不授未知，无关 own 写不洗外部 stale/删除，section 不凭零散内容认证；后项失败回滚；doc-only 局部读成功，whole 仍需 full。
4. 两个 supersede、补偿重读、事务回滚、session/card/track/area 直接删、fork/reset；超50提交且 wake 推进 HEAD 后旧块仍可 cat_at，own 覆盖/清理释放引用；sweep 后真 diff 可用。
5. 真实 dispatcher→canonical acceptance→claim→input_dispatch→测试 provider：各 session 建议独立，已知/own rev 正文及 report.md 行均零回送；无/相同 HEAD、60+混合提交不漏外部。篡改 event/card/session/source 拒绝；接受后读/编辑/重启/retry 字节冻结，新观测仅未见 rev；legacy live/fold 同测，历史/非 Report 保持，不复制 delta fixture。
6. 真块 diff/大改引用、删除/null、UTF-8/围栏预算、缺作者省略；四原因/码/target/next_step/重读与身份不泄露。关键授权/回滚/不漏 stale 断言按 AGENTS.md 单因素 mutation：预测完整红集、跑相关完整 filter、恢复复绿。

实现测试用 mcp_integration_suite、dispatcher/harness lib、calm-truth，list 非零；worker focused、kernel gates/replay、CI broad，无真实 Codex E2E。本文字节/链接自检，文本仅 kernel gate。
