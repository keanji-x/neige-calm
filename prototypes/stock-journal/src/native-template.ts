// Demo fixtures instantiate the same Recipe and native layout renderer as the
// real app. This file is never imported by the production fe/ entry point.
import recipeBody from '../../../fe/web/src/features/report/recipe/examples/portfolio.md?raw';
import { parse } from '../../../fe/core/markdown/public.ts';
import { reportLayoutSchema } from '../../../fe/core/domain/report-layout.ts';
import type { ReportBlock } from '../../../fe/core/domain/report.ts';
import snapshot from './portfolio-snapshot.json';

export function portfolioTemplateBlocks(): ReportBlock[] {
  let section = 0;
  const parsed = parse(recipeBody);
  if (parsed.status !== 'ready') throw new Error('Invalid demo Recipe');
  return parsed.value.children.map((node, index): ReportBlock => {
    if (node.type === 'code' && node.language === 'neige-block' && node.meta === 'layout') {
      const payload = reportLayoutSchema.parse(JSON.parse(node.value));
      for (const item of payload.items) {
        if (item.kind !== 'table') continue;
        if ('source' in item.data && item.data.annotations) {
          item.data.annotations.rows = snapshot.holdings.map(holding => ({ venue: 'DEMO', asset: holding.name,
            name: holding.name, track: holding.trackId, nextEvent: `${holding.nextEvent.date} · ${holding.nextEvent.title}` }));
        } else if ('rows' in item.data) item.data.rows = snapshot.trades.map(trade => ({
          date: trade.date, name: trade.name, track: trade.trackId, side: trade.side === 'buy' ? '买入' : '卖出',
          quantity: trade.quantity, price: trade.price, currency: trade.currency, reason: trade.reason,
        }));
      }
      return { id: `layout-${index}`, kind: 'layout', payload };
    }
    const id = node.type === 'heading' ? ['portfolio-intro', 'holdings-heading', 'review-plan'][section++] : `intro-${index}`;
    return { id, kind: 'prose', payload: { markdown: recipeBody.slice(node.position.start.offset, node.position.end.offset) } };
  });
}

export function portfolioOverlays() {
  const rows = snapshot.holdings.map(holding => ({ asset: holding.name, venue: 'DEMO', qty: holding.quantity,
    price: holding.quote.price, currency: holding.quote.currency, value: holding.quantity * holding.quote.price,
    change: (holding.quote.price / holding.quote.previousClose - 1) * 100 }));
  const total = rows.reduce((sum, row) => sum + row.value, snapshot.cash);
  const holdings = { rows: [...rows,
    { asset: '现金', venue: 'DEMO', qty: snapshot.cash, price: 1, currency: 'CNY', value: snapshot.cash, change: null },
    { asset: 'Total', venue: null, qty: null, price: null, currency: 'CNY', value: total, change: null }], caption: '示例数据 · 非真实持仓与行情' };
  const history = { rows: snapshot.history.map(point => ({ at: point.date, total: point.value, currency: 'CNY' })), caption: '示例估值历史' };
  return [holdings, history].map((payload, index) => ({ id: `demo-overlay-${index}`, plugin_id: 'dev-neige-market', entity_kind: 'track',
    entity_id: 'portfolio', kind: index === 0 ? 'portfolio.holdings' : 'portfolio.history', payload, updated_at: Date.now() }));
}
