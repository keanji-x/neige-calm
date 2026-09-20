// The recipe body's text editor: CodeMirror, Markdown, soft wrap, nothing else.
// Its own module so jsdom-tier tests can substitute it: CodeMirror measures a layout jsdom does not have.

import { EditorView } from '@codemirror/view';
import { loadLanguage } from '@uiw/codemirror-extensions-langs';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import CodeMirror from '@uiw/react-codemirror';
import { useMemo } from 'react';

/** Light or dark, resolved by `app/theme` and injected: `features/**` may not import `app/**`. */
export type RecipeEditorTheme = 'light' | 'dark';

export function RecipeBodyEditor({ id, value, theme, label, onChange }: Readonly<{
  id: string;
  value: string;
  theme: RecipeEditorTheme;
  /** The editable's accessible name, carried as an attribute: CodeMirror's `contenteditable` has no control for a `<label for>`. */
  label: string;
  onChange: (next: string) => void;
}>) {
  const extensions = useMemo(() => {
    /* `contentAttributes`, not an `aria-label` prop: `@uiw/react-codemirror` spreads unknown props onto the wrapper div, leaving the editable unnamed. */
    const named = EditorView.contentAttributes.of({ 'aria-label': label });
    const markdown = loadLanguage('markdown');
    return markdown === null
      ? [EditorView.lineWrapping, named]
      : [EditorView.lineWrapping, named, markdown];
  }, [label]);

  return (
    <CodeMirror
      id={id}
      value={value}
      theme={theme === 'dark' ? githubDark : githubLight}
      extensions={extensions}
      basicSetup={{ lineNumbers: true, foldGutter: false }}
      onChange={onChange}
    />
  );
}
