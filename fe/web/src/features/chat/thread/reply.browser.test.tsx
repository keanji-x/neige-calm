import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
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
    const conversation: Conversation = { id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'running', updatedAt: 0 };
    const imageFiles = { root: '/work/track', files: { rawUrl: () => screenshot } };
    const reply = (id: string, text: string): ConversationTurn => ({ id, author: 'agent', text, atMs: 1 });
    const props = { conversation, imageFiles, canContinue: false, cards: {}, stalled: false };
    const { container, rerender } = render(<div style={{ inlineSize: 320 }}>
      <ChatThread {...props} turns={[reply('live', 'Here is the screenshot.')]} />
    </div>);
    const text = 'Here is the screenshot.\n\n![Screenshot](/work/track/screenshots/page.svg)';
    rerender(<div style={{ inlineSize: 320 }}>
      <ChatThread {...props} turns={[reply('live', text)]} />
    </div>);
    await waitFor(() => expect(container.querySelector<HTMLImageElement>('img')?.naturalWidth).toBe(1200));
    const image = screen.getByRole('img', { name: 'Screenshot' });
    expect(image.getBoundingClientRect().width).toBeGreaterThan(0);
    expect(image.getBoundingClientRect().width).toBeLessThanOrEqual(320);
    expect(container.firstElementChild?.scrollWidth).toBeLessThanOrEqual(320);
    rerender(<div style={{ inlineSize: 320 }}>
      <ChatThread {...props} turns={[reply('stored', text)]} />
    </div>);
    await waitFor(() => expect(container.querySelector<HTMLImageElement>('img')?.naturalWidth).toBe(1200));
  } finally {
    URL.revokeObjectURL(screenshot);
  }
});
