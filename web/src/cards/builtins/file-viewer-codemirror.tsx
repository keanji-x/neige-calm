import CodeMirror from '@uiw/react-codemirror';
import { loadLanguage } from '@uiw/codemirror-extensions-langs';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import { MergeView } from '@codemirror/merge';
import { EditorView, keymap } from '@codemirror/view';
import { Prec } from '@codemirror/state';
import {
  SearchQuery,
  findNext,
  findPrevious,
  getSearchQuery,
  search,
  setSearchQuery,
} from '@codemirror/search';
import { useEffect, useMemo, useRef } from 'react';
import type { PaneSearchAdapter } from './file-viewer-markdown';

export interface CodePaneProps {
  path: string;
  text: string;
  theme: 'light' | 'dark';
  /** Handshake — see MarkdownPaneProps.onSearchAdapterReady. */
  onSearchAdapterReady?: (adapter: PaneSearchAdapter | null) => void;
  onSearchCount?: (current: number, total: number) => void;
  /**
   * Called when the user pressed `/` inside the editor (registered as a
   * CodeMirror keymap at the highest precedence — so Firefox quick-find
   * never fires, and vim-style command-mode keys don't collide either).
   */
  onSlashOpen?: () => void;
}

export interface DiffPaneProps {
  path: string;
  headText: string | null;
  workingText: string | null;
  theme: 'light' | 'dark';
}

// Empty DOM panel — passed to search() to suppress @codemirror/search's
// built-in search UI. We render our own bar in React.
function emptyPanel() {
  return { dom: document.createElement('div') };
}

/**
 * Count total occurrences of `query` in the editor's document, and where
 * the current selection falls in that sequence. Returns 1-based index
 * (0 when no current match is selected) and total.
 */
function computeMatchState(
  view: EditorView,
  query: SearchQuery,
): { current: number; total: number } {
  if (!query.valid) return { current: 0, total: 0 };
  const cursor = query.getCursor(view.state.doc);
  let total = 0;
  let current = 0;
  const selFrom = view.state.selection.main.from;
  const selTo = view.state.selection.main.to;
  let step = cursor.next();
  while (!step.done) {
    total += 1;
    if (step.value.from === selFrom && step.value.to === selTo) {
      current = total;
    }
    step = cursor.next();
  }
  return { current, total };
}

function buildCodeSearchAdapter(
  view: EditorView,
  onCount: (current: number, total: number) => void,
): PaneSearchAdapter {
  const emit = () => {
    const q = getSearchQuery(view.state);
    const { current, total } = computeMatchState(view, q);
    onCount(current, total);
  };
  return {
    setQuery(pattern) {
      const q = new SearchQuery({ search: pattern, caseSensitive: false });
      view.dispatch({ effects: setSearchQuery.of(q) });
      if (!pattern) {
        onCount(0, 0);
        return;
      }
      // Selecting the first match up-front matches the mental model of
      // "type, land on hit 1".
      findNext(view);
      emit();
    },
    next() {
      findNext(view);
      emit();
    },
    prev() {
      findPrevious(view);
      emit();
    },
    dispose() {
      view.dispatch({
        effects: setSearchQuery.of(new SearchQuery({ search: '' })),
      });
    },
  };
}

export function CodePane({
  path,
  text,
  theme,
  onSearchAdapterReady,
  onSearchCount,
  onSlashOpen,
}: CodePaneProps) {
  const viewRef = useRef<EditorView | null>(null);
  const onSearchAdapterReadyRef = useRef(onSearchAdapterReady);
  const onSearchCountRef = useRef(onSearchCount);
  const onSlashOpenRef = useRef(onSlashOpen);
  useEffect(() => {
    onSearchAdapterReadyRef.current = onSearchAdapterReady;
  }, [onSearchAdapterReady]);
  useEffect(() => {
    onSearchCountRef.current = onSearchCount;
  }, [onSearchCount]);
  useEffect(() => {
    onSlashOpenRef.current = onSlashOpen;
  }, [onSlashOpen]);

  const extensions = useMemo(
    () => [
      ...extensionsFor(path),
      // Suppress the built-in search panel — we render our own React bar.
      search({ createPanel: emptyPanel }),
      // `/` opens our bar. Prec.highest ensures we win over any default
      // binding CodeMirror or a language extension might install for `/`.
      Prec.highest(
        keymap.of([
          {
            key: '/',
            run: () => {
              onSlashOpenRef.current?.();
              return true;
            },
          },
        ]),
      ),
    ],
    [path],
  );

  // (Re)build the adapter when the file identity changes. `viewRef.current`
  // becomes non-null only after `onCreateEditor` fires (i.e. after the
  // first render), so we rely on onCreateEditor to trigger the initial
  // handshake via a queued microtask.
  useEffect(() => {
    let disposed = false;
    let adapter: PaneSearchAdapter | null = null;
    const wire = () => {
      const view = viewRef.current;
      if (!view || disposed) return;
      adapter = buildCodeSearchAdapter(view, (cur, total) => {
        onSearchCountRef.current?.(cur, total);
      });
      onSearchAdapterReadyRef.current?.(adapter);
    };
    // If the editor is already up (e.g. after path/text change without an
    // unmount) wire immediately; otherwise let onCreateEditor call us via
    // the microtask queue below.
    if (viewRef.current) {
      wire();
    } else {
      queueMicrotask(wire);
    }
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
      onCreateEditor={(view) => {
        viewRef.current = view;
      }}
    />
  );
}

export function DiffPane({ path, headText, workingText, theme }: DiffPaneProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const extensions = useMemo(() => extensionsFor(path, theme), [path, theme]);

  useEffect(() => {
    if (!ref.current) return;
    const merge = new MergeView({
      parent: ref.current,
      a: {
        doc: headText ?? '',
        extensions,
      },
      b: {
        doc: workingText ?? '',
        extensions,
      },
      collapseUnchanged: { margin: 3, minSize: 4 },
    });
    return () => {
      merge.destroy();
    };
  }, [extensions, headText, workingText]);

  return (
    <div
      ref={ref}
      className={`file-viewer-merge file-viewer-merge-${theme}`}
      data-wheel-pane="merge"
      data-empty-left={headText === null ? 'true' : undefined}
      data-empty-right={workingText === null ? 'true' : undefined}
    />
  );
}

function extensionsFor(path: string, theme?: 'light' | 'dark') {
  const language = languageName(path);
  const lang = language
    ? loadLanguage(language as Parameters<typeof loadLanguage>[0])
    : null;
  return [
    EditorView.editable.of(false),
    EditorView.lineWrapping,
    ...(theme ? [theme === 'dark' ? githubDark : githubLight] : []),
    ...(lang ? [lang] : []),
  ];
}

function languageName(path: string) {
  const ext = path.split('.').pop()?.toLowerCase();
  switch (ext) {
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
    case 'rs':
      return 'rust';
    case 'py':
      return 'python';
    case 'go':
      return 'go';
    case 'java':
      return 'java';
    case 'json':
      return 'json';
    case 'md':
    case 'markdown':
      return 'markdown';
    case 'css':
      return 'css';
    case 'html':
      return 'html';
    case 'toml':
      return 'toml';
    case 'yaml':
    case 'yml':
      return 'yaml';
    case 'sh':
    case 'bash':
    case 'zsh':
      return 'shell';
    default:
      return null;
  }
}
