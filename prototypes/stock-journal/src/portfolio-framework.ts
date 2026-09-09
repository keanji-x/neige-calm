import { z } from '../../../fe/node_modules/zod/index.js';
import type { ReportBlock } from '../../../fe/core/domain/report.ts';

const nonnegative = z.number().finite().nonnegative();
const date = z.string().refine(value => Number.isFinite(Date.parse(value)), 'Invalid date');
const quoteSchema = z.object({
  price: nonnegative.nullable(), previousClose: nonnegative.nullable(),
  currency: z.string().min(1), fxToBase: z.number().finite().positive().nullable(),
  asOf: date.nullable(), status: z.enum(['current', 'stale', 'unavailable']),
}).refine(value => value.status === 'unavailable' || (value.price !== null && value.asOf !== null),
  'Available quotes need a price and timestamp');

export const portfolioSnapshotSchema = z.object({
  schemaVersion: z.literal(1), source: z.enum(['demo', 'live']), asOf: date, currency: z.string().min(1), cash: nonnegative,
  reportedValues: z.record(z.string(), nonnegative.nullable()).nullable(),
  reportedTotal: nonnegative.nullable(),
  notes: z.array(z.string()),
  holdings: z.array(z.object({
    id: z.string().min(1), symbol: z.string().min(1), name: z.string().min(1), quantity: nonnegative,
    trackId: z.string().min(1).nullable(),
    nextEvent: z.object({ date, title: z.string().min(1) }).nullable(), quote: quoteSchema,
  })),
  history: z.array(z.object({ date, value: nonnegative.nullable() })),
  trades: z.array(z.object({
    id: z.string().min(1), symbol: z.string().min(1), name: z.string().min(1), date,
    trackId: z.string().min(1).nullable(), side: z.enum(['buy', 'sell']),
    quantity: z.number().finite().positive(), price: nonnegative, currency: z.string().min(1),
    fee: nonnegative, reason: z.string(),
  })),
}).superRefine((snapshot, context) => {
  for (const [key, values] of [
    ['holding IDs', snapshot.holdings.map(item => item.id)],
    ['holding symbols', snapshot.holdings.map(item => item.symbol)],
    ['trade IDs', snapshot.trades.map(item => item.id)],
    ['history dates', snapshot.history.map(item => item.date)],
  ] as const) if (new Set(values).size !== values.length) context.addIssue({ code: 'custom', message: `Duplicate ${key}` });
  snapshot.holdings.forEach(item => {
    if (item.quote.currency === snapshot.currency && item.quote.fxToBase !== null && item.quote.fxToBase !== 1) {
      context.addIssue({ code: 'custom', message: 'Same-currency conversion must equal 1' });
    }
  });
  if (snapshot.reportedValues !== null) {
    if (snapshot.cash !== 0) context.addIssue({ code: 'custom', message: 'Do not add external cash to a plugin-valued portfolio' });
    const ids = new Set(snapshot.holdings.map(item => item.id));
    if (Object.keys(snapshot.reportedValues).length !== ids.size || Object.keys(snapshot.reportedValues).some(id => !ids.has(id))) {
      context.addIssue({ code: 'custom', message: 'Reported values must cover exactly the holdings' });
    }
  } else if (snapshot.reportedTotal !== null) {
    context.addIssue({ code: 'custom', message: 'Reported totals require reported holding values' });
  }
});

export type PortfolioSnapshot = z.infer<typeof portfolioSnapshotSchema>;
export type QuoteUpdate = { symbol: string; quote: z.infer<typeof quoteSchema> };

/** Existing connectors normalize their read-only quote result at this seam. */
export function applyQuoteUpdates(snapshot: PortfolioSnapshot, updates: readonly QuoteUpdate[]): PortfolioSnapshot {
  if (snapshot.reportedValues !== null) throw new Error('Plugin-valued snapshots must refresh from plugin overlays');
  const known = new Set(snapshot.holdings.map(item => item.symbol));
  const quotes = new Map<string, z.infer<typeof quoteSchema>>();
  for (const update of updates) {
    if (!known.has(update.symbol)) throw new Error(`Unknown holding symbol: ${update.symbol}`);
    if (quotes.has(update.symbol)) throw new Error(`Duplicate quote: ${update.symbol}`);
    const quote = quoteSchema.parse(update.quote);
    const previous = snapshot.holdings.find(item => item.symbol === update.symbol)!.quote;
    if (quote.asOf !== null && previous.asOf !== null && Date.parse(quote.asOf) < Date.parse(previous.asOf)) {
      throw new Error(`Out-of-order quote: ${update.symbol}`);
    }
    quotes.set(update.symbol, quote);
  }
  return portfolioSnapshotSchema.parse({ ...snapshot,
    holdings: snapshot.holdings.map(item => ({ ...item, quote: quotes.get(item.symbol) ?? item.quote })),
  });
}

export function portfolioView(snapshot: PortfolioSnapshot) {
  const rows = snapshot.holdings.map(item => {
    const quote = item.quote;
    const rawValue = snapshot.reportedValues !== null ? snapshot.reportedValues[item.id]
      : quote.status === 'unavailable' || quote.price === null || quote.fxToBase === null
        ? null : item.quantity * quote.price * quote.fxToBase;
    const value = rawValue !== null && Number.isFinite(rawValue) ? rawValue : null;
    const dayChange = quote.status === 'unavailable' || quote.price === null || quote.previousClose === null || quote.previousClose <= 0
      ? null : (quote.price / quote.previousClose - 1) * 100;
    return { ...item, value, dayChange: dayChange !== null && Number.isFinite(dayChange) ? dayChange : null };
  });
  const sum = rows.every(item => item.value !== null)
    ? snapshot.reportedValues !== null ? snapshot.reportedTotal : rows.reduce((total, item) => total + item.value!, snapshot.cash)
    : null;
  const total = sum !== null && Number.isFinite(sum) ? sum : null;
  return {
    rows: rows.map(item => ({ ...item, weight: total !== null && total > 0 && item.value !== null ? item.value / total * 100 : null })),
    total, stale: rows.some(item => item.quote.status === 'stale'),
    history: [...snapshot.history].sort((a, b) => Date.parse(a.date) - Date.parse(b.date)),
    trades: [...snapshot.trades].sort((a, b) => Date.parse(b.date) - Date.parse(a.date)),
  };
}

function text(value: string) {
  return value.replace(/[\r\n]+/g, ' ').replaceAll('\\', '\\\\').replace(/[|[\]()*_`<>]/g, value => `\\${value}`);
}
function trackLabel(name: string, trackId: string | null) {
  return trackId === null ? text(name) : `[${text(name)}](neige://wave/${encodeURIComponent(trackId)})`;
}
function number(value: number | null, digits = 2) {
  return value === null ? '—' : value.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits });
}
const prose = (id: string, markdown: string): ReportBlock => ({ id, kind: 'prose', payload: { markdown } });

export function portfolioReportBlocks(snapshot: PortfolioSnapshot, chartSource: string): ReportBlock[] {
  if (!/^\/(?!\/)[^\\\u0000-\u001f\u007f]*$/.test(chartSource)) throw new Error('Chart source must be a same-origin path');
  const view = portfolioView(snapshot);
  const origin = snapshot.source === 'demo' ? '示例数据' : `Market data · ${text(snapshot.currency)} · 仅含插件登记资产`;
  const notices = [view.total === null ? '部分行情或汇率不可用，暂不显示完整估值与权重。' : '',
    view.stale ? '含过期行情，请注意各股票的报价时间。' : ''].filter(Boolean).join(' ');
  const holdings = view.rows.length === 0 ? '暂无持仓。录入持仓后，这里会显示报价、研究 Track 与下次事件。'
    : '| 股票 / 研究档案 | 现价 | 当日涨跌 | 仓位 | 下次事件 |\n| :--- | ---: | ---: | ---: | :--- |\n' + view.rows.map(item => {
      const price = item.quote.status === 'unavailable' ? null : item.quote.price;
      const change = item.dayChange === null ? '—' : `${item.dayChange > 0 ? '▲ +' : item.dayChange < 0 ? '▼ −' : '— '}${number(Math.abs(item.dayChange))}%`;
      const event = item.nextEvent === null ? '暂无事件' : `${text(item.nextEvent.date.slice(5, 10).replace('-', '.'))} · ${text(item.nextEvent.title)}`;
      const stale = item.quote.status === 'stale' ? '（旧行情）' : '';
      return `| ${trackLabel(item.name, item.trackId)} | ${number(price)} ${text(item.quote.currency)}${stale} | ${change} | ${item.weight === null ? '—' : `${number(item.weight, 1).replace(/\.0$/, '')}%`} | ${event} |`;
    }).join('\n');
  return [
    prose('portfolio-intro', `# 组合概览\n\n${text(snapshot.asOf.slice(0, 10))} · ${origin}${notices ? `\n\n${notices}` : ''}`),
    { id: 'portfolio-visuals', kind: 'app', payload: { src: chartSource, title: '组合概览图表', height: 510 } },
    prose('holdings-heading', `# 持仓明细\n\n${holdings}${snapshot.notes.length ? `\n\n${snapshot.notes.map(note => `> ${text(note)}`).join('\n>\n')}` : ''}`),
    prose('review-plan', `# 交易日志\n\n${portfolioTradeLog(view.trades)}`),
  ];
}

export function portfolioTradeLog(trades: PortfolioSnapshot['trades']): string {
  const ordered = [...trades].sort((a, b) => Date.parse(b.date) - Date.parse(a.date));
  return trades.length === 0 ? '暂无交易记录。这里只记录已发生的交易，不会执行下单。'
    : '| 日期 | 股票 | 操作 | 数量 / 成交价 | 当时的理由 |\n| :--- | :--- | :--- | ---: | :--- |\n' + ordered.map(item =>
      `| ${text(item.date.slice(0, 10))} | ${trackLabel(item.name, item.trackId)} | ${item.side === 'buy' ? '买入' : '卖出'} | ${item.quantity.toLocaleString('en-US', { maximumFractionDigits: 8 })} 股 / ${number(item.price)} ${text(item.currency)} | ${text(item.reason || '未记录理由')} |`).join('\n');
}
