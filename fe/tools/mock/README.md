# Mock contract generator

`npm run mock:generate` reads the legacy OpenAPI document and the frozen wire type file, then writes the complete operation contract to `mock/generated`. Generated files are immutable review artifacts: `npm run test:mock-drift` obtains the expected file set with `git ls-files`, regenerates in memory, and compares every tracked byte.

The PR2 path-dispatch checkpoint is `validateNoManualPathDispatch`. Runtime adapters must consume structured `template` tokens from `mock/generated`; path-template literals outside `mock/generated` and `mock/scenarios` are violations. PR2 must wire that function to the then-known adapter source set rather than adding handwritten method/path branches.

Fixture manifests are intentionally hard-coded in `generator.test.ts`. Each positive kills loss of a supported OpenAPI shape; each negative source owns a specifically asserted violation. Adding either a manifest entry without a file or a file outside the manifest fails the bidirectional equality test.
