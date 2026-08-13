import { readdirSync, readFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';
import postcss from 'postcss';

const root = resolve(process.argv[2] ?? '.');
const webRoot = resolve(root, 'web');
const sourceRoot = resolve(webRoot, 'src');
const declaration = readFileSync(resolve(sourceRoot, 'styles/breakpoints.ts'), 'utf8')
  .match(/export const RAIL_COLLAPSE_REM = (\d+(?:\.\d+)?);/);
if (!declaration) throw new Error('Could not read RAIL_COLLAPSE_REM from web/src/styles/breakpoints.ts');
const expected = Number(declaration[1]);

/** @param {string} directory @returns {string[]} */
function cssFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return ['dist', 'node_modules'].includes(entry.name) ? [] : cssFiles(path);
    return entry.isFile() && entry.name.endsWith('.css') ? [path] : [];
  });
}

/** @param {string} source @param {string} [path] @returns {string[]} */
export function breakpointMismatches(source, path = '<css>') {
  /** @type {string[]} */
  const mismatches = [];
  const ast = postcss.parse(source, { from: path });
  ast.walkAtRules(/^(media|container)$/i, (rule) => {
    const params = rule.params.replace(/(['"])(?:\\.|(?!\1).)*\1/g, '');
    for (const match of params.matchAll(/(\d+(?:\.\d+)?)([a-z%]+)/gi)) {
      if (Number(match[1]) !== expected || match[2].toLowerCase() !== 'rem') {
        mismatches.push(`${path}: ${match[1]}${match[2]} (expected ${expected}rem)`);
      }
    }
  });
  return mismatches;
}

const mismatches = cssFiles(webRoot).flatMap((path) => breakpointMismatches(readFileSync(path, 'utf8'), relative(root, path)));
if (mismatches.length > 0) {
  console.error(`breakpoint-literals: ${mismatches.join('\n')}`);
  process.exitCode = 1;
}
