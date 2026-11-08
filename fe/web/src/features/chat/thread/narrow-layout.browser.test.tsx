import '../../../styles/entry.css';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { commands, userEvent } from 'vitest/browser';
import type { ConversationActivity } from '../../../../../core/domain/conversation.ts';
import { ChatComposer, ChatThread } from './public.tsx';
import { ContextRing } from './context-ring.tsx';
import { ThreadStatusNotice } from './status-notice.tsx';

beforeEach(async () => { await commands.emulateReducedMotion(true); });
afterEach(async () => {
  cleanup(); document.documentElement.style.removeProperty('font-size');
  await commands.emulateReducedMotion(false);
});

function completed(): ConversationActivity {
  return { id: 'search', author: 'activity', tool: null, verb: 'Searched the web', target: 'a-long-search-query',
    state: 'done', durationMs: 600000, detail: 'Search results', atMs: 1 };
}

it.each([
  [112, 16, false], [112, 16, true], [192, 32, false], [192, 32, true],
] as const)('keeps detail glyphs inside a %ipx column at %ipx text (grouped: %s)', (width, font, grouped) => {
  document.documentElement.style.fontSize = `${font}px`;
  const turns = [completed(), ...(grouped ? [{ ...completed(), id: 'other', verb: 'Thought' }] : [])];
  const { container } = render(<div style={{ width }}>
    <ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false}
      conversation={{ id: 'chat', provider: 'codex', trackId: 'track', title: null, kind: 'codex', state: 'idle', updatedAt: 1 }} turns={turns} />
  </div>);
  const group = container.querySelector<HTMLElement>('[data-nc-entry]')!;
  if (grouped) fireEvent.click(group.querySelector<HTMLElement>(':scope > [aria-expanded]')!);
  const row = screen.getByText('Searched the web').parentElement!;
  const clip = grouped ? group.children[1].firstElementChild! : group;
  const bounds = clip.getBoundingClientRect();
  for (const svg of row.querySelectorAll('svg')) {
    const rect = svg.getBoundingClientRect();
    expect(svg.checkVisibility({ visibilityProperty: true })).toBe(true);
    expect(rect.width).toBeGreaterThan(0);
    expect(rect.left).toBeGreaterThanOrEqual(bounds.left - 1);
    expect(rect.right).toBeLessThanOrEqual(bounds.right + (grouped ? 1 : 5));
  }
  fireEvent.click(row);
  expect(screen.getByText('Search results').checkVisibility()).toBe(true);
});

it.each([[192, 20], [232, 32], [288, 16]])('separates status text and actions at %ipx / %ipx text before and after hover', async (width, font) => {
  document.documentElement.style.fontSize = `${font}px`;
  render(<div style={{ width }}><ThreadStatusNotice heading="Interrupted" clock={{ elapsedMs: 27000, timestamp: { kind: 'finished', atMs: 1000 } }}
    copyAction={{ id: 'copy', text: 'Reply', run: vi.fn().mockResolvedValue(undefined) }}
    editAction={{ id: 'edit', run: vi.fn() }} regenerateAction={{ id: 'regenerate', run: vi.fn().mockResolvedValue(undefined) }}>
    <p>Interrupted details</p>
  </ThreadStatusNotice></div>);
  const meta = screen.getByRole('status', { name: 'Current response status' });
  const label = screen.getByText('Interrupted', { exact: true });
  const check = () => {
    const text = label.parentElement!.parentElement!.getBoundingClientRect();
    const actions = screen.getByRole('group', { name: 'Response actions' }).getBoundingClientRect();
    const overlapX = Math.min(text.right, actions.right) - Math.max(text.left, actions.left);
    const overlapY = Math.min(text.bottom, actions.bottom) - Math.max(text.top, actions.top);
    expect(overlapX <= 1 || overlapY <= 1).toBe(true);
    const bounds = meta.getBoundingClientRect();
    for (const button of screen.getByRole('group', { name: 'Response actions' }).querySelectorAll('button')) {
      expect(button.getBoundingClientRect().left).toBeGreaterThanOrEqual(bounds.left - 1);
      expect(button.getBoundingClientRect().right).toBeLessThanOrEqual(bounds.right + 1);
      expect(button.getBoundingClientRect().height).toBeGreaterThanOrEqual(28);
    }
  };
  check(); await userEvent.hover(label); expect(meta.querySelector('time')).not.toBeNull(); check();
  const trigger = meta.querySelector<HTMLElement>('[aria-expanded]')!;
  await userEvent.click(trigger); expect(screen.getByText('Interrupted details').checkVisibility()).toBe(true); check();
});

it.each(['send', 'stop', 'image', 'edit'] as const)('keeps the %s action reachable beside context usage in a narrow composer', async (mode) => {
  const run = vi.fn();
  const { container } = render(<div style={{ width: 160 }}><ChatComposer onSend={run}
    onStop={mode === 'stop' ? run : undefined} allowEmptyText={mode === 'image'}
    draft={{ text: mode === 'image' ? '' : 'A draft', onChange: vi.fn() }}
    editing={mode === 'edit' ? { preview: 'Original message', onCancel: vi.fn() } : undefined}
    sendAdornment={<ContextRing usage={{ used_tokens: 5000, context_window: 10000, percent: 50, at_ms: 1 }} />} /></div>);
  const label = mode === 'stop' ? 'Stop' : mode === 'image' ? 'Send image' : mode === 'edit' ? 'Replace message' : 'Send';
  const button = screen.getByRole('button', { name: label });
  const bounds = container.querySelector('[data-nc-composer]')!.getBoundingClientRect();
  const box = button.getBoundingClientRect();
  expect(box.left).toBeGreaterThanOrEqual(bounds.left - 1);
  expect(box.right).toBeLessThanOrEqual(bounds.right + 1);
  await userEvent.click(button); expect(run).toHaveBeenCalledTimes(1);
});

it.each([192, 280])('keeps long messages, code and tables inside a %ipx conversation column', (width) => {
  const word = 'long-path-segment-'.repeat(30);
  const reply = `A reply with https://example.invalid/${word}\n\n\`\`\`text\n${word}\n\`\`\`\n\n| File | Result |\n| --- | --- |\n| ${word} | Checked |`;
  const { container } = render(<div style={{ width }}><ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false}
    conversation={{ id: 'chat', provider: 'codex', trackId: 'track', title: null, kind: 'codex', state: 'idle', updatedAt: 1 }}
    turns={[{ id: 'user', author: 'you', text: word, atMs: 1 }, { id: 'reply', author: 'agent', text: reply, atMs: 2 }]} /></div>);
  const thread = container.querySelector<HTMLElement>('[data-nc-thread]')!;
  expect(thread.scrollWidth).toBeLessThanOrEqual(thread.clientWidth + 1);
  expect(screen.getByText('Checked').checkVisibility()).toBe(true);
});
