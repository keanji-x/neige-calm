import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const { generateMockFiles } = await import(new URL('./generator.ts', import.meta.url).href);

const feRoot = resolve(import.meta.dirname, '../..');
const repositoryRoot = resolve(feRoot, '..');
const openApiPath = resolve(repositoryRoot, 'web/src/api/openapi.json');
if (!existsSync(openApiPath)) throw new Error(`mock generation input is missing: ${openApiPath}; restore or relocate the legacy OpenAPI document and update tools/mock/generate.mjs plus check-drift.mjs`);
const openApi = JSON.parse(readFileSync(openApiPath, 'utf8'));
const wireSource = ['core/api/generated/wire.ts', 'tools/mock/response-wire.ts']
  .map((path) => readFileSync(resolve(feRoot, path), 'utf8')).join('\n');
const outputRoot = resolve(feRoot, 'mock/generated');
mkdirSync(outputRoot, { recursive: true });
for (const file of generateMockFiles(openApi, wireSource)) writeFileSync(resolve(outputRoot, file.path), file.content);
