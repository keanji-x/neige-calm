# #1245 设计：AI 对话改用 astryx Chat 家族

## 0. 目标与非目标

**目标**：把对话里今天由我们自己实现、而 astryx 已经有成品的部分换成库；视觉以现状为基准，靠 token 覆盖对齐，不靠改 DOM 结构去追。

**非目标**（本设计明确不做，理由在 §2）：不接管抽屉的滚动，不引入 `ChatLayout`／`ChatMessageList`，不动 `ExchangeRail`，不动 composer（它已经是库的）。

## 1. 采纳线：叶子组件采纳，布局／滚动壳不采纳

这是本设计最重要的一条判断，其余决策都从它推出。

astryx Chat 家族分两类：

- **叶子（表现型）**：`ChatMessage`、`ChatMessageBubble`、`ChatMessageMetadata`、`ChatSystemMessage`、`ChatToolCalls`、`Markdown`、`CodeBlock`。它们只管一行／一块内容长什么样，不持有滚动，不持有容器。
- **壳（布局＋滚动）**：`ChatLayout`、`ChatMessageList`、`ChatLayoutScrollButton`、`useChatStreamScroll`、`useChatNewMessages`。它们要当滚动容器、要把 composer 焊在底部、要自带弹簧滚动与「新消息」按钮。

**只采纳叶子。** 三条理由，都是从现有代码量出来的，不是偏好：

1. **滚动口只有一个，而且已经有主人。** 抽屉的滚动窗是 `ui/drawer` 交出的 `[data-nc-drawer-scroll]`，今天有三处机制读写它：follow-newest effect（`public.tsx:217-235`）、亮点规则（`:358-435`）、`jumpToExchange`（`:1548-1557`）。这三处的注释各自记录了它们踩过的坑（`scrollIntoView` 会连带滚动每一层祖先 scrollport，已修过两次：b1481da2、3f51ea50；按 `turns.length` 触发会把「读旧消息」的读者甩到底部）。`ChatLayout` 自带 `useChatStreamScroll` 的弹簧滚动＋上滚解锁＋scrollend 重锁，接进来就是**一个滚动口两个主人**——正是那些注释记录的失败类别。
2. **composer 不在转录区里。** `ChatLayout` 把 composer 焊在自己底部并盖一层毛玻璃；我们的 composer 是 drawer 的 footer 插槽，由 router 渲染，上面还叠着错误条（`ChatFooterNotice`）。要用 `ChatLayout` 就得把 router 的 footer 组装搬进转录组件，这是把两个已经分清的职责重新搅到一起。
3. **`ExchangeRail` 不在列里。** 库没有对应物，且它依赖 `ui/drawer` 交出的 seam（`.drawer` 是 `overflow: hidden`，rail 必须 portal 到卡片的**兄弟**节点才画得出来）。它与消息渲染正交，本设计不碰。

**这条线的可执行判据**：任何一个新引入的 astryx 组件，如果它读或写 `scrollTop`／安装 `scroll` 监听／声明自己是 scroll container，就不进来。

## 2. 逐项决策

| 今天 | 改成 | 理由 |
|---|---|---|
| agent 回复 `<p>{turn.text}` 纯文本 | **`Markdown`**（`isStreaming` + 内置 `CodeBlock`） | 今天 agent 的 markdown / 代码块**根本不渲染**，塌成一段纯文本。库自带 parser 与增量解析，零外部依赖。 |
| `ActivityLine`（手写 `<p>`，verb + target + Failed） | **暂不换 `ChatToolCalls`** | 见 §2.3：换了会净亏。等 wire 长出工具明细再换。 |
| `.said` / `.reply` 两个 `<p>` | **暂不换 `ChatMessage`** | 见 §2.4。 |
| 空态 `<div className={styles.empty}>` | 不动 | 库的 `emptyState` 属于 `ChatMessageList`／`ChatLayout`，按 §1 不进来。 |
| `ExchangeRail` / 三处滚动机制 / composer | 不动 | §1。 |

### 2.1 no-bubble 论证不推翻，用库照样表达

`public.tsx` 文件头论证：抽屉 364px，气泡自带 padding 后正文只剩 ~320px、约 45 字符/行，达不到 65–75 的阅读行宽 ⇒ **回复不进气泡、保留整列左边缘**；且不打 per-turn 的 YOU/AGENT 标签和时间戳。

库原生支持这个形态，不需要绕：
- `ChatMessageBubble` 的 doc 明写「不是所有消息都需要气泡处理」，未加气泡的消息用 `ChatMessage` 自己的 `name`/`metadata` 插槽；另有 `variant="ghost"`（无底色、保留 padding 以对齐）。
- 所以：**agent 行 = `ChatMessage sender="assistant"` 直接放内容，不套 bubble**；**you 行 = `ChatMessage sender="user"` + `ChatMessageBubble variant="filled"`**，右对齐由 `ChatMessage` 的 `rootUser` 给，底色由 token 桥给。
- 不传 `name`／`metadata` ⇒ 没有 per-turn 标签与时间戳，与今天一致。`opensAfterGap` 的分隔时间仍是我们自己的 `<p className={styles.gap}>`。

### 2.2 视觉一致性的机制：作用域内的 token 覆盖，不改 DOM

`styles/astryx-theme.css` 已经是成型的 token 桥（astryx 的 `--color-*`/`--radius-*`/`--font-family-*` → 我们的 `--paper`/`--hairline`/`--accent`/`--font-sans`），且入口层序 `vendor.css` → `astryx-theme.css` 已冻结在 `entry.css`。

一处必须处理的偏差：**回复是 serif**。`.reply` 今天取 `--font-serif` / `--text-md`（`thread.module.css` 开头有整段论证：写文档的和写回复的是同一个 agent，所以回复用报告的字族），而 astryx 的 `Markdown` 走 `--font-family-body`，桥把它映到了 `--font-sans`。

做法：**在 `.reply` 作用域内覆盖 astryx 变量**，而不是去改 `Markdown` 的 DOM 或加 `!important`：

```css
.reply {
  --font-family-body: var(--font-serif);   /* astryx 命名空间，仅此子树 */
  --text-body-size: var(--text-md);
}
```

这与 `astryx-theme.css` 顶部记录的原则同构：「行为和结构是 astryx 的，外观是我们的」。

### 2.3 `ChatToolCalls` 本轮不换 —— 量出来是净亏，不是偏好

原计划把 `ActivityLine` 换成 `ChatToolCalls`。读完实现后撤回，理由三条，都是从 `node_modules/@astryxdesign/core/src/Chat/ChatToolCalls.tsx` 读出来的事实：

1. **它的全部增量字段，我们的 wire 一个都填不了。** `ChatToolCallItem` 相对我们多出 `duration`、`resultDetail`、`node`、`additions`/`deletions`。domain 的 `ConversationActivity` 只有 `{ verb, target, state, atMs }`——单个时间戳，没有配对的结束时间，凑不出真实耗时；结果明细、沙箱名、增删行数在 `/api/cards/{id}/harness/items` 上都不存在。换过去只能映射 `name`/`target`/`status` 三项，等于用一个更重的组件渲染同样的三个字段。
2. **失败信号会从「一个词」退化成「一个图标」。** 今天失败行印出可见的 `Failed`（`.activityFailure`，CSS 注释称之为「这里唯一值得上颜色的状态」）。库版的 error 态是 `ChatToolCalls.tsx:375-390`：一个红色 `×` 图标，错误文本挂在 `title` 上，**没有可见文字、没有 `aria-label`**。这是可读性与无障碍的双向倒退。
3. **每行的状态 locator 会整个消失。** `ChatToolCalls` 只在**根节点**打 `themeProps('chat-tool-calls')`，行级不带任何 `data-*`（`Chat.doc.mjs` 的 theming target 列表里 `astryx-chat-tool-calls` 也确实没有 visualProps）。我们今天的 `data-nc-state="failed"` 是行级的，且被 `public.test.tsx:309` 断言。换过去后行级状态只剩图标颜色，没有任何可断言的钩子。

**换的条件**：等内核把工具调用的结束时间与结果明细放上 wire（`duration` + `resultDetail` 有真值可填）时再换，那时它的折叠与展开才有东西可展开。这条挂在 §3 的 S3。

### 2.4 `ChatMessage` 本轮不换

`ChatMessage` 给的是 sender 感知的对齐（我们的 CSS 已经有）和 `metadata` 插槽（后续 message actions 的挂点）。但 actions 本身是内核阻塞的（§3 S4），所以现在换只是把两个调好的 `<p>` 换成库的 div，再把 `thread.module.css` 里 1551 行调过的排版重新贴一遍——**当期收益为零，回归面是全部 3900 行浏览器测试**。等 S4 真要落 actions 时连着换，那一次换是有载荷的。

## 3. 切片

- **S1｜回复渲染 Markdown**（本 PR）—— 唯一当期就能付清的一条：库替掉的是真机器（parser + 高亮 + 流式增量），不是一个 `<p>`。
- **S2｜附件**：依赖内核。今天写口只有 `POST /api/cards/{id}/spec/input { text }`，`crates/` 里没有任何 chat 附件通路。落地形态已定：`ChatComposerDrawer` + `ChatComposerInput.onFiles`（粘贴／拖放）+ `Thumbnail`。
- **S3｜活动行换 `ChatToolCalls`**：依赖内核把工具调用的结束时间与结果明细放上 wire（§2.3）。
- **S4｜message actions／编辑已发送消息／AI 弹出选项**：依赖内核 + 产品决策（编辑后重跑还是分叉？transcript 今天只有 `text`，带不了结构化选项集）。库侧也**没有**现成的「编辑态」组件，只有可以填内容的 `metadata` 插槽；换 `ChatMessage`（§2.4）与这一条同时做。

**一句话总结采纳判据**：库能替掉*机器*的地方换（markdown 引擎、附件通路、可展开的工具明细），库只能替掉*一个带 class 的 `<p>`* 的地方不换。后者不是造轮子。

## 4. 回归证据面

基线是现有的 `thread.browser.test.tsx` + `thread.coarse.browser.test.tsx`（约 3900 行浏览器测试）+ `public.test.tsx`（943 行）：**迁移后它们必须仍然绿，每一条改动的断言都要能说清为什么语义变了**，不接受「改断言让它绿」。

S1 的可见行为变化只有一处，需明确接受：**live 脉冲点从「跟在最后一个词后面」变成「跟在最后一个块后面」**。markdown 渲染出的是块级元素，`<span>` 作为其兄弟必然另起一行；要保持行内就得让脉冲进到最后一个 `<p>` 里，而那是 markdown 的产物，我们没有接口。

新增覆盖：
- agent 回复里的 markdown（标题／列表／围栏代码）真的渲染成对应元素，而不是纯文本；
- 纯文本回复的换行仍然逐字保留（今天靠 `.reply` 的 `white-space: pre-wrap`，markdown 子树里必须由 markdown 自己的段落负责，两者不能同时生效）；
- 回复子树内 `--font-family-body` 解析到 serif（token 覆盖真的生效，而不是只写了 CSS）；
- `you` 行不走 markdown（用户输入的 `*` `#` 不该被解释）。

## 5. 待决

1. `ExchangeRail` 保留（本设计的假设）——如要删是另一个决定。
2. 「你说的话」保留 filled 气泡，还是也 ghost 化以最大化窄抽屉行宽？
3. 抽屉之外（wave 页／手机端，#1234）是否要 `ChatLayout` 全屏形态？若要，§1 的采纳线需要重新评估：那里滚动口没有既有主人。
