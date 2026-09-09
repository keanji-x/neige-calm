import { lazy, Suspense } from 'react';

import { parseReportLink, type ReportLinkTarget } from '../../../../../core/domain/report.ts';
import type { LayoutItem, LayoutTable, ReportLayout } from '../../../../../core/domain/report-layout.ts';
import { layoutCell, layoutCellTrack, layoutChartData, layoutShareTotal, resolveLayoutData, type LayoutData } from '../../../../../core/domain/report-layout-data.ts';
import styles from './layout.module.css';

const Chart = lazy(() => import('../../../ui/chart/public.tsx').then(module => ({ default: module.Chart })));

export function ReportLayoutBlock({ payload, resolveLive, onOpenLink }: {
  payload: ReportLayout; resolveLive?: (source: string) => unknown; onOpenLink?: (target: ReportLinkTarget) => void;
}) {
  return <div className={`${styles.layout} ${styles[payload.gap]} ${styles[payload.surface]}`} style={{ gridTemplateColumns: `repeat(${payload.columns}, minmax(0, 1fr))` }}>
    {payload.items.map((item, index) => <section key={index} className={styles.item} style={{ gridColumn: `span ${item.span}` }} aria-label={item.title || undefined}>
      {item.title !== '' && <h3 className={styles.title}>{item.title}</h3>}
      <LayoutContent key={JSON.stringify(item)} item={item} resolveLive={resolveLive} onOpenLink={onOpenLink}/>
    </section>)}
  </div>;
}

function LayoutContent({ item, resolveLive, onOpenLink }: {
  item: LayoutItem; resolveLive?: (source: string) => unknown; onOpenLink?: (target: ReportLinkTarget) => void;
}) {
  const data = resolveLayoutData(item, resolveLive);
  const chart = item.kind === 'chart' ? layoutChartData(item, data) : null;
  return <>
    {data.kind === 'unavailable' ? <p role="status" className={styles.notice}>{data.message}</p>
      : item.kind === 'table' ? <LayoutTableContent item={item} data={data} onOpenLink={onOpenLink}/>
        : chart?.kind === 'unavailable' ? <p role="status" className={styles.notice}>{chart.message}</p>
          : chart?.kind === 'ready' ? <Suspense fallback={<p role="status" className={styles.notice}>正在加载图表…</p>}>
            <Chart kind={item.chart} label={item.title || '图表'} points={chart.points} unit={chart.unit} color={item.color}
              height={item.height} ranges={item.ranges} defaultRange={item.defaultRange}/>
          </Suspense> : null}
    {data.caption !== '' && <p className={styles.caption}>{data.caption}</p>}
  </>;
}

function LayoutTableContent({ item, data, onOpenLink }: {
  item: LayoutTable; data: Extract<LayoutData, { kind: 'ready' }>; onOpenLink?: (target: ReportLinkTarget) => void;
}) {
  const totals = item.columns.map(column => column.format === 'share' ? layoutShareTotal(item, column, data) : null);
  return <div className={styles.scroll}>
    <table className={styles.table}>
      <thead><tr>{item.columns.map(column => <th key={column.key} scope="col" className={column.format === 'text' ? undefined : styles.numeric}>{column.label}</th>)}</tr></thead>
      <tbody>{data.rows.map((row, index) => <tr key={index}>{item.columns.map((column, columnIndex) => {
        const text = layoutCell(column, row, totals[columnIndex] ?? null);
        const track = layoutCellTrack(column, row);
        const target = track === null ? null : parseReportLink(`neige://wave/${encodeURIComponent(track)}`);
        return <td key={column.key} className={column.format === 'text' ? undefined : styles.numeric}>
          {target !== null && onOpenLink !== undefined ? <button className={styles.link} type="button" onClick={() => onOpenLink(target)}>{text}</button> : text}
        </td>;
      })}</tr>)}</tbody>
    </table>
    {data.rows.length === 0 && <p role="status" className={styles.notice}>暂无记录。</p>}
    {totals.some((total, index) => item.columns[index]?.format === 'share' && total === null) && data.rows.length > 0
      && <p role="status" className={styles.notice}>估值不完整或总额不一致，暂不显示占比。</p>}
  </div>;
}
