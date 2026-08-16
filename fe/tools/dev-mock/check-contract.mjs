// @ts-nocheck -- executable contract probe uses minimal Connect request/response doubles.
import { EventEmitter } from 'node:events';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cardSchema, coveSchema, overlaySchema, waveSchema } from '../../core/api/schemas.ts';
import { DEV_MOCK_ROUTES, handleDevMockRequest } from './server.mjs';
import { DEV_MOCK_ROUTE_EXEMPTIONS } from './route-exemptions.mjs';

const openapi = JSON.parse(readFileSync(resolve(import.meta.dirname, '../../../web/src/api/openapi.json'), 'utf8'));
const serverSource = readFileSync(resolve(import.meta.dirname, 'server.mjs'), 'utf8');
const openApiRoutes = new Set(Object.entries(openapi.paths).flatMap(([path, item]) =>
  Object.keys(item).filter((method) => ['get', 'post', 'put', 'patch', 'delete'].includes(method))
    .map((method) => `${method.toUpperCase()} ${path}`)));
const declared = new Set(DEV_MOCK_ROUTES.map(([method, path]) => `${method} ${path}`));
const exempted = new Set(DEV_MOCK_ROUTE_EXEMPTIONS.map(({ route }) => route));
const inventoryPaths = new Set(DEV_MOCK_ROUTES.map(([, path]) => path));
const nonRoutePaths = new Set(['/api/', '/api/auth/whoami', '/api/auth/logout']);
const literalImplementationPaths = new Set(Array.from(serverSource.matchAll(/(['"])(\/api\/[^'"`$]*)\1/g), (match) => match[2]));
const wildImplementationPaths = [...literalImplementationPaths].filter((path) => !inventoryPaths.has(path) && !nonRoutePaths.has(path));
if (wildImplementationPaths.length) {
  console.error(`dev-mock-contract: implemented paths absent from inventory: ${wildImplementationPaths.join(', ')}`); process.exitCode = 1;
}
if (DEV_MOCK_ROUTE_EXEMPTIONS.some(({ reason, expiry }) => reason.trim() === '' || expiry < new Date().toISOString().slice(0, 10))
  || new Set(DEV_MOCK_ROUTE_EXEMPTIONS.map(({ reason }) => reason)).size !== DEV_MOCK_ROUTE_EXEMPTIONS.length
  || exempted.size !== DEV_MOCK_ROUTE_EXEMPTIONS.length) {
  console.error('dev-mock-contract: route exemptions require unique routes/reasons and live expiries'); process.exitCode = 1;
}
const absent = [...declared].filter((route) => !openApiRoutes.has(route));
if (absent.length) { console.error(`dev-mock-contract: routes absent from OpenAPI: ${absent.join(', ')}`); process.exitCode = 1; }
const unaccounted = [...openApiRoutes].filter((route) => !declared.has(route) && !exempted.has(route));
const staleExemptions = [...exempted].filter((route) => !openApiRoutes.has(route) || declared.has(route));
if (unaccounted.length || staleExemptions.length) {
  console.error(`dev-mock-contract: unaccounted OpenAPI routes: ${unaccounted.join(', ')}; stale exemptions: ${staleExemptions.join(', ')}`);
  process.exitCode = 1;
}

function concrete(path) {
  return path.replace('{cove_id}', 'cove-atlas').replace('{id}', path.startsWith('/api/coves/') ? 'cove-atlas' : 'w-1');
}
async function invoke(method, path, body = {}, headers = {}) {
  const req = new EventEmitter(); req.method = method; req.url = concrete(path); req.headers = headers;
  const result = await new Promise((resolveResult) => {
    const headers = {}; const chunks = [];
    const res = { statusCode: 0, setHeader: (key, value) => { headers[key] = value; }, end: (chunk = '') => {
      chunks.push(String(chunk)); resolveResult({ status: res.statusCode, body: chunks.join('') ? JSON.parse(chunks.join('')) : undefined });
    } };
    void handleDevMockRequest(req, res, () => resolveResult({ status: 0, body: undefined }));
    queueMicrotask(() => { req.emit('data', JSON.stringify(body)); req.emit('end'); });
  });
  return result;
}

// Probe every exempted operation. A newly implemented dispatch is discovered from behavior,
// so an unlisted handler cannot hide behind the inventory.
for (const route of exempted) {
  const [method, path] = route.split(' ');
  const response = await invoke(method, path);
  if (response.status !== 404 && !declared.has(route)) {
    console.error(`dev-mock-contract: implemented route absent from inventory: ${route}`); process.exitCode = 1;
  }
}

const checks = [
  ['GET', '/api/coves', coveSchema.strict().array()], ['GET', '/api/waves', waveSchema.strict().array()],
  ['GET', '/api/overlays', overlaySchema.strict().array()],
];
for (const [method, path, schema] of checks) {
  const response = await invoke(method, path);
  const expected = Number(Object.keys(openapi.paths[path][method.toLowerCase()].responses).find((status) => /^2\d\d$/.test(status)));
  if (response.status !== expected) { console.error(`dev-mock-contract: ${method} ${path} returned ${response.status}, expected ${expected}`); process.exitCode = 1; }
  const parsed = schema.safeParse(response.body);
  if (!parsed.success) { console.error(`dev-mock-contract: invalid response for ${method} ${path}: ${parsed.error.message}`); process.exitCode = 1; }
}
const detail = await invoke('GET', '/api/waves/{id}');
const detailChecks = [waveSchema.strict().safeParse(detail.body?.wave), cardSchema.strict().array().safeParse(detail.body?.cards),
  overlaySchema.strict().array().safeParse(detail.body?.overlays)];
if (detailChecks.some((result) => !result.success)) { console.error('dev-mock-contract: invalid real GET /api/waves/{id} response'); process.exitCode = 1; }
for (const [method, path] of [['POST', '/api/coves'], ['POST', '/api/waves']]) {
  const response = await invoke(method, path, path.endsWith('coves') ? { name: 'probe', color: '#000000' } : { cove_id: 'cove-atlas', title: 'probe' });
  const expected = Number(Object.keys(openapi.paths[path][method.toLowerCase()].responses).find((status) => /^2\d\d$/.test(status)));
  if (response.status !== expected) { console.error(`dev-mock-contract: ${method} ${path} returned ${response.status}, expected ${expected}`); process.exitCode = 1; }
}

const version = await invoke('GET', '/api/version');
if (version.status !== 200 || !Number.isInteger(version.body?.webCompatVersion)
  || !Number.isInteger(version.body?.minWebCompatVersion) || !Number.isInteger(version.body?.syncEventVersion)
  || typeof version.body?.dbInstanceId !== 'string') {
  console.error('dev-mock-contract: invalid GET /api/version response'); process.exitCode = 1;
}
for (const method of ['GET', 'PUT']) {
  const response = await invoke(method, '/api/settings', method === 'PUT' ? { settings: { http_proxy: 'http://probe' } } : {});
  if (response.status !== 200 || typeof response.body?.settings !== 'object') {
    console.error(`dev-mock-contract: invalid ${method} /api/settings response`); process.exitCode = 1;
  }
}
const coveWaves = await invoke('GET', '/api/coves/{cove_id}/waves');
if (coveWaves.status !== 200 || !waveSchema.strict().array().safeParse(coveWaves.body).success) {
  console.error('dev-mock-contract: invalid GET /api/coves/{cove_id}/waves response'); process.exitCode = 1;
}
const chatWaveCreated = await invoke('POST', '/api/coves/{cove_id}/chat-wave/ensure');
const chatWaveExisting = await invoke('POST', '/api/coves/{cove_id}/chat-wave/ensure');
if (chatWaveCreated.status !== 201 || chatWaveExisting.status !== 200
  || chatWaveCreated.body?.id !== chatWaveExisting.body?.id
  || chatWaveCreated.body?.purpose !== 'cove-chat'
  || !waveSchema.strict().safeParse(chatWaveCreated.body).success) {
  console.error('dev-mock-contract: invalid POST /api/coves/{cove_id}/chat-wave/ensure response'); process.exitCode = 1;
}
const conversationsEmpty = await invoke('GET', '/api/coves/{cove_id}/conversations');
const conversationCreated = await invoke('POST', '/api/coves/{cove_id}/conversations', { text: 'first message' }, { 'idempotency-key': 'probe-1' });
const conversationRetried = await invoke('POST', '/api/coves/{cove_id}/conversations', { text: 'first message' }, { 'idempotency-key': 'probe-1' });
const conversationsListed = await invoke('GET', '/api/coves/{cove_id}/conversations');
const conversationMissingKey = await invoke('POST', '/api/coves/{cove_id}/conversations', { text: 'first message' });
if (conversationsEmpty.status !== 200 || !Array.isArray(conversationsEmpty.body) || conversationsEmpty.body.length !== 0
  || conversationCreated.status !== 201 || conversationCreated.body?.kind !== 'shared-chat'
  || typeof conversationCreated.body?.waveId !== 'string' || conversationCreated.body?.title !== null
  || typeof conversationCreated.body?.updatedAt !== 'number'
  || 'idempotencyKey' in (conversationCreated.body ?? {})
  || conversationRetried.status !== 201 || conversationRetried.body?.id !== conversationCreated.body?.id
  || conversationsListed.body?.length !== 1
  || conversationsListed.body.some((row) => 'idempotencyKey' in row)
  || conversationMissingKey.status !== 400) {
  console.error('dev-mock-contract: invalid /api/coves/{cove_id}/conversations behaviour'); process.exitCode = 1;
}

for (const [method, path, body, schema] of [
  ['PATCH', '/api/coves/{id}', { name: 'atlas-probed' }, coveSchema.strict()],
  ['PATCH', '/api/waves/{id}', { title: 'wave-probed' }, waveSchema.strict()],
]) {
  const response = await invoke(method, path, body);
  if (response.status !== 200 || !schema.safeParse(response.body).success) {
    console.error(`dev-mock-contract: invalid ${method} ${path} response`); process.exitCode = 1;
  }
}
for (const path of ['/api/waves/{id}', '/api/coves/{id}']) {
  const response = await invoke('DELETE', path);
  if (response.status !== 204) { console.error(`dev-mock-contract: DELETE ${path} returned ${response.status}`); process.exitCode = 1; }
}
for (const path of ['/api/waves/missing-probe', '/api/coves/missing-probe']) {
  const response = await invoke('GET', path);
  if (response.status !== 404) { console.error(`dev-mock-contract: missing resource ${path} did not return 404`); process.exitCode = 1; }
}
const invalidCreate = await invoke('POST', '/api/coves', {});
if (invalidCreate.status !== 400) { console.error('dev-mock-contract: invalid POST /api/coves did not return 400'); process.exitCode = 1; }
