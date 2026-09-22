import type { NativeComponent } from '../../../../../core/domain/report-view.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './native.module.css';

type Records = Extract<NativeComponent, { kind: 'records' }>;
export function RecordBrowser({ component }: { component: Records }) {
  const [datasetId, setDatasetId] = useState(component.datasets[0]?.id ?? '');
  const [mode, setMode] = useState<'cards' | 'list'>('cards');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const data = component.datasets.find(d => d.id === datasetId) ?? component.datasets[0];
  const selected = data?.items.find(item => item.id === selectedId);
  return <div>
    <div className={styles.controls}>
      {component.datasets.length > 1 && <div className={styles.segments} aria-label="记录场景">{component.datasets.map(d =>
        <button type="button" key={d.id} aria-pressed={data?.id === d.id} onClick={() => { setDatasetId(d.id); setSelectedId(null); }}>{d.label}</button>)}</div>}
      <div className={styles.segments} aria-label="记录呈现方式">
        <button type="button" aria-pressed={mode === 'cards'} onClick={() => setMode('cards')}>卡片</button>
        <button type="button" aria-pressed={mode === 'list'} onClick={() => setMode('list')}>队列</button>
      </div>
    </div>
    {!data?.items.length ? <p className={styles.muted}>{component.emptyText}</p> : <div className={mode === 'cards' ? styles.cards : styles.list}>
      {data.items.map(item => <article key={item.id} className={styles.record}>
        <div className={styles.recordTop}><span>{item.category}</span><span className={styles[item.status.tone]}>{item.status.label}</span></div>
        <h4>{item.title}</h4><p>{item.summary}</p>
        <dl className={styles.facts}>{item.facts.map((field, index) => <div key={index}><dt>{field.label}</dt><dd>{field.value}</dd></div>)}</dl>
        <div className={styles.recordBottom}><span className={styles[item.handling.tone]}>{item.handling.label}</span>
          <button type="button" onClick={() => setSelectedId(selectedId === item.id ? null : item.id)} aria-expanded={selectedId === item.id}>查看证据</button></div>
      </article>)}
    </div>}
    {selected && <section className={styles.recordDetail} aria-label={`${selected.title} 详情`}>
      <div className={styles.controls}><h4>{selected.title}</h4><button type="button" onClick={() => setSelectedId(null)}>收起详情</button></div>
      <dl>{selected.sections.map((section, index) => <div key={index} className={styles.section}>
        <dt>{section.label}</dt><dd>{section.body}</dd>
      </div>)}</dl>
      {selected.evidence.map(e => <details key={e.id} className={styles.evidence}>
        <summary><span className={styles[e.tone]}>{e.id}</span> · {e.date} · {e.label}</summary>
        <blockquote>{e.body}</blockquote><p>{e.note}</p>
      </details>)}
    </section>}
  </div>;
}
