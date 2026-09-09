import { z } from '../../../fe/node_modules/zod/index.js';
import { portfolioSnapshotSchema, type PortfolioSnapshot } from './portfolio-framework.ts';

const amount = z.number().finite().nonnegative();
const holdingRow = z.object({
  asset: z.string().min(1), venue: z.enum(['CRYPTO', 'US', 'HK', 'SH', 'SZ', 'CN']).nullable(),
  qty: amount.nullable(), price: amount.nullable(), currency: z.string().nullable(),
  value: amount.nullable(), rate: amount.nullable().optional(),
});
const table = z.object({ rows: z.array(holdingRow), caption: z.string() });
const historyTable = z.object({ rows: z.array(z.object({
  at: z.string().refine(value => Number.isFinite(Date.parse(value))),
  total: amount, currency: z.string().nullable(),
})), caption: z.string() });
const overlay = z.object({ plugin_id: z.string(), entity_kind: z.string(), entity_id: z.string(),
  kind: z.string(), payload: z.unknown(), updated_at: z.number().finite() });

export type PortfolioMetadata = {
  assets: Record<string, { name: string; trackId: string | null; nextEvent: { date: string; title: string } | null }>;
  trades: PortfolioSnapshot['trades'];
};
export type MarketRead = { kind: 'ready'; snapshot: PortfolioSnapshot }
  | { kind: 'waiting'; message: string } | { kind: 'invalid'; message: string };

/** Decode the current plugin's actual table-shaped overlay outputs. */
export function readMarketPortfolio(trackId: string, input: unknown, metadata: PortfolioMetadata, nowMs = Date.now()): MarketRead {
  try {
    const all = z.array(overlay).parse(input).filter(item => item.plugin_id === 'dev-neige-market'
      && item.entity_kind === 'track' && item.entity_id === trackId);
    const holdings = all.filter(item => item.kind === 'portfolio.holdings');
    const histories = all.filter(item => item.kind === 'portfolio.history');
    if (holdings.length === 0) return { kind: 'waiting', message: '等待 Market data 持仓快照。请在此 Track 登记持仓。' };
    if (holdings.length !== 1 || histories.length > 1) throw new Error('Duplicate market overlays');
    const source = holdings[0];
    const data = table.parse(source.payload);
    const totals = data.rows.filter(row => row.asset === 'Total' && row.venue === null);
    if (totals.length !== 1) throw new Error('Expected exactly one plugin Total row');
    const positions = data.rows.filter(row => row !== totals[0]);
    if (positions.length === 0 && totals[0].value === 0) return { kind: 'waiting', message: '此 Track 暂未登记持仓。' };
    const declaredCurrency = totals[0].currency;
    // A null Total has no usable denomination. Do not infer one from a label,
    // another holding, a previous snapshot, or a configured currency.
    if (declaredCurrency === null) return { kind: 'invalid', message: `插件当前无法确定完整计价币种。${data.caption}` };
    const history = histories.length ? historyTable.parse(histories[0].payload) : null;
    const asOf = new Date(source.updated_at).toISOString();
    const stale = nowMs - source.updated_at > 120_000;
    const values: Record<string, number | null> = {};
    const rows = positions.map(row => {
      if (row.venue === null || row.qty === null) throw new Error('Holding identity or quantity missing');
      if (row.value !== null && (row.price === null || row.currency === null)) throw new Error('Valued holding is missing its quote denomination');
      const symbol = `${row.venue}:${row.asset}`;
      if (Object.hasOwn(values, symbol)) throw new Error(`Duplicate holding ${symbol}`);
      values[symbol] = row.value;
      const extra = metadata.assets[symbol];
      return {
        id: symbol, symbol, name: extra?.name ?? symbol, quantity: row.qty,
        trackId: extra?.trackId ?? null, nextEvent: extra?.nextEvent ?? null,
        quote: { price: row.price, previousClose: null,
          currency: row.currency ?? '未知币种', fxToBase: row.rate ?? (row.currency === declaredCurrency ? 1 : null),
          asOf, status: row.price === null ? 'unavailable' : stale ? 'stale' : 'current' },
      };
    });
    if (rows.every(row => values[row.id] !== null) && totals[0].value !== null) {
      const sum = rows.reduce((total, row) => total + values[row.id]!, 0);
      // The producer rounds each row and the overall total independently.
      // Permit that cent-level rounding, not a materially different total.
      if (!Number.isFinite(sum) || Math.abs(sum - totals[0].value) > .005 * (rows.length + 1) + 1e-8) {
        throw new Error('Plugin total does not match its converted holding values');
      }
    }
    const gaps = history?.rows.filter(row => row.currency !== declaredCurrency).length ?? 0;
    const notes = [data.caption];
    if (gaps) notes.push(`有 ${gaps} 个历史点的币种不同或未知，曲线在这些位置断开。`);
    // No separate cash account is invented: all assets registered with this
    // plugin remain rows, including stablecoins. Its converted values win.
    const snapshot = portfolioSnapshotSchema.parse({ schemaVersion: 1, source: 'live', asOf,
      currency: declaredCurrency, cash: 0, reportedValues: values, reportedTotal: totals[0].value, notes,
      holdings: rows, trades: metadata.trades,
      history: history?.rows.map(row => ({ date: new Date(row.at).toISOString(), value: row.currency === declaredCurrency ? row.total : null })) ?? [],
    });
    return { kind: 'ready', snapshot };
  } catch (error) {
    return { kind: 'invalid', message: `Market data 数据暂不能读取：${error instanceof Error ? error.message : '格式错误'}` };
  }
}
