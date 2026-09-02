## BLOCKER

1. §5.1 的 launchpad 解析链实际不可达。

   - 文档：「已加载的 waves 里找 `purpose === 'launchpad'`；缺失时 ensure，用返回的 `report_card_id`。」
   - 代码反证：
     - `TodayLaunchpad` 响应只有 `wave_id/spec_card_id/terminal_card_id/terminal_id`；`report_card_id` 只在内部 `EnsureTxResult`，不在响应里：[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:41)。
     - launchpad 位于 system cove，而 `GET /api/coves` 只返回 user cove：[coves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/coves.rs:85)。新 FE 又执行一次 `visibleCoves` 后才 fan-out waves：[queries.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/providers/queries.ts:347)。
     - 新 FE 的 `waveWireSchema` 和 `Wave` 域模型都丢弃 `purpose`：[wave.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/wave.ts:13)。
   - 建议：不要从 workspace waves 查隐藏 launchpad。页面直接调用幂等 `ensure`，用返回的 `wave_id` 请求现有 wave-detail 并从 cards 读 report；或新增只读 resolve GET，并把 `report_card_id` 真正加入 DTO。重写 INV-002，不要把 system wave 暴露进普通 workspace fan-out。

2. D1/D2 把现有 `wave-report` 当成累积日报，但它自带的维护契约明确禁止这种写法。

   - 文档：「单份累积文档，按天分段」「这张卡已经存在……任何新载体都要重做一遍。」
   - 代码反证：
     - 默认 report 明示“反映当下的状态，不是历史”“每次更新 REWRITE”“历史由 event timeline 承载”：[wave_report_contract_rules.md](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report_contract_rules.md:7)。
     - 它还禁止新增四个固定 H1 之外的章节：[wave_report_section_rules.md](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report_section_rules.md:1)。
     - ensure 正是用 `WaveReportPayload::initial()` 创建这份卡：[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:229)；现有 renderer 会把四个空 heading 实际画出来：[public.test.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/features/report/document/public.test.tsx:408)。
   - 建议：可以复用 `wave-report` 的存储和 renderer，但必须给 launchpad 一份专用 document contract，并定义不可删除、不可分日的 system/contract block；Today 只渲染 daily blocks。否则 agent 同时收到“累积历史”和“必须重写当前快照”两套冲突指令。

3. D4 没有完整复用 #951 的 fail-closed 裁决，仍有跨 cove 读取漏洞。

   - 文档：「判据是 `waves.purpose='launchpad'`」「`tools/list` 与 `tools/call` 两处都要闸。」
   - 代码反证：
     - 当前未解析 daemon 的 `tools/list` 会返回所有 role-visible descriptor 的并集：[transport.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/transport.rs:419)。若 descriptor 对 Spec/Assistant 可见，未归因连接也会看到它。
     - `tools/call` 按名字直接找到 registry handler，完全不参考 list：[transport.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/transport.rs:625)。
     - `ToolCallIdentity` 还携带 role；仅检查所属 wave 的 marker，会让 launchpad 上的 Spec、Worker 或未来其它卡都通过，而 D5 实际只需要 Assistant：[registry.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/registry.rs:100)。
     - [issue #951](https://github.com/keanji-x/neige-calm/issues/951) 的裁决还要求 descriptor `visible_to_roles: &[]`，仅在 resolved + active + marked 身份下做 contextual augmentation，并覆盖 unresolved、cross-session、dormant、missing row、DB error。
   - 建议：定义唯一的 `day_activity_allowed(identity)`：明确允许的 role（建议仅 Assistant）+ active session + `wave_id` 存在 + DB wave marker + cove/card 归属一致，所有缺失和 DB 错误均拒绝。descriptor 保持 `&[]`，只在已验证身份的 list 分支追加；handler 独立重查。同样限制 `since/until` 为至多一个日窗、`until <= now`，并设确定性排序、行数和字节上限。

4. D6 并没有形成硬增长边界，也不能保证 30 天保留。

   - 文档：「保留最近 30 天，更早的段由汇总时删除」「回答 §3.4。」
   - 代码反证：
     - 所谓“块级 256KB”只约束非 prose 的 canonical JSON：[kinds.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/report_blocks/kinds.rs:43)；prose 校验没有尺寸上限：[report_blocks/mod.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/report_blocks/mod.rs:193)。
     - 一天可以创建无限多个块；“30 天”因此既不限制块数，也不限制字节数。
     - 清理只在“有活动且用户手动汇总且 agent 成功遵循 prompt”时运行。连续无活动、agent 失败或 CAS 冲突都会留下第 31 天。
     - 默认 Wave VCS 只保留最近 50 个 commit，不是永久历史：[gc.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/wave_vcs/gc.rs:13)。
     - `wave.report_edited` 永久保存完整 `body_before/body_after`：[event.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/event.rs:450)，所以删除 report 内容还会把增长转移到永久事件表。
   - 建议：在服务端写边界或独立 GC 中实施保留；同时设置文档总字节数、块数和单 prose 块上限。30 天只作为产品展示窗口，不能替代资源上限。system contract block必须排除清理。

## MAJOR

5. §2 的“结构性事件已足够”被 D4 过度引申，当前 DTO 无法只靠 `wave.* / card.*` 生成。

   - 文档：「只依赖结构性事件（`wave.* / card.*`）」；返回值却含 `turns` 和 task completed/failed。
   - 代码反证：
     - turn 事实在 `harness.item.added`，它正是 30 天可 prune 的 kind：[event.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/event.rs:368)、[events_prune.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/events_prune.rs:110)。
     - task 结果是 `task.completed/task.failed`，不是 `wave.* / card.*`：[event.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/event.rs:608)。
     - 现有 conversation DTO 明确拒绝提供 turns，因为正确计数需要解析全部 `harness_items.params`：[model.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/model.rs:471)。
   - 建议：列出精确 event-kind allowlist。删除 `turns`，或给它一个不同且可实现的定义；`task.*` 可声明“当前不在 prune allowlist”，但不能称为 wave/card 事件。

6. D3 的备选裁决不完整：B 被错误否决，C 则实际上不可实现。

   - 文档：「块 id 编码日期会把一天一块写死」「从 `wave.report_edited` 派生块首次出现时间。」
   - 代码反证：
     - `wave.report_edited` 只有 flat `body_before/body_after`，没有 block id 或 block diff：[event.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/event.rs:450)。重复文本、拆块、重排后无法可靠恢复“某 block 首次出现”。
     - 日期不必占满 block id；多个块完全可以共享一个 typed `day_key`，或使用受校验的日期前缀加独立 suffix。
     - 仓库没有 workspace timezone；当前 Today 全部用浏览器本地 `Date` 分日：[public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/features/today/public.tsx:92)。
     - `created_at_ms` 实际是“创建日”；次日修改同一块仍留在原桶，不能笼统称为“写入日”。
   - 建议：C 应因缺少 block identity 被明确否决。优先使用 server-owned `day_key` 作为业务分段键，时间戳只作审计；定义唯一时区。若保留 timestamp 方案，也必须说明 browser-local 语义、旧块策略和 contract block 排除规则。

7. D2 对“每天一份 report”的否决理由并不成立。

   - 文档：「日历跨天预览时变成 N 次请求……而 D3 用一个字段就买到了。」
   - 代码反证：现有读取本来就是按 wave-detail 一次取一份 report：[queries.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/providers/queries.ts:370)；选中某天时懒加载只需要一次请求，日历索引可单独返回。反之，时间戳不是“一个字段”：它要进入 CRDT block entry、JSON projection、MCP read、REST、wire 和 FE domain；当前 block snapshot 只存 kind/rev/text：[wave_report_doc.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/wave_report_doc.rs:379)。
   - 建议：保留 D2 可以，但把理由改成“产品希望单一文档/统一引用”，不要用错误的 N 请求和“一字段”成本比较。

8. D5 的空活动判定与真实素材源不是同一个事实。

   - 文档：「FE 复用已加载的 waves/overlays 判活动窗口为空。」
   - 代码反证：loaded waves 只有当前 lifecycle/created/updated 等快照：[wave.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/wave.ts:13)；report edit、task completed、历史 lifecycle transition 都可能存在于 D4 事件窗，却不在 overlays，也未必更新 wave row。
   - 建议：抽出同一份 server-side activity projection，同时提供只读 REST preview 给 FE 和 MCP；否则只能证明“当前 waves 快照为空时没调用”，不能证明“活动窗口为空时没调用”。

9. INV-006 的 `doc_rev` CAS 不是幂等机制。

   - 文档：「重复触发幂等……第二次 upsert 覆盖」「`doc_rev` CAS 幂等」。
   - 代码反证：新 block 创建使用 `if_doc_rev`，并发第二次只会 rev conflict；server-generated block id 下没有“同一天同一段”的稳定键：[wave_report_blocks.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report_blocks.rs:179)。此外 conversation POST 必须带 `Idempotency-Key`，文档的 `{text}` 调用少了必需契约：[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:96)。
   - 建议：为 daily segment 增加服务端唯一 day key/upsert，或定义确定性 block id；明确 conversation key 是“同请求重试”还是“同日汇总”。CAS 只负责冲突检测。

10. §5.2 多条不变量按当前措辞证不死。

   - 文档：「反例必须红」以及“乱码 heading 变异测试证死”。
   - 代码反证：
     - INV-001“没有第二真源”是全仓未来路径的全称否定，单条集成路径不能证明。
     - INV-003“前端不存在任何分支”不是行为契约；乱码样例抓不到“仅对合法日期 heading 生效”的隐藏分支。
     - INV-005 的值级“无绝对路径”不可证明：合法 `title/cove_name` 本身可以是 `/home/x`；能证明的只是 schema 没有 path/body 字段。
     - INV-006、008 依赖 agent 遵循 prompt，不能写成内核不变量。
     - INV-007 当前证明的是错误的 FE 快照谓词，见上一条。
     - INV-002 在 system cove 被过滤的现状下没有 read-first 正路径。
   - 建议：001 改成“唯一写 API/唯一 storage key”；003 改成“固定 timestamps 时任意两个合法日期 heading 得到相同分桶”的 metamorphic/property test；005 改成字段 allowlist；006/008 下沉为服务端约束。

11. INV-004 正是“全称否定却只靠列举验证”的那条；INV-003/005 也有同类问题。

   - 文档：「对每一个非 launchpad 身份 × 两个入口的矩阵，不是列举几个身份。」
   - 代码反证：身份空间不只是四个 `CardRole`，还包括 unresolved daemon、card-bound/no-thread、cross-session、dormant、missing card/wave、purpose null/other、DB error 等 transport 分支；普通 Spec/Worker 枚举不能覆盖：[transport.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/transport.rs:419)。
   - 建议：把可允许条件收敛成纯 gate helper，测试“iff 明确 allow predicate”；再按 #951 对 transport 每个分支做集成测试。不要声称有限扫表证明了开放世界的全称否定。

12. 三个 PR 有未写明的顺序依赖，且 §182 自相矛盾。

   - 文档：「各自可独立验证」「如果 §9 开放问题翻案，翻的是 PR2/PR3，PR1 不受影响。」
   - 代码反证：
     - PR3 明确依赖 PR1 的 timestamp/day identity 和 PR2 的 `calm.day.activity`。
     - PR3 的清理 agent 必须从 `calm.report.read` 看见时间戳；当前 block index 只返回 `{id,kind,rev}`：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report.rs:181)，但 PR1 内容没列 MCP read 契约更新。
     - PR3 的空活动预检需要 PR2 的 projection，却没有 FE 可调用接口。
     - PR3 的幂等需要 day identity 在 PR1/D3 先锁定。
     - §9 Q1 正是 PR1 的 D3，Q3 正是 PR1 的 D7；它们翻案不可能“不影响 PR1”。
   - 建议：明确 DAG 为 PR1 → PR3、PR2 → PR3；PR1 必须含 launchpad 专用 contract、完整 CRDT/JSON/MCP timestamp 投影和 day identity。Q1/Q3 在 PR1 开工前关闭。

## MINOR

13. §2 其余三条事实成立，但位置或措辞需要校正。

   - 文档：「文档渲染器完整且已在生产路由使用。」
   - 代码反证：事实成立，但文档给的 `router/public.tsx:1534` 已过时；真实组合在 [public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/app/router/public.tsx:2352)。
   - 建议：去掉易腐行号，引用 symbol `WaveRoute → ReportDocument`。

   - 文档：「Assistant 已被允许写块。」
   - 代码反证：无；upsert/move/delete/write-markdown 均允许 Spec/Assistant：[wave_report_blocks.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/mcp_server/tools/wave_report_blocks.rs:96)。
   - 建议：保留，但改成“现有授权与写通道可复用”，不要引申成 D3/D6 无内核工作。

   - 文档：「人写块与 agent 写块是两条通道，互不冒充。」
   - 代码反证：无；REST 强制 User actor：[wave_report_blocks.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_report_blocks.rs:74)，MCP 通过 ToolCallIdentity/role attribution。
   - 建议：保留。

14. D1 的“新 card kind”否决、D2 的“只存今天会销毁历史”否决、D7 的状态优先级没有代码级反证。

   - 文档：对应 D1、D2-B、D7。
   - 代码反证：无；问题在现有 report contract 和分段/保留实现，不在这三个产品方向本身。
   - 建议：保留方向，修正上述载体契约和实现前提。

总判：**fix-then-ship**。
