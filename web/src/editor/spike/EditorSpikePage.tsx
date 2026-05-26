import { useMemo, type CSSProperties } from 'react';
import { AIChatPlugin, AIPlugin } from '@platejs/ai/react';
import {
  BlockquoteRules,
  BoldRules,
  CodeRules,
  HeadingRules,
  ItalicRules,
  StrikethroughRules,
} from '@platejs/basic-nodes';
import {
  BlockquotePlugin,
  BoldPlugin,
  CodePlugin,
  H1Plugin,
  H2Plugin,
  H3Plugin,
  ItalicPlugin,
  StrikethroughPlugin,
} from '@platejs/basic-nodes/react';
import { CodeBlockRules } from '@platejs/code-block';
import { CodeBlockPlugin } from '@platejs/code-block/react';
import { LinkPlugin } from '@platejs/link/react';
import { BulletedListRules, OrderedListRules } from '@platejs/list';
import { ListPlugin } from '@platejs/list/react';
import { MarkdownPlugin } from '@platejs/markdown';
import { type Value } from 'platejs';
import { ParagraphPlugin, Plate, PlateContent, usePlateEditor } from 'platejs/react';

import './editor-spike.css';

const initialValue: Value = [
  {
    type: 'h2',
    children: [{ text: 'Plate editor spike' }],
  },
  {
    type: 'p',
    children: [
      { text: 'Try markdown shortcuts: ' },
      { text: '# ', code: true },
      { text: ' for heading, ' },
      { text: '**bold**', code: true },
      { text: ', ' },
      { text: '- ', code: true },
      { text: ' for list, etc. The editor stores Plate AST; markdown is only an input shortcut.' },
    ],
  },
];

export function EditorSpikePage() {
  const plugins = useMemo(
    () => [
      ParagraphPlugin,
      H1Plugin.configure({ inputRules: [HeadingRules.markdown()] }),
      H2Plugin.configure({ inputRules: [HeadingRules.markdown()] }),
      H3Plugin.configure({ inputRules: [HeadingRules.markdown()] }),
      BlockquotePlugin.configure({ inputRules: [BlockquoteRules.markdown()] }),
      BoldPlugin.configure({ inputRules: [BoldRules.markdown({ variant: '*' })] }),
      ItalicPlugin.configure({
        inputRules: [
          ItalicRules.markdown({ variant: '*' }),
          ItalicRules.markdown({ variant: '_' }),
        ],
      }),
      CodePlugin.configure({ inputRules: [CodeRules.markdown()] }),
      StrikethroughPlugin.configure({ inputRules: [StrikethroughRules.markdown()] }),
      CodeBlockPlugin.configure({ inputRules: [CodeBlockRules.markdown({ on: 'match' })] }),
      LinkPlugin,
      ListPlugin.configure({
        inputRules: [
          BulletedListRules.markdown({ variant: '-' }),
          BulletedListRules.markdown({ variant: '*' }),
          OrderedListRules.markdown({ variant: '.' }),
        ],
      }),
      MarkdownPlugin,
      AIPlugin,
      AIChatPlugin,
    ],
    [],
  );

  const editor = usePlateEditor({
    plugins,
    value: initialValue,
  });

  return (
    <main className="editor-spike-root" style={styles.page}>
      <header style={styles.header}>
        <h1 style={styles.title}>Editor spike</h1>
        <p style={styles.subtitle}>
          Markdown shortcuts active. Cmd+Z / Shift+Cmd+Z works; buttons below mirror those.
        </p>
      </header>

      <div style={styles.toolbar}>
        <button type="button" onClick={() => editor.tf.undo()}>Undo</button>
        <button type="button" onClick={() => editor.tf.redo()}>Redo</button>
      </div>

      <Plate editor={editor}>
        <PlateContent
          aria-label="Plate editor spike"
          placeholder="Type # for heading, ** for bold, - for list…"
          className="editor-spike-surface"
        />
      </Plate>
    </main>
  );
}

const styles: Record<string, CSSProperties> = {
  page: {
    display: 'flex',
    flexDirection: 'column',
    gap: 16,
    minHeight: '100%',
    padding: '32px 24px',
    background: '#f7f5ef',
    color: '#201f1a',
    maxWidth: 860,
    margin: '0 auto',
    boxSizing: 'border-box',
  },
  header: {
    display: 'flex',
    flexDirection: 'column',
    gap: 4,
  },
  title: {
    margin: 0,
    fontSize: 22,
    lineHeight: 1.2,
    fontWeight: 600,
  },
  subtitle: {
    margin: 0,
    color: '#5c5a50',
    fontSize: 13,
  },
  toolbar: {
    display: 'flex',
    gap: 8,
    justifyContent: 'flex-end',
  },
};
