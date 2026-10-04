import { cleanup, fireEvent, render, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';

import '../../../styles/entry.css';
import { ChatThread } from './public.tsx';
import type { Conversation, ConversationActivity } from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);

const conversation: Conversation = Object.freeze({ id: 'motion', trackId: 'track', title: null, kind: 'codex', state: 'running', updatedAt: 1 });
function action(id: string, state: 'running' | 'done'): ConversationActivity {
  return { id, author: 'activity', tool: null, verb: state === 'running' ? 'Running' : 'Ran', target: 'npm test', state,
    durationMs: state === 'done' ? 1400 : null, detail: null, atMs: 1 };
}

it('uses the execution mark in a collapsed and expanded tool group and removes it when the turn ends', () => {
  const { container, rerender } = render(<ChatThread canContinue={false} conversation={conversation} turns={[action('first', 'done'), action('second', 'running')]} pending cards={{}} stalled={false} />);
  const status = container.querySelector<HTMLElement>('[role="status"]')!;
  expect(status.querySelector('[data-nc-motion="execution"]')!.checkVisibility()).toBe(true);
  expect(getComputedStyle(status.querySelector(':scope > svg:not([data-nc-motion])')!).display).toBe('none');
  fireEvent.click(container.querySelector('[aria-expanded]')!);
  expect(container.querySelector<HTMLElement>('[role="status"]')!.querySelector('[data-nc-motion="execution"]')!.checkVisibility()).toBe(true);
  rerender(<ChatThread canContinue={false} conversation={conversation} turns={[action('first', 'done'), action('second', 'running')]} cards={{}} stalled={false} />);
  expect(container.querySelector('[data-nc-motion]')).toBeNull();
  expect(container.querySelector('[role="status"]')!.checkVisibility()).toBe(false);
});

it('shows a plain check without the vendor disk on a completed group', () => {
  const { container } = render(<ChatThread canContinue={false} conversation={conversation} turns={[action('first', 'done'), action('second', 'done')]} cards={{}} stalled={false} />);
  const icon = container.querySelector('[aria-expanded="false"] > span:first-child')!;
  expect(getComputedStyle(icon.firstElementChild!).display).toBe('none');
  expect(icon.querySelector('svg')).not.toBeNull();
  expect(container.querySelector('[data-nc-motion]')).toBeNull();
});

it('keeps the tool group toggle clickable through the decorative execution animation', async () => {
  const { container } = render(<ChatThread canContinue={false} conversation={conversation} turns={[action('first', 'done'), action('second', 'running')]} pending cards={{}} stalled={false} />);
  await userEvent.click(container.querySelector<HTMLElement>('[role="status"]')!);
  expect(container.querySelector('[aria-expanded="true"]')).not.toBeNull();
});

it('places the lone execution mark before the verb and centers it within the row', () => {
  const { container } = render(<ChatThread canContinue={false} conversation={conversation} turns={[action('single', 'running')]} pending cards={{}} stalled={false} />);
  const marker = container.querySelector<SVGElement>('[data-nc-motion="execution"]')!;
  const row = container.querySelector<HTMLElement>('[data-nc-entry] > div')!;
  expect(marker.checkVisibility()).toBe(true);
  /* The painted verb, not Astryx's visually hidden `Running` status. */
  const verb = within(row).getAllByText('Running').find((element) => element.getBoundingClientRect().width > 1)!;
  const iconBox = marker.getBoundingClientRect();
  const textBox = verb.getBoundingClientRect();
  const rowBox = row.getBoundingClientRect();
  expect(container.querySelectorAll('[data-nc-motion]')).toHaveLength(1);
  expect(iconBox.right).toBeLessThanOrEqual(textBox.left);
  expect(iconBox.top + iconBox.height / 2).toBeCloseTo(rowBox.top + rowBox.height / 2, 1);
});
