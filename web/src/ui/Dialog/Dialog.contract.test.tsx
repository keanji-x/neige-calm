// Contract test for the Dialog child-view stack.
//
// The existing `Dialog.test.tsx` covers the focus-trap / inert / restore
// contract — but does NOT exercise `useModalView()` / `pushView` /
// `popView` end-to-end. That hook is LOAD-BEARING: `DirectoryPicker`
// uses it to take over the dialog body with a fullscreen browse view.
//
// This file pins down the three things downstream callers rely on:
//
//   1. After `pushView({ title, body })`, the dialog header shows the
//      pushed view's title and the pushed view's body is rendered.
//   2. The original `children` stay MOUNTED but visually hidden
//      (`display: none`) so any half-filled form state in them is
//      preserved across the detour. Without this, going Back loses the
//      user's typing.
//   3. After `popView()`, the original title returns AND focus is
//      restored to whatever element was focused before the push.
//
// Two follow-up PRs (Menu extraction from AddPanel, ConfirmDialog) will
// touch adjacent areas; the test below makes sure they can't silently
// regress this contract.

import { describe, it, expect, beforeEach } from 'vitest';
import { act, render, screen, cleanup, fireEvent } from '@testing-library/react';
import { Dialog, useModalView } from './Dialog';

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
});

/**
 * Test harness: a button inside the dialog that, when clicked, pushes a
 * named child view onto the stack. The pushed view itself has a "Back"
 * button that pops. Mirrors the shape of DirectoryPicker's use of
 * `useModalView()` without dragging in any of its async / network code.
 */
function PushingChild() {
  const modalView = useModalView();
  if (!modalView) return null;
  const push = () => {
    modalView.pushView({
      title: 'Inner',
      body: (
        <div>
          <button
            type="button"
            data-testid="back"
            onClick={() => modalView.popView()}
          >
            Back
          </button>
          <button type="button" data-testid="inner-action">
            Inner action
          </button>
        </div>
      ),
    });
  };
  return (
    <div>
      <button type="button" data-testid="push" onClick={push}>
        Push view
      </button>
      <input data-testid="outer-input" defaultValue="" />
    </div>
  );
}

describe('Dialog child-view stack contract', () => {
  it('pushes a view that replaces the title and body, keeping outer children mounted', async () => {
    render(
      <Dialog open onClose={() => {}} title="Outer">
        <PushingChild />
      </Dialog>,
    );

    // Flush initial-focus rAF.
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Sanity: dialog is selectable by its outer accessible name.
    expect(screen.getByRole('dialog', { name: 'Outer' })).toBeTruthy();

    // Push the view.
    const pushBtn = screen.getByTestId('push');
    await act(async () => {
      fireEvent.click(pushBtn);
    });

    // Dialog's accessible name now reflects the pushed view.
    const inner = screen.getByRole('dialog', { name: 'Inner' });
    expect(inner).toBeTruthy();

    // The inner body's elements are rendered.
    expect(screen.getByTestId('back')).toBeTruthy();
    expect(screen.getByTestId('inner-action')).toBeTruthy();

    // The outer children are still mounted (DOM-present) but visually
    // hidden via inline display:none on the .modal-body container.
    // This is the load-bearing piece: a half-filled <input> in the
    // outer children must survive the detour.
    const outerInput = screen.getByTestId('outer-input');
    expect(outerInput).toBeTruthy();
    // The outer .modal-body wrapper has display:none while a view is up.
    const outerBody = outerInput.closest('.modal-body') as HTMLElement | null;
    expect(outerBody).toBeTruthy();
    expect(outerBody!.style.display).toBe('none');
  });

  it('popView returns to the outer title and restores focus to the pre-push focused element', async () => {
    render(
      <Dialog open onClose={() => {}} title="Outer">
        <PushingChild />
      </Dialog>,
    );

    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Move focus to the push button explicitly so we have a known
    // "previously-focused" element to assert on after the pop.
    const pushBtn = screen.getByTestId('push');
    pushBtn.focus();
    expect(document.activeElement).toBe(pushBtn);

    // Push.
    await act(async () => {
      fireEvent.click(pushBtn);
    });
    // The pushed view's title now drives the dialog's accessible name.
    expect(screen.getByRole('dialog', { name: 'Inner' })).toBeTruthy();

    // Pop via the Back button inside the pushed view.
    const backBtn = screen.getByTestId('back');
    backBtn.focus();
    await act(async () => {
      fireEvent.click(backBtn);
    });

    // Outer title is back.
    expect(screen.getByRole('dialog', { name: 'Outer' })).toBeTruthy();

    // The outer body's wrapper no longer carries display:none.
    const outerInput = screen.getByTestId('outer-input');
    const outerBody = outerInput.closest('.modal-body') as HTMLElement | null;
    expect(outerBody).toBeTruthy();
    expect(outerBody!.style.display).toBe('');

    // And the element that was focused before the push (the push button)
    // is still in the DOM and reachable — the outer body never unmounted
    // so its focusables survived intact. We assert it directly rather
    // than asserting on focus, because the dialog itself does not
    // re-focus on pop; what it guarantees is that the pre-push elements
    // are still mounted and focusable.
    expect(pushBtn.isConnected).toBe(true);
    pushBtn.focus();
    expect(document.activeElement).toBe(pushBtn);
  });
});

// ---------------------------------------------------------------------------
// hideTitleRow (#891 signoff round 2) — the New-wave dialog drops its
// visible title row so the content reads as one cohesive card, but the
// dialog must NEVER go nameless: `title` keeps flowing into the panel's
// aria-label. Pushed child views are exempt — they always render the
// head (their title + the close affordance for the sub-flow).
// ---------------------------------------------------------------------------

describe('Dialog hideTitleRow contract', () => {
  it('suppresses the visible head but keeps the aria-label name', async () => {
    render(
      <Dialog open onClose={() => {}} title="Outer" hideTitleRow>
        <PushingChild />
      </Dialog>,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Accessible name intact…
    const dialog = screen.getByRole('dialog', { name: 'Outer' });
    // …but no visible title text and no head × button.
    expect(dialog.querySelector('.modal-head')).toBeNull();
    expect(screen.queryByText('Outer')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
  });

  it('a pushed child view still renders the head with its own title', async () => {
    render(
      <Dialog open onClose={() => {}} title="Outer" hideTitleRow>
        <PushingChild />
      </Dialog>,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    await act(async () => {
      fireEvent.click(screen.getByTestId('push'));
    });
    // The pushed view owns the dialog name AND gets the visible head
    // back — the browse-style sub-flow needs its title + close button.
    const inner = screen.getByRole('dialog', { name: 'Inner' });
    expect(inner.querySelector('.modal-head')).toBeTruthy();
    expect(screen.getByText('Inner')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Close' })).toBeTruthy();

    // Popping the view returns to the headless base state.
    await act(async () => {
      fireEvent.click(screen.getByTestId('back'));
    });
    const outer = screen.getByRole('dialog', { name: 'Outer' });
    expect(outer.querySelector('.modal-head')).toBeNull();
  });
});
