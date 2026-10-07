import { sha256 } from '@noble/hashes/sha256';
import { bytesToHex, utf8ToBytes } from '@noble/hashes/utils';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TrackSourceDetail } from '../../../../core/domain/report-source.ts';
import { trackReportLinkUrl, type TrackReport } from '../../../../core/domain/report.ts';

export function evidenceExamples(): TrackReport {
  const citation = '[表格来源](neige://source/src_0971fbde#q1)';
  const table = { columns: [{ key: 'where', label: '位置' }, { key: 'evidence', label: '来源' }],
    rows: [{ where: '插件生成的表格', evidence: citation }] };
  return { summary: '', body: '', blocks: [
    { id: 'sources', kind: 'prose', payload: { markdown:
      '# 来源与边界\n\n以下内容均为交互示例，不是真实研究资料。\n\n摘要层：[来源正文与引用高亮](neige://source/src_0971fbde#q1)。移入卡片后可以滚动长文，点击 “Open in workspace” 查看完整来源详情。\n\n也可以试试 [来源缺失](neige://source/src_deadbeef)、[无效链接](neige://source/src_bad#q0)、[锚点缺失](neige://source/src_0971fbde#q7) 和 [失败后重试](neige://source/src_fa11ed00)。\n\n## 跨报告引用\n\n悬停 [另一份报告](' + trackReportLinkUrl('reference-demo') + ')，再悬停其中的来源链接。那条来源属于另一份报告，内容会明确显示所属报告。',
    } },
    { id: 'source-table', kind: 'table', payload: table },
    { id: 'source-view', kind: 'view', payload: { version: 1, title: '插件视图中的来源', description: '这张表使用正式的插件视图组件。',
      snapshot: { id: 'preview-example', observedAt: 1791360000000, producedAt: 1791360000000 },
      rows: [{ id: 'row', title: '', layout: 'one', cells: [{ id: 'table', kind: 'table', title: '', table: {
        ...table, rows: [{ where: '原生插件视图', evidence: '[视图来源](neige://source/src_0971fbde#q1)' }],
      } }] }],
    } },
  ] };
}

function referencedReport(): TrackReport {
  return { summary: '', body: '', blocks: [{ id: 'evidence', kind: 'prose', payload: { markdown:
    '# 另一份报告\n\n这也是示例内容。它与主报告使用相同的来源 ID，但保存了不同的正文。\n\n[另一报告的来源](neige://source/src_0971fbde#q1)',
  } }] };
}

function sourceExample(sourceId: string, trackId: string): TrackSourceDetail {
  const quote = '悬浮卡片可以读取来源正文，并保留引用高亮。';
  const body = (trackId === 'reference-demo'
    ? '这条来源属于另一份报告（reference-demo），不是主报告的来源。\n\n'
    : '来源正文预览示例：这是主报告保存的示例摘要。\n\n') + quote + '\n\n'
    + Array.from({ length: 20 }, (_, index) => `阅读段落 ${index + 1}：在悬浮卡片中滚动，原报告不会跟着移动。这里仅展示交互效果，不包含真实研究结论。`).join('\n\n');
  const bytes = utf8ToBytes(body);
  const start = utf8ToBytes(body.slice(0, body.indexOf(quote))).length;
  return { source_id: sourceId, title: trackId === 'reference-demo' ? '另一报告保存的来源 · 示例' : '来源正文预览 · 示例摘要',
    provenance: 'summary', origin: { kind: 'plugin', plugin_id: 'preview-example', tool: 'read_example',
      args_sha256: bytesToHex(sha256(utf8ToBytes(trackId))), args_canon: 'v1' },
    published_at: '2026-10-07', captured_at: '2026-10-07T08:00:00Z', body,
    body_bytes: bytes.length, body_sha256: bytesToHex(sha256(bytes)),
    quotes: [{ id: 'q1', text: quote, start, end: start + utf8ToBytes(quote).length }],
  };
}

/** Sample responses only. Rendering, queries, decoding, retry and source panels
 * are the production implementation; this transport performs no real writes. */
export function createEvidencePreviewTransport(): ApiTransportPort {
  let failureShown = false;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  return Object.freeze({ send: async (request: ApiRequest) => {
    await new Promise(resolve => { setTimeout(resolve, 300); });
    if (request.method !== 'GET') return { status: 405, statusText: 'Method Not Allowed', body: { error: 'Read-only preview' } };
    if (request.path === '/api/tracks/reference-demo' || request.path === '/api/tracks/link-preview') {
      const trackId = request.path.slice('/api/tracks/'.length);
      const report = trackId === 'reference-demo' ? referencedReport() : evidenceExamples();
      return ok({ track: { id: trackId, title: trackId === 'reference-demo' ? '另一份报告' : '来源预览', area_id: 'examples',
        sort: 1, cwd: '/preview', created_at: 1, updated_at: 1 }, can_reopen: false, can_close: false, overlays: [],
        cards: [{ id: `report-${trackId}`, track_id: trackId, kind: 'track-report', title: null, sort: 1, deletable: false,
          created_at: 1, updated_at: 1, payload: { schemaVersion: 3, docRev: 1, ...report } }],
      });
    }
    const source = /^\/api\/tracks\/(link-preview|reference-demo)\/sources\/(src_[0-9a-f]{8})$/.exec(request.path);
    if (source !== null && (source[2] === 'src_0971fbde' || source[2] === 'src_fa11ed00')) {
      if (source[2] === 'src_fa11ed00' && !failureShown) {
        failureShown = true;
        return { status: 503, statusText: 'Service Unavailable', body: { error: '示例：首次读取失败，请点击 Retry 重试。' } };
      }
      return ok(sourceExample(source[2], source[1]));
    }
    return { status: 404, statusText: 'Not Found', body: { error: '示例来源不存在', code: 'not_found' } };
  } });
}
