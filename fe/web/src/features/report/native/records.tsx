import type { NativeComponent } from '../../../../../core/domain/report-view.ts';
import { useId, useRef } from 'react';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './native.module.css';

type Records = Extract<NativeComponent, { kind: 'records' }>;
export type RecordSelection = { datasetId: string; mode: 'cards' | 'list'; selectedId: string | null; evidence: readonly string[] };
export function RecordBrowser({ component, selection, onSelection }: {
  component: Records; selection: RecordSelection; onSelection: (selection: RecordSelection) => void;
}) {
  const { datasetId, mode, selectedId } = selection;
  const data = component.datasets.find(d => d.id === datasetId) ?? component.datasets[0];
  const selected = data?.items.find(item => item.id === selectedId);
  const disclosureId = useId();
  const opener = useRef<HTMLButtonElement | null>(null);
  const closeDetail = () => { onSelection({ ...selection, selectedId: null, evidence: [] }); opener.current?.focus(); };
  return <div className={styles.recordBrowser}>
    <div className={styles.controls}>
      {component.datasets.length > 1 && <div className={styles.segments} aria-label="记录场景">{component.datasets.map(d =>
        <button type="button" key={d.id} aria-pressed={data?.id === d.id} onClick={() => onSelection({ ...selection, datasetId: d.id, selectedId: null, evidence: [] })}>{d.label}</button>)}</div>}
      <div className={styles.segments} aria-label="记录呈现方式">
        <button type="button" aria-pressed={mode === 'cards'} onClick={() => onSelection({ ...selection, mode: 'cards' })}>卡片</button>
        <button type="button" aria-pressed={mode === 'list'} onClick={() => onSelection({ ...selection, mode: 'list' })}>队列</button>
      </div>
    </div>
    {data?.description && <p className={styles.datasetDescription}>{data.description}</p>}
    <div className={selected ? styles.reviewWorkspace : undefined}>
    {!data?.items.length ? <p className={styles.muted}>{component.emptyText}</p> : <div className={mode === 'cards' ? styles.cards : styles.list}>
      {data.items.map(item => <article key={item.id} className={`${styles.record} ${selectedId === item.id ? styles.activeRecord : ''}`}>
        <div className={styles.recordTop}><span>{item.id} · {item.category}</span><span className={`${styles.status} ${styles[item.status.tone]}`}>{item.status.label}</span></div>
        <h4>{item.title}</h4><p>{item.summary}</p>
        <dl className={styles.facts}>{item.facts.slice(0, 2).map((field, index) => <div key={index}><dt>{field.label}</dt><dd>{field.value}</dd></div>)}</dl>
        <div className={styles.recordBottom}><span className={styles[item.handling.tone]}>{item.handling.label}</span>
          <button type="button" ref={selectedId === item.id ? opener : undefined}
            onClick={() => onSelection({ ...selection, selectedId: selectedId === item.id ? null : item.id, evidence: [] })} aria-expanded={selectedId === item.id}>查看证据</button></div>
      </article>)}
    </div>}
    {selected && <section className={styles.recordDetail} aria-label={`${selected.title} 详情`}>
      <header className={styles.detailHeader}><div><span>{selected.id} · {selected.category}</span><h4>{selected.title}</h4></div>
        <button type="button" className={styles.expand} aria-label="收起详情" title="收起详情" onClick={closeDetail}><Icon name="close" size="sm" /></button></header>
      <p className={styles.recordStates}><span className={styles[selected.status.tone]}>状态：{selected.status.label}</span>
        <span className={styles[selected.handling.tone]}>处理：{selected.handling.label}</span></p>
      <p className={styles.detailSummary}>{selected.summary}</p>
      <dl className={styles.facts}>{selected.facts.map((field, index) => <div key={index}><dt>{field.label}</dt><dd>{field.value}</dd></div>)}</dl>
      <dl className={styles.sections}>{selected.sections.map((section, index) => <div key={index} className={styles.section}>
        <dt>{section.label}</dt><dd>{section.body}</dd>
      </div>)}</dl>
      {selected.evidence.map(e => {
        const panelId = `${disclosureId}-${selected.id}-${e.id}`;
        const open = selection.evidence.includes(e.id);
        return <div key={e.id} className={styles.evidence}>
          <button type="button" id={`${panelId}-label`} className={styles.disclosureToggle} aria-expanded={open} aria-controls={panelId}
            onClick={() => onSelection({ ...selection, evidence: open ? selection.evidence.filter(id => id !== e.id) : [...selection.evidence, e.id] })}>
            <Icon name="chevron-right" size="sm" /><span><span className={styles[e.tone]}>{e.id}</span> · {e.date} · {e.label}</span>
          </button>
          <div id={panelId} hidden={!open} role="region" aria-labelledby={`${panelId}-label`}><blockquote>{e.body}</blockquote><p>{e.note}</p></div>
        </div>;
      })}
    </section>}
    </div>
  </div>;
}
