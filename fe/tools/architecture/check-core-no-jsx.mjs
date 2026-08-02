import { readdirSync } from 'node:fs';
import { extname, resolve } from 'node:path';

export function checkCoreNoJsx(corePath = 'core') {
  const core = resolve(corePath);
  const jsx = readdirSync(core, { recursive: true }).filter((entry) => ['.tsx', '.jsx'].includes(extname(String(entry))));
  return jsx.length ? `core-no-jsx: forbidden JSX files: ${jsx.join(', ')}` : '';
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const error = checkCoreNoJsx(process.argv[2]);
  if (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
