// Component-level tests for the Menu primitive.
//
// What this file covers (and what it deliberately does NOT)
// ---------------------------------------------------------
// `useRovingTabindex.test.tsx` already exhaustively exercises the
// roving-tabindex / typeahead / Enter / Space / Escape semantics. We do
// NOT duplicate that surface here; instead we verify that `Menu` wires
// the hook correctly:
//
//   - The trigger exposes `aria-haspopup="menu"` + `aria-expanded`, and
//     the expanded value toggles when the menu opens/closes.
//   - The popover has `role="menu"`; each item has `role="menuitem"`
//     with the supplied label as its accessible name.
//   - One representative keyboard path each (ArrowDown, Home/End,
//     typeahead, Escape) — to prove the hook is plumbed in, not to
//     re-verify the hook itself.
//   - The empty-state slot renders when `items` is empty.
//
// The two subtler contracts — outside-click-no-restore and synchronous
// focus restore before onSelect — live in `Menu.contract.test.tsx`.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Menu, type MenuItem } from './Menu';

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
});

function makeItems(specs: Array<{ label: string; onSelect?: () => void }>): MenuItem[] {
  return specs.map(({ label, onSelect }) => ({
    label,
    onSelect: onSelect ?? (() => {}),
  }));
}

function Harness({ items }: { items: MenuItem[] }) {
  return (
    <Menu
      items={items}
      trigger={(props) => (
        <button {...props} type="button">
          Open
        </button>
      )}
    />
  );
}

describe('Menu — trigger ARIA', () => {
  it('renders a button trigger with aria-haspopup="menu" and aria-expanded=false when closed', () => {
    render(<Harness items={makeItems([{ label: 'One' }])} />);
    const trigger = screen.getByRole('button', { name: 'Open' });
    expect(trigger).toHaveAttribute('aria-haspopup', 'menu');
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    // Menu is closed: no role=menu in the tree.
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('toggles aria-expanded when the trigger is clicked', async () => {
    const user = userEvent.setup();
    render(<Harness items={makeItems([{ label: 'One' }])} />);
    const trigger = screen.getByRole('button', { name: 'Open' });
    await user.click(trigger);
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByRole('menu')).toBeInTheDocument();
    await user.click(trigger);
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('menu')).toBeNull();
  });
});

describe('Menu — popover structure', () => {
  it('renders role="menu" and role="menuitem" with accessible names from `label`', async () => {
    const user = userEvent.setup();
    render(
      <Harness
        items={makeItems([{ label: 'Alpha' }, { label: 'Beta' }, { label: 'Gamma' }])}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    const menu = screen.getByRole('menu');
    expect(menu).toBeInTheDocument();
    const items = screen.getAllByRole('menuitem');
    expect(items).toHaveLength(3);
    expect(items[0]).toHaveAccessibleName('Alpha');
    expect(items[1]).toHaveAccessibleName('Beta');
    expect(items[2]).toHaveAccessibleName('Gamma');
  });

  it('renders the empty-state slot when items is empty', async () => {
    const user = userEvent.setup();
    render(
      <Menu
        items={[]}
        emptyState="Nothing here"
        emptyClassName="my-empty"
        trigger={(props) => (
          <button {...props} type="button">
            Open
          </button>
        )}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    expect(screen.queryByRole('menuitem')).toBeNull();
    expect(screen.getByText('Nothing here')).toBeInTheDocument();
  });
});

describe('Menu — keyboard wiring', () => {
  // We only assert that the hook is plumbed in — the hook's own tests
  // cover every key. Each test below picks ONE representative key.

  it('opens with focus on the first menuitem', async () => {
    const user = userEvent.setup();
    render(
      <Harness items={makeItems([{ label: 'First' }, { label: 'Second' }])} />,
    );
    const trigger = screen.getByRole('button', { name: 'Open' });
    trigger.focus();
    await user.keyboard('{Enter}');
    const items = screen.getAllByRole('menuitem');
    expect(items[0]).toHaveFocus();

    // A menu can open under a stationary pointer; mouseenter from that
    // mount must not override keyboard initial focus.
    fireEvent.mouseEnter(items[1]);
    expect(items[0]).toHaveFocus();

    fireEvent.mouseMove(items[1]);
    expect(items[1]).toHaveFocus();
  });

  it('ArrowDown moves focus to the next item', async () => {
    const user = userEvent.setup();
    render(
      <Harness items={makeItems([{ label: 'First' }, { label: 'Second' }])} />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    await user.keyboard('{ArrowDown}');
    const items = screen.getAllByRole('menuitem');
    expect(items[1]).toHaveFocus();
  });

  it('Home/End jump to first/last item', async () => {
    const user = userEvent.setup();
    render(
      <Harness items={makeItems([{ label: 'A' }, { label: 'B' }, { label: 'C' }])} />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    await user.keyboard('{End}');
    const items = screen.getAllByRole('menuitem');
    expect(items[2]).toHaveFocus();
    await user.keyboard('{Home}');
    expect(items[0]).toHaveFocus();
  });

  it('typeahead jumps to the first item matching the typed prefix', async () => {
    const user = userEvent.setup();
    render(
      <Harness
        items={makeItems([{ label: 'Apple' }, { label: 'Banana' }, { label: 'Cherry' }])}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    const items = screen.getAllByRole('menuitem');
    // Drive the typeahead via fireEvent to dodge userEvent + fake-timer
    // interactions (matches the hook's own test pattern).
    fireEvent.keyDown(items[0], { key: 'c' });
    expect(items[2]).toHaveFocus();
  });

  it('Escape closes the menu and restores focus to the trigger', async () => {
    const user = userEvent.setup();
    render(<Harness items={makeItems([{ label: 'One' }])} />);
    const trigger = screen.getByRole('button', { name: 'Open' });
    await user.click(trigger);
    expect(screen.getByRole('menu')).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it('Enter on a menuitem fires its onSelect', async () => {
    const user = userEvent.setup();
    const a = vi.fn();
    const b = vi.fn();
    render(
      <Harness items={makeItems([{ label: 'A', onSelect: a }, { label: 'B', onSelect: b }])} />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    await user.keyboard('{ArrowDown}');
    await user.keyboard('{Enter}');
    expect(a).not.toHaveBeenCalled();
    expect(b).toHaveBeenCalledTimes(1);
  });

  it('mouse click on a menuitem fires its onSelect and closes the menu', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <Harness items={makeItems([{ label: 'Only', onSelect }])} />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    await user.click(screen.getByRole('menuitem', { name: 'Only' }));
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('menu')).toBeNull();
  });
});

describe('Menu — disabled items', () => {
  it('skips activation on disabled items but keeps them visible and focusable', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <Harness
        items={[
          { label: 'Disabled', onSelect, disabled: true },
          { label: 'Enabled', onSelect: () => {} },
        ]}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Open' }));
    const items = screen.getAllByRole('menuitem');
    expect(items[0]).toHaveAttribute('aria-disabled', 'true');
    // Roving navigation still reaches the disabled item (WAI-ARIA APG:
    // disabled menuitems remain focusable for screen-reader discoverability).
    expect(items[0]).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(onSelect).not.toHaveBeenCalled();
  });
});
