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
