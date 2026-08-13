# 1059 实现评审（commit 66748f69）

范围：`git diff origin/main..HEAD`，对照 `docs/_conversation-api-design.md`、后端
`cards.rs`/SQLite read 实现，以及旧前端 `useSpecChatHistory.ts`、
`useSpecCurrentRun.ts`、`SpecConversation.tsx`。结论：当前实现尚不能替换 stub；核心实时链路
和历史分页均存在功能性问题。

## Blocking

1. **页面打开后的新消息和运行 phase 永远不会更新。**
   `useConversationStore` 只在挂载时执行 `useInfiniteQuery`/`useQuery`
   (`fe/web/src/app/router/public.tsx:88-92`)，没有订阅
   `harness.item.added`、`harness.phase.changed`、`harness.transcript.cleared`；现有全局事件策略又明确把
   三者设为 no-op（`fe/core/events/invalidation-plan.ts:99-101`）。发送 mutation 也不 invalidate
   history/run（`queries.ts:95`）。因此 POST 成功后只剩本地 echo，持久化的 user item、agent 回复、
   working/stopping 状态都不会出现，除非发生无关的重挂载/手工 refetch。旧 web 会用事件追尾 asc
   拉取、更新 phase、在 transcript clear 后重拉。此问题直接违背“从 STUB_REPLY 换成真实后端”的目标。

2. **desc 历史分页使用了错误游标，长历史会大量重复并产生重复 React key。**
   后端 desc SQL 是严格 `id < cursor`，取完后会 `rows.reverse()`，所以响应仍按 id 升序
   （`crates/calm-truth/src/db/sqlite/read.rs:613-643`）。下一页游标应取 `page[0].id`，与旧 web 的
   `oldestRawId = rows[0].id` 一致；实现却取页尾 id（`queries.ts:76-77`）。例如首页 701..1000，
   下一页会取 700..999，而不是 401..700；每次只前进一行、重叠 299 行。hook 又自动连续翻到末尾
   （`public.tsx:100-103`），会把约 N/300 个请求放大到约 N-299 个请求，并把同一 item 多次铺进
   `serverTurns`（没有去重），最终 `ChatThread` 使用重复 `turn.id` 作为 key。严格 `<` 本身没有
   off-by-one；错误在选择了最新而非最老游标。

## Major

1. **历史分页行为悄悄从“按需 Load earlier”变成“打开即拉完整历史”。**
   旧 web 首页 300 条，用户触发 `loadEarlier()` 才向上翻；fe 的 effect 会在 `hasNextPage` 时自动
   `fetchNextPage()`。即使修正游标，也会无条件下载、解析全部 transcript，失去旧实现的加载边界和
   可重试 UI。`300` 来自旧 web 的 `PAGE_LIMIT = 300` 及设计文档建议，不是后端默认值；后端默认
   是 100、上限 500（`cards.rs:161-165,204-206`）。由于 operation 显式发送 `limit=300`，当前
   page-length 判定与实际请求一致，但数字在 operation 与 query 两处裸写，任一处调整都会破坏翻页。
   “恰好 300 条”会多发一次空页请求，这是常见的无 total/count 游标边界，不会漏/重；本身仅是
   一次额外请求。

2. **乐观回显没有可靠对账键，并发相同文本会错误合并。**
   `/spec/input` 响应只有 card/runtime id，当前只能按 trim 后的文本相等或前缀匹配
   （`public.tsx:104-109`）。该匹配不是一对一：两个并发的相同 echo 遇到一个服务器 user row 时会
   一次删除两个，短暂只显示一条；第二个服务器 row 到来时也无法恢复正确映射。不同消息中若一个
   是另一个的首行/前缀，也可能误删。旧 web 至少用 `matchedUserIndexes` 保证一条 server row 只消费
   一个 echo（并限制已有行 lookback）。fe composer 没有 submit-pending gate，允许并发发送；后端
   如何排队不等于 UI 能正确对账。

3. **发送/Stop/reset 的失败与 pending 行为相较旧 web 退化。**
   发送会先插 echo 并立即清空草稿；失败只删 echo，错误完全不展示。Stop 永远显示、未按
   working/stopping 控制、没有 pending 防重，错误也被丢弃；`stopped: true` 后缺少旧 web 的本地
   “Turn stopped”系统行。reset 在请求开始前关闭确认框，失败无提示且无法就地重试；旧 web 保持
   dialog、显示 reset error，并维护各 mutation pending/dormant typed error。三处裸 `void`/then-only
   调用（`public.tsx:122-138,277-296`）还可能产生未处理 rejection（interrupt；reset 失败亦无 catch）。

4. **Esc 行为不是中断，而是关闭 drawer。**
   新代码没有会话级 Esc 中断监听；通用 Drawer 在任意 Escape 上直接 `onClose`
   （`fe/web/src/ui/drawer/public.tsx:82-87`）。旧 web 仅在 drawer 内、working、非 reset/IME 时消费
   Esc 并调用 stop（`web/src/pages/SpecConversation.tsx:458-480`）。这不是等价替换，也使正在运行的
   turn 在 UI 消失后继续执行。

## Minor

1. **两类消息降级是显式的，但丢弃方式没有兜底。**
   domain 注释明确承认内核种类更宽，代码也显式只接受 completed 的 `agent_message`/
   `user_message`（`conversation.ts:168-170`），测试明确锁定 `command_execution` 和坏 JSON 返回 null。
   但 tool/run/edit/reasoning/compact/unknown、started、坏 JSON、形状变化、空文本都会静默消失；没有
   unknown 占位、日志或计数。旧 web 会解析 7+ 类并为未知/坏 JSON 生成 `unknown` 行。因此“已知降级”
   在源码层显式，在用户可见/可观测层仍是静默丢弃。

2. **`features/chat/**` 确实 0 行改动。**
   diff 只包含 `core/domain/conversation*`、`app/providers/queries.ts`、
   `app/router/public.tsx`。让 feature 继续只接收 turns/callback props、把 API/Query 组装留在 app 层，
   与 `fe/AGENTS.md` 的依赖方向一致，理由成立。

3. **未发现 Rust 或明显超范围产品改动。**
   提交共 4 个 fe 文件，均服务于接口契约、查询组装或 drawer 接线；0 行 Rust。`public.tsx` 保留了两段
   已过时的“endpoint 不存在 / route-local stub”注释（约 52-74、204-221），应随真实接线清理，避免
   后续维护者继续依据错误事实。设计文档本身不在 commit 中（当前为未跟踪文件），不计入被评审改动。

4. **测试覆盖不足以发现集成退化。**
   新增测试只覆盖两类 item 的纯解析；没有覆盖后端实际 desc 返回顺序下的游标、分页去重、事件追尾/
   clear/phase、相同文本并发 echo、mutation 失败、Stop/reset pending 与 Esc。上述 Blocking/Major 问题
   因而均未被锁定。

## 修复报告（2026-08-12）

- **B2**：desc 页的下一游标改取 `page[0].id`。新增 contract test 模拟首页按
  `701..1000` 升序返回，并锁定第二页请求包含 `after_id=701`。
- **B1-a**：send 成功后与 interrupt/reset 一样失效 history + run；只刷新已持久化的用户消息。
  agent 异步回复仍明确留给 #1057 事件流，本分支未加轮询，也未改两个禁止触碰的 invalidation 文件。
- **M1**：删除自动 `fetchNextPage` effect，drawer 内仅在 `hasNextPage` 时显示用户触发的
  `Load earlier`，并提供 loading/失败状态。页大小统一为 `HARNESS_ITEMS_PAGE_LIMIT = 300`，
  operation 与翻页判定共用该常量。
- **M2**：新增有 50 条 user row lookback 的一对一 echo 对账；一个 server row 只能消费一个
  echo。测试覆盖两个相同 echo 仅到达一个 server row，以及超出 lookback 不误配。
  composer 在 send pending 时禁用，另用同步 ref gate 阻止同一帧重复提交。
- **M3**：send/stop/reset 均补齐 catch/finally 与可见错误。Stop 仅在 working/stopping 显示，
  stopping/pending 时禁用防重，成功中断显示 `Turn stopped`。reset pending 使用 busy 状态，
  失败保留确认框并在框内展示错误，可直接重试；所有 fire-and-forget promise 都有 rejection 处理。
- **M4**：未改通用 Drawer。在会话组合层用 capture listener 拦截 Esc：仅 drawer 内、working、
  非 stopping、非 reset dialog、非 IME 时消费并 interrupt；其他 Esc 保持 Drawer 的关闭语义。
- **Minor 1**：未扩展 tool/run/edit/reasoning/unknown 渲染；这是原设计明确的已知降级，不属于本轮
  必修，也未搬运旧前端 349 行 params 解析。
- **Minor 2/3**：保持 `features/chat/**` 与 Rust 零改动；已删除 router 内两段过期 stub 注释。
- **事件实时链路**：未修，按任务边界继续由 #1057 PR-B 负责。
- **Ownership**：所有 FE 改动均位于 inventory 已登记且非 readonly 的既有模块，没有新模块，
  因此提交不需要 `OWNERSHIP-CHANGE` 尾注。

### 门禁

- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：通过；184 modules / 513 dependencies / 0 violations。
- `npm run build`：通过；441 modules transformed；JS 542.00 kB（gzip 162.84 kB），
  CSS 173.99 kB（gzip 30.26 kB）。仅有既存的 chunk > 500 kB 提示。
- `npm test`：通过；86 test files passed、1 skipped；1027 tests passed、1 skipped；
  `test:wire` 与 `test:mock-drift` 均通过。
