import { readdirSync, readFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';

const root = resolve(process.argv[2] ?? '.');
const sourceRoot = resolve(root, 'web/src');
const breakpointSource = readFileSync(resolve(sourceRoot, 'styles/breakpoints.ts'), 'utf8');
const declaration = breakpointSource.match(/export const RAIL_COLLAPSE_REM = (\d+(?:\.\d+)?);/);

if (!declaration) throw new Error('Could not read RAIL_COLLAPSE_REM from web/src/styles/breakpoints.ts');

const expected = Number(declaration[1]);
const mediaWidth = /@media\s*\(\s*width\s*[<>]=?\s*(\d+(?:\.\d+)?)([a-z%]+)\s*\)/gi;

/** @param {string} directory @returns {string[]} */
function cssFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return cssFiles(path);
    return entry.isFile() && entry.name.endsWith('.css') ? [path] : [];
  });
}

const mismatches = cssFiles(sourceRoot).flatMap((path) =>
  [...readFileSync(path, 'utf8').matchAll(mediaWidth)]
    .filter((match) => Number(match[1]) !== expected || match[2].toLowerCase() !== 'rem')
    .map((match) => `${relative(root, path)}: ${match[1]}${match[2]} (expected ${expected}rem)`),
);

if (mismatches.length > 0) {
  console.error(`breakpoint-literals: ${mismatches.join('\n')}`);
  process.exitCode = 1;
}
