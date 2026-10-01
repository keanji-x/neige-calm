// The new-track sentence with the `@` menu the route attaches (#1881): Enter and Tab reach the menu
// only for a row it has rendered as highlighted, are dropped while it searches, and otherwise take
// the form's own path; an IME Enter is always the candidate's.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { MentionSearch, MentionSuggestion } from '../../../../core/domain/mentions.ts';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
import { useMentionTrigger } from '../../features/chat/thread/mention-trigger.tsx';

const TAG: MentionSuggestion = {
  id: 'tag:@`tag:zz`', kind: 'tag', label: '#zz', detail: '1 track', chip: '#zz', insert: '@`tag:zz`',
};

function Form({ search, onSubmit, submitBlocked = false }: {
  search: MentionSearch;
  onSubmit: () => void;
  submitBlocked?: boolean;
}) {
  const mentionTrigger = useMentionTrigger(search);
  return <NewTrackForm loadTemplate={() => Promise.reject(new Error('This fixture offers no templates'))} mentionTrigger={mentionTrigger} submitting={false} error={null} templates={[]} templatesLoaded
    initialTemplateId={null} initialCwd={null} onManageRecipes={vi.fn()} submitBlocked={submitBlocked}
    listDirectory={vi.fn(() => Promise.resolve({ path: '/', parent: null, entries: [] }))} onSubmit={onSubmit} />;
}

const field = () => screen.getByRole('combobox', { name: 'What this track should do' });
const imeEnter = () => { fireEvent.keyDown(field(), { key: 'Enter', isComposing: true }); };

/* jsdom has no layout; astryx scrolls the highlighted row into view. */
const scrollIntoView = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollIntoView');
beforeEach(() => {
  Object.defineProperty(Element.prototype, 'scrollIntoView', { configurable: true, writable: true, value: () => undefined });
});
afterEach(() => {
  cleanup();
  if (scrollIntoView === undefined) Reflect.deleteProperty(Element.prototype, 'scrollIntoView');
  else Object.defineProperty(Element.prototype, 'scrollIntoView', scrollIntoView);
});

describe('Enter in the new-track sentence while the @ menu is open', () => {
  it('enters a mention category with Enter without creating the track', async () => {
    const onSubmit = vi.fn();
    const search = vi.fn<MentionSearch>(() => Promise.resolve([TAG]));
    render(<Form search={search} onSubmit={onSubmit} />);
    await userEvent.type(field(), 'fix @');
    await screen.findByRole('option', { name: /^Tags/ });
    await userEvent.keyboard('{ArrowDown}{Enter}');
    await screen.findByRole('option', { name: /#zz/ });
    expect(field().textContent).toBe('fix @#');
    expect(search).toHaveBeenLastCalledWith('#', expect.anything());
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('with no rows, sends the sentence as typed through the form and keeps it in the field', async () => {
    const onSubmit = vi.fn();
    render(<Form search={() => Promise.resolve([])} onSubmit={onSubmit} />);
    await userEvent.type(field(), ' fix @zz');
    await screen.findByText('No matches');
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).toHaveBeenCalledWith({ message: ' fix @zz' });
    expect(field().textContent).toBe(' fix @zz');
  });

  it('with no rows and the Area unavailable, sends nothing and keeps the sentence', async () => {
    const onSubmit = vi.fn();
    render(<Form search={() => Promise.resolve([])} onSubmit={onSubmit} submitBlocked />);
    await userEvent.type(field(), 'fix @zz');
    await screen.findByText('No matches');
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).not.toHaveBeenCalled();
    expect(field().textContent).toBe('fix @zz');
  });

  it('while composing over shown rows, inserts no chip and sends nothing', async () => {
    const onSubmit = vi.fn();
    render(<Form search={() => Promise.resolve([TAG])} onSubmit={onSubmit} />);
    await userEvent.type(field(), 'fix @zz');
    await screen.findByRole('option', { name: /#zz/ });
    imeEnter();
    expect(field().querySelector('[data-astryx-token]')).toBeNull();
    expect(onSubmit).not.toHaveBeenCalled();
    expect(field().textContent).toBe('fix @zz');
  });

  it('while composing during the search, sends nothing', async () => {
    const onSubmit = vi.fn();
    render(<Form search={() => new Promise(() => undefined)} onSubmit={onSubmit} />);
    await userEvent.type(field(), 'fix @zz');
    await screen.findByText('Searching…');
    imeEnter();
    await waitFor(() => { expect(field().textContent).toBe('fix @zz'); });
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('during the search, drops Enter: the track is not created with the raw query', async () => {
    const onSubmit = vi.fn();
    render(<Form search={() => new Promise(() => undefined)} onSubmit={onSubmit} />);
    await userEvent.type(field(), 'fix @roll');
    await screen.findByText('Searching…');
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).not.toHaveBeenCalled();
    expect(field().textContent).toBe('fix @roll');
  });

  it('with a shown row, picks it', async () => {
    const onSubmit = vi.fn();
    render(<Form search={() => Promise.resolve([TAG])} onSubmit={onSubmit} />);
    await userEvent.type(field(), 'fix @zz');
    await screen.findByRole('option', { name: /#zz/ });
    await userEvent.keyboard('{Enter}');
    expect(field().querySelector('[data-astryx-token]')?.textContent).toBe('#zz');
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
