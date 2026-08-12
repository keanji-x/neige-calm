# Architecture rules

The `core-no-web-styles` fixture directory is the styles-path case for the
`core-no-web-layers` dependency-cruiser rule; it does not represent a separate rule.

Dependency-cruiser fails closed on unresolvable imports, treats `styles` as a leaf
that cannot import runtime layers, and prevents non-test runtime code from importing
verification-only `mock`, `tools`, or `web/e2e` modules so fixtures cannot enter bundles.

`architecture/no-direct-persistence` keeps browser persistence in
`core/keys/storage.ts`. Application code should receive a storage port; this
keeps platform access explicit and makes core logic runnable in Node.

The rule catches direct globals, `window`/`globalThis` members (including
computed members), destructuring, and aliases created from those expressions.
It does not attempt inter-module or arbitrary function data-flow analysis.

`architecture/no-calm-key-outside-core-keys` makes `core/keys/**` the owner of
all string and template heads beginning `calm:` or `calm.`. Consumers should
import a key factory or accept the key through an injected port. A deliberately
split constant such as `'calm' + ':x'` is a known escape: recognizing it safely
requires constant folding/data-flow, while banning all concatenation would be
unrelated to this namespace contract. A template whose non-first static segment
contains the prefix (for example, `` `${prefix}calm:x` ``) is likewise a known
escape because only the statically anchored head proves the runtime prefix.

`architecture/no-class-dom-query` parses static selectors used by
`querySelector(All)`, `closest`, and `matches`; every class selector is banned,
and dynamic selectors fail closed. A module-scope `const` initialized once from
a string literal in the same file is resolved as that literal. Function
parameters, imports, template literals, concatenations, and reassigned `let`
bindings remain fail-closed. `getElementsByClassName` is always banned.
Destructuring a method first (for example,
`const { querySelector } = document`) is a known escape because the call no
longer retains its DOM receiver; resolving that alias requires data-flow.
Use stable `data-*` hooks. Third-party DOM may use an exact `{ file, selector }`
exception only when the selector includes an application container prefix,
such as `.file-viewer-code-wrap .cm-scroller`.

`duplication-manifest.mjs` is the single owner of `INV-DUP-001..010` paths.
Eight entries enforce one named implementation and one import source. The two
markdown entries enforce the sole `core/markdown/public.ts` entry and divide
renderer tooling from parser/outline tooling so each fence is independently
testable. Consumers should use the canonical public path, never deep imports.
An export specifier's public alias is checked (for example,
`export { Local as SchemaForm }`). Renaming a declaration to an unlisted public
name such as `SchemaFormV2` remains a known escape: detecting semantic
replacements would require type/behavior analysis rather than symbol ownership.

共用解析、不共用策略：抽公共 helper 时，把“怎么解析”和“解析结果怎么判定”分开，
策略通过参数传入或留在调用方。这里 consumer 的无扩展名 import 需要去扩展名后解析，
而 owner 文件必须与 manifest 的真实路径精确相等。已审计本 checker 的其余共用 helper：
`staticString()` 只解析受支持的静态模块源，`packageMatches()` 只执行包围栏匹配，
`exportedNames()` 只提取 ownership 所需的公开名，均没有被语义相反的策略消费者共用。

The public-symbol syntax table is exhaustive over the enumerated statement forms:

| Public shape | Decision |
| --- | --- |
| exported variable (including destructuring), function, class, interface, type, enum, namespace | checked |
| local export alias, named re-export, `export * as X` | checked by the public export name |
| `export declare` variable/function/class | checked |
| `export default class X`, `export = X`, `export { X as default }` | allowed: these publish `default`/the module value, not a named `X` export |
| `export default function X` | allowed: publishes `default`, not a named `X` export |
| bare `export * from './module'` | registered escape: the checker does not resolve the re-exported module's names; the consumer-import check catches only named consumption |
| CommonJS `exports.X = ...` | registered escape: the ownership checker analyzes TypeScript export statements, not CommonJS assignments |
| nested `export namespace NS { export const X }` | registered escape: `X` is nested under `NS`, not a top-level named export |
| ambient `declare module` / `declare global` exports | registered escape: declarations augment external/global types rather than publish a runtime module export |

The package-import syntax table is also exhaustive for literal package sources:

| Package shape | Decision |
| --- | --- |
| static, side-effect, and type-only import | checked |
| named, star, and namespace re-export | checked |
| dynamic `import()`, `require()`, and TypeScript `import = require()` | checked |

Non-literal computed package names are a registered escape: resolving them would
require constant folding/data-flow, while rejecting every computed load would ban
unrelated application behavior. Dependency-cruiser fixtures verify that both
public-entry rules see dynamic imports. ESLint fixtures lint a dynamic markdown
import through each of the three markdown-restriction pattern configurations.

The consumer-import syntax table is exhaustive over these enumerated forms:

| Consumer shape | Decision |
| --- | --- |
| named import and default import | checked by the consumed/local binding name |
| namespace import followed by member access | checked by the accessed member name |
| named re-export from another module | checked as consumption of the original export name |
| dynamic `import()` followed by destructuring | checked for a literal relative source |
| `require()` followed by member access | checked for a literal relative source |

Computed module sources and computed namespace properties are registered escapes:
proving their values would require constant folding or data-flow analysis.

The local mutation harness first runs the named sentinel, then reruns its whole
Vitest file without `-t`. Its report lists every failed test and the full-suite
failure count; multiple failures are expected when one shared checker branch
guards several independently enumerated shapes or contracts.
The harness publishes its restore target only after the temporary backup copy
succeeds, so an early signal cannot restore an empty file. Bash may defer a
SIGTERM trap until the foreground Vitest process exits; restoration then runs
before the harness terminates.

| Contract | Constraint type | Rule/check | Fixture directory |
| --- | --- | --- | --- |
| INV-DUP-001 | unique implementation | duplication manifest | `dup-inv-001` |
| INV-DUP-002 | unique implementation | duplication manifest | `dup-inv-002` |
| INV-DUP-003 | unique implementation | duplication manifest | `dup-inv-003` |
| INV-DUP-004 | markdown import fence | duplication manifest + markdown public entry | `dup-inv-004` |
| INV-DUP-005 | markdown import fence | duplication manifest + markdown public entry | `dup-inv-005` |
| INV-DUP-006 | unique implementation | duplication manifest | `dup-inv-006` |
| INV-DUP-007 | unique implementation | duplication manifest | `dup-inv-007` |
| INV-DUP-008 | unique implementation | duplication manifest | `dup-inv-008` |
| INV-DUP-009 | unique implementation | duplication manifest | `dup-inv-009` |
| INV-DUP-010 | unique implementation | duplication manifest | `dup-inv-010` |

The markdown import fence includes `micromark*`; consumers use
`core/markdown/public.ts`. `architecture/no-core-platform-escape` closes the
`globalThis.fetch` and dynamic `import()` paths left by identifier-only global
restrictions. Inject a transport or adapter through a static boundary instead.
