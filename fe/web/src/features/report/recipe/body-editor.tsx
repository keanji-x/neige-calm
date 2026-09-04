// The recipe body's text editor: CodeMirror, Markdown, soft wrap, nothing else.
//
// A module of its own for one reason, and it is the same reason
// `systems/fs-viewers` keeps `code-pane.tsx` separate: **CodeMirror measures a
// layout jsdom does not have.** The editor's real subject — what a save sends,
// what the view renders afterwards, what a 409 leaves standing — is logic that
// belongs in the jsdom tier, and it can only be tested there if the one part
// that needs a real engine is a module a test can substitute. Keeping the
// widget inline would have forced every one of those assertions into a browser
// run, where they would be slower and no more true.
//
// It is deliberately thin, and thinner than the file viewer's pane: no search
// adapter, no `/` keymap, no language table. Those answer questions this
// surface does not have — one document, one language, no find bar — and
// copying them here would be re-deriving behaviour that was measured against
// real files, for a screen that has none.

import { EditorView } from '@codemirror/view';
import { loadLanguage } from '@uiw/codemirror-extensions-langs';
import { githubDark, githubLight } from '@uiw/codemirror-theme-github';
import CodeMirror from '@uiw/react-codemirror';
import { useMemo } from 'react';

/** Light or dark, resolved by `app/theme` and injected — `features/**` may not
 *  import `app/**`, and CodeMirror needs a concrete theme extension. */
export type RecipeEditorTheme = 'light' | 'dark';

export function RecipeBodyEditor({ id, value, theme, label, onChange }: Readonly<{
  id: string;
  value: string;
  theme: RecipeEditorTheme;
  /** The editable's accessible name. CodeMirror's `contenteditable` has no
   *  labelled control to attach to, so the name is carried as an attribute
   *  rather than by a `<label for>`. */
  label: string;
  onChange: (next: string) => void;
}>) {
  /* `EditorView.lineWrapping` because a recipe body is prose and fences, not
     code with a column budget: a horizontal scrollbar on a paragraph is a
     worse reading surface than a wrapped line. `loadLanguage` returning `null`
     is a real branch — the language pack is lazily resolved — and the honest
     answer to it is plain text, not a crash. */
  const extensions = useMemo(() => {
    /*
     * `contentAttributes`, not an `aria-label` prop on the component.
     * `@uiw/react-codemirror` spreads unknown props onto the **wrapper** div,
     * and the element carrying `role="textbox"` is the `.cm-content`
     * `contenteditable` inside it — so a prop lands the name on a node with no
     * role and leaves the editable unnamed. Measured: the browser case could
     * not find the field by name until this moved here.
     */
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
