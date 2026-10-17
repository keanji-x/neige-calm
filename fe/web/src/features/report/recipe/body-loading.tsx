import { Button } from '@astryxdesign/core/Button';
import { TextArea } from '@astryxdesign/core/TextArea';
import { useEffect, type ComponentProps, type ComponentType } from 'react';
import { useState } from '../../../ui/state/public.ts';
import type { RecipeBodyEditor } from './body-editor.tsx';
import { loadRecipeBodyEditor } from './load-body-editor.ts';
import styles from './recipe.module.css';

type BodyProps = ComponentProps<typeof RecipeBodyEditor>;

/** The parent owns the draft. An explicit plain-text choice lasts until editing ends. */
export function RecipeBodyLoading(props: BodyProps) {
  const [plain, setPlain] = useState(false);
  const [Editor, setEditor] = useState<ComponentType<BodyProps> | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (plain) return;
    let cancelled = false;
    loadRecipeBodyEditor().then(
      module => { if (!cancelled) setEditor(() => module.RecipeBodyEditor); },
      () => { if (!cancelled) setFailed(true); },
    );
    return () => { cancelled = true; };
  }, [plain, setEditor, setFailed]);

  if (plain) return <TextArea id={props.id} label={props.label} value={props.value}
    onChange={props.onChange} isReadOnly={props.readOnly} rows={12} width="100%" hasSpellCheck={false}
    /* Entered only by the reader's explicit action, never by module completion. */
    hasAutoFocus />;
  if (Editor !== null) return <Editor {...props} />;
  return <div>
    <p role={failed ? 'alert' : 'status'}>{failed
      ? 'The Markdown editor could not load. You can continue in plain text.'
      : 'Loading Markdown editor…'}</p>
    <Button type="button" variant="secondary" label="Use plain text" onClick={() => setPlain(true)} />
    <pre className={styles.example}>{props.value}</pre>
  </div>;
}
