/** @type {import('dependency-cruiser').IConfiguration} */
module.exports = {
  forbidden: [
    { name: 'core-no-web-layers', severity: 'error', from: { path: '^core/' }, to: { path: '^web/src/(app|features|systems|ui)/' } },
    { name: 'ui-only-core-type-whitelist', severity: 'error', from: { path: '^web/src/ui/' }, to: { path: '^(web/src/(app|features|systems)/|core/(?!types/(ids|a11y)\\.ts$))' } },
    { name: 'systems-no-features-or-app', severity: 'error', from: { path: '^web/src/systems/' }, to: { path: '^web/src/(features|app)/' } },
    { name: 'features-no-app', severity: 'error', from: { path: '^web/src/features/' }, to: { path: '^web/src/app/' } },
    ...['wave', 'cove', 'today', 'report', 'spec', 'settings', 'auth'].flatMap((domain) => ({
      name: `features-${domain}-no-cross-domain`, severity: 'error',
      from: { path: `^web/src/features/${domain}/` },
      to: { path: `^web/src/features/(?!${domain}/)` },
    })),
    { name: 'core-no-react', severity: 'error', from: { path: '^core/' }, to: { path: '(^|node_modules/)(react|react-dom)(/|$)' } },
    { name: 'no-barrel-index', severity: 'error', from: { path: '(^|/)index\\.(ts|tsx|js|jsx)$', pathNot: ['^web/src/systems/events/index\\.ts$'] }, to: { dependencyTypes: ['export'] } },
    { name: 'cards-public-entry-only', severity: 'error', from: { path: '^(core/|web/src/)' }, to: { path: '^web/src/systems/cards/(?!public\\.ts$)' } },
  ],
  options: {
    doNotFollow: { path: 'node_modules' },
    exclude: { path: 'tools/architecture/fixtures' },
    tsConfig: { fileName: 'tsconfig.app.json' },
    enhancedResolveOptions: { exportsFields: ['exports'], conditionNames: ['import', 'types', 'default'] },
    reporterOptions: { dot: { collapsePattern: 'node_modules/[^/]+' } },
  },
};
