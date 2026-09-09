import type { ApiTransportPort, ApiTransportResponse } from '../../../fe/core/api/types.ts';
import { createPreviewTracks, previewArea, sourceFiles } from './native-data';
import { portfolioOverlays } from './native-template';

/** Fixture responses only. This adapter never contacts an API or launches an agent. */
export function createInvestmentPreviewTransport(): ApiTransportPort {
  const tracks = createPreviewTracks();
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body: structuredClone(body) });
  const unavailable = (message: string): ApiTransportResponse => ({ status: 501, statusText: 'Preview only', body: { error: message, code: 'preview_only' } });
  return { async send(request) {
    if (request.method !== 'GET') return unavailable('这是只读投资研究预览，未连接真实执行器或数据库。');
    const url = new URL(request.path, 'http://preview.local');
    if (url.pathname === '/api/areas') return ok([previewArea]);
    if (url.pathname === '/api/areas/investments/tracks') return ok(tracks.map(item => item.track));
    if (url.pathname === '/api/settings') return ok({});
    if (url.pathname === '/api/today/launchpad') return ok(null);
    if (['/api/overlays', '/api/track-templates', '/api/track-recipes', '/api/plugins'].includes(url.pathname)) return ok([]);
    const match = /^\/api\/tracks\/([^/]+)(.*)$/.exec(url.pathname);
    if (match) {
      const item = tracks.find(candidate => candidate.track.id === match[1]);
      if (!item) return { status: 404, statusText: 'Not found', body: { error: '示例档案不存在。', code: 'not_found' } };
      const tail = match[2];
      if (tail === '') return ok({ track: item.track, can_resume: false, overlays: item.track.id === 'portfolio' ? portfolioOverlays() : [], cards: [{
        id: `report-${item.track.id}`, track_id: item.track.id, kind: 'track-report', title: null,
        sort: 0, deletable: false, created_at: item.track.created_at, updated_at: item.track.updated_at,
        payload: item.report,
      }] });
      if (tail === '/report') return ok({ ...item.report, taskDiagnostics: [] });
      if (tail === '/conversations') return ok([]);
      if (tail === '/backlinks') {
        const links = tracks.flatMap(source => source.report.blocks.flatMap(block => {
          if (block.kind !== 'prose') return [];
          const markdown = (block.payload as { markdown: string }).markdown;
          const targets = [...markdown.matchAll(/\[([^\]]+)\]\(neige:\/\/wave\/([^#)]+)(?:#([^)]+))?\)/g)];
          return targets.filter(target => target[2] === item.track.id).map(target => ({
            src_track_id: source.track.id, src_track_title: source.track.title, src_block_id: block.id,
            dst_block_id: target[3] ?? null, label: target[1], updated_at: source.track.updated_at,
          }));
        }));
        return ok({ backlinks: links, truncated: false, skipped_sources: 0 });
      }
      if (tail === '/workspace/readfile') {
        const path = url.searchParams.get('path') ?? '';
        const text = Object.hasOwn(sourceFiles, path) ? sourceFiles[path] : undefined;
        if (text !== undefined) return ok({ path, text, size: new TextEncoder().encode(text).length, truncated: false });
        return { status: 404, statusText: 'Not found', body: { error: '示例资料不存在。', code: 'not_found' } };
      }
    }
    return unavailable(`当前预览未提供此功能：${url.pathname}`);
  } };
}
