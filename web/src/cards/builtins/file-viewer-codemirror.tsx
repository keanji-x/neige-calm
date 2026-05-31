import CodeMirror from '@uiw/react-codemirror';
import { loadLanguage } from '@uiw/codemirror-extensions-langs';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import { MergeView } from '@codemirror/merge';
import { EditorView } from '@codemirror/view';
import { useEffect, useMemo, useRef } from 'react';

export interface CodePaneProps {
  path: string;
  text: string;
  theme: 'light' | 'dark';
}

export interface DiffPaneProps {
  path: string;
  headText: string | null;
  workingText: string | null;
  theme: 'light' | 'dark';
}

export function CodePane({ path, text, theme }: CodePaneProps) {
  const extensions = useMemo(() => extensionsFor(path), [path]);
  return (
    <CodeMirror
      value={text}
      height="100%"
      theme={theme === 'dark' ? githubDark : githubLight}
      extensions={extensions}
      editable={false}
      basicSetup={{ lineNumbers: true, foldGutter: true }}
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
