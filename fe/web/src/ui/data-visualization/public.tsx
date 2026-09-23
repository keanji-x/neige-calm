import { useId } from 'react';
import { Icon } from '../icon/public.tsx';
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
export type PlotSelection = { datasetId: string; selected: string | null; sample: number | null; readoutOpen: boolean };

function number(value: number, decimals = 2) {
  return new Intl.NumberFormat('zh-CN', { maximumFractionDigits: decimals }).format(value);
}
function axisNumber(value: number) {
  return new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 2 }).format(value);
}
function observationNumber(value: number) {
  if (value !== 0 && Math.abs(value) < 1e-6) return String(value);
  return new Intl.NumberFormat('zh-CN', { maximumSignificantDigits: 17 }).format(value);
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

export function DistributionChart({ label, unit, slices, emptyText, selected, onSelect }: {
  label: string; unit: string; slices: readonly DistributionDatum[]; emptyText: string;
  selected: string | null; onSelect: (id: string | null) => void;
}) {
  const total = slices.reduce((sum, slice) => sum + slice.value, 0);
  const current = slices.find(slice => slice.id === selected);
  if (slices.length === 0) return <p className={styles.empty}>{emptyText}</p>;
  const percentage = (value: number) => total === 0 ? '—' : `${number(value / total * 100, 1)}%`;
  let cumulative = 0;
  const circumference = 2 * Math.PI * 50;
  return <div className={styles.distribution}>
    <svg className={styles.donut} viewBox="0 0 150 150" role="img"
      aria-label={`${label}: ${slices.map(s => `${s.label} ${s.value} ${unit}, ${percentage(s.value)}`).join(', ')}`}>
      {slices.map(slice => {
        const ratio = total === 0 ? 0 : slice.value / total;
        const start = cumulative; cumulative += ratio;
        return <circle key={slice.id} className={palette(slice.palette)} cx="75" cy="75" r="50"
          fill="none" stroke="currentColor" strokeWidth="18" transform="rotate(-90 75 75)"
          strokeDasharray={`${ratio * circumference} ${circumference}`}
          strokeDashoffset={-start * circumference} opacity={current && current.id !== slice.id ? 0.25 : 1} />;
      })}
      <text x="75" y="73" textAnchor="middle" className={styles.donutValue}>{percentage(current?.value ?? total)}</text>
      <text x="75" y="94" textAnchor="middle" className={styles.donutLabel}>占比</text>
    </svg>
    <div className={`${styles.legend} ${styles.distributionLegend}`}>{slices.map(slice => <button key={slice.id} type="button"
      aria-pressed={selected === slice.id} onClick={() => onSelect(selected === slice.id ? null : slice.id)}>
      <span className={`${styles.swatch} ${palette(slice.palette)}`} aria-hidden="true" />
      <span className={styles.legendLabel}>{slice.label}</span><span>{percentage(slice.value)}</span>
    </button>)}</div>
    <p className={styles.detail}>{current?.label ?? label} · {observationNumber(current?.value ?? total)} {unit}</p>
  </div>;
}

const WIDTH = 600;
const HEIGHT = 170;
const PAD = 5;

/** Match the plotted time axis, not evenly-spaced indices, including irregular samples. */
export function nearestSample(stamps: readonly number[], fraction: number): number {
  const target = (stamps[0] ?? 0) + Math.max(0, Math.min(1, fraction)) * ((stamps.at(-1) ?? 0) - (stamps[0] ?? 0));
  return stamps.reduce((nearest, stamp, index) => Math.abs(stamp - target) < Math.abs(stamps[nearest] - target) ? index : nearest, 0);
}

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

export function TimeSeriesChart({ label, datasets, emptyText, selection, onSelection }: {
  label: string; datasets: readonly PlotDataset[]; emptyText: string;
  selection: PlotSelection; onSelection: (selection: PlotSelection) => void;
}) {
  const readoutId = useId();
  const { datasetId, selected, sample, readoutOpen } = selection;
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
  const maximum = data.style === 'stacked' ? (high > 0 ? high : 1) : high + span * 0.08;
  const y = (value: number) => HEIGHT - PAD - (value - minimum) / (maximum - minimum || 1) * (HEIGHT - 2 * PAD);
  const sampleIndex = Math.max(0, Math.min(sample ?? points.length - 1, points.length - 1));
  const focused = points[sampleIndex];
  const describeSample = focused ? `${focused.date}; ${data.series.map((series, i) => `${series.label}: ${focused.values[i] === null ? '未知' : observationNumber(focused.values[i])} ${data.unit}`).join('; ')}` : '';
  const selectAt = (element: HTMLInputElement, clientX: number) => {
    const box = element.getBoundingClientRect();
    if (box.width === 0) return;
    const fraction = ((clientX - box.left) / box.width * WIDTH - PAD) / (WIDTH - 2 * PAD);
    onSelection({ ...selection, sample: nearestSample(stamps, fraction) });
  };
  return <div className={styles.root}>
    <div className={styles.toolbar}><h4>{label}</h4><div className={styles.toolbarControls}><span className={styles.unit}>{data.unit}</span>
      {datasets.length === 2 && <div className={styles.tabs} aria-label={`${label} 数据视图`}>{datasets.map(d =>
        <button type="button" key={d.id} aria-pressed={data.id === d.id} onClick={() => onSelection({ ...selection, datasetId: d.id, sample: null, selected: null })}>{d.label}</button>)}</div>}
      {datasets.length > 2 && <select aria-label={`${label} 数据视图`} value={data.id}
        onChange={event => onSelection({ ...selection, datasetId: event.target.value, sample: null, selected: null })}>
        {datasets.map(d => <option key={d.id} value={d.id}>{d.label}</option>)}
      </select>}
    </div></div>
    {!hasData ? <p className={styles.empty}>{emptyText}</p> : <>
      <div className={styles.plot}>
        <div className={styles.yAxis} aria-hidden="true">{[maximum, (maximum + minimum) / 2, minimum].map((v, i) => <span key={i}>{axisNumber(v)}</span>)}</div>
        <div className={styles.plotFrame}>
        <svg className={styles.plotSvg} viewBox={`0 0 ${WIDTH} ${HEIGHT}`} preserveAspectRatio="none" role="img"
          aria-label={`${label}: ${points.length} observations, ${points[0]?.date} to ${points.at(-1)?.date}; ${data.unit}`}>
          {[minimum, (maximum + minimum) / 2, maximum].map((v, i) => <line key={i} className={styles.grid}
            x1={PAD} x2={WIDTH - PAD} y1={y(v)} y2={y(v)} vectorEffect="non-scaling-stroke" />)}
          {data.series.map((series, seriesIndex) => {
            const muted = selected !== null && selected !== series.id;
            if (data.style === 'stacked') {
              if (points.length === 1) {
                const bottom = points[0].values.slice(0, seriesIndex).reduce<number>((sum, v) => sum + (v ?? 0), 0);
                const top = bottom + (points[0].values[seriesIndex] ?? 0);
                return <rect key={series.id} className={palette(series.palette)} x={WIDTH / 2 - 6} y={y(top)}
                  width={12} height={y(bottom) - y(top)} fill="currentColor" opacity={muted ? 0.12 : 0.65} />;
              }
              const bottom = points.map((p, i) => `${x(stamps[i])} ${y(p.values.slice(0, seriesIndex).reduce<number>((sum, v) => sum + (v ?? 0), 0))}`);
              const top = points.map((p, i) => `${x(stamps[i])} ${y(p.values.slice(0, seriesIndex + 1).reduce<number>((sum, v) => sum + (v ?? 0), 0))}`);
              return <path key={series.id} className={palette(series.palette)} fill="currentColor" opacity={muted ? 0.12 : 0.65}
                d={`M${top.join(' L')} L${bottom.reverse().join(' L')} Z`} />;
            }
            const coordinates = points.map((p, i) => ({ x: x(stamps[i]), y: p.values[seriesIndex] == null ? null : y(p.values[seriesIndex]) }));
            return <g key={series.id} className={palette(series.palette)} opacity={muted ? 0.2 : 1}>
              {linePaths(coordinates).map((d, i) => <path key={i} d={d} fill="none" stroke="currentColor" strokeWidth="2" vectorEffect="non-scaling-stroke" />)}
              {coordinates.filter((p, i) => p.y !== null && (i === sampleIndex
                || ((i === 0 || coordinates[i - 1].y === null) && (i === coordinates.length - 1 || coordinates[i + 1].y === null))))
                .map((p, i) => <circle key={i} cx={p.x} cy={p.y!} r="3" fill="currentColor" />)}
            </g>;
          })}
          {focused && <line className={styles.cursor} x1={x(stamps[sampleIndex])} x2={x(stamps[sampleIndex])}
            y1={PAD} y2={HEIGHT - PAD} vectorEffect="non-scaling-stroke" />}
        </svg>
        <input className={styles.dateCursor} type="range" aria-label={`${label} 观察日期`} aria-valuetext={describeSample}
          min={0} max={Math.max(0, points.length - 1)} value={sampleIndex}
          onPointerDown={event => { event.preventDefault(); event.currentTarget.focus(); selectAt(event.currentTarget, event.clientX); }}
          onPointerMove={event => selectAt(event.currentTarget, event.clientX)}
          onChange={event => onSelection({ ...selection, sample: Number(event.target.value) })} />
        {focused && <div className={styles.tooltip} aria-hidden="true" style={sampleIndex < points.length / 2 ? { right: 8 } : { left: 8 }}>
          <strong>{focused.date}</strong>
        </div>}
        </div>
        <div className={styles.xAxis}><span>{points[0]?.date}</span><span>{points.at(-1)?.date}</span></div>
      </div>
      <button type="button" className={styles.readoutToggle} aria-label={`${label} 观察值`}
        aria-expanded={readoutOpen} aria-controls={readoutId} onClick={() => onSelection({ ...selection, readoutOpen: !readoutOpen })}>
        <span>{focused?.date}</span><span className={styles.readoutCommand}>观察值 <Icon name="chevron-right" size="sm" /></span>
      </button>
      {focused && <section id={readoutId} hidden={!readoutOpen} className={styles.readout} aria-label={`${label} 观察值`}>
        <dl>{data.series.map((series, i) => <div key={series.id}>
          <dt><button type="button" aria-pressed={selected === series.id} aria-describedby={`${readoutId}-${i}`}
            onClick={() => onSelection({ ...selection, selected: selected === series.id ? null : series.id })}>
            <span className={`${styles.swatch} ${palette(series.palette)}`} aria-hidden="true" />
            <span className={styles.legendLabel}>{series.label}</span>
          </button></dt>
          <dd id={`${readoutId}-${i}`}><span>{focused.values[i] === null ? '未知' : observationNumber(focused.values[i])}</span> {data.unit}</dd>
        </div>)}</dl>
      </section>}
      {!readoutOpen && <div className={`${styles.legend} ${styles.plotLegend}`}>{data.series.map(series => <button key={series.id} type="button"
        aria-pressed={selected === series.id} onClick={() => onSelection({ ...selection, selected: selected === series.id ? null : series.id })}>
        <span className={`${styles.swatch} ${palette(series.palette)}`} aria-hidden="true" /><span className={styles.legendLabel}>{series.label}</span>
      </button>)}</div>}
    </>}
  </div>;
}
