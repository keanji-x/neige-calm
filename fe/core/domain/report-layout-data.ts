import {
  layoutLiveDataSchema, type LayoutChart, type LayoutColumn, type LayoutItem,
  type LayoutRow, type LayoutSelector, type LayoutTable,
} from './report-layout.js';

export type LayoutData = { kind: 'ready'; rows: LayoutRow[]; allRows: LayoutRow[]; caption: string }
  | { kind: 'unavailable'; message: string; caption: string };

function matches(row: LayoutRow, selector: LayoutSelector) {
  return Object.hasOwn(row, selector.key) && row[selector.key] === selector.value;
}
function field(row: LayoutRow, key: string) {
  return Object.hasOwn(row, key) ? row[key] : undefined;
}
function nonnegative(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

/** The caller supplies the current Track's existing overlay resolver. No IO. */
export function resolveLayoutData(item: LayoutItem, resolveLive?: (source: string) => unknown): LayoutData {
  let caption = '';
  try {
    let rows: LayoutRow[];
    if ('rows' in item.data) rows = item.data.rows;
    else {
      const raw = resolveLive?.(item.data.source);
      if (raw === undefined) return { kind: 'unavailable', message: '等待数据来源更新。', caption };
      if (raw !== null && typeof raw === 'object' && 'caption' in raw && typeof raw.caption === 'string') caption = raw.caption;
      const parsed = layoutLiveDataSchema.parse(raw);
      rows = parsed.rows;
      const extra = item.data.annotations;
      if (extra !== undefined) {
        const byKey = new Map(extra.rows.map(entry => [JSON.stringify(extra.keys.map(key => field(entry, key))), entry]));
        const seen = new Set<string>();
        rows = rows.map(entry => {
          const keys = extra.keys.map(key => field(entry, key));
          if (keys.some(value => value === undefined || value === null)) return entry;
          const tuple = JSON.stringify(keys);
          if (seen.has(tuple)) throw new Error('来源含重复的关联键。');
          seen.add(tuple);
          const annotation = byKey.get(tuple);
          if (annotation === undefined) return entry;
          if (Object.keys(annotation).some(key => !extra.keys.includes(key) && Object.hasOwn(entry, key))) {
            throw new Error('补充记录不能覆盖来源字段。');
          }
          // Object spread defines own properties; __proto__ is ordinary data.
          return { ...entry, ...annotation };
        });
      }
    }
    return { kind: 'ready', allRows: rows, rows: item.exclude === undefined ? rows : rows.filter(row => !matches(row, item.exclude!)), caption };
  } catch (error) {
    return { kind: 'unavailable', message: error instanceof Error ? `数据不可用：${error.message}` : '数据格式无效。', caption };
  }
}

export type ChartObservation = { x: number | string; value: number | null };
export type LayoutChartData = { kind: 'ready'; points: ChartObservation[]; unit: string }
  | { kind: 'unavailable'; message: string };

export function layoutChartData(item: LayoutChart, data: LayoutData): LayoutChartData {
  if (data.kind !== 'ready') return data;
  const unit = item.unit;
  if (unit?.row !== undefined) {
    const summary = data.allRows.filter(row => matches(row, unit.row!));
    if (summary.length !== 1 || field(summary[0], unit.key) !== unit.equals) {
      return { kind: 'unavailable', message: '来源计价单位与图表设置不一致。' };
    }
  }
  const points: ChartObservation[] = [];
  for (const row of data.rows) {
    const rawX = field(row, item.x);
    const rawY = field(row, item.y);
    const matchingUnit = unit === undefined || unit.row !== undefined || field(row, unit.key) === unit.equals;
    if (item.chart === 'line') {
      const x = typeof rawX === 'number' ? rawX : typeof rawX === 'string' ? Date.parse(rawX) : NaN;
      if (!Number.isFinite(x) || !Number.isFinite(new Date(x).getTime())) return { kind: 'unavailable', message: '时间字段缺失或无效。' };
      points.push({ x, value: matchingUnit && typeof rawY === 'number' && Number.isFinite(rawY) ? rawY : null });
    } else {
      if (!matchingUnit || rawX === null || rawX === undefined || !nonnegative(rawY)) {
        return { kind: 'unavailable', message: '等待完整数值与一致的计价单位，暂不显示占比。' };
      }
      points.push({ x: String(rawX), value: rawY });
    }
  }
  if (item.chart === 'line') points.sort((a, b) => Number(a.x) - Number(b.x));
  else if (!Number.isFinite(points.reduce((sum, point) => sum + point.value!, 0))) return { kind: 'unavailable', message: '数值总和超出可显示范围。' };
  return { kind: 'ready', points, unit: unit?.equals ?? '' };
}

/** Each share column independently proves a complete, matching denominator. */
export function layoutShareTotal(item: LayoutTable, column: LayoutColumn, data: LayoutData): number | null {
  if (data.kind !== 'ready' || item.total === undefined) return null;
  const candidates = data.allRows.filter(row => matches(row, item.total!.row));
  const total = candidates.length === 1 ? field(candidates[0], item.total.key) : null;
  const values = data.rows.map(row => field(row, column.key));
  if (!nonnegative(total) || total <= 0 || !values.every(nonnegative)) return null;
  const sum = values.reduce((value, next) => value + next, 0);
  return Number.isFinite(sum) && Math.abs(sum - total) <= .005 * (values.length + 1) + 1e-8 ? total : null;
}

export function layoutCell(column: LayoutColumn, row: LayoutRow, shareTotal: number | null): string {
  const raw = field(row, column.key);
  const value = raw ?? (column.fallbackKey === undefined ? null : field(row, column.fallbackKey));
  if (value === undefined || value === null) return '—';
  let result: string;
  if (column.format === 'text') result = String(value);
  else {
    // Display fallbacks never supply numeric/valuation data.
    if (typeof raw !== 'number' || !Number.isFinite(raw)) return '—';
    if (column.format === 'share' && shareTotal === null) return '—';
    const number = column.format === 'share' ? raw / shareTotal! * 100 : raw;
    if (!Number.isFinite(number)) return '—';
    result = number.toLocaleString('zh-CN', { minimumFractionDigits: column.digits, maximumFractionDigits: column.digits });
    if (column.format === 'percent' || column.format === 'share') result += '%';
  }
  const suffix = column.suffixKey === undefined ? null : field(row, column.suffixKey);
  return suffix === undefined || suffix === null || suffix === '' ? result : `${result} ${String(suffix)}`;
}

export function layoutCellTrack(column: LayoutColumn, row: LayoutRow): string | null {
  const value = column.linkKey === undefined ? null : field(row, column.linkKey);
  return typeof value === 'string' && value.trim() !== '' ? value : null;
}
