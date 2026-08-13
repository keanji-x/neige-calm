import { readdirSync, readFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';

const root = resolve(process.argv[2] ?? '.');
const webRoot = resolve(root, 'web');
const sourceRoot = resolve(webRoot, 'src');
const breakpointSource = readFileSync(resolve(sourceRoot, 'styles/breakpoints.ts'), 'utf8');
const declaration = breakpointSource.match(/export const RAIL_COLLAPSE_REM = (\d+(?:\.\d+)?);/);

if (!declaration) throw new Error('Could not read RAIL_COLLAPSE_REM from web/src/styles/breakpoints.ts');

const expected = Number(declaration[1]);
const mediaQuery = /@media\s+([^{}]+)\{/gi;
const widthForms = Object.freeze([
  /\(\s*(?:min|max)-width\s*:\s*(\d+(?:\.\d+)?)([a-z%]+)\s*\)/gi,
  /\(\s*width\s*[<>]=?\s*(\d+(?:\.\d+)?)([a-z%]+)\s*\)/gi,
  /\(\s*(\d+(?:\.\d+)?)([a-z%]+)\s*[<>]=?\s*width\s*\)/gi,
]);

/** @param {string} directory @returns {string[]} */
function cssFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return entry.name === 'dist' || entry.name === 'node_modules' ? [] : cssFiles(path);
    return entry.isFile() && entry.name.endsWith('.css') ? [path] : [];
  });
}

const mismatches = cssFiles(webRoot).flatMap((path) => [...readFileSync(path, 'utf8').matchAll(mediaQuery)]
  .flatMap((query) => widthForms.flatMap((form) => [...query[1].matchAll(form)]))
  .filter((match) => Number(match[1]) !== expected || match[2].toLowerCase() !== 'rem')
  .map((match) => `${relative(root, path)}: ${match[1]}${match[2]} (expected ${expected}rem)`));

if (mismatches.length > 0) {
  console.error(`breakpoint-literals: ${mismatches.join('\n')}`);
  process.exitCode = 1;
}
