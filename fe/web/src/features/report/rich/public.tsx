import { Button } from '@astryxdesign/core/Button';

import { signedBarLayout, type OverviewChart, type ReportLiveView } from '../../../../../core/domain/report-live-view.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { InlineTable } from '../table/inline.tsx';
import styles from './rich.module.css';

function number(value: number) {
  return new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 }).format(value);
}

function time(value: string) {
  return new Intl.DateTimeFormat('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
    .format(new Date(value));
}

function toneClass(tone: string) {
  switch (tone) {
    case 'positive': return styles.positive;
    case 'negative': return styles.negative;
    case 'warning': return styles.warning;
    default: return styles.neutral;
  }
}

export function ReportLiveViewBlock({ payload, onOpenSourceLink }: {
  payload: ReportLiveView;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
}) {
  switch (payload.view) {
    case 'overview': return <Overview payload={payload} />;
    case 'activity': return <Activity payload={payload} />;
    case 'cards': return <ReviewCards payload={payload} />;
    case 'details': return (
      <details className={styles.details}>
        <summary className={styles.summary}><Icon name="chevron-right" size="sm" /><span>{payload.title}</span></summary>
        <div className={styles.detailBody}><InlineTable payload={payload.table} onOpenSourceLink={onOpenSourceLink} /></div>
      </details>
    );
  }
}

function Overview({ payload }: { payload: Extract<ReportLiveView, { view: 'overview' }> }) {
  return (
    <div className={styles.root}>
      {payload.notices.map((notice, index) => (
        <div key={index} className={`${styles.notice} ${toneClass(notice.tone)}`} role="note">
          <Icon name="notification" /><div><strong>{notice.title}</strong><p>{notice.detail}</p></div>
        </div>
      ))}
      <dl className={styles.metrics}>
        {payload.metrics.map((metric, index) => (
          <div key={index} className={styles.metric}>
            <dt>{metric.label}</dt><dd className={`${styles.metricValue} ${toneClass(metric.tone)}`}>{metric.value}</dd>
            <dd className={styles.metricDetail}>{metric.detail}</dd>
          </div>
        ))}
      </dl>
      {payload.asOf !== null && <p className={styles.timestamp}>最近对账 <time dateTime={payload.asOf}>{time(payload.asOf)}</time></p>}
      <div className={styles.charts}>{payload.charts.map((chart, index) => <Chart key={index} chart={chart} />)}</div>
    </div>
  );
}

function Chart({ chart }: { chart: OverviewChart }) {
  if (chart.kind === 'budget') {
    const over = chart.used !== null && chart.limit !== null && chart.used > chart.limit;
    return (
      <figure className={styles.chart}>
        <figcaption className={styles.chartTitle}>{chart.title}</figcaption>
        {chart.used !== null && chart.limit !== null ? (
          <>
            <div className={`${styles.budgetPercent} ${over ? styles.negative : styles.neutral}`}>
              {number(chart.used / chart.limit * 100)}<span>%</span>
            </div>
            <meter className={styles.meter} aria-label={chart.title} min={0} max={chart.limit}
              value={Math.min(chart.used, chart.limit)} aria-valuetext={`${number(chart.used)} / ${number(chart.limit)} ${chart.unit}`} />
            <div className={styles.budgetLabels}><span>已使用 {number(chart.used)}</span><span>上限 {number(chart.limit)} {chart.unit}</span></div>
            {over && <p className={styles.negative}>已超过当前预算</p>}
          </>
        ) : <p className={styles.empty}>暂无已确认的预算数据</p>}
        <p className={styles.chartNote}>{chart.detail}</p>
      </figure>
    );
  }
  const layout = signedBarLayout(chart.points.map((point) => point.value));
  return (
    <figure className={styles.chart}>
      <figcaption className={styles.chartTitle}>{chart.title}</figcaption>
      {chart.points.length === 0 ? <p className={styles.empty}>{chart.emptyText}</p> : (
        <div className={styles.bars} role="img" aria-label={`${chart.title}：${chart.points.map((point) => `${point.label} ${number(point.value)}`).join('；')} ${chart.unit}`}>
          {chart.points.map((point, index) => (
            <div key={index} className={styles.barRow}>
              <div className={styles.barLabels}><span>{point.label}</span><strong className={point.value < 0 ? styles.negative : point.value > 0 ? styles.positive : styles.neutral}>
                {point.value > 0 ? '+' : ''}{number(point.value)}
              </strong></div>
              <div className={styles.barTrack} title={`${point.label}: ${number(point.value)} ${chart.unit}`}>
                <span className={styles.zeroLine} style={{ insetInlineStart: `${layout[index]?.zero ?? 0}%` }} />
                <span className={point.value < 0 ? styles.barNegative : styles.barPositive}
                  style={{ insetInlineStart: `${layout[index]?.start ?? 0}%`, inlineSize: `${layout[index]?.width ?? 0}%` }} />
              </div>
            </div>
          ))}
        </div>
      )}
      <p className={styles.chartNote}>{chart.unit}</p>
    </figure>
  );
}

function Activity({ payload }: { payload: Extract<ReportLiveView, { view: 'activity' }> }) {
  const [expanded, setExpanded] = useState(false);
  const items = expanded ? payload.items : payload.items.slice(0, 5);
  return (
    <div className={styles.root}>
      {items.length === 0 ? <p className={styles.empty}>{payload.emptyText}</p> : (
        <ol className={styles.activity}>{items.map((item) => (
          <li key={item.id} className={styles.activityItem}>
            <span className={`${styles.dot} ${toneClass(item.tone)}`} aria-hidden="true" />
            <div><div className={styles.activityHead}><strong>{item.title}</strong><time dateTime={item.at}>{time(item.at)}</time></div>
              <p>{item.detail}</p></div>
          </li>
        ))}</ol>
      )}
      {payload.items.length > 5 && <Button variant="ghost" label={expanded ? '收起动态' : `更多动态（${payload.items.length - 5}）`}
        onClick={() => setExpanded(!expanded)} />}
    </div>
  );
}

function ReviewCards({ payload }: { payload: Extract<ReportLiveView, { view: 'cards' }> }) {
  const [expanded, setExpanded] = useState(false);
  const items = expanded ? payload.items : payload.items.slice(0, 3);
  return (
    <div className={styles.root}>
      {items.length === 0 ? <p className={styles.empty}>{payload.emptyText}</p> : (
        <div className={styles.reviews}>{items.map((item) => (
          <section key={item.id} className={styles.review} aria-label={item.title}>
            <h3>{item.title}</h3><p>{item.body}</p>
            {item.next !== '' && <div className={styles.next}><strong>下一步</strong><p>{item.next}</p></div>}
            {item.footer !== '' && <p className={styles.reviewFooter}>{item.footer}</p>}
          </section>
        ))}</div>
      )}
      {payload.items.length > 3 && <Button variant="ghost" label={expanded ? '收起复盘' : `更多复盘（${payload.items.length - 3}）`}
        onClick={() => setExpanded(!expanded)} />}
    </div>
  );
}
