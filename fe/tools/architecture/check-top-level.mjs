import { readdirSync } from 'node:fs';
import { extname, resolve } from 'node:path';

export function checkTopLevel(sourcePath = 'web/src') {
  const source = resolve(sourcePath);
  const forbidden = readdirSync(source).filter((entry) => ['.ts', '.tsx', '.js', '.jsx'].includes(extname(entry)) && entry !== 'main.tsx');
  return forbidden.length ? `top-level-only-main: forbidden source files: ${forbidden.join(', ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkTopLevel(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
