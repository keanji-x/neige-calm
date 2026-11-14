# 统一 Report 浏览：绑定 Track 的名称路径

本设计是本轮唯一方案，对应 Report `b_e68f` 的 15 项约定及
[#3668](https://github.com/keanji-x/neige-calm/issues/3668)。固定绑定 Track 为 cwd，
以名称目录浏览用户 Area；复用已经合入的 Report 读取和日期投影。
实施为 **L2**：改变跨 Track 的定位与隔离边界，须由 Planner 安排两个独立隔离评审。
名称浏览核心与 mentions/FE/运行时消费者均遵循以下最终契约。
验证证据与剩余发布条件见[验证说明](../verification/3668-named-report-paths.md)。

## 唯一路径契约

| # | 约定 | 可观察行为 |
| --- | --- | --- |
| 1 | 固定 cwd | 每次从 live binding 的真实 Track/Area ID 起步，不由标题拼回身份；无 cd、可变 cwd 或路径重绑定。 |
| 2 | 当前 Report | `neige cat`、`neige cat report.md`、`neige cat --sections 概要` 指向同一自身 Report；仅省略 path 默认 `report.md`。显式空字符串报路径错误，MCP null/非字符串仍错误。 |
| 3 | 当前 Area | `ls ..` 直接列所属 Area 的 Track；`cat ../登录重构/report.md` 访问兄弟，无 tracks 中间层。 |
| 4 | 全局根 | `ls /` 列用户 Area 与保留目录 `.neige/`；`ls /研发/` 直接列 Track；`cat /研发/登录重构/report.md` 读 Report。 |
| 5 | 相对名称 | `ls 登录重构` 只查自身子目录；找不到就报错，不尝试 current-area/global fallback。 |
| 6 | dot/parent | `ls` 与 `ls .` 列自身 view；`./report.md` 等同 `report.md`，`../..` 到全局根，再上行拒绝。只逐段走虚拟树，不用宿主 realpath。 |
| 7 | 名称编码 | Area/Track 共用严格 canonical 段 codec；编码分隔符、百分号、消歧符、控制字符及开头点号，保留可读 Unicode。`.neige` 名称输出 `%2Eneige`，不占系统目录。含空格路径需 shell 引号，列表输出可复制路径。 |
| 8 | 同级消歧 | 唯一非空名直接寻址；同名每项及空名均加 `~` 与最短唯一 ID 前缀，至少 8 字符；完整同级集参与计算。无后缀且歧义返回候选，不选第一项。 |
| 9 | 身份与改名 | 名称是当前视图，改名后重新 ls，无永久名称 alias。行内保留 area_id/track_id；持久引用继续用既有 ID URI 与 block 锚点。 |
| 10 | 自身/foreign | 目标 ID 等于绑定 Track 时，所有可达路径均进入同一自身 view；foreign user Track 仅 Report 与 report-edits。三角色相同，绝对路径和 ../ 不绕 fence。 |
| 11 | system | 全局/父级目录不暴露 system Area 或其兄弟 Track；自身 system Track 的 report/cards/runs/guide/ops 沿既有角色规则可读。不能利用 .. 或名称列 system siblings；可上行到根浏览 user Areas。 |
| 12 | 日期目录 | 全局聚合只在 `/.neige/report-changes/YYYY-MM-DD.json`；Track edits 在该 Track 的 `report-edits/YYYY-MM-DD.json`。 |
| 13 | 分页与日期 | 目录按稳定 ID 排序，复用 Page 50 行/32 KiB；cursor 绑定目录身份。outline 最多 40 blocks，完整 index 不截断。日期 timezone、水位、20 项分页及 HTTP DTO 不变。 |
| 14 | 退役与历史 | 不再识别旧 `/areas/<id>/tracks/...`、根 `/report-changes/...`、浏览 `area/reports/` grammar，不留隐藏 alias。历史引用的最小保留边界见下节，新 producer 只输出名称路径或稳定 ID 引用。 |
| 15 | 实施边界 | server 唯一目录 resolver/codec；CLI/MCP 只传参和投影，复用 snapshot/Selection/date/Page。预期无数据库迁移、额外状态服务或持久 alias。 |

路径按原始 `/` 切段；先识别字面的 `.`/`..` 导航，再对名称段严格单次解码，
不把解码出的 `/`、`.`、`..` 再当导航。不得先整体 normalize 或折叠 `file/..`：
`report.md/..`、`report.md/子项` 都因文件不能作父目录而拒绝；未知目录不能被后续 .. 抹掉。
目录末尾 `/` 可用，文件末尾 `/` 拒绝；空中间段拒绝，不对不规范路径做猜测。
保留编码前后空格等名称数据，不能沿用对全路径 `.trim()` 而损坏可复制名称。
根没有旧的 ID 通配语法：名为 `areas` 的真实 Area 按普通名称寻址，不为它硬编码保留词。

system 起点以绑定 ID 表示，因此无需在全局树泄露名称即可表达自身 view。
`..` 定位父级时仍做 user-Area gate，不能列 system 父级；`../..` 可继续到根。
解析器区分身份导航和目录可见性，任何 foreign system 内容都拒绝。
用户目录继续使用 `track_access::user_visible_track` 隐藏退休 Area-conversation 容器，
closed Track 保留；不得借目录隐藏规则丢弃日期历史事件。既有直接 Report 读取与目录可见性
并非同一权限；按 ID 定位的引用继续走原授权边界，不开放隐藏目录的新名称发现入口。

## owner 与真实调用链

生产入口为 CLI shim → server CLI commands → registry/transport guard →
`tools/track_file::{track_ls,track_cat}` → `managed_track::report_browse_context` →
`report_paths::resolve`。`report_paths` 唯一拥有名称段 codec、完整同级消歧和基于 live
binding ID 的目录导航；`tools/unified_browse` 负责 envelope/目录投影，内容仍交原 owner。

Area 集来自 `areas_list_user_visible`，Track 集来自 `tracks_by_area`。这些完整身份集
先命名，再应用目录展示、Report 存在性、tag/glob、候选排名与分页过滤。
`area_reports::outlines` 使用同一名称 owner，保留单次 SQL 的 payload/tag 投影和 snapshot
相同的 block ID 规则；`mentions::rank` 保留排名和每组 8 项上限。
system Area 无合法名称发现路径，故不提供 Report、block 或其 tag 候选。
`area_reports/name.rs` 薄包装和旧地址 producer 已删除；find 使用既有 canonical 实现。

| owner/调用者（路径均相对 `crates/calm-server/src/`，另注明者除外） | 实施动作/保留契约 |
| --- | --- |
| `mcp_server/tools/{track_file,unified_browse}.rs` | 统一 typed resolver；自身目标统一返回绑定 view（含 guide/ops/gate log），foreign 只 Report/edits；cat 的缺省与显式空值分开。 |
| `managed_track.rs`、`mcp_server/{registry,transport,wiring}` | 保留 session/token/thread/card/role/track/area live binding 与 reports-only guard；不由名称推断身份，不引入 membership。 |
| `track_report_read.rs`、`tools/track_report.rs`、`track_file::Selection`、`report_sections.rs` | 保留 `load_report_for_track`、`load_report_doc_snapshot`、`parse_select_arg`、`selected_blocks_text`；full/选择/index/outline 委托既有投影，浏览不调用写 ledger 的 report_read。 |
| `area_reports/{mod,glob,store}.rs`、`tools/area_reports.rs` | 过滤/tag/SQL 与 outline 投影保留在原 owner；find 定位 Area 后调用 report_paths 命名，不保留旧 grammar。 |
| `mcp_server/cli/{commands,help,catalog,render}.rs`、`cli/render/listing.rs`、`crates/neige-cli` shim | cat positional 可省，MCP schema 同步；CLI 删除旧 Area path 分类和专用浏览 render 分支，按 server 结构渲染 ID/标题/path/cursor。shim 继续透传，无独立授权/codec。 |
| `mentions/{mod,rank}.rs`、`crates/calm-types/src/mentions.rs` | 新 report/block insert 用名称路径及 block 锚点，不再产生旧地址；不变更候选 DTO 字段、排名/组上限。完整同级命名先于候选筛选，system 不产生 foreign 名称引用。 |
| `fe/core/domain/mentions.ts` → `fe/web/src/app/providers/mentions.ts` → `app/router/public.tsx`、`features/chat/thread/{mention-trigger,public}.tsx` | provider 取服务端 insert，composer 发送原始文本；sentMentionParts 保留旧文本 pill，增加新 insert 的显示解释，不承担授权/目录解析；实施前读各层 AGENTS。 |
| `report_backlinks.rs`、`tools/report_links.rs`、`crates/calm-types/src/report_links.rs`、Report fork/task refs | ID URI 和 block 锚点保持；backlinks 扫描/同 Area 权限及 refs 校验不改成名称推断。mail.rs、tools/mail 与 track_mail 引导迁移新发现路径，mail 同 Area fence 不扩权。 |
| `workspace_reports.rs`、`daily_planner.rs`、`routes/{daily_planner,today}.rs`、`harness/run_loop/issuance.rs`、workspace_report_tests | 仅迁移工具/路径消费者与引导，查询/时区/creation metadata owner 不变，不在 harness 复制目录策略。 |
| `prompts/{workspace-reports,workspace-operations,planner,assistant}.md`、`prompts/worker/*`、`prompts/guides/{outputs,report}.md`、`prompts/tools/*`、`prompts/result-receipt/*`、`templates/builtin/*`、`harness/{backend_metadata,result_receipt}.rs` | 扫描旧路径和新输出：ls/cat/find/link_ls/mail/op/history 指引同步；不得以冻结历史扫描例外掩盖运行时旧输出。 |
| `docs/{using-neige-calm,conventions/agent-commands}.md`、operations 指引、tests/goldens、CLI/registry/role/identity suites、neige-cli forwarder、e2e 指引 | 实施时迁移可运行示例并用真实 generator 更新发生变化的 goldens；已退役七工具仍保持退役，不复活。 |
| `routes/tracks.rs` → `calm-truth::track_fs_view`、HTTP daily views → FE daily-planner domain/router/generated wire | HTTP 保留；不能把新全局浏览 resolver 接入这些原有 route 来“顺便”扩权。 |

`report find` 保持 Planner-only 的搜索职责，不成为另一套浏览器：path 指向 resolver
解析出的当前 user Area（典型 `neige report find .. --tag 标签`），拒绝 foreign Area/system。
返回原 `{reports:[...]}` 字段与更新时间排序/AND 条件/超过 500 拒绝契约，path 输出新 canonical
Report 路径；`--name` glob 改匹配 Track 的 canonical 名称段，不含历史 `.md` 文件扩展。
这是显式输入语义迁移，必须改帮助/mentions tag 指引及测试，不接受旧 `area/reports/` path。
目录 ls 的 ID 排序/Page 与搜索的排序/上限是不同职责，不能合并成默默截断的实现。

## 历史引用与持久化边界

新引用链路为 `area_reports::outlines` → `mentions::rank` → HTTP candidate insert →
FE provider/composer 原样发送。`sentMentionParts` 仅解释 canonical Report 路径的显示外观，
不加载内容、不做身份解析、canonical 校验或授权，也不生成名称/消歧策略。

历史发送文本/Report 正文保持原字节，FE 已发送 mention 继续显示旧格式 pill；既有
ID URI、backlinks、task refs、Report fork 的 ID 重写保留。名称不是持久身份：改名后
重新发现，不建 alias。历史旧浏览命令不再执行；没有独立历史内容读取消费者，因此不保留
死兼容 wrapper，不新增历史读取工具/HTTP 服务，不批量改写历史正文/事件/冻结迁移。
新运行时说明和 mention insert 使用名称路径，需持久定位的 producer 使用稳定 ID URI。

浏览（自身、foreign、index、history）不写 `report_knowledge`。
`tools/track_report::report_read` 的 ledger-anchor 写前读取、`track_report/identity.rs` 的
写身份与 Report commit/write 协议保持；Worker 不得写，Planner/Assistant 不因能解析
foreign 名称而获得 foreign 写权限。`track diff/log/show` 仍是绑定 Track 的 Git 历史，
不接全局解析，也不把日期 events 当 Git revision。

HTTP 保留 route/query/DTO/Principal 与原写边界。证据：`routes/tracks.rs::{list_track_files,
cat_track_file}` 直接走 TrackFsView，cat 仍要求 path；这两个函数目前明确无 Principal
extractor 并有 multi-user ownership TODO，不能把它们作为新 MCP 的授权依据或在本轮修复。
`list_tracks_by_area` 的 Principal member 查询是另一条链；agent 的 ToolCallIdentity
并非人类 Principal。不把 HTTP 现状复制为 server 名称解析的授权模型。

## 投影、分页与日期保留项

全局/Area/foreign Track 目录返回 `{entries,next_cursor}`，行有 `id,title,path,kind` 及
相应 `area_id/track_id`；Track 行保持 `report_path,closed_at,creator_track_id,doc_rev,
updated_at,active_tasks_count,blocks,blocks_truncated`。不得返回完整内部 model/default_cwd。
自身目录保留既有 view 字段并补 canonical path，不因此裁掉 cards/runs/guide/ops 等入口。
目录稳定 ID 升序；固定系统节点使用 owner 声明的协议 key，不用名称排序。
沿用 `tools/paging::Page` 50 行/32 KiB，超大首行独占页、字节预算提前截页，绝不漏行。

cursor opaque 且校验目录种类、稳定目录身份和末行身份（不能只用相同名称或末行 ID）；
不跨目录复用，不取决于输入的相对/绝对拼写。改名/删除导致路径或 cursor 无效时明确要求
从头；若仍可按身份安全续页，无需仅为改名强制失效。无永久名称缓存或签名状态服务。
目录不是数据库快照，插入更小 ID 后重新开始才能看到；日期 cursor 不改成这个格式。

cat 仍 `{content,content_type}`：full/blocks/sections 为 Markdown（`text/markdown`），
index 的 content 为 JSON 文本（`application/json`），含 `track_id,area_id,title,summary,doc_rev,blocks:[{id,kind,rev,heading}]`。
一次自洽 Report snapshot 投影正文/选择/index；index 按文档顺序全量，无分页/40 项截断。
outline 只预览最多 40 blocks，不可冒充完整 index。CLI 普通及 JSON cat 都消费 content，
不增加 renderer 不读取的第二层 index。index 与 blocks/sections 互斥，选择错误沿用 owner。
非 Report 拒绝 selectors；step 仅自身 gate log；cursor/through_event_id 仅日期 JSON，
日期拒绝 selectors/step。日期目录返回空 entries、null cursor 和 `YYYY-MM-DD.json` 元数据，
不枚举无限日期、不给可 cat 的占位模板。

唯一日期 owner 为 `workspace_reports::{changes,edits}`，HTTP 与 MCP 共用：严格日期，
默认 Asia/Shanghai，managed metadata 时区不丢；半开日窗 `[start,end)`；changes 首次省水位
时在原 writer transaction 取 MAX(id)，续页必须原 through_event_id；edits 每次必需非负水位。
即使绑定到自身 system Track，edits 查询仍按既有 `a.kind='user'` 返回范围，不借“自身 view”
放开 system 历史查询。固定 date/timezone/scope/through，closed/revert、compact decode/scope
检查不变；每页 20，changes Track ID keyset，edits 十进制 event ID 升序 cursor。

MCP 日期 JSON 放 content：changes 保留 `date,timezone,through_event_id,changes,next_cursor`，
行计数、首末 IDs、summary before/after、500 行 patch 和 patch_truncated；edits 保留
`edits,next_cursor,event_id,edited_at`（RFC3339）和完整 edit（edit_id/body/summary 前后）。
HTTP 的 `at` 毫秒 DTO 不改。改名仅影响浏览 path，日期结果不额外嵌入名称路径。

## 验证与维护边界

HTTP mentions 回归覆盖真实路由、完整同级命名、system 隐藏、特殊名称和 resolver/Report
snapshot roundtrip。FE 回归覆盖 provider 透传、composer 原文发送、新旧已发送 pill 的回显。
关键命名断言按根 mutation 规范做单因素生产变异，保留完整预测/实际红集和恢复 green 证据。
核心 foreign/file-parent/date predicate 和观察者不变的部分复用已接受 mutation 证据。

worker 运行针对性 Rust/FE consumer tests、mutation 和格式修复；kernel gate 持有最终
text、Rust quick、full contracts、Rust focused replay、FE lint/build/full tests。
仅发生生成物变化时运行真实 registry/prompt/API generators，不手改 golden/wire。
浏览器、真实 preview 和 Tier 1 Today 由 Planner 后续 terminal 验证；共享主机禁真实 Codex E2E。
上游同步、两个独立 L2 review、CI 与合并批准仍是发布前置条件。

两独立评审检查领域/guard 边界、重复逻辑、应用硬编码、外部事实来源及永久维护成本。
普通新增/改名 Area/Track 不需改源码清单；本轮不增加永久测试名 pin、扫描例外或 CI 轮次。
现有 suite 注册自动承载新测试；临时命令、预测和原始日志留 `/tmp`，最终证据收拢到验证说明。
