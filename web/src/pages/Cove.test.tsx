// Tests for the keyboard-entry rename path on CovePage (slice 3 of #56).
//
// Mirrors `Wave.test.tsx`: the EditableTitle in CovePage shares the same
// keyboard contract (Enter / F2 → edit; Escape / Enter → exit + focus
// restore) but renders as a styled <h1> instead of a plain span.

import { describe, it, expect, vi } from 'vitest';
import { render, screen, act, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { CovePage } from './Cove';
import type { Cove } from '../types';

function makeCove(): Cove {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9' };
}

describe('CovePage EditableTitle keyboard entry', () => {
  it('renders the cove title as a focusable button with Rename label', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={() => {}}
      />,
    );
    // Rendered as an intrinsic <button> nested inside an <h1> so heading
    // semantics survive — no explicit tabindex needed (buttons are
    // focusable by default).
    const title = screen.getByRole('button', { name: /Rename cove name: Atlas/i });
    expect(title.tagName).toBe('BUTTON');
    // The wrapping h1 should still be discoverable by heading nav.
    expect(screen.getByRole('heading', { level: 1 })).toContainElement(title);
  });

  it('falls back to a plain h1 when onRenameCove is absent', () => {
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
      />,
    );
    // Heading exists but is not interactive.
    expect(screen.queryByRole('button', { name: /Rename cove/i })).toBeNull();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Atlas.');
  });

  it('Enter on the title opens rename mode and focuses the input', async () => {
    const user = userEvent.setup();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={() => {}}
      />,
    );
    const title = screen.getByRole('button', { name: /Rename cove name/i });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Cove name' });
    expect(input).toBeInTheDocument();
    expect(document.activeElement).toBe(input);
  });

  it('F2 on the title opens rename mode', async () => {
    const user = userEvent.setup();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={() => {}}
      />,
    );
    const title = screen.getByRole('button', { name: /Rename cove name/i });
    title.focus();
    await user.keyboard('{F2}');
    expect(screen.getByRole('textbox', { name: 'Cove name' })).toBeInTheDocument();
  });

  it('Escape exits rename mode and restores focus to the title', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={onRename}
      />,
    );
    const title = screen.getByRole('button', { name: /Rename cove name/i });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Cove name' });
    await user.type(input, ' edits');
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('textbox', { name: 'Cove name' })).not.toBeInTheDocument();
    expect(onRename).not.toHaveBeenCalled();
    const restored = screen.getByRole('button', { name: /Rename cove name: Atlas/i });
    expect(document.activeElement).toBe(restored);
  });

  it('Enter commits a renamed value and restores focus to the title display', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <CovePage
        cove={makeCove()}
        waves={[]}
        onGo={() => {}}
        onRenameCove={onRename}
      />,
    );
    const title = screen.getByRole('button', { name: /Rename cove name/i });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Cove name' });
    // Change the value via fireEvent so we don't depend on userEvent
    // simulating the controlled-input lifecycle around an immediate
    // re-render on Enter, then dispatch the Enter key directly on the
    // input — the same path the production onKeyDown handles.
    fireEvent.change(input, { target: { value: 'Beacon' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    // useEffect-driven focus restore happens after the render flush.
    await act(async () => {
      await Promise.resolve();
    });

    expect(onRename).toHaveBeenCalledWith('c1', 'Beacon');
    const restored = screen.getByRole('button', { name: /Rename cove name: Atlas/i });
    expect(document.activeElement).toBe(restored);
  });
});
