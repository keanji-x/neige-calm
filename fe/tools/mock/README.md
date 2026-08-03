# Mock contract generator

`npm run mock:generate` reads the legacy OpenAPI document and the frozen wire type file, then writes the complete operation contract to `mock/generated`. Generated files are immutable review artifacts: `npm run test:mock-drift` obtains the expected file set with `git ls-files`, regenerates in memory, and compares every tracked byte.

The PR2 path-dispatch checkpoint is `validateNoManualPathDispatch`, but its current regex is definition-only and incomplete: it catches only quoted, slash-leading literals containing `{...}`. It misses concatenation (`'/api/coves/' + id`), prefix checks, regular-expression routes, and split/index dispatch. Its `mock/scenarios` exemption also reserves exactly the place most likely to hard-code scenario paths. PR2 must replace this with import-graph/AST enforcement over the then-known adapter sources and remove the scenario exemption; wiring the current regex would create a false-green gate.

`mock` currently compiles in the Node TypeScript project. PR2 must split browser/Vite-middleware compilation boundaries so browser-side mock code cannot use Node APIs without a diagnostic.

Fixture manifests are intentionally hard-coded in `generator.test.ts`. Each positive kills loss of a supported OpenAPI shape; each negative source owns exactly one asserted violation. The lint-time tracked-fixture check compares both mock fixture directories with `git ls-files`, so a fixture omitted from the Git index fails locally.
