/*
 * The Start from picker in a real engine: both layers are `popover` elements, and
 * jsdom has neither the UA close watcher that turns Escape into a light dismiss
 * nor real hover.
 */
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

// These token-free fixtures pin the desktop picker and focus contracts.
// The production-style compact overflow is exercised by new-track-model.browser.test.
beforeEach(async () => { await page.viewport(1280, 720); });

import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import { NewTrackForm } from './public.tsx';

/* The template chip, matched on its prefix only; the rest of the name is what the assertions vary. */
const TEMPLATE_CHIP = /^Template: /;

const TEMPLATES: readonly TrackTemplate[] = [{
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
    <NewTrackForm
      submitting={false}
      error={null}
      templates={TEMPLATES}
      templatesLoaded
      initialTemplateId={null}
      initialCwd={null}
      onManageRecipes={vi.fn()}
      /* Never exercised here; the prop is required so a call site cannot render a
               picker that silently lists nothing. */
      listDirectory={vi.fn(() => Promise.resolve({ path: '/', parent: null, entries: [] }))}
      onSubmit={vi.fn()}
    />,
  );
}

/* The composer's focus ring, in a real browser because that is where `:has()`
 * resolves: the editable sets `outline: none`, and axe-core does not test focus
 * visibility. `:has(… :focus)` rather than `:focus-within`, so a focused chip does
 * not also ring the composer. */
describe('the composer focus ring', () => {
  /* `--accent` is supplied because this project loads CSS Modules but not
   * `styles/tokens.css`, and an undefined `var()` makes the whole declaration invalid. */
  beforeEach(() => { document.documentElement.style.setProperty('--accent', 'rgb(0, 0, 255)'); });
  afterEach(() => { document.documentElement.style.removeProperty('--accent'); });

  function composerBody(): HTMLElement {
    const editable = screen.getByLabelText('What this track should do');
    const body = editable.closest('[data-density] > *');
    if (!(body instanceof HTMLElement)) throw new Error('composer body not found');
    return body;
  }

  it('shows a ring while the field has focus, and none once it leaves', async () => {
    renderForm();
    const field = screen.getByLabelText('What this track should do');
    /* The page focuses the field on arrival, so "at rest" has to be reached by blurring. */
    await waitFor(() => { expect(document.activeElement).toBe(field); });
    expect(getComputedStyle(composerBody()).boxShadow).not.toBe('none');

    field.blur();
    await waitFor(() => { expect(document.activeElement).not.toBe(field); });
    expect(getComputedStyle(composerBody()).boxShadow).toBe('none');
  });

  it('does not ring the whole composer when a chip takes focus', async () => {
    renderForm();
    trigger().focus();
    await waitFor(() => { expect(document.activeElement).toBe(trigger()); });
    expect(getComputedStyle(composerBody()).boxShadow).toBe('none');
  });
});

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
  it('dismisses the hover card and menu together with one Escape', async () => {
    renderForm();
    const menu = await openMenu();
    const option = screen.getByRole('menuitem', { name: /^Small change/ });
    const card = document.getElementById(option.getAttribute('aria-describedby') ?? '');

    /* Asserted on the card's shown state, not on `aria-describedby`, which is
           present from first paint. */
    option.focus();
    await waitFor(() => { expect(card?.matches(':popover-open')).toBe(true); });

    /* The Template pill owns Escape in capture, before the HoverCard's native
           listener can strand DropdownMenu's delegated handler. */
    await userEvent.keyboard('{Escape}');
    await waitFor(() => {
      expect(card?.matches(':popover-open')).toBe(false);
      expect(menu.matches(':popover-open')).toBe(false);
      expect(trigger().getAttribute('aria-expanded')).toBe('false');
      expect(document.activeElement).toBe(trigger());
    });

    /* Closing leaves two states behind, the DOM's and React's; if only the DOM had
           closed this click would be read as "close" and swallowed. */
    /* The 100 ms is astryx's: `DropdownMenu` swallows any trigger click within 50 ms
           of a hide, for iOS Safari's pointerdown-then-click. */
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
