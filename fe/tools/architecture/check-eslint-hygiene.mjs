import { readdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const root = resolve('.');
const configs = readdirSync(root, { recursive: true }).map(String).filter((path) => !path.startsWith('node_modules/') && /(^|\/)eslint\.config\.(js|cjs|mjs|ts)$/.test(path));
const nested = configs.filter((path) => !/^eslint\.config\.(js|cjs|mjs|ts)$/.test(path));
if (nested.length) {
  console.error(`eslint-config-root-only: nested configs: ${nested.join(', ')}`);
  process.exitCode = 1;
}

const configLines = readFileSync(resolve(root, 'eslint.config.js'), 'utf8').split('\n');
const unexplainedOff = configLines.findIndex((line, index) => /['"]off['"]/.test(line) && !/^\s*\/\/ Reason:/.test(configLines[index - 1] ?? ''));
if (unexplainedOff >= 0) {
  console.error(`eslint-no-off-shims: unexplained off rule at eslint.config.js:${unexplainedOff + 1}`);
  process.exitCode = 1;
}
