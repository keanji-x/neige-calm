// Unit tests for `Dialog`'s focus contract (Slice 2 of issue #56).
//
// Coverage (per the contract documented at the top of Dialog.tsx):
//
//   1. Opening the dialog moves focus into the panel (initial focus).
//   2. Tab from the last focusable wraps to the first.
//   3. Shift+Tab from the first focusable wraps to the last.
//   4. Closing the dialog restores focus to the previously-focused
//      element (typically the trigger button).
//   5. Background siblings of the portal root get `inert` +
//      `aria-hidden` while the dialog is open and have those cleared on
//      close.
//   6. A caller-provided `initialFocusRef` takes precedence over the
//      default first-focusable behavior.
//
// The trap itself runs entirely client-side; no network mocks are
// needed. jsdom doesn't honor `inert` reachability when computing
// tab order, so for the wrap tests we synthesize `Tab` keydown events
// directly rather than walking through every element with
// `userEvent.tab()` — the production behavior we care about is the
// handler's preventDefault + focus call, which we can observe exactly.

import { describe, it, expect, beforeEach } from 'vitest';
import { act, render, screen, cleanup } from '@testing-library/react';
import { useRef } from 'react';
import { Dialog } from './Dialog';

// jsdom doesn't ship pointer/keyboard event interop for synthetic
// React handlers in all versions — using `fireEvent.keyDown` on the
// panel directly is the most reliable way to exercise the trap.
import { fireEvent } from '@testing-library/react';

beforeEach(() => {
  // Each test mounts its own DOM; testing-library auto-cleans, but
  // belt-and-suspenders for the body-children inspection tests.
  cleanup();
  document.body.innerHTML = '';
});

function ClosedThenOpen({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <>
      <button data-testid="trigger">Trigger</button>
      <Dialog open={open} onClose={onClose} title="Test">
        <button data-testid="first">First</button>
        <button data-testid="middle">Middle</button>
        <button data-testid="last">Last</button>
      </Dialog>
    </>
  );
}

describe('Dialog focus contract', () => {
  it('moves focus into the panel when it opens', async () => {
    const { rerender } = render(<ClosedThenOpen open={false} onClose={() => {}} />);
    // Trigger has focus before we open — simulates the user clicking a
    // button that toggles the dialog.
    const trigger = screen.getByTestId('trigger');
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    rerender(<ClosedThenOpen open onClose={() => {}} />);
    // Initial focus is deferred behind requestAnimationFrame; flush it.
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    // Close button comes first in DOM order (it's inside the header,
    // which renders before the body children), so it should receive
    // the default initial focus.
    const closeBtn = screen.getByRole('button', { name: 'Close' });
    expect(document.activeElement).toBe(closeBtn);
  });

  it('wraps Tab from last focusable back to the first', async () => {
    render(<ClosedThenOpen open onClose={() => {}} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    const last = screen.getByTestId('last');
    last.focus();
    expect(document.activeElement).toBe(last);

    const panel = screen.getByRole('dialog');
    fireEvent.keyDown(panel, { key: 'Tab' });

    // The first focusable inside the panel is the close button (DOM order:
    // header close button → first/middle/last in the body).
    const closeBtn = screen.getByRole('button', { name: 'Close' });
    expect(document.activeElement).toBe(closeBtn);
  });

  it('wraps Shift+Tab from first focusable to the last', async () => {
    render(<ClosedThenOpen open onClose={() => {}} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    const closeBtn = screen.getByRole('button', { name: 'Close' });
    closeBtn.focus();
    expect(document.activeElement).toBe(closeBtn);

    const panel = screen.getByRole('dialog');
    fireEvent.keyDown(panel, { key: 'Tab', shiftKey: true });

    expect(document.activeElement).toBe(screen.getByTestId('last'));
  });

  it('restores focus to the previously-focused element when it closes', async () => {
    const { rerender } = render(<ClosedThenOpen open={false} onClose={() => {}} />);
    const trigger = screen.getByTestId('trigger');
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    rerender(<ClosedThenOpen open onClose={() => {}} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    // Focus moved into the dialog.
    expect(document.activeElement).not.toBe(trigger);

    rerender(<ClosedThenOpen open={false} onClose={() => {}} />);
    // Restore happens in the effect cleanup synchronously when `open`
    // flips false; no rAF needed here.
    expect(document.activeElement).toBe(trigger);
  });

  it('marks background siblings inert while open and restores on close', async () => {
    // Set up a sibling under document.body that should be inerted.
    const sibling = document.createElement('div');
    sibling.id = 'app-root';
    sibling.innerHTML = '<button>Background</button>';
    document.body.appendChild(sibling);
    expect(sibling.hasAttribute('inert')).toBe(false);

    const { rerender, unmount } = render(
      <ClosedThenOpen open onClose={() => {}} />,
      {
        // Mount the test tree inside our sibling so the portal's overlay
        // is a *separate* direct child of document.body — that's the
        // shape the inert effect expects in production.
        container: sibling,
      },
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // The portal target is document.body; one of body's direct children
    // is our sibling (mount point), the other is the modal-overlay. The
    // sibling should be inert.
    expect(sibling.hasAttribute('inert')).toBe(true);
    expect(sibling.getAttribute('aria-hidden')).toBe('true');

    rerender(<ClosedThenOpen open={false} onClose={() => {}} />);
    // After close, the inert attribute should be gone (it wasn't there
    // before we opened).
    expect(sibling.hasAttribute('inert')).toBe(false);
    expect(sibling.hasAttribute('aria-hidden')).toBe(false);

    unmount();
  });

  it('honors a custom initialFocusRef', async () => {
    function Harness({ open }: { open: boolean }) {
      const ref = useRef<HTMLButtonElement | null>(null);
      return (
        <Dialog open={open} onClose={() => {}} title="X" initialFocusRef={ref}>
          <button data-testid="first">First</button>
          <button ref={ref} data-testid="target">
            Target
          </button>
          <button data-testid="last">Last</button>
        </Dialog>
      );
    }
    const { rerender } = render(<Harness open={false} />);
    rerender(<Harness open />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    expect(document.activeElement).toBe(screen.getByTestId('target'));
  });

  it('honors a custom restoreFocusRef on close', async () => {
    function Harness({ open }: { open: boolean }) {
      const ref = useRef<HTMLButtonElement | null>(null);
      return (
        <>
          <button data-testid="trigger">Trigger</button>
          <button ref={ref} data-testid="restore-target">
            Restore here
          </button>
          <Dialog open={open} onClose={() => {}} title="X" restoreFocusRef={ref}>
            <button data-testid="inside">Inside</button>
          </Dialog>
        </>
      );
    }
    const { rerender } = render(<Harness open={false} />);
    const trigger = screen.getByTestId('trigger');
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    rerender(<Harness open />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    expect(document.activeElement).not.toBe(trigger);

    rerender(<Harness open={false} />);
    // restoreFocusRef wins over the captured trigger.
    expect(document.activeElement).toBe(screen.getByTestId('restore-target'));
  });

  // Regression guard for the bug surfaced in PR #73 (slice 7 of #56) and
  // folded into this followup from issue/PR #75:
  //
  // The Dialog declares two `useEffect`s gated on `open` — first the
  // background-inert effect, then the focus-restore effect. React runs
  // effect cleanups in declaration order on the same render pass, so on
  // close the inert blanket is removed BEFORE focus is restored. If a
  // future refactor flipped the declaration order, focus-restore would
  // run first and target an element whose ancestor is still `inert`,
  // which silently no-ops in real browsers (jsdom does not model this
  // exactly, so we observe the inert *attribute* directly).
  //
  // Strategy: render the trigger as a sibling of the portal root, spy
  // on its `focus()` method, and capture whether the sibling still has
  // `inert` set at the moment focus is restored. With the correct
  // declaration order it must be removed already.
  it('removes background inert before restoring focus on close', async () => {
    // Sibling subtree under document.body — same shape as the AddPanel
    // trigger that surfaced the original bug.
    const sibling = document.createElement('div');
    sibling.id = 'app-root';
    document.body.appendChild(sibling);

    const { rerender, unmount } = render(
      <ClosedThenOpen open={false} onClose={() => {}} />,
      // Mount inside the sibling so the modal's overlay portal is a
      // *separate* direct child of document.body — only then does the
      // inert effect mark `sibling` as inert.
      { container: sibling },
    );
    const trigger = screen.getByTestId('trigger');
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    rerender(<ClosedThenOpen open onClose={() => {}} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });
    // Sanity: inert is applied while open.
    expect(sibling.hasAttribute('inert')).toBe(true);

    // Spy on the trigger's focus() — record whether `sibling` still has
    // `inert` set at the moment focus-restore runs.
    let inertAtFocusRestore: boolean | null = null;
    const realFocus = trigger.focus.bind(trigger);
    trigger.focus = () => {
      inertAtFocusRestore = sibling.hasAttribute('inert');
      realFocus();
    };

    // Close. React runs effect cleanups in declaration order:
    //   1. inert cleanup → removes `inert` from sibling
    //   2. focus-restore cleanup → calls trigger.focus()
    // If the declarations were ever flipped, step 2 would run first and
    // observe inert=true.
    rerender(<ClosedThenOpen open={false} onClose={() => {}} />);

    expect(inertAtFocusRestore).toBe(false);
    // And focus actually landed back on the trigger (matches the
    // existing restore-on-close test, but worth pinning here too).
    expect(document.activeElement).toBe(trigger);

    unmount();
  });
});
