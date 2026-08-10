// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { WavePage } from './public.tsx';
import { card, cove, renderPage, wave } from './test-fixtures.tsx';

afterEach(cleanup);

describe('WavePage header', () => {
  it('shows the wave title, the cove crumb and the lifecycle badge', () => {
    renderPage({ wave: wave({ title: 'Ship the rewrite', lifecycle: 'blocked' }) });
    expect(screen.getByRole('button', { name: 'Rename wave' }).textContent).toBe('Ship the rewrite');
    expect(screen.getByRole('button', { name: 'Work' })).toBeTruthy();
    expect(screen.getByRole('img', { name: 'Wave lifecycle: Blocked' })).toBeTruthy();
  });

  it('falls back to Unknown cove when the cove is not readable', () => {
    renderPage({ cove: undefined });
    expect(screen.getByRole('button', { name: 'Unknown cove' })).toBeTruthy();
  });

  it('falls back to the untitled label for a blank title', () => {
    renderPage({ wave: wave({ title: '  ' }) });
    expect(screen.getByRole('button', { name: 'Rename wave' }).textContent).toBe('Untitled wave');
  });

  it('navigates to the cove from both the back button and the crumb', async () => {
    const onOpenCove = vi.fn();
    renderPage({ onOpenCove });
    await userEvent.click(screen.getByRole('button', { name: 'Back to cove' }));
    await userEvent.click(screen.getByRole('button', { name: 'Work' }));
    expect(onOpenCove).toHaveBeenCalledTimes(2);
  });

  it('navigates to Today from the Today crumb', async () => {
    const onOpenToday = vi.fn();
    renderPage({ onOpenToday });
    await userEvent.click(screen.getByRole('button', { name: 'Today' }));
    expect(onOpenToday).toHaveBeenCalledTimes(1);
  });
});

describe('WavePage card inventory', () => {
  // §5.3 caps an empty state at one short sentence, so the old
  // "This wave has no cards yet" became "No cards yet." The assertion is on the
  // rendered string because that string *is* the contract here.
  it('says the wave has no cards yet when the list is empty', () => {
    renderPage({ cards: [] });
    expect(screen.getByText('No cards yet.')).toBeTruthy();
  });

  // One label per row, not two. `kind` is the card's identity and `title` its
  // label; when a card has a title, printing the kind beside it says the same
  // thing twice in a 308px panel column.
  it('labels a card by its title, and does not also print the kind', () => {
    renderPage({ cards: [card({ id: 'k1', kind: 'terminal', title: 'Build log' })] });
    expect(screen.getByText('Build log')).toBeTruthy();
    expect(screen.queryByText('terminal')).toBeNull();
  });

  it('falls back to the kind when a card has no title', () => {
    const { container } = render(<WavePage
      wave={wave()}
      cove={cove()}
      cards={[card({ id: 'k1', kind: 'notes', title: null })]}
      onOpenCove={vi.fn()}
      onOpenToday={vi.fn()}
      onRenameWave={vi.fn()}
      onDeleteWave={vi.fn()}
    />);
    // Exactly once: with no title the kind stands alone rather than twice.
    expect(container.textContent).toContain('notes');
    expect(screen.getAllByText('notes').length).toBe(1);
  });

  it('marks non-deletable cards as kernel-owned', () => {
    renderPage({ cards: [card({ id: 'k1', deletable: false }), card({ id: 'k2', deletable: true })] });
    expect(screen.getAllByText('kernel-owned').length).toBe(1);
  });

  // Deliberately gone. §5.3: an unbuilt region shows the *shape* of what is
  // coming, and nothing else — "no module path, no slice name, no apology".
  // The card list is built; there is nothing here to apologise for.
  it('does not apologise for unbuilt slices in the card panel', () => {
    const { container } = renderPage({ cards: [card({ id: 'k1' })] });
    expect(container.textContent).not.toMatch(/later slice/i);
  });
});

describe('WavePage delete', () => {
  it('does not open the confirm until the delete button is pressed', () => {
    renderPage();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('uses the shared destructive copy', async () => {
    renderPage();
    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    expect(screen.getByRole('dialog', { name: 'Delete this wave?' })).toBeTruthy();
    expect(screen.getByText(/This cannot be undone/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete wave' })).toBeTruthy();
  });

  it('cancelling closes the confirm without deleting', async () => {
    const onDeleteWave = vi.fn();
    renderPage({ onDeleteWave });
    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(onDeleteWave).not.toHaveBeenCalled();
  });

  it('confirming calls onDeleteWave and closes', async () => {
    const onDeleteWave = vi.fn(() => Promise.resolve());
    renderPage({ onDeleteWave });
    await userEvent.click(screen.getByRole('button', { name: /^Delete wave / }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
    expect(onDeleteWave).toHaveBeenCalledTimes(1);
    await screen.findByRole('button', { name: /^Delete wave / });
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
