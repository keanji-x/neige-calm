# 统一 Report 浏览

目标：用 `neige ls/cat`（MCP `neige_ls/neige_cat`）替代重复读取入口，
Planner、Assistant、Worker 获得相同的跨用户 Area Report 只读能力。
沿用[既有读权限设计](assistant-read-access.md)，本设计取代其 workspace 工具命名，
不改变 HTTP 查询契约，不引入数据库迁移或前端改动。实施为 **L2**：涉及跨角色、
跨 Track 的 authority/isolation 边界。本轮只交付设计，不执行下列实施检查。
Issue 创建 op `0794741ebde04955bf3ea8f0baf8bb78` 尚 pending，编号未知。

证据：本 Track 调查结果 `runs/962955a9f18a4018aba502aac9c643cb:inspect-unified-report-reads/result`
（`neige track cat` 可读）；以下链接指向已核对的当前源码，描述的是目标契约，
不代表新入口已经存在。

## 路径与授权

| 入口 | 契约 |
| --- | --- |
| `ls /` | 全局根目录，列出 `/areas/`、`/report-changes/` |
| `ls /areas/` | 所有 `kind=user` Area，稳定 ID、可读标题、绝对路径 |
| `ls /areas/<area-id>/` | 列出该 Area 的 `tracks/` |
| `ls /areas/<area-id>/tracks/` | Track 目录，包含 closed Track；每行给 Track ID、标题、绝对目录路径及 Report 路径 |
| `ls /areas/<area-id>/tracks/<track-id>/` | 只列 `report.md`、`report-edits/`，不暴露整个 Track 文件树 |
| `cat /areas/<area-id>/tracks/<track-id>/report.md` | 完整正文，或原 `blocks`/`sections` 选择，或 `detail=index` |
| `cat /report-changes/<YYYY-MM-DD>.json` | 所有用户 Area 的当天 Report changes 聚合 |
| `cat /areas/<area-id>/tracks/<track-id>/report-edits/<YYYY-MM-DD>.json` | 指定 Track 当天 edit events，需 `through_event_id` |
| 无 path 的 `ls`、相对路径 | 保持绑定 Track 的现有文件、guide、Report 视图；`cat` 仍要求 path |

绝对路径必须在旧 normalize 去掉前导 `/` 之前分类。新 resolver 使用窄 typed scope，
只接受表中语法；拒绝 traversal、Area/Track 不匹配、未知 ID 和 foreign 非 Report
路径（cards、conversation、runs、gate、ops、host 等），不把目标 Track 交给完整
`TrackFsView`。日期目录是参数化投影，`ls` 返回带日期占位说明的目录元数据和空 entries，
不枚举无限日期、不把模板当可 cat 文件；实际日期由 cat 严格校验。

三角色的 registry availability/discovery、直接 MCP、CLI 均一致；共用同一 registry
和执行 guard。保留 live session/token/thread/card/role/track/area binding 校验，路径
不得重绑定 caller。绝对全局路径只接受 user Area，即使显式指向自身 system Track 也
拒绝；相对自身 system 视图（含 Today/launchpad Report）照旧。全局目录隐藏退休
Area-conversation 容器，复用并下沉 HTTP 的 `user_visible_track` 规则，不在 CLI
复制 purpose 特例；日期查询保持原始 user-kind 事件集合，不因目录隐藏容器而丢历史。
仍保留该隐藏容器的直接 user Report 读取。历史容器的实际数据分布尚未逐项核实。

不新增人类 membership 推断：HTTP Principal 与 agent ToolCallIdentity 不是同一身份；
这里延续已有 workspace 的跨 user Area 范围。所有浏览（含 own Report/index/history）
不写 `report_knowledge`；`report_read` 的 ledger-anchor 写前读取协议保留独立职责。
Worker 仍不可写；Planner/Assistant 仅可按既有 anchor 协议写自身 Report，foreign
selector 不得进入写 API。`link_ls`、mail 的 same-area 权限不随目录扩权。
证据：[track_file](../../crates/calm-server/src/mcp_server/tools/track_file.rs)、
[managed guard](../../crates/calm-server/src/managed_track.rs)、
[写身份](../../crates/calm-server/src/track_report/identity.rs)、
[HTTP Track 过滤](../../crates/calm-server/src/routes/tracks.rs)。

## 返回值、分页与水位

`ls` 统一 `{entries, next_cursor}`；目录行有 `id,title,path,kind`，Area/Track 行再给
`area_id`/`track_id`，不得返回 `default_cwd` 等完整内部 model。Track 行复用旧
area outline 的 `closed_at,creator_track_id,doc_rev,updated_at,active_tasks_count,blocks,
blocks_truncated`；heading 预览仍最多 40 blocks，明确不是完整 index。
目录按稳定 ID 升序 keyset，用现有 `Page`（50 行、32 KiB，可提前结束，超大首行独占页），
`next_cursor` 为末行 key，末页 null；空页不得漏行，foreign/已消失 cursor 拒绝。
续页固定同一目录；cursor 不跨 Area/目录使用，helper 与 owner 校验本目录 key，
客户端视其为 opaque。目录不是数据库快照，新插入的较小 key 需重新开始查看。
CLI 文本必须显示 ID、标题、路径及续页提示，`--json` 输出完整结构。

`cat` 保留 `{content,content_type}`。full/blocks/sections 的 content 为 Markdown，
`content_type=text/markdown`；`detail` 默认 full，index 与 blocks/sections 互斥，
沿用既有选择错误契约。index 的 content 是 JSON 文本（`application/json`），包含
`track_id,area_id,title,summary,doc_rev,blocks:[{id,kind,rev,heading}]`，文档顺序全量返回，
不沿用 40-block 截断、不分页、不记录知识。正文和 index 各从一次自洽 snapshot 投影。
CLI 普通 cat 输出 content，JSON 模式仍保持现有 content 渲染行为，因而 index 的
ID 在两种模式均可见；不要增加另一份不被 renderer 消费的 index 外壳。
`step` 只对自身 gate log 有效；`cursor/through_event_id` 只对日期 JSON 有效，
非 Report 拒绝 Report selectors，日期 JSON 拒绝 selectors/step。

日期 JSON 也放在 `content` 中，MCP 旧 changes 字段全部保留：`date,timezone,
through_event_id,changes,next_cursor`，每行保留计数、首末 event IDs、summary before/after、
500 行 patch 及 `patch_truncated`；edits 保留 `edits,next_cursor`、event_id、
RFC3339 `edited_at` 和完整 edit（含 edit_id、body/summary before/after）。HTTP
继续使用原 DTO 的 `at` 毫秒及原 route/query，不因 MCP envelope 改动它。

日期 owner 保持严格 YYYY-MM-DD、时区半开日窗 `[start,end)`；普通默认 Asia/Shanghai，
managed metadata 的 timezone/context 保留。changes 首次可省水位，snapshot 中取 MAX(id)，
续页必须原样传回 `through_event_id`；edits 首次与续页都必需该非负水位。
固定 date/timezone/scope/through，不在续页重新取 MAX；closed Track、净回滚的
edit_count、compact payload decode/scope 校验均保留。changes 每页 20、按 Track ID；
edits 每页 20、event ID 升序、cursor 为十进制 event ID，保持原水位/keyset 语义，
不改用目录 cursor、Git hash 或目录字节预算，不引入 HTTP cursor 格式变化。
证据：[分页 helper](../../crates/calm-server/src/mcp_server/tools/paging.rs)、
[outline](../../crates/calm-server/src/mcp_server/tools/report_links.rs)、
[现有日期 owner](../../crates/calm-server/src/workspace_reports.rs)、
[旧 MCP 投影](../../crates/calm-server/src/mcp_server/tools/workspace_reports.rs)、
[HTTP DTO](../../crates/calm-types/src/daily_planner.rs)。

## 实施清单与删除边界

1. server 浏览 owner 承担统一 scope resolver/只读 fence；Area 查询复用
   `RepoRead::areas_list_user_visible`，Track 查询复用 `tracks_by_area`，输出窄投影。
   Report 文档投影只留一个 owner，以 `track_report_read::load_report_doc_snapshot`、
   `load_report_for_track`、既有 Selection/parse_select_arg/selected_blocks_text 为基础，
   own、foreign、legacy Report view 与 outline 均委托它；不得调用有 ledger 副作用的
   `report_read`。普通 bound 文件仍委托原 TrackFsView，不往 calm-truth 塞全局策略。
   日期/patch 仍由 `workspace_reports.rs::changes/edits` 唯一负责，HTTP 与 MCP 共用。
2. CLI `commands.rs` 为 `ls → neige_ls`、`cat → neige_cat` 增加最小显式 command spelling
   映射，先识别一个 root verb，再用现有 positional/options 解析、registry call、guard、
   render；help/catalog 从同一表生成。shim 继续透传，不能另建路径/授权/参数解析栈。
3. 退役四个 workspace 工具、旧 `neige_area_ls`、`neige_track_ls/neige_track_cat` 的
   注册、schema、描述、CLI 入口与 allowlist 引用；list/describe/call 均无隐藏 alias。
   删除重复 SQL handler、角色 policy 和 capability 列表，managed reports-only policy
   改认新入口，仍限制写工具。Git `track diff/log/show` 不在退役范围，仍绑定自身 Track，
   不把日期 events 混入 Git history。
4. 迁移完整调用链：`tools/{track_file,workspace_reports,report_links,area_reports}.rs`、
   registry/wiring、`cli/{commands,help,catalog,render}` 及 listing renderer；
   `managed_track.rs`、`daily_planner.rs`、`routes/{daily_planner,today}.rs`、
   `harness/run_loop/issuance.rs` 及 workspace_report_tests；prompts/workspace-reports、
   workspace-operations、planner/assistant/worker、guides/report、builtin/daily-planner、
   旧工具描述、report_find/link_ls/mail_send/track_diff/log/show 引导；mail.rs/track_mail
   的发现提示与 tests/goldens/registry/role/CLI/result-shapes/identity/Report/ledger suites。
   HTTP/FE 是保留契约的消费者，核对 daily-planner domain/router 与生成 DTO，不修改。
5. 相对 `area/reports/` 作为既有内容地址保留并委托上述 owner，仍限 bound Area，
   用户 Area 的 Report 同样向三角色只读开放；system Area 仅可读自身 Report，不开放
   system siblings。自身普通文件视图不变；mentions、HTTP、backlinks、mail 仍使用的
   `area_reports/{name,glob,store}` codec/owner 不删除。历史正文里的旧命令引用、冻结迁移
   0141/0142 及退休扫描例外不是工具 alias，不批量改写；运行时说明必须迁移。

证据：[area owner](../../crates/calm-server/src/area_reports/mod.rs)、
[CLI parser](../../crates/calm-server/src/mcp_server/cli/commands.rs)、
[read owner](../../crates/calm-server/src/track_report_read.rs)。模块改名可选，
无需新增平行 owner；未来新增 Report variant 只改其 owning projection 与已有注册，
不新增永久文件名/测试名清单或 CI 轮次。

## 实施验证与评审计划

先在真实 CLI/MCP 生产入口加 red reproduction，再做最小 green 实现：三角色跨 Area
目录/full/index/blocks/sections/date/edits 同等成功；system foreign、伪 binding、错配路径、
非 Report foreign/traversal 拒绝；own system/relative/no-path 保持；foreign 不入 ledger、
写 fence 保持；旧工具无 alias；分页每行恰一次、空/超大行/foreign cursor；午夜边界、
late event 固水位、closed/revert、compact history；CLI 文本和 JSON 都断言可观察结果。
将用例注册到现有 suites，不把嵌套 case 文件误作独立 `--test`。

以下命令是后续实施计划，不是本次运行结果。共同前缀为
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server`，
每组附 `--no-tests=fail --test-threads 8`：

```text
--lib -E 'test(/workspace_reports::|daily_planner::tests::|harness::run_loop::workspace_report_tests::|mcp_server::cli::/)'
--test mcp_core_suite -E 'test(/^(mcp_assistant_tool_gate|mcp_tool_role_matrix|mcp_tools_list_role_filter|mcp_result_shapes|neige_cli_area_reports|neige_cli_commands|neige_cli_fence|neige_cli_tool_catalog|mcp_thread_identity|mcp_bootstrap_discovery|mcp_shim_round_trip)::/)'
--test mcp_integration_suite -E 'test(/^(mcp_track_file|mcp_area_reports|mcp_report_links|mcp_report_sections|mcp_report_anchor_knowledge|mcp_report_anchor_knowledge_persistence|mcp_report_anchor_knowledge_history|mcp_report_anchor_knowledge_conflicts|mcp_report_receipts)::/)'
--test planner_harness_suite -E 'test(/^track_mail::/)'
--test api_suite -E 'test(/^domain_api_suite::(today_launchpad|today_launchpad_reset|openapi|openapi_statuses)::/)'
```

先核对实际注册与枚举，再冻结实施 gate filter；任何不存在 selector 均须报告，不能
改 test layout 凑命令。上列是本设计证据时的旧模块名，迁移时以实际注册替换。
Registry golden 必须用真实 generator：

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 REGEN_MCP_TOOL_REGISTRY_GOLDEN=1 cargo test --locked -p calm-server --lib mcp_server::tools::tests::default_registry_matches_full_golden -- --exact
# 仅共享 prompt 改变时：
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 REGEN_PROMPT_GOLDENS=1 cargo test --locked -p calm-server --lib planner_card::tests::
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 REGEN_PLANNER_PROMPT_GOLDEN=1 cargo test --locked -p calm-server --lib plugin_host::manifest_tests::context_usage_shipped_dev_rendered_prompt_matches_full_golden -- --exact
```

这些 generator 写文件后故意失败；人工审 diff 后去掉 REGEN 变量重跑才是 green。
来源：[registry](../../crates/calm-server/src/mcp_server/tools/mod.rs)、
[prompt snapshots](../../crates/calm-server/src/planner_card.rs)、
[dev prompt](../../crates/calm-server/src/plugin_host/manifest_tests.rs)。本范围不运行
`npm run gen:api` 或 Report presentation generator，不手改生成物。

Mutation 在独占可恢复 worktree 执行，先写完整预期红集，再单因素改生产条件：日期
`workspace_reports.rs` changes 的 `e.at<?2` 改为 `e.at<=?2`，跑整个 lib
`test(/workspace_reports::/)`（附 `--no-fail-fast`）；预期唯一红为
`workspace_reports::tests::report_day_changes_include_reverts_closed_tracks_and_exact_date_edges`。
若缺红或多红必须解释、作废预测；恢复后 green 并查 residue。新路径实现后另对唯一
Report-only refusal 条件做 mutation，预测三角色矩阵完整红集，跑所在整个 suite/filter；
当前尚无该实现，不能虚构其测试名或 mutation 通过证据。

实施 worker 跑 focused 与格式修复；kernel gate 独占 quick Rust（`--quick --base origin/main`）、
text、full local-contract-gates（L2），并 replay 一个拒绝空选择的 focused filter；CI 跑 broad
suites。HTTP 集成 smoke 可用 `./e2e/run.sh --tier 1 --case 030-daily-planner`，它允许空 changes，
不能代替新 CLI/security 测试；共享主机禁 Tier 2/真实 Codex E2E。
Planner 安排两个独立隔离 review，逐项审 abstraction、duplicate logic、hardcoded assumptions、
external contracts、permanence；行为修复后双通道 fresh review，机械 delta 按仓库规则复核。
检查范围及成本复用[现有 contract gate](../../scripts/local-contract-gates.sh)，不新增测试名 pin。
本次仅文档链接检查，kernel gate 独占 `scripts/local-ratchet-gates.sh`；未运行 Rust、生成器或 mutation。
