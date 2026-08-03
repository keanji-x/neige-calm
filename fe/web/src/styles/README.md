# styles

## 用途

`tokens.css` 冻结跨模块使用的颜色、排版、间距、动效和堆叠层级词汇，`public.ts` 给 TypeScript 消费方提供封闭的 token 名类型，`font-stack.ts` 给无法读取 CSS 自定义属性的终端代码提供等宽字体栈。它们只定义接口，不包含业务选择器或组件样式，因为阶段 1 先稳定跨模块边界；`tokens.css` 的入口接线属于 P8。

## 契约

token 清单由契约测试独立手写，新增、删除或改名必须显式修改测试，避免自动发现让接口变化失去 review 信号。主题色在 light/dark 两侧对称，具体 surface 与 overlay 保持 oklch 字面量，语义 alias 只在根主题裸引用底层 token；形状标尺不随主题变化，各子族分别限制 px、无单位数、em、秒或整数。`--tracking-normal` 固定为数值 `0`，让后续 lint 能禁止裸 `normal`。六级 z-index 严格递增，因为层级顺序而非具体数字才是语义。`--font-mono` 与 `MONO_STACK` 逐字节固定为同一已部署字面量，因为终端初始化时不能依赖 DOM cascade。

另有跨语言不变量：未来 `themeRgb.ts` 的终端前景/背景值必须同时被 `XtermView` 消费，并与 Rust `RequestTheme::default_dark()` 同步；本序列只记录该边界，因为另外两端分别由后续 terminal slice 与 Rust owner 的跨端测试锁定。

## 故意不做

GATE-TOKENS-009 的故意缺口仅是 `--r` 不进入 radius 形状数组；本阶段不引入该兼容 alias，六个 radius token 本身仍由 CSS 形状与公开类型契约冻结。GATE-TOKENS-018 的故意例外是 `--overlay-scrim` 保持 rgba，而非放弃冻结；测试同时锁定 light/dark 两侧的 rgba 形状，并继续把它排除在通用 oklch 循环之外。也不迁移 GATE-TOKENS-030..036 的恒真 `console.warn` 漂移扫描器，因为它们不能形成失败闸门，公开入口明确不暴露 scanner 类型。

不增加组件级业务样式、Astryx 消费迁移或全局视觉规则；这些属于实现阶段。

## Cascade 与 manifest

`entry.css` 是唯一入口并固定 `reset → vendor → tokens → base → astryx → ui → features → overrides`；`tokens.css` 由入口接入 `tokens`。Astryx 自带的真实层名是 `astryx-base`，因此由 `vendor.css` 汇入后再由入口包进顶层 `astryx`，而不是企图用同名层声明覆盖它；所有第三方 CSS 只允许在 `vendor.css` import，TS/TSX 禁止直接 import CSS。CSS Modules 不会自动进入 layer：`ui/**.module.css` 必须包 `@layer ui`，`features/**.module.css` 必须包 `@layer features`，机器检查在第一份模块样式出现前已经生效。

`global-classes.yaml` 今天是空数组，因为 `web/src` 尚无组件/业务 CSS，`tokens.css` 也没有 class selector。未来只有经 change request 批准、确需跨模块共享的应用类才会登记；CSS AST 实际集合与 manifest 双向相等，所以空值表达“任何未登记全局类都禁止”，不是未完成占位。负向 fixture 注入 `.escaped` 会触发 `CSS-only class`。

`unlayered-exceptions.yaml` 今天也是空数组，因为尚无 CodeMirror 组件 CSS，所有当前规则均可分层。未来仅可登记无法进入 layer 的第三方 CodeMirror 覆盖，条目必须精确到 `path + selector + property + expiry`，且 selector 最右复合必须含 `.cm-*`；负向 fixture/变异用 `.panel .not-cm` 证明非 CodeMirror 规则会红。因此空数组是“零已批准例外”的可执行策略，不是空洞。

旧实现的 14 处 `.cm-*` 祖先 hook 决策为迁移到 `data-nc-*` 应用容器，不进入全局类 manifest；阶段 2 的 `systems/fs-viewers` owner 执行。第三方 `.cm-*` 保持供应商类，应用祖先不再依赖会被 CSS Modules 哈希化的类名。

## `data-*` DOM 契约

应用自有 locator 使用 `data-nc-<kebab-case>`；名称全小写，至少一个语义名段。允许无值布尔标记或稳定的枚举/ID 字符串值，禁止把可见文案或样式状态当 locator。`data-theme` 是文档主题协议，`data-testid` 只供测试；冻结接口中既有的 `data-variant` 是视觉变体而非 locator，并作为精确路径遗留项。`aria-*` 只表达无障碍语义，不能替代测试/运行时定位；`data-nc-*` 也不能替代 role、name、state。AST 检查会拒绝生产 TSX 中其他 `data-` 名称，negative fixture 的 `data-card-id` 必红。
