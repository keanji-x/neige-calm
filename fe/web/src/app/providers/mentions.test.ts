import { expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { mentionSearchOf } from './mentions.ts';

it('references disabled plugin documentation using only catalog reads', async () => {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path === '/api/plugins' ? [{
      id: 'dev.example', version: '1', enabled: false, state: 'disabled',
      manifest_name: 'development', manifest_description: 'Develop issues and publish changes.',
      has_config: false, can_uninstall: false,
    }] : { tags: [], tracks: [], blocks: [] } });
  } };
  const search = mentionSearchOf(transport, createUnauthorizedChannel({ enqueue: task => task() }), 'area', 'track');
  const suggestions = await search('development', new AbortController().signal);
  expect(suggestions).toHaveLength(1);
  expect(suggestions[0]).toMatchObject({ kind: 'plugin', chip: 'development' });
  expect(suggestions[0].insert).toContain('Develop issues and publish changes.');
  expect(suggestions[0].insert).toContain('documentation only');
  expect(requests.every(request => request.method === 'GET')).toBe(true);
  expect(requests.map(request => request.path)).toContain('/api/plugins');
});

it('keeps successful report candidates when the plugin catalog fails', async () => {
  const transport: ApiTransportPort = { send(request) {
    return Promise.resolve(request.path === '/api/plugins'
      ? { status: 500, statusText: 'Internal error', body: { error: 'catalog failure' } }
      : { status: 200, statusText: 'OK', body: { tags: [], blocks: [], tracks: [
        { label: 'Report', track_id: 'report', insert: '@`area/reports/Report.md`' },
      ] } });
  } };
  const search = mentionSearchOf(transport, createUnauthorizedChannel({ enqueue: task => task() }), 'area', 'track');
  expect((await search('Report', new AbortController().signal)).map(item => item.label)).toEqual(['Report']);
});

it.each([
  ['plugin-only', null, 'Report'],
  ['report-only', 'area', '/Report'],
] as const)('preserves %s errors when its only actual source fails', async (_name, areaId, typed) => {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    return Promise.resolve({ status: 500, statusText: 'Internal error', body: { error: 'source failure' } });
  } };
  const search = mentionSearchOf(transport, createUnauthorizedChannel({ enqueue: task => task() }), areaId, null);
  await expect(search(typed, new AbortController().signal)).rejects.toThrow('source failure');
  expect(requests).toHaveLength(1);
});

it.each(['+development', '＋development'])('filters %s through the plugin catalog alone', async (typed) => {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path === '/api/plugins' ? [{
      id: 'dev.example', version: '1', enabled: false, state: 'disabled',
      manifest_name: 'development', manifest_description: 'Develop issues and publish changes.',
      has_config: false, can_uninstall: false,
    }] : { tags: [], tracks: [], blocks: [] } });
  } };
  const search = mentionSearchOf(transport, createUnauthorizedChannel({ enqueue: task => task() }), 'area', 'track');
  const suggestions = await search(typed, new AbortController().signal);
  expect(suggestions.map(item => item.kind)).toEqual(['plugin']);
  expect(suggestions[0].insert).toContain('documentation only');
  expect(requests.map(request => [request.method, request.path])).toEqual([['GET', '/api/plugins']]);
});
