# P8b-1 行号差分对拍

路径 A 从 `statement` + `why` 正向定位实现；路径 B 不读取 A 的候选，从 authoritative test（有测试时）或同 family 已有引用与调用方反向定位。表中 `A` / `B` 均为各路径独立得到的最小支持区间；`交集` 是最终落盘引用覆盖的共同实现点。路径迁移时同时更正文件名。

| # | id / 字段 | 路径 A 结论 | 路径 B 结论 | 交集 |
|---:|---|---|---|---|
| 1 | INV-A11Y-007 source | `Sidebar.tsx:224-322` landmark 声明与 JSX | axe landmark scope → `224-322` | 是 |
| 2 | INV-A11Y-008 source | `Sidebar.tsx:240-322` collapse 条件 | 同族 007/054 → `240-322` | 是 |
| 3 | INV-A11Y-009 source | `Sidebar.tsx:258-267` Today button | keyboard Today locator → `258-267` | 是 |
| 4 | INV-A11Y-011 source | `WaveRow.tsx:44-119` sibling buttons | axe nested-interactive → `58-119` | 是 |
| 5 | INV-A11Y-012 source | `WaveRow.tsx:106-118` delete label | delete e2e locator → `106-118` | 是 |
| 6 | INV-A11Y-013 source | `Cove.tsx:168-189` Waves region | 同族 014/056 → `168-189` | 是 |
| 7 | INV-A11Y-014 source | `Cove.tsx:99,168-189` single sorted list | 同族 013 → `99,173-188` | 是 |
| 8 | CAP-A11Y-017 source | `Wave.tsx:61-95` cycle order/button | keyboard cycle test → `73-95` | 是 |
| 9 | INV-A11Y-018 source | `Wave.tsx:73-95` label/title | e2e exact label → `80-94` | 是 |
| 10 | INV-A11Y-019 source | `Wave.tsx:252-266` overlay default | 同族 020/050 → `252-266` | 是 |
| 11 | CAP-A11Y-020 source | `Wave.tsx:252-273,404-433` 三分支 | keyboard view test → `404-433` | 是 |
| 12 | CAP-A11Y-021 source | `Wave.tsx:275-282` add 后切 grid | 同族 020 调用链 → `275-282` | 是 |
| 13 | CAP-A11Y-022 source | `WaveReportPage.tsx:924-945` empty branch | 同族 report 条目 → `930-944` | 是 |
| 14 | INV-A11Y-023 source | `WaveGrid.tsx:239-254` mouse layout | 对价条目 024 → `WaveGrid.tsx:239-254` | 是 |
| 15 | INV-A11Y-024 source | `WaveList.tsx:1-32,190-269` canonical list | keyboard list test → `190-269` | 是 |
| 16 | CAP-A11Y-025 source | `Wave.tsx:61-95` report→grid→list | keyboard cycle test → `61-95` | 是 |
| 17 | INV-A11Y-026 source | `calm.css:4176-4306` supports/picker | 同族 027/029 → `4205-4306` | 是 |
| 18 | INV-A11Y-027 source | `calm.css:4185-4250` trigger | Cove/NewTaskForm markup → `4185-4250` | 是 |
| 19 | INV-A11Y-029 source | `calm.css:4283-4305` checkmark/no fill | 同族 picker rules → `4283-4305` | 是 |
| 20 | INV-A11Y-030 source | `Cove.tsx:494-552` option spans | 同族 select contract → `540-552` | 是 |
| 21 | INV-A11Y-031 source | `Cove.tsx:450-478,554-560` key/focus | 同族 workflow switch → `466-478,554-560` | 是 |
| 22 | INV-A11Y-032 source | Cove `480-490` + Dialog `342-382` | 相邻 caller/API 反查 → 两区间相同 | 是 |
| 23 | CAP-A11Y-033 source | `NewTaskForm.tsx:621-659` checkbox | 同族 034 → `633-659` | 是 |
| 24 | INV-A11Y-034 source | `NewTaskForm.tsx:621-659` description | 同族 033 → `639-657` | 是 |
| 25 | INV-A11Y-035 source | Cove `507-552` + NewTaskForm `913-998` | 两个 label/combobox 调用链反查 → 同区间 | 是 |
| 26 | INV-A11Y-036 source | WaveGrid `239-254` + 三个 CardHead 调用点 | drag selector与消费者反查 → 同四区间 | 是 |
| 27 | INV-A11Y-038 source | CardHead `146-204` + 三个调用点 | 三种 card 反查共享组件 → 同四区间 | 是 |
| 28 | INV-A11Y-040 source | `XtermView.tsx:481-501` tabindex 降级 | 同族 041 → `481-501` | 是 |
| 29 | INV-A11Y-041 source | `XtermView.tsx:481-501,1082-1110` ownership | 同族 040 与容器注释 → 相同 | 是 |
| 30 | INV-A11Y-050 source | schema `39-44` + Wave `252-273` | keyboard overlay test → Wave `252-273` | 是 |
| 31 | INV-A11Y-051 source | layout/view-mode schema `39-44` | 同族 050 → Wave `252-257` | 是 |
| 32 | INV-A11Y-053 source | `Sidebar.tsx:240-510` DOM stop 顺序 | 同族 007/054/055 → `240-510` | 是 |
| 33 | INV-A11Y-054 source | `Sidebar.tsx:240-272` collapsed stop | 同族 008 → `240-272` | 是 |
| 34 | INV-A11Y-055 source | `Sidebar.tsx:253-430` landmarks/buttons | 同族 007/053 → `253-430` | 是 |
| 35 | INV-A11Y-056 source | `Cove.tsx:99-143,168-189` DOM 顺序 | 同族 013/014 → `99-143,168-189` | 是 |
| 36 | INV-A11Y-057 source | `Wave.tsx:295-433` DOM 顺序 | 同族 view-mode 条目 → `295-433` | 是 |
| 37 | INV-A11Y-059 source | `WaveRow.tsx:44-65` native button | Cove EditableTitle 同族范式 → `44-65` | 是 |
| 38 | CAP-A11Y-063 source | hook `265-314` + Menu `123-148` | e2e 反查 hook/Menu → 同两区间 | 是 |
| 39 | CAP-A11Y-066 source | `WaveList.tsx:18-22,123-125,200-230` | keyboard test → 同区间 | 是 |
| 40 | CAP-A11Y-067 source | `WaveList.tsx:18-22,123-125,200-230` | 同族 066 → 同区间 | 是 |
| 41 | CAP-A11Y-068 source | `WaveList.tsx:24-28,133-187,205-217` | keyboard/trace test → 同区间 | 是 |
| 42 | CAP-A11Y-069 source | `WaveList.tsx:30-32,205-225` | 同族 068/070 → `205-225` | 是 |
| 43 | CAP-A11Y-070 source | `WaveList.tsx:18-22,123-125,200-230` | roving hook Tab 不消费 → 同区间 | 是 |
| 44 | INV-A11Y-071 source | `WaveList.tsx:232-267` aria-keyshortcuts | 同族 keyboard 条目 → `232-267` | 是 |
| 45 | INV-A11Y-072 source | `WaveList.tsx:1-43,190-275` 无 resize | 对价 023 → `1-43,190-275` | 是 |
| 46 | CAP-A11Y-073 source | roving hook `230-340` 全键盘 | keyboard menu test → `265-340` | 是 |
| 47 | INV-A11Y-074 source | hook `187-205,265-340` + Menu `123-148` | menu test 反查 → 同三段 | 是 |
| 48 | GATE-A11Y-092 source | `calm.css:386-406` reset 锚点 | focus-visible 邻族扫描 → `386-406` | 是；否定契约 |
| 49 | CAP-A11Y-103 source | `trace.ts:36-89` 四个 helper | trace smoke imports/calls → `36-89` | 是 |
| 50 | INV-A11Y-104 source | config `9-127` + setup `1-168` | trace smoke 反查 → 同两区间 | 是 |
| 51 | INV-A11Y-105 source | playwright dependencies `28-32,91-127` | setup lifecycle → 同区间 | 是 |
| 52 | INV-A11Y-110 source | `ui/README.md:217-289` 文档边界 | 同 family 相邻文档条目 → `217-289` | 是 |
| 53 | GATE-REPORT-BLOCKS-002 source | index `15-27` + schema `26-90` | parity test 反查 → 同两区间 | 是 |
| 54 | GATE-WIRE-003 source | `.cargo/config.toml:19-25` env | schemas test → `19-25` | 是 |
| 55 | CAP-APP-076 authoritative_test | e2e `106-188` instrumentation | source theme hook → `106-188` | 是 |
| 56 | GATE-UI-LAYER-072 authoritative_test | eslint test `61-185` 正反路径 | source README/rule → `61-185` | 是 |
| 57 | INV-APP-071 authoritative_test | e2e `117-188` no-remount | source router/theme seam → `117-188` | 是 |
| 58 | INV-CONNIND-019 authoritative_test | axe suite `229-425` 双主题 | source 注释/相邻 axe 条目 → `229-425` | 是 |

结论：58/58 区间重叠；0 条分歧，未硬填候选。
