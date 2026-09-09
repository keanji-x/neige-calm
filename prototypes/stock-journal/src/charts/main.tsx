import { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Area, AreaChart, CartesianGrid, Cell, Pie, PieChart, XAxis, YAxis } from 'recharts';
import { ChartContainer, ChartTooltip, ChartTooltipContent, type ChartConfig } from './chart';
import { portfolioSnapshot } from '../portfolio-state';
import { portfolioView, portfolioSnapshotSchema, type PortfolioSnapshot } from '../portfolio-framework';

const config = { value: { label: '组合资产', color: '#4a5f9b' } } satisfies ChartConfig;
const percent = (value: number) => `${Number(value.toFixed(1))}%`;
const axisAmount = (value: number) => Math.abs(value) >= 10000 ? `${Number((value / 10000).toFixed(2))}万`
  : value.toLocaleString('zh-CN', { maximumFractionDigits: 2 });

function PortfolioCharts({ snapshot }: { snapshot: PortfolioSnapshot }) {
const view = portfolioView(snapshot);
const palette = ['#4a5f9b', '#91a6b5', '#b6a483', '#7f9b8b', '#a49ab0'];
const holdings = [
  ...view.rows.filter(item => item.value !== null && item.value > 0).map((item, index) => ({
    id: `holding:${item.id}`, name: item.name, value: item.value!, color: palette[index % palette.length],
  })),
  ...(snapshot.cash > 0 ? [{ id: 'cash', name: '现金', value: snapshot.cash, color: '#d9d8d4' }] : []),
];
  const allocationConfig = Object.fromEntries(holdings.map((item, index) => [`holding${index}`, { label: item.name, color: item.color }])) satisfies ChartConfig;
  const money = (value: number) => `${value.toLocaleString('zh-CN')} ${snapshot.currency}`;
  const [range, setRange] = useState<1 | 3 | 6>(6);
  const [selected, setSelected] = useState<string | null>(null);
  const latest = view.history.at(-1);
  const days = range === 1 ? 31 : range === 3 ? 93 : 186;
  const visible = latest ? view.history.filter(item => Date.parse(item.date) >= Date.parse(latest.date) - days * 86400000) : [];
  const intraday = visible.length > 1 && Date.parse(visible.at(-1)!.date) - Date.parse(visible[0].date) <= 86400000;
  const timeLabel = (value: string) => intraday ? new Date(value).toISOString().slice(11, 16)
    : `${Number(value.slice(5, 7))}/${Number(value.slice(8, 10))}`;
  const activeHolding = holdings.find(item => item.id === selected);
  const stockWeight = view.total !== null && view.total > 0 ? (view.total - snapshot.cash) / view.total * 100 : null;
  const selectedWeight = activeHolding && view.total !== null && view.total > 0 ? activeHolding.value / view.total * 100 : stockWeight;
  return <main className="overview">
    <section aria-label="组合资产走势">
      <div className="chart-title"><h1>组合走势</h1><span>{snapshot.currency}{intraday ? ' · UTC' : ''}</span><div className="range-controls" aria-label="走势时间范围">{([1, 3, 6] as const).map(months => <button type="button" key={months} aria-pressed={range === months} onClick={() => setRange(months)}>{months}M</button>)}</div></div>
      {visible.filter(point => point.value !== null).length < 2 ? <p className="chart-empty">暂无足够的估值历史。记录至少两个时间点后显示走势。</p> :
      <ChartContainer config={config} className="performance-chart" aria-label="组合资产折线图">
        <AreaChart accessibilityLayer data={visible} margin={{ left: 0, top: 16, right: 12, bottom: 0 }}>
          <defs><linearGradient id="portfolio-fill" x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor="var(--color-value)" stopOpacity={.18}/><stop offset="100%" stopColor="var(--color-value)" stopOpacity={.01}/></linearGradient></defs>
          <CartesianGrid vertical={false}/>
          <XAxis dataKey="date" axisLine={false} tickLine={false} minTickGap={36} tickMargin={10} tickFormatter={value => timeLabel(String(value))}/>
          <YAxis axisLine={false} tickLine={false} width={52} domain={['auto', 'auto']} tickCount={4} tickFormatter={value => axisAmount(Number(value))}/>
          <ChartTooltip content={<ChartTooltipContent labelFormatter={label => String(label)} formatter={value => <div className="tooltip-value"><span>组合资产</span><strong>{money(Number(value))}</strong></div>}/>}/>
          <Area connectNulls={false} type="linear" dataKey="value" stroke="var(--color-value)" strokeWidth={2} fill="url(#portfolio-fill)" activeDot={{ r: 4, strokeWidth: 2 }} isAnimationActive={false}/>
        </AreaChart>
      </ChartContainer>}
    </section>
    <section className="breakdowns" aria-label="持仓权重">
      <div className="chart-title"><h2>持仓权重</h2><span>{view.stale ? '含旧行情' : '按市值'}</span></div>
      {view.total === null ? <p className="chart-empty">等待完整行情与汇率，暂不显示组合权重。</p> : view.total === 0 ?
        <p className="chart-empty">暂无可估值资产。录入持仓或现金后显示权重。</p> :
      <div className="allocation-layout">
        <div className="pie-wrap">
          <ChartContainer config={allocationConfig} className="allocation-chart" aria-label="持仓权重环形图">
            <PieChart accessibilityLayer>
              <ChartTooltip content={<ChartTooltipContent hideLabel nameKey="name" formatter={(value, name) => <div className="tooltip-value"><span>{String(name)}</span><strong>{money(Number(value))} · {percent(Number(value) / view.total! * 100)}</strong></div>}/>}/>
              <Pie data={holdings} dataKey="value" nameKey="name" innerRadius="65%" outerRadius="90%" strokeWidth={0} stroke="none" paddingAngle={2} isAnimationActive={false}
                onClick={(_item, index) => { const id = holdings[index]?.id; if (id) setSelected(current => current === id ? null : id); }}>
                {holdings.map(item => <Cell key={item.id} fill={item.color} fillOpacity={!activeHolding || selected === item.id ? 1 : .25}/>) }
              </Pie>
            </PieChart>
          </ChartContainer>
          <div className="pie-center" aria-live="polite"><strong>{activeHolding ? percent(selectedWeight!) : snapshot.source === 'live' ? view.rows.length : selectedWeight === null ? '—' : percent(selectedWeight)}</strong><small>{activeHolding?.name ?? (snapshot.source === 'live' ? '登记资产' : '股票仓位')}</small></div>
        </div>
        <div className="legend">{holdings.map(item => <button type="button" key={item.id} aria-pressed={selected === item.id} onClick={() => setSelected(current => current === item.id ? null : item.id)}><i style={{ background: item.color }}/><span>{item.name}</span><strong>{percent(item.value / view.total! * 100)}</strong></button>)}</div>
      </div>}
    </section>
  </main>;
}

const root = document.getElementById('charts-root');
if (!root) throw new Error('Missing chart root');
function LiveCharts({ trackId }: { trackId: string }) {
  const [state, setState] = useState<{ snapshot: PortfolioSnapshot | null; message: string }>({ snapshot: null, message: '正在读取 Market data…' });
  useEffect(() => {
    const origin = new URL(location.href).origin;
    const receive = (event: MessageEvent) => {
      if (event.source !== parent || event.origin !== origin || event.data?.type !== 'neige:portfolio-snapshot' || event.data.trackId !== trackId) return;
      const result = event.data.result;
      if (result?.kind === 'ready') {
        const parsed = portfolioSnapshotSchema.safeParse(result.snapshot);
        setState(parsed.success ? { snapshot: parsed.data, message: '' } : { snapshot: null, message: '组合数据格式无效。' });
      } else if ((result?.kind === 'waiting' || result?.kind === 'invalid') && typeof result.message === 'string') {
        setState({ snapshot: null, message: result.message });
      }
    };
    window.addEventListener('message', receive);
    parent.postMessage({ type: 'neige:portfolio-ready', trackId }, origin);
    return () => window.removeEventListener('message', receive);
  }, [trackId]);
  return state.snapshot ? <PortfolioCharts snapshot={state.snapshot}/> : <p className="chart-empty" role="status">{state.message}</p>;
}
const liveTrack = new URL(location.href).searchParams.get('track');
createRoot(root).render(liveTrack === null ? <PortfolioCharts snapshot={portfolioSnapshot}/> : <LiveCharts trackId={liveTrack}/>);
