import { afterEach, expect, it, vi } from 'vitest';
import { areaCreationCapabilityOperation, createAreaOperation } from '../../../../core/domain/area.ts';
import { createTrackOperation, updateTrackOperation, trackDetailOperation } from '../../../../core/domain/track.ts';
import { sendPlannerInputOperation } from '../../../../core/domain/conversation.ts';
import { runOperation } from '../providers/queries.ts';
import { createOnboardingScenario } from './scenario.ts';

afterEach(() => vi.restoreAllMocks());

it('runs create, preview, reopen and the prerecorded revision entirely in memory', async () => {
  const fetch = vi.spyOn(globalThis, 'fetch');
  const changed = vi.fn();
  const lesson = createOnboardingScenario(changed);
  expect(await runOperation(lesson.transport, areaCreationCapabilityOperation(), undefined)).toBe('supported');
  const area = await runOperation(lesson.transport, createAreaOperation({ name: '练习', color: '#6574cd' }, 'area-key'), undefined);
  const request = createTrackOperation({ area_id: area.id, planner_provider: 'codex',
    theme: { fg: [0, 0, 0], bg: [255, 255, 255] }, first_message: '生成页面' }, 'track-key');
  const track = await runOperation(lesson.transport, request, undefined);
  expect(await runOperation(lesson.transport, request, undefined)).toEqual(track);
  const files = lesson.workspaceFiles(track.id);
  expect((await files.readFile('index.html')).text).toContain('<h1>我的作品集</h1>');
  expect(files.rawUrl('index.html')).toMatch(/^data:text\/html/);
  const detail = await runOperation(lesson.transport, trackDetailOperation(track.id), undefined);
  const planner = detail.cards.find(card => card.kind === 'codex')!;
  expect(detail.can_reopen).toBe(true);
  await runOperation(lesson.transport, updateTrackOperation(track.id, { closed: false }), undefined);
  await runOperation(lesson.transport, sendPlannerInputOperation(planner.id, '改标题', [], 'followup-key', null), undefined);
  expect((await files.readFile('index.html')).text).toContain('<h1>我的第一个作品</h1>');
  const final = await runOperation(lesson.transport, trackDetailOperation(track.id), undefined);
  expect(JSON.stringify(final.cards)).toContain('没有调用 AI');
  expect(changed).toHaveBeenCalledTimes(4);
  expect(fetch).not.toHaveBeenCalled();
  fetch.mockRestore();
});

it('refuses unsupported routes and foreign files instead of contacting a service', async () => {
  const lesson = createOnboardingScenario();
  const response = await lesson.transport.send({ method: 'POST', path: '/api/cards/real-card/planner/input',
    credentials: 'include', body: { text: 'Run on a real project' } });
  expect(response.status).toBeGreaterThanOrEqual(400);
  expect((await lesson.transport.send({ method: 'GET', path: 'https://example.com/api/areas', credentials: 'include' })).status).toBe(403);
  await expect(lesson.workspaceFiles('real-track').readFile('/etc/passwd')).rejects.toThrow();
});

it('does not share areas or preferences between new lessons', async () => {
  const first = createOnboardingScenario(); const second = createOnboardingScenario();
  await runOperation(first.transport, createAreaOperation({ name: 'Only in the first lesson', color: '#6574cd' }, 'key'), undefined);
  const result = await second.transport.send({ method: 'GET', path: '/api/areas', credentials: 'include' });
  expect(JSON.stringify(result.body)).not.toContain('Only in the first lesson');
});
