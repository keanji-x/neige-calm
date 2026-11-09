import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { Reply } from './reply.tsx';
import { ChatThread } from './public.tsx';

afterEach(cleanup);

it('loads a local screenshot in streamed and stored replies and fits a narrow conversation', async () => {
  const screenshot = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800">'
    + '<rect width="1200" height="800" fill="#eef2f6"/>'
    + '<rect width="1200" height="70" fill="#25344b"/>'
    + '<text x="60" y="160" font-size="40">Screenshot preview</text></svg>',
  ], { type: 'image/svg+xml' }));
  try {
    const conversation: Conversation = { provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'running', updatedAt: 0 };
    const imageFiles = { root: '/work/track', files: { rawUrl: () => screenshot } };
    const reply = (id: string, text: string): ConversationTurn => ({ id, author: 'agent', text, atMs: 1 });
    const props = { conversation, imageFiles, canContinue: false, cards: {}, stalled: false };
    const { container, rerender } = render(<div style={{ inlineSize: 320 }}>
      <ChatThread hasPendingMessages={false} {...props} turns={[reply('live', 'Here is the screenshot.')]} />
    </div>);
    const text = 'Here is the screenshot.\n\n![Screenshot](/work/track/screenshots/page.svg)';
    rerender(<div style={{ inlineSize: 320 }}>
      <ChatThread hasPendingMessages={false} {...props} turns={[reply('live', text)]} />
    </div>);
    await waitFor(() => expect(container.querySelector<HTMLImageElement>('img')?.naturalWidth).toBe(1200));
    const image = screen.getByRole('img', { name: 'Screenshot' });
    expect(image.getBoundingClientRect().width).toBeGreaterThan(0);
    expect(image.getBoundingClientRect().width).toBeLessThanOrEqual(320);
    expect(container.firstElementChild?.scrollWidth).toBeLessThanOrEqual(320);
    rerender(<div style={{ inlineSize: 320 }}>
      <ChatThread hasPendingMessages={false} {...props} turns={[reply('stored', text)]} />
    </div>);
    await waitFor(() => expect(container.querySelector<HTMLImageElement>('img')?.naturalWidth).toBe(1200));
  } finally {
    URL.revokeObjectURL(screenshot);
  }
});

it('shares the code contract with Astryx fences through the real chat renderer', async () => {
  render(<Reply text="```rust\nfn main() {}\n```" imageFiles={null} />);
  await waitFor(() => {
    const content = screen.getByRole('textbox', { name: 'Code' });
    expect(EditorView.findFromDOM(content)?.state.facet(language)?.name).toBe('rust');
  }, { timeout: 10000 });
  expect(screen.getByRole('button', { name: 'Copy code' })).toBeTruthy();
});

it('aligns Markdown tables with surrounding reply prose on both inline edges while keeping inter-column padding', () => {
  const conversation: Conversation = { provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'idle', updatedAt: 0 };
  const prose = 'A long introductory paragraph that wraps across the full width of the conversation container so its inline-start and inline-end edges establish the reading measure. '.repeat(3).trim();
  const markdown = [
    '## Section summary',
    prose,
    ['| Index | Topic | Action |', '| :--- | :--- | ---: |', '| #1 | Area creation flow | Navigate to new area |'].join('\n'),
  ].join('\n\n');
  render(
    <div style={{ inlineSize: 780 }}>
      <ChatThread
        hasPendingMessages={false}
        canContinue={false}
        cards={{}}
        stalled={false}
        conversation={conversation}
        turns={[{ id: 'reply-1', author: 'agent', text: markdown, atMs: 1 }]}
      />
    </div>,
  );

  const heading = screen.getByRole('heading', { level: 4, name: 'Section summary' });
  const paragraph = screen.getByText(prose);
  const table = screen.getByRole('table');
  const [firstHeader, middleHeader, lastHeader] = screen.getAllByRole('columnheader');
  const [firstCell, middleCell, lastCell] = screen.getAllByRole('cell');

  const headingBox = heading.getBoundingClientRect();
  const paragraphBox = paragraph.getBoundingClientRect();
  const tableBox = table.getBoundingClientRect();
  const firstHeaderBox = firstHeader.getBoundingClientRect();
  const firstCellBox = firstCell.getBoundingClientRect();
  const lastHeaderBox = lastHeader.getBoundingClientRect();
  const lastCellBox = lastCell.getBoundingClientRect();

  // Prose is not capped at Astryx's 680px default inside the 740px reply column.
  expect(paragraphBox.width).toBeGreaterThan(700);
  expect(Math.abs(paragraphBox.left - headingBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(tableBox.left - paragraphBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(tableBox.right - paragraphBox.right)).toBeLessThanOrEqual(1);

  // Outer column edges sit flush with the prose and row rules; inner column edges keep their spacing.
  expect(getComputedStyle(firstHeader).paddingInlineStart).toBe('0px');
  expect(getComputedStyle(firstCell).paddingInlineStart).toBe('0px');
  expect(getComputedStyle(lastHeader).paddingInlineEnd).toBe('0px');
  expect(getComputedStyle(lastCell).paddingInlineEnd).toBe('0px');
  expect(Number.parseFloat(getComputedStyle(firstCell).paddingInlineEnd)).toBeGreaterThan(0);
  expect(Number.parseFloat(getComputedStyle(middleHeader).paddingInlineStart)).toBeGreaterThan(0);
  expect(Number.parseFloat(getComputedStyle(middleCell).paddingInlineStart)).toBeGreaterThan(0);
  expect(Number.parseFloat(getComputedStyle(lastCell).paddingInlineStart)).toBeGreaterThan(0);

  expect(Math.abs(firstHeaderBox.left - paragraphBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(firstCellBox.left - paragraphBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(lastHeaderBox.right - paragraphBox.right)).toBeLessThanOrEqual(1);
  expect(Math.abs(lastCellBox.right - paragraphBox.right)).toBeLessThanOrEqual(1);
});
