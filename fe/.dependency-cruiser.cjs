/** @type {import('dependency-cruiser').IConfiguration} */
module.exports = {
  forbidden: [
    { name: 'core-no-web-layers', severity: 'error', from: { path: '^core/' }, to: { path: '^web/src/(app|features|styles|systems|ui)/' } },
    // UI may consume branded/a11y primitives and core/state infrastructure types only;
    // business-field domain types remain forbidden regardless of type-only imports.
    { name: 'ui-only-core-type-whitelist', severity: 'error', from: { path: '^web/src/ui/' }, to: { path: '^(web/src/(app|features|systems)/|core/(?!(types/(ids|a11y)|state/types)\\.ts$))' } },
    { name: 'systems-no-features-or-app', severity: 'error', from: { path: '^web/src/systems/' }, to: { path: '^web/src/(features|app)/' } },
    { name: 'features-no-app', severity: 'error', from: { path: '^web/src/features/' }, to: { path: '^web/src/app/' } },
    { name: 'layers-no-main-entry', severity: 'error', from: { path: '^(core/|web/src/(features|systems|ui)/)' }, to: { path: '^web/src/main\\.tsx$' } },
    { name: 'features-no-cross-domain', severity: 'error', from: { path: '^web/src/features/([^/]+)/' }, to: { path: '^web/src/features/', pathNot: '^web/src/features/$1/' } },
    { name: 'core-no-react', severity: 'error', from: { path: '^core/' }, to: { path: '(^|node_modules/)(react|react-dom)(/|$)' } },
    { name: 'no-barrel-index', severity: 'error', from: { path: '(^|/)index\\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$' }, to: { dependencyTypes: ['export', 'import'] } },
    { name: 'cards-public-entry-only', severity: 'error', from: { path: '^(core/|web/src/)', pathNot: '^web/src/systems/cards/' }, to: { path: '^web/src/systems/cards/(?!public\\.ts$)' } },
    { name: 'markdown-public-entry-only', severity: 'error', from: { path: '^(core/|web/src/)' }, to: { path: '^core/markdown/(?!public\\.ts$)' } },
    { name: 'no-shared-directory', severity: 'error', from: {}, to: { path: '(^|/)shared(/|$)' } },
    { name: 'styles-no-runtime-layers', severity: 'error', from: { path: '^web/src/styles/', pathNot: '\\.(?:test|spec)\\.[cm]?[jt]sx?$' }, to: { path: '^(core/|web/src/(app|features|systems|ui)/|web/src/main\\.tsx$)' } },
    { name: 'runtime-no-verification-domains', severity: 'error', from: { path: '^(core/|web/src/)', pathNot: '\\.(?:test|spec)\\.[cm]?[jt]sx?$' }, to: { path: '^(mock|tools|e2e|web/e2e)/' } },
    // Modules carrying a final `.test.`/`.spec.` suffix live beside production code rather than
    // in a verification domain, so the rule above cannot see them. A production-reachable test
    // module ships fixtures and mocks in the bundle, and — because eslint `architecture/*` and the
    // checkers exclude `*.test.*` by path — becomes a hole in every gate scoped to production
    // files. The scope is exactly that suffix and it is case-sensitive: test helpers named without
    // it (`web/src/features/track/page/test-fixtures.tsx`) and `__tests__/` directory layouts are
    // out of range by design; widening to catch them would risk false positives on ordinary names.
    { name: 'runtime-no-test-modules', severity: 'error', from: { path: '^(core/|web/src/)', pathNot: '\\.(?:test|spec)\\.[cm]?[jt]sx?$' }, to: { path: '\\.(?:test|spec)\\.[cm]?[jt]sx?$' } },
    /*
     * #1234 — Today declares, per prop, what its compact viewport renders
     * (`features/today/page-props.ts`), and the declaration is only worth
     * anything while one function cannot hold both the viewport bit and the
     * full props: with both in hand, `if (compact) return <>{
     * props.launchpadDocument}</>` type-checks and renders a prop the ledger
     * says the phone does not draw. That was this file's shape before #1234
     * and it is one import away from returning, so the bit is confined to
     * `features/today/viewport-dispatch.tsx`, which is generic in both prop
     * packs and cannot name a field of either.
     *
     * The `pathNot` is that one module and nothing else. Everywhere outside
     * `features/today` — `app/shell`, `app/router`, `ui/drawer` — is
     * untouched: this rule says where Today may read the viewport, not who
     * may read it.
     */
    { name: 'today-viewport-bit-in-dispatcher-only', severity: 'error', from: { path: '^web/src/features/today/', pathNot: '^web/src/features/today/viewport-dispatch\\.tsx$' }, to: { path: '^web/src/ui/viewport/' } },
    { name: 'not-to-unresolvable', severity: 'error', from: {}, to: { couldNotResolve: true } },
    { name: 'no-circular', severity: 'error', from: {}, to: { circular: true } },
  ],
  options: {
    tsPreCompilationDeps: true,
    doNotFollow: { path: 'node_modules' },
    exclude: { path: 'tools/architecture/fixtures' },
    tsConfig: { fileName: 'tsconfig.app.json' },
    enhancedResolveOptions: { exportsFields: ['exports'], conditionNames: ['import', 'types', 'default'] },
    reporterOptions: { dot: { collapsePattern: 'node_modules/[^/]+' } },
  },
};
