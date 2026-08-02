# `core/markdown`

## 用途

这里是全仓唯一的 Markdown 解析公开入口：把不可信字符串规范化为平台无关 AST，再提供安全 AST 策略与可配置 heading outline。这样 report 与 file-viewer 共用解析语义，又能保留各自的层级和锚点规则；`core` 不依赖 DOM、React 或浏览器能力。

## 契约

方言固定为 GFM（CommonMark 基础加表格、删除线等 GFM 节点），因为五条旧渲染管线都启用 GFM。raw HTML 不进入 AST 节点，只作为普通文本并记安全诊断；这是为了保留可见原文且不让 core 暗示其可直接插入 DOM。setext 与 ATX heading 会收集，fence 和缩进代码中的 heading 语法不会收集；heading 文本固定使用 `heading-label`：图片取 alt、inline code 取字面内容、行内强调/链接只取可见文字。heading 合法深度是 H1–H6，调用者用 H1–H6 的 `maxDepth` 收窄。

ID 由版本化 `HeadingIdPolicy` 生成，并明确接收 heading 节点、从 0 开始的全局/局部 ordinal 和调用上下文。版本 1 以 ordinal 而非源码 offset 或标题 slug 为稳定依据，因此同一 heading 顺序跨解析器版本保持 ID；结构顺序变化会有意改变后续 ID。重复标题不合并、不加 slug 后缀，各自占用独立 ordinal。report 使用 H1–H2 与 `<blockId>-h<n>`（每 block 局部从 0 开始），file-viewer 使用 H1–H4 与 `md-h-<n>`（全文件从 0 开始）。malformed Markdown 不抛错：尽量返回 `ready` AST 并附诊断；真正的内部规范化失败才通过与 `core/api` / `core/state` 一致的 `status: 'failed'` 数据通道返回。

`sanitizeAstPolicy` 只输出不含 raw-HTML 节点的平台无关安全中间 AST；它不是“可安全插入 DOM 的 HTML”承诺。这个边界使端侧 renderer 能独立决定元素映射和链接行为，而不会把平台策略倒灌进 core。

## 故意不做

这里故意没有 JSX、React renderer、元素/属性白名单、`dangerouslySetInnerHTML`、URL transform 或 `ReportLink`，因为它们依赖具体渲染平台。也不做 report 的 block-to-heading 归组、`number: null` fallback 顶层项、连续编号或 `children` 组装，因为它们是 `features/report` 的业务组合，不是 AST heading policy。H3–H6 故意不进入 report outline，但仍保留在 AST 并可供 file-viewer 使用；这是两端参数差异，不是第二套解析器。
