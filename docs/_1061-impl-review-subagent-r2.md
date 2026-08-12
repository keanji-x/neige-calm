# 1061 收尾修复报告

范围：在 `2c1a5e91` + `631fd6aa` 上收口 r2 的 1 Major / 5 Minor。以下输出均为
`fe/node_modules/.bin/depcruise <inputs> --config tools/architecture/fixture-config.cjs` 实跑。

## 结论

- `runtime-no-verification-domains` 目标改为 `^(mock|tools|e2e|web/e2e)/`；negative fixture
  同时保留 `e2e/` 并新增 `web/e2e/`，两条未来落点各有一条真实依赖。
- 测试豁免放宽为 `\.(?:test|spec)\.[cm]?[jt]sx?$`。理由：与 `check-test-tier.mjs`
  已认可的 JS/TS 扩展集合一致，也兑现 README 的 `*.test.*` / `*.spec.*` 约定。
- `styles-no-runtime-layers` 增加同一测试豁免；positive fixture 证明 styles 契约测试可导 core。
- `.dependency-cruiser.cjs`、`fe/AGENTS.md`、architecture README 与设计文档口径已同步。

## 改前 / 改后实测证据

### 1. `e2e/` 与 `web/e2e/` 两个目标分支

改前，同一 negative fixture 的四条真实 import 中只有前三条红，`web/e2e` 漏过：

```text
error runtime-no-verification-domains: web/src/ui/bad.ts → tools/value.ts
error runtime-no-verification-domains: web/src/ui/bad.ts → mock/value.ts
error runtime-no-verification-domains: web/src/ui/bad.ts → e2e/value.ts
x 3 dependency violations (3 errors, 0 warnings). 5 modules, 4 dependencies cruised.
```

改后，`web/e2e` 变红，且 `e2e` 仍红；四条均只命中目标规则：

```text
error runtime-no-verification-domains: web/src/ui/bad.ts → web/e2e/value.ts
error runtime-no-verification-domains: web/src/ui/bad.ts → tools/value.ts
error runtime-no-verification-domains: web/src/ui/bad.ts → mock/value.ts
error runtime-no-verification-domains: web/src/ui/bad.ts → e2e/value.ts
x 4 dependency violations (4 errors, 0 warnings). 5 modules, 4 dependencies cruised.
```

### 2. JS/MJS 测试形状豁免

改前，新增的两个 positive 分支均误红：

```text
error runtime-no-verification-domains: web/src/ui/good.test.mjs → tools/value.mjs
error runtime-no-verification-domains: web/src/ui/good.test.js → tools/value.js
x 2 dependency violations (2 errors, 0 warnings). 9 modules, 6 dependencies cruised.
```

改后，同一 fixture 变绿：

```text
✔ no dependency violations found (9 modules, 6 dependencies cruised)
```

### 3. styles 测试豁免

改前，真实存在目标文件时契约测试导 core 被误伤：

```text
error styles-no-runtime-layers: web/src/styles/tokens.contract.test.ts → core/value.ts
x 1 dependency violations (1 errors, 0 warnings). 4 modules, 2 dependencies cruised.
```

改后，同一 fixture 变绿：

```text
✔ no dependency violations found (4 modules, 2 dependencies cruised)
```

## 其余 Minor 逐条处置

- m2（纯命名豁免过宽）：维持；这是已记录且可接受的命名契约，不扩大目录豁免。
- m4（fixture 空转护栏只覆盖 depcruise 路由一半）：本轮不固化。完整实现需要解析
  import/export/dynamic import/require 并复刻各 fixture 路由，还要维护
  `not-to-unresolvable` negative、duplication/ESLint 纯文本 fixture 的故意缺失例外；低成本
  正则扫描会制造假护栏和脆弱 allowlist。登记后续：提取 architecture harness 的统一模块解析器，
  由路由显式声明“必须可解析/故意不可解析”，再让 tracked-fixtures 复用。
- m5（`web/dist/` 与根级同名文件）：不扩规则。它们不属于允许的源码落点，且评审已判定
  为理论风险；把目录规则改成同名文件匹配会扩大误报面。风险继续登记于本报告。
- 设计文档漂移：已低成本收口；两处 e2e 描述、两条规则草案和豁免说明均与实现一致。

## 验证

- architecture fixture harness：`63 passed (63)`。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint && npm run build && npm test`：通过；depcruise
  `184 modules / 507 dependencies` 无违规，Vitest `1024 passed / 1 skipped`，wire 与 mock drift 通过。
