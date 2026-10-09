import {
  resolveLiveSlot, type LiveSlotResolution, type NativeComponent, type NativeLiveSlot, type NativeSnapshot, type NativeViewPayload,
} from '../../../../../core/domain/report-view.ts';
import { useId, useMemo, type ReactNode } from 'react';
import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { BarChart, MeterChart, MetricGroup, DistributionChart, TimeSeriesChart, type PlotSelection } from '../../../ui/data-visualization/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { LivePlaceholder } from '../live/placeholder.tsx';
import type { ReportLinkPreviewResources } from '../link-preview/public.tsx';
import { InlineTable } from '../table/inline.tsx';
import { RecordBrowser, type RecordSelection } from './records.tsx';
import styles from './native.module.css';

type Inspection = { plots: ReadonlyMap<string, PlotSelection>; distributions: ReadonlyMap<string, string | null>; records: ReadonlyMap<string, RecordSelection>; snapshotOpen: boolean };
type ReadingProps = { renderText?: (text: string) => ReactNode; inspection: Inspection; onInspection: (next: Inspection) => void; onOpenSourceLink?: (target: ReportSourceLinkTarget) => void; linkPreview?: ReportLinkPreviewResources };
/** Live slot resolutions by slot id; `null` when the surface injects no overlay resolver. */
type Resolutions = ReadonlyMap<string, LiveSlotResolution> | null;
/** `at` keys inspection state: the component id inline, the slot id for a live unit. */
function Cell({ component, at, inspection, onInspection, onOpenSourceLink, linkPreview, renderText }: ReadingProps & { component: NativeComponent; at: string }) {
  switch (component.kind) {
    case 'bars': return <BarChart label={component.title} unit={component.unit} points={component.points} emptyText={component.emptyText} />;
    case 'meter': return <MeterChart label={component.title} unit={component.unit} used={component.used} limit={component.limit} usedLabel={component.usedLabel} limitLabel={component.limitLabel} detail={component.detail} emptyText={component.emptyText} tone={component.tone} />;
    case 'metrics': return <MetricGroup items={component.items} />;
    case 'time-series': return <><TimeSeriesChart label={component.title} datasets={component.datasets} emptyText={component.emptyText}
      selection={inspection.plots.get(at) ?? { datasetId: component.datasets[0].id, selected: null, sample: null, readoutOpen: false }}
      onSelection={next => onInspection({ ...inspection, plots: new Map(inspection.plots).set(at, next) })} /><p className={styles.muted}>{component.caption}</p></>;
    case 'distribution': return <DistributionChart label={component.title} unit={component.unit} slices={component.slices} emptyText={component.emptyText}
      selected={inspection.distributions.get(at) ?? null} onSelect={next => onInspection({ ...inspection, distributions: new Map(inspection.distributions).set(at, next) })} />;
    case 'table': return <InlineTable renderText={renderText} payload={component.table} onOpenSourceLink={onOpenSourceLink} linkPreview={linkPreview} />;
    case 'records': return <RecordBrowser component={component}
      selection={inspection.records.get(at) ?? { datasetId: component.datasets[0].id, mode: 'cards', selectedId: null, disclosures: [] }}
      onSelection={next => onInspection({ ...inspection, records: new Map(inspection.records).set(at, next) })} />;
  }
}
function Titled({ component, ...props }: ReadingProps & { component: NativeComponent; at: string }) {
  return <>{component.title && !['time-series', 'bars', 'meter'].includes(component.kind) && <h4>{component.title}</h4>}<Cell component={component} {...props} /></>;
}
/** A slot degrades on its own: the placeholder takes the slot's place in the row. */
function Slot({ slot, resolutions, ...reading }: ReadingProps & { slot: NativeLiveSlot; resolutions: Resolutions }) {
  const resolution = resolutions?.get(slot.id);
  if (resolution === undefined) return <LivePlaceholder state="detached" source={slot.source} />;
  switch (resolution.state) {
    case 'pending': return <LivePlaceholder state="pending" source={slot.source} />;
    case 'unavailable': return <LivePlaceholder state="unavailable" source={slot.source} reason={resolution.reason} />;
    case 'ok': return <Titled component={resolution.unit.cell} at={slot.id} {...reading} />;
  }
}
const instant = (time: number | null) => time === null ? '未知' : new Date(time).toISOString();
function provenance(payload: NativeViewPayload, resolutions: Resolutions) {
  const entries: { key: string; label: string; snapshot: NativeSnapshot }[] = [];
  if (payload.snapshot !== null) entries.push({ key: '', label: payload.snapshot.id, snapshot: payload.snapshot });
  for (const cell of payload.rows.flatMap(row => row.cells)) {
    const resolution = cell.kind === 'live' ? resolutions?.get(cell.id) : undefined;
    if (resolution?.state === 'ok') entries.push({ key: cell.id, label: resolution.unit.cell.title || cell.id, snapshot: resolution.unit.snapshot });
  }
  return entries;
}
function Composition({ payload, resolutions, action, ...reading }: ReadingProps & { payload: NativeViewPayload; resolutions: Resolutions; action?: ReactNode }) {
  const snapshotId = useId();
  const sources = provenance(payload, resolutions);
  return <div className={styles.composition}>
    {payload.description !== '' && <p className={styles.description}>{payload.description}</p>}
    {payload.rows.map(row => <section key={row.id} className={styles.row} aria-label={row.title === '' ? undefined : row.title}>
      {row.title !== '' && <h3>{row.title}</h3>}
      <div className={styles[row.layout]}>{row.cells.map(cell => <div key={cell.id} className={styles.cell}>
        {cell.kind === 'live' ? <Slot slot={cell} resolutions={resolutions} {...reading} /> : <Titled component={cell} at={cell.id} {...reading} />}
      </div>)}</div>
    </section>)}
    {(sources.length > 0 || action !== undefined) && <footer className={styles.footer}>
      {sources.length > 0 && <div className={styles.snapshot}><button type="button" className={styles.disclosureToggle} aria-expanded={reading.inspection.snapshotOpen}
      aria-controls={snapshotId} onClick={() => reading.onInspection({ ...reading.inspection, snapshotOpen: !reading.inspection.snapshotOpen })}>
      <Icon name="chevron-right" size="sm" />快照信息</button>
      <div id={snapshotId} hidden={!reading.inspection.snapshotOpen}>{sources.map(({ key, label, snapshot }) =>
        <p key={key}>{label} · 资料截止 {instant(snapshot.observedAt)} · 生成 {instant(snapshot.producedAt)}</p>)}</div></div>}
      {action}
    </footer>}
  </div>;
}
export function NativeReportView({ payload, resolveOverlay, onOpenSourceLink, linkPreview, renderText }: {
  renderText?: (text: string) => ReactNode;
  payload: NativeViewPayload;
  /** The surface's exact overlay lookup; without one, every live slot shows that this view carries no live data. */
  resolveOverlay?: (source: string) => unknown;
  onOpenSourceLink?: (target: ReportSourceLinkTarget) => void;
  linkPreview?: ReportLinkPreviewResources;
}) {
  const [expanded, setExpanded] = useState(false);
  const [inspection, setInspection] = useState<Inspection>(() => ({ plots: new Map(), distributions: new Map(), records: new Map(), snapshotOpen: false }));
  const resolutions = useMemo<Resolutions>(() => resolveOverlay === undefined ? null : new Map(payload.rows.flatMap(row => row.cells)
    .flatMap(cell => cell.kind === 'live' ? [[cell.id, resolveLiveSlot(cell, resolveOverlay)] as const] : [])), [payload, resolveOverlay]);
  const reading = { inspection, onInspection: setInspection, onOpenSourceLink, linkPreview, resolutions };
  const title = payload.title === '' ? null : <h2>{payload.title}</h2>;
  return <div className={styles.root}>
    {title !== null && <header className={styles.header}>{title}</header>}
    <Composition renderText={renderText} payload={payload} {...reading} action={<button type="button" className={styles.wideToggle}
      aria-label={payload.title === '' ? '放大查看' : `放大查看 ${payload.title}`} title="放大查看" onClick={() => setExpanded(true)}><Icon name="fullscreen" size="sm" />放大查看</button>} />
    <Dialog open={expanded} onClose={() => setExpanded(false)} title={payload.title === '' ? '视图' : payload.title} hideTitleRow wide>
      <div className={styles.root}>
        <header className={styles.header}>{title}<button type="button" className={styles.expand}
          aria-label="Close" title="关闭视图" onClick={() => setExpanded(false)}><Icon name="close" size="sm" /></button></header>
        <Composition renderText={renderText} payload={payload} {...reading} />
      </div>
    </Dialog>
  </div>;
}
