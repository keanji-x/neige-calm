# Architecture rules

`architecture/no-direct-persistence` keeps browser persistence in
`core/keys/storage.ts`. Application code should receive a storage port; this
keeps platform access explicit and makes core logic runnable in Node.

The rule catches direct globals, `window`/`globalThis` members (including
computed members), destructuring, and aliases created from those expressions.
It does not attempt inter-module or arbitrary function data-flow analysis.
