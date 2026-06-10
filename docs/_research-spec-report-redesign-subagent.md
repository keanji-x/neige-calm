# Research — Spec + Wave Report 视觉重设计 (subagent channel)

Read-only design research. 3 mutually-distinct concept directions for
`web/src/cards/builtins/{spec,wave-report}.tsx` rendered side-by-side at
6 + 6 cols of the wave grid.

## Section A — 现状解读: 为什么这两张 card 看起来"丑/缺设计感"

1. **角色同构, 但内容异构.** 两张 card 共用 `.card-head` chrome (1px hairline
   底边 + uppercase 12.5px title + `.card-head-status` 右贴), 即便它们的内容
   节奏完全不同 — 左边是"正在发生"的流式 telemetry, 右边是"已成稿"的编辑
   prose. 视觉系统把"过程"和"成品"画成了亲生兄弟, 眼睛失去了锚点.

2. **Spec 的 chat bubble 是 1990s BBS 美学.** 每条 `TimelineBubble` 都是
   1px hairline 框 + 上贴 700/uppercase/`text-3` 的 "USER" / "MCP" attribution
   小标. 这是邮件列表存档的视觉传统 — 强调"逐条边界", 而不是"对话节奏".
   而且 user 消息、agent 消息、function_call、function_call_output 四种语义
   的差异只靠 paper / paper-2 切换和 hairline 强度区分, 三阶都挤在同一根灰度
   轴上, 没有暖/冷/中性三级.

3. **Report 的 section 像非自定义 Notion.** ▸/▾ caret + uppercase 12.5px
   `text-1` heading + 1px 左 border (only when `attention`). 这套语言是
   Notion 1.0 默认外观, 它对 wave-report 想表达的"AI 在写一份带 H1 结构的
   工作纪要"基本是 0 语义增益. "needs attention" 走 `--warn-soft` 染色, 但
   它是孤岛 — 其它 H1 既不分阶, 也没有 timeline / blockers / decisions 这
   类 section-type 自己的视觉位置.

4. **Color affordance 跑在一根灰阶上.** `--paper` 和 `--paper-2` 是仅有的
   两层背景, `--warn-soft` 是唯一暖色注入点. 既没有 `--accent` 的冷调存在感,
   也没有 cove 染色的"这是哪一团事"的归属感. 整张 6+6 视图变成了两块发灰
   的纸, 眼前无暖意, 眼角无锚点.

5. **chrome 被 affordance 推得变形.** Report 的铅笔编辑按钮塞在 `CardHead`
   的 `children` 槽位里, 为了不撞 absolute-positioned 的关闭 ×, CSS 里写
   了一条 `margin-right: calc(close-size + space-2)` 的修补 — 这是 chrome
   被外挂功能磨损的痕迹.

6. **Inline-style 债务暴露设计真空.** Spec card 里 `HarnessStateChip`,
   `TimelineBubble`, `TimelinePre`, attribution 小标全是行内 style — `.card-head`
   / `.wave-report-*` 类家族根本没有为 timeline / chip / pre 等内容类型
   留位置. 没有 named class 就意味着没有设计决策被写下来, 一切都是临时.

7. **唯一活的东西是个圆点.** FSM dot 是整个左 card 仅存的"live"信号. 一个
   2px 圆点承担"这是个实时 agent"的全部存在感 — 比例失衡.

## Section B — 三个互不重叠的设计方向

---

### Concept 1 — Editorial Broadsheet

> 一句话气质: "Spec 是发报室的拍纸, Report 是排好版的内页."

把"restrained, editorial"自述当真. **左 card 取 newsroom 拍纸 (telex)
节拍**; **右 card 取报刊内页 essay 节拍**. 两边共享 chrome 的同时, body
内部走两套不同的 typographic persona — 不是字号大小变化, 是 voice 变化.

```
+- SPEC ---------------------------------+  +- REPORT --------------------------------+
| S  SPEC                  WORKING  Plan |  | R  REPORT                        REVIEW |
+---|--------------------------------|---+  +-----------------------------------------+
| Goal — 让 spec card 不再丑          |  | Restrained editorial redesign            |
| ──────────────────────────────── |  |                                          |
|                                 |  | The pair carries opposing temporal     |
| 09:42 · user                    |  | modes: spec is process; report is the  |
| > 你看一下 wave-report 这边     |  | settled artifact. The chrome stays     |
| > 怎么调好看一点                |  | shared. Voice diverges inside.         |
|                                 |  |                                          |
| 09:42 · agent · plan            |  | # Decisions                              |
| 我打算先看 Bubble 的 attribution  |  | ───────                                   |
| 节奏, 再看 section toggle 的留白. |  | · adopt --font-serif on H1 body         |
|                                 |  | · keep IBM Plex Sans for everything UI  |
| 09:43 · function · grep         |  | · drop section-left-border in favor of  |
|   $ rg "TimelineBubble"         |  |   eyebrow + smallcaps                   |
|                                 |  |                                          |
| 09:43 · output                  |  | # Needs attention                       |
|   spec.tsx:425  TimelineBubble  |  | ───────                                   |
|   spec.tsx:483  text ? 'default'|  | Decision needed on whether report goes  |
|                                 |  | full prose or stays bullet-led.        |
| 09:44 · reasoning  ⌃            |  |                                          |
|                                 |  | # Timeline   ▸                          |
| 09:44 · MCP · calm.report.read  |  |                                          |
|   server/tool · ok              |  | # Blockers (none)                       |
|                                 |  |                                          |
+----------------------------------+  +-----------------------------------------+
```

**设计语言关键决策**

- **Typography**: Report 的 H1 切换到 `--font-serif` (New York / Charter
  fallback), body 保持 Plex Sans 但增到 14-15px / `--leading-loose`,
  H1 下增加一根 `½em` thin rule (排版传统的 "section break"). Spec 全部
  走 Plex Mono (`--font-code`) 作为 timestamps + attribution, body 文本
  仍是 Plex Sans 但走 `--text-sm` + `--leading-snug` (telex-condensed).
- **色彩策略**: 中性, 不引入 accent. 用印刷品的"墨色梯度" — text-1 / text-2 /
  text-3 三阶, 加一根 `--hairline` 在 H1 下方做 rule. 不用 cove tint.
- **间距**: Spec 用 `--space-2 / --space-3` 紧凑节拍; Report 用 `--space-5 /
  --space-6` 留白宽松节拍. 同一根纸面, 两种呼吸.
- **Motion**: 几乎无. FSM dot 的 pulse 保留, 其它一切静止. 编辑态 → 阅读态
  做 200ms ease-out crossfade.

**与现有 token 体系的契合度**

高契合. `--font-serif` 已在 `:root` 定义但当前 0 consumer (calm.css:11),
这就是它的第一波启用. 不需要新颜色 token. 需要新增的 token 仅 ~3 个:
`--rule-section` (薄 rule), `--prose-measure` (~62ch readable measure),
`--telex-stamp-color` (alias `--text-3`).

**风险**

- 在 6 列宽度下塞 prose measure 容易"挤"; report 折叠后单列宽度 ~360-420px,
  接近 60ch 临界值, serif 在小尺寸 + 高 DPI 下要看实际渲染.
- Plex Mono 的 timestamps 在 spec card 里如果太密会变成 ascii art; 节拍
  控制是这个方向最难的一环.
- Serif H1 是审美双刃 — 若用户的"editorial"指的是"印刷美", 完美; 若指的
  是"克制 sans", 这一刀就走偏了.

---

### Concept 2 — Quiet Operations Console

> 一句话气质: "把 cove 染色搬到 card body, 让两张 card 各自属于一个色块."

延续最近的 color-anchored sidebar 方向. **每张 wave 已经有一根 cove
tint**; 让这根 tint 从 sidebar 一路渗透到 card body — Spec / Report
不再是两块灰纸, 而是同一团 cove 颜色下的两个**操作面板**.

```
+========== SPEC =========================+  +========== REPORT =======================+
‖ S  SPEC                  WORKING  ◉    ‖  ‖ R  REPORT                        REVIEW ‖
+========================================+  +========================================+
‖ ▸ Goal                                ‖  ‖ ──────────────────────────────────────  ‖
‖   让 spec card 不再丑                  ‖  ‖   Restrained editorial redesign       ‖
‖                                       ‖  ‖ ──────────────────────────────────────  ‖
‖ │ 09:42                                ‖  ‖ ▾ Decisions                            ‖
‖ ●─ user · 你看一下 wave-report         ‖  ‖   · adopt cove tint on card surfaces  ‖
‖ │                                      ‖  ‖   · drop hairline borders in body     ‖
‖ ●─ agent · 我打算先看 attribution      ‖  ‖   · ribbon rather than bubble list    ‖
‖ │   节奏, 再看 section toggle 留白     ‖  ‖                                          ‖
‖ │                                      ‖  ‖ ▾ Needs attention   ▣ tinted block    ‖
‖ ○─ grep · TimelineBubble               ‖  ‖   Decision needed on prose vs bullet ‖
‖ │   spec.tsx:425, :483                 ‖  ‖                                          ‖
‖ │                                      ‖  ‖ ▾ Open questions   ▣ tinted block     ‖
‖ ○─ reasoning ⌃                         ‖  ‖   Section type vocabulary not yet    ‖
‖ │                                      ‖  ‖   landed; do we add 'risk' / 'decision'‖
‖ ◇─ MCP · calm.report.read · ok        ‖  ‖   as first-class section kinds?       ‖
‖ │                                      ‖  ‖                                          ‖
‖ ●─ agent · …                          ‖  ‖ ▸ Timeline                              ‖
+========================================+  +========================================+
       │ cove-color rail (left edge)             │ cove-color rail (left edge)
```

**设计语言关键决策**

- **Typography**: 仍是 Plex Sans 单字体; 但 timeline 引入 `--font-nav-*`
  bundle 的 actor 角色 — user / agent / tool / mcp / reasoning 五个角色
  各自有 weight + tracking + color 套. Report 的 H1 取 `--font-nav-section`
  (uppercase 11px/700/wider) 但**移除 left-border**, 改成 H1 之下的
  ¼em rule + 节段之间用 cove-tint block 染色.
- **色彩策略**: **核心改动**. 引入 `--card-tint-cove` token, 由 wave 所属
  cove 的 hue 驱动 (类似 sidebar 的 `--cove-tint-strength: 10%`). card 整体
  background 取 paper, 但左 8px 是 cove-tint 的浓缩色带 (rail); body 内的
  attention block 取该 cove hue 的 mix(paper, hue, 6%) 作为内 block 底色,
  替代当前的 warn-soft 黄色独占. warn 只留给真正"出错"的 row.
- **间距**: Spec timeline 走 `--space-2` 节拍 + 左 gutter 18px 内画一根
  `1px dashed --hairline` 时间轴; Report 节段间留 `--space-6`, block 内
  压回 `--space-3`. 节段之间的留白才是结构感来源.
- **Motion**: Spec timeline 新 row 用 80ms slide-up + 200ms fade-in,
  保持 telemetry 的"在动"感. FSM dot 保留 pulse. Report 切到编辑态用
  300ms ease 把 body 替换为 textarea (避免 snap).

**与现有 token 体系的契合度**

中-高. 需要新引入的 token family:
- `--card-tint-{cove-id}`: cove 染色的浓缩值 (类似 `--cove-tint-strength`).
- `--timeline-actor-{user|agent|tool|mcp|reasoning}-{color|weight}`: 5 个
  actor 的语义颜色 / 字重, 大量替代当前 `HarnessStateChip` / `TimelineBubble`
  内的 inline style.
- `--section-tint-soft`: 替代 `--warn-soft` 作为 attention block 底色, 自适应
  cove hue.

bonus: `WaveContext` 已经能拿到 wave 的 cove (sidebar 已经在用), 不需要
新的数据通路.

**风险**

- Cove tint 染色如果不够 desaturate, 6 列宽里两张 card 会变成色块墙;
  调到正确的"几乎察觉不到"是 oklch chroma 0.01-0.03 的细活.
- Timeline actor 颜色族会引入一组新的 5-彩, 与现有 `--accent` / `--warn` /
  `--success` / `--error` 的语义空间相邻 — 容易撞车. 必须在 token 命名上
  明确区分"actor identity"和"status outcome".
- 用 cove hue 染 attention block 后, 不同 wave 之间 "attention" 的视觉
  权重不一致 (蓝 cove vs 红 cove 的 attention block 显眼程度不同). 可能
  需要保留 warn 作为 "critical attention" 的 escape hatch.

---

### Concept 3 — Studio Sheet (radical)

> 一句话气质: "拆掉 card chrome, 把两张 card 当成一对排版稿."

放弃"card 是个有 header 的盒子"的隐含约定. 不再有 `.card-head` 这条
hairline 底边; 取而代之, 每张 card 左侧 56-72px 是一条**marginalia
gutter** — status / lifecycle / actor glyph / timestamps / 操作按钮全
搬进 gutter. body 浮在右侧, 不被 top-border 框住. **Spec 的 chat bubble
被彻底删除**, 改成连续的 typographic ribbon (没有边框, 用 typeface 区分
说话人); **Report 用悬挂式排版** (H1 挂在 gutter 里, body 走 narrow measure).

```
+---SPEC---------------------------------+  +---REPORT-------------------------------+
| S |                            WORKING |  | R |                          IN REVIEW |
|   |                                    |  |   |                                    |
|   | Goal — 让 spec card 不再丑         |  |   | Restrained editorial               |
|   | ─────────                          |  |   | redesign for the wave              |
|   |                                    |  |   | grid's left-and-right cards.       |
|09 |                                    |  |   |                                    |
|:42| 你看一下 wave-report 这边怎么调.   |  |Dec| Two cards, two voices: spec is     |
|usr|                                    |  |isi| process, report is artifact.       |
|   |                                    |  | on| Chrome separation costs both.      |
|09 | 我打算先看 attribution 节奏,        |  | s |                                    |
|:42| 再看 section toggle 的留白.        |  |   | · drop card-head as a slot row.    |
|agt|                                    |  |   | · gutter holds status + lifecycle. |
|   |                                    |  |   | · spec ribbon replaces bubbles.    |
|09 |   $ rg "TimelineBubble"           |  |   |                                    |
|:43|   → spec.tsx:425, :483             |  |Att| Decision needed on prose vs.       |
|grp|                                    |  |ent| bullet-led for the body.           |
|   |                                    |  |ion|                                    |
|09 | 〈reasoning, 38 lines, ⌃ 展开〉    |  |   |                                    |
|:44|                                    |  |Tim| 09:42  brief written              |
|rsn|                                    |  |eln| 09:44  3 reads complete           |
|   |                                    |  | ie| 09:51  draft pushed               |
|09 | calm.report.read · ok              |  |   |                                    |
|:45|                                    |  |Blk| (none)                             |
|mcp|                                    |  |ers|                                    |
+---+------------------------------------+  +---+------------------------------------+
   │                                          │
   gutter: timestamp + actor + glyph         gutter: section name vertical
```

**设计语言关键决策**

- **Typography**: **引入第二字体**. Report body 切到 `--font-serif`
  (New York / Charter), 整段 prose 走 15px / `--leading-loose`, 宽度
  ~52ch readable measure (受 gutter 挤压后正好). Spec 全部 Plex Sans;
  但说话人语义用**字形差异**表达:
    - user: italic, `--text-1`, regular weight (说出的话, 引用语气)
    - agent: regular, `--text`, body weight (作者声音)
    - tool / output: `--font-code` (Plex Mono), `--surface-code` strip
    - reasoning: hairline-bracketed aside, 38px hanging quote glyph
    - MCP: → arrow + monospace tool path
  完全无 bubble box, 节奏靠**段落 + 缩进 + 字形** 而非边框.
- **色彩策略**: 极简. paper / hairline / text-1/2/3, 几乎不引入 accent.
  Gutter 用 `paper-2` 浅底, body 用 `paper`. Lifecycle badge 撤出 header,
  改竖排 `text-orientation: sideways` 写在 gutter 顶部 (uppercase 9px
  vertical mark). attention 不染色 — 用 H1 旁的 `▣` 实心方块 marker.
- **间距**: 重排了 box-model 含义. gutter 是固定 64px 列, body 是流式列;
  H1 用 `margin-left: -56px` hanging 出 gutter (悬挂排版). 章节间留
  `2em` 留白. 整个 card 内部呼吸是杂志感.
- **Motion**: 编辑态切换走 250ms 的 column-grid 重排 (gutter 收起,
  textarea 占满); spec timeline 新 actor 出场用 fade-in only, 不
  slide — 因为没有 bubble 框可以 slide, 就是字直接显出来. FSM dot
  从 header 撤到 gutter 顶端 + 改用 vertical text "running" 代替.

**与现有 token 体系的契合度**

低. 这是激进方向. 需要新增的:
- `--gutter-w`, `--gutter-bg`, `--gutter-pad-v`: 排版网格 token.
- `--prose-measure`, `--prose-hang`: 悬挂排版的 hang amount.
- `--actor-voice-{user|agent|reasoning}` 的字形约定: italic / regular /
  hairline-bracketed, 不是颜色而是 style.
- 必须改 `CardHead` 的 API 假设 — 不再"icon + title + status + close"
  的水平槽, 改成"top-of-gutter region", `.card-head` 类几乎被废.

**风险**

- **chrome 拆解的连锁伤害**: `.card-head` 是全局共享的 (Codex / Terminal /
  File Viewer 等多张 card), 在 spec + report 上独立打破会让两张 card 与
  wave 视图里其它 card 强烈分裂. 需要要么所有 card 都跟进, 要么明确这两张
  是"特殊 card class" (会增加视觉异构).
- Serif body 在 13-15px 范围内的 Plex 替代字体可获得性有限. 真正的 New York /
  Charter 在 Linux 上未必能渲染好; fallback 链可能塌到 Georgia.
- 悬挂排版需要 H1 出 card 边界 ~50px, 与 6 列宽度的物理边界硬冲突. 在
  ≤480px 宽度下要 fallback 到 inline H1.
- "斩 card-head" 在产品里会让用户找不到"关闭" / "操作" 按钮 — 需要重新教育.

---

## Section C — 推荐

推 **Concept 2 (Quiet Operations Console)**.

理由:

1. **与现有 design language 直接接轨**: color-anchored sidebar 已经在落地,
   把 cove tint 从 sidebar 延展到 wave-body 是同一根设计线的下一拍,
   不需要解释"为什么变". Concept 1 把 `--font-serif` 启用是可选项,
   Concept 3 拆 `.card-head` 是革命 — 1 没强势收益, 3 成本太高.

2. **实现成本可控**: 主要工作量在 token 引入 (cove-tint + actor identity)
   和把 `spec.tsx` 里大量 inline-style 收编到 named class. 不动 `CardHead`
   API, 不动卡片网格. 大部分改动落在 calm.css + 两个 builtin 文件内.

3. **用户使用频次最高**: 单人开发 / 架构师每天盯 wave 视图, 6+6 是主屏
   中央. 让两张 card 各自属于一个 cove 色块, 在多 wave 切换时眼睛找位置
   的速度 (locate-by-color) 直接吃下收益. 这点收益是 Concept 1 给不了的
   (1 是审美收益, 不改变 scanning behavior).

4. **视觉差异化最强 / 美学风险最低**: 不引入新字体, 不动 chrome 拓扑,
   也不需要为"印刷感"做 fallback. 给 Spec 加 actor identity 颜色族
   是一次性 vocabulary 投资, 之后每个新的 harness item type 都能用
   同一根 token 表达 "这是谁说的话". 这是可复利的 token 资产.

如果用户 review 后觉得 Concept 2 还不够"editorial", 那 Concept 1 可
作为后续 PR 叠加 (它和 2 不冲突 — 2 给位置感, 1 给字形感, 两者
正交). Concept 3 留作长线 (R&D / 大重构的 anchor) — 它代表"如果整套
card 系统都重新排版"的终极形态, 不适合作为单 PR.
