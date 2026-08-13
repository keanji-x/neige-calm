import { readdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { architecturePlugin } from './plugin.mjs';

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
  const enabledArchitectureRules = new Set();
  let registersArchitecturePlugin = false;
  for (const item of flatten(imported.default ?? [])) {
    const config = /** @type {{ files?: string[], rules?: Record<string, unknown>, plugins?: Record<string, unknown> }} */ (item);
    if (config.plugins?.architecture) registersArchitecturePlugin = true;
    for (const [ruleName, value] of Object.entries(config?.rules ?? {})) {
      const setting = Array.isArray(value) ? value[0] : value;
      const architectureRule = ruleName.startsWith('architecture/');
      if (architectureRule && (setting === 'error' || setting === 2)) enabledArchitectureRules.add(ruleName.slice(13));
      if (architectureRule && (setting === 'warn' || setting === 1)) {
        errors.push(`eslint-no-warn-shims: architecture rule must be error: ${ruleName}`);
      }
      if (architectureRule && (setting === 'off' || setting === 0)
        && !(config.files?.length && config.files.every((pattern) => pattern.includes('.test.') || pattern.includes('.contract.test.')))) {
        errors.push(`eslint-architecture-scope: architecture rule disabled outside test-only files: ${ruleName}`);
      }
      if ((setting === 'off' || setting === 0) && (architectureRule || declaresRule(source, ruleName))
        && !hasDocumentedReason(source, ruleName)) {
        errors.push(`eslint-no-off-shims: unexplained off rule ${ruleName}`);
      }
    }
  }
  if (!registersArchitecturePlugin) errors.push('eslint-architecture-registration: architecture plugin is not registered');
  for (const ruleName of Object.keys(architecturePlugin.rules ?? {})) {
    if (!enabledArchitectureRules.has(ruleName)) errors.push(`eslint-architecture-completeness: missing error rule architecture/${ruleName}`);
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
