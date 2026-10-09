import { useEffect, useMemo, useRef } from 'react';
import { Compartment, EditorState } from '@codemirror/state';
import { EditorView, keymap, lineNumbers } from '@codemirror/view';
import { standardKeymap } from '@codemirror/commands';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import { useState } from '../state/public.ts';
import { useCodeLanguage } from './use-language.ts';
import type { ReadOnlyCodeProps } from './public.tsx';
import styles from './code.module.css';

export function ReadOnlyPane({ text, source, theme }: ReadOnlyCodeProps) {
  const parent = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [configuration] = useState(() => new Compartment());
  const { description, support, status } = useCodeLanguage(source, text);
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const copyGeneration = useRef(0);
  const label = status === 'ready' ? description?.name
    : status === 'loading' ? `Loading ${description?.name}…`
      : status === 'limited' ? 'Plain text · large file'
        : status === 'unavailable' ? 'Plain text · highlighting unavailable' : 'Plain text';
  const extensions = useMemo(() => [
    EditorState.readOnly.of(true), EditorView.editable.of(false),
    EditorView.contentAttributes.of({ 'aria-label': 'Code', 'aria-readonly': 'true', tabindex: '0' }),
    lineNumbers(), keymap.of(standardKeymap),
    theme === 'light' ? githubLight : githubDark,
    EditorView.theme({
      '&': { fontSize: 'inherit', backgroundColor: 'var(--surface-code)', color: 'var(--text-1)' },
      '.cm-scroller': { overflow: 'auto', maxHeight: '24rem', fontFamily: 'var(--font-code)' },
      '.cm-gutters': { backgroundColor: 'var(--surface-code)', color: 'var(--text-3)', border: 'none' },
      '.cm-content': { padding: 'var(--space-6) 0' },
      '&.cm-focused': { outline: '2px solid var(--accent)', outlineOffset: '-2px' },
    }),
    ...(support === null ? [] : [support]),
  ], [support, theme]);
  useEffect(() => {
    if (parent.current === null) return;
    const view = new EditorView({ parent: parent.current, state: EditorState.create({ extensions: configuration.of([]) }) });
    viewRef.current = view;
    return () => { viewRef.current = null; view.destroy(); };
  }, [configuration]);
  useEffect(() => {
    const view = viewRef.current;
    if (view === null) return;
    view.dispatch({ effects: configuration.reconfigure(extensions) });
  }, [configuration, extensions]);
  useEffect(() => {
    const view = viewRef.current;
    if (view === null || view.state.doc.toString() === text) return;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
    view.scrollDOM.scrollTop = 0;
    view.scrollDOM.scrollLeft = 0;
  }, [text]);
  useEffect(() => {
    copyGeneration.current += 1;
    setCopyState('idle');
    return () => { copyGeneration.current += 1; };
  }, [text, setCopyState]);
  const copy = async () => {
    const generation = ++copyGeneration.current;
    try {
      await navigator.clipboard.writeText(text);
      if (generation === copyGeneration.current) setCopyState('copied');
    } catch {
      if (generation === copyGeneration.current) setCopyState('failed');
    }
  };
  return <div className={styles.code} data-nc-code-status={status}>
    <div className={styles.toolbar}>
      <span>{label}</span>
      <button type="button" onClick={() => { void copy(); }}>Copy code</button>
      <span role="status">{copyState === 'copied' ? 'Copied' : copyState === 'failed' ? 'Copy failed' : ''}</span>
    </div>
    <div ref={parent} />
  </div>;
}
