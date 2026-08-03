# Architecture rules

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
unrelated to this namespace contract.

`architecture/no-class-dom-query` parses static selectors used by
`querySelector(All)`, `closest`, and `matches`; every class selector is banned,
and dynamic selectors fail closed. `getElementsByClassName` is always banned.
Use stable `data-*` hooks. Third-party DOM may use an exact `{ file, selector }`
exception only when the selector includes an application container prefix,
such as `.file-viewer-code-wrap .cm-scroller`.

`duplication-manifest.mjs` is the single owner of `INV-DUP-001..010` paths.
Eight entries enforce one named implementation and one import source. The two
markdown entries enforce the sole `core/markdown/public.ts` entry and divide
renderer tooling from parser/outline tooling so each fence is independently
testable. Consumers should use the canonical public path, never deep imports.

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
