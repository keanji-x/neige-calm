// The two CodeMirror panes: one file, and one file's two sides.
//
// Ported from `web/src/cards/builtins/file-viewer-codemirror.tsx` and kept
// deliberately close to it — the editor wiring (search adapter, the empty
// search panel, the `/` keymap at highest precedence, the language table) is
// behaviour that was measured against real files and real engines, and
// rewriting it would have been re-deriving it.
//
// Both panes are read-only. Editing a file from a card is a different feature
// with a different failure mode (a write that races the agent working in the
// same tree), and nothing here is prepared for it: `editable` is false at both
// the component and the extension level so a future edit has to be a decision
// rather than an omission.

import { useEffect, useMemo, useRef } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { loadLanguage } from '@uiw/codemirror-extensions-langs';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import { MergeView } from '@codemirror/merge';
import { EditorView, keymap } from '@codemirror/view';
import { Prec } from '@codemirror/state';
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

export type PaneTheme = 'light' | 'dark';

/**
 * What the shared search bar drives, whatever the pane is made of. Today only
 * the code pane implements it; the type is the seam that kept the bar from
 * knowing about CodeMirror in the first place, and it is worth keeping for the
 * same reason.
 */
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
  /** The reader pressed `/` inside the editor — see the keymap below. */
  onSlashOpen?: () => void;
}

export interface DiffPaneProps {
  path: string;
  headText: string | null;
  workingText: string | null;
  theme: PaneTheme;
}

/* An empty DOM panel handed to `search()` so CodeMirror's own search UI never
   renders: the bar is React's, and two search boxes over one document is a
   worse answer than either alone. */
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

/**
 * How many matches the document holds, and which one the selection is on.
 * 1-based, `0` meaning "no current match" — which is what the bar prints as an
 * empty count rather than as `0/n`.
 */
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
      // Landing on the first hit as you type is the whole mental model of a
      // find bar; leaving the selection where it was would make the count a
      // fact about a document nobody is looking at.
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
  const viewRef = useRef<EditorView | null>(null);
  /* The three callbacks live in refs so that a caller re-creating them on every
     render cannot tear the editor down: they are read at call time, and only
     the file's identity rebuilds the extensions. */
  const onSearchAdapterReadyRef = useRef(onSearchAdapterReady);
  const onSearchCountRef = useRef(onSearchCount);
  const onSlashOpenRef = useRef(onSlashOpen);
  useEffect(() => { onSearchAdapterReadyRef.current = onSearchAdapterReady; }, [onSearchAdapterReady]);
  useEffect(() => { onSearchCountRef.current = onSearchCount; }, [onSearchCount]);
  useEffect(() => { onSlashOpenRef.current = onSlashOpen; }, [onSlashOpen]);

  const extensions = useMemo(
    () => [
      ...extensionsFor(path),
      search({ createPanel: emptyPanel }),
      /* `Prec.highest` so `/` reaches the bar before any language extension or
         default binding claims it — and before Firefox's own quick-find. */
      Prec.highest(keymap.of([{
        key: '/',
        run: () => { onSlashOpenRef.current?.(); return true; },
      }])),
    ],
    [path],
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
    /* `viewRef` is filled by `onCreateEditor`, which has not run on the first
       pass — hence the microtask. A later file change keeps the same view, so
       that branch wires immediately. */
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
      editable={false}
      basicSetup={{ lineNumbers: true, foldGutter: true }}
      onCreateEditor={(view) => { viewRef.current = view; }}
    />
  );
}

/**
 * HEAD on the left, the working tree on the right.
 *
 * The kernel sends the two texts rather than a patch, so this is a merge view
 * and not a diff parser. `null` on either side is a real state — a file that is
 * not in HEAD, or one that has been deleted — and is passed through to the DOM
 * as `data-nc-fs-empty-*` so the CSS can label the empty half instead of leaving a
 * blank pane the reader has to interpret.
 */
export function DiffPane({ path, headText, workingText, theme }: DiffPaneProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const extensions = useMemo(() => extensionsFor(path, theme), [path, theme]);

  useEffect(() => {
    const parent = ref.current;
    if (parent === null) return;
    const merge = new MergeView({
      parent,
      a: { doc: headText ?? '', extensions },
      b: { doc: workingText ?? '', extensions },
      collapseUnchanged: { margin: 3, minSize: 4 },
    });
    return () => { merge.destroy(); };
  }, [extensions, headText, workingText]);

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

function extensionsFor(path: string, theme?: PaneTheme) {
  const language = languageName(path);
  const lang = language === null
    ? null
    : loadLanguage(language as Parameters<typeof loadLanguage>[0]);
  return [
    EditorView.editable.of(false),
    EditorView.lineWrapping,
    ...(theme === undefined ? [] : [theme === 'dark' ? githubDark : githubLight]),
    ...(lang === null ? [] : [lang]),
  ];
}

/**
 * Extension → language, for the extensions this product actually opens.
 *
 * Deliberately a short table and not a lookup over everything
 * `@uiw/codemirror-extensions-langs` ships: an unknown extension falls through
 * to no highlighting, which renders the file correctly, and a wrong guess
 * renders it wrongly.
 */
function languageName(path: string): string | null {
  const extension = path.split('.').pop()?.toLowerCase();
  switch (extension) {
    case 'cjs':
    case 'cts':
    case 'js':
    case 'jsx':
    case 'mjs':
      return 'javascript';
    case 'mts':
    case 'ts':
    case 'tsx':
      return 'typescript';
    case 'rs': return 'rust';
    case 'py': return 'python';
    case 'go': return 'go';
    case 'java': return 'java';
    case 'json': return 'json';
    case 'md':
    case 'markdown':
      return 'markdown';
    case 'css': return 'css';
    case 'html': return 'html';
    case 'toml': return 'toml';
    case 'yaml':
    case 'yml':
      return 'yaml';
    case 'sh':
    case 'bash':
    case 'zsh':
      return 'shell';
    default: return null;
  }
}
