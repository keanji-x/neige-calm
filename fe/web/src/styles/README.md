# styles/tokens

## 用途

`tokens.css` 冻结跨模块使用的颜色、排版、间距、动效和堆叠层级词汇，`public.ts` 给 TypeScript 消费方提供封闭的 token 名类型，`font-stack.ts` 给无法读取 CSS 自定义属性的终端代码提供等宽字体栈。它们只定义接口，不包含业务选择器或组件样式，因为阶段 1 先稳定跨模块边界；`tokens.css` 的入口接线属于 P8。

## 契约

token 清单由契约测试独立手写，新增、删除或改名必须显式修改测试，避免自动发现让接口变化失去 review 信号。主题色在 light/dark 两侧对称，具体 surface 与 overlay 保持 oklch 字面量，语义 alias 只在根主题裸引用底层 token；形状标尺不随主题变化，各子族分别限制 px、无单位数、em、秒或整数。`--tracking-normal` 固定为数值 `0`，让后续 lint 能禁止裸 `normal`。六级 z-index 严格递增，因为层级顺序而非具体数字才是语义。`--font-mono` 与 `MONO_STACK` 逐字节固定为同一已部署字面量，因为终端初始化时不能依赖 DOM cascade。

另有跨语言不变量：未来 `themeRgb.ts` 的终端前景/背景值必须同时被 `XtermView` 消费，并与 Rust `RequestTheme::default_dark()` 同步；本序列只记录该边界，因为另外两端分别由后续 terminal slice 与 Rust owner 的跨端测试锁定。

## 故意不做

GATE-TOKENS-009 的故意缺口仅是 `--r` 不进入 radius 形状数组；本阶段不引入该兼容 alias，六个 radius token 本身仍由 CSS 形状与公开类型契约冻结。GATE-TOKENS-018 的故意例外是 `--overlay-scrim` 保持 rgba，而非放弃冻结；测试同时锁定 light/dark 两侧的 rgba 形状，并继续把它排除在通用 oklch 循环之外。也不迁移 GATE-TOKENS-030..036 的恒真 `console.warn` 漂移扫描器，因为它们不能形成失败闸门，公开入口明确不暴露 scanner 类型。

不声明全局 layer 顺序、全局类 manifest、unlayered 例外 manifest 或 `data-*` 约定，因为它们属于阶段 1 P8；也不增加组件级 `.module.css`、业务样式、组件 CSS、Astryx 接入或 token 消费迁移，因为这些都超出接口冻结范围。
