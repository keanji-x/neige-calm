import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
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

it('shares the code contract with fenced code through the real chat renderer', async () => {
  render(<Reply text={'```rust\nfn main() {}\n```'} imageFiles={null} />);
  await waitFor(() => {
    const content = screen.getByRole('textbox', { name: 'Code' });
    expect(EditorView.findFromDOM(content)?.state.facet(language)?.name).toBe('rust');
  }, { timeout: 10000 });
  expect(screen.getByRole('button', { name: 'Copy code' })).toBeTruthy();
});


it('preserves readable heading typography and block spacing at every reply level', () => {
  const { container } = render(<Reply imageFiles={null} text={'# First\n\ntext\n\n## Second\n\n### Third\n\n#### Fourth'} />);
  const headings = [...container.querySelectorAll<HTMLElement>('h3, h4, h5, h6')];
  for (const [index, heading] of headings.entries()) {
    const painted = getComputedStyle(heading);
    expect(painted.fontWeight).toBe('600');
    const probe = document.createElement('span');
    probe.style.fontSize = `var(--text-heading-${index + 3}-size)`;
    probe.style.lineHeight = `var(--text-heading-${index + 3}-leading)`;
    document.body.append(probe);
    expect(painted.fontSize).toBe(getComputedStyle(probe).fontSize);
    expect(painted.lineHeight).toBe(getComputedStyle(probe).lineHeight);
    probe.remove();
    expect(Number.parseFloat(painted.lineHeight)).toBeGreaterThan(Number.parseFloat(painted.fontSize));
    expect(Number.parseFloat(painted.marginBlockStart)).toBe(index === 0 ? 0 : index < 3 ? 24 : 16);
    expect(Number.parseFloat(painted.marginBlockEnd)).toBe(index < 3 ? 12 : 8);
  }
});

it('keeps content-sized table columns readable inside their own narrow scroller', () => {
  const { container } = render(<div style={{ inlineSize: 240, overflowWrap: 'anywhere' }}><Reply imageFiles={null}
    text={'| ID | Medium label | Very long descriptive header | Extra |\n| - | - | - | - |\n| 1 | words | **long body value with spaces** | longunbrokentextvalue |'} /></div>);
  const table = container.querySelector('table')!;
  const wrap = table.parentElement!;
  const widths = [...table.querySelectorAll('th')].map(cell => cell.getBoundingClientRect().width);
  expect(widths[0]).toBeGreaterThanOrEqual(60);
  expect(widths[1]).toBeGreaterThanOrEqual(80);
  expect(widths[2]).toBeGreaterThanOrEqual(120);
  expect(widths[3]).toBeGreaterThanOrEqual(120);
  expect(wrap.scrollWidth).toBeGreaterThan(wrap.clientWidth);
  wrap.scrollLeft = 50;
  expect(wrap.scrollLeft).toBe(50);
  expect(container.firstElementChild!.scrollWidth).toBeLessThanOrEqual(240);
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

it('opens a reply image in the Lightbox on click, dismisses only the Lightbox on Escape inside a Drawer, and leaves linked images as links', async () => {
  const screenshot = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800">'
    + '<rect width="1200" height="800" fill="#eef2f6"/>'
    + '<text x="60" y="160" font-size="40">Architecture diagram</text></svg>',
  ], { type: 'image/svg+xml' }));
  const onCloseDrawer = vi.fn();
  try {
    const imageFiles = { root: '/work/track', files: { rawUrl: () => screenshot } };
    render(
      <Drawer open title="Planner" onClose={onCloseDrawer}>
        <Reply
          text={'![Architecture diagram](/work/track/diagram.svg)\n\n![](/work/track/unnamed.svg)\n\n[![Linked badge](/work/track/diagram.svg)](https://example.com/docs)'}
          imageFiles={imageFiles}
        />
      </Drawer>,
    );

    const linked = screen.getByRole('link', { name: 'Linked badge' });
    expect(linked.getAttribute('href')).toBe('https://example.com/docs');
    expect(linked.querySelector('button')).toBeNull();
    expect(document.querySelector('dialog')).toBeNull();

    const unnamedTrigger = screen.getByRole('button', { name: 'Open Thumbnail' });
    expect(unnamedTrigger.getAttribute('aria-haspopup')).toBe('dialog');

    const trigger = screen.getByRole('button', { name: 'Architecture diagram' });
    expect(trigger.getAttribute('aria-label')).toBeNull();
    expect(trigger.getAttribute('role')).toBeNull();
    expect(trigger.getAttribute('tabindex')).toBeNull();
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole('dialog', { name: 'Architecture diagram' });
    expect(within(dialog).getByRole('img', { name: 'Architecture diagram' }).getAttribute('src')).toBe(screenshot);
    expect(within(dialog).getByRole('button', { name: 'Zoom' }).getAttribute('aria-pressed')).toBe('false');

    fireEvent.keyDown(document, { key: 'Escape', bubbles: true, cancelable: true });
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Architecture diagram' })).toBeNull());
    expect(onCloseDrawer).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(trigger);
  } finally {
    URL.revokeObjectURL(screenshot);
  }
});

it('opens user message attachments in the Lightbox gallery, preserves unclipped button focus geometry, and navigates across images', async () => {
  const firstUrl = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600"><rect width="800" height="600" fill="#dbeafe"/></svg>',
  ], { type: 'image/svg+xml' }));
  const secondUrl = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="900" height="600"><rect width="900" height="600" fill="#dcfce7"/></svg>',
  ], { type: 'image/svg+xml' }));
  try {
    const conversation: Conversation = {
      provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'idle', updatedAt: 0,
    };
    render(
      <ChatThread
        hasPendingMessages={false}
        canContinue={false}
        cards={{}}
        stalled={false}
        conversation={conversation}
        turns={[{
          id: 'u1',
          author: 'you',
          text: 'Compare these two captures',
          atMs: 1,
          attachments: [
            { id: 'att-1', url: firstUrl, contentType: 'image/svg+xml', size: 100 },
            { id: 'att-2', url: secondUrl, contentType: 'image/svg+xml', size: 100 },
          ],
        }]}
      />,
    );

    expect(document.querySelector('dialog')).toBeNull();
    const secondTrigger = screen.getByRole('button', { name: 'Image 2 of 2' });
    expect(getComputedStyle(secondTrigger.parentElement!).overflow).toBe('visible');
    expect(getComputedStyle(secondTrigger).overflow).toBe('hidden');
    expect(getComputedStyle(secondTrigger).borderRadius).toBe('6px');
    secondTrigger.focus();
    fireEvent.click(secondTrigger);

    const dialog = await screen.findByRole('dialog', { name: 'Media viewer' });
    expect(within(dialog).getByText('2 / 2')).toBeTruthy();
    expect(dialog.querySelector('img')?.getAttribute('src')).toBe(secondUrl);

    fireEvent.click(within(dialog).getByRole('button', { name: 'Previous' }));
    expect(within(dialog).getByText('1 / 2')).toBeTruthy();
    expect(dialog.querySelector('img')?.getAttribute('src')).toBe(firstUrl);

    fireEvent.keyDown(dialog, { key: 'ArrowRight' });
    expect(within(dialog).getByText('2 / 2')).toBeTruthy();
    expect(dialog.querySelector('img')?.getAttribute('src')).toBe(secondUrl);

    fireEvent.click(within(dialog).getByRole('button', { name: 'Close' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Media viewer' })).toBeNull());
    expect(document.activeElement).toBe(secondTrigger);
  } finally {
    URL.revokeObjectURL(firstUrl);
    URL.revokeObjectURL(secondUrl);
  }
});
