// The two CodeMirror panes: one file, and one file's two sides. Both are read-only.

import { useEffect, useMemo, useRef } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { useState } from '../../ui/state/public.ts';
import { useCodeLanguage, type CodeTheme } from '../../ui/code/public.tsx';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import { MergeView } from '@codemirror/merge';
import { EditorView, keymap } from '@codemirror/view';
import { Compartment, EditorState, Prec } from '@codemirror/state';
import {
  SearchQuery,
  closeSearchPanel,
  findNext,
  findPrevious,
  getSearchQuery,
  openSearchPanel,
  search,
  searchPanelOpen,
  setSearchQuery,
} from '@codemirror/search';

export type PaneTheme = CodeTheme;

/** What the shared search bar drives; the seam that keeps the bar from knowing about CodeMirror. */
export interface PaneSearchAdapter {
  setQuery(pattern: string): void;
  next(): void;
  prev(): void;
  dispose(): void;
}

export interface CodePaneProps {
  path: string;
  text: string;
  theme: PaneTheme;
  onSearchAdapterReady?: (adapter: PaneSearchAdapter | null) => void;
  onSearchCount?: (current: number, total: number) => void;
  onSlashOpen?: () => void;
}

export interface DiffPaneProps {
  path: string;
  headText: string | null;
  workingText: string | null;
  theme: PaneTheme;
}

/* An empty panel handed to `search()` so CodeMirror's own search UI never renders; the bar is React's. */
function emptyPanel() {
  const dom = document.createElement('div');
  dom.className = 'fv-code-search-panel-empty';
  dom.setAttribute('aria-hidden', 'true');
  return {
    dom,
    mount() { dom.parentElement?.classList.add('fv-code-search-panels-empty'); },
    destroy() { dom.parentElement?.classList.remove('fv-code-search-panels-empty'); },
  };
}

/** 1-based; `current` of `0` means no current match. */
function computeMatchState(view: EditorView, query: SearchQuery): { current: number; total: number } {
  if (!query.valid) return { current: 0, total: 0 };
  const cursor = query.getCursor(view.state.doc);
  let total = 0;
  let current = 0;
  const selectionFrom = view.state.selection.main.from;
  const selectionTo = view.state.selection.main.to;
  let step = cursor.next();
  while (!step.done) {
    total += 1;
    if (step.value.from === selectionFrom && step.value.to === selectionTo) current = total;
    step = cursor.next();
  }
  return { current, total };
}

function buildCodeSearchAdapter(
  view: EditorView,
  onCount: (current: number, total: number) => void,
): PaneSearchAdapter {
  const emit = () => {
    const { current, total } = computeMatchState(view, getSearchQuery(view.state));
    onCount(current, total);
  };
  return {
    setQuery(pattern) {
      const query = new SearchQuery({ search: pattern, caseSensitive: false });
      if (pattern !== '' && !searchPanelOpen(view.state)) openSearchPanel(view);
      view.dispatch({ effects: setSearchQuery.of(query) });
      if (pattern === '') {
        closeSearchPanel(view);
        onCount(0, 0);
        return;
      }
      findNext(view);
      emit();
    },
    next() { findNext(view); emit(); },
    prev() { findPrevious(view); emit(); },
    dispose() {
      view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: '' })) });
      closeSearchPanel(view);
    },
  };
}

export function CodePane({
  path, text, theme, onSearchAdapterReady, onSearchCount, onSlashOpen,
}: CodePaneProps) {
  const { support } = useCodeLanguage({ kind: 'filename', value: path }, text);
  const viewRef = useRef<EditorView | null>(null);
  /* Callbacks live in refs so a caller re-creating them per render cannot tear the editor down. */
  const onSearchAdapterReadyRef = useRef(onSearchAdapterReady);
  const onSearchCountRef = useRef(onSearchCount);
  const onSlashOpenRef = useRef(onSlashOpen);
  useEffect(() => { onSearchAdapterReadyRef.current = onSearchAdapterReady; }, [onSearchAdapterReady]);
  useEffect(() => { onSearchCountRef.current = onSearchCount; }, [onSearchCount]);
  useEffect(() => { onSlashOpenRef.current = onSlashOpen; }, [onSlashOpen]);

  const extensions = useMemo(
    () => [
      EditorView.lineWrapping,
      EditorView.contentAttributes.of({ tabindex: '0', 'aria-readonly': 'true', 'aria-label': 'File code' }),
      ...(support === null ? [] : [support]),
      search({ createPanel: emptyPanel }),
      /* `Prec.highest` so `/` reaches the bar before any language/default binding — and before Firefox's quick-find. */
      Prec.highest(keymap.of([{
        key: '/',
        run: () => { onSlashOpenRef.current?.(); return true; },
      }])),
    ],
    [support],
  );

  useEffect(() => {
    let disposed = false;
    let adapter: PaneSearchAdapter | null = null;
    const wire = () => {
      const view = viewRef.current;
      if (view === null || disposed) return;
      adapter = buildCodeSearchAdapter(view, (current, total) => {
        onSearchCountRef.current?.(current, total);
      });
      onSearchAdapterReadyRef.current?.(adapter);
    };
    /* `viewRef` is filled by `onCreateEditor`, which has not run on the first pass — hence the microtask. */
    if (viewRef.current !== null) wire();
    else queueMicrotask(wire);
    return () => {
      disposed = true;
      adapter?.dispose();
      onSearchAdapterReadyRef.current?.(null);
    };
  }, [path, text]);

  return (
    <CodeMirror
      value={text}
      height="100%"
      theme={theme === 'dark' ? githubDark : githubLight}
      extensions={extensions}
      editable={true}
      readOnly={true}
      basicSetup={{ lineNumbers: true, foldGutter: true }}
      onCreateEditor={(view) => { viewRef.current = view; }}
    />
  );
}

/** HEAD on the left, the working tree on the right. `null` on either side is a real state (not in HEAD / deleted), exposed as `data-nc-fs-empty-*`. */
export function DiffPane({ path, headText, workingText, theme }: DiffPaneProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const mergeRef = useRef<MergeView | null>(null);
  const [configuration] = useState(() => new Compartment());
  const { support } = useCodeLanguage({ kind: 'filename', value: path }, (headText ?? '') + (workingText ?? ''));
  const extensions = useMemo(() => [
    EditorView.lineWrapping,
    theme === 'dark' ? githubDark : githubLight,
    ...(support === null ? [] : [support]),
  ], [support, theme]);

  useEffect(() => {
    const parent = ref.current;
    if (parent === null) return;
    const initial = [EditorState.readOnly.of(true),
      EditorView.contentAttributes.of({ tabindex: '0', 'aria-readonly': 'true' }), configuration.of([])];
    const merge = new MergeView({
      parent,
      a: { doc: headText ?? '', extensions: [initial, EditorView.contentAttributes.of({ 'aria-label': 'HEAD code' })] },
      b: { doc: workingText ?? '', extensions: [initial, EditorView.contentAttributes.of({ 'aria-label': 'Working tree code' })] },
      collapseUnchanged: { margin: 3, minSize: 4 },
    });
    mergeRef.current = merge;
    return () => { mergeRef.current = null; merge.destroy(); };
  }, [configuration, path, headText, workingText]);

  useEffect(() => {
    const merge = mergeRef.current;
    if (merge === null) return;
    merge.a.dispatch({ effects: configuration.reconfigure(extensions) });
    merge.b.dispatch({ effects: configuration.reconfigure(extensions) });
  }, [configuration, extensions, path, headText, workingText]);

  return (
    <div
      ref={ref}
      className="fv-merge"
      data-nc-fs-merge=""
      data-nc-fs-empty-left={headText === null ? 'true' : undefined}
      data-nc-fs-empty-right={workingText === null ? 'true' : undefined}
    />
  );
}

