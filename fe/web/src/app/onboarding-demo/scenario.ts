import { z } from 'zod';
import type { AreaWire } from '../../../../core/domain/area.ts';
import type { CardWire, TrackWire } from '../../../../core/domain/track.ts';
import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { demoPortfolio } from './portfolio.ts';

const HOME_ID = 'demo-home';
const SYSTEM_ID = 'demo-system';

type DemoTrack = { wire: TrackWire; revision: number; messages: Array<Readonly<{ text: string; user: boolean }>> };

/** The entire lesson is browser memory. No transport, fetch or filesystem fallback is accepted. */
export function createOnboardingScenario(onChange: () => void = () => undefined) {
  const now = Date.now();
  let sequence = 0;
  const areas: AreaWire[] = [{ id: SYSTEM_ID, name: 'Demo', color: '#6574cd', sort: 0, kind: 'system',
    default_template_id: null, default_cwd: null, created_at: now, updated_at: now }];
  const tracks = new Map<string, DemoTrack>();
  const answers = new Map<string, Readonly<{ body: string; response: ApiTransportResponse }>>();
  const receipts = new Map<string, unknown>();
  const response = (body: unknown, status = 200): ApiTransportResponse => ({ status, statusText: status < 400 ? 'OK' : 'Demo action unavailable', body });
  const unavailable = () => response({ error: 'demo_only', message: 'Demo 只演示建项目、生成页面和继续修改；这项操作没有连接真实服务。' }, 403);
  const missing = () => response({ error: 'not_found', message: '演示中没有这项内容。' }, 404);
  const bad = () => response({ error: 'bad_request', message: '请先填写这一步所需的内容。' }, 400);
  const wire = (id: string, areaId: string, title: string): TrackWire => ({ id, area_id: areaId, title, sort: sequence,
    cwd: '/演示项目', pinned_at: null, closed_at: null, admin_granted_at: null, created_at: now, updated_at: now });
  tracks.set(HOME_ID, { wire: wire(HOME_ID, SYSTEM_ID, '开始入门演示'), revision: 0, messages: [] });
  const plannerId = (id: string) => `${id}-planner`;
  const trackForCard = (id: string) => [...tracks.values()].find(track => plannerId(track.wire.id) === id);
  const deliver = (track: DemoTrack, text: string) => {
    track.revision += 1;
    track.wire = { ...track.wire, updated_at: now + track.revision, closed_at: now + track.revision };
    track.messages.push({ text, user: true }, { user: false, text: track.revision === 1
      ? '演示回复：预置作品集页面已展示在 Report。这里没有调用模型或运行验证；接下来可以打开页面，再试一次改标题。'
      : '演示回复：已切换到预置的改标题结果。Demo 只有“生成页面”和“改标题”这两种结果，不会理解或执行其他指令。' });
  };
  const report = (track: DemoTrack): CardWire => ({
    id: `${track.wire.id}-report`, track_id: track.wire.id, kind: 'track-report', title: null, sort: -1,
    deletable: false, created_at: now, updated_at: now + track.revision,
    payload: { schemaVersion: 3, docRev: track.revision, summary: '', body: '', blocks: track.revision === 0
      ? [{ id: 'intro', rev: 1, kind: 'prose', payload: { markdown: '# 先在 Demo 里试一次\n\n跟着左侧提示，建一个练习项目。演示数据只保留在当前页面，随时可以重新开始。' } }]
      : [
        { id: 'result', rev: track.revision, kind: 'prose', payload: { markdown: `# 演示成果\n\n${track.revision === 1 ? '一个三张卡片的作品集页面已准备好。' : '主标题已切换为“我的第一个作品”。'}\n\n这是预置结果，没有调用 AI、修改真实文件或运行测试。点下方 Open 看页面；要继续练习，点“重新打开并继续”。` } },
        { id: 'page', rev: track.revision, kind: 'html', payload: { path: 'index.html', title: '作品集演示', height: 480 } },
        { id: 'demo-task', rev: track.revision, kind: 'task', payload: { key: 'demo-page', kind: 'codex', declared_by: 'user', ready: true,
          goal: '展示预置页面（演示任务）', acceptance: '这里只演示任务状态；未运行模型或测试。' } },
      ] },
  });
  const cards = (track: DemoTrack): CardWire[] => [report(track), {
    id: plannerId(track.wire.id), track_id: track.wire.id, kind: 'codex', title: '演示助手', sort: 1,
    payload: { planner_harness: true, planner_provider: 'codex' }, deletable: false, created_at: now, updated_at: now + track.revision,
  }];
  const read = (request: ApiRequest): ApiTransportResponse => {
    const url = new URL(request.path, 'https://demo.invalid');
    const path = url.pathname;
    if (path === '/api/areas') return response(areas);
    const areaTracks = /^\/api\/areas\/([^/]+)\/tracks$/.exec(path);
    if (areaTracks) return response([...tracks.values()].filter(track => track.wire.area_id === areaTracks[1]).map(track => track.wire));
    const trackMatch = /^\/api\/tracks\/([^/]+)(.*)$/.exec(path);
    if (trackMatch) {
      const track = tracks.get(trackMatch[1]); if (!track) return missing();
      const suffix = trackMatch[2];
      if (suffix === '') return response({ track: track.wire, cards: cards(track), overlays: [],
        can_reopen: track.wire.closed_at !== null, can_close: track.wire.closed_at === null, can_admin: false });
      if (suffix === '/report') return response({ taskDiagnostics: track.revision === 0 ? [] : [{
        blockId: 'demo-task', key: 'demo-page', schedulable: true, status: 'done', statusDetail: '预置演示结果', workerCardId: null,
      }] });
      if (suffix === '/tasks/demo-page/attempts' && track.revision > 0) {
        const attempt = { attempt_id: `${track.wire.id}-demo-attempt`, generation: 1, status: 'done',
          status_detail: '预置演示，没有运行模型或测试。', blocking_reason: null, worker_card_id: null,
          created_at_ms: now, finished_at_ms: now + track.revision };
        return response({ key: 'demo-page', current: attempt, attempts: [attempt] });
      }
      if (suffix === '/conversations' || suffix === '/previews') return response([]);
      if (suffix === '/backlinks') return response({ backlinks: [], truncated: false, skipped_sources: 0 });
      return unavailable();
    }
    const cardMatch = /^\/api\/cards\/([^/]+)\/(planner\/run|harness\/items)$/.exec(path);
    if (cardMatch) {
      const track = trackForCard(cardMatch[1]); if (!track) return missing();
      if (cardMatch[2] === 'planner/run') return response({ card_id: cardMatch[1], worker_session_id: `demo-session-${track.wire.id}`,
        phase: 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
        pending: [], manual_compact_supported: false, compact_outcome: null, context_usage_invalidated: false,
        attachments_supported: false, running_turn: null });
      const after = Number(url.searchParams.get('after_id') ?? 0);
      return response(track.messages.map((message, index) => ({ id: index + 1, worker_session_id: `demo-session-${track.wire.id}`,
        card_id: cardMatch[1], track_id: track.wire.id, thread_id: `demo-thread-${track.wire.id}`,
        turn_id: `demo-turn-${Math.floor(index / 2)}`, turn_error_text: null, item_uuid: `demo-item-${index}`,
        item_type: message.user ? 'userMessage' : 'agentMessage', method: 'item/completed',
        params: JSON.stringify({ item: message.user ? { content: [{ type: 'text', text: message.text }] } : { text: message.text } }),
        created_at_ms: now + index,
      })).filter(item => item.id > after));
    }
    if (path === '/api/today/daily') return response({ date: url.searchParams.get('date') ?? new Date().toISOString().slice(0, 10), time_zone: 'Asia/Shanghai', track_id: HOME_ID });
    if (path === '/api/today/report-changes') return response({ date: url.searchParams.get('date') ?? '', timezone: 'Asia/Shanghai', through_event_id: 0, next_cursor: null, changes: [] });
    if (path === '/api/read-receipts') return response({ baselineMs: now, receipts: [...receipts.values()] });
    if (path === '/api/settings') return response({});
    if (path === '/api/plugins' || path === '/api/overlays' || path === '/api/track-recipes') return response([]);
    if (path === '/api/track-templates') return response([{ id: 'small-change', title: 'Small change', description: '本地小改练习 · Demo 结果预置', availability: { status: 'ready' }, tasks: [] }]);
    if (path === '/api/track-templates/small-change') return response({ id: 'small-change', title: 'Small change', description: 'Demo 练习', instructions: '演示生成与修改页面。', body: '# Demo\n\n仅展示预置结果。' });
    if (path === '/api/track-templates/small-change/plugin-guides') return response([]);
    if (path === '/api/fs/listdir') return response({ path: '/演示项目', parent: null, entries: [] });
    const availability = { provider: 'codex', status: 'ready', reason: null, checked_at_ms: now, authentication_notice: null };
    if (path === '/api/agent-providers') return response([availability]);
    if (path === '/api/agent-providers/managed') return response([{ availability, capabilities: { managed_conversation: true,
      images: false, manual_compact: false, history_replacement: false, requires_ready_availability: true } }]);
    if (path === '/api/models') return response({ models: [], default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null },
      default_source: 'unknown', source: 'live', fetched_at_ms: now });
    if (path === '/api/version') return response({ areaCreateIdempotency: true, assistantProviders: true, conversationCreateModel: true, conversationSide: false,
      webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 1,
      dbInstanceId: 'onboarding-demo-session', databaseId: 'onboarding-demo' });
    return unavailable();
  };
  const write = (request: ApiRequest): ApiTransportResponse => {
    const path = new URL(request.path, 'https://demo.invalid').pathname;
    if (request.method === 'POST' && path === '/api/areas') {
      const parsed = z.object({ name: z.string().trim().min(1), color: z.string(), default_template_id: z.string().nullable().optional(), default_cwd: z.string().nullable().optional() }).safeParse(request.body);
      if (!parsed.success) return bad();
      const area: AreaWire = { id: `demo-area-${++sequence}`, ...parsed.data, sort: sequence, kind: 'user',
        default_template_id: parsed.data.default_template_id ?? null, default_cwd: parsed.data.default_cwd ?? null, created_at: now, updated_at: now };
      areas.push(area); onChange(); return response(area, 201);
    }
    if (request.method === 'POST' && path === '/api/tracks') {
      const parsed = z.object({ area_id: z.string(), first_message: z.string().min(1) }).safeParse(request.body);
      if (!parsed.success || !areas.some(area => area.id === parsed.data.area_id && area.kind === 'user')) return bad();
      const track: DemoTrack = { wire: wire(`demo-track-${++sequence}`, parsed.data.area_id, '作品集练习'), revision: 0, messages: [] };
      tracks.set(track.wire.id, track); deliver(track, parsed.data.first_message); onChange(); return response(track.wire, 201);
    }
    const trackPatch = /^\/api\/tracks\/([^/]+)$/.exec(path);
    if (request.method === 'PATCH' && trackPatch) {
      const track = tracks.get(trackPatch[1]); if (!track || track.wire.id === HOME_ID) return missing();
      const parsed = z.object({ closed: z.boolean().optional(), title: z.string().optional() }).strict().safeParse(request.body);
      if (!parsed.success) return unavailable();
      track.wire = { ...track.wire, ...(parsed.data.closed === undefined ? {} : { closed_at: parsed.data.closed ? now + track.revision : null }),
        ...(parsed.data.title === undefined ? {} : { title: parsed.data.title }) }; onChange(); return response(track.wire);
    }
    const input = /^\/api\/cards\/([^/]+)\/planner\/input$/.exec(path);
    if (request.method === 'POST' && input) {
      const track = trackForCard(input[1]); const parsed = z.object({ text: z.string().min(1) }).safeParse(request.body);
      if (!track || track.wire.id === HOME_ID || !parsed.success || track.wire.closed_at !== null) return bad();
      deliver(track, parsed.data.text);
      onChange();
      return response({ card_id: input[1], worker_session_id: `demo-session-${track.wire.id}`, entry_id: `demo-entry-${track.revision}` });
    }
    const receipt = /^\/api\/read-receipts\/(track|area)\/([^/]+)\/(read|unread)$/.exec(path);
    if (request.method === 'POST' && receipt) {
      const value = { kind: receipt[1], targetId: receipt[2], readThroughMs: now, manualUnread: receipt[3] === 'unread' };
      receipts.set(`${receipt[1]}:${receipt[2]}`, value); return response(value);
    }
    return unavailable();
  };
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    if (request.signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    if (!request.path.startsWith('/api/') || request.path.includes('#')) return unavailable();
    const key = request.headers?.['Idempotency-Key'];
    const identity = key === undefined ? null : `${request.method}:${request.path}:${key}`;
    const prior = identity === null ? undefined : answers.get(identity);
    const body = JSON.stringify(request.body ?? null);
    if (prior !== undefined) return prior.body === body ? prior.response
      : response({ error: 'idempotency_key_reused', message: '这个演示请求已经使用过，请重新开始这一步。' }, 409);
    const answer = request.method === 'GET' ? read(request) : write(request);
    if (identity !== null && answer.status < 400) answers.set(identity, { body, response: answer });
    return answer;
  } };
  const workspaceFiles = (trackId: string): WorkspaceFilePort => {
    const page = () => {
      const track = tracks.get(trackId);
      if (!track || track.revision === 0) throw new ApiError({ kind: 'http', status: 404, code: 'path_not_found', message: '演示页面尚未生成。' });
      return demoPortfolio(track.revision > 1);
    };
    const check = (path: string) => {
      if (path !== 'index.html') throw new ApiError({ kind: 'http', status: 404, code: 'path_not_found', message: 'Demo 只提供 index.html。' });
      return page();
    };
    return { readFile: path => Promise.resolve().then(() => { const text = check(path); return { path, size: new TextEncoder().encode(text).length, text, truncated: false }; }),
      rawUrl: path => `data:text/html;charset=utf-8,${encodeURIComponent(check(path))}` };
  };
  return { transport, workspaceFiles };
}
