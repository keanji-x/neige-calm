# Oracle 条目 schema（所有提取 agent 必须严格遵守）

每个条目一个 YAML block，写进各自的 `docs/oracle/<slice>.yaml`。字段全部必填。

```yaml
- id: INV-DIALOG-001          # <KIND>-<域大写>-<三位序号>，KIND ∈ {INV, CAP, GATE}
  kind: invariant             # invariant | capability | gate
  family: dialog-focus        # 同族条目共享，用于分组
  statement: >                # 一句话，陈述句，说"必须/不得"，不写"应该"
    inert effect 必须声明在 focus-restore effect 之前
  why: >                      # 为什么存在。没有 why 的条目会在重写中被"优化"掉
    React 按声明顺序跑 cleanup；顺序反了 restore 目标仍在 inert 子树下，focus() 静默失败
  source: web/src/ui/Dialog/Dialog.tsx:183-193      # 实现出处，带行号
  authoritative_test: web/src/ui/Dialog/Dialog.test.tsx:230   # 锁定它的测试；无测试写 NONE
  owner_slice: ui/dialog      # 新架构里谁负责
  intentional_omission: false # true = "故意不做"型契约（如 Menu 故意不处理左右箭头）
  runtime_layer: ui           # core | ui | systems | features | app | styles | none
  verification_owner: unit    # e2e | unit | lint | css | build | architecture | review-waiver | null
  test_tier: browser          # browser | jsdom | static | none
  migration: pending          # pending | migrated | skipped（skipped 必须同时写 skip_reason）
```

## 归一化后的四个字段（见 owner-aliases.yaml / NORMALIZATION-REPORT.md）

- **owner_slice** 规范格式 `<layer>/<slice>[/<detail>]`，取值必须出现在 `owner-aliases.yaml` 的值域里
- **runtime_layer** 直接由 owner_slice 的层前缀派生，不得与之冲突。
  `none` = 非运行时（lint 规则 / CI 闸门 / 构建配置 / e2e 基础设施 / 纯文档政策）
- **verification_owner** — 这条契约由谁保证。`architecture` 指依赖方向/文件布局类的架构测试；
  `review-waiver` 指无法自动化、只能靠人 review 的；`null` 仅用于 `migration: skipped`
- **test_tier** — 防「恒真断言」。jsdom 没有布局，"断言没有发生重排"在 jsdom 里永远通过。
  凡涉及布局/几何/滚动/真实焦点/canvas/PTY/计算样式的一律 `browser`；
  静态检查（lint/类型/CSS 解析）`static`；纯逻辑/数据流 `jsdom`；不可自动化 `none`
- **migration: skipped** 用于已知死代码与"故意不迁"，必须给 `skip_reason`

## kind 的区分

- **invariant** — 踩坑教训、排序依赖、竞态防护、"故意不做"。**丢了会重新踩坑**
- **capability** — 用户可达的能力：一个操作、一个快捷键、一个错误态/空态。**丢了功能就少了**
- **gate** — 机器闸门：类型级穷尽、lint 规则、token 形状契约。**丢了防线就没了**

## 多位置引用文法

`source` 与 `authoritative_test` 可引用多个位置。逗号连接同一文件的多个行号或行号区间，
例如 `providers.tsx:110-122,231-236`；分号、` + ` 或空白连接多个文件，
例如 `a.ts:152; b.tsx:73-82`。每个位置均须包含行号，并分别通过路径存在性与行号范围校验。

## 纪律

1. **每条必须有 source 行号。** 无法定位出处的不要写
2. **why 必须是从代码/注释/issue 号读出来的**，不是你推测的。推测的标注 `why: 【推测】...`
3. **"故意不做"型契约优先级最高** —— 它们最容易在重写中丢失，因为新写的人只会看到"没实现"
4. 宁可多写一条，不要漏。重复条目后续合并时去重
5. 不要写实现细节，写约束。"用 useRef 存 prevOverflow" 是实现；"body scroll lock 解除时必须精确还原原 overflow 值" 是约束
