import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { CardDockHost, createCardHost, createCardRegistry, type MountedCard, type BoardHostItem } from '../public.js';
import { TERMINAL_CARD_ENTRY } from '../builtins/terminal.ts';

afterEach(cleanup);

it('retains the single lifecycle mount while hidden, publishes visibility/focus and disposes only on removal', () => {
  const dispose = vi.fn();
  const registry = createCardRegistry();
  registry.register({ ...TERMINAL_CARD_ENTRY, createController: () => ({ dispose }), component: () => <input aria-label="retained resource" /> });
  const host = { ...createCardHost(registry) };
  const mount = vi.spyOn(host, 'mount');
  const item: BoardHostItem = { card: { type: 'terminal', id: 'one', title: 'One', terminalId: null, sessionState: null, cwd: null, gateCwd: null },
    title: 'One', originalIndex: 0, activity: null, notice: null };
  const view = render(<CardDockHost host={host} item={item} visible focused />);
  const input = view.getByRole('textbox');
  const instance = mount.mock.results[0]?.value as MountedCard;
  expect(instance.card.lifecycle.getSnapshot()).toMatchObject({ visible: true, focused: true });
  view.rerender(<CardDockHost host={host} item={item} visible={false} focused={false} />);
  expect(view.container.querySelector('input')).toBe(input);
  expect(view.container.firstElementChild?.hasAttribute('inert')).toBe(true);
  expect(instance.card.lifecycle.getSnapshot()).toMatchObject({ visible: false, focused: false });
  expect(dispose).not.toHaveBeenCalled();
  view.rerender(<CardDockHost host={host} item={item} visible focused />);
  expect(view.getByRole('textbox')).toBe(input);
  expect(mount).toHaveBeenCalledTimes(1);
  view.unmount();
  expect(dispose).toHaveBeenCalledTimes(1);
});
