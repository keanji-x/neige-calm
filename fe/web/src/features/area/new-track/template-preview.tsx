import { useEffect, type ReactNode } from 'react';
import { Button } from '@astryxdesign/core/Button';
import type { ApiFailure } from '../../../../../core/api/types.ts';
import { readFailureOf, readFailureText } from '../../../../../core/domain/read-failure.ts';
import type { LoadTemplate, TemplateDetail } from '../../../../../core/domain/template.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './template-preview.module.css';

type PreviewState =
  | Readonly<{ id: string; source: LoadTemplate; status: 'loading' }>
  /* `failure` is the rejected read's, or `null` when the answer named another template. */
  | Readonly<{ id: string; source: LoadTemplate; status: 'error'; failure: ApiFailure | null }>
  | Readonly<{ id: string; source: LoadTemplate; status: 'ready'; detail: TemplateDetail }>;

/** Text-only preview: source comments and markup never execute in this surface. */
export function TemplatePreview({ id, title, recipeBody, loadTemplate, children, onDetail }: Readonly<{
  id: string;
  title: string;
  recipeBody?: string;
  loadTemplate: LoadTemplate;
  children?: ReactNode;
  onDetail?: (detail: TemplateDetail | null) => void;
}>) {
  const [state, setState] = useState<PreviewState>({ id, source: loadTemplate, status: 'loading' });
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (recipeBody !== undefined) return;
    let active = true;
    setState({ id, source: loadTemplate, status: 'loading' });
    void loadTemplate(id).then((detail) => {
      if (active) setState(detail.id === id ? { id, source: loadTemplate, status: 'ready', detail } : { id, source: loadTemplate, status: 'error', failure: null });
    }, (error: unknown) => { if (active) setState({ id, source: loadTemplate, status: 'error', failure: readFailureOf(error) }); });
    return () => { active = false; };
  }, [id, recipeBody, loadTemplate, retry]);
  const current = state.id === id && state.source === loadTemplate ? state : { id, source: loadTemplate, status: 'loading' as const };
  const detail = recipeBody !== undefined
    ? { id, title, description: null, instructions: null, body: recipeBody }
    : current.status === 'ready' ? current.detail : null;

  useEffect(() => {
    if (recipeBody === undefined) onDetail?.(state.id === id && state.source === loadTemplate && state.status === 'ready' ? state.detail : null);
  }, [id, recipeBody, state, onDetail, loadTemplate]);

  return <section className={styles.preview} aria-label={recipeBody === undefined ? 'Selected template' : 'Selected recipe'}>
    <h2 className={styles.title}>{title}</h2>
    <div className={styles.body}>
    <div className={styles.overview}>
    {detail === null ? <div className={styles.status} aria-live="polite">
      {current.status === 'error' ? <>
        <span>{readFailureText(current.failure, 'Could not load the template preview. Your selection is still available.')}</span>
        <Button label="Retry preview" size="sm" variant="ghost" onClick={() => setRetry((n) => n + 1)} />
      </> : 'Loading template preview…'}
    </div> : <>
      {detail.description !== null && <p className={styles.description}>{detail.description}</p>}
      {detail.instructions !== null && <details className={styles.method}>
        <summary><span className={styles.chevron}><Icon name="chevron-right" size="sm" /></span>Working method</summary>
        <ol>{detail.instructions.split('\n').filter((line) => line.trim() !== '').map((line, index) => <li key={index}>{line}</li>)}</ol>
      </details>}
      <details className={styles.source}>
        <summary><span className={styles.chevron}><Icon name="chevron-right" size="sm" /></span>{recipeBody === undefined ? 'View full template' : 'View recipe content'}</summary>
        <pre>{detail.body}</pre>
      </details>
    </>}
    </div>
    {children !== undefined && <div className={styles.inputs}>{children}</div>}
    </div>
  </section>;
}
