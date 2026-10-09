import { useEffect, useMemo, useRef } from 'react';
import { Compartment, EditorState } from '@codemirror/state';
import { EditorView, keymap, lineNumbers } from '@codemirror/view';
import { standardKeymap } from '@codemirror/commands';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import { CopyCodeButton } from './copy-button.tsx';
import { useState } from '../state/public.ts';
import { useCodeLanguage } from './use-language.ts';
import type { ReadOnlyCodeProps } from './public.tsx';
import styles from './code.module.css';

export function ReadOnlyPane({ text, source, theme }: ReadOnlyCodeProps) {
  const parent = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [configuration] = useState(() => new Compartment());
  const { description, support, status } = useCodeLanguage(source, text);
  const label = status === 'ready' ? description?.name
    : status === 'loading' ? `Loading ${description?.name}…`
      : status === 'limited' ? 'Plain text · large file'
        : status === 'unavailable' ? 'Plain text · highlighting unavailable' : 'Plain text';
  const extensions = useMemo(() => [
    EditorState.readOnly.of(true),
    EditorView.contentAttributes.of({ 'aria-label': 'Code', 'aria-readonly': 'true', tabindex: '0' }),
    lineNumbers(), keymap.of(standardKeymap),
    theme === 'light' ? githubLight : githubDark,
    EditorView.theme({
      '&': { fontSize: 'inherit', backgroundColor: 'var(--surface-code)', color: 'var(--text-1)' },
      '.cm-scroller': { overflow: 'auto', maxHeight: '24rem', fontFamily: 'var(--font-code)' },
      '.cm-gutters': { backgroundColor: 'var(--bg)', color: 'var(--text-3)', border: 'none' },
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
    const top = view.scrollDOM.scrollTop;
    const left = view.scrollDOM.scrollLeft;
    const selection = view.state.selection.main;
    const document = view.state.toText(text);
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: document },
      selection: { anchor: Math.min(selection.anchor, document.length), head: Math.min(selection.head, document.length) } });
    view.scrollDOM.scrollTop = top;
    view.scrollDOM.scrollLeft = left;
  }, [text]);
  useEffect(() => {
    const view = viewRef.current;
    if (view === null) return;
    view.dispatch({ selection: { anchor: 0 } });
    view.scrollDOM.scrollTop = 0;
    view.scrollDOM.scrollLeft = 0;
  }, [source.kind, source.value]);
  return <div className={styles.code} data-nc-code-status={status}>
    <div className={styles.toolbar}>
      <span>{label}</span>
      <CopyCodeButton text={text} />
    </div>
    <div ref={parent} />
  </div>;
}
