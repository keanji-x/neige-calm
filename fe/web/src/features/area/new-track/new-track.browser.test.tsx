/*
 * The Start from picker in a real engine: both layers are `popover` elements, and
 * jsdom has neither the UA close watcher that turns Escape into a light dismiss
 * nor real hover.
 */
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

// These token-free fixtures pin the desktop picker and focus contracts.
// The production-style compact overflow is exercised by new-track-model.browser.test.
beforeEach(async () => { await page.viewport(1280, 720); });

import { NewTrackForm } from './public.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

function renderForm() {
  return render(
    <NewTrackForm
      submitting={false}
      error={null}
      initialCwd={null}
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
  return screen.getByRole('button', { name: 'Repo: Neige workspace' });
}
