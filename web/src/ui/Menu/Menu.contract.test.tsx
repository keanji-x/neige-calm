// Contract tests for Menu's two subtler behaviors.
//
// Neither of these is reachable from the keyboard / ARIA surface that
// `useRovingTabindex.test.tsx` and `Menu.test.tsx` cover — they live in
// the open/close lifecycle owned by Menu itself, and silently
// regressing them would break observable focus behavior that real users
// notice (and that screen reader users especially notice).
//
// Contract A — outside-click closes without restoring focus
// ---------------------------------------------------------
// When the user clicks OUTSIDE the menu wrapper, the menu closes; we do
// NOT yank focus back to the trigger. Yanking focus back would be
// hostile: the user gestured elsewhere intentionally, and pulling their
// focus point away from the spot they clicked is exactly the kind of
// thing assistive tech users complain about. The escape and activation
// paths restore focus (they're keyboard-driven closes that the user
// expects to land back at the trigger); outside click does not.
//
// Contract B — synchronous focus restore BEFORE onSelect
// ------------------------------------------------------
// When the user activates a menuitem, Menu does two things in a single
// synchronous tick, in this order:
//   1. setOpen(false) + triggerRef.current.focus()
//   2. item.onSelect()
//
// If `onSelect` opens a Dialog, the Dialog's mount-time effect
// snapshots `document.activeElement` to know where to restore focus on
// close. If we focused the trigger via a microtask (or any deferred
// path), the Dialog would race us and snapshot the about-to-unmount
// menuitem; closing the Dialog would then noop the restore (its target
// is detached) and focus would fall to <body>. Doing focus + onSelect
// synchronously means the trigger button is `document.activeElement`
// by the time onSelect runs.
//
// We test this end-to-end with the real Dialog primitive — that's the
// shape the contract actually has to hold against, and PR1's review
// surfaced exactly this integration as load-bearing-but-untested.

import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from '../../shared/state';
import { Menu, type MenuItem } from './Menu';
import { Dialog } from '../Dialog/Dialog';

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
});

// --- Contract A ----------------------------------------------------------

describe('Menu contract — outside-click closes WITHOUT focus restore', () => {
  function Harness() {
    return (
      <div>
        <Menu
          items={[{ label: 'Item', onSelect: () => {} } satisfies MenuItem]}
          trigger={(props) => (
            <button {...props} type="button">
              Trigger
            </button>
          )}
        />
        <button type="button" data-testid="outside">
          Outside
        </button>
      </div>
    );
  }

  it('closes the menu and does NOT focus the trigger after outside click', async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const trigger = screen.getByRole('button', { name: 'Trigger' });
    await user.click(trigger);
    // Sanity: menu is open, first item has focus.
    expect(screen.getByRole('menu')).toBeInTheDocument();
    expect(screen.getByRole('menuitem', { name: 'Item' })).toHaveFocus();

    // Click on the outside button — userEvent fires mousedown which is
    // what the document-level handler listens for. Use pointer down +
    // explicit click target so we get a real mousedown event on the
    // outside element (the document listener catches it via bubbling).
    const outside = screen.getByTestId('outside');
    await user.click(outside);

    // Menu has closed.
    expect(screen.queryByRole('menu')).toBeNull();
    // CONTRACT: focus is NOT on the trigger. It either lives on the
    // clicked element (real browsers) or on <body> (jsdom doesn't move
    // focus on click for non-form controls). Either is acceptable —
    // the load-bearing assertion is that we didn't pull focus BACK to
    // the trigger behind the user's gesture.
    expect(document.activeElement).not.toBe(trigger);
  });
});

// --- Contract B ----------------------------------------------------------

describe('Menu contract — synchronous focus restore BEFORE onSelect', () => {
  // Harness: the menu's only item opens a Dialog when activated. The
  // Dialog uses its standard restoreFocus path — capturing
  // `document.activeElement` at mount, restoring on unmount.
  //
  // The contract is: by the time Dialog's mount effect runs (which
  // captures previously-focused), the trigger button must already be
  // `document.activeElement`. We assert this two ways:
  //
  //   1. While the menu's onSelect is running (between the synchronous
  //      focus call and the Dialog's mount effect), the trigger is
  //      focused.
  //   2. After the Dialog closes, focus is RESTORED to the trigger —
  //      this is the load-bearing end-state from the user's POV.
  //
  // The Dialog mount effect is asynchronous wrt the synchronous
  // activate path (it runs on the next commit), so by the time it runs
  // the trigger has had time to be focused. But the snapshot has to
  // see the trigger, not <body> — and that's only true if the trigger
  // is focused IN the same React batch, not later.

  it('focuses the trigger before onSelect runs, so a Dialog opened from onSelect can snapshot it', async () => {
    const user = userEvent.setup();

    // Spy harness that observes `document.activeElement` at the moment
    // `onSelect` runs and stashes it for assertion. The point of the
    // observation: at this instant the trigger MUST already be focused,
    // because any deferred path would lose the snapshot a Dialog would
    // capture if opened from inside `onSelect`.
    let activeAtOnSelectStart: Element | null = null;

    function SpyingHarness() {
      const [open, setOpen] = useState(false);
      const items: MenuItem[] = [
        {
          label: 'Open dialog',
          onSelect: () => {
            // CONTRACT-A observation: by the time onSelect runs, the
            // trigger must already be `document.activeElement`. This is
            // the same instant the Dialog's mount-time snapshot would
            // happen if the Dialog were opened synchronously here.
            activeAtOnSelectStart = document.activeElement;
            setOpen(true);
          },
        },
      ];
      return (
        <div>
          <Menu
            items={items}
            trigger={(props) => (
              <button {...props} type="button">
                Trigger
              </button>
            )}
          />
          <Dialog open={open} onClose={() => setOpen(false)} title="Dialog">
            <button type="button" data-testid="dialog-close" onClick={() => setOpen(false)}>
              Close
            </button>
          </Dialog>
        </div>
      );
    }

    render(<SpyingHarness />);
    const trigger = screen.getByRole('button', { name: 'Trigger' });
    await user.click(trigger);
    expect(screen.getByRole('menu')).toBeInTheDocument();

    // Activate the first menuitem via Enter.
    await user.keyboard('{Enter}');

    // CONTRACT B (the load-bearing assertion): at the start of onSelect
    // the trigger button was `document.activeElement`. If Menu deferred
    // the focus call (e.g. queueMicrotask), this would be the menuitem
    // — which is about to unmount — and Dialog's snapshot would be a
    // detached node.
    expect(activeAtOnSelectStart).toBe(trigger);

    // End-to-end consequence: the Dialog opened, and when we close it
    // focus returns to the trigger. (This is the user-visible flavor
    // of the contract.)
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    // Close the dialog via its Close button.
    await user.click(screen.getByTestId('dialog-close'));
    expect(screen.queryByRole('dialog')).toBeNull();
    // Dialog's restore path returns focus to whatever was active when
    // it mounted — i.e. the trigger.
    expect(trigger).toHaveFocus();
  });

  it('keyboard activation path: menu closes synchronously so the trigger is the active element by the time activate() returns', async () => {
    const user = userEvent.setup();
    let triggerAtSelect: Element | null = null;
    function Probe() {
      const items: MenuItem[] = [
        {
          label: 'Probe',
          onSelect: () => {
            triggerAtSelect = document.activeElement;
          },
        },
      ];
      return (
        <Menu
          items={items}
          trigger={(props) => (
            <button {...props} type="button">
              Trigger
            </button>
          )}
        />
      );
    }
    render(<Probe />);
    const trigger = screen.getByRole('button', { name: 'Trigger' });
    await user.click(trigger);
    await user.keyboard('{Enter}');
    expect(triggerAtSelect).toBe(trigger);
  });
});
