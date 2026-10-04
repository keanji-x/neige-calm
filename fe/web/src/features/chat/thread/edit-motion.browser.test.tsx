/* Edit's motion against a real engine: one View Transition carries a copy of the message to the composer field, which stays put; visual only. */
import { act, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';

import { moveIntoComposer } from './edit-motion.ts';
import { useState } from '../../../ui/state/public.ts';

afterEach(() => { vi.restoreAllMocks(); document.body.replaceChildren(); });

/** The drawer card as far as the motion reads it: the message, then the composer it moves into. */
function Scene({ onUpdate }: { onUpdate: () => void }) {
  const [moved, setMoved] = useState(false);
  return (
    <div data-nc-drawer="">
      <p data-nc-turn="you" {...(moved ? { 'data-nc-editing': '' } : {})}>Original prompt</p>
      <button type="button" onClick={() => moveIntoComposer(document.querySelector('[data-nc-turn="you"]'), () => {
        onUpdate();
        setMoved(true);
      })}>Edit</button>
      <div data-nc-composer=""><div contentEditable="true" suppressContentEditableWarning>{moved ? 'Original prompt' : ''}</div></div>
    </div>
  );
}

it('carries the message into the field as one transition of at most 250 ms, keeps the message, and leaves no name behind', async () => {
  const start = vi.spyOn(document, 'startViewTransition');
  const update = vi.fn();
  render(<Scene onUpdate={update} />);
  const message = document.querySelector<HTMLElement>('[data-nc-turn="you"]')!;
  act(() => { screen.getByRole('button', { name: 'Edit' }).click(); });
  expect(start).toHaveBeenCalledOnce();
  expect(message.style.viewTransitionName).toBe('nc-edited-message');
  const transition = start.mock.results[0].value as ViewTransition;
  await transition.ready;
  expect(update).toHaveBeenCalledOnce();
  expect(message.isConnected && message.hasAttribute('data-nc-editing')).toBe(true);
  expect(message.style.viewTransitionName).toBe('');
  const field = document.querySelector<HTMLElement>('[contenteditable="true"]')!;
  expect(field.textContent).toBe('Original prompt');
  expect(field.style.viewTransitionName).toBe('nc-edited-message');
  const moving = document.getAnimations().filter((animation) =>
    (animation.effect as KeyframeEffect | null)?.pseudoElement === '::view-transition-group(nc-edited-message)');
  expect(moving.length).toBeGreaterThan(0);
  for (const animation of moving) expect(Number(animation.effect?.getTiming().duration)).toBeLessThanOrEqual(250);
  await transition.finished;
  expect(field.style.viewTransitionName).toBe('');
});

it('makes the same change at once, with no transition, under reduced motion', () => {
  const start = vi.spyOn(document, 'startViewTransition');
  vi.spyOn(window, 'matchMedia').mockImplementation((query) => ({ matches: query === '(prefers-reduced-motion: reduce)' }) as MediaQueryList);
  const update = vi.fn();
  render(<Scene onUpdate={update} />);
  act(() => { screen.getByRole('button', { name: 'Edit' }).click(); });
  expect(start).not.toHaveBeenCalled();
  expect(update).toHaveBeenCalledOnce();
  expect(document.querySelector('[data-nc-turn="you"]')?.hasAttribute('data-nc-editing')).toBe(true);
  expect(document.querySelector<HTMLElement>('[contenteditable="true"]')!.style.viewTransitionName).toBe('');
});
