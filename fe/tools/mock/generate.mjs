import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const { generateMockFiles } = await import(new URL('./generator.ts', import.meta.url).href);

const feRoot = resolve(import.meta.dirname, '../..');
const openApiPath = resolve(feRoot, 'core/api/generated/openapi.json');
if (!existsSync(openApiPath)) throw new Error(`mock generation input is missing: ${openApiPath}; run npm run gen:api`);
const openApi = JSON.parse(readFileSync(openApiPath, 'utf8'));
const wireSource = readFileSync(resolve(feRoot, 'core/api/generated/wire.ts'), 'utf8');
const outputRoot = resolve(feRoot, 'mock/generated');
mkdirSync(outputRoot, { recursive: true });
for (const file of generateMockFiles(openApi, wireSource)) writeFileSync(resolve(outputRoot, file.path), file.content);
