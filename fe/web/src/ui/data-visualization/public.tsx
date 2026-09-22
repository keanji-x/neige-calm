import { useState } from '../state/public.ts';
import styles from './visualization.module.css';

export type ValueDisplay =
  | { state: 'known'; amount: number; unit: string; placement: 'prefix' | 'suffix'; decimals: number; signed: boolean }
  | { state: 'unknown'; reason: string };
export type MetricDatum = { id: string; label: string; value: ValueDisplay; detail: string;
  tone: 'neutral' | 'positive' | 'negative' | 'warning'; emphasis: 'primary' | 'normal' };
export type PlotDataset = { id: string; label: string; unit: string; style: 'line' | 'stacked';
  series: readonly { id: string; label: string; palette: number }[];
  points: readonly { date: string; values: readonly (number | null)[] }[] };
export type DistributionDatum = { id: string; label: string; value: number; palette: number };

function number(value: number, decimals = 2) {
  return new Intl.NumberFormat('zh-CN', { maximumFractionDigits: decimals }).format(value);
}
function scalar(value: ValueDisplay) {
  if (value.state === 'unknown') return '—';
  const sign = value.amount < 0 ? '-' : value.signed && value.amount > 0 ? '+' : '';
  const amount = number(Math.abs(value.amount), value.decimals);
  return sign + (value.placement === 'prefix' ? value.unit + amount : amount + value.unit);
}
function palette(value: number) { return styles[`series${value}`] ?? ''; }

export function MetricGroup({ items }: { items: readonly MetricDatum[] }) {
  return <dl className={styles.metrics}>{items.map(item => <div key={item.id}
    className={item.emphasis === 'primary' ? styles.primaryMetric : styles.metric}>
    <dt>{item.label}</dt><dd className={styles[item.tone]}>{scalar(item.value)}</dd>
    <dd className={styles.detail}>{item.value.state === 'unknown' ? item.value.reason : item.detail}</dd>
  </div>)}</dl>;
}

export function DistributionChart({ label, unit, slices, emptyText }: {
  label: string; unit: string; slices: readonly DistributionDatum[]; emptyText: string;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const total = slices.reduce((sum, slice) => sum + slice.value, 0);
  const current = slices.find(slice => slice.id === selected);
  if (total === 0) return <p className={styles.empty}>{emptyText}</p>;
  let cumulative = 0;
  const circumference = 2 * Math.PI * 50;
  return <div className={styles.distribution}>
    <svg className={styles.donut} viewBox="0 0 150 150" role="img"
      aria-label={`${label}: ${slices.map(s => `${s.label} ${number(s.value / total * 100, 1)}%`).join(', ')}`}>
      {slices.map(slice => {
        const start = cumulative; cumulative += slice.value / total;
        return <circle key={slice.id} className={palette(slice.palette)} cx="75" cy="75" r="50"
          fill="none" stroke="currentColor" strokeWidth="18" transform="rotate(-90 75 75)"
          strokeDasharray={`${slice.value / total * circumference} ${circumference}`}
          strokeDashoffset={-start * circumference} opacity={current && current.id !== slice.id ? 0.25 : 1} />;
      })}
      <text x="75" y="73" textAnchor="middle" className={styles.donutValue}>{current ? `${number(current.value / total * 100, 1)}%` : '100%'}</text>
      <text x="75" y="94" textAnchor="middle" className={styles.donutLabel}>占比</text>
    </svg>
    <div className={styles.legend}>{slices.map(slice => <button key={slice.id} type="button"
      aria-pressed={selected === slice.id} onClick={() => setSelected(selected === slice.id ? null : slice.id)}>
      <span className={`${styles.swatch} ${palette(slice.palette)}`} aria-hidden="true" />
      {slice.label}<span>{number(slice.value / total * 100, 1)}%</span>
    </button>)}</div>
    <p className={styles.detail}>{current?.label ?? label} · {number(current?.value ?? total)} {unit}</p>
  </div>;
}

const WIDTH = 600;
const HEIGHT = 170;
const PAD = 5;

/** Null observations split the path; zero remains an ordinary observation. */
export function linePaths(points: readonly { x: number; y: number | null }[]): string[] {
  const paths: string[] = [];
  let current = '';
  for (const point of points) {
    if (point.y === null) { if (current) paths.push(current); current = ''; }
    else current += `${current ? ' L' : 'M'}${point.x},${point.y}`;
  }
  if (current) paths.push(current);
  return paths;
}

export function TimeSeriesChart({ label, datasets, emptyText }: {
  label: string; datasets: readonly PlotDataset[]; emptyText: string;
}) {
  const [datasetId, setDatasetId] = useState(datasets[0]?.id ?? '');
  const [selected, setSelected] = useState<string | null>(null);
  const [sample, setSample] = useState<number | null>(null);
  const data = datasets.find(d => d.id === datasetId) ?? datasets[0];
  if (!data) return <p className={styles.empty}>{emptyText}</p>;
  const points = data.points;
  const incompleteStack = data.style === 'stacked' && points.some(point => point.values.some(v => v === null || v < 0));
  if (incompleteStack) return <p className={styles.empty}>无法绘制不完整的堆叠数据。</p>;
  const hasData = points.some(point => point.values.some(v => v !== null));
  const stamps = points.map(p => Date.parse(`${p.date}T00:00:00Z`));
  const start = stamps[0] ?? 0, end = stamps.at(-1) ?? start;
  const x = (stamp: number) => end === start ? WIDTH / 2 : PAD + (stamp - start) / (end - start) * (WIDTH - 2 * PAD);
  const all = data.style === 'stacked'
    ? points.map(p => p.values.reduce<number>((sum, value) => sum + (value ?? 0), 0))
    : points.flatMap(p => p.values.filter((v): v is number => v !== null));
  const low = data.style === 'stacked' || !hasData ? 0 : Math.min(...all);
  const high = hasData ? Math.max(...all) : 1;
  const span = high > low ? high - low : Math.max(Math.abs(high) * 0.1, 1);
  const minimum = data.style === 'stacked' ? 0 : low - span * 0.08;
  const maximum = high + span * 0.08;
  const y = (value: number) => HEIGHT - PAD - (value - minimum) / (maximum - minimum || 1) * (HEIGHT - 2 * PAD);
  const focused = points[Math.min(sample ?? points.length - 1, points.length - 1)];
  return <div className={styles.root}>
    {datasets.length > 1 && <div className={styles.tabs} aria-label={`${label} 数据视图`}>{datasets.map(d =>
      <button type="button" key={d.id} aria-pressed={data.id === d.id} onClick={() => { setDatasetId(d.id); setSample(null); setSelected(null); }}>{d.label}</button>)}</div>}
    <p className={styles.detail}>{data.unit}</p>
    {!hasData ? <p className={styles.empty}>{emptyText}</p> : <>
      <div className={styles.plot}>
        <div className={styles.yAxis} aria-hidden="true">{[maximum, (maximum + minimum) / 2, minimum].map((v, i) => <span key={i}>{number(v)}</span>)}</div>
        <svg className={styles.plotSvg} viewBox={`0 0 ${WIDTH} ${HEIGHT}`} preserveAspectRatio="none" role="img"
          aria-label={`${label}: ${points.length} observations, ${points[0]?.date} to ${points.at(-1)?.date}; ${data.unit}`}>
          {[minimum, (maximum + minimum) / 2, maximum].map((v, i) => <line key={i} className={styles.grid}
            x1={PAD} x2={WIDTH - PAD} y1={y(v)} y2={y(v)} vectorEffect="non-scaling-stroke" />)}
          {data.series.map((series, seriesIndex) => {
            const muted = selected !== null && selected !== series.id;
            if (data.style === 'stacked') {
              const bottom = points.map((p, i) => `${x(stamps[i])} ${y(p.values.slice(0, seriesIndex).reduce<number>((sum, v) => sum + (v ?? 0), 0))}`);
              const top = points.map((p, i) => `${x(stamps[i])} ${y(p.values.slice(0, seriesIndex + 1).reduce<number>((sum, v) => sum + (v ?? 0), 0))}`);
              return <path key={series.id} className={palette(series.palette)} fill="currentColor" opacity={muted ? 0.12 : 0.5}
                d={`M${top.join(' L')} L${bottom.reverse().join(' L')} Z`} />;
            }
            const coordinates = points.map((p, i) => ({ x: x(stamps[i]), y: p.values[seriesIndex] == null ? null : y(p.values[seriesIndex]) }));
            return <g key={series.id} className={palette(series.palette)} opacity={muted ? 0.2 : 1}>
              {linePaths(coordinates).map((d, i) => <path key={i} d={d} fill="none" stroke="currentColor" strokeWidth="2" vectorEffect="non-scaling-stroke" />)}
              {coordinates.filter(p => p.y !== null).map((p, i) => <circle key={i} cx={p.x} cy={p.y!} r="2" fill="currentColor" />)}
            </g>;
          })}
        </svg>
        <div className={styles.xAxis}><span>{points[0]?.date}</span><span>{points.at(-1)?.date}</span></div>
      </div>
      <div className={styles.legend}>{data.series.map((series, i) => <button key={series.id} type="button"
        aria-pressed={selected === series.id} onClick={() => setSelected(selected === series.id ? null : series.id)}>
        <span className={`${styles.swatch} ${palette(series.palette)}`} aria-hidden="true" />{series.label}
        <span>{focused?.values[i] == null ? '未知' : number(focused.values[i])} {data.unit}</span>
      </button>)}</div>
      <label className={styles.sample}><span>{focused?.date}</span><input type="range" aria-label={`${label} 观察日期`}
        min={0} max={Math.max(0, points.length - 1)} value={sample ?? points.length - 1}
        onChange={e => setSample(Number(e.target.value))} /></label>
    </>}
  </div>;
}
