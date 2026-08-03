import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
const { generateMockFiles } = await import(new URL('./generator.ts', import.meta.url).href);

const feRoot = resolve(import.meta.dirname, '../..');
const repositoryRoot = resolve(feRoot, '..');
const openApi = JSON.parse(readFileSync(resolve(repositoryRoot, 'web/src/api/openapi.json'), 'utf8'));
const wireSource = readFileSync(resolve(feRoot, 'core/api/generated/wire.ts'), 'utf8');
const outputRoot = resolve(feRoot, 'mock/generated');
mkdirSync(outputRoot, { recursive: true });
for (const file of generateMockFiles(openApi, wireSource)) writeFileSync(resolve(outputRoot, file.path), file.content);
