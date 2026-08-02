import { readdirSync } from 'node:fs';
import { extname, resolve } from 'node:path';

const source = resolve(process.argv[2] ?? 'web/src');
const forbidden = readdirSync(source).filter((entry) => ['.ts', '.tsx', '.js', '.jsx'].includes(extname(entry)) && entry !== 'main.tsx');
if (forbidden.length) {
  console.error(`top-level-only-main: forbidden source files: ${forbidden.join(', ')}`);
  process.exitCode = 1;
}
