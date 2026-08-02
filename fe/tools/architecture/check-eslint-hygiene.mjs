import { readdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

export function checkEslintHygiene(rootPath = '.') {
  const root = resolve(rootPath);
  const configs = readdirSync(root, { recursive: true }).map(String).filter((path) => !path.startsWith('node_modules/') && !path.startsWith('tools/architecture/fixtures/') && /(^|\/)eslint\.config\.(js|cjs|mjs|ts)$/.test(path));
  const nested = configs.filter((path) => !/^eslint\.config\.(js|cjs|mjs|ts)$/.test(path));
  const errors = nested.length ? [`eslint-config-root-only: nested configs: ${nested.join(', ')}`] : [];

  const configLines = readFileSync(resolve(root, 'eslint.config.js'), 'utf8').split('\n');
  const unexplainedOff = configLines.findIndex((line, index) => /['"]off['"]/.test(line) && !/^\s*\/\/ Reason:/.test(configLines[index - 1] ?? ''));
  if (unexplainedOff >= 0) errors.push(`eslint-no-off-shims: unexplained off rule at eslint.config.js:${unexplainedOff + 1}`);
  return errors;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const errors = checkEslintHygiene();
  for (const error of errors) console.error(error);
  if (errors.length) process.exitCode = 1;
}
