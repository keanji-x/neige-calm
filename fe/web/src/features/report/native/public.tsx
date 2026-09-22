import type { NativeComponent, NativeViewPayload } from '../../../../../core/domain/report-view.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { MetricGroup, DistributionChart, TimeSeriesChart } from '../../../ui/data-visualization/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { InlineTable } from '../table/inline.tsx';
import { RecordBrowser } from './records.tsx';
import styles from './native.module.css';

function Cell({ component, onOpenSourceLink }: { component: NativeComponent; onOpenSourceLink?: (target: ReportSourceLinkTarget) => void }) {
  switch (component.kind) {
    case 'metrics': return <MetricGroup items={component.items} />;
    case 'time-series': return <><TimeSeriesChart label={component.title} datasets={component.datasets} emptyText={component.emptyText} /><p className={styles.muted}>{component.caption}</p></>;
    case 'distribution': return <DistributionChart label={component.title} unit={component.unit} slices={component.slices} emptyText={component.emptyText} />;
    case 'table': return <InlineTable payload={component.table} onOpenSourceLink={onOpenSourceLink} />;
    case 'records': return <RecordBrowser component={component} />;
  }
}
function Composition({ payload, onOpenSourceLink }: { payload: NativeViewPayload; onOpenSourceLink?: (target: ReportSourceLinkTarget) => void }) {
  return <div className={styles.composition}>
    <p className={styles.description}>{payload.description}</p>
    {payload.rows.map(row => <section key={row.id} className={styles.row} aria-label={row.title}>
      <h3>{row.title}</h3>
      <div className={styles[row.layout]}>{row.cells.map(component => <div key={component.id} className={styles.cell}>
        <h4>{component.title}</h4><Cell component={component} onOpenSourceLink={onOpenSourceLink} />
      </div>)}</div>
    </section>)}
    <p className={styles.snapshot}>快照 {payload.snapshot.id} · 资料截止 {new Date(payload.snapshot.observedAt).toISOString()} · 生成 {new Date(payload.snapshot.producedAt).toISOString()}</p>
  </div>;
}
export function NativeReportView({ payload, onOpenSourceLink }: { payload: NativeViewPayload; onOpenSourceLink?: (target: ReportSourceLinkTarget) => void }) {
  const [expanded, setExpanded] = useState(false);
  return <div className={styles.root}>
    <header className={styles.header}><h2>{payload.title}</h2><button type="button" className={styles.expand}
      aria-label={`展开 ${payload.title}`} title="展开视图" onClick={() => setExpanded(true)}><Icon name="arrow-up" size="sm" /></button></header>
    <Composition payload={payload} onOpenSourceLink={onOpenSourceLink} />
    <Dialog open={expanded} onClose={() => setExpanded(false)} title={payload.title} wide>
      <div className={styles.root}><Composition payload={payload} onOpenSourceLink={onOpenSourceLink} /></div>
    </Dialog>
  </div>;
}
