import { readdirSync } from 'node:fs';
import { extname, resolve } from 'node:path';

const core = resolve(process.argv[2] ?? 'core');
const jsx = readdirSync(core, { recursive: true }).filter((entry) => ['.tsx', '.jsx'].includes(extname(String(entry))));
if (jsx.length) {
  console.error(`core-no-jsx: forbidden JSX files: ${jsx.join(', ')}`);
  process.exitCode = 1;
}
