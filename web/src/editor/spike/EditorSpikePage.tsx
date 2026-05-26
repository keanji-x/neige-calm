import { useCallback, useMemo, useRef, type CSSProperties } from 'react';
import { BaseAIPlugin } from '@platejs/ai';
import { AIChatPlugin, AIPlugin, streamInsertChunk } from '@platejs/ai/react';
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
import { getPluginType, KEYS, type Value } from 'platejs';
import { ParagraphPlugin, Plate, PlateContent, usePlateEditor } from 'platejs/react';
import { useState } from '../../shared/state';

const initialValue: Value = [
  {
    type: 'h2',
    children: [{ text: 'Plate editor spike' }],
  },
  {
    type: 'p',
    children: [
      { text: 'This paragraph is the target for preview rewrite. Try typing, ' },
      { text: 'bold', bold: true },
      { text: ', italic, undo, and redo while AI writes to the same AST.' },
    ],
  },
];

const cannedAiChunks = [
  '## AI draft\n\n',
  'Plate can stream ',
  '**Markdown**',
  ' into the editor ',
  'without waiting for the full response.\n\n',
  '- First chunked list item\n',
  '- Second chunked list item with `inline code`\n\n',
  '```ts\n',
  'const streamed = true;\n',
  '```\n',
];

export function EditorSpikePage() {
  const [streaming, setStreaming] = useState(false);
  const [previewActive, setPreviewActive] = useState(false);
  const [status, setStatus] = useState('Ready.');
  const streamRunRef = useRef(0);

  const plugins = useMemo(
    () => [
      ParagraphPlugin,
      H1Plugin.configure({
        inputRules: [HeadingRules.markdown()],
      }),
      H2Plugin.configure({
        inputRules: [HeadingRules.markdown()],
      }),
      H3Plugin.configure({
        inputRules: [HeadingRules.markdown()],
      }),
      BlockquotePlugin.configure({
        inputRules: [BlockquoteRules.markdown()],
      }),
      BoldPlugin.configure({
        inputRules: [BoldRules.markdown({ variant: '*' })],
      }),
      ItalicPlugin.configure({
        inputRules: [
          ItalicRules.markdown({ variant: '*' }),
          ItalicRules.markdown({ variant: '_' }),
        ],
      }),
      CodePlugin.configure({
        inputRules: [CodeRules.markdown()],
      }),
      StrikethroughPlugin.configure({
        inputRules: [StrikethroughRules.markdown()],
      }),
      CodeBlockPlugin.configure({
        inputRules: [CodeBlockRules.markdown({ on: 'match' })],
      }),
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

  const resetAiStreamingState = useCallback(() => {
    editor.setOption(AIChatPlugin, 'streaming', false);
    editor.setOption(AIChatPlugin, '_blockChunks', '');
    editor.setOption(AIChatPlugin, '_blockPath', null);
    editor.setOption(AIChatPlugin, '_mdxName', null);
  }, [editor]);

  const streamAiDraft = useCallback(async () => {
    if (streaming) return;

    setStreaming(true);
    setStatus('Streaming canned markdown chunks into Plate AST...');
    streamRunRef.current += 1;
    const runId = streamRunRef.current;

    resetAiStreamingState();
    editor.setOption(AIChatPlugin, 'streaming', true);

    for (const chunk of cannedAiChunks) {
      if (streamRunRef.current !== runId) return;

      streamInsertChunk(editor, chunk, {
        textProps: {
          [getPluginType(editor, KEYS.ai)]: true,
        },
      });

      await new Promise((resolve) => setTimeout(resolve, 220));
    }

    resetAiStreamingState();
    setStreaming(false);
    setStatus('Streaming complete. The inserted text remains editable.');
  }, [editor, resetAiStreamingState, streaming]);

  const stopStreaming = useCallback(() => {
    streamRunRef.current += 1;
    resetAiStreamingState();
    setStreaming(false);
    setStatus('Streaming stopped.');
  }, [resetAiStreamingState]);

  const beginRewritePreview = useCallback(() => {
    const firstParagraphIndex = editor.children.findIndex(
      (node: { type?: unknown }) => node.type === 'p',
    );
    if (firstParagraphIndex < 0) {
      setStatus('No paragraph found for preview rewrite.');
      return;
    }

    const originalBlock = structuredClone(editor.children[firstParagraphIndex]);
    const nextValue = structuredClone(editor.children) as Value;
    nextValue[firstParagraphIndex] = {
      type: 'p',
      children: [
        {
          text: 'Preview rewrite: Plate keeps this AI proposal reversible until you accept it.',
          [getPluginType(editor, KEYS.ai)]: true,
        },
      ],
    };

    editor.getTransforms(BaseAIPlugin).ai.beginPreview({
      originalBlocks: [originalBlock],
    });
    editor.tf.withoutSaving(() => {
      editor.tf.setValue(nextValue);
    });

    setPreviewActive(true);
    setStatus('Preview active. Accept commits it; reject restores the rollback point.');
  }, [editor]);

  const acceptPreview = useCallback(() => {
    const accepted = editor.getTransforms(BaseAIPlugin).ai.acceptPreview();
    setPreviewActive(false);
    setStatus(accepted ? 'Preview accepted as one AI undo batch.' : 'No preview to accept.');
  }, [editor]);

  const rejectPreview = useCallback(() => {
    const rejected = editor.getTransforms(BaseAIPlugin).ai.cancelPreview();
    setPreviewActive(false);
    setStatus(rejected ? 'Preview rejected and rollback restored.' : 'No preview to reject.');
  }, [editor]);

  return (
    <main style={styles.page}>
      <section style={styles.header}>
        <div>
          <h1 style={styles.title}>Plate AI-first editor spike</h1>
          <p style={styles.subtitle}>
            Canned AI streaming, preview accept/reject, markdown shortcuts active, and human
            editing on one Plate value.
          </p>
        </div>
        <div style={styles.status}>{status}</div>
      </section>

      <section style={styles.toolbar}>
        <button type="button" onClick={streamAiDraft} disabled={streaming}>
          Ask AI
        </button>
        <button type="button" onClick={stopStreaming} disabled={!streaming}>
          Stop
        </button>
        <button type="button" onClick={beginRewritePreview} disabled={previewActive}>
          Preview rewrite
        </button>
        <button type="button" onClick={acceptPreview} disabled={!previewActive}>
          Accept
        </button>
        <button type="button" onClick={rejectPreview} disabled={!previewActive}>
          Reject
        </button>
        <button type="button" onClick={() => editor.tf.undo()}>
          Undo
        </button>
        <button type="button" onClick={() => editor.tf.redo()}>
          Redo
        </button>
      </section>

      <section style={styles.panel}>
        <h2 style={styles.panelTitle}>Editor</h2>
        <Plate editor={editor}>
          <PlateContent
            aria-label="Plate editor spike"
            placeholder="Type here..."
            style={styles.editor}
          />
        </Plate>
      </section>
    </main>
  );
}

const styles: Record<string, CSSProperties> = {
  page: {
    display: 'flex',
    flexDirection: 'column',
    gap: 16,
    minHeight: '100%',
    padding: 24,
    background: '#f7f5ef',
    color: '#201f1a',
  },
  header: {
    display: 'flex',
    alignItems: 'flex-start',
    justifyContent: 'space-between',
    gap: 24,
  },
  title: {
    margin: 0,
    fontSize: 28,
    lineHeight: 1.15,
  },
  subtitle: {
    margin: '8px 0 0',
    maxWidth: 720,
    color: '#5c5a50',
  },
  status: {
    minWidth: 260,
    border: '1px solid #d6d0c4',
    borderRadius: 6,
    padding: '10px 12px',
    background: '#fffdf7',
    fontSize: 13,
  },
  toolbar: {
    display: 'flex',
    flexWrap: 'wrap',
    gap: 8,
  },
  panel: {
    border: '1px solid #d6d0c4',
    borderRadius: 8,
    padding: 16,
    background: '#fffdf7',
  },
  panelTitle: {
    margin: '0 0 12px',
    fontSize: 16,
  },
  editor: {
    minHeight: 420,
    border: '1px solid #d6d0c4',
    borderRadius: 6,
    padding: 16,
    background: '#ffffff',
    outline: 'none',
  },
};
