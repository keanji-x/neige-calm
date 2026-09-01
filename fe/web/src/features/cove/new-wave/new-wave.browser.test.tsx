/*
 * #1209 — the two things about the Start from picker that only a real engine
 * can answer, and that jsdom answers *wrongly*.
 *
 * Both layers here are `popover` elements. jsdom implements enough of the
 * Popover API to render them and to hide them from the accessibility tree, but
 * not the parts that live in the browser itself: the UA close watcher that
 * turns Escape into a light dismiss, and real hover.
 *
 *   1. **The picker is dismissible from an option that owns a hover card.**
 *      `useHoverCard` attaches a native `keydown` listener to its trigger that
 *      calls `stopPropagation()` on Escape — and because the trigger *is* the
 *      menu item, that listener sits below `DropdownMenu`'s React `onKeyDown`,
 *      which is delegated at the root and therefore never runs. Escape's
 *      effect on the *menu* is left to the engine and is not stable; Tab is,
 *      and Tab is what this pins. Without a real engine there is no top layer
 *      and no light dismiss to measure at all.
 *   2. **Hovering the option — the name, with no separate trigger — opens the
 *      card.** That is the whole of the user-visible change, and it cannot be
 *      driven in jsdom, which has no pointer.
 */
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WaveTemplate } from '../../../../../core/domain/wave.ts';
import { NewWaveForm } from './public.tsx';

/* The template chip, matched on either of the two things it can say: it asks
   while nothing is chosen and names the choice after. Never on the whole
   string — the rest of the name is what the assertions vary. */
const TEMPLATE_CHIP = /^Choose a template$|^Template: /;

const TEMPLATES: readonly WaveTemplate[] = [{
  id: 'small-change',
  title: 'Small change',
  tasks: [
    { key: 'inspect', goal: 'Read the requested change and the code it touches.' },
    { key: 'implement', goal: 'Implement the change and commit it.' },
  ],
}];

afterEach(() => { document.body.replaceChildren(); });

function renderForm() {
  return render(
    <NewWaveForm
      submitting={false}
      error={null}
      templates={TEMPLATES}
      /* #1147 S3 — the folder picker's port. Never exercised here: this file
         is about the Start from menu's top layer. It is passed because the
         prop is required, which is deliberate — an optional one would let a
         call site render a picker that silently lists nothing. */
      listDirectory={vi.fn(() => Promise.resolve({ path: '/', parent: null, entries: [] }))}
      titleRef={{ current: null }}
      onCancel={vi.fn()}
      onSubmit={vi.fn()}
    />,
  );
}

function trigger(): HTMLButtonElement {
  return screen.getByRole('button', { name: TEMPLATE_CHIP });
}

async function openMenu() {
  await userEvent.click(trigger());
  await waitFor(() => { expect(screen.getByRole('menu')).toBeTruthy(); });
  const layer = screen.getByRole('menu').closest('[popover]');
  expect(layer?.matches(':popover-open')).toBe(true);
  return layer as HTMLElement;
}

describe('Start from, in a real engine', () => {
  it('dismisses cleanly from an option that owns a hover card', async () => {
    renderForm();
    const menu = await openMenu();
    const option = screen.getByRole('menuitem', { name: /^Small change/ });
    const card = document.getElementById(option.getAttribute('aria-describedby') ?? '');

    /* Focus alone opens the card — the keyboard path. Asserted on its *shown*
       state and not on the `aria-describedby` wiring, which is present from
       first paint and would pass without the card ever opening. */
    option.focus();
    await waitFor(() => { expect(card?.matches(':popover-open')).toBe(true); });

    /* Escape dismisses the card. This much is `useHoverCard`'s own doing —
       its native keydown listener on the trigger — and it is deterministic. */
    await userEvent.keyboard('{Escape}');
    await waitFor(() => { expect(card?.matches(':popover-open')).toBe(false); });

    /* Whether the *menu* also goes is not asserted, and that is a finding, not
       an omission: `useHoverCard`'s listener calls `stopPropagation()` on
       Escape, so `DropdownMenu`'s React `onKeyDown` — delegated at the root —
       never runs, and the menu's dismissal is left to the UA's close request
       against `popover="auto"`. Which layer that request lands on depends on
       whether the DOM listener already hid the card, i.e. on a race between a
       listener and the engine; measured here it goes both ways run to run.
       So the escape hatch that *is* pinned is the one that does not depend on
       that race — Tab, which `DropdownMenu` handles itself (menu items are
       `tabIndex={-1}`, so the APG menu-button pattern closes on Tab and hands
       focus back to the trigger). That is what makes this not a keyboard
       trap, which is the property that actually matters. */
    await userEvent.keyboard('{Tab}');
    await waitFor(() => {
      expect(menu.matches(':popover-open')).toBe(false);
      expect(trigger().getAttribute('aria-expanded')).toBe('false');
    });

    /* And the picker still works afterwards. Closing leaves two states behind
       — the DOM's and React's — and if only the DOM had closed, this click
       would be read as "close" and swallowed, leaving a picker that takes two
       clicks to open for the rest of the dialog's life. */
    /* The 100 ms is astryx's, not padding: `DropdownMenu` keeps a
       `lastHideTimeRef` and swallows any trigger click within 50 ms of a hide,
       so iOS Safari's pointerdown-then-click cannot re-open what light dismiss
       just closed. Clicking inside that window would test the guard. */
    await new Promise((resolve) => { setTimeout(resolve, 100); });
    await userEvent.click(trigger());
    await waitFor(() => { expect(menu.matches(':popover-open')).toBe(true); });
  });

  it('opens the task card by hovering the option itself', async () => {
    renderForm();
    await openMenu();
    const option = screen.getByRole('menuitem', { name: /^Small change/ });
    const cardId = option.getAttribute('aria-describedby') ?? '';
    expect(cardId).not.toBe('');
    const card = document.getElementById(cardId);
    expect(card?.matches(':popover-open')).toBe(false);

    await userEvent.hover(option);
    // `HoverCard`'s show delay is 300 ms; `waitFor` outlasts it.
    await waitFor(() => { expect(card?.matches(':popover-open')).toBe(true); }, { timeout: 2000 });
    expect(card?.textContent).toContain('implement');
  });
});
