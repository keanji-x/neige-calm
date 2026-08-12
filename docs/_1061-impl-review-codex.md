# 1061 评审修复报告

范围：修复 `2c1a5e91` 的 depcruise fail-open；所有变异均在 `/tmp/depcruise-1061-evidence/*` 独立副本运行，目标文件真实存在。

## 修复

- `styles-no-runtime-layers` 目标加入 `^core/`，现在完整覆盖 `core/app/features/systems/ui/main.tsx`。
- `runtime-no-verification-domains` 将错误的 `web/e2e/` 改为 cruise cwd 下的 `e2e/`。
- 豁免契约定为所有测试形状：`*.test.*`（含 `contract.test`、`browser.test`）及 `*.spec.*`；README 与 fixture 已锁定。
- negative fixtures 分别覆盖 styles 六个目标和 runtime 三个目标；每个 negative 仍只产生同名规则。
- test-tier 与 Vitest discovery 窄排除 architecture fixtures；这些形状由 architecture harness 执行，不是真实测试入口。
- 顺带将 `core-no-web-layers/positive` 改为已解析的 `core/good.ts -> core/value.ts` 合法边。

旧规则缺口复现：`styles -> core` 与 `runtime -> e2e` 均为
`✔ no dependency violations found (2 modules, 1 dependencies cruised)`；修复后的对应 RED 见下。

## 逐分支变异证据

每项“前”是目标文件已存在但尚未写违规 import；“后”只写入所列 import。

### styles-no-runtime-layers

- `styles -> app`：前 `✔ ... (2 modules, 0 dependencies cruised)`；后
  `error styles-no-runtime-layers: web/src/styles/bad.ts → web/src/app/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `styles -> features`：前 `✔ ... (2 modules, 0 dependencies cruised)`；后
  `error styles-no-runtime-layers: web/src/styles/bad.ts → web/src/features/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `styles -> systems`：前 `✔ ... (2 modules, 0 dependencies cruised)`；后
  `error styles-no-runtime-layers: web/src/styles/bad.ts → web/src/systems/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `styles -> ui`：前 `✔ ... (2 modules, 0 dependencies cruised)`；后
  `error styles-no-runtime-layers: web/src/styles/bad.ts → web/src/ui/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `styles -> main.tsx`：前 `✔ ... (2 modules, 0 dependencies cruised)`；后
  `error styles-no-runtime-layers: web/src/styles/bad.ts → web/src/main.tsx`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `styles -> core`：前 `✔ ... (2 modules, 0 dependencies cruised)`；后
  `error styles-no-runtime-layers: web/src/styles/bad.ts → core/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`

### runtime-no-verification-domains

- `runtime -> mock`：前 `✔ ... (1 modules, 0 dependencies cruised)`；后
  `error runtime-no-verification-domains: web/src/ui/bad.ts → mock/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `runtime -> tools`：前 `✔ ... (1 modules, 0 dependencies cruised)`；后
  `error runtime-no-verification-domains: web/src/ui/bad.ts → tools/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`
- `runtime -> e2e`：前 `✔ ... (1 modules, 0 dependencies cruised)`；后
  `error runtime-no-verification-domains: web/src/ui/bad.ts → e2e/value.ts`
  `x 1 dependency violations (1 errors, 0 warnings). 2 modules, 1 dependencies cruised.`

### 测试形状豁免

四项都实际导入真实的 `tools/value.ts`，输出均为
`✔ no dependency violations found (2 modules, 1 dependencies cruised)`：

- `web/src/ui/good.test.ts`
- `web/src/ui/good.contract.test.ts`
- `web/src/ui/good.browser.test.tsx`
- `web/src/ui/good.spec.ts`

## 验证

- 定向 architecture fixtures：`63 passed (63)`。
- `OWNERSHIP_BASE_SHA=origin/main npm run lint`：通过；depcruise `184 modules / 507 dependencies / 0 violations`。
- `npm run build`：通过；Vite `441 modules transformed`。
- `npm test`：通过；Vitest `86 passed / 1 skipped` files，`1024 passed / 1 skipped` tests；wire 与 mock drift 通过。
