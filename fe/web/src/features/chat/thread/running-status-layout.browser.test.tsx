import '../../../styles/entry.css';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';
import type { ConversationActivity } from '../../../../../core/domain/conversation.ts';
import { ChatThread } from './public.tsx';

beforeEach(async () => { await commands.emulateReducedMotion(true); });
afterEach(async () => {
  cleanup(); document.documentElement.style.removeProperty('font-size');
  await commands.emulateReducedMotion(false);
});

function thought(id: string): ConversationActivity {
  return { id, author: 'activity', tool: null, verb: 'Thought', target: null, state: 'done', durationMs: null, detail: 'Reasoning', atMs: 1 };
}
const conversation = Object.freeze({ id: 'chat', trackId: 'track', provider: 'codex' as const, kind: 'codex' as const, title: null, state: 'running' as const, updatedAt: 1 });

function indicatorBesideStatus(container: HTMLElement) {
  const meta = screen.getByRole('status', { name: 'Current response status' });
  const marker = container.querySelector<HTMLElement>('[data-nc-activity="working"]')!;
  expect(marker).not.toBeNull();
  expect(marker.closest('[data-nc-current-meta]')).toBe(meta);
  const label = within(meta).getByText('Running', { exact: true });
  const iconBox = marker.getBoundingClientRect(), labelBox = label.getBoundingClientRect();
  expect(Math.abs(iconBox.top + iconBox.height / 2 - labelBox.top - labelBox.height / 2)).toBeLessThanOrEqual(1);
  expect(iconBox.right).toBeLessThanOrEqual(labelBox.left);
}

it.each([[192, 16, false], [192, 16, true], [320, 16, false], [320, 16, true], [232, 32, false], [232, 32, true]] as const)('keeps completed Thought separate from one inline Running indicator (%ipx, %ipx text, grouped: %s)', (width, font, grouped) => {
  document.documentElement.style.fontSize = `${font}px`;
  const { container } = render(<div style={{ width }}><ChatThread hasPendingMessages={false} canContinue={false} cards={{}} stalled={false}
    conversation={conversation} turns={grouped ? [thought('one'), thought('two')] : [thought('one')]} pending /></div>);
  const entry = container.querySelector<HTMLElement>('[data-nc-entry]')!;
  const header = within(entry).getByRole('button', { name: /Thought/ });
  expect(header.querySelector('svg')!.checkVisibility()).toBe(true);
  indicatorBesideStatus(container);
  const marker = container.querySelector<HTMLElement>('[data-nc-activity="working"]')!;
  const complete = header.querySelector('svg')!.getBoundingClientRect();
  const active = marker.getBoundingClientRect();
  expect(Math.abs(complete.left + complete.width / 2 - active.left - active.width / 2)).toBeLessThanOrEqual(1);
  const thoughtLabel = within(header).getByText('Thought', { exact: true }).getBoundingClientRect();
  const runningLabel = within(screen.getByRole('status', { name: 'Current response status' })).getByText('Running', { exact: true }).getBoundingClientRect();
  expect(Math.abs(thoughtLabel.left - runningLabel.left)).toBeLessThanOrEqual(1);
  expect(container.querySelectorAll('[data-nc-activity="working"]')).toHaveLength(1);
});

it('hands the live mark between the status row and an executing tool without duplication', () => {
  const done = [thought('one'), thought('two')];
  const props = { hasPendingMessages: false, canContinue: false, cards: {}, stalled: false, conversation };
  const { container, rerender } = render(<ChatThread {...props} turns={done} pending />);
  indicatorBesideStatus(container);
  const running: ConversationActivity = { ...thought('tool'), verb: 'Calling tool', state: 'running' };
  rerender(<ChatThread {...props} turns={[...done, running]} pending />);
  expect(container.querySelector('[data-nc-current-meta] [data-nc-activity="working"]')).toBeNull();
  expect([...container.querySelectorAll('[data-nc-motion="execution"]')].filter(node => node.checkVisibility({ visibilityProperty: true }))).toHaveLength(1);
  rerender(<ChatThread {...props} turns={[...done, { ...running, state: 'done' }]} pending />);
  indicatorBesideStatus(container);
  rerender(<ChatThread {...props} turns={[...done, { ...running, state: 'done' }]} />);
  expect(container.querySelector('[data-nc-activity="working"]')).toBeNull();
});
