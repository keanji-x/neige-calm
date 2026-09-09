// Recharts primitives in the restrained shadcn chart presentation: transparent
// responsive container, fine grid, no card chrome, and an explicit tooltip.
// This component knows no Report, source, portfolio, or application route.
import { useId } from 'react';
import { Area, AreaChart, CartesianGrid, Cell, Pie, PieChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';

import { useState } from '../state/public.ts';
import styles from './chart.module.css';

type Point = Readonly<{ x: number | string; value: number | null }>;
export type ChartProps = Readonly<{
  kind: 'line' | 'donut'; label: string; points: readonly Point[]; color: string; height: number; unit: string;
  ranges?: readonly number[]; defaultRange?: number;
}>;

function amount(value: number) {
  return value.toLocaleString('zh-CN', { maximumFractionDigits: 2, notation: 'compact' });
}
function dateLabel(value: number, intraday: boolean) {
  return new Date(value).toISOString().slice(intraday ? 11 : 5, intraday ? 16 : 10);
}

export function Chart({ kind, label, points, color, height, unit, ranges, defaultRange }: ChartProps) {
  const gradient = `fill-${useId().replace(/:/g, '')}`;
  const [range, setRange] = useState(defaultRange);
  const [selected, setSelected] = useState<number | null>(null);
  const latest = points.at(-1);
  const visible = kind === 'line' && range !== undefined && latest !== undefined
    ? points.filter(point => Number(point.x) >= Number(latest.x) - range * 86_400_000) : points;
  const total = points.reduce((sum, point) => sum + (point.value ?? 0), 0);
  const empty = kind === 'line' ? visible.filter(point => point.value !== null).length < 2 : total <= 0;
  const intraday = visible.length > 1 && Number(visible.at(-1)!.x) - Number(visible[0].x) <= 86_400_000;
  const money = (value: number) => `${value.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}${unit ? ` ${unit}` : ''}`;
  return <div className={styles.root}>
    <div className={styles.controls}>
      <span>{unit}{kind === 'line' && intraday ? ' · UTC' : ''}</span>
      {ranges !== undefined && <div className={styles.ranges} aria-label={`${label}时间范围`}>
        {ranges.map(days => <button key={days} type="button" aria-pressed={range === days} onClick={() => setRange(days)}>{days}天</button>)}
      </div>}
    </div>
    {empty ? <p className={styles.notice} role="status">暂无足够数据。</p> :
      <div className={kind === 'donut' ? styles.donut : styles.line}>
        <div className={styles.canvas} style={{ blockSize: height }} role="img" aria-label={label}>
          <ResponsiveContainer width="100%" height="100%" minWidth={0}>
            {kind === 'line' ? <AreaChart accessibilityLayer data={[...visible]} margin={{ top: 12, right: 12, bottom: 8, left: 0 }}>
              <defs><linearGradient id={gradient} x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor={color} stopOpacity={.18}/><stop offset="100%" stopColor={color} stopOpacity={0}/></linearGradient></defs>
              <CartesianGrid vertical={false} stroke="var(--hairline)"/>
              <XAxis dataKey="x" type="number" scale="time" domain={['dataMin', 'dataMax']} axisLine={false} tickLine={false}
                minTickGap={32} tickFormatter={value => dateLabel(Number(value), intraday)} stroke="var(--text-2)"/>
              <YAxis axisLine={false} tickLine={false} width={54} domain={['auto', 'auto']} tickFormatter={amount} stroke="var(--text-2)"/>
              <Tooltip content={({ active, payload, label: currentLabel }) => active && payload?.length ? <div className={styles.tooltip}>
                <span>{new Date(Number(currentLabel)).toISOString().replace('T', ' ').slice(0, 19)} UTC</span>
                <strong>{typeof payload[0]?.value === 'number' ? money(payload[0].value) : '—'}</strong>
              </div> : null}/>
              <Area dataKey="value" type="linear" connectNulls={false} stroke={color} strokeWidth={2} fill={`url(#${gradient})`} isAnimationActive={false}/>
            </AreaChart> : <PieChart accessibilityLayer>
              <Tooltip content={({ active, payload }) => active && payload?.length ? <div className={styles.tooltip}>
                <span>{String(payload[0]?.name)}</span><strong>{money(Number(payload[0]?.value))}</strong>
              </div> : null}/>
              <Pie data={[...points]} dataKey="value" nameKey="x" innerRadius="65%" outerRadius="88%" stroke="none" paddingAngle={2}
                isAnimationActive={false} onClick={(_entry, index) => setSelected(value => value === index ? null : index)}>
                {points.map((_point, index) => <Cell key={index} fill={color} fillOpacity={selected !== null && selected !== index ? .12 : 1 - (index % 5) * .15}/>)}
              </Pie>
            </PieChart>}
          </ResponsiveContainer>
        </div>
        {kind === 'donut' && <div className={styles.legend} aria-label={`${label}明细`}>
          {points.map((point, index) => <button key={index} type="button" aria-pressed={selected === index} onClick={() => setSelected(value => value === index ? null : index)}>
            <span>{String(point.x)}</span><strong>{((point.value ?? 0) / total * 100).toFixed(1)}%</strong>
          </button>)}
        </div>}
      </div>}
  </div>;
}
