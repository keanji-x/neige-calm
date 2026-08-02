/** @type {import('dependency-cruiser').IConfiguration} */
module.exports = {
  forbidden: [
    { name: 'core-no-web-layers', severity: 'error', from: { path: '^core/' }, to: { path: '^web/src/(app|features|systems|ui)/' } },
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
    { name: 'no-shared-directory', severity: 'error', from: {}, to: { path: '(^|/)shared(/|$)' } },
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
