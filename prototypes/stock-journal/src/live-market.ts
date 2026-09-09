import { z } from '../../../fe/node_modules/zod/index.js';
import type { ApiTransportPort } from '../../../fe/core/api/types.ts';
import { trackDetailSchema } from '../../../fe/core/domain/track.ts';
import type { ReportBlock } from '../../../fe/core/domain/report.ts';
import { readFileWireSchema, readTrackWorkspaceFileOperation } from '../../../fe/core/domain/fs.ts';
import { portfolioReportBlocks, portfolioSnapshotSchema, portfolioTradeLog } from './portfolio-framework';
import { readMarketPortfolio, type MarketRead } from './market-adapter';
import { metadataIsMissing, PORTFOLIO_METADATA_PATH } from './portfolio-metadata';

const metadataSchema = z.object({
  assets: z.record(z.string(), z.object({ name: z.string().min(1), trackId: z.string().nullable(),
    nextEvent: z.object({ date: z.string(), title: z.string() }).nullable() })),
  trades: portfolioSnapshotSchema.shape.trades,
});

export function createLiveMarketTransport(base: ApiTransportPort, changed: (id: string, result: MarketRead) => void,
  selection: { current: string | null; save: (id: string) => void; clear: () => void }) {
  const snapshots = new Map<string, MarketRead>();
  let epoch = 0;
  let portfolioId = selection.current;
  const clear = () => { epoch += 1; snapshots.clear(); portfolioId = null; selection.clear(); };
  const transport: ApiTransportPort = { async send(request) {
    // Authentication is the only write this read-only portfolio viewer forwards.
    if (request.method !== 'GET' && !(request.method === 'POST'
      && ['/api/auth/login', '/api/auth/logout'].includes(request.path))) {
      return { status: 403, statusText: 'Read-only portfolio', body: { code: 'readonly_view', error: '当前是只读持仓视图。请在 Neige 原应用中登记持仓或修改记录。' } };
    }
    const started = epoch;
    const response = await base.send(request);
    if (started !== epoch) return { status: 401, statusText: 'Session changed', body: { error: 'Session changed', code: 'unauthorized' } };
    if (response.status === 401) { clear(); return response; }
    const match = /^\/api\/tracks\/([^/?]+)$/.exec(request.path);
    if (request.method !== 'GET' || !match || response.status !== 200) return response;
    const detail = trackDetailSchema.parse(response.body);
    const id = decodeURIComponent(match[1]);
    if (detail.track.id !== id) throw new Error('Track detail identity mismatch');
    if (portfolioId !== null && portfolioId !== id) return response;
    if (!detail.overlays.some(item => item.plugin_id === 'dev-neige-market' && item.entity_kind === 'track'
      && item.entity_id === id && ['portfolio.holdings', 'portfolio.history'].includes(item.kind))) {
      snapshots.delete(id);
      return response;
    }
    if (portfolioId === null) { portfolioId = id; selection.save(id); }
    const metadataResponse = await base.send({ ...request,
      path: readTrackWorkspaceFileOperation(id, PORTFOLIO_METADATA_PATH).path });
    if (started !== epoch) return { status: 401, statusText: 'Session changed', body: { error: 'Session changed', code: 'unauthorized' } };
    if (metadataResponse.status === 401) { clear(); return metadataResponse; }
    if (metadataResponse.status !== 200 && !metadataIsMissing(metadataResponse, detail.track.cwd)) return metadataResponse;
    let metadata: z.infer<typeof metadataSchema> = { assets: {}, trades: [] };
    if (metadataResponse.status === 200) {
      const file = readFileWireSchema.parse(metadataResponse.body);
      if (file.truncated) throw new Error('Portfolio metadata was truncated; records cannot be safely read');
      metadata = metadataSchema.parse(JSON.parse(file.text));
    }
    const result = readMarketPortfolio(id, detail.overlays, metadata);
    snapshots.set(id, result); changed(id, result);
    const blocks: ReportBlock[] = result.kind === 'ready' ? portfolioReportBlocks(result.snapshot, `/next/portfolio-demo.html?track=${encodeURIComponent(id)}`)
      : [
        { id: 'portfolio-intro', kind: 'prose', payload: { markdown: `# 组合概览\n\n${result.message}` } },
        { id: 'holdings-heading', kind: 'prose', payload: { markdown: '# 持仓明细' } },
        { id: 'holdings', kind: 'table', payload: { source: 'neige://plugin/dev-neige-market/portfolio.holdings' } },
        { id: 'review-plan', kind: 'prose', payload: { markdown: `# 交易日志\n\n${portfolioTradeLog(metadata.trades)}` } },
      ];
    const body = blocks.filter(block => block.kind === 'prose').map(block => (block.payload as { markdown: string }).markdown).join('\n\n');
    const saved = detail.cards.find(card => card.kind === 'track-report');
    const card = { id: saved?.id ?? `portfolio-view-${id}`, track_id: id, title: null, kind: 'track-report', sort: 0,
      deletable: false, created_at: detail.track.created_at, updated_at: detail.track.updated_at,
      payload: { schemaVersion: 3, docRev: 1, summary: 'Market data 持仓视图', body, blocks } };
    return { ...response, body: { ...detail, cards: [card], can_resume: false } };
  } };
  return { transport, snapshots, clear };
}
