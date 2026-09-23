import type { NativeComponent, NativeViewPayload } from '../../../../../core/domain/report-view.ts';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { MetricGroup, DistributionChart, TimeSeriesChart, type PlotSelection } from '../../../ui/data-visualization/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { InlineTable } from '../table/inline.tsx';
import { RecordBrowser, type RecordSelection } from './records.tsx';
import styles from './native.module.css';

type Inspection = { plots: ReadonlyMap<string, PlotSelection>; distributions: ReadonlyMap<string, string | null>; records: ReadonlyMap<string, RecordSelection> };
type ReadingProps = { inspection: Inspection; onInspection: (next: Inspection) => void; onOpenSourceLink?: (target: ReportSourceLinkTarget) => void };
function Cell({ component, inspection, onInspection, onOpenSourceLink }: ReadingProps & { component: NativeComponent }) {
  switch (component.kind) {
    case 'metrics': return <MetricGroup items={component.items} />;
    case 'time-series': return <><TimeSeriesChart label={component.title} datasets={component.datasets} emptyText={component.emptyText}
      selection={inspection.plots.get(component.id) ?? { datasetId: component.datasets[0].id, selected: null, sample: null }}
      onSelection={next => onInspection({ ...inspection, plots: new Map(inspection.plots).set(component.id, next) })} /><p className={styles.muted}>{component.caption}</p></>;
    case 'distribution': return <DistributionChart label={component.title} unit={component.unit} slices={component.slices} emptyText={component.emptyText}
      selected={inspection.distributions.get(component.id) ?? null} onSelect={next => onInspection({ ...inspection, distributions: new Map(inspection.distributions).set(component.id, next) })} />;
    case 'table': return <InlineTable payload={component.table} onOpenSourceLink={onOpenSourceLink} />;
    case 'records': return <RecordBrowser component={component}
      selection={inspection.records.get(component.id) ?? { datasetId: component.datasets[0].id, mode: 'cards', selectedId: null, evidence: [] }}
      onSelection={next => onInspection({ ...inspection, records: new Map(inspection.records).set(component.id, next) })} />;
  }
}
function Composition({ payload, ...reading }: ReadingProps & { payload: NativeViewPayload }) {
  return <div className={styles.composition}>
    <p className={styles.description}>{payload.description}</p>
    {payload.rows.map(row => <section key={row.id} className={styles.row} aria-label={row.title}>
      <h3>{row.title}</h3>
      <div className={styles[row.layout]}>{row.cells.map(component => <div key={component.id} className={styles.cell}>
        {component.title && component.kind !== 'time-series' && <h4>{component.title}</h4>}<Cell component={component} {...reading} />
      </div>)}</div>
    </section>)}
    <details className={styles.snapshot}><summary>快照信息</summary><p>{payload.snapshot.id} · 资料截止 {new Date(payload.snapshot.observedAt).toISOString()} · 生成 {new Date(payload.snapshot.producedAt).toISOString()}</p></details>
  </div>;
}
export function NativeReportView({ payload, onOpenSourceLink }: { payload: NativeViewPayload; onOpenSourceLink?: (target: ReportSourceLinkTarget) => void }) {
  const [expanded, setExpanded] = useState(false);
  const [inspection, setInspection] = useState<Inspection>(() => ({ plots: new Map(), distributions: new Map(), records: new Map() }));
  const reading = { inspection, onInspection: setInspection, onOpenSourceLink };
  return <div className={styles.root}>
    <header className={styles.header}><h2>{payload.title}</h2><button type="button" className={styles.expand}
      aria-label={`展开 ${payload.title}`} title="展开视图" onClick={() => setExpanded(true)}><Icon name="arrow-up" size="sm" /></button></header>
    <Composition payload={payload} {...reading} />
    <Dialog open={expanded} onClose={() => setExpanded(false)} title={payload.title} hideTitleRow wide>
      <div className={styles.root}>
        <header className={styles.header}><h2>{payload.title}</h2><button type="button" className={styles.expand}
          aria-label="Close" title="关闭视图" onClick={() => setExpanded(false)}><Icon name="close" size="sm" /></button></header>
        <Composition payload={payload} {...reading} />
      </div>
    </Dialog>
  </div>;
}
