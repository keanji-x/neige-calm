import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cardSchema, coveSchema, overlaySchema, waveSchema } from '../../core/api/schemas.ts';
import { DEV_MOCK_ROUTES, devMockCards, devMockCoves, devMockOverlays, devMockWaves } from './server.mjs';

const openapi = JSON.parse(readFileSync(resolve(import.meta.dirname, '../../../web/src/api/openapi.json'), 'utf8'));
const actual = new Set(Object.entries(openapi.paths).flatMap(([path, item]) =>
  Object.keys(item).filter((method) => ['get', 'post', 'put', 'patch', 'delete'].includes(method))
    .map((method) => `${method.toUpperCase()} ${path}`)));
const missing = DEV_MOCK_ROUTES.map(([method, path]) => `${method} ${path}`).filter((route) => !actual.has(route));
if (missing.length > 0) {
  console.error(`dev-mock-contract: routes absent from OpenAPI: ${missing.join(', ')}`);
  process.exitCode = 1;
}
/** @type {ReadonlyArray<readonly [string, { safeParse(value: unknown): { success: boolean, error?: { message: string } } }, unknown]>} */
const payloads = [
  ...devMockCoves.map((value) => /** @type {const} */ (['Cove', coveSchema, value])),
  ...devMockWaves.map((value) => /** @type {const} */ (['Wave', waveSchema, value])),
  ...devMockOverlays.map((value) => /** @type {const} */ (['Overlay', overlaySchema, value])),
  ...Object.values(devMockCards).flat().map((value) => /** @type {const} */ (['Card', cardSchema,
    { ...value, created_at: 0, updated_at: 0 }])),
];
for (const [name, schema, value] of payloads) {
  const result = schema.safeParse(value);
  if (!result.success) {
    console.error(`dev-mock-contract: invalid ${name} payload: ${result.error?.message ?? 'unknown schema error'}`);
    process.exitCode = 1;
  }
}
