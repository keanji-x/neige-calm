import { readdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

/** @param {string} root @param {string} [relative] @returns {string[]} */
function findConfigs(root, relative = '') {
  const configs = [];
  for (const entry of readdirSync(resolve(root, relative), { withFileTypes: true })) {
    const path = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      if (entry.name !== 'node_modules' && path !== 'tools/architecture/fixtures') configs.push(...findConfigs(root, path));
    } else if (/(^|\/)eslint\.config\.(js|cjs|mjs|ts)$/.test(path)) {
      configs.push(path);
    }
  }
  return configs;
}

/** @param {unknown[]} items @returns {unknown[]} */
function flatten(items) {
  return items.flatMap((item) => Array.isArray(item) ? flatten(item) : [item]);
}

/** @param {string} source @param {string} ruleName */
function hasDocumentedReason(source, ruleName) {
  const escaped = ruleName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`//\\s*Reason:[^\\n]*\\n\\s*['"]${escaped}['"]\\s*:`).test(source);
}

/** @param {string} source @param {string} ruleName */
function declaresRule(source, ruleName) {
  const escaped = ruleName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`['"]${escaped}['"]\\s*:`).test(source);
}

export async function checkEslintHygiene(rootPath = '.') {
  const root = resolve(rootPath);
  const configs = findConfigs(root);
  const nested = configs.filter((path) => !/^eslint\.config\.(js|cjs|mjs|ts)$/.test(path));
  const errors = nested.length ? [`eslint-config-root-only: nested configs: ${nested.join(', ')}`] : [];
  const rootConfig = resolve(root, 'eslint.config.js');
  const source = readFileSync(rootConfig, 'utf8');
  const imported = await import(`${pathToFileURL(rootConfig).href}?hygiene=${Date.now()}`);
  for (const item of flatten(imported.default ?? [])) {
    const config = /** @type {{ rules?: Record<string, unknown> }} */ (item);
    for (const [ruleName, value] of Object.entries(config?.rules ?? {})) {
      const setting = Array.isArray(value) ? value[0] : value;
      if ((setting === 'off' || setting === 0) && declaresRule(source, ruleName) && !hasDocumentedReason(source, ruleName)) {
        errors.push(`eslint-no-off-shims: unexplained off rule ${ruleName}`);
      }
    }
  }
  if (/\.\.\.\s*tseslint\.configs\.disableTypeChecked/.test(source) && !/\/\/\s*Reason:[^\n]*\n\s*\.\.\.\s*tseslint\.configs\.disableTypeChecked/.test(source)) {
    errors.push('eslint-no-off-shims: unexplained disableTypeChecked preset');
  }
  return errors;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const errors = await checkEslintHygiene();
  for (const error of errors) console.error(error);
  if (errors.length) process.exitCode = 1;
}
